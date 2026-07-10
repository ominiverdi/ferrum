use super::{Provider, ProviderResponse, StreamEvent, TokenUsage};
use crate::{
    agent::{
        messages::{ContentBlock, Message, Role, sanitize_thinking_text},
        tools::ToolDefinition,
    },
    auth::openai_codex,
    cancel::{self, WaitError},
    config::ThinkingLevel,
};
use anyhow::{Context, Result};
use futures_util::StreamExt;
use reqwest::{Client, Response, StatusCode};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    env,
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const PROVIDER_INITIAL_RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);
const PROVIDER_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

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

impl OpenAiCompatProvider {
    pub fn new(
        api_key_env: Option<String>,
        base_url: String,
        streaming: bool,
        stream_usage: bool,
    ) -> Self {
        Self {
            api_key_env,
            base_url: base_url.trim_end_matches('/').to_string(),
            streaming,
            stream_usage,
            client: Client::new(),
        }
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
            let response = http_request
                .json(&request)
                .send()
                .await
                .context("OpenAI-compatible request failed")?;

            let status = response.status();
            let text = response
                .text()
                .await
                .context("failed to read provider response")?;
            let text = if !status.is_success() {
                if reasoning_effort.is_some() && is_reasoning_effort_unsupported_error(&text) {
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
                    let retry_response = retry_http_request
                        .json(&retry_request)
                        .send()
                        .await
                        .context("OpenAI-compatible reasoning retry failed")?;
                    let retry_status = retry_response.status();
                    let retry_text = retry_response
                        .text()
                        .await
                        .context("failed to read provider retry response")?;
                    if !retry_status.is_success() {
                        anyhow::bail!(
                            "OpenAI-compatible provider returned {retry_status}: {retry_text}"
                        );
                    }
                    retry_text
                } else {
                    anyhow::bail!("OpenAI-compatible provider returned {status}: {text}");
                }
            } else {
                text
            };

            let body: ChatResponse = serde_json::from_str(&text)
                .with_context(|| format!("failed to parse provider response: {text}"))?;
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
                let text = response
                    .text()
                    .await
                    .context("failed to read provider error response")?;
                if self.stream_usage && is_stream_usage_unsupported_error(&text) {
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
                        let retry_text = response
                            .text()
                            .await
                            .context("failed to read provider retry error response")?;
                        anyhow::bail!(
                            "OpenAI-compatible provider returned {retry_status}: {retry_text}"
                        );
                    }
                } else if thinking.as_openai().is_some()
                    && is_reasoning_effort_unsupported_error(&text)
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
                        let retry_text = response
                            .text()
                            .await
                            .context("failed to read provider reasoning retry error response")?;
                        anyhow::bail!(
                            "OpenAI-compatible provider returned {retry_status}: {retry_text}"
                        );
                    }
                } else {
                    anyhow::bail!("OpenAI-compatible provider returned {status}: {text}");
                }
            }

            let mut parser = ChatSseParser::default();
            let mut buffer = String::new();
            let mut stream = response.bytes_stream();
            loop {
                let next = cancel::race_timeout(
                    stream.next(),
                    cancelled.as_ref(),
                    PROVIDER_STREAM_IDLE_TIMEOUT,
                )
                .await;
                let chunk = match next {
                    Ok(Some(chunk)) => chunk,
                    Ok(None) => break,
                    Err(WaitError::Cancelled) => anyhow::bail!("aborted"),
                    Err(WaitError::TimedOut) => anyhow::bail!(
                        "OpenAI-compatible stream idle for {}s",
                        PROVIDER_STREAM_IDLE_TIMEOUT.as_secs()
                    ),
                };
                let chunk = chunk.context("failed to read OpenAI-compatible stream")?;
                buffer.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(index) = buffer.find('\n') {
                    let line = buffer[..index].trim_end_matches('\r').to_string();
                    buffer.drain(..=index);
                    parser.process_line(&line, Some(on_event));
                }
            }
            if !buffer.is_empty() {
                let line = buffer.trim_end_matches('\r').to_string();
                parser.process_line(&line, Some(on_event));
            }
            parser.finish()
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
    pub fn new(base_url: String, auth_path: PathBuf) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            auth_path,
            client: Client::new(),
        }
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
            let api_key = openai_codex::get_api_key_from_path(self.auth_path.clone())
                .await?
                .context("OpenAI Codex auth not found; run `ferrum login openai`")?;
            let account_id = openai_codex::extract_account_id(&api_key)?;
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

            let response = send_codex_request_with_retries(
                &self.client,
                &self.responses_url(),
                &api_key,
                &account_id,
                &request,
                None,
            )
            .await?;
            let text = cancel::race_timeout(response.text(), None, PROVIDER_STREAM_IDLE_TIMEOUT)
                .await
                .map_err(|error| match error {
                    WaitError::Cancelled => anyhow::anyhow!("aborted"),
                    WaitError::TimedOut => anyhow::anyhow!(
                        "OpenAI Codex response idle for {}s",
                        PROVIDER_STREAM_IDLE_TIMEOUT.as_secs()
                    ),
                })??;
            Ok(ProviderResponse::message(
                extract_sse_responses_message(&text).unwrap_or_else(|| {
                    let content = serde_json::from_str::<serde_json::Value>(&text)
                        .ok()
                        .and_then(|body| extract_responses_text(&body))
                        .unwrap_or_default();
                    Message::text(Role::Assistant, content)
                }),
            ))
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
            let api_key = openai_codex::get_api_key_from_path(self.auth_path.clone())
                .await?
                .context("OpenAI Codex auth not found; run `ferrum login openai`")?;
            let account_id = openai_codex::extract_account_id(&api_key)?;
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

            let response = send_codex_request_with_retries(
                &self.client,
                &self.responses_url(),
                &api_key,
                &account_id,
                &request,
                cancelled.as_ref(),
            )
            .await?;

            let mut parser = ResponsesSseParser::default();
            let mut buffer = String::new();
            let mut stream = response.bytes_stream();
            loop {
                let next = cancel::race_timeout(
                    stream.next(),
                    cancelled.as_ref(),
                    PROVIDER_STREAM_IDLE_TIMEOUT,
                )
                .await;
                let chunk = match next {
                    Ok(Some(chunk)) => chunk,
                    Ok(None) => break,
                    Err(WaitError::Cancelled) => anyhow::bail!("aborted"),
                    Err(WaitError::TimedOut) => anyhow::bail!(
                        "OpenAI Codex stream idle for {}s",
                        PROVIDER_STREAM_IDLE_TIMEOUT.as_secs()
                    ),
                };
                let chunk = chunk.context("failed to read OpenAI Codex stream")?;
                buffer.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(index) = buffer.find('\n') {
                    let line = buffer[..index].trim_end_matches('\r').to_string();
                    buffer.drain(..=index);
                    parser.process_line(&line, Some(on_event));
                }
            }
            if !buffer.is_empty() {
                let line = buffer.trim_end_matches('\r').to_string();
                parser.process_line(&line, Some(on_event));
            }
            parser.finish()
        })
    }
}

