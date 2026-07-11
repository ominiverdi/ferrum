use crate::{
    agent::{
        AgentSession,
        events::{AgentEvent, AgentEventSink, TurnCancellation, TurnOptions, TurnOutcome},
    },
    config::Config,
    terminal_text,
    text_truncate::truncate_to_max_bytes,
};
use agent_client_protocol_schema::{
    ProtocolVersion,
    v1::{
        AGENT_METHOD_NAMES, AgentCapabilities, CLIENT_METHOD_NAMES, CancelNotification,
        ContentBlock, ContentChunk, Error as AcpError, Implementation, InitializeRequest,
        InitializeResponse, JsonRpcMessage, NewSessionRequest, NewSessionResponse, Notification,
        PromptCapabilities, PromptRequest, PromptResponse, RequestId, Response,
        SessionNotification, SessionUpdate, StopReason, TextContent, ToolCall, ToolCallContent,
        ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields, ToolKind, UsageUpdate,
    },
};
use anyhow::{Context, Result};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::{
    io::{self, AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter},
    sync::{Mutex as AsyncMutex, Semaphore, mpsc},
    task::JoinSet,
};
use url::Url;
use uuid::Uuid;

const MAX_INPUT_LINE_BYTES: usize = 32 * 1024 * 1024;
const MAX_DECODED_REQUEST_BYTES: usize = 30 * 1024 * 1024;
const MAX_DECODED_REQUEST_NODES: usize = 100_000;
const MAX_DECODED_REQUEST_DEPTH: usize = 64;
const MAX_OUTPUT_LINE_BYTES: usize = 256 * 1024;
const MAX_OUTPUT_QUEUE: usize = 128;
const MAX_SESSIONS: usize = 16;
const MAX_CONCURRENT_TURNS: usize = 4;
const MAX_PROMPT_TEXT_BYTES: usize = 2 * 1024 * 1024;
const MAX_RESOURCE_LINKS: usize = 128;
const MAX_RESOURCE_NAME_BYTES: usize = 1024;
const MAX_RESOURCE_URI_BYTES: usize = 16 * 1024;
const MAX_SESSION_ID_BYTES: usize = 128;
const MAX_UPDATE_TEXT_BYTES: usize = 48 * 1024;
const MAX_TOOL_CONTENT_BYTES: usize = 64 * 1024;
const MAX_TOOL_INPUT_BYTES: usize = 64 * 1024;
const MAX_TOOL_ID_BYTES: usize = 16 * 1024;

struct ServerState {
    initialized: bool,
    sessions: HashMap<String, Arc<SessionEntry>>,
    active_request_ids: HashSet<RequestId>,
}

impl ServerState {
    fn new() -> Self {
        Self {
            initialized: false,
            sessions: HashMap::new(),
            active_request_ids: HashSet::new(),
        }
    }
}

struct SessionEntry {
    agent: AsyncMutex<AgentSession>,
    active: Mutex<Option<ActivePrompt>>,
}

#[derive(Clone)]
struct ActivePrompt {
    request_id: RequestId,
    cancellation: TurnCancellation,
}

#[derive(Clone)]
struct Output {
    sender: mpsc::Sender<Value>,
}

impl Output {
    async fn send(&self, value: Value) -> Result<()> {
        self.sender
            .send(value)
            .await
            .map_err(|_| anyhow::anyhow!("ACP output closed"))
    }

    fn try_send(&self, value: Value) -> Result<()> {
        self.sender.try_send(value).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => {
                anyhow::anyhow!("ACP output queue limit reached")
            }
            mpsc::error::TrySendError::Closed(_) => anyhow::anyhow!("ACP output closed"),
        })
    }
}

struct AcpEventSink {
    session_id: String,
    output: Output,
    context_size: u64,
    text_sanitizer: terminal_text::Sanitizer,
    thought_sanitizer: terminal_text::Sanitizer,
}

impl AcpEventSink {
    fn new(session_id: String, output: Output, context_size: usize) -> Self {
        Self {
            session_id,
            output,
            context_size: u64::try_from(context_size).unwrap_or(u64::MAX),
            text_sanitizer: terminal_text::Sanitizer::default(),
            thought_sanitizer: terminal_text::Sanitizer::default(),
        }
    }

