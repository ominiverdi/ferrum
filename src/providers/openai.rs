use super::{
    Provider, ProviderFailure, ProviderResponse, StreamEvent, TokenUsage,
    transport::{
        BodyLimits, MAX_PROVIDER_ERROR_BODY_BYTES, MAX_PROVIDER_JSON_BODY_BYTES, SseControl,
        collect_response_body, consume_sse_response,
    },
};
use crate::{
    agent::{
        messages::{ContentBlock, Message, Role, sanitize_thinking_text},
        tools::ToolDefinition,
    },
    auth::openai_codex,
    cancel::{self, WaitError},
    config::ThinkingLevel,
    text_truncate::truncate_to_max_bytes,
};
use anyhow::{Context, Result};
use reqwest::{Client, Response, StatusCode, header, redirect::Policy};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    env,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const PROVIDER_INITIAL_RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);
const PROVIDER_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(90);
const PROVIDER_BODY_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const PROVIDER_BODY_TOTAL_TIMEOUT: Duration = Duration::from_secs(120);
const PROVIDER_ERROR_BODY_IDLE_TIMEOUT: Duration = Duration::from_secs(10);
const PROVIDER_ERROR_BODY_TOTAL_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_PROVIDER_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_PROVIDER_THINKING_BYTES: usize = 8 * 1024 * 1024;
const MAX_PROVIDER_TOOL_ARGUMENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_PROVIDER_FIELD_BYTES: usize = 16 * 1024;
const MAX_PROVIDER_ERROR_DISPLAY_BYTES: usize = 8 * 1024;
const MAX_PROVIDER_EVENTS: usize = 100_000;

pub struct OpenAiCompatProvider {
    api_key_env: Option<String>,
    base_url: String,
    streaming: bool,
    stream_usage: bool,
    client: Client,
}

fn metrics_enabled() -> bool {
    matches!(
        std::env::var("FERRUM_METRICS").ok().as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

fn success_body_limits() -> BodyLimits {
    BodyLimits {
        max_bytes: MAX_PROVIDER_JSON_BODY_BYTES,
        idle_timeout: PROVIDER_BODY_IDLE_TIMEOUT,
        total_timeout: PROVIDER_BODY_TOTAL_TIMEOUT,
    }
}

fn error_body_limits() -> BodyLimits {
    BodyLimits {
        max_bytes: MAX_PROVIDER_ERROR_BODY_BYTES,
        idle_timeout: PROVIDER_ERROR_BODY_IDLE_TIMEOUT,
        total_timeout: PROVIDER_ERROR_BODY_TOTAL_TIMEOUT,
    }
}

async fn collect_provider_body(
    response: Response,
    cancelled: Option<&Arc<AtomicBool>>,
    success: bool,
    label: &str,
) -> Result<Vec<u8>> {
    collect_response_body(
        response,
        cancelled,
        if success {
            success_body_limits()
        } else {
            error_body_limits()
        },
        label,
    )
    .await
}

fn provider_stream_error(error: anyhow::Error, label: &str) -> anyhow::Error {
    if error.to_string() == "aborted" {
        return error;
    }
    error.context(format!(
        "{label} failed after the provider accepted the request; Ferrum did not retry it to avoid replaying partial output"
    ))
}

fn utf8_body<'a>(body: &'a [u8], label: &str) -> Result<&'a str> {
    std::str::from_utf8(body).with_context(|| format!("{label} was not valid UTF-8"))
}

fn provider_status_error(label: &str, status: StatusCode, body: &[u8]) -> anyhow::Error {
    let text = String::from_utf8_lossy(body);
    let text = if text.len() > MAX_PROVIDER_ERROR_DISPLAY_BYTES {
        format!(
            "{} [truncated]",
            truncate_to_max_bytes(&text, MAX_PROVIDER_ERROR_DISPLAY_BYTES)
        )
    } else {
        text.into_owned()
    };
    let message = format!("{label} returned {status}: {text}");
    if is_structured_context_overflow(status, body) {
        return ProviderFailure::ContextOverflow { message }.into();
    }
    if status == StatusCode::UNAUTHORIZED {
        return ProviderFailure::Authentication { message }.into();
    }
    anyhow::anyhow!(message)
}

fn is_structured_context_overflow(status: StatusCode, body: &[u8]) -> bool {
    if status == StatusCode::PAYLOAD_TOO_LARGE {
        return true;
    }
    if status != StatusCode::BAD_REQUEST {
        return false;
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    structured_context_overflow_value(&value)
}

fn structured_context_overflow_value(value: &serde_json::Value) -> bool {
    let error = value
        .get("error")
        .or_else(|| {
            value
                .get("response")
                .and_then(|response| response.get("error"))
        })
        .unwrap_or(value);
    let code = error
        .get("code")
        .or_else(|| error.get("type"))
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    matches!(
        code,
        "context_length_exceeded"
            | "model_context_window_exceeded"
            | "request_too_large"
            | "prompt_too_long"
            | "input_too_long"
    )
}

fn optional_string_field<'a>(
    value: &'a serde_json::Value,
    field: &str,
    label: &str,
) -> Result<Option<&'a str>> {
    match value.get(field) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(Some)
            .with_context(|| format!("{label} `{field}` must be a string")),
    }
}

fn push_bounded(target: &mut String, value: &str, max_bytes: usize, field: &str) -> Result<()> {
    if target.len().saturating_add(value.len()) > max_bytes {
        anyhow::bail!("provider {field} exceeded {max_bytes} bytes");
    }
    target.push_str(value);
    Ok(())
}

fn validate_field(value: &str, max_bytes: usize, field: &str) -> Result<()> {
    if value.len() > max_bytes {
        anyhow::bail!("provider {field} exceeded {max_bytes} bytes");
    }
    Ok(())
}

impl OpenAiCompatProvider {
    pub fn new(
        api_key_env: Option<String>,
        base_url: String,
        streaming: bool,
        stream_usage: bool,
    ) -> Result<Self> {
        Ok(Self {
            api_key_env,
            base_url: base_url.trim_end_matches('/').to_string(),
            streaming,
            stream_usage,
            client: Client::builder().redirect(Policy::none()).build()?,
        })
    }
}

impl Provider for OpenAiCompatProvider {
    fn complete<'a>(
        &'a self,
        model: &'a str,
        messages: &'a [Message],
        _tools: &'a [ToolDefinition],
        thinking: ThinkingLevel,
    ) -> Pin<Box<dyn Future<Output = Result<ProviderResponse>> + Send + 'a>> {
        Box::pin(async move {
            let api_key = self
                .api_key_env
                .as_ref()
                .map(|name| env::var(name).with_context(|| format!("{name} is not set")))
                .transpose()?;
            let reasoning_effort = thinking.as_openai();
            let request = ChatRequest {
                model,
                messages: messages.iter().map(ChatMessage::from_message).collect(),
                tools: openai_tools(_tools),
                tool_choice: if _tools.is_empty() {
                    None
                } else {
                    Some("auto")
                },
                reasoning_effort,
                stream: false,
                stream_options: None,
            };

            let mut http_request = self
                .client
                .post(format!("{}/chat/completions", self.base_url));
            if let Some(api_key) = api_key.as_deref() {
                http_request = http_request.bearer_auth(api_key);
            }
            let response = cancel::race_timeout(
                http_request.json(&request).send(),
                None,
                PROVIDER_INITIAL_RESPONSE_TIMEOUT,
            )
            .await
            .map_err(|error| match error {
                WaitError::Cancelled => anyhow::anyhow!("aborted"),
                WaitError::TimedOut => anyhow::anyhow!(
                    "OpenAI-compatible provider did not respond within {}s",
                    PROVIDER_INITIAL_RESPONSE_TIMEOUT.as_secs()
                ),
            })?
            .context("OpenAI-compatible request failed")?;

            let status = response.status();
            let body = collect_provider_body(
                response,
                None,
                status.is_success(),
                "OpenAI-compatible response",
            )
            .await?;
            let body = if !status.is_success() {
                let text = utf8_body(&body, "OpenAI-compatible error response")?;
                if reasoning_effort.is_some() && is_reasoning_effort_unsupported_error(text) {
                    let retry_request = ChatRequest {
                        reasoning_effort: None,
                        ..request
                    };
                    let mut retry_http_request = self
                        .client
                        .post(format!("{}/chat/completions", self.base_url));
                    if let Some(api_key) = api_key.as_deref() {
                        retry_http_request = retry_http_request.bearer_auth(api_key);
                    }
                    let retry_response = cancel::race_timeout(
                        retry_http_request.json(&retry_request).send(),
                        None,
                        PROVIDER_INITIAL_RESPONSE_TIMEOUT,
                    )
                    .await
                    .map_err(|error| match error {
                        WaitError::Cancelled => anyhow::anyhow!("aborted"),
                        WaitError::TimedOut => anyhow::anyhow!(
                            "OpenAI-compatible reasoning retry did not respond within {}s",
                            PROVIDER_INITIAL_RESPONSE_TIMEOUT.as_secs()
                        ),
                    })?
                    .context("OpenAI-compatible reasoning retry failed")?;
                    let retry_status = retry_response.status();
                    let retry_body = collect_provider_body(
                        retry_response,
                        None,
                        retry_status.is_success(),
                        "OpenAI-compatible reasoning retry response",
                    )
                    .await?;
                    if !retry_status.is_success() {
                        return Err(provider_status_error(
                            "OpenAI-compatible provider",
                            retry_status,
                            &retry_body,
                        ));
                    }
                    retry_body
                } else {
                    return Err(provider_status_error(
                        "OpenAI-compatible provider",
                        status,
                        &body,
                    ));
                }
            } else {
                body
            };

            let body: ChatResponse = serde_json::from_slice(&body)
                .context("failed to parse OpenAI-compatible provider response")?;
            if body.choices.len() > MAX_PARALLEL_TOOL_CALLS {
                anyhow::bail!(
                    "OpenAI-compatible response exceeded {MAX_PARALLEL_TOOL_CALLS} choices"
                );
            }
            let usage = body.usage.as_ref().map(OpenAiUsage::to_token_usage);
            let message = body.choices.into_iter().next().map(|choice| choice.message);
            Ok(ProviderResponse {
                message: chat_response_to_message(message)?,
                usage,
            })
        })
    }

    fn complete_streaming<'a>(
        &'a self,
        model: &'a str,
        messages: &'a [Message],
        tools: &'a [ToolDefinition],
        thinking: ThinkingLevel,
        on_event: &'a mut (dyn FnMut(StreamEvent) + Send),
        cancelled: Option<Arc<AtomicBool>>,
    ) -> Pin<Box<dyn Future<Output = Result<ProviderResponse>> + Send + 'a>> {
        if !self.streaming {
            return Box::pin(async move {
                if cancelled
                    .as_ref()
                    .is_some_and(|flag| flag.load(Ordering::Relaxed))
                {
                    anyhow::bail!("aborted");
                }
                let response = cancel::race(
                    self.complete(model, messages, tools, thinking),
                    cancelled.as_ref(),
                )
                .await
                .map_err(|_| anyhow::anyhow!("aborted"))??;

                if cancelled
                    .as_ref()
                    .is_some_and(|flag| flag.load(Ordering::Relaxed))
                {
                    anyhow::bail!("aborted");
                }
                emit_message_stream_events(&response.message, on_event);
                Ok(response)
            });
        }
        Box::pin(async move {
            let api_key = self
                .api_key_env
                .as_ref()
                .map(|name| env::var(name).with_context(|| format!("{name} is not set")))
                .transpose()?;
            let mut response = send_openai_compat_stream_request(
                self,
                api_key.as_deref(),
                model,
                messages,
                tools,
                CompatStreamOptions {
                    thinking,
                    include_usage: self.stream_usage,
                    cancelled: cancelled.as_ref(),
                },
            )
            .await?;
            let status = response.status();
            if !status.is_success() {
                let body = collect_provider_body(
                    response,
                    cancelled.as_ref(),
                    false,
                    "OpenAI-compatible error response",
                )
                .await?;
                let text = utf8_body(&body, "OpenAI-compatible error response")?;
                if self.stream_usage && is_stream_usage_unsupported_error(text) {
                    response = send_openai_compat_stream_request(
                        self,
                        api_key.as_deref(),
                        model,
                        messages,
                        tools,
                        CompatStreamOptions {
                            thinking,
                            include_usage: false,
                            cancelled: cancelled.as_ref(),
                        },
                    )
                    .await?;
                    let retry_status = response.status();
                    if !retry_status.is_success() {
                        let retry_body = collect_provider_body(
                            response,
                            cancelled.as_ref(),
                            false,
                            "OpenAI-compatible retry error response",
                        )
                        .await?;
                        return Err(provider_status_error(
                            "OpenAI-compatible provider",
                            retry_status,
                            &retry_body,
                        ));
                    }
                } else if thinking.as_openai().is_some()
                    && is_reasoning_effort_unsupported_error(text)
                {
                    response = send_openai_compat_stream_request(
                        self,
                        api_key.as_deref(),
                        model,
                        messages,
                        tools,
                        CompatStreamOptions {
                            thinking: ThinkingLevel::Off,
                            include_usage: self.stream_usage,
                            cancelled: cancelled.as_ref(),
                        },
                    )
                    .await?;
                    let retry_status = response.status();
                    if !retry_status.is_success() {
                        let retry_body = collect_provider_body(
                            response,
                            cancelled.as_ref(),
                            false,
                            "OpenAI-compatible reasoning retry error response",
                        )
                        .await?;
                        return Err(provider_status_error(
                            "OpenAI-compatible provider",
                            retry_status,
                            &retry_body,
                        ));
                    }
                } else {
                    return Err(provider_status_error(
                        "OpenAI-compatible provider",
                        status,
                        &body,
                    ));
                }
            }

            let mut parser = ChatSseParser::default();
            consume_sse_response(
                response,
                cancelled.as_ref(),
                PROVIDER_STREAM_IDLE_TIMEOUT,
                "OpenAI-compatible stream",
                |data| {
                    if parser.process_data(data, Some(on_event))? {
                        Ok(SseControl::Stop)
                    } else {
                        Ok(SseControl::Continue)
                    }
                },
            )
            .await
            .map_err(|error| provider_stream_error(error, "OpenAI-compatible stream"))?;
            parser
                .finish()
                .map_err(|error| provider_stream_error(error, "OpenAI-compatible stream"))
        })
    }
}

