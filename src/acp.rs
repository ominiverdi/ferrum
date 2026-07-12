use crate::{
    agent::{
        AgentSession, HeadlessCommandInvocation, HeadlessCommandOutcome,
        events::{
            AgentEvent, AgentEventSink, ToolPermissionDecision, ToolPermissionHandler,
            ToolPermissionRequest, TurnCancellation, TurnOptions, TurnOutcome,
        },
        headless_commands,
        messages::{ContentBlock as FerrumContentBlock, Role as FerrumRole},
        parse_headless_command, restore_session_preferences,
    },
    cli::AcpPermissionPolicy,
    config::Config,
    mcp, session, terminal_text,
    text_truncate::truncate_to_max_bytes,
};
use agent_client_protocol_schema::{
    ProtocolVersion,
    v1::{
        AGENT_METHOD_NAMES, AgentCapabilities, AvailableCommand, AvailableCommandInput,
        AvailableCommandsUpdate, CLIENT_METHOD_NAMES, CancelNotification, CloseSessionRequest,
        CloseSessionResponse, ContentBlock, ContentChunk, DeleteSessionRequest,
        DeleteSessionResponse, Error as AcpError, Implementation, InitializeRequest,
        InitializeResponse, JsonRpcMessage, ListSessionsRequest, ListSessionsResponse,
        LoadSessionRequest, LoadSessionResponse, McpCapabilities, McpServer, NewSessionRequest,
        NewSessionResponse, Notification, PermissionOption, PermissionOptionKind,
        PromptCapabilities, PromptRequest, PromptResponse, RequestId, RequestPermissionOutcome,
        RequestPermissionRequest, RequestPermissionResponse, Response, ResumeSessionRequest,
        ResumeSessionResponse, SessionCapabilities, SessionCloseCapabilities,
        SessionDeleteCapabilities, SessionInfo as AcpSessionInfo, SessionListCapabilities,
        SessionNotification, SessionResumeCapabilities, SessionUpdate, StopReason, TextContent,
        ToolCall, ToolCallContent, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields, ToolKind,
        UnstructuredCommandInput, UsageUpdate,
    },
};
use anyhow::{Context, Result};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{self, AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter},
    sync::{Mutex as AsyncMutex, Semaphore, mpsc, oneshot},
    task::JoinSet,
};
use url::Url;

const MAX_INPUT_LINE_BYTES: usize = 32 * 1024 * 1024;
const MAX_DECODED_REQUEST_BYTES: usize = 30 * 1024 * 1024;
const MAX_DECODED_REQUEST_NODES: usize = 100_000;
const MAX_DECODED_REQUEST_DEPTH: usize = 64;
const MAX_OUTPUT_LINE_BYTES: usize = 256 * 1024;
const MAX_OUTPUT_QUEUE: usize = 128;
const MAX_SESSIONS: usize = 16;
const MAX_LIST_PAGE: usize = 100;
const MAX_SESSION_DIRECTORY_ENTRIES: usize = 20_000;
const MAX_PERSISTED_SESSIONS: usize = 10_000;
const MAX_LIST_PAYLOAD_BYTES: usize = 128 * 1024;
const MAX_SESSION_CWD_BYTES: usize = 16 * 1024;
const MAX_SESSION_TITLE_BYTES: usize = 1024;
const MAX_LOAD_ENTRIES: usize = 10_000;
const MAX_LOAD_HISTORY_BYTES: usize = 64 * 1024 * 1024;
const MAX_REPLAY_IMAGE_BASE64_BYTES: usize = 128 * 1024;
const MAX_REPLAY_IMAGE_MIME_BYTES: usize = 256;
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
const MAX_CLIENT_MCP_SERVERS: usize = 16;
const MAX_CLIENT_MCP_SERVER_NAME_BYTES: usize = 256;
const MAX_CLIENT_MCP_COMMAND_BYTES: usize = 16 * 1024;
const MAX_CLIENT_MCP_ARGS: usize = 256;
const MAX_CLIENT_MCP_ARG_BYTES: usize = 16 * 1024;
const MAX_CLIENT_MCP_ARG_TOTAL_BYTES: usize = 128 * 1024;
const MAX_CLIENT_MCP_ENV: usize = 128;
const MAX_CLIENT_MCP_ENV_NAME_BYTES: usize = 256;
const MAX_CLIENT_MCP_ENV_VALUE_BYTES: usize = 64 * 1024;
const MAX_CLIENT_MCP_ENV_TOTAL_BYTES: usize = 256 * 1024;
const PERMISSION_RESPONSE_TIMEOUT: Duration = Duration::from_secs(300);
const PERMISSION_ALLOW_ONCE_ID: &str = "allow_once";
const PERMISSION_REJECT_ONCE_ID: &str = "reject_once";

static NEXT_PERMISSION_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

struct ServerState {
    initialized: bool,
    sessions: HashMap<String, Arc<SessionEntry>>,
    opening_session_ids: HashSet<String>,
    active_request_ids: HashSet<RequestId>,
    pending_client_responses: HashMap<RequestId, oneshot::Sender<Value>>,
}

impl ServerState {
    fn new() -> Self {
        Self {
            initialized: false,
            sessions: HashMap::new(),
            opening_session_ids: HashSet::new(),
            active_request_ids: HashSet::new(),
            pending_client_responses: HashMap::new(),
        }
    }
}