const CODEX_MAX_RETRIES: usize = 3;
const CODEX_RETRY_BASE_DELAY: Duration = Duration::from_secs(2);

async fn send_codex_request_with_retries(
    client: &Client,
    url: &str,
    api_key: &str,
    account_id: &str,
    request: &CodexResponsesRequest<'_>,
    cancelled: Option<&Arc<AtomicBool>>,
) -> Result<Response> {
    let mut attempt = 0usize;
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
            Err(WaitError::TimedOut) if attempt < CODEX_MAX_RETRIES => {
                attempt += 1;
                sleep_before_codex_retry(
                    attempt,
                    &format!(
                        "OpenAI Codex did not respond within {}s",
                        PROVIDER_INITIAL_RESPONSE_TIMEOUT.as_secs()
                    ),
                    cancelled,
                )
                .await?;
                continue;
            }
            Err(WaitError::TimedOut) => anyhow::bail!(
                "OpenAI Codex did not respond within {}s",
                PROVIDER_INITIAL_RESPONSE_TIMEOUT.as_secs()
            ),
        };
        match response {
            Ok(response) if response.status().is_success() => return Ok(response),
            Ok(response) => {
                let status = response.status();
                let retryable = is_retryable_codex_status(status);
                let text = response
                    .text()
                    .await
                    .context("failed to read OpenAI Codex error response")?;
                if retryable && attempt < CODEX_MAX_RETRIES {
                    attempt += 1;
                    sleep_before_codex_retry(
                        attempt,
                        &format!("OpenAI Codex returned {status}: {text}"),
                        cancelled,
                    )
                    .await?;
                    continue;
                }
                anyhow::bail!("OpenAI Codex returned {status}: {text}");
            }
            Err(error) => {
                let retryable = is_retryable_codex_send_error(&error);
                if retryable && attempt < CODEX_MAX_RETRIES {
                    attempt += 1;
                    sleep_before_codex_retry(
                        attempt,
                        &format!("OpenAI Codex request failed: {error}"),
                        cancelled,
                    )
                    .await?;
                    continue;
                }
                return Err(error).context("OpenAI Codex request failed");
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
    error.is_connect() || error.is_timeout() || error.is_body()
}

async fn sleep_before_codex_retry(
    attempt: usize,
    reason: &str,
    cancelled: Option<&Arc<AtomicBool>>,
) -> Result<()> {
    let delay = CODEX_RETRY_BASE_DELAY * (1 << (attempt - 1));
    eprintln!(
        "[provider] {reason}; retrying in {}s ({attempt}/{CODEX_MAX_RETRIES})",
        delay.as_secs()
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
    reasoning_content: Option<&'static str>,
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
        Self {
            role,
            content,
            tool_call_id,
            tool_calls: has_tool_calls.then_some(tool_calls),
            reasoning_content: (role == "assistant" && has_tool_calls).then_some(""),
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
        content.push(ContentBlock::Thinking {
            text,
            signature: None,
        });
    }
    if let Some(text) = message.content.filter(|text| !text.is_empty()) {
        content.push(ContentBlock::Text { text });
    }
    for call in message.tool_calls.unwrap_or_default() {
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

#[derive(Default)]
struct ChatSseParser {
    output: String,
    thinking: String,
    tool_calls: Vec<ChatStreamToolCall>,
    usage: Option<TokenUsage>,
}

#[derive(Default)]
struct ChatStreamToolCall {
    id: String,
    name: String,
    arguments: String,
}

impl ChatSseParser {
    fn process_line(
        &mut self,
        line: &str,
        mut on_event: Option<&mut (dyn FnMut(StreamEvent) + Send)>,
    ) {
        let Some(data) = line.strip_prefix("data: ") else {
            return;
        };
        if data == "[DONE]" {
            return;
        }
        let Ok(event) = serde_json::from_str::<serde_json::Value>(data) else {
            return;
        };
        if let Some(usage) = event.get("usage").filter(|usage| !usage.is_null())
            && let Ok(parsed) = serde_json::from_value::<OpenAiUsage>(usage.clone())
        {
            self.usage = Some(parsed.to_token_usage());
        }
        for choice in event
            .get("choices")
            .and_then(|value| value.as_array())
            .into_iter()
            .flatten()
        {
            let Some(delta) = choice.get("delta") else {
                continue;
            };
            if let Some(text) = delta.get("content").and_then(|value| value.as_str()) {
                self.output.push_str(text);
                if let Some(on_event) = on_event.as_deref_mut() {
                    on_event(StreamEvent::TextDelta(text.to_string()));
                }
            }
            if let Some(text) = delta
                .get("reasoning_content")
                .or_else(|| delta.get("reasoning"))
                .and_then(|value| value.as_str())
            {
                let text = sanitize_thinking_text(text);
                if text.is_empty() {
                    continue;
                }
                self.thinking.push_str(&text);
                if let Some(on_event) = on_event.as_deref_mut() {
                    on_event(StreamEvent::ThinkingDelta(text));
                }
            }
            for tool_call in delta
                .get("tool_calls")
                .and_then(|value| value.as_array())
                .into_iter()
                .flatten()
            {
                let index = tool_call
                    .get("index")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(self.tool_calls.len() as u64) as usize;
                while self.tool_calls.len() <= index {
                    self.tool_calls.push(ChatStreamToolCall::default());
                }
                let current = &mut self.tool_calls[index];
                if let Some(id) = tool_call.get("id").and_then(|value| value.as_str()) {
                    current.id = id.to_string();
                }
                if let Some(function) = tool_call.get("function") {
                    if let Some(name) = function.get("name").and_then(|value| value.as_str()) {
                        current.name.push_str(name);
                    }
                    if let Some(arguments) =
                        function.get("arguments").and_then(|value| value.as_str())
                    {
                        current.arguments.push_str(arguments);
                    }
                }
            }
        }
    }

    fn finish(self) -> Result<ProviderResponse> {
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
                continue;
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
    thinking_signature: Option<String>,
    usage: Option<TokenUsage>,
    tool_calls: Vec<(String, String, String, String)>,
    current_calls: BTreeMap<String, (String, String, String, String)>,
    pending_call_args: BTreeMap<String, String>,
    error: Option<String>,
}

impl ResponsesSseParser {
    fn process_line(
        &mut self,
        line: &str,
        mut on_event: Option<&mut (dyn FnMut(StreamEvent) + Send)>,
    ) {
        let Some(data) = line.strip_prefix("data: ") else {
            return;
        };
        if data == "[DONE]" {
            return;
        }
        let Ok(event) = serde_json::from_str::<serde_json::Value>(data) else {
            return;
        };
        if let Some(usage) = event
            .get("response")
            .and_then(|response| response.get("usage"))
            .or_else(|| event.get("usage"))
            .filter(|usage| !usage.is_null())
        {
            self.absorb_usage(usage);
        }
        let event_type = event.get("type").and_then(|value| value.as_str());
        if let Some(event_type) = event_type {
            emit_codex_usage_metrics_if_enabled(event_type, &event);
            if event_type.contains("reasoning") && event_type.contains("summary") {
                if let Some(delta) = event.get("delta").and_then(|value| value.as_str()) {
                    self.append_thinking_delta(delta, &mut on_event);
                }
                if let Some(text) = event.get("text").and_then(|value| value.as_str()) {
                    self.absorb_completed_thinking(text, &mut on_event);
                }
            } else if event_type == "response.reasoning_text.delta"
                && let Some(delta) = event.get("delta").and_then(|value| value.as_str())
            {
                self.append_thinking_delta(delta, &mut on_event);
            }
        }
        match event_type {
            Some("response.output_text.delta") => {
                if let Some(delta) = event.get("delta").and_then(|value| value.as_str()) {
                    self.output.push_str(delta);
                    if let Some(on_event) = on_event.as_mut() {
                        on_event(StreamEvent::TextDelta(delta.to_string()));
                    }
                }
            }
            Some("response.output_text.done") => {
                if let Some(text) = event.get("text").and_then(|value| value.as_str()) {
                    self.append_completed_output(text, None);
                }
            }
            Some("response.completed") => {
                if let Some(response) = event.get("response") {
                    self.absorb_completed_response(response);
                }
            }
            Some("response.output_item.added") => {
                if let Some(item) = event.get("item")
                    && item.get("type").and_then(|value| value.as_str()) == Some("function_call")
                {
                    match parse_response_function_call_added_item(item) {
                        Ok(mut call) => {
                            let key = response_event_call_key(&event)
                                .unwrap_or_else(|| response_call_key(item, &call));
                            if let Some(args) = self.pending_call_args.remove(&key) {
                                call.3.push_str(&args);
                            }
                            self.current_calls.insert(key, call);
                        }
                        Err(error) => self.error = Some(error.to_string()),
                    }
                }
            }
            Some("response.function_call_arguments.delta") => {
                if let Some(delta) = event.get("delta").and_then(|value| value.as_str()) {
                    self.update_current_call_args(&event, |args| args.push_str(delta));
                }
            }
            Some("response.function_call_arguments.done") => {
                if let Some(done) = event.get("arguments").and_then(|value| value.as_str()) {
                    self.update_current_call_args(&event, |args| *args = done.to_string());
                }
            }
            Some("response.output_item.done") => {
                if let Some(item) = event.get("item") {
                    if item.get("type").and_then(|value| value.as_str()) == Some("reasoning") {
                        let final_thinking = thinking_text_from_item(item);
                        if !final_thinking.trim().is_empty() {
                            self.thinking = final_thinking;
                        }
                        self.thinking_signature = Some(item.to_string());
                    }
                    if item.get("type").and_then(|value| value.as_str()) == Some("function_call") {
                        match parse_response_function_call_item(item) {
                            Ok(call) => {
                                let key = response_event_call_key(&event)
                                    .unwrap_or_else(|| response_call_key(item, &call));
                                self.current_calls.remove(&key);
                                self.tool_calls.push(call);
                            }
                            Err(error) => self.error = Some(error.to_string()),
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn append_thinking_delta(
        &mut self,
        text: &str,
        on_event: &mut Option<&mut (dyn FnMut(StreamEvent) + Send)>,
    ) {
        let text = self.sanitize_thinking_delta(text);
        let delta = self.merge_thinking_text(&text);
        if delta.is_empty() {
            return;
        }
        if let Some(on_event) = on_event.as_deref_mut() {
            on_event(StreamEvent::ThinkingDelta(delta));
        }
    }

    fn absorb_completed_thinking(
        &mut self,
        text: &str,
        on_event: &mut Option<&mut (dyn FnMut(StreamEvent) + Send)>,
    ) {
        let text = sanitize_thinking_text(text);
        let delta = self.merge_thinking_text(&text);
        if !delta.is_empty()
            && let Some(on_event) = on_event.as_deref_mut()
        {
            on_event(StreamEvent::ThinkingDelta(delta));
        }
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
    ) {
        if text.is_empty() || self.output.ends_with(text) {
            return;
        }
        let delta = text.strip_prefix(&self.output).unwrap_or(text).to_string();
        self.output.push_str(&delta);
        if !delta.is_empty()
            && let Some(on_event) = on_event
        {
            on_event(StreamEvent::TextDelta(delta));
        }
    }

    fn update_current_call_args(
        &mut self,
        event: &serde_json::Value,
        update: impl FnOnce(&mut String),
    ) {
        let Some(key) = response_event_call_key(event) else {
            if self.current_calls.len() == 1 {
                if let Some((_, _, _, args)) = self.current_calls.values_mut().next() {
                    update(args);
                }
            } else if !self.current_calls.is_empty() {
                self.error = Some(
                    "OpenAI Codex function_call arguments event did not identify which parallel call to update"
                        .to_string(),
                );
            }
            return;
        };
        if let Some((_, _, _, args)) = self.current_calls.get_mut(&key) {
            update(args);
        } else {
            let args = self.pending_call_args.entry(key).or_default();
            update(args);
        }
    }

    fn absorb_usage(&mut self, usage: &serde_json::Value) {
        if let Ok(parsed) = serde_json::from_value::<OpenAiUsage>(usage.clone()) {
            self.usage = Some(parsed.to_token_usage());
        }
    }

    fn absorb_completed_response(&mut self, response: &serde_json::Value) {
        if let Some(usage) = response.get("usage").filter(|usage| !usage.is_null()) {
            self.absorb_usage(usage);
        }
        if let Some(text) = extract_responses_text(response) {
            self.append_completed_output(&text, None);
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
                    self.thinking = final_thinking;
                }
                self.thinking_signature = Some(item.to_string());
            }
            if item.get("type").and_then(|value| value.as_str()) == Some("function_call") {
                match parse_response_function_call_item(item) {
                    Ok(call) => {
                        if !self
                            .tool_calls
                            .iter()
                            .any(|existing| existing.0 == call.0 || existing.1 == call.1)
                        {
                            self.tool_calls.push(call.clone());
                        }
                        self.current_calls.remove(&response_call_key(item, &call));
                    }
                    Err(error) => self.error = Some(error.to_string()),
                }
            }
        }
    }

    fn finish(mut self) -> Result<ProviderResponse> {
        if let Some(error) = self.error.take() {
            anyhow::bail!(error);
        }
        for (_, call) in self.current_calls {
            if !self
                .tool_calls
                .iter()
                .any(|existing| existing.0 == call.0 || existing.1 == call.1)
            {
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

fn extract_sse_responses_message_result(text: &str) -> Result<Message> {
    let mut parser = ResponsesSseParser::default();
    for line in text.lines() {
        parser.process_line(line, None);
    }
    parser.finish().map(|response| response.message)
}

fn extract_sse_responses_message(text: &str) -> Option<Message> {
    extract_sse_responses_message_result(text).ok()
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

fn extract_responses_text(body: &serde_json::Value) -> Option<String> {
    if let Some(text) = body.get("output_text").and_then(|value| value.as_str()) {
        return Some(text.to_string());
    }
    let mut parts = Vec::new();
    for item in body.get("output")?.as_array()? {
        for content in item
            .get("content")
            .and_then(|value| value.as_array())
            .into_iter()
            .flatten()
        {
            if let Some(text) = content.get("text").and_then(|value| value.as_str()) {
                parts.push(text);
            }
        }
    }
    Some(parts.join(""))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        parser.process_line(r#"data: {"choices":[{"delta":{"content":"hel"}}]}"#, None);
        parser.process_line(r#"data: {"choices":[{"delta":{"content":"lo"}}]}"#, None);
        parser.process_line(
            r#"data: {"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":3,"total_tokens":13,"prompt_tokens_details":{"cached_tokens":4}}}"#,
            None,
        );

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
            "data: [DONE]\n\n",
        );

        let error = extract_sse_responses_message_result(sse).unwrap_err();
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
            "data: [DONE]\n\n",
        );
        let error = extract_sse_responses_message_result(sse).unwrap_err();
        assert!(error.to_string().contains("function_call missing"));
    }

    #[test]
    fn rejects_malformed_responses_function_call_missing_arguments() {
        let sse = concat!(
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"id\":\"fc_1\",\"name\":\"read\"}}\n\n",
            "data: [DONE]\n\n",
        );
        let error = extract_sse_responses_message_result(sse).unwrap_err();
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
        parser.process_line(
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read","arguments":"{not-json"}}]}}]}"#,
            None,
        );
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
        parser.process_line(
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read","arguments":"{\"pa"}}]}}]}"#,
            None,
        );
        parser.process_line(
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"Cargo.toml\"}"}}]}}]}"#,
            None,
        );

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
            "data: [DONE]\n\n",
        );
        let mut events = Vec::new();
        let mut parser = ResponsesSseParser::default();
        for line in sse.lines() {
            parser.process_line(line, Some(&mut |event| events.push(event)));
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
    fn codex_reasoning_summary_split_comments_do_not_render() {
        let sse = concat!(
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"**Planning response verification approach**\\n\\n<!-\"}\n\n",
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"- -->**Planning response verification approach** **Planning targeted tool tests** <!\"}\n\n",
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"-- -->**Planning targeted tool tests**\"}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"done\"}\n\n",
            "data: [DONE]\n\n",
        );
        let mut events = Vec::new();
        let mut parser = ResponsesSseParser::default();
        for line in sse.lines() {
            parser.process_line(line, Some(&mut |event| events.push(event)));
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
            "data: [DONE]\n\n",
        );
        let mut events = Vec::new();
        let mut parser = ResponsesSseParser::default();
        for line in sse.lines() {
            parser.process_line(line, Some(&mut |event| events.push(event)));
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
                parser.process_line(line, None);
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