fn emit_message_stream_events(message: &Message, on_event: &mut (dyn FnMut(StreamEvent) + Send)) {
    for block in &message.content {
        match block {
            ContentBlock::Thinking { text, .. } if !text.is_empty() => {
                on_event(StreamEvent::ThinkingDelta(text.clone()));
            }
            ContentBlock::Text { text } if !text.is_empty() => {
                on_event(StreamEvent::TextDelta(text.clone()));
            }
            _ => {}
        }
    }
}

struct CompatStreamOptions<'a> {
    thinking: ThinkingLevel,
    include_usage: bool,
    cancelled: Option<&'a Arc<AtomicBool>>,
}

async fn send_openai_compat_stream_request(
    provider: &OpenAiCompatProvider,
    api_key: Option<&str>,
    model: &str,
    messages: &[Message],
    tools: &[ToolDefinition],
    options: CompatStreamOptions<'_>,
) -> Result<Response> {
    let CompatStreamOptions {
        thinking,
        include_usage,
        cancelled,
    } = options;

    let request = ChatRequest {
        model,
        messages: messages.iter().map(ChatMessage::from_message).collect(),
        tools: openai_tools(tools),
        tool_choice: if tools.is_empty() { None } else { Some("auto") },
        reasoning_effort: thinking.as_openai(),
        stream: true,
        stream_options: include_usage.then_some(ChatStreamOptions {
            include_usage: true,
        }),
    };

    let mut http_request = provider
        .client
        .post(format!("{}/chat/completions", provider.base_url));
    if let Some(api_key) = api_key {
        http_request = http_request.bearer_auth(api_key);
    }
    cancel::race_timeout(
        http_request.json(&request).send(),
        cancelled,
        PROVIDER_INITIAL_RESPONSE_TIMEOUT,
    )
    .await
    .map_err(|error| match error {
        WaitError::Cancelled => anyhow::anyhow!("aborted"),
        WaitError::TimedOut => anyhow::anyhow!(
            "OpenAI-compatible provider did not respond within {}s",
            PROVIDER_INITIAL_RESPONSE_TIMEOUT.as_secs()
        ),
    })?
    .context("OpenAI-compatible streaming request failed")
}

fn is_stream_usage_unsupported_error(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("stream_options")
        || lower.contains("stream options")
        || (lower.contains("include_usage") && lower.contains("unsupported"))
        || (lower.contains("include_usage") && lower.contains("unknown"))
        || (lower.contains("include_usage") && lower.contains("invalid"))
}

fn is_reasoning_effort_unsupported_error(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    (lower.contains("reasoning_effort") || lower.contains("reasoning effort"))
        && (lower.contains("unsupported")
            || lower.contains("unknown")
            || lower.contains("unrecognized")
            || lower.contains("invalid")
            || lower.contains("extra")
            || lower.contains("not permitted"))
}

pub struct OpenAiCodexProvider {
    base_url: String,
    auth_path: PathBuf,
    client: Client,
}

impl OpenAiCodexProvider {
    pub fn new(base_url: String, auth_path: PathBuf) -> Result<Self> {
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            auth_path,
            client: Client::builder().redirect(Policy::none()).build()?,
        })
    }

    fn responses_url(&self) -> String {
        if self.base_url.ends_with("/codex/responses") {
            self.base_url.clone()
        } else if self.base_url.ends_with("/codex") {
            format!("{}/responses", self.base_url)
        } else {
            format!("{}/codex/responses", self.base_url)
        }
    }
}

impl Provider for OpenAiCodexProvider {
    fn complete<'a>(
        &'a self,
        model: &'a str,
        messages: &'a [Message],
        _tools: &'a [ToolDefinition],
        thinking: ThinkingLevel,
    ) -> Pin<Box<dyn Future<Output = Result<ProviderResponse>> + Send + 'a>> {
        Box::pin(async move {
            let instructions = codex_instructions(messages);
            let request = CodexResponsesRequest {
                model,
                store: false,
                stream: true,
                instructions: &instructions,
                input: codex_inputs(messages),
                text: CodexText { verbosity: "low" },
                include: vec!["reasoning.encrypted_content"],
                tools: codex_tools(_tools),
                tool_choice: if _tools.is_empty() {
                    None
                } else {
                    Some("auto")
                },
                reasoning: thinking.as_codex().map(|effort| CodexReasoning {
                    effort,
                    summary: "detailed",
                }),
                parallel_tool_calls: !_tools.is_empty(),
            };

            let response = send_codex_authenticated_request(
                &self.client,
                &self.responses_url(),
                &self.auth_path,
                &request,
                None,
            )
            .await?;
            let mut parser = ResponsesSseParser::default();
            consume_sse_response(
                response,
                None,
                PROVIDER_STREAM_IDLE_TIMEOUT,
                "OpenAI Codex stream",
                |data| {
                    if parser.process_data(data, None)? {
                        Ok(SseControl::Stop)
                    } else {
                        Ok(SseControl::Continue)
                    }
                },
            )
            .await
            .map_err(|error| provider_stream_error(error, "OpenAI Codex stream"))?;
            parser
                .finish()
                .map_err(|error| provider_stream_error(error, "OpenAI Codex stream"))
        })
    }

    fn complete_streaming<'a>(
        &'a self,
        model: &'a str,
        messages: &'a [Message],
        _tools: &'a [ToolDefinition],
        thinking: ThinkingLevel,
        on_event: &'a mut (dyn FnMut(StreamEvent) + Send),
        cancelled: Option<Arc<AtomicBool>>,
    ) -> Pin<Box<dyn Future<Output = Result<ProviderResponse>> + Send + 'a>> {
        Box::pin(async move {
            let instructions = codex_instructions(messages);
            let request = CodexResponsesRequest {
                model,
                store: false,
                stream: true,
                instructions: &instructions,
                input: codex_inputs(messages),
                text: CodexText { verbosity: "low" },
                include: vec!["reasoning.encrypted_content"],
                tools: codex_tools(_tools),
                tool_choice: if _tools.is_empty() {
                    None
                } else {
                    Some("auto")
                },
                reasoning: thinking.as_codex().map(|effort| CodexReasoning {
                    effort,
                    summary: "detailed",
                }),
                parallel_tool_calls: !_tools.is_empty(),
            };

            let response = send_codex_authenticated_request(
                &self.client,
                &self.responses_url(),
                &self.auth_path,
                &request,
                cancelled.as_ref(),
            )
            .await?;

            let mut parser = ResponsesSseParser::default();
            consume_sse_response(
                response,
                cancelled.as_ref(),
                PROVIDER_STREAM_IDLE_TIMEOUT,
                "OpenAI Codex stream",
                |data| {
                    if parser.process_data(data, Some(on_event))? {
                        Ok(SseControl::Stop)
                    } else {
                        Ok(SseControl::Continue)
                    }
                },
            )
            .await
            .map_err(|error| provider_stream_error(error, "OpenAI Codex stream"))?;
            parser
                .finish()
                .map_err(|error| provider_stream_error(error, "OpenAI Codex stream"))
        })
    }
}

async fn send_codex_authenticated_request(
    client: &Client,
    url: &str,
    auth_path: &Path,
    request: &CodexResponsesRequest<'_>,
    cancelled: Option<&Arc<AtomicBool>>,
) -> Result<Response> {
    let api_key = openai_codex::get_api_key_from_path(auth_path.to_path_buf())
        .await?
        .context("OpenAI Codex auth not found; run `ferrum login openai`")?;
    let account_id = openai_codex::extract_account_id(&api_key)?;
    match send_codex_request_with_retries(client, url, &api_key, &account_id, request, cancelled)
        .await
    {
        Err(error) if crate::providers::is_authentication_error(&error) => {
            let refreshed_api_key =
                openai_codex::refresh_after_rejection(auth_path.to_path_buf(), &api_key)
                    .await?
                    .context("OpenAI Codex auth not found; run `ferrum login openai`")?;
            let account_id = openai_codex::extract_account_id(&refreshed_api_key)?;
            send_codex_request_with_retries(
                client,
                url,
                &refreshed_api_key,
                &account_id,
                request,
                cancelled,
            )
            .await
        }
        result => result,
    }
}

const CODEX_MAX_RETRIES: usize = 3;
const CODEX_RETRY_BASE_DELAY: Duration = Duration::from_secs(2);
const CODEX_MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

async fn send_codex_request_with_retries(
    client: &Client,
    url: &str,
    api_key: &str,
    account_id: &str,
    request: &CodexResponsesRequest<'_>,
    cancelled: Option<&Arc<AtomicBool>>,
) -> Result<Response> {
    let mut retries = 0usize;
    loop {
        if cancelled.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            anyhow::bail!("aborted");
        }
        let response = cancel::race_timeout(
            client
                .post(url)
                .bearer_auth(api_key)
                .header("chatgpt-account-id", account_id)
                .header("originator", "ferrum")
                .header("OpenAI-Beta", "responses=experimental")
                .header("content-type", "application/json")
                .json(request)
                .send(),
            cancelled,
            PROVIDER_INITIAL_RESPONSE_TIMEOUT,
        )
        .await;
        let response = match response {
            Ok(response) => response,
            Err(WaitError::Cancelled) => anyhow::bail!("aborted"),
            Err(WaitError::TimedOut) => anyhow::bail!(
                "OpenAI Codex did not respond within {}s; the request may have reached the provider, so Ferrum did not retry it",
                PROVIDER_INITIAL_RESPONSE_TIMEOUT.as_secs()
            ),
        };
        match response {
            Ok(response) if response.status().is_success() => return Ok(response),
            Ok(response) => {
                let status = response.status();
                let retryable = is_retryable_codex_status(status);
                let retry_after = retry_after_delay(response.headers());
                let body = match collect_provider_body(
                    response,
                    cancelled,
                    false,
                    "OpenAI Codex error response",
                )
                .await
                {
                    Ok(body) => body,
                    Err(error) => return Err(final_codex_error(error, retries)),
                };
                let error = provider_status_error("OpenAI Codex", status, &body);
                if retryable && retries < CODEX_MAX_RETRIES {
                    retries += 1;
                    sleep_before_codex_retry(retries, &error.to_string(), retry_after, cancelled)
                        .await?;
                    continue;
                }
                return Err(final_codex_error(error, retries));
            }
            Err(error) => {
                let retryable = is_retryable_codex_send_error(&error);
                let error = anyhow::Error::new(error).context("OpenAI Codex request failed");
                if retryable && retries < CODEX_MAX_RETRIES {
                    retries += 1;
                    sleep_before_codex_retry(retries, &error.to_string(), None, cancelled).await?;
                    continue;
                }
                return Err(final_codex_error(error, retries));
            }
        }
    }
}