struct SessionEntry {
    agent: AsyncMutex<AgentSession>,
    config: Config,
    active: Mutex<Option<ActivePrompt>>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AcpPolicy {
    pub restore: SessionRestorePolicy,
    pub permissions: AcpPermissionPolicy,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SessionRestorePolicy {
    pub thinking: bool,
    pub safety: bool,
    pub tools: bool,
    pub provider: bool,
    pub model: bool,
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

struct AcpPermissionHandler {
    session_id: String,
    output: Output,
    state: Arc<AsyncMutex<ServerState>>,
}

impl ToolPermissionHandler for AcpPermissionHandler {
    fn request(
        &self,
        request: ToolPermissionRequest,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ToolPermissionDecision>> + Send + '_>,
    > {
        Box::pin(async move {
            if request.cancellation.is_cancelled() {
                return Ok(ToolPermissionDecision::Cancelled);
            }
            let sequence = NEXT_PERMISSION_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
            let request_id: RequestId =
                serde_json::from_value(Value::String(format!("ferrum-permission-{sequence}")))?;
            let mut fields = ToolCallUpdateFields::new()
                .title(terminal_text::sanitize_title(&request.name))
                .kind(tool_kind(&request.name))
                .status(ToolCallStatus::Pending);
            if let Some(input) = bounded_sanitized_json(request.input, MAX_TOOL_INPUT_BYTES) {
                fields = fields.raw_input(input);
            }
            let params = RequestPermissionRequest::new(
                self.session_id.clone(),
                ToolCallUpdate::new(
                    truncate_to_max_bytes(&terminal_text::sanitize(&request.id), MAX_TOOL_ID_BYTES),
                    fields,
                ),
                vec![
                    PermissionOption::new(
                        PERMISSION_ALLOW_ONCE_ID,
                        "Allow once",
                        PermissionOptionKind::AllowOnce,
                    ),
                    PermissionOption::new(
                        PERMISSION_REJECT_ONCE_ID,
                        "Reject",
                        PermissionOptionKind::RejectOnce,
                    ),
                ],
            );
            let (sender, receiver) = oneshot::channel();
            {
                let mut state = self.state.lock().await;
                state
                    .pending_client_responses
                    .insert(request_id.clone(), sender);
            }
            let message = serde_json::json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "method": CLIENT_METHOD_NAMES.session_request_permission,
                "params": params,
            });
            if let Err(error) = self.output.send(message).await {
                self.state
                    .lock()
                    .await
                    .pending_client_responses
                    .remove(&request_id);
                return Err(error);
            }
            let response = tokio::select! {
                biased;
                _ = request.cancellation.cancelled() => {
                    self.state.lock().await.pending_client_responses.remove(&request_id);
                    return Ok(ToolPermissionDecision::Cancelled);
                }
                response = tokio::time::timeout(PERMISSION_RESPONSE_TIMEOUT, receiver) => response,
            };
            let value = match response {
                Ok(Ok(value)) => value,
                Ok(Err(_)) => return Ok(ToolPermissionDecision::Cancelled),
                Err(_) => {
                    self.state
                        .lock()
                        .await
                        .pending_client_responses
                        .remove(&request_id);
                    return Ok(ToolPermissionDecision::Reject);
                }
            };
            if value.get("error").is_some() {
                return Ok(ToolPermissionDecision::Reject);
            }
            let Some(result) = value.get("result") else {
                return Ok(ToolPermissionDecision::Reject);
            };
            let response: RequestPermissionResponse = match serde_json::from_value(result.clone()) {
                Ok(response) => response,
                Err(_) => return Ok(ToolPermissionDecision::Reject),
            };
            Ok(match response.outcome {
                RequestPermissionOutcome::Cancelled => ToolPermissionDecision::Cancelled,
                RequestPermissionOutcome::Selected(selected)
                    if selected.option_id.to_string() == PERMISSION_ALLOW_ONCE_ID =>
                {
                    ToolPermissionDecision::Allow
                }
                _ => ToolPermissionDecision::Reject,
            })
        })
    }
}

async fn send_session_update(
    output: &Output,
    session_id: &str,
    update: SessionUpdate,
) -> Result<()> {
    let notification = Notification {
        method: CLIENT_METHOD_NAMES.session_update.into(),
        params: Some(SessionNotification::new(session_id.to_string(), update)),
    };
    output
        .send(serde_json::to_value(JsonRpcMessage::wrap(notification))?)
        .await
}

async fn send_available_commands(output: &Output, session_id: &str) -> Result<()> {
    let commands = headless_commands()
        .iter()
        .map(|spec| {
            let command = AvailableCommand::new(spec.name, spec.description);
            if let Some(hint) = spec.input_hint {
                command.input(AvailableCommandInput::Unstructured(
                    UnstructuredCommandInput::new(hint),
                ))
            } else {
                command
            }
        })
        .collect();
    send_session_update(
        output,
        session_id,
        SessionUpdate::AvailableCommandsUpdate(AvailableCommandsUpdate::new(commands)),
    )
    .await
}

fn load_session_history(
    path: &Path,
) -> std::result::Result<Vec<crate::agent::messages::Message>, AcpError> {
    session::jsonl::load_visible_history_messages_bounded(
        path,
        MAX_LOAD_ENTRIES,
        MAX_LOAD_HISTORY_BYTES,
    )
    .map_err(|_| {
        AcpError::internal_error().data("session history exceeds load limits or is unreadable")
    })
}

fn validate_session_history(path: &Path) -> std::result::Result<(), AcpError> {
    session::jsonl::validate_session_history_bounded(path, MAX_LOAD_ENTRIES, MAX_LOAD_HISTORY_BYTES)
        .map_err(|_| {
            AcpError::internal_error().data("session history exceeds load limits or is unreadable")
        })
}

async fn replay_session_history(
    output: &Output,
    session_id: &str,
    messages: Vec<crate::agent::messages::Message>,
) -> std::result::Result<(), AcpError> {
    for message in messages {
        let role = message.role;
        for block in message.content {
            let update = match block {
                FerrumContentBlock::Text { text } => {
                    let update = match &role {
                        FerrumRole::User => SessionUpdate::UserMessageChunk,
                        FerrumRole::Assistant => SessionUpdate::AgentMessageChunk,
                        FerrumRole::System | FerrumRole::Tool => continue,
                    };
                    for chunk in utf8_chunks(&terminal_text::sanitize(&text), MAX_UPDATE_TEXT_BYTES)
                    {
                        send_session_update(
                            output,
                            session_id,
                            update(ContentChunk::new(ContentBlock::Text(TextContent::new(
                                chunk,
                            )))),
                        )
                        .await
                        .map_err(|_| AcpError::internal_error())?;
                    }
                    continue;
                }
                FerrumContentBlock::Thinking { text, .. } => {
                    for chunk in utf8_chunks(&terminal_text::sanitize(&text), MAX_UPDATE_TEXT_BYTES)
                    {
                        send_session_update(
                            output,
                            session_id,
                            SessionUpdate::AgentThoughtChunk(ContentChunk::new(
                                ContentBlock::Text(TextContent::new(chunk)),
                            )),
                        )
                        .await
                        .map_err(|_| AcpError::internal_error())?;
                    }
                    continue;
                }
                FerrumContentBlock::ToolUse { id, name, input } => {
                    let id =
                        truncate_to_max_bytes(&terminal_text::sanitize(&id), MAX_TOOL_ID_BYTES);
                    let title = terminal_text::sanitize_title(&name);
                    let mut call = ToolCall::new(id, title)
                        .kind(tool_kind(&name))
                        .status(ToolCallStatus::InProgress);
                    if let Some(input) = bounded_sanitized_json(input, MAX_TOOL_INPUT_BYTES) {
                        call = call.raw_input(input);
                    }
                    SessionUpdate::ToolCall(call)
                }
                FerrumContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    let content = truncate_to_max_bytes(
                        &terminal_text::sanitize(&content),
                        MAX_TOOL_CONTENT_BYTES,
                    );
                    let fields = ToolCallUpdateFields::new()
                        .status(if is_error {
                            ToolCallStatus::Failed
                        } else {
                            ToolCallStatus::Completed
                        })
                        .content(vec![ToolCallContent::from(ContentBlock::Text(
                            TextContent::new(content),
                        ))]);
                    SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                        truncate_to_max_bytes(
                            &terminal_text::sanitize(&tool_use_id),
                            MAX_TOOL_ID_BYTES,
                        ),
                        fields,
                    ))
                }
                FerrumContentBlock::Image {
                    mime_type,
                    data_base64,
                    ..
                } => {
                    if data_base64.len() > MAX_REPLAY_IMAGE_BASE64_BYTES
                        || mime_type.len() > MAX_REPLAY_IMAGE_MIME_BYTES
                    {
                        continue;
                    }
                    let content = ContentBlock::Image(
                        agent_client_protocol_schema::v1::ImageContent::new(data_base64, mime_type),
                    );
                    match &role {
                        FerrumRole::User => {
                            SessionUpdate::UserMessageChunk(ContentChunk::new(content))
                        }
                        FerrumRole::Assistant => {
                            SessionUpdate::AgentMessageChunk(ContentChunk::new(content))
                        }
                        FerrumRole::System | FerrumRole::Tool => continue,
                    }
                }
            };
            send_session_update(output, session_id, update)
                .await
                .map_err(|_| AcpError::internal_error())?;
        }
    }
    Ok(())
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
    command: Option<HeadlessCommandInvocation>,
}