    fn emit_update(&self, update: SessionUpdate) -> Result<()> {
        let params = SessionNotification::new(self.session_id.clone(), update);
        let notification = Notification {
            method: CLIENT_METHOD_NAMES.session_update.into(),
            params: Some(params),
        };
        self.output
            .try_send(serde_json::to_value(JsonRpcMessage::wrap(notification))?)
    }

    fn emit_text_chunks(&self, text: &str, thought: bool) -> Result<()> {
        for chunk in utf8_chunks(text, MAX_UPDATE_TEXT_BYTES) {
            let content = ContentChunk::new(ContentBlock::Text(TextContent::new(chunk)));
            let update = if thought {
                SessionUpdate::AgentThoughtChunk(content)
            } else {
                SessionUpdate::AgentMessageChunk(content)
            };
            self.emit_update(update)?;
        }
        Ok(())
    }
}

impl AgentEventSink for AcpEventSink {
    fn emit(&mut self, event: AgentEvent) -> Result<()> {
        match event {
            AgentEvent::TextDelta(delta) => {
                let text = self.text_sanitizer.push(&delta);
                self.emit_text_chunks(&text, false)?;
            }
            AgentEvent::ThinkingDelta(delta) => {
                let text = self.thought_sanitizer.push(&delta);
                self.emit_text_chunks(&text, true)?;
            }
            AgentEvent::UsageUpdated {
                estimated_context_tokens,
                ..
            } => {
                self.emit_update(SessionUpdate::UsageUpdate(UsageUpdate::new(
                    u64::try_from(estimated_context_tokens).unwrap_or(u64::MAX),
                    self.context_size,
                )))?;
            }
            AgentEvent::ToolCallStarted {
                id, name, input, ..
            } => {
                let id = truncate_to_max_bytes(&terminal_text::sanitize(&id), MAX_TOOL_ID_BYTES);
                let title = terminal_text::sanitize_title(&name);
                let mut call = ToolCall::new(id, title)
                    .kind(tool_kind(&name))
                    .status(ToolCallStatus::InProgress);
                if let Some(input) = bounded_sanitized_json(input, MAX_TOOL_INPUT_BYTES) {
                    call = call.raw_input(input);
                }
                self.emit_update(SessionUpdate::ToolCall(call))?;
            }
            AgentEvent::ToolCallCompleted {
                id,
                content,
                is_error,
                aborted,
                ..
            } => {
                let content = truncate_to_max_bytes(
                    &terminal_text::sanitize(&content),
                    MAX_TOOL_CONTENT_BYTES,
                );
                let fields = ToolCallUpdateFields::new()
                    .status(if is_error || aborted {
                        ToolCallStatus::Failed
                    } else {
                        ToolCallStatus::Completed
                    })
                    .content(vec![ToolCallContent::from(ContentBlock::Text(
                        TextContent::new(content),
                    ))]);
                self.emit_update(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                    truncate_to_max_bytes(&terminal_text::sanitize(&id), MAX_TOOL_ID_BYTES),
                    fields,
                )))?;
            }
            AgentEvent::Notice { message, .. } => {
                eprintln!("{}", terminal_text::sanitize(&message));
            }
            AgentEvent::TurnStarted { .. }
            | AgentEvent::ModelRequestStarted { .. }
            | AgentEvent::AssistantMessage { .. }
            | AgentEvent::TurnCancelled
            | AgentEvent::TurnCompleted => {}
        }
        Ok(())
    }
}

#[derive(Debug)]
struct PreparedPrompt {
    text: String,
    images: Vec<(String, String)>,
}

enum BoundedLine {
    Line(Vec<u8>),
    Oversized,
    Eof,
}