fn is_retryable_codex_status(status: StatusCode) -> bool {
    status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

fn is_retryable_codex_send_error(error: &reqwest::Error) -> bool {
    error.is_connect()
}

fn retry_after_delay(headers: &header::HeaderMap) -> Option<Duration> {
    let value = headers.get(header::RETRY_AFTER)?.to_str().ok()?.trim();
    let delay = value
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
        .or_else(|| {
            let when = httpdate::parse_http_date(value).ok()?;
            Some(
                when.duration_since(std::time::SystemTime::now())
                    .unwrap_or(Duration::ZERO),
            )
        })?;
    Some(delay.min(CODEX_MAX_RETRY_AFTER))
}

fn final_codex_error(error: anyhow::Error, retries: usize) -> anyhow::Error {
    if error.to_string() == "aborted" || retries == 0 {
        return error;
    }
    let summary = truncate_to_max_bytes(&error.to_string(), 1_000);
    eprintln!(
        "[provider] OpenAI Codex request failed after {retries} retries ({} total attempts); no retries remain: {summary}",
        retries + 1
    );
    error.context(format!(
        "OpenAI Codex request failed after {retries} retries ({} total attempts); no retries remain",
        retries + 1
    ))
}

async fn sleep_before_codex_retry(
    retry: usize,
    reason: &str,
    retry_after: Option<Duration>,
    cancelled: Option<&Arc<AtomicBool>>,
) -> Result<()> {
    let exponential = CODEX_RETRY_BASE_DELAY * (1 << (retry - 1));
    let delay = retry_after.unwrap_or(exponential);
    let reason = truncate_to_max_bytes(reason, 1_000);
    eprintln!(
        "[provider] {reason}; retrying in {:.1}s ({retry}/{CODEX_MAX_RETRIES})",
        delay.as_secs_f64()
    );
    match cancel::race(tokio::time::sleep(delay), cancelled).await {
        Ok(()) => Ok(()),
        Err(WaitError::Cancelled) => anyhow::bail!("aborted"),
        Err(WaitError::TimedOut) => unreachable!("cancellation race has no timeout"),
    }
}

#[derive(Debug, Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<OpenAiTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'static str>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<ChatStreamOptions>,
}

#[derive(Debug, Serialize)]
struct ChatStreamOptions {
    include_usage: bool,
}

#[derive(Debug, Serialize)]
struct ChatMessage {
    role: &'static str,
    content: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ChatToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_content: Option<String>,
}

#[derive(Debug, Serialize)]
struct ChatToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    function: ChatToolFunction,
}

#[derive(Debug, Serialize)]
struct ChatToolFunction {
    name: String,
    arguments: String,
}

impl ChatMessage {
    fn from_message(message: &Message) -> Self {
        let role = match message.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        let has_images = message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Image { .. }));
        let text = message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                ContentBlock::ToolResult { content, .. } => Some(content.as_str()),
                ContentBlock::ToolUse { .. }
                | ContentBlock::Image { .. }
                | ContentBlock::Thinking { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("");
        let content = if has_images {
            serde_json::Value::Array(
                message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } if !text.is_empty() => {
                            Some(serde_json::json!({
                                "type": "text",
                                "text": text,
                            }))
                        }
                        ContentBlock::Image {
                            mime_type,
                            data_base64,
                            ..
                        } => Some(serde_json::json!({
                            "type": "image_url",
                            "image_url": {
                                "url": format!("data:{mime_type};base64,{data_base64}")
                            }
                        })),
                        ContentBlock::ToolResult { content, .. } if !content.is_empty() => {
                            Some(serde_json::json!({"type": "text", "text": content}))
                        }
                        _ => None,
                    })
                    .collect(),
            )
        } else {
            serde_json::Value::String(text)
        };
        let tool_call_id = message.content.iter().find_map(|block| match block {
            ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.clone()),
            _ => None,
        });
        let tool_calls: Vec<_> = message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolUse { id, name, input } => Some(ChatToolCall {
                    id: id.clone(),
                    kind: "function",
                    function: ChatToolFunction {
                        name: name.clone(),
                        arguments: input.to_string(),
                    },
                }),
                _ => None,
            })
            .collect();
        let has_tool_calls = !tool_calls.is_empty();
        let reasoning_content = message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Thinking { text, .. } if !text.is_empty() => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        Self {
            role,
            content,
            tool_call_id,
            tool_calls: has_tool_calls.then_some(tool_calls),
            reasoning_content: (role == "assistant" && !reasoning_content.is_empty())
                .then_some(reasoning_content),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAiUsage {
    #[serde(alias = "input_tokens", alias = "input")]
    prompt_tokens: Option<u64>,
    #[serde(alias = "output_tokens", alias = "output")]
    completion_tokens: Option<u64>,
    #[serde(alias = "totalTokens", alias = "total")]
    total_tokens: Option<u64>,
    #[serde(alias = "input_tokens_details")]
    prompt_tokens_details: Option<OpenAiPromptTokensDetails>,
}

#[derive(Debug, Deserialize)]
struct OpenAiPromptTokensDetails {
    cached_tokens: Option<u64>,
}

impl OpenAiUsage {
    fn to_token_usage(&self) -> TokenUsage {
        TokenUsage {
            input_tokens: self.prompt_tokens,
            output_tokens: self.completion_tokens,
            total_tokens: self.total_tokens.or_else(|| {
                self.prompt_tokens
                    .zip(self.completion_tokens)
                    .map(|(input, output)| input.saturating_add(output))
            }),
            cache_read_tokens: self
                .prompt_tokens_details
                .as_ref()
                .and_then(|details| details.cached_tokens)
                .unwrap_or(0),
            cache_write_tokens: 0,
            source: "provider".to_string(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatChoiceMessage,
}

#[derive(Debug, Deserialize)]
struct ChatChoiceMessage {
    content: Option<String>,
    reasoning_content: Option<String>,
    reasoning: Option<String>,
    tool_calls: Option<Vec<ChatResponseToolCall>>,
}

#[derive(Debug, Deserialize)]
struct ChatResponseToolCall {
    id: String,
    function: ChatResponseToolFunction,
}

#[derive(Debug, Deserialize)]
struct ChatResponseToolFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Serialize)]
struct OpenAiTool {
    #[serde(rename = "type")]
    kind: &'static str,
    function: OpenAiToolFunction,
}

#[derive(Debug, Serialize)]
struct OpenAiToolFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

fn openai_tools(tools: &[ToolDefinition]) -> Vec<OpenAiTool> {
    tools
        .iter()
        .map(|tool| OpenAiTool {
            kind: "function",
            function: OpenAiToolFunction {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.input_schema.clone(),
            },
        })
        .collect()
}

fn chat_response_to_message(message: Option<ChatChoiceMessage>) -> Result<Message> {
    let Some(message) = message else {
        anyhow::bail!("OpenAI-compatible response produced no choices");
    };
    let mut content = Vec::new();
    if let Some(text) = message
        .reasoning_content
        .or(message.reasoning)
        .filter(|text| !text.trim().is_empty())
    {
        validate_field(&text, MAX_PROVIDER_THINKING_BYTES, "reasoning text")?;
        content.push(ContentBlock::Thinking {
            text,
            signature: None,
        });
    }
    if let Some(text) = message.content.filter(|text| !text.is_empty()) {
        validate_field(&text, MAX_PROVIDER_OUTPUT_BYTES, "output text")?;
        content.push(ContentBlock::Text { text });
    }
    let tool_calls = message.tool_calls.unwrap_or_default();
    if tool_calls.len() > MAX_PARALLEL_TOOL_CALLS {
        anyhow::bail!("OpenAI-compatible response exceeded {MAX_PARALLEL_TOOL_CALLS} tool calls");
    }
    for call in tool_calls {
        validate_field(&call.id, MAX_PROVIDER_FIELD_BYTES, "tool-call id")?;
        validate_field(&call.function.name, MAX_PROVIDER_FIELD_BYTES, "tool name")?;
        validate_field(
            &call.function.arguments,
            MAX_PROVIDER_TOOL_ARGUMENT_BYTES,
            "tool arguments",
        )?;
        let input = serde_json::from_str(&call.function.arguments).with_context(|| {
            format!(
                "failed to parse tool-call arguments for `{}` as JSON",
                call.function.name
            )
        })?;
        content.push(ContentBlock::ToolUse {
            id: call.id,
            name: call.function.name,
            input,
        });
    }
    if content.is_empty() {
        anyhow::bail!("OpenAI-compatible response produced no message content");
    }
    Ok(Message {
        role: Role::Assistant,
        content,
        usage: None,
    })
}

const MAX_PARALLEL_TOOL_CALLS: usize = 128;

#[derive(Default)]
struct ChatSseParser {
    output: String,
    thinking: String,
    tool_calls: Vec<ChatStreamToolCall>,
    usage: Option<TokenUsage>,
    events: usize,
    done: bool,
}

#[derive(Default)]
struct ChatStreamToolCall {
    id: String,
    name: String,
    arguments: String,
}

impl ChatSseParser {
    #[cfg(test)]
    fn process_line(
        &mut self,
        line: &str,
        on_event: Option<&mut (dyn FnMut(StreamEvent) + Send)>,
    ) -> Result<bool> {
        let Some(mut data) = line.strip_prefix("data:") else {
            return Ok(false);
        };
        if let Some(stripped) = data.strip_prefix(' ') {
            data = stripped;
        }
        self.process_data(data, on_event)
    }

    fn process_data(
        &mut self,
        data: &str,
        mut on_event: Option<&mut (dyn FnMut(StreamEvent) + Send)>,
    ) -> Result<bool> {
        if self.done {
            anyhow::bail!("OpenAI-compatible stream emitted data after [DONE]");
        }
        if data == "[DONE]" {
            self.done = true;
            return Ok(true);
        }
        self.events = self.events.saturating_add(1);
        if self.events > MAX_PROVIDER_EVENTS {
            anyhow::bail!("OpenAI-compatible stream exceeded {MAX_PROVIDER_EVENTS} events");
        }
        let event = serde_json::from_str::<serde_json::Value>(data)
            .context("failed to parse OpenAI-compatible SSE event")?;
        event
            .as_object()
            .context("OpenAI-compatible SSE event must be an object")?;
        if event.get("error").is_some_and(|error| !error.is_null()) {
            let message = format!(
                "OpenAI-compatible stream returned an error event: {}",
                truncate_to_max_bytes(data, 1_000)
            );
            if structured_context_overflow_value(&event) {
                return Err(ProviderFailure::ContextOverflow { message }.into());
            }
            anyhow::bail!(message);
        }
        if let Some(usage) = event.get("usage").filter(|usage| !usage.is_null()) {
            let parsed = serde_json::from_value::<OpenAiUsage>(usage.clone())
                .context("failed to parse OpenAI-compatible stream usage")?;
            self.usage = Some(parsed.to_token_usage());
        }
        let choices = event
            .get("choices")
            .map(|value| {
                value
                    .as_array()
                    .context("OpenAI-compatible SSE choices must be an array")
            })
            .transpose()?;
        if choices.is_some_and(|choices| choices.len() > MAX_PARALLEL_TOOL_CALLS) {
            anyhow::bail!("OpenAI-compatible SSE event exceeded {MAX_PARALLEL_TOOL_CALLS} choices");
        }
        for choice in choices.into_iter().flatten() {
            let choice = choice
                .as_object()
                .context("OpenAI-compatible SSE choice must be an object")?;
            let delta = choice
                .get("delta")
                .context("OpenAI-compatible SSE choice missing `delta`")?;
            delta
                .as_object()
                .context("OpenAI-compatible SSE choice `delta` must be an object")?;
            if let Some(text) = optional_string_field(delta, "content", "OpenAI-compatible delta")?
            {
                push_bounded(
                    &mut self.output,
                    text,
                    MAX_PROVIDER_OUTPUT_BYTES,
                    "output text",
                )?;
                if let Some(on_event) = on_event.as_deref_mut() {
                    on_event(StreamEvent::TextDelta(text.to_string()));
                }
            }
            let reasoning =
                optional_string_field(delta, "reasoning_content", "OpenAI-compatible delta")?.or(
                    optional_string_field(delta, "reasoning", "OpenAI-compatible delta")?,
                );
            if let Some(text) = reasoning {
                let text = sanitize_thinking_text(text);
                if text.is_empty() {
                    continue;
                }
                push_bounded(
                    &mut self.thinking,
                    &text,
                    MAX_PROVIDER_THINKING_BYTES,
                    "reasoning text",
                )?;
                if let Some(on_event) = on_event.as_deref_mut() {
                    on_event(StreamEvent::ThinkingDelta(text));
                }
            }
            let tool_calls = delta
                .get("tool_calls")
                .map(|value| {
                    value
                        .as_array()
                        .context("OpenAI-compatible delta `tool_calls` must be an array")
                })
                .transpose()?;
            for tool_call in tool_calls.into_iter().flatten() {
                tool_call
                    .as_object()
                    .context("OpenAI-compatible streamed tool call must be an object")?;
                let raw_index = match tool_call.get("index") {
                    None => self.tool_calls.len() as u64,
                    Some(value) => value
                        .as_u64()
                        .context("streamed tool-call index must be a non-negative integer")?,
                };
                let index = usize::try_from(raw_index)
                    .context("streamed tool-call index does not fit in memory")?;
                if index >= MAX_PARALLEL_TOOL_CALLS {
                    anyhow::bail!(
                        "streamed tool-call index {index} exceeds maximum {}",
                        MAX_PARALLEL_TOOL_CALLS - 1
                    );
                }
                if index > self.tool_calls.len() {
                    anyhow::bail!(
                        "streamed tool-call index {index} is sparse; next valid index is {}",
                        self.tool_calls.len()
                    );
                }
                if index == self.tool_calls.len() {
                    self.tool_calls.push(ChatStreamToolCall::default());
                }
                let current = &mut self.tool_calls[index];
                if let Some(id) =
                    optional_string_field(tool_call, "id", "OpenAI-compatible tool call")?
                {
                    validate_field(id, MAX_PROVIDER_FIELD_BYTES, "tool-call id")?;
                    current.id = id.to_string();
                }
                if let Some(function) = tool_call.get("function") {
                    function
                        .as_object()
                        .context("OpenAI-compatible tool `function` must be an object")?;
                    let name =
                        optional_string_field(function, "name", "OpenAI-compatible tool function")?;
                    if let Some(name) = name {
                        push_bounded(
                            &mut current.name,
                            name,
                            MAX_PROVIDER_FIELD_BYTES,
                            "tool name",
                        )?;
                    }
                    if let Some(arguments) = optional_string_field(
                        function,
                        "arguments",
                        "OpenAI-compatible tool function",
                    )? {
                        push_bounded(
                            &mut current.arguments,
                            arguments,
                            MAX_PROVIDER_TOOL_ARGUMENT_BYTES,
                            "tool arguments",
                        )?;
                    }
                }
            }
        }
        Ok(false)
    }

    fn finish(self) -> Result<ProviderResponse> {
        if !self.done {
            anyhow::bail!("OpenAI-compatible stream ended without [DONE]");
        }
        let mut content = Vec::new();
        if !self.thinking.trim().is_empty() {
            content.push(ContentBlock::Thinking {
                text: self.thinking.trim().to_string(),
                signature: None,
            });
        }
        if !self.output.is_empty() {
            content.push(ContentBlock::Text { text: self.output });
        }
        for call in self.tool_calls {
            if call.name.is_empty() {
                anyhow::bail!("OpenAI-compatible stream produced a tool call without a name");
            }
            let input = serde_json::from_str(&call.arguments).with_context(|| {
                format!(
                    "failed to parse streamed tool-call arguments for `{}` as JSON",
                    call.name
                )
            })?;
            content.push(ContentBlock::ToolUse {
                id: if call.id.is_empty() {
                    format!("call_{}", content.len())
                } else {
                    call.id
                },
                name: call.name,
                input,
            });
        }
        if content.is_empty() {
            anyhow::bail!("OpenAI-compatible stream produced no message content");
        }
        Ok(ProviderResponse {
            message: Message {
                role: Role::Assistant,
                content,
                usage: None,
            },
            usage: self.usage,
        })
    }
}

#[derive(Debug, Serialize)]
struct CodexResponsesRequest<'a> {
    model: &'a str,
    store: bool,
    stream: bool,
    instructions: &'a str,
    input: Vec<ResponsesInput>,
    text: CodexText<'a>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    include: Vec<&'a str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<CodexTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<CodexReasoning<'a>>,
    parallel_tool_calls: bool,
}

#[derive(Debug, Serialize)]
struct CodexReasoning<'a> {
    effort: &'a str,
    summary: &'a str,
}