enum BoundedLine {
    Line(Vec<u8>),
    Oversized,
    Eof,
}

pub async fn run(config: Config, policy: AcpPolicy) -> Result<()> {
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
                    policy,
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
    policy: AcpPolicy,
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
    if object.get("method").is_none()
        && object.get("id").is_some()
        && (object.get("result").is_some() || object.get("error").is_some())
    {
        handle_client_response(object, &state).await;
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
            let session_capabilities = SessionCapabilities::new()
                .list(SessionListCapabilities::new())
                .delete(SessionDeleteCapabilities::new())
                .resume(SessionResumeCapabilities::new())
                .close(SessionCloseCapabilities::new());
            let capabilities = AgentCapabilities::new()
                .load_session(true)
                .prompt_capabilities(PromptCapabilities::new().image(true))
                .mcp_capabilities(McpCapabilities::new().http(false).sse(false))
                .session_capabilities(session_capabilities);
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
            let requested_mcp_servers = request.mcp_servers;
            let cwd = match validate_cwd(&request.cwd) {
                Ok(cwd) => cwd,
                Err(error) => {
                    output.send(error_response(id, error)?).await?;
                    return Ok(());
                }
            };
            let session_config = match config.for_cwd(&cwd) {
                Ok(config) => config,
                Err(_) => {
                    output
                        .send(error_response(
                            id,
                            AcpError::invalid_params().data("project configuration is invalid"),
                        )?)
                        .await?;
                    return Ok(());
                }
            };
            let client_mcp_servers =
                match validate_client_mcp_servers(requested_mcp_servers, &session_config) {
                    Ok(servers) => servers,
                    Err(error) => {
                        output.send(error_response(id, error)?).await?;
                        return Ok(());
                    }
                };
            {
                let state = state.lock().await;
                if state
                    .sessions
                    .len()
                    .saturating_add(state.opening_session_ids.len())
                    >= MAX_SESSIONS
                {
                    drop(state);
                    output
                        .send(error_response(id, invalid_state("session limit reached"))?)
                        .await?;
                    return Ok(());
                }
            }
            let mut agent = match AgentSession::new_at_cwd(&session_config, cwd) {
                Ok(agent) => agent,
                Err(_) => {
                    output
                        .send(error_response(id, AcpError::internal_error())?)
                        .await?;
                    return Ok(());
                }
            };
            if let Err(_error) = agent
                .start_client_mcp(&session_config, client_mcp_servers)
                .await
            {
                let path = agent.session_path().to_path_buf();
                drop(agent);
                let _ = session::jsonl::delete_session(&path);
                output
                    .send(error_response(
                        id,
                        AcpError::internal_error().data("MCP session setup failed"),
                    )?)
                    .await?;
                return Ok(());
            }
            let session_info = match session::jsonl::session_info(agent.session_path()) {
                Ok(Some(info)) => info,
                _ => {
                    let path = agent.session_path().to_path_buf();
                    drop(agent);
                    let _ = session::jsonl::delete_session(&path);
                    output
                        .send(error_response(id, AcpError::internal_error())?)
                        .await?;
                    return Ok(());
                }
            };
            let session_id = session_info.id;
            let session_path = agent.session_path().to_path_buf();
            let entry = Arc::new(SessionEntry {
                agent: AsyncMutex::new(agent),
                config: session_config,
                active: Mutex::new(None),
            });
            let mut state = state.lock().await;
            if state
                .sessions
                .len()
                .saturating_add(state.opening_session_ids.len())
                >= MAX_SESSIONS
            {
                drop(state);
                drop(entry);
                let _ = session::jsonl::delete_session(&session_path);
                output
                    .send(error_response(id, invalid_state("session limit reached"))?)
                    .await?;
                return Ok(());
            }
            state.sessions.insert(session_id.clone(), entry);
            drop(state);
            output
                .send(success_response(
                    id,
                    NewSessionResponse::new(session_id.clone()),
                )?)
                .await?;
            send_available_commands(&output, &session_id).await?;
        }
        method if method == AGENT_METHOD_NAMES.session_list => {
            let request = match parse_params::<ListSessionsRequest>(params) {
                Ok(request) => request,
                Err(error) => {
                    output.send(error_response(id, error)?).await?;
                    return Ok(());
                }
            };
            let cwd_filter = match request.cwd.as_deref().map(validate_list_cwd).transpose() {
                Ok(cwd) => cwd,
                Err(error) => {
                    output.send(error_response(id, error)?).await?;
                    return Ok(());
                }
            };
            let mut sessions = match session::jsonl::list_sessions_bounded(
                &config.sessions_dir(),
                MAX_SESSION_DIRECTORY_ENTRIES,
                MAX_PERSISTED_SESSIONS,
            ) {
                Ok(sessions) => sessions,
                Err(_) => {
                    output
                        .send(error_response(id, AcpError::internal_error())?)
                        .await?;
                    return Ok(());
                }
            };
            if let Some(cwd) = cwd_filter {
                sessions.retain(|info| {
                    info.cwd
                        .as_deref()
                        .is_some_and(|value| Path::new(value) == cwd)
                });
            }
            let sessions = sessions
                .iter()
                .filter_map(acp_session_info)
                .collect::<Vec<_>>();
            let offset = match parse_list_cursor(request.cursor.as_deref(), sessions.len()) {
                Ok(offset) => offset,
                Err(error) => {
                    output.send(error_response(id, error)?).await?;
                    return Ok(());
                }
            };
            let (page, end) = session_list_page(&sessions, offset);
            let mut response = ListSessionsResponse::new(page);
            if end < sessions.len() {
                response = response.next_cursor(end.to_string());
            }
            output.send(success_response(id, response)?).await?;
        }
        method if method == AGENT_METHOD_NAMES.session_load => {
            let request = match parse_params::<LoadSessionRequest>(params) {
                Ok(request) => request,
                Err(error) => {
                    output.send(error_response(id, error)?).await?;
                    return Ok(());
                }
            };
            if let Err(error) = validate_additional_directories(&request.additional_directories) {
                output.send(error_response(id, error)?).await?;
                return Ok(());
            }
            let requested_mcp_servers = request.mcp_servers;
            let requested_session_id = request.session_id.to_string();
            let session_id = match validate_session_id(&requested_session_id) {
                Ok(session_id) => session_id,
                Err(error) => {
                    output.send(error_response(id, error)?).await?;
                    return Ok(());
                }
            };
            let info = match find_persisted_session(&config, session_id) {
                Ok(info) => info,
                Err(error) => {
                    output.send(error_response(id, error)?).await?;
                    return Ok(());
                }
            };
            let cwd = match validate_persisted_cwd(&info, &request.cwd) {
                Ok(cwd) => cwd,
                Err(error) => {
                    output.send(error_response(id, error)?).await?;
                    return Ok(());
                }
            };
            let session_config = match config.for_cwd(&cwd) {
                Ok(config) => config,
                Err(_) => {
                    output
                        .send(error_response(
                            id,
                            AcpError::invalid_params().data("project configuration is invalid"),
                        )?)
                        .await?;
                    return Ok(());
                }
            };
            let client_mcp_servers =
                match validate_client_mcp_servers(requested_mcp_servers, &session_config) {
                    Ok(servers) => servers,
                    Err(error) => {
                        output.send(error_response(id, error)?).await?;
                        return Ok(());
                    }
                };
            let history = match load_session_history(&info.path) {
                Ok(history) => history,
                Err(error) => {
                    output.send(error_response(id, error)?).await?;
                    return Ok(());
                }
            };
            let entry = match open_persisted_session(
                &state,
                &session_config,
                &info,
                cwd,
                policy.restore,
                client_mcp_servers,
            )
            .await
            {
                Ok(entry) => entry,
                Err(error) => {
                    output.send(error_response(id, error)?).await?;
                    return Ok(());
                }
            };
            if let Err(error) = replay_session_history(&output, session_id, history).await {
                let mut state = state.lock().await;
                if state
                    .sessions
                    .get(session_id)
                    .is_some_and(|current| Arc::ptr_eq(current, &entry))
                {
                    state.sessions.remove(session_id);
                }
                drop(state);
                output.send(error_response(id, error)?).await?;
                return Ok(());
            }
            output
                .send(success_response(id, LoadSessionResponse::new())?)
                .await?;
            send_available_commands(&output, session_id).await?;
        }
        method if method == AGENT_METHOD_NAMES.session_resume => {
            let request = match parse_params::<ResumeSessionRequest>(params) {
                Ok(request) => request,
                Err(error) => {
                    output.send(error_response(id, error)?).await?;
                    return Ok(());
                }
            };
            if let Err(error) = validate_additional_directories(&request.additional_directories) {
                output.send(error_response(id, error)?).await?;
                return Ok(());
            }
            let requested_mcp_servers = request.mcp_servers;
            let requested_session_id = request.session_id.to_string();
            let session_id = match validate_session_id(&requested_session_id) {
                Ok(session_id) => session_id,
                Err(error) => {
                    output.send(error_response(id, error)?).await?;
                    return Ok(());
                }
            };
            let info = match find_persisted_session(&config, session_id) {
                Ok(info) => info,
                Err(error) => {
                    output.send(error_response(id, error)?).await?;
                    return Ok(());
                }
            };
            let cwd = match validate_persisted_cwd(&info, &request.cwd) {
                Ok(cwd) => cwd,
                Err(error) => {
                    output.send(error_response(id, error)?).await?;
                    return Ok(());
                }
            };
            let session_config = match config.for_cwd(&cwd) {
                Ok(config) => config,
                Err(_) => {
                    output
                        .send(error_response(
                            id,
                            AcpError::invalid_params().data("project configuration is invalid"),
                        )?)
                        .await?;
                    return Ok(());
                }
            };
            let client_mcp_servers =
                match validate_client_mcp_servers(requested_mcp_servers, &session_config) {
                    Ok(servers) => servers,
                    Err(error) => {
                        output.send(error_response(id, error)?).await?;
                        return Ok(());
                    }
                };
            if let Err(error) = validate_session_history(&info.path) {
                output.send(error_response(id, error)?).await?;
                return Ok(());
            }
            if let Err(error) = open_persisted_session(
                &state,
                &session_config,
                &info,
                cwd,
                policy.restore,
                client_mcp_servers,
            )
            .await
            {
                output.send(error_response(id, error)?).await?;
                return Ok(());
            }
            output
                .send(success_response(id, ResumeSessionResponse::new())?)
                .await?;
            send_available_commands(&output, session_id).await?;
        }
        method if method == AGENT_METHOD_NAMES.session_close => {
            let request = match parse_params::<CloseSessionRequest>(params) {
                Ok(request) => request,
                Err(error) => {
                    output.send(error_response(id, error)?).await?;
                    return Ok(());
                }
            };
            let requested_session_id = request.session_id.to_string();
            let session_id = match validate_session_id(&requested_session_id) {
                Ok(session_id) => session_id,
                Err(error) => {
                    output.send(error_response(id, error)?).await?;
                    return Ok(());
                }
            };
            let mut server = state.lock().await;
            if server.opening_session_ids.contains(session_id) {
                drop(server);
                output
                    .send(error_response(id, invalid_state("session is opening"))?)
                    .await?;
                return Ok(());
            }
            let Some(entry) = server.sessions.get(session_id) else {
                drop(server);
                output
                    .send(error_response(id, AcpError::resource_not_found(None))?)
                    .await?;
                return Ok(());
            };
            if entry
                .active
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_some()
            {
                drop(server);
                output
                    .send(error_response(
                        id,
                        invalid_state("session prompt is active"),
                    )?)
                    .await?;
                return Ok(());
            }
            let removed = server.sessions.remove(session_id);
            drop(server);
            drop(removed);
            output
                .send(success_response(id, CloseSessionResponse::new())?)
                .await?;
        }
        method if method == AGENT_METHOD_NAMES.session_delete => {
            let request = match parse_params::<DeleteSessionRequest>(params) {
                Ok(request) => request,
                Err(error) => {
                    output.send(error_response(id, error)?).await?;
                    return Ok(());
                }
            };
            let requested_session_id = request.session_id.to_string();
            let session_id = match validate_session_id(&requested_session_id) {
                Ok(session_id) => session_id,
                Err(error) => {
                    output.send(error_response(id, error)?).await?;
                    return Ok(());
                }
            };
            let server = state.lock().await;
            if server.sessions.contains_key(session_id)
                || server.opening_session_ids.contains(session_id)
            {
                drop(server);
                output
                    .send(error_response(
                        id,
                        invalid_state("active sessions must be closed before deletion"),
                    )?)
                    .await?;
                return Ok(());
            }
            let info = match find_persisted_session(&config, session_id) {
                Ok(info) => info,
                Err(error) => {
                    drop(server);
                    output.send(error_response(id, error)?).await?;
                    return Ok(());
                }
            };
            if session::jsonl::delete_session(&info.path).is_err() {
                drop(server);
                output
                    .send(error_response(id, AcpError::internal_error())?)
                    .await?;
                return Ok(());
            }
            drop(server);
            output
                .send(success_response(id, DeleteSessionResponse::new())?)
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
            let cancellation = TurnCancellation::new();
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
                    drop(state);
                    output
                        .send(error_response(
                            id,
                            invalid_state("session prompt already active"),
                        )?)
                        .await?;
                    return Ok(());
                }
                state.active_request_ids.insert(id.clone());
                entry
            };
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
            let task_output = output.clone();
            tasks.spawn(async move {
                let _permit = permit;
                let result = run_prompt(
                    &entry,
                    &session_id,
                    prepared,
                    cancellation,
                    task_output.clone(),
                    Arc::clone(&task_state),
                    policy.permissions,
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
    cancellation: TurnCancellation,
    output: Output,
    state: Arc<AsyncMutex<ServerState>>,
    permission_policy: AcpPermissionPolicy,
) -> std::result::Result<TurnOutcome, AcpError> {
    let mut agent = entry.agent.lock().await;
    if let Some(command) = prompt.command {
        if !prompt.images.is_empty() {
            return Err(AcpError::invalid_params().data("commands do not accept image content"));
        }
        let outcome = agent
            .execute_headless_command(command, &entry.config, &cancellation)
            .await
            .map_err(|_| AcpError::internal_error().data("command execution failed"))?;
        return match outcome {
            HeadlessCommandOutcome::Completed(text) => {
                for chunk in utf8_chunks(&text, MAX_UPDATE_TEXT_BYTES) {
                    send_session_update(
                        &output,
                        session_id,
                        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
                            TextContent::new(chunk),
                        ))),
                    )
                    .await
                    .map_err(|_| AcpError::internal_error())?;
                }
                Ok(TurnOutcome::Completed)
            }
            HeadlessCommandOutcome::Cancelled => Ok(TurnOutcome::Cancelled),
        };
    }
    if !prompt.images.is_empty() {
        agent
            .attach_data_images(prompt.images)
            .map_err(|_| AcpError::invalid_params().data("invalid image prompt content"))?;
    }
    let mut sink = AcpEventSink::new(
        session_id.to_string(),
        output.clone(),
        entry.config.max_context_tokens,
    );
    let mut options = TurnOptions::headless(cancellation.clone());
    if matches!(permission_policy, AcpPermissionPolicy::Ask) {
        options = options.with_permission_handler(Arc::new(AcpPermissionHandler {
            session_id: session_id.to_string(),
            output: output.clone(),
            state,
        }));
    }
    agent
        .run_turn_with_events(prompt.text, &entry.config, options, &mut sink)
        .await
        .map_err(|_| AcpError::internal_error())
}