pub async fn run(config: Config) -> Result<()> {
    let config = Arc::new(config);
    let state = Arc::new(AsyncMutex::new(ServerState::new()));
    let turns = Arc::new(Semaphore::new(MAX_CONCURRENT_TURNS));
    let (sender, receiver) = mpsc::channel(MAX_OUTPUT_QUEUE);
    let output = Output { sender };
    let mut writer = tokio::spawn(write_output(receiver));
    let mut tasks = JoinSet::new();
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut writer_terminated = false;
    let input_result = loop {
        while tasks.try_join_next().is_some() {}
        let line = tokio::select! {
            biased;
            writer_result = &mut writer => {
                writer_terminated = true;
                break writer_result
                    .context("ACP output task failed")?
                    .context("ACP output disconnected");
            }
            line = read_bounded_line(&mut reader, MAX_INPUT_LINE_BYTES) => line,
        };
        match line {
            Ok(BoundedLine::Eof) => break Ok(()),
            Ok(BoundedLine::Oversized) => {
                let response = error_response(
                    RequestId::Null,
                    AcpError::invalid_request().data("input line exceeds ACP limit"),
                )?;
                if let Err(error) = output.send(response).await {
                    break Err(error.context("ACP output disconnected"));
                }
            }
            Ok(BoundedLine::Line(line)) => {
                if let Err(error) = handle_line(
                    &line,
                    Arc::clone(&config),
                    Arc::clone(&state),
                    Arc::clone(&turns),
                    output.clone(),
                    &mut tasks,
                )
                .await
                {
                    break Err(error);
                }
            }
            Err(error) => break Err(error.into()),
        }
    };

    cancel_all(&state).await;
    while tasks.join_next().await.is_some() {}
    drop(output);
    if !writer_terminated {
        writer.await.context("ACP output task failed")??;
    }
    input_result
}