#[derive(Debug, Serialize)]
struct CodexTool {
    #[serde(rename = "type")]
    kind: &'static str,
    name: String,
    description: String,
    parameters: serde_json::Value,
    strict: Option<bool>,
}

fn codex_tools(tools: &[ToolDefinition]) -> Vec<CodexTool> {
    tools
        .iter()
        .map(|tool| CodexTool {
            kind: "function",
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: tool.input_schema.clone(),
            strict: None,
        })
        .collect()
}

#[derive(Debug, Serialize)]
struct CodexText<'a> {
    verbosity: &'a str,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum ResponsesInput {
    Raw(serde_json::Value),
    Message {
        role: &'static str,
        content: serde_json::Value,
    },
    FunctionCall {
        #[serde(rename = "type")]
        kind: &'static str,
        id: String,
        call_id: String,
        name: String,
        arguments: String,
    },
    FunctionCallOutput {
        #[serde(rename = "type")]
        kind: &'static str,
        call_id: String,
        output: String,
    },
}

fn codex_instructions(messages: &[Message]) -> String {
    let mut instructions = String::from("You are a helpful coding assistant.");
    for message in messages {
        if matches!(message.role, Role::System) {
            let text = message.text_content();
            if !text.trim().is_empty() {
                instructions.push_str("\n\n");
                instructions.push_str(&text);
            }
        }
    }
    instructions
}

fn codex_message_content(message: &Message) -> serde_json::Value {
    let has_images = message
        .content
        .iter()
        .any(|block| matches!(block, ContentBlock::Image { .. }));
    if !has_images {
        return serde_json::Value::String(message.text_content());
    }
    serde_json::Value::Array(
        message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } if !text.is_empty() => Some(serde_json::json!({
                    "type": "input_text",
                    "text": text,
                })),
                ContentBlock::Image {
                    mime_type,
                    data_base64,
                    ..
                } => Some(serde_json::json!({
                    "type": "input_image",
                    "image_url": format!("data:{mime_type};base64,{data_base64}"),
                })),
                _ => None,
            })
            .collect(),
    )
}

fn codex_inputs(messages: &[Message]) -> Vec<ResponsesInput> {
    let completed_tool_call_ids = messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolResult { tool_use_id, .. } => Some(
                tool_use_id
                    .split('|')
                    .next()
                    .unwrap_or(tool_use_id)
                    .to_string(),
            ),
            _ => None,
        })
        .collect::<std::collections::HashSet<_>>();

    let mut inputs = Vec::new();
    for message in messages {
        if matches!(message.role, Role::System) {
            continue;
        }
        for block in &message.content {
            match block {
                ContentBlock::Thinking {
                    signature: Some(signature),
                    ..
                } => {
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(signature) {
                        inputs.push(ResponsesInput::Raw(value));
                    }
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => {
                    inputs.push(ResponsesInput::FunctionCallOutput {
                        kind: "function_call_output",
                        call_id: tool_use_id
                            .split('|')
                            .next()
                            .unwrap_or(tool_use_id)
                            .to_string(),
                        output: content.clone(),
                    });
                }
                ContentBlock::ToolUse { id, name, input } => {
                    let call_id = id.split('|').next().unwrap_or(id).to_string();
                    if completed_tool_call_ids.contains(&call_id) {
                        let item_id = id
                            .split('|')
                            .nth(1)
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("fc_{call_id}"));
                        inputs.push(ResponsesInput::FunctionCall {
                            kind: "function_call",
                            id: item_id,
                            call_id,
                            name: name.clone(),
                            arguments: input.to_string(),
                        });
                    }
                }
                ContentBlock::Text { .. }
                | ContentBlock::Image { .. }
                | ContentBlock::Thinking { .. } => {}
            }
        }
        if has_codex_message_content(message) {
            let role = match message.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "user",
            };
            inputs.push(ResponsesInput::Message {
                role,
                content: codex_message_content(message),
            });
        }
    }
    inputs
}

fn has_codex_message_content(message: &Message) -> bool {
    message.content.iter().any(|block| match block {
        ContentBlock::Text { text } => !text.is_empty(),
        ContentBlock::Image { .. } => true,
        _ => false,
    })
}

#[derive(Default)]
struct ResponsesSseParser {
    output: String,
    thinking: String,
    thinking_comment_open: bool,
    thinking_comment_pending: String,
    thinking_summary_part: Option<(u64, u64)>,
    thinking_signature: Option<String>,
    usage: Option<TokenUsage>,
    tool_calls: Vec<(String, String, String, String)>,
    current_calls: BTreeMap<String, (String, String, String, String)>,
    pending_call_args: BTreeMap<String, String>,
    error: Option<String>,
    events: usize,
    completed: bool,
}

impl ResponsesSseParser {
    #[cfg(test)]
    fn process_line(
        &mut self,
        line: &str,
        on_event: Option<&mut (dyn FnMut(StreamEvent) + Send)>,
    ) -> Result<bool> {
        let Some(mut data) = line.strip_prefix("data:") else {
            return Ok(false);
        };
        if let Some(stripped) = data.strip_prefix(' ') {
            data = stripped;
        }
        self.process_data(data, on_event)
    }

    fn process_data(
        &mut self,
        data: &str,
        mut on_event: Option<&mut (dyn FnMut(StreamEvent) + Send)>,
    ) -> Result<bool> {
        if data == "[DONE]" {
            return Ok(true);
        }
        if self.completed {
            anyhow::bail!("OpenAI Codex stream emitted data after `response.completed`");
        }
        self.events = self.events.saturating_add(1);
        if self.events > MAX_PROVIDER_EVENTS {
            anyhow::bail!("OpenAI Codex stream exceeded {MAX_PROVIDER_EVENTS} events");
        }
        let event = serde_json::from_str::<serde_json::Value>(data)
            .context("failed to parse OpenAI Codex SSE event")?;
        let event_type = event
            .get("type")
            .and_then(|value| value.as_str())
            .context("OpenAI Codex SSE event missing string `type`")?;
        validate_field(event_type, MAX_PROVIDER_FIELD_BYTES, "event type")?;
        if matches!(
            event_type,
            "response.failed" | "response.incomplete" | "error"
        ) {
            let message = format!("OpenAI Codex terminal failure event `{event_type}`");
            if structured_context_overflow_value(&event) {
                return Err(ProviderFailure::ContextOverflow { message }.into());
            }
            anyhow::bail!(message);
        }
        if let Some(usage) = event
            .get("response")
            .and_then(|response| response.get("usage"))
            .or_else(|| event.get("usage"))
            .filter(|usage| !usage.is_null())
        {
            self.absorb_usage(usage)?;
        }
        let event_type = Some(event_type);
        if let Some(event_type) = event_type {
            emit_codex_usage_metrics_if_enabled(event_type, &event);
            if event_type.contains("reasoning") && event_type.contains("summary") {
                let summary_part = reasoning_summary_part(&event);
                if let Some(delta) = optional_string_field(&event, "delta", event_type)? {
                    self.append_thinking_delta(delta, summary_part.as_ref(), &mut on_event)?;
                }
                if let Some(text) = optional_string_field(&event, "text", event_type)? {
                    self.absorb_completed_thinking(text, summary_part.as_ref(), &mut on_event)?;
                }
            } else if event_type == "response.reasoning_text.delta"
                && let Some(delta) = optional_string_field(&event, "delta", event_type)?
            {
                self.append_thinking_delta(delta, None, &mut on_event)?;
            }
        }
        match event_type {
            Some("response.output_text.delta") => {
                let delta = optional_string_field(&event, "delta", "response.output_text.delta")?
                    .context("OpenAI Codex output-text delta event missing `delta`")?;
                push_bounded(
                    &mut self.output,
                    delta,
                    MAX_PROVIDER_OUTPUT_BYTES,
                    "output text",
                )?;
                if let Some(on_event) = on_event.as_mut() {
                    on_event(StreamEvent::TextDelta(delta.to_string()));
                }
            }
            Some("response.output_text.done") => {
                let text = optional_string_field(&event, "text", "response.output_text.done")?
                    .context("OpenAI Codex output-text done event missing `text`")?;
                self.append_completed_output(text, None)?;
            }
            Some("response.completed") => {
                if self.completed {
                    anyhow::bail!("OpenAI Codex stream emitted duplicate `response.completed`");
                }
                self.completed = true;
                let response = event
                    .get("response")
                    .and_then(|value| value.as_object().map(|_| value))
                    .context("OpenAI Codex completed event missing object `response`")?;
                self.absorb_completed_response(response)?;
            }
            Some("response.output_item.added") => {
                let item = event
                    .get("item")
                    .and_then(|value| value.as_object().map(|_| value))
                    .context("OpenAI Codex output-item added event missing object `item`")?;
                if item.get("type").and_then(|value| value.as_str()) == Some("function_call") {
                    match parse_response_function_call_added_item(item) {
                        Ok(mut call) => {
                            let key = response_event_call_key(&event)
                                .unwrap_or_else(|| response_call_key(item, &call));
                            validate_field(&key, MAX_PROVIDER_FIELD_BYTES, "tool-call key")?;
                            if let Some(args) = self.pending_call_args.remove(&key) {
                                push_bounded(
                                    &mut call.3,
                                    &args,
                                    MAX_PROVIDER_TOOL_ARGUMENT_BYTES,
                                    "tool arguments",
                                )?;
                            }
                            validate_provider_call(&call)?;
                            if !self.current_calls.contains_key(&key)
                                && self.current_calls.len() >= MAX_PARALLEL_TOOL_CALLS
                            {
                                anyhow::bail!(
                                    "OpenAI Codex stream exceeded {MAX_PARALLEL_TOOL_CALLS} concurrent tool calls"
                                );
                            }
                            self.current_calls.insert(key, call);
                        }
                        Err(error) => self.error = Some(error.to_string()),
                    }
                }
            }
            Some("response.function_call_arguments.delta") => {
                let delta = optional_string_field(
                    &event,
                    "delta",
                    "response.function_call_arguments.delta",
                )?
                .context("OpenAI Codex function-call arguments delta event missing `delta`")?;
                self.update_current_call_args(&event, |args| {
                    push_bounded(
                        args,
                        delta,
                        MAX_PROVIDER_TOOL_ARGUMENT_BYTES,
                        "tool arguments",
                    )
                })?;
            }
            Some("response.function_call_arguments.done") => {
                let done = optional_string_field(
                    &event,
                    "arguments",
                    "response.function_call_arguments.done",
                )?
                .context("OpenAI Codex function-call arguments done event missing `arguments`")?;
                validate_field(done, MAX_PROVIDER_TOOL_ARGUMENT_BYTES, "tool arguments")?;
                self.update_current_call_args(&event, |args| {
                    *args = done.to_string();
                    Ok(())
                })?;
            }
            Some("response.output_item.done") => {
                let item = event
                    .get("item")
                    .and_then(|value| value.as_object().map(|_| value))
                    .context("OpenAI Codex output-item done event missing object `item`")?;
                if item.get("type").and_then(|value| value.as_str()) == Some("reasoning") {
                    let final_thinking = thinking_text_from_item(item);
                    if !final_thinking.trim().is_empty() {
                        validate_field(
                            &final_thinking,
                            MAX_PROVIDER_THINKING_BYTES,
                            "reasoning text",
                        )?;
                        self.thinking = final_thinking;
                    }
                    let signature = item.to_string();
                    validate_field(
                        &signature,
                        MAX_PROVIDER_TOOL_ARGUMENT_BYTES,
                        "reasoning signature",
                    )?;
                    self.thinking_signature = Some(signature);
                }
                if item.get("type").and_then(|value| value.as_str()) == Some("function_call") {
                    match parse_response_function_call_item(item) {
                        Ok(call) => {
                            validate_provider_call(&call)?;
                            let key = response_event_call_key(&event)
                                .unwrap_or_else(|| response_call_key(item, &call));
                            self.current_calls.remove(&key);
                            if self.tool_calls.len() >= MAX_PARALLEL_TOOL_CALLS {
                                anyhow::bail!(
                                    "OpenAI Codex stream exceeded {MAX_PARALLEL_TOOL_CALLS} tool calls"
                                );
                            }
                            self.tool_calls.push(call);
                        }
                        Err(error) => self.error = Some(error.to_string()),
                    }
                }
            }
            _ => {}
        }
        Ok(self.completed)
    }