async fn handle_client_response(
    object: &serde_json::Map<String, Value>,
    state: &Arc<AsyncMutex<ServerState>>,
) {
    let Some(id) = object
        .get("id")
        .cloned()
        .and_then(|id| serde_json::from_value::<RequestId>(id).ok())
    else {
        return;
    };
    let sender = state.lock().await.pending_client_responses.remove(&id);
    if let Some(sender) = sender {
        let _ = sender.send(Value::Object(object.clone()));
    }
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
    let command = parse_headless_command(&text).map_err(|error| {
        AcpError::invalid_params().data(terminal_text::sanitize(&error.to_string()))
    })?;
    Ok(PreparedPrompt {
        text,
        images,
        command,
    })
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

fn validate_list_cwd(path: &Path) -> std::result::Result<PathBuf, AcpError> {
    if !path.is_absolute() {
        return Err(AcpError::invalid_params().data("cwd must be absolute"));
    }
    Ok(path.canonicalize().unwrap_or_else(|_| path.to_path_buf()))
}

fn validate_session_id(session_id: &str) -> std::result::Result<&str, AcpError> {
    if session_id.is_empty()
        || session_id.len() > MAX_SESSION_ID_BYTES
        || session_id.starts_with('.')
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(AcpError::invalid_params().data("invalid sessionId"));
    }
    Ok(session_id)
}