async fn handle_line(
    line: &[u8],
    config: Arc<Config>,
    state: Arc<AsyncMutex<ServerState>>,
    turns: Arc<Semaphore>,
    output: Output,
    tasks: &mut JoinSet<()>,
) -> Result<()> {
    let value: Value = match serde_json::from_slice(line) {
        Ok(value) => value,
        Err(_) => {
            output
                .send(error_response(RequestId::Null, AcpError::parse_error())?)
                .await?;
            return Ok(());
        }
    };
    if !decoded_request_within_bounds(
        &value,
        MAX_DECODED_REQUEST_BYTES,
        MAX_DECODED_REQUEST_NODES,
        MAX_DECODED_REQUEST_DEPTH,
    ) {
        output
            .send(error_response(
                RequestId::Null,
                AcpError::invalid_request().data("decoded request exceeds ACP limit"),
            )?)
            .await?;
        return Ok(());
    }
    let object = match value.as_object() {
        Some(object) => object,
        None => {
            output
                .send(error_response(
                    RequestId::Null,
                    AcpError::invalid_request(),
                )?)
                .await?;
            return Ok(());
        }
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        output
            .send(error_response(
                RequestId::Null,
                AcpError::invalid_request(),
            )?)
            .await?;
        return Ok(());
    }
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        output
            .send(error_response(
                RequestId::Null,
                AcpError::invalid_request(),
            )?)
            .await?;
        return Ok(());
    };
    let params = object.get("params").cloned();
    let Some(id_value) = object.get("id") else {
        handle_notification(method, params, &state).await;
        return Ok(());
    };
    let id: RequestId = match serde_json::from_value(id_value.clone()) {
        Ok(id) => id,
        Err(_) => {
            output
                .send(error_response(
                    RequestId::Null,
                    AcpError::invalid_request(),
                )?)
                .await?;
            return Ok(());
        }
    };

    if method != AGENT_METHOD_NAMES.initialize && !is_initialized(&state).await {
        output
            .send(error_response(
                id,
                invalid_state("initialize must run first"),
            )?)
            .await?;
        return Ok(());
    }

    match method {
        method if method == AGENT_METHOD_NAMES.initialize => {
            let request = match parse_params::<InitializeRequest>(params) {
                Ok(request) => request,
                Err(error) => {
                    output.send(error_response(id, error)?).await?;
                    return Ok(());
                }
            };
            let mut state = state.lock().await;
            if state.initialized {
                drop(state);
                output
                    .send(error_response(id, invalid_state("already initialized"))?)
                    .await?;
                return Ok(());
            }
            state.initialized = true;
            drop(state);
            let version = if request.protocol_version == ProtocolVersion::V1 {
                request.protocol_version
            } else {
                ProtocolVersion::V1
            };
            let capabilities =
                AgentCapabilities::new().prompt_capabilities(PromptCapabilities::new().image(true));
            let response = InitializeResponse::new(version)
                .agent_capabilities(capabilities)
                .agent_info(Implementation::new("ferrum", env!("CARGO_PKG_VERSION")));
            output.send(success_response(id, response)?).await?;
        }
        method if method == AGENT_METHOD_NAMES.session_new => {
            if let Err(error) = validate_new_session_params(params.as_ref()) {
                output.send(error_response(id, error)?).await?;
                return Ok(());
            }
            let request = match parse_params::<NewSessionRequest>(params) {
                Ok(request) => request,
                Err(error) => {
                    output.send(error_response(id, error)?).await?;
                    return Ok(());
                }
            };
            if !request.additional_directories.is_empty() {
                output
                    .send(error_response(
                        id,
                        AcpError::invalid_params()
                            .data("additionalDirectories is not supported by this baseline"),
                    )?)
                    .await?;
                return Ok(());
            }
            if !request.mcp_servers.is_empty() {
                output
                    .send(error_response(
                        id,
                        AcpError::invalid_params()
                            .data("client-supplied MCP servers are not supported yet"),
                    )?)
                    .await?;
                return Ok(());
            }
            let cwd = match validate_cwd(&request.cwd) {
                Ok(cwd) => cwd,
                Err(error) => {
                    output.send(error_response(id, error)?).await?;
                    return Ok(());
                }
            };
            {
                let state = state.lock().await;
                if state.sessions.len() >= MAX_SESSIONS {
                    drop(state);
                    output
                        .send(error_response(id, invalid_state("session limit reached"))?)
                        .await?;
                    return Ok(());
                }
            }
            let agent = match AgentSession::new_at_cwd(&config, cwd) {
                Ok(agent) => agent,
                Err(_) => {
                    output
                        .send(error_response(id, AcpError::internal_error())?)
                        .await?;
                    return Ok(());
                }
            };
            let session_id = Uuid::new_v4().to_string();
            let entry = Arc::new(SessionEntry {
                agent: AsyncMutex::new(agent),
                active: Mutex::new(None),
            });
            let mut state = state.lock().await;
            if state.sessions.len() >= MAX_SESSIONS {
                drop(state);
                output
                    .send(error_response(id, invalid_state("session limit reached"))?)
                    .await?;
                return Ok(());
            }
            state.sessions.insert(session_id.clone(), entry);
            drop(state);
            output
                .send(success_response(id, NewSessionResponse::new(session_id))?)
                .await?;
        }
        method if method == AGENT_METHOD_NAMES.session_prompt => {
            if let Err(error) = validate_prompt_params(params.as_ref()) {
                output.send(error_response(id, error)?).await?;
                return Ok(());
            }
            let request = match parse_params::<PromptRequest>(params) {
                Ok(request) => request,
                Err(error) => {
                    output.send(error_response(id, error)?).await?;
                    return Ok(());
                }
            };
            let session_id = request.session_id.to_string();
            if session_id.is_empty() || session_id.len() > MAX_SESSION_ID_BYTES {
                output
                    .send(error_response(id, AcpError::invalid_params())?)
                    .await?;
                return Ok(());
            }
            let prepared = match prepare_prompt(request.prompt) {
                Ok(prepared) => prepared,
                Err(error) => {
                    output.send(error_response(id, error)?).await?;
                    return Ok(());
                }
            };
            let entry = {
                let mut state = state.lock().await;
                let Some(entry) = state.sessions.get(&session_id).cloned() else {
                    drop(state);
                    output
                        .send(error_response(id, AcpError::resource_not_found(None))?)
                        .await?;
                    return Ok(());
                };
                if state.active_request_ids.contains(&id) {
                    drop(state);
                    output
                        .send(error_response(
                            id,
                            invalid_state("request id is already active"),
                        )?)
                        .await?;
                    return Ok(());
                }
                state.active_request_ids.insert(id.clone());
                entry
            };
            let cancellation = TurnCancellation::new();
            let already_active = {
                let mut active = entry
                    .active
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if active.is_some() {
                    true
                } else {
                    *active = Some(ActivePrompt {
                        request_id: id.clone(),
                        cancellation: cancellation.clone(),
                    });
                    false
                }
            };
            if already_active {
                state.lock().await.active_request_ids.remove(&id);
                output
                    .send(error_response(
                        id,
                        invalid_state("session prompt already active"),
                    )?)
                    .await?;
                return Ok(());
            }
            let permit = match turns.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    clear_active(&state, &entry, &id).await;
                    output
                        .send(error_response(
                            id,
                            invalid_state("concurrent turn limit reached"),
                        )?)
                        .await?;
                    return Ok(());
                }
            };
            let task_state = Arc::clone(&state);
            let task_config = Arc::clone(&config);
            let task_output = output.clone();
            tasks.spawn(async move {
                let _permit = permit;
                let result = run_prompt(
                    &entry,
                    &session_id,
                    prepared,
                    &task_config,
                    cancellation,
                    task_output.clone(),
                )
                .await;
                let response = match result {
                    Ok(outcome) => success_response(
                        id.clone(),
                        PromptResponse::new(match outcome {
                            TurnOutcome::Completed => StopReason::EndTurn,
                            TurnOutcome::Cancelled => StopReason::Cancelled,
                        }),
                    ),
                    Err(error) => error_response(id.clone(), error),
                };
                clear_active(&task_state, &entry, &id).await;
                if let Ok(response) = response {
                    let _ = task_output.send(response).await;
                }
            });
        }
        _ => {
            output
                .send(error_response(id, AcpError::method_not_found())?)
                .await?;
        }
    }
    Ok(())
}