    fn append_thinking_delta(
        &mut self,
        text: &str,
        summary_part: Option<&(u64, u64)>,
        on_event: &mut Option<&mut (dyn FnMut(StreamEvent) + Send)>,
    ) -> Result<()> {
        validate_field(text, MAX_PROVIDER_THINKING_BYTES, "reasoning delta")?;
        let text = self.sanitize_thinking_delta(text);
        if text.is_empty() {
            return Ok(());
        }
        self.start_thinking_summary_part(summary_part, on_event)?;
        let delta = self.merge_thinking_text(&text);
        validate_field(
            &self.thinking,
            MAX_PROVIDER_THINKING_BYTES,
            "reasoning text",
        )?;
        if delta.is_empty() {
            return Ok(());
        }
        if let Some(on_event) = on_event.as_deref_mut() {
            on_event(StreamEvent::ThinkingDelta(delta));
        }
        Ok(())
    }

    fn absorb_completed_thinking(
        &mut self,
        text: &str,
        summary_part: Option<&(u64, u64)>,
        on_event: &mut Option<&mut (dyn FnMut(StreamEvent) + Send)>,
    ) -> Result<()> {
        validate_field(text, MAX_PROVIDER_THINKING_BYTES, "completed reasoning")?;
        let text = sanitize_thinking_text(text);
        if text.is_empty() {
            return Ok(());
        }
        self.start_thinking_summary_part(summary_part, on_event)?;
        let delta = self.merge_thinking_text(&text);
        validate_field(
            &self.thinking,
            MAX_PROVIDER_THINKING_BYTES,
            "reasoning text",
        )?;
        if !delta.is_empty()
            && let Some(on_event) = on_event.as_deref_mut()
        {
            on_event(StreamEvent::ThinkingDelta(delta));
        }
        Ok(())
    }

    fn start_thinking_summary_part(
        &mut self,
        summary_part: Option<&(u64, u64)>,
        on_event: &mut Option<&mut (dyn FnMut(StreamEvent) + Send)>,
    ) -> Result<()> {
        let Some(summary_part) = summary_part else {
            return Ok(());
        };
        if self.thinking_summary_part.as_ref() == Some(summary_part) {
            return Ok(());
        }
        let changed = self.thinking_summary_part.is_some();
        self.thinking_summary_part = Some(*summary_part);
        if !changed || self.thinking.is_empty() {
            return Ok(());
        }
        let separator = if self.thinking.ends_with("\n\n") {
            ""
        } else if self.thinking.ends_with('\n') {
            "\n"
        } else {
            "\n\n"
        };
        push_bounded(
            &mut self.thinking,
            separator,
            MAX_PROVIDER_THINKING_BYTES,
            "reasoning text",
        )?;
        if !separator.is_empty()
            && let Some(on_event) = on_event.as_deref_mut()
        {
            on_event(StreamEvent::ThinkingDelta(separator.to_string()));
        }
        Ok(())
    }

    fn sanitize_thinking_delta(&mut self, text: &str) -> String {
        let combined;
        let mut rest = if self.thinking_comment_pending.is_empty() {
            text
        } else {
            combined = format!("{}{}", self.thinking_comment_pending, text);
            self.thinking_comment_pending.clear();
            combined.as_str()
        };
        let mut output = String::with_capacity(rest.len());
        loop {
            if self.thinking_comment_open {
                let Some(end) = rest.find("-->") else {
                    return output;
                };
                self.thinking_comment_open = false;
                rest = &rest[end + "-->".len()..];
                continue;
            }
            let Some(start) = rest.find("<!--") else {
                let pending_len = partial_comment_prefix_suffix_len(rest);
                if pending_len > 0 {
                    let split = rest.len() - pending_len;
                    output.push_str(&rest[..split]);
                    self.thinking_comment_pending = rest[split..].to_string();
                } else {
                    output.push_str(rest);
                }
                return output;
            };
            output.push_str(&rest[..start]);
            let after_start = &rest[start + "<!--".len()..];
            let Some(end) = after_start.find("-->") else {
                self.thinking_comment_open = true;
                return output;
            };
            rest = &after_start[end + "-->".len()..];
        }
    }

    fn merge_thinking_text(&mut self, incoming: &str) -> String {
        if incoming.is_empty() {
            return String::new();
        }
        let current = self.thinking.as_str();
        if current == incoming || current.trim() == incoming.trim() {
            self.thinking = incoming.to_string();
            return String::new();
        }
        if current.ends_with(incoming) || current.trim_end().ends_with(incoming.trim()) {
            return String::new();
        }
        if let Some(delta) = incoming.strip_prefix(current) {
            self.thinking.push_str(delta);
            return delta.to_string();
        }
        if let Some(delta) = incoming.trim_start().strip_prefix(current.trim_start()) {
            self.thinking = incoming.to_string();
            return delta.to_string();
        }
        let current_flat = collapse_whitespace(current);
        let incoming_flat = collapse_whitespace(incoming);
        if current_flat == incoming_flat {
            self.thinking = incoming_flat;
            return String::new();
        }
        if incoming_flat.starts_with(&current_flat) {
            let delta = incoming_flat[current_flat.len()..].to_string();
            self.thinking = incoming_flat;
            return delta;
        }
        if current_flat.ends_with(&incoming_flat) {
            self.thinking = current_flat;
            return String::new();
        }
        self.thinking.push_str(incoming);
        incoming.to_string()
    }

    fn append_completed_output(
        &mut self,
        text: &str,
        on_event: Option<&mut (dyn FnMut(StreamEvent) + Send)>,
    ) -> Result<()> {
        if text.is_empty() || self.output.ends_with(text) {
            return Ok(());
        }
        let delta = text.strip_prefix(&self.output).unwrap_or(text).to_string();
        push_bounded(
            &mut self.output,
            &delta,
            MAX_PROVIDER_OUTPUT_BYTES,
            "output text",
        )?;
        if !delta.is_empty()
            && let Some(on_event) = on_event
        {
            on_event(StreamEvent::TextDelta(delta));
        }
        Ok(())
    }

    fn update_current_call_args(
        &mut self,
        event: &serde_json::Value,
        update: impl FnOnce(&mut String) -> Result<()>,
    ) -> Result<()> {
        let Some(key) = response_event_call_key(event) else {
            if self.current_calls.len() == 1 {
                if let Some((_, _, _, args)) = self.current_calls.values_mut().next() {
                    update(args)?;
                }
            } else if !self.current_calls.is_empty() {
                self.error = Some(
                    "OpenAI Codex function_call arguments event did not identify which parallel call to update"
                        .to_string(),
                );
            }
            return Ok(());
        };
        validate_field(&key, MAX_PROVIDER_FIELD_BYTES, "tool-call key")?;
        if let Some((_, _, _, args)) = self.current_calls.get_mut(&key) {
            update(args)?;
        } else {
            if !self.pending_call_args.contains_key(&key)
                && self.pending_call_args.len() >= MAX_PARALLEL_TOOL_CALLS
            {
                anyhow::bail!(
                    "OpenAI Codex stream exceeded {MAX_PARALLEL_TOOL_CALLS} pending tool calls"
                );
            }
            let args = self.pending_call_args.entry(key).or_default();
            update(args)?;
        }
        Ok(())
    }

    fn absorb_usage(&mut self, usage: &serde_json::Value) -> Result<()> {
        let parsed = serde_json::from_value::<OpenAiUsage>(usage.clone())
            .context("failed to parse OpenAI Codex usage")?;
        self.usage = Some(parsed.to_token_usage());
        Ok(())
    }

    fn absorb_completed_response(&mut self, response: &serde_json::Value) -> Result<()> {
        if let Some(usage) = response.get("usage").filter(|usage| !usage.is_null()) {
            self.absorb_usage(usage)?;
        }
        if let Some(text) = extract_responses_text(response)? {
            self.append_completed_output(&text, None)?;
        }
        for item in response
            .get("output")
            .and_then(|value| value.as_array())
            .into_iter()
            .flatten()
        {
            if item.get("type").and_then(|value| value.as_str()) == Some("reasoning") {
                let final_thinking = thinking_text_from_item(item);
                if !final_thinking.trim().is_empty() {
                    validate_field(
                        &final_thinking,
                        MAX_PROVIDER_THINKING_BYTES,
                        "reasoning text",
                    )?;
                    self.thinking = final_thinking;
                }
                let signature = item.to_string();
                validate_field(
                    &signature,
                    MAX_PROVIDER_TOOL_ARGUMENT_BYTES,
                    "reasoning signature",
                )?;
                self.thinking_signature = Some(signature);
            }
            if item.get("type").and_then(|value| value.as_str()) == Some("function_call") {
                match parse_response_function_call_item(item) {
                    Ok(call) => {
                        validate_provider_call(&call)?;
                        if !self
                            .tool_calls
                            .iter()
                            .any(|existing| existing.0 == call.0 || existing.1 == call.1)
                        {
                            if self.tool_calls.len() >= MAX_PARALLEL_TOOL_CALLS {
                                anyhow::bail!(
                                    "OpenAI Codex stream exceeded {MAX_PARALLEL_TOOL_CALLS} tool calls"
                                );
                            }
                            self.tool_calls.push(call.clone());
                        }
                        self.current_calls.remove(&response_call_key(item, &call));
                    }
                    Err(error) => self.error = Some(error.to_string()),
                }
            }
        }
        Ok(())
    }