fn validate_additional_directories(
    additional_directories: &[PathBuf],
) -> std::result::Result<(), AcpError> {
    if !additional_directories.is_empty() {
        return Err(AcpError::invalid_params().data("additionalDirectories is not supported"));
    }
    Ok(())
}

fn validate_client_mcp_servers(
    servers: Vec<McpServer>,
    config: &Config,
) -> std::result::Result<Vec<mcp::ClientMcpServer>, AcpError> {
    if servers.len() > MAX_CLIENT_MCP_SERVERS {
        return Err(AcpError::invalid_params().data("too many MCP servers"));
    }
    if !servers.is_empty() && !config.mcp_enabled {
        return Err(AcpError::invalid_params().data("MCP is disabled by Ferrum configuration"));
    }

    let mut names = HashSet::new();
    let mut validated = Vec::with_capacity(servers.len());
    for server in servers {
        let server = match server {
            McpServer::Stdio(server) => server,
            McpServer::Http(_) | McpServer::Sse(_) => {
                return Err(
                    AcpError::invalid_params().data("remote MCP transport is not supported")
                );
            }
            _ => {
                return Err(AcpError::invalid_params().data("unsupported MCP transport"));
            }
        };
        if server.name.is_empty()
            || server.name.len() > MAX_CLIENT_MCP_SERVER_NAME_BYTES
            || server.name.chars().any(char::is_control)
        {
            return Err(AcpError::invalid_params().data("invalid MCP server name"));
        }
        if config
            .project_mcp_allow
            .as_ref()
            .is_some_and(|allow| !allow.iter().any(|name| name == &server.name))
            || config
                .mcp_server_deny
                .iter()
                .any(|name| name == &server.name)
        {
            return Err(AcpError::invalid_params().data("MCP server is denied by project policy"));
        }
        if !names.insert(server.name.clone()) {
            return Err(AcpError::invalid_params().data("duplicate MCP server name"));
        }
        let command_bytes = server.command.as_os_str().as_bytes();
        if !server.command.is_absolute()
            || command_bytes.is_empty()
            || command_bytes.len() > MAX_CLIENT_MCP_COMMAND_BYTES
            || command_bytes.contains(&0)
        {
            return Err(AcpError::invalid_params().data("invalid MCP server command"));
        }
        if server.args.len() > MAX_CLIENT_MCP_ARGS {
            return Err(AcpError::invalid_params().data("too many MCP server arguments"));
        }
        let mut argument_bytes = 0usize;
        for argument in &server.args {
            argument_bytes = argument_bytes.saturating_add(argument.len());
            if argument.len() > MAX_CLIENT_MCP_ARG_BYTES || argument.contains('\0') {
                return Err(AcpError::invalid_params().data("invalid MCP server argument"));
            }
        }
        if argument_bytes > MAX_CLIENT_MCP_ARG_TOTAL_BYTES {
            return Err(AcpError::invalid_params().data("MCP server arguments are too large"));
        }
        if server.env.len() > MAX_CLIENT_MCP_ENV {
            return Err(AcpError::invalid_params().data("too many MCP environment variables"));
        }
        let mut environment_names = HashSet::new();
        let mut environment_bytes = 0usize;
        let mut environment = Vec::with_capacity(server.env.len());
        for variable in server.env {
            if variable.name.is_empty()
                || variable.name.len() > MAX_CLIENT_MCP_ENV_NAME_BYTES
                || !variable
                    .name
                    .bytes()
                    .all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
                || !environment_names.insert(variable.name.clone())
            {
                return Err(
                    AcpError::invalid_params().data("invalid MCP environment variable name")
                );
            }
            environment_bytes = environment_bytes
                .saturating_add(variable.name.len())
                .saturating_add(variable.value.len());
            if variable.value.len() > MAX_CLIENT_MCP_ENV_VALUE_BYTES
                || variable.value.contains('\0')
            {
                return Err(
                    AcpError::invalid_params().data("invalid MCP environment variable value")
                );
            }
            environment.push((variable.name, variable.value));
        }
        if environment_bytes > MAX_CLIENT_MCP_ENV_TOTAL_BYTES {
            return Err(AcpError::invalid_params().data("MCP environment is too large"));
        }
        validated.push(mcp::ClientMcpServer {
            name: server.name,
            command: server.command,
            args: server.args,
            env: environment,
        });
    }
    Ok(validated)
}