async fn run_prompt(
    entry: &SessionEntry,
    session_id: &str,
    prompt: PreparedPrompt,
    config: &Config,
    cancellation: TurnCancellation,
    output: Output,
) -> std::result::Result<TurnOutcome, AcpError> {
    let mut agent = entry.agent.lock().await;
    if !prompt.images.is_empty() {
        agent
            .attach_data_images(prompt.images)
            .map_err(|_| AcpError::invalid_params().data("invalid image prompt content"))?;
    }
    let mut sink = AcpEventSink::new(session_id.to_string(), output, config.max_context_tokens);
    agent
        .run_turn_with_events(
            prompt.text,
            config,
            TurnOptions::headless(cancellation),
            &mut sink,
        )
        .await
        .map_err(|_| AcpError::internal_error())
}

async fn handle_notification(
    method: &str,
    params: Option<Value>,
    state: &Arc<AsyncMutex<ServerState>>,
) {
    if method == AGENT_METHOD_NAMES.session_cancel {
        if let Ok(cancel) = parse_params::<CancelNotification>(params) {
            cancel_session(state, &cancel.session_id.to_string()).await;
        }
    } else if method == "$/cancel_request" {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct CancelById {
            request_id: RequestId,
        }
        if let Ok(cancel) = parse_params::<CancelById>(params) {
            cancel_request(state, &cancel.request_id).await;
        }
    }
}

async fn cancel_session(state: &Arc<AsyncMutex<ServerState>>, session_id: &str) {
    let entry = state.lock().await.sessions.get(session_id).cloned();
    if let Some(entry) = entry {
        let active = entry
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(active) = active.as_ref() {
            active.cancellation.cancel();
        }
    }
}

async fn cancel_request(state: &Arc<AsyncMutex<ServerState>>, request_id: &RequestId) {
    let sessions = state
        .lock()
        .await
        .sessions
        .values()
        .cloned()
        .collect::<Vec<_>>();
    for entry in sessions {
        let active = entry
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if active
            .as_ref()
            .is_some_and(|active| &active.request_id == request_id)
        {
            active.as_ref().unwrap().cancellation.cancel();
            return;
        }
    }
}

async fn cancel_all(state: &Arc<AsyncMutex<ServerState>>) {
    let sessions = state
        .lock()
        .await
        .sessions
        .values()
        .cloned()
        .collect::<Vec<_>>();
    for entry in sessions {
        let active = entry
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(active) = active.as_ref() {
            active.cancellation.cancel();
        }
    }
}

async fn clear_active(
    state: &Arc<AsyncMutex<ServerState>>,
    entry: &SessionEntry,
    request_id: &RequestId,
) {
    {
        let mut active = entry
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if active
            .as_ref()
            .is_some_and(|active| &active.request_id == request_id)
        {
            *active = None;
        }
    }
    state.lock().await.active_request_ids.remove(request_id);
}