    fn finish(mut self) -> Result<ProviderResponse> {
        if let Some(error) = self.error.take() {
            anyhow::bail!(error);
        }
        if !self.completed {
            anyhow::bail!("OpenAI Codex stream ended without `response.completed`");
        }
        for (_, call) in self.current_calls {
            validate_provider_call(&call)?;
            if !self
                .tool_calls
                .iter()
                .any(|existing| existing.0 == call.0 || existing.1 == call.1)
            {
                if self.tool_calls.len() >= MAX_PARALLEL_TOOL_CALLS {
                    anyhow::bail!(
                        "OpenAI Codex stream exceeded {MAX_PARALLEL_TOOL_CALLS} tool calls"
                    );
                }
                self.tool_calls.push(call);
            }
        }

        let mut content = Vec::new();
        if !self.thinking.trim().is_empty() {
            content.push(ContentBlock::Thinking {
                text: self.thinking.trim().to_string(),
                signature: self.thinking_signature,
            });
        }
        if !self.output.is_empty() {
            content.push(ContentBlock::Text { text: self.output });
        }
        for (call_id, item_id, name, args) in self.tool_calls {
            let input = serde_json::from_str(&args).with_context(|| {
                format!("failed to parse Codex tool-call arguments for `{name}` as JSON")
            })?;
            content.push(ContentBlock::ToolUse {
                id: format!("{call_id}|{item_id}"),
                name,
                input,
            });
        }
        if content.is_empty() {
            anyhow::bail!("OpenAI Codex stream produced no message content");
        }
        Ok(ProviderResponse {
            message: Message {
                role: Role::Assistant,
                content,
                usage: None,
            },
            usage: self.usage,
        })
    }
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn reasoning_summary_part(event: &serde_json::Value) -> Option<(u64, u64)> {
    let output_index = event.get("output_index")?.as_u64()?;
    let summary_index = event.get("summary_index")?.as_u64()?;
    Some((output_index, summary_index))
}

fn partial_comment_prefix_suffix_len(text: &str) -> usize {
    ["<!--", "<!-", "<!", "<"]
        .iter()
        .find_map(|prefix| text.ends_with(prefix).then_some(prefix.len()))
        .unwrap_or(0)
}

fn response_call_key(item: &serde_json::Value, call: &(String, String, String, String)) -> String {
    item.get("id")
        .and_then(|value| value.as_str())
        .or_else(|| item.get("item_id").and_then(|value| value.as_str()))
        .map(str::to_string)
        .unwrap_or_else(|| call.1.clone())
}

fn response_event_call_key(event: &serde_json::Value) -> Option<String> {
    event
        .get("item_id")
        .and_then(|value| value.as_str())
        .or_else(|| {
            event
                .get("item")
                .and_then(|item| item.get("id"))
                .and_then(|value| value.as_str())
        })
        .or_else(|| event.get("call_id").and_then(|value| value.as_str()))
        .or_else(|| {
            event
                .get("item")
                .and_then(|item| item.get("call_id"))
                .and_then(|value| value.as_str())
        })
        .map(str::to_string)
        .or_else(|| {
            event
                .get("output_index")
                .and_then(|value| value.as_u64())
                .map(|index| index.to_string())
        })
}

fn emit_codex_usage_metrics_if_enabled(event_type: &str, event: &serde_json::Value) {
    if !metrics_enabled() || !event_type.starts_with("response.") {
        return;
    }
    let Some(usage) = event
        .get("response")
        .and_then(|response| response.get("usage"))
    else {
        return;
    };
    let input = usage
        .get("input_tokens")
        .or_else(|| usage.get("input"))
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let output = usage
        .get("output_tokens")
        .or_else(|| usage.get("output"))
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let total = usage
        .get("total_tokens")
        .or_else(|| usage.get("totalTokens"))
        .or_else(|| usage.get("total"))
        .and_then(|value| value.as_u64())
        .unwrap_or_else(|| input.saturating_add(output));
    let cache_read = usage
        .get("input_tokens_details")
        .and_then(|details| details.get("cached_tokens"))
        .or_else(|| usage.get("cache_read_input_tokens"))
        .or_else(|| usage.get("cacheRead"))
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let cache_write = usage
        .get("cache_write_input_tokens")
        .or_else(|| usage.get("cacheWrite"))
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let response_id = event
        .get("response")
        .and_then(|response| response.get("id"))
        .and_then(|value| value.as_str())
        .unwrap_or("");
    eprintln!(
        "[metrics:codex usage] event={event_type} response_id={response_id} input_tokens={input} output_tokens={output} cached_input_tokens={cache_read} cache_write_input_tokens={cache_write} total_tokens={total}"
    );
}

#[cfg(test)]
fn extract_sse_responses_message(text: &str) -> Result<Message> {
    let mut parser = ResponsesSseParser::default();
    for line in text.lines() {
        parser.process_line(line, None)?;
    }
    parser.finish().map(|response| response.message)
}

fn validate_provider_call(call: &(String, String, String, String)) -> Result<()> {
    validate_field(&call.0, MAX_PROVIDER_FIELD_BYTES, "tool call id")?;
    validate_field(&call.1, MAX_PROVIDER_FIELD_BYTES, "tool item id")?;
    validate_field(&call.2, MAX_PROVIDER_FIELD_BYTES, "tool name")?;
    validate_field(&call.3, MAX_PROVIDER_TOOL_ARGUMENT_BYTES, "tool arguments")
}

fn parse_response_function_call_added_item(
    item: &serde_json::Value,
) -> Result<(String, String, String, String)> {
    let call_id = required_non_empty_string(item, "call_id")?.to_string();
    let item_id = required_non_empty_string(item, "id")?.to_string();
    let name = required_non_empty_string(item, "name")?.to_string();
    let arguments = item
        .get("arguments")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    Ok((call_id, item_id, name, arguments))
}

fn parse_response_function_call_item(
    item: &serde_json::Value,
) -> Result<(String, String, String, String)> {
    let call_id = required_non_empty_string(item, "call_id")?.to_string();
    let item_id = required_non_empty_string(item, "id")?.to_string();
    let name = required_non_empty_string(item, "name")?.to_string();
    let arguments = required_string(item, "arguments")?.to_string();
    Ok((call_id, item_id, name, arguments))
}

fn required_non_empty_string<'a>(item: &'a serde_json::Value, field: &str) -> Result<&'a str> {
    let value = required_string(item, field)?;
    if value.trim().is_empty() {
        anyhow::bail!("OpenAI Codex function_call missing non-empty `{field}`");
    }
    Ok(value)
}

fn required_string<'a>(item: &'a serde_json::Value, field: &str) -> Result<&'a str> {
    item.get(field)
        .and_then(|value| value.as_str())
        .with_context(|| format!("OpenAI Codex function_call missing string `{field}`"))
}

fn thinking_text_from_item(item: &serde_json::Value) -> String {
    let summary = item
        .get("summary")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|part| part.get("text").and_then(|value| value.as_str()))
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    if !summary.trim().is_empty() {
        return sanitize_thinking_text(&summary);
    }

    let content = item
        .get("content")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|part| part.get("text").and_then(|value| value.as_str()))
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    if !content.trim().is_empty() {
        return sanitize_thinking_text(&content);
    }

    sanitize_thinking_text(
        item.get("text")
            .and_then(|value| value.as_str())
            .unwrap_or_default(),
    )
}