fn find_persisted_session(
    config: &Config,
    session_id: &str,
) -> std::result::Result<session::jsonl::SessionInfo, AcpError> {
    validate_session_id(session_id)?;
    session::jsonl::list_sessions_bounded(
        &config.sessions_dir(),
        MAX_SESSION_DIRECTORY_ENTRIES,
        MAX_PERSISTED_SESSIONS,
    )
    .map_err(|_| AcpError::internal_error())?
    .into_iter()
    .find(|info| info.id == session_id)
    .ok_or_else(|| AcpError::resource_not_found(None))
}

fn validate_persisted_cwd(
    info: &session::jsonl::SessionInfo,
    requested_cwd: &Path,
) -> std::result::Result<PathBuf, AcpError> {
    let requested_cwd = validate_cwd(requested_cwd)?;
    let persisted_cwd = info
        .cwd
        .as_deref()
        .map(Path::new)
        .ok_or_else(|| AcpError::invalid_params().data("session has no persisted cwd"))?;
    if !persisted_cwd.is_absolute() || persisted_cwd != requested_cwd {
        return Err(AcpError::invalid_params().data("cwd does not match the persisted session"));
    }
    Ok(requested_cwd)
}

async fn open_persisted_session(
    state: &Arc<AsyncMutex<ServerState>>,
    config: &Config,
    info: &session::jsonl::SessionInfo,
    cwd: PathBuf,
    restore_policy: SessionRestorePolicy,
    client_mcp_servers: Vec<mcp::ClientMcpServer>,
) -> std::result::Result<Arc<SessionEntry>, AcpError> {
    {
        let mut server = state.lock().await;
        if server.sessions.contains_key(&info.id) || server.opening_session_ids.contains(&info.id) {
            return Err(invalid_state("session is already active or opening"));
        }
        if server
            .sessions
            .len()
            .saturating_add(server.opening_session_ids.len())
            >= MAX_SESSIONS
        {
            return Err(invalid_state("session limit reached"));
        }
        server.opening_session_ids.insert(info.id.clone());
    }

    let setup = async {
        let mut session_config = config.clone();
        restore_session_preferences(
            &mut session_config,
            &info.path,
            restore_policy.thinking,
            restore_policy.safety,
            restore_policy.tools,
            restore_policy.provider,
            restore_policy.model,
        )
        .map_err(|_| AcpError::internal_error())?;
        session_config.enforce_project_constraints();
        let mut agent = AgentSession::open_session_at_cwd(&session_config, info.path.clone(), cwd)
            .map_err(|_| AcpError::internal_error())?;
        agent
            .start_client_mcp(&session_config, client_mcp_servers)
            .await
            .map_err(|_| AcpError::internal_error().data("MCP session setup failed"))?;
        Ok::<_, AcpError>(Arc::new(SessionEntry {
            agent: AsyncMutex::new(agent),
            config: session_config,
            active: Mutex::new(None),
        }))
    }
    .await;

    let mut server = state.lock().await;
    server.opening_session_ids.remove(&info.id);
    let entry = setup?;
    if server.sessions.contains_key(&info.id) {
        return Err(invalid_state("session is already active"));
    }
    server.sessions.insert(info.id.clone(), Arc::clone(&entry));
    Ok(entry)
}