async fn is_initialized(state: &Arc<AsyncMutex<ServerState>>) -> bool {
    state.lock().await.initialized
}

fn validate_new_session_params(params: Option<&Value>) -> std::result::Result<(), AcpError> {
    let object = params
        .and_then(Value::as_object)
        .ok_or_else(AcpError::invalid_params)?;
    if !object.get("cwd").is_some_and(Value::is_string)
        || !object.get("mcpServers").is_some_and(Value::is_array)
    {
        return Err(AcpError::invalid_params());
    }
    if let Some(directories) = object.get("additionalDirectories") {
        let Some(directories) = directories.as_array() else {
            return Err(AcpError::invalid_params());
        };
        if directories.iter().any(|directory| !directory.is_string()) {
            return Err(AcpError::invalid_params());
        }
    }
    let servers = object["mcpServers"].as_array().unwrap();
    if servers.iter().any(|server| {
        serde_json::from_value::<agent_client_protocol_schema::v1::McpServer>(server.clone())
            .is_err()
    }) {
        return Err(AcpError::invalid_params());
    }
    Ok(())
}

fn validate_prompt_params(params: Option<&Value>) -> std::result::Result<(), AcpError> {
    let object = params
        .and_then(Value::as_object)
        .ok_or_else(AcpError::invalid_params)?;
    if !object.get("sessionId").is_some_and(Value::is_string) {
        return Err(AcpError::invalid_params());
    }
    let blocks = object
        .get("prompt")
        .and_then(Value::as_array)
        .ok_or_else(AcpError::invalid_params)?;
    if blocks
        .iter()
        .any(|block| serde_json::from_value::<ContentBlock>(block.clone()).is_err())
    {
        return Err(AcpError::invalid_params());
    }
    Ok(())
}

fn prepare_prompt(blocks: Vec<ContentBlock>) -> std::result::Result<PreparedPrompt, AcpError> {
    if blocks.is_empty() {
        return Err(AcpError::invalid_params().data("prompt must not be empty"));
    }
    let mut text = String::new();
    let mut images = Vec::new();
    let mut resource_count = 0usize;
    for block in blocks {
        match block {
            ContentBlock::Text(content) => append_prompt_text(&mut text, &content.text)?,
            ContentBlock::Image(image) => images.push((image.mime_type, image.data)),
            ContentBlock::ResourceLink(resource) => {
                resource_count += 1;
                if resource_count > MAX_RESOURCE_LINKS
                    || resource.name.len() > MAX_RESOURCE_NAME_BYTES
                    || resource.uri.len() > MAX_RESOURCE_URI_BYTES
                    || Url::parse(&resource.uri).is_err()
                {
                    return Err(
                        AcpError::invalid_params().data("invalid or oversized resource link")
                    );
                }
                let description = format!(
                    "\n\n[Untrusted resource link supplied by the user]\nName: {}\nURI: {}\n",
                    resource.name, resource.uri
                );
                append_prompt_text(&mut text, &description)?;
            }
            ContentBlock::Audio(_) | ContentBlock::Resource(_) => {
                return Err(AcpError::invalid_params().data("unsupported prompt content type"));
            }
            _ => {
                return Err(AcpError::invalid_params().data("unsupported prompt content type"));
            }
        }
    }
    if text.is_empty() && images.is_empty() {
        return Err(AcpError::invalid_params().data("prompt has no usable content"));
    }
    if text.is_empty() {
        text.push_str("User attached image content.");
    }
    Ok(PreparedPrompt { text, images })
}

fn append_prompt_text(text: &mut String, addition: &str) -> std::result::Result<(), AcpError> {
    if text.len().saturating_add(addition.len()) > MAX_PROMPT_TEXT_BYTES {
        return Err(AcpError::invalid_params().data("prompt text exceeds ACP limit"));
    }
    text.push_str(addition);
    Ok(())
}