fn extract_responses_text(body: &serde_json::Value) -> Result<Option<String>> {
    if let Some(value) = body.get("output_text") {
        let text = value
            .as_str()
            .context("OpenAI Codex `output_text` must be a string")?;
        validate_field(text, MAX_PROVIDER_OUTPUT_BYTES, "output text")?;
        return Ok(Some(text.to_string()));
    }
    let Some(output) = body.get("output") else {
        return Ok(None);
    };
    let output = output
        .as_array()
        .context("OpenAI Codex response `output` must be an array")?;
    let mut text = String::new();
    for item in output {
        let Some(content) = item.get("content") else {
            continue;
        };
        let content = content
            .as_array()
            .context("OpenAI Codex output-item `content` must be an array")?;
        for part in content {
            if let Some(part_text) = optional_string_field(part, "text", "OpenAI Codex content")? {
                push_bounded(
                    &mut text,
                    part_text,
                    MAX_PROVIDER_OUTPUT_BYTES,
                    "output text",
                )?;
            }
        }
    }
    Ok(Some(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
        time::Instant,
    };

    fn test_codex_request() -> CodexResponsesRequest<'static> {
        CodexResponsesRequest {
            model: "test-model",
            store: false,
            stream: true,
            instructions: "test",
            input: Vec::new(),
            text: CodexText { verbosity: "low" },
            include: Vec::new(),
            tools: Vec::new(),
            tool_choice: None,
            reasoning: None,
            parallel_tool_calls: false,
        }
    }

    fn spawn_stream_server(
        body: &'static [u8],
        hold_open: Duration,
    ) -> (String, thread::JoinHandle<usize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut request = [0u8; 16 * 1024];
            let _ = stream.read(&mut request);
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            stream.write_all(body).unwrap();
            stream.flush().unwrap();
            thread::sleep(hold_open);
            1
        });
        (format!("http://{address}"), handle)
    }

    fn spawn_retry_server() -> (String, thread::JoinHandle<usize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut requests = 0usize;
            while requests < CODEX_MAX_RETRIES + 1 && Instant::now() < deadline {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("retry test server accept failed: {error}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let mut request = [0u8; 16 * 1024];
                let _ = stream.read(&mut request);
                let body = br#"{"error":{"code":"server_busy"}}"#;
                write!(
                    stream,
                    "HTTP/1.1 503 Service Unavailable\r\nContent-Length: {}\r\nRetry-After: 0\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(body).unwrap();
                requests += 1;
            }
            requests
        });
        (format!("http://{address}/responses"), handle)
    }

    #[test]
    fn replays_openai_compatible_reasoning_content() {
        let message = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Thinking {
                    text: "provider reasoning".to_string(),
                    signature: None,
                },
                ContentBlock::Text {
                    text: "answer".to_string(),
                },
            ],
            usage: None,
        };
        let chat = ChatMessage::from_message(&message);
        assert_eq!(
            chat.reasoning_content.as_deref(),
            Some("provider reasoning")
        );
        let value = serde_json::to_value(chat).unwrap();
        assert_eq!(value["reasoning_content"], "provider reasoning");
    }

    #[test]
    fn stream_error_context_preserves_cancellation_and_typed_overflow() {
        let aborted = provider_stream_error(anyhow::anyhow!("aborted"), "test stream");
        assert_eq!(aborted.to_string(), "aborted");

        let overflow = provider_stream_error(
            ProviderFailure::ContextOverflow {
                message: "overflow".to_string(),
            }
            .into(),
            "test stream",
        );
        assert!(crate::providers::is_context_overflow_error(&overflow));
    }

    #[test]
    fn typed_context_overflow_requires_structured_provider_signal() {
        let structured = provider_status_error(
            "test provider",
            StatusCode::BAD_REQUEST,
            br#"{"error":{"code":"context_length_exceeded","message":"too long"}}"#,
        );
        assert!(crate::providers::is_context_overflow_error(&structured));

        let unstructured = provider_status_error(
            "test provider",
            StatusCode::BAD_REQUEST,
            br#"{"error":{"code":"bad_request","message":"too many tokens"}}"#,
        );
        assert!(!crate::providers::is_context_overflow_error(&unstructured));
    }

    #[test]
    fn unauthorized_status_is_typed_for_refresh_recovery() {
        let error = provider_status_error(
            "test provider",
            StatusCode::UNAUTHORIZED,
            br#"{"error":{"code":"invalid_token"}}"#,
        );
        assert!(crate::providers::is_authentication_error(&error));
    }

    #[test]
    fn parses_bounded_retry_after_values() {
        let mut headers = header::HeaderMap::new();
        headers.insert(header::RETRY_AFTER, header::HeaderValue::from_static("120"));
        assert_eq!(retry_after_delay(&headers), Some(CODEX_MAX_RETRY_AFTER));
        headers.insert(
            header::RETRY_AFTER,
            header::HeaderValue::from_static("invalid"),
        );
        assert_eq!(retry_after_delay(&headers), None);
    }

    #[tokio::test]
    async fn streaming_cancellation_remains_an_abort() {
        let (base_url, server) = spawn_stream_server(b"", Duration::from_millis(200));
        let provider = OpenAiCompatProvider::new(None, base_url, true, false).unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let trigger = Arc::clone(&cancelled);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            trigger.store(true, Ordering::Relaxed);
        });
        let error = provider
            .complete_streaming(
                "test-model",
                &[],
                &[],
                ThinkingLevel::Off,
                &mut |_| {},
                Some(cancelled),
            )
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "aborted");
        assert_eq!(server.join().unwrap(), 1);
    }

    #[tokio::test]
    async fn stream_failure_after_partial_output_is_not_retried() {
        const BODY: &[u8] = b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n";
        let (base_url, server) = spawn_stream_server(BODY, Duration::ZERO);
        let provider = OpenAiCompatProvider::new(None, base_url, true, false).unwrap();
        let mut events = Vec::new();
        let error = provider
            .complete_streaming(
                "test-model",
                &[],
                &[],
                ThinkingLevel::Off,
                &mut |event| events.push(event),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(server.join().unwrap(), 1);
        assert!(error.to_string().contains("did not retry"));
        assert!(matches!(
            events.as_slice(),
            [StreamEvent::TextDelta(text)] if text == "partial"
        ));
    }

    #[tokio::test]
    async fn done_event_stops_reading_before_connection_close() {
        const BODY: &[u8] =
            b"data: {\"choices\":[{\"delta\":{\"content\":\"done\"}}]}\n\ndata:[DONE]\n\n";
        let (base_url, server) = spawn_stream_server(BODY, Duration::from_millis(750));
        let provider = OpenAiCompatProvider::new(None, base_url, true, false).unwrap();
        let mut events = Vec::new();
        let response = tokio::time::timeout(
            Duration::from_millis(500),
            provider.complete_streaming(
                "test-model",
                &[],
                &[],
                ThinkingLevel::Off,
                &mut |event| events.push(event),
                None,
            ),
        )
        .await
        .expect("provider waited for connection close after [DONE]")
        .unwrap();
        assert_eq!(response.message.display_text(), "done");
        assert_eq!(server.join().unwrap(), 1);
    }

    #[tokio::test]
    async fn exhausted_retries_report_attempt_count() {
        let (url, server) = spawn_retry_server();
        let error = send_codex_request_with_retries(
            &Client::new(),
            &url,
            "test-key",
            "test-account",
            &test_codex_request(),
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(server.join().unwrap(), CODEX_MAX_RETRIES + 1);
        let text = error.to_string();
        assert!(text.contains("after 3 retries"));
        assert!(text.contains("4 total attempts"));
        assert!(text.contains("no retries remain"));
    }

    #[test]
    fn rejects_malformed_and_unterminated_provider_streams() {
        let mut chat = ChatSseParser::default();
        let error = chat.process_line("data: {not-json}", None).unwrap_err();
        assert!(error.to_string().contains("failed to parse"));

        let mut codex = ResponsesSseParser::default();
        codex
            .process_line(
                r#"data: {"type":"response.output_text.delta","delta":"partial"}"#,
                None,
            )
            .unwrap();
        let error = codex.finish().unwrap_err();
        assert!(error.to_string().contains("response.completed"));

        let mut terminal = ResponsesSseParser::default();
        let stopped = terminal
            .process_line(
                r#"data: {"type":"response.completed","response":{"output_text":"complete"}}"#,
                None,
            )
            .unwrap();
        assert!(stopped);
        assert_eq!(
            terminal.finish().unwrap().message.display_text(),
            "complete"
        );

        let mut chat = ChatSseParser::default();
        chat.process_line(
            r#"data: {"choices":[{"delta":{"content":"partial"}}]}"#,
            None,
        )
        .unwrap();
        let error = chat.finish().unwrap_err();
        assert!(error.to_string().contains("without [DONE]"));
    }

    #[test]
    fn enforces_provider_field_budgets() {
        let mut chat = ChatSseParser::default();
        let oversized = "x".repeat(MAX_PROVIDER_OUTPUT_BYTES + 1);
        let event = serde_json::json!({"choices":[{"delta":{"content":oversized}}]});
        let error = chat.process_data(&event.to_string(), None).unwrap_err();
        assert!(error.to_string().contains("output text exceeded"));
    }

    #[test]
    fn converts_normalized_text_message() {
        let message = Message::text(Role::User, "hello");
        let chat = ChatMessage::from_message(&message);
        assert_eq!(chat.role, "user");
        assert_eq!(chat.content, "hello");
    }

    #[test]
    fn extracts_chat_stream_usage() {
        let mut parser = ChatSseParser::default();
        parser
            .process_line(r#"data: {"choices":[{"delta":{"content":"hel"}}]}"#, None)
            .unwrap();
        parser
            .process_line(r#"data: {"choices":[{"delta":{"content":"lo"}}]}"#, None)
            .unwrap();
        parser
            .process_line(
                r#"data: {"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":3,"total_tokens":13,"prompt_tokens_details":{"cached_tokens":4}}}"#,
                None,
            )
            .unwrap();
        parser.process_line("data:[DONE]", None).unwrap();

        let response = parser.finish().unwrap();

        assert_eq!(response.message.display_text(), "hello");
        let usage = response.usage.unwrap();
        assert_eq!(usage.source, "provider");
        assert_eq!(usage.input_tokens, Some(10));
        assert_eq!(usage.output_tokens, Some(3));
        assert_eq!(usage.total_tokens, Some(13));
        assert_eq!(usage.cache_read_tokens, 4);
    }

    #[test]
    fn rejects_empty_chat_response() {
        let error = chat_response_to_message(None).unwrap_err();
        assert!(error.to_string().contains("produced no choices"));

        let empty = ChatChoiceMessage {
            content: Some(String::new()),
            reasoning_content: None,
            reasoning: None,
            tool_calls: None,
        };
        let error = chat_response_to_message(Some(empty)).unwrap_err();
        assert!(error.to_string().contains("produced no message content"));
    }

    #[test]
    fn non_streaming_event_emitter_replays_text_and_thinking() {
        let message = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Thinking {
                    text: "thought".to_string(),
                    signature: None,
                },
                ContentBlock::Text {
                    text: "answer".to_string(),
                },
            ],
            usage: None,
        };
        let mut events = Vec::new();
        emit_message_stream_events(&message, &mut |event| events.push(event));

        assert!(matches!(
            &events[0],
            StreamEvent::ThinkingDelta(text) if text == "thought"
        ));
        assert!(matches!(
            &events[1],
            StreamEvent::TextDelta(text) if text == "answer"
        ));
    }

    #[test]
    fn extracts_interleaved_codex_function_calls() {
        let sse = concat!(
            "data: {\"type\":\"response.output_item.added\",\"item_id\":\"fc_1\",\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"id\":\"fc_1\",\"name\":\"read\",\"arguments\":\"\"}}\n\n",
            "data: {\"type\":\"response.output_item.added\",\"item_id\":\"fc_2\",\"item\":{\"type\":\"function_call\",\"call_id\":\"call_2\",\"id\":\"fc_2\",\"name\":\"ls\",\"arguments\":\"\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_1\",\"delta\":\"{\\\"path\\\":\"}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_2\",\"delta\":\"{\\\"path\\\":\"}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_1\",\"delta\":\"\\\"Cargo.toml\\\"}\"}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_2\",\"delta\":\"\\\"src\\\"}\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
            "data: [DONE]\n\n",
        );

        let message = extract_sse_responses_message(sse).unwrap();
        let calls = message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolUse { id, name, input } => Some((id, name, input)),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "call_1|fc_1");
        assert_eq!(calls[0].1, "read");
        assert_eq!(calls[0].2["path"], "Cargo.toml");
        assert_eq!(calls[1].0, "call_2|fc_2");
        assert_eq!(calls[1].1, "ls");
        assert_eq!(calls[1].2["path"], "src");
    }

    #[test]
    fn accepts_codex_argument_delta_before_call_item() {
        let sse = concat!(
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_1\",\"delta\":\"{\\\"path\\\":\"}\n\n",
            "data: {\"type\":\"response.output_item.added\",\"item_id\":\"fc_1\",\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"id\":\"fc_1\",\"name\":\"read\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_1\",\"delta\":\"\\\"Cargo.toml\\\"}\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
            "data: [DONE]\n\n",
        );

        let message = extract_sse_responses_message(sse).unwrap();
        let Some(ContentBlock::ToolUse { id, name, input }) = message
            .content
            .iter()
            .find(|block| matches!(block, ContentBlock::ToolUse { .. }))
        else {
            panic!("missing tool call");
        };

        assert_eq!(id, "call_1|fc_1");
        assert_eq!(name, "read");
        assert_eq!(input["path"], "Cargo.toml");
    }

    #[test]
    fn rejects_ambiguous_parallel_codex_argument_delta() {
        let sse = concat!(
            "data: {\"type\":\"response.output_item.added\",\"item_id\":\"fc_1\",\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"id\":\"fc_1\",\"name\":\"read\",\"arguments\":\"\"}}\n\n",
            "data: {\"type\":\"response.output_item.added\",\"item_id\":\"fc_2\",\"item\":{\"type\":\"function_call\",\"call_id\":\"call_2\",\"id\":\"fc_2\",\"name\":\"ls\",\"arguments\":\"\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"delta\":\"{}\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
            "data: [DONE]\n\n",
        );

        let error = extract_sse_responses_message(sse).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("did not identify which parallel call")
        );
    }

    #[test]
    fn rejects_malformed_responses_function_call_missing_name() {
        let sse = concat!(
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"id\":\"fc_1\",\"arguments\":\"{}\"}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
            "data: [DONE]\n\n",
        );
        let error = extract_sse_responses_message(sse).unwrap_err();
        assert!(error.to_string().contains("function_call missing"));
    }

    #[test]
    fn rejects_malformed_responses_function_call_missing_arguments() {
        let sse = concat!(
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"id\":\"fc_1\",\"name\":\"read\"}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
            "data: [DONE]\n\n",
        );
        let error = extract_sse_responses_message(sse).unwrap_err();
        assert!(error.to_string().contains("function_call missing"));
    }

    #[test]
    fn non_streaming_chat_response_preserves_reasoning() {
        let message = ChatChoiceMessage {
            content: Some("final".to_string()),
            reasoning_content: Some("thought".to_string()),
            reasoning: None,
            tool_calls: None,
        };
        let message = chat_response_to_message(Some(message)).unwrap();
        assert!(matches!(
            &message.content[0],
            ContentBlock::Thinking { text, .. } if text == "thought"
        ));
        assert!(matches!(
            &message.content[1],
            ContentBlock::Text { text } if text == "final"
        ));
    }

    #[test]
    fn rejects_malformed_openai_compatible_event_shapes() {
        for line in [
            "data: []",
            r#"data: {"choices":[null]}"#,
            r#"data: {"choices":[{"delta":null}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[null]}}]}"#,
        ] {
            let mut parser = ChatSseParser::default();
            assert!(parser.process_line(line, None).is_err(), "accepted {line}");
        }
    }

    #[test]
    fn rejects_malformed_chat_tool_call_json() {
        let message = ChatChoiceMessage {
            content: None,
            reasoning_content: None,
            reasoning: None,
            tool_calls: Some(vec![ChatResponseToolCall {
                id: "call_1".to_string(),
                function: ChatResponseToolFunction {
                    name: "read".to_string(),
                    arguments: "{not-json".to_string(),
                },
            }]),
        };
        let error = chat_response_to_message(Some(message)).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("failed to parse tool-call arguments")
        );
    }

    #[test]
    fn rejects_malformed_stream_tool_call_json() {
        let mut parser = ChatSseParser::default();
        parser
            .process_line(
                r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read","arguments":"{not-json"}}]}}]}"#,
                None,
            )
            .unwrap();
        parser.process_line("data:[DONE]", None).unwrap();
        let error = parser.finish().unwrap_err();
        assert!(
            error
                .to_string()
                .contains("failed to parse streamed tool-call arguments")
        );
    }

    #[test]
    fn extracts_chat_stream_tool_call() {
        let mut parser = ChatSseParser::default();
        parser
            .process_line(
                r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read","arguments":"{\"pa"}}]}}]}"#,
                None,
            )
            .unwrap();
        parser
            .process_line(
                r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"Cargo.toml\"}"}}]}}]}"#,
                None,
            )
            .unwrap();
        parser.process_line("data:[DONE]", None).unwrap();

        let response = parser.finish().unwrap();

        let Some(ContentBlock::ToolUse { id, name, input }) = response.message.content.first()
        else {
            panic!("missing tool call");
        };
        assert_eq!(id, "call_1");
        assert_eq!(name, "read");
        assert_eq!(input["path"], "Cargo.toml");
    }

    #[test]
    fn rejects_out_of_range_chat_stream_tool_call_index() {
        let mut parser = ChatSseParser::default();
        let error = parser
            .process_line(
                r#"data: {"choices":[{"delta":{"tool_calls":[{"index":128,"function":{"name":"read","arguments":"{}"}}]}}]}"#,
                None,
            )
            .unwrap_err();

        assert!(error.to_string().contains("exceeds maximum"));
        assert!(parser.tool_calls.is_empty());
    }

    #[test]
    fn rejects_sparse_chat_stream_tool_call_index() {
        let mut parser = ChatSseParser::default();
        let error = parser
            .process_line(
                r#"data: {"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"name":"read","arguments":"{}"}}]}}]}"#,
                None,
            )
            .unwrap_err();

        assert!(error.to_string().contains("is sparse"));
        assert!(parser.tool_calls.is_empty());
    }

    #[test]
    fn serializes_chat_stream_options() {
        let request = ChatRequest {
            model: "m",
            messages: Vec::new(),
            tools: Vec::new(),
            tool_choice: None,
            reasoning_effort: None,
            stream: true,
            stream_options: Some(ChatStreamOptions {
                include_usage: true,
            }),
        };

        let json = serde_json::to_value(request).unwrap();

        assert_eq!(json["stream"], true);
        assert_eq!(json["stream_options"]["include_usage"], true);
    }

    #[test]
    fn detects_stream_usage_unsupported_errors() {
        assert!(is_stream_usage_unsupported_error(
            "400: unknown field stream_options"
        ));
        assert!(is_stream_usage_unsupported_error(
            "include_usage is unsupported by this provider"
        ));
        assert!(is_stream_usage_unsupported_error(
            "invalid include_usage option"
        ));
        assert!(!is_stream_usage_unsupported_error("model is unavailable"));
    }

    #[test]
    fn detects_reasoning_effort_unsupported_errors() {
        assert!(is_reasoning_effort_unsupported_error(
            "unknown field reasoning_effort"
        ));
        assert!(is_reasoning_effort_unsupported_error(
            "reasoning effort is unsupported by this backend"
        ));
        assert!(is_reasoning_effort_unsupported_error(
            "extra inputs are not permitted: reasoning_effort"
        ));
        assert!(!is_reasoning_effort_unsupported_error(
            "reasoning_effort high exhausted quota"
        ));
    }

    #[test]
    fn extracts_thinking_from_sse() {
        let sse = concat!(
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"Checked context.\"}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
            "data: [DONE]\n\n",
        );
        let message = extract_sse_responses_message(sse).unwrap();
        assert_eq!(message.thinking_text(), "Checked context.");
        assert_eq!(message.display_text(), "hello");
    }

    #[test]
    fn codex_reasoning_summary_done_replaces_delta_without_duplicate() {
        let sse = concat!(
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"**Switching to fixed directory usage** \"}\n\n",
            "data: {\"type\":\"response.reasoning_summary_text.done\",\"text\":\"**Switching to fixed directory usage** <!-- -->\"}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"done\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
            "data: [DONE]\n\n",
        );
        let message = extract_sse_responses_message(sse).unwrap();

        assert_eq!(
            message.thinking_text().trim(),
            "**Switching to fixed directory usage**"
        );
        assert_eq!(
            message
                .thinking_text()
                .matches("Switching to fixed directory usage")
                .count(),
            1
        );
    }

    #[test]
    fn codex_reasoning_summary_cumulative_deltas_do_not_duplicate() {
        let sse = concat!(
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"**Planning response verification approach**\\n\\n<!-- -->\"}\n\n",
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"**Planning response verification approach**\\n\\n<!-- -->**Checking Ferrum version post-restart**\\n\\n<!-- -->\"}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"done\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
            "data: [DONE]\n\n",
        );
        let mut events = Vec::new();
        let mut parser = ResponsesSseParser::default();
        for line in sse.lines() {
            parser
                .process_line(line, Some(&mut |event| events.push(event)))
                .unwrap();
        }
        let message = parser.finish().unwrap().message;
        let thinking = message.thinking_text();
        let rendered = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ThinkingDelta(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();

        assert_eq!(
            thinking
                .matches("Planning response verification approach")
                .count(),
            1
        );
        assert_eq!(
            thinking
                .matches("Checking Ferrum version post-restart")
                .count(),
            1
        );
        assert!(!thinking.contains("<!--"));
        assert_eq!(
            rendered
                .matches("Planning response verification approach")
                .count(),
            1
        );
        assert_eq!(
            rendered
                .matches("Checking Ferrum version post-restart")
                .count(),
            1
        );
        assert!(!rendered.contains("<!--"));
    }

    #[test]
    fn codex_reasoning_summary_parts_keep_streamed_newlines() {
        let sse = concat!(
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"rs_1\",\"output_index\":0,\"summary_index\":0,\"delta\":\"**Planning verification**\"}\n\n",
            "data: {\"type\":\"response.reasoning_summary_text.done\",\"item_id\":\"rs_1\",\"output_index\":0,\"summary_index\":0,\"text\":\"**Planning verification**\"}\n\n",
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"rs_1\",\"output_index\":0,\"summary_index\":1,\"delta\":\"**Checking results**\"}\n\n",
            "data: {\"type\":\"response.reasoning_summary_text.done\",\"item_id\":\"rs_1\",\"output_index\":0,\"summary_index\":1,\"text\":\"**Checking results**\"}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"reasoning\",\"id\":\"rs_1\",\"summary\":[{\"text\":\"**Planning verification**\"},{\"text\":\"**Checking results**\"}]}}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"done\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
            "data: [DONE]\n\n",
        );
        let mut events = Vec::new();
        let mut parser = ResponsesSseParser::default();
        for line in sse.lines() {
            parser
                .process_line(line, Some(&mut |event| events.push(event)))
                .unwrap();
        }
        let message = parser.finish().unwrap().message;
        let rendered = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ThinkingDelta(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();

        assert_eq!(
            rendered,
            "**Planning verification**\n\n**Checking results**"
        );
        assert_eq!(message.thinking_text(), rendered);
    }

    #[test]
    fn codex_reasoning_summary_split_comments_do_not_render() {
        let sse = concat!(
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"**Planning response verification approach**\\n\\n<!-\"}\n\n",
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"- -->**Planning response verification approach** **Planning targeted tool tests** <!\"}\n\n",
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"-- -->**Planning targeted tool tests**\"}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"done\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
            "data: [DONE]\n\n",
        );
        let mut events = Vec::new();
        let mut parser = ResponsesSseParser::default();
        for line in sse.lines() {
            parser
                .process_line(line, Some(&mut |event| events.push(event)))
                .unwrap();
        }
        let message = parser.finish().unwrap().message;
        let thinking = message.thinking_text();
        let rendered = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ThinkingDelta(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();

        assert_eq!(
            thinking
                .matches("Planning response verification approach")
                .count(),
            1
        );
        assert_eq!(thinking.matches("Planning targeted tool tests").count(), 1);
        assert!(!thinking.contains("<!--"));
        assert!(!thinking.contains("-->"));
        assert_eq!(
            rendered
                .matches("Planning response verification approach")
                .count(),
            1
        );
        assert_eq!(rendered.matches("Planning targeted tool tests").count(), 1);
        assert!(!rendered.contains("<!--"));
        assert!(!rendered.contains("-->"));
    }

    #[test]
    fn codex_reasoning_summary_done_does_not_duplicate_trim_equivalent_delta() {
        let sse = concat!(
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"**Planning response verification approach**\\n\\n<!-- -->\"}\n\n",
            "data: {\"type\":\"response.reasoning_summary_text.done\",\"text\":\"**Planning response verification approach**\"}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"done\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
            "data: [DONE]\n\n",
        );
        let mut events = Vec::new();
        let mut parser = ResponsesSseParser::default();
        for line in sse.lines() {
            parser
                .process_line(line, Some(&mut |event| events.push(event)))
                .unwrap();
        }
        let message = parser.finish().unwrap().message;

        assert_eq!(
            message.thinking_text().trim(),
            "**Planning response verification approach**"
        );
        assert_eq!(
            events
                .iter()
                .filter_map(|event| match event {
                    StreamEvent::ThinkingDelta(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>()
                .matches("Planning response verification approach")
                .count(),
            1
        );
    }

    #[test]
    fn extracts_thinking_signature_from_reasoning_item() {
        let sse = concat!(
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"reasoning\",\"id\":\"rs_1\",\"summary\":[{\"text\":\"Plan first.\"}]}}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"done\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
            "data: [DONE]\n\n",
        );
        let message = extract_sse_responses_message(sse).unwrap();
        let Some(ContentBlock::Thinking { text, signature }) = message.content.first() else {
            panic!("missing thinking block");
        };
        assert_eq!(text, "Plan first.");
        assert!(signature.as_deref().unwrap_or_default().contains("rs_1"));
    }

    #[test]
    fn extracts_output_from_completed_response_event() {
        let sse = concat!(
            "data: {\"type\":\"response.completed\",\"response\":{\"output_text\":\"hello\"}}\n\n",
            "data: [DONE]\n\n",
        );
        let message = extract_sse_responses_message(sse).unwrap();
        assert_eq!(message.display_text(), "hello");
    }

    #[test]
    fn extracts_output_from_output_text_done_event() {
        let sse = concat!(
            "data: {\"type\":\"response.output_text.done\",\"text\":\"hello\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
            "data: [DONE]\n\n",
        );
        let message = extract_sse_responses_message(sse).unwrap();
        assert_eq!(message.display_text(), "hello");
    }

    #[test]
    fn extracts_codex_usage_from_completed_response() {
        let sse = concat!(
            "data: {\"type\":\"response.completed\",\"response\":{\"output_text\":\"hello\",\"usage\":{\"input_tokens\":10,\"output_tokens\":3,\"total_tokens\":13,\"input_tokens_details\":{\"cached_tokens\":4}}}}\n\n",
            "data: [DONE]\n\n",
        );
        let response = {
            let mut parser = ResponsesSseParser::default();
            for line in sse.lines() {
                parser.process_line(line, None).unwrap();
            }
            parser.finish().unwrap()
        };

        assert_eq!(response.message.display_text(), "hello");
        let usage = response.usage.unwrap();
        assert_eq!(usage.source, "provider");
        assert_eq!(usage.input_tokens, Some(10));
        assert_eq!(usage.output_tokens, Some(3));
        assert_eq!(usage.total_tokens, Some(13));
        assert_eq!(usage.cache_read_tokens, 4);
    }

    #[test]
    fn completed_response_deduplicates_tool_calls() {
        let sse = concat!(
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"id\":\"fc_1\",\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"Cargo.toml\\\"}\"}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"output\":[{\"type\":\"function_call\",\"call_id\":\"call_1\",\"id\":\"fc_1\",\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"Cargo.toml\\\"}\"}]}}\n\n",
            "data: [DONE]\n\n",
        );

        let message = extract_sse_responses_message(sse).unwrap();
        let tool_calls = message
            .content
            .iter()
            .filter(|block| matches!(block, ContentBlock::ToolUse { .. }))
            .count();

        assert_eq!(tool_calls, 1);
    }

    #[test]
    fn skips_codex_tool_call_without_output() {
        let orphan_call = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call_1|fc_1".to_string(),
                name: "wait".to_string(),
                input: serde_json::json!({"command": "date"}),
            }],
            usage: None,
        };
        let inputs = codex_inputs(&[orphan_call]);

        assert!(inputs.is_empty());
    }

    #[test]
    fn replays_codex_tool_call_with_output() {
        let tool_call = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call_1|fc_1".to_string(),
                name: "wait".to_string(),
                input: serde_json::json!({"command": "date"}),
            }],
            usage: None,
        };
        let tool_result = Message {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_1|fc_1".to_string(),
                content: "aborted".to_string(),
                is_error: true,
            }],
            usage: None,
        };
        let inputs = codex_inputs(&[tool_call, tool_result]);
        let json = serde_json::to_value(&inputs).unwrap();

        assert_eq!(json[0]["type"], "function_call");
        assert_eq!(json[0]["call_id"], "call_1");
        assert_eq!(json[1]["type"], "function_call_output");
        assert_eq!(json[1]["call_id"], "call_1");
        assert_eq!(json[1]["output"], "aborted");
    }

    #[test]
    fn replays_codex_visible_text_from_mixed_message() {
        let tool_call = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "Visible answer.".to_string(),
                },
                ContentBlock::ToolUse {
                    id: "call_1|fc_1".to_string(),
                    name: "wait".to_string(),
                    input: serde_json::json!({"command": "date"}),
                },
            ],
            usage: None,
        };
        let tool_result = Message {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_1|fc_1".to_string(),
                content: "done".to_string(),
                is_error: false,
            }],
            usage: None,
        };
        let inputs = codex_inputs(&[tool_call, tool_result]);
        let json = serde_json::to_value(&inputs).unwrap();

        assert_eq!(json[0]["type"], "function_call");
        assert_eq!(json[1]["role"], "assistant");
        assert_eq!(json[1]["content"], "Visible answer.");
        assert_eq!(json[2]["type"], "function_call_output");
    }

    #[test]
    fn replays_thinking_signature_for_codex() {
        let message = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Thinking {
                text: "Plan first.".to_string(),
                signature: Some(r#"{"type":"reasoning","id":"rs_1"}"#.to_string()),
            }],
            usage: None,
        };
        let inputs = codex_inputs(&[message]);
        let json = serde_json::to_value(&inputs).unwrap();
        assert_eq!(json[0]["type"], "reasoning");
        assert_eq!(json[0]["id"], "rs_1");
    }
}