fn parse_list_cursor(
    cursor: Option<&str>,
    session_count: usize,
) -> std::result::Result<usize, AcpError> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    if cursor.is_empty() || cursor.len() > 20 || !cursor.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(AcpError::invalid_params().data("invalid session list cursor"));
    }
    let offset = cursor
        .parse::<usize>()
        .map_err(|_| AcpError::invalid_params().data("invalid session list cursor"))?;
    if offset > session_count {
        return Err(AcpError::invalid_params().data("session list cursor is out of range"));
    }
    Ok(offset)
}

fn acp_session_info(info: &session::jsonl::SessionInfo) -> Option<AcpSessionInfo> {
    validate_session_id(&info.id).ok()?;
    let cwd_value = info.cwd.as_deref()?;
    if cwd_value.len() > MAX_SESSION_CWD_BYTES {
        return None;
    }
    let cwd = PathBuf::from(cwd_value);
    if !cwd.is_absolute() {
        return None;
    }
    let title = truncate_to_max_bytes(
        &terminal_text::sanitize_title(&info.title),
        MAX_SESSION_TITLE_BYTES,
    );
    Some(AcpSessionInfo::new(info.id.clone(), cwd).title(title))
}

fn session_list_page(sessions: &[AcpSessionInfo], offset: usize) -> (Vec<AcpSessionInfo>, usize) {
    let mut page = Vec::new();
    let mut bytes = 0usize;
    let mut end = offset;
    for session in sessions.iter().skip(offset).take(MAX_LIST_PAGE) {
        let session_bytes = serde_json::to_vec(session).map_or(MAX_LIST_PAYLOAD_BYTES, |v| v.len());
        if !page.is_empty() && bytes.saturating_add(session_bytes) > MAX_LIST_PAYLOAD_BYTES {
            break;
        }
        bytes = bytes.saturating_add(session_bytes);
        page.push(session.clone());
        end += 1;
    }
    (page, end)
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

    #[tokio::test]
    async fn slow_reader_cannot_grow_output_queue_past_bound() {
        let (sender, mut receiver) = mpsc::channel(1);
        let output = Output { sender };

        output.try_send(json!({"sequence": 1})).unwrap();
        let error = output.try_send(json!({"sequence": 2})).unwrap_err();
        assert!(error.to_string().contains("output queue limit"));

        assert_eq!(receiver.recv().await.unwrap(), json!({"sequence": 1}));
        output.try_send(json!({"sequence": 2})).unwrap();
        assert_eq!(receiver.recv().await.unwrap(), json!({"sequence": 2}));
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
    fn session_list_cursor_and_page_are_bounded() {
        assert_eq!(parse_list_cursor(None, 3).unwrap(), 0);
        assert_eq!(parse_list_cursor(Some("2"), 3).unwrap(), 2);
        assert!(parse_list_cursor(Some("../1"), 3).is_err());
        assert!(parse_list_cursor(Some("4"), 3).is_err());

        let sessions = (0..150)
            .map(|index| AcpSessionInfo::new(format!("session-{index}"), "/tmp"))
            .collect::<Vec<_>>();
        let (first, cursor) = session_list_page(&sessions, 0);
        assert_eq!(first.len(), MAX_LIST_PAGE);
        assert_eq!(cursor, MAX_LIST_PAGE);
        let (second, cursor) = session_list_page(&sessions, cursor);
        assert_eq!(second.len(), 50);
        assert_eq!(cursor, 150);
    }

    #[test]
    fn tool_input_controls_are_removed_and_size_is_bounded() {
        let value = bounded_sanitized_json(json!({"command": "safe\u{1b}[31m"}), 1024).unwrap();
        assert_eq!(value["command"], "safe");
        assert!(bounded_sanitized_json(json!({"value": "x".repeat(1024)}), 16).is_none());
    }
}