fn validate_cwd(path: &Path) -> std::result::Result<PathBuf, AcpError> {
    if !path.is_absolute() {
        return Err(AcpError::invalid_params().data("cwd must be absolute"));
    }
    let canonical = path
        .canonicalize()
        .map_err(|_| AcpError::invalid_params().data("cwd does not exist"))?;
    if !canonical.is_dir() {
        return Err(AcpError::invalid_params().data("cwd must be a directory"));
    }
    Ok(canonical)
}

fn parse_params<T: DeserializeOwned>(params: Option<Value>) -> std::result::Result<T, AcpError> {
    let params = params.ok_or_else(AcpError::invalid_params)?;
    serde_json::from_value(params).map_err(|_| AcpError::invalid_params())
}

fn invalid_state(message: &str) -> AcpError {
    AcpError::invalid_params().data(message)
}

fn success_response(id: RequestId, result: impl Serialize) -> Result<Value> {
    let result = serde_json::to_value(result)?;
    Ok(serde_json::to_value(JsonRpcMessage::wrap(Response::<
        Value,
    >::Result {
        id,
        result,
    }))?)
}

fn error_response(id: RequestId, error: AcpError) -> Result<Value> {
    Ok(serde_json::to_value(JsonRpcMessage::wrap(Response::<
        Value,
    >::Error {
        id,
        error,
    }))?)
}

async fn write_output(mut receiver: mpsc::Receiver<Value>) -> Result<()> {
    let stdout = io::stdout();
    let mut writer = BufWriter::new(stdout);
    while let Some(value) = receiver.recv().await {
        let bytes = serde_json::to_vec(&value)?;
        if bytes.len() > MAX_OUTPUT_LINE_BYTES {
            anyhow::bail!("ACP output line exceeds configured limit");
        }
        writer.write_all(&bytes).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
    }
    Ok(())
}

async fn read_bounded_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    max_bytes: usize,
) -> io::Result<BoundedLine> {
    let mut line = Vec::new();
    let mut oversized = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if line.is_empty() && !oversized {
                return Ok(BoundedLine::Eof);
            }
            return Ok(if oversized {
                BoundedLine::Oversized
            } else {
                strip_line_ending(&mut line);
                BoundedLine::Line(line)
            });
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        if !oversized {
            if line.len().saturating_add(consumed) > max_bytes.saturating_add(1) {
                oversized = true;
                line.clear();
            } else {
                line.extend_from_slice(&available[..consumed]);
            }
        }
        reader.consume(consumed);
        if newline.is_some() {
            if oversized {
                return Ok(BoundedLine::Oversized);
            }
            strip_line_ending(&mut line);
            return Ok(BoundedLine::Line(line));
        }
    }
}

fn decoded_request_within_bounds(
    value: &Value,
    max_bytes: usize,
    max_nodes: usize,
    max_depth: usize,
) -> bool {
    fn visit(
        value: &Value,
        depth: usize,
        max_bytes: usize,
        max_nodes: usize,
        max_depth: usize,
        bytes: &mut usize,
        nodes: &mut usize,
    ) -> bool {
        *nodes = nodes.saturating_add(1);
        if *nodes > max_nodes || depth > max_depth {
            return false;
        }
        match value {
            Value::String(value) => {
                *bytes = bytes.saturating_add(value.len());
            }
            Value::Array(values) => {
                for value in values {
                    if !visit(
                        value,
                        depth + 1,
                        max_bytes,
                        max_nodes,
                        max_depth,
                        bytes,
                        nodes,
                    ) {
                        return false;
                    }
                }
            }
            Value::Object(values) => {
                for (key, value) in values {
                    *bytes = bytes.saturating_add(key.len());
                    if !visit(
                        value,
                        depth + 1,
                        max_bytes,
                        max_nodes,
                        max_depth,
                        bytes,
                        nodes,
                    ) {
                        return false;
                    }
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
        *bytes <= max_bytes
    }

    let mut bytes = 0usize;
    let mut nodes = 0usize;
    visit(
        value, 0, max_bytes, max_nodes, max_depth, &mut bytes, &mut nodes,
    )
}

fn strip_line_ending(line: &mut Vec<u8>) {
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
}

fn utf8_chunks(text: &str, max_bytes: usize) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let mut end = rest.len().min(max_bytes);
        while end > 0 && !rest.is_char_boundary(end) {
            end -= 1;
        }
        if end == 0 {
            end = rest
                .char_indices()
                .nth(1)
                .map_or(rest.len(), |(index, _)| index);
        }
        chunks.push(rest[..end].to_string());
        rest = &rest[end..];
    }
    chunks
}

fn tool_kind(name: &str) -> ToolKind {
    match name {
        "read" | "history_read" => ToolKind::Read,
        "write" | "edit" => ToolKind::Edit,
        "grep" | "find" | "ls" | "history_search" => ToolKind::Search,
        "bash" | "wait" => ToolKind::Execute,
        name if name.contains("fetch") || name.contains("web") => ToolKind::Fetch,
        _ => ToolKind::Other,
    }
}

fn bounded_sanitized_json(value: Value, max_bytes: usize) -> Option<Value> {
    let value = sanitize_json(value, 0);
    let size = serde_json::to_vec(&value).ok()?.len();
    (size <= max_bytes).then_some(value)
}

fn sanitize_json(value: Value, depth: usize) -> Value {
    if depth > 32 {
        return Value::String("[nested value omitted]".to_string());
    }
    match value {
        Value::String(value) => Value::String(terminal_text::sanitize(&value)),
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|value| sanitize_json(value, depth + 1))
                .collect(),
        ),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| {
                    (
                        terminal_text::sanitize(&key),
                        sanitize_json(value, depth + 1),
                    )
                })
                .collect(),
        ),
        value => value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn bounded_reader_recovers_after_oversized_line() {
        let input = b"123456789\n{}\n".as_slice();
        let mut reader = BufReader::new(input);
        assert!(matches!(
            read_bounded_line(&mut reader, 4).await.unwrap(),
            BoundedLine::Oversized
        ));
        let BoundedLine::Line(line) = read_bounded_line(&mut reader, 4).await.unwrap() else {
            panic!("expected second line");
        };
        assert_eq!(line, b"{}");
    }

    #[test]
    fn decoded_request_budget_limits_nodes_bytes_and_depth() {
        assert!(decoded_request_within_bounds(
            &json!({"a": [1, 2]}),
            8,
            8,
            3
        ));
        assert!(!decoded_request_within_bounds(&json!([1, 2, 3]), 8, 3, 3));
        assert!(!decoded_request_within_bounds(&json!("12345"), 4, 8, 3));
        assert!(!decoded_request_within_bounds(
            &json!({"a": {"b": {"c": 1}}}),
            32,
            16,
            2,
        ));
    }

    #[test]
    fn prompt_supports_text_resource_links_and_images() {
        let prompt = prepare_prompt(vec![
            ContentBlock::Text(TextContent::new("hello")),
            ContentBlock::ResourceLink(agent_client_protocol_schema::v1::ResourceLink::new(
                "readme",
                "file:///tmp/README.md",
            )),
            ContentBlock::Image(agent_client_protocol_schema::v1::ImageContent::new(
                "AA==",
                "image/png",
            )),
        ])
        .unwrap();
        assert!(prompt.text.contains("hello"));
        assert!(prompt.text.contains("Untrusted resource link"));
        assert_eq!(prompt.images.len(), 1);
    }

    #[test]
    fn prompt_rejects_unadvertised_content() {
        let error = prepare_prompt(vec![ContentBlock::Audio(
            agent_client_protocol_schema::v1::AudioContent::new("AA==", "audio/wav"),
        )])
        .unwrap_err();
        assert_eq!(i32::from(error.code), -32602);
    }

    #[test]
    fn update_chunks_are_utf8_safe_and_bounded() {
        let text = format!("{}é{}", "x".repeat(5), "y".repeat(5));
        let chunks = utf8_chunks(&text, 6);
        assert_eq!(chunks.concat(), text);
        assert!(chunks.iter().all(|chunk| chunk.len() <= 6));
    }

    #[test]
    fn tool_input_controls_are_removed_and_size_is_bounded() {
        let value = bounded_sanitized_json(json!({"command": "safe\u{1b}[31m"}), 1024).unwrap();
        assert_eq!(value["command"], "safe");
        assert!(bounded_sanitized_json(json!({"value": "x".repeat(1024)}), 16).is_none());
    }
}
