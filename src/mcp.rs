mod transport;

use crate::{
    agent::tools::ToolDefinition,
    cancel::{self, WaitError},
    config::McpServerConfig,
};
use anyhow::{Context, Result};
use futures_util::future::join_all;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    env, io,
    os::unix::process::CommandExt,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use thiserror::Error;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, BufReader},
    process::{Child, ChildStderr, Command},
    sync::Mutex,
    time::timeout,
};

use transport::McpTransport;

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
const MAX_MCP_OUTPUT_CHARS: usize = 20_000;
const MAX_MCP_DESCRIPTION_CHARS: usize = 2_000;
const MAX_MCP_SCHEMA_BYTES: usize = 8_000;
const MAX_MCP_TOOL_PAGES: usize = 100;
const MAX_MCP_TOOLS: usize = 2_000;
const MAX_MCP_CURSOR_CHARS: usize = 1_000;
const MAX_MCP_STDERR_LINE_BYTES: usize = 16 * 1024;
const MCP_BASE_ENVIRONMENT: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "COLORTERM",
    "TMPDIR",
    "XDG_RUNTIME_DIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "DBUS_SESSION_BUS_ADDRESS",
];
const MCP_START_TIMEOUT: Duration = Duration::from_secs(10);
const MCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MCP_CANCEL_WRITE_TIMEOUT: Duration = Duration::from_millis(250);
const MCP_DIAGNOSTIC_MESSAGES: usize = 20;
const MCP_DIAGNOSTIC_CHARS: usize = 4_000;

#[derive(Clone, Default)]
struct DiagnosticRing {
    messages: Arc<Mutex<VecDeque<String>>>,
}

impl DiagnosticRing {
    async fn push(&self, message: impl Into<String>) {
        let mut messages = self.messages.lock().await;
        messages.push_back(truncate_chars(&message.into(), MCP_DIAGNOSTIC_CHARS));
        while messages.len() > MCP_DIAGNOSTIC_MESSAGES {
            messages.pop_front();
        }
    }

    async fn snapshot(&self) -> Vec<String> {
        self.messages.lock().await.iter().cloned().collect()
    }

    async fn context(&self) -> String {
        let messages = self.snapshot().await;
        if messages.is_empty() {
            String::new()
        } else {
            format!("; recent MCP diagnostics: {}", messages.join(" | "))
        }
    }
}

#[derive(Debug, Error)]
#[error("MCP tool returned error: {0}")]
struct McpToolError(String);

pub struct McpManager {
    servers: Vec<McpServer>,
    tools: Vec<ToolDefinition>,
    tool_routes: HashMap<String, (usize, String)>,
}

impl McpManager {
    pub async fn start(configs: &[McpServerConfig]) -> Result<Self> {
        let mut servers = Vec::new();
        let mut tools = Vec::new();
        let mut tool_routes = HashMap::new();

        let started_servers = join_all(configs.iter().filter(|server| server.enabled).map(
            |server_config| async move {
                let mut server =
                    match timeout(MCP_START_TIMEOUT, McpServer::start(server_config)).await {
                        Ok(Ok(server)) => server,
                        Ok(Err(error)) => {
                            eprintln!("[mcp] failed to start `{}`: {error}", server_config.name);
                            return None;
                        }
                        Err(_) => {
                            eprintln!("[mcp] timed out starting `{}`", server_config.name);
                            return None;
                        }
                    };
                let listed_tools = match timeout(MCP_REQUEST_TIMEOUT, server.list_tools()).await {
                    Ok(Ok(tools)) => tools,
                    Ok(Err(error)) => {
                        eprintln!(
                            "[mcp] failed to list tools for `{}`: {error}",
                            server_config.name
                        );
                        return None;
                    }
                    Err(_) => {
                        eprintln!("[mcp] timed out listing tools for `{}`", server_config.name);
                        return None;
                    }
                };
                Some((server_config, server, listed_tools))
            },
        ))
        .await;

        for (server_config, server, listed_tools) in started_servers.into_iter().flatten() {
            let server_index = servers.len();
            for tool in listed_tools {
                let exposed_name = exposed_tool_name(&server_config.name, &tool.name);
                let route = (server_index, tool.name.clone());
                if let Some((_existing_server_index, existing_tool_name)) =
                    tool_routes.insert(exposed_name.clone(), route)
                {
                    anyhow::bail!(
                        "MCP tool name collision after sanitization: {exposed_name} maps to both `{existing_tool_name}` and `{}`",
                        tool.name
                    );
                }
                tools.push(ToolDefinition {
                    name: exposed_name,
                    description: bounded_tool_description(
                        &tool.name,
                        &server_config.name,
                        tool.description.as_deref().unwrap_or_default(),
                    ),
                    input_schema: bounded_input_schema(
                        tool.input_schema
                            .unwrap_or_else(|| json!({"type":"object"})),
                    ),
                });
            }
            servers.push(server);
        }

        Ok(Self {
            servers,
            tools,
            tool_routes,
        })
    }

    pub fn definitions(&self) -> &[ToolDefinition] {
        &self.tools
    }

    pub fn has_tool(&self, name: &str) -> bool {
        self.tool_routes.contains_key(name)
    }

    pub async fn call(
        &mut self,
        exposed_name: &str,
        arguments: &Value,
        cancelled: Option<&Arc<AtomicBool>>,
    ) -> Result<String> {
        let (server_index, tool_name) = self
            .tool_routes
            .get(exposed_name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown MCP tool: {exposed_name}"))?;
        self.servers[server_index]
            .call_tool(&tool_name, arguments, cancelled)
            .await
    }
}

struct McpServer {
    transport: McpTransport,
    child: Child,
    process_group: i32,
    next_id: u64,
    diagnostics: DiagnosticRing,
}

impl McpServer {
    async fn start(config: &McpServerConfig) -> Result<Self> {
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .env_clear();
        for name in mcp_environment_names(&config.env)? {
            if let Some(value) = env::var_os(name) {
                command.env(name, value);
            }
        }
        command.as_std_mut().process_group(0);
        let mut child = command
            .spawn()
            .with_context(|| format!("failed to start MCP server `{}`", config.name))?;
        let process_group = child
            .id()
            .and_then(|pid| i32::try_from(pid).ok())
            .context("MCP server process id does not fit in i32")?;
        let stdin = child
            .stdin
            .take()
            .context("MCP server stdin was not piped")?;
        let stdout = child
            .stdout
            .take()
            .context("MCP server stdout was not piped")?;
        let diagnostics = DiagnosticRing::default();
        if let Some(stderr) = child.stderr.take() {
            collect_stderr(stderr, diagnostics.clone());
        }
        let mut server = Self {
            transport: McpTransport::start(stdin, stdout, diagnostics.clone()),
            child,
            process_group,
            next_id: 1,
            diagnostics,
        };
        if let Err(error) = server.initialize().await {
            let context = server.diagnostics.context().await;
            anyhow::bail!("{error}{context}");
        }
        Ok(server)
    }

    async fn initialize(&mut self) -> Result<()> {
        let value = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {
                        "name": "ferrum",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            )
            .await?;
        let result: McpInitializeResult =
            serde_json::from_value(value).context("failed to parse MCP initialize response")?;
        if result.protocol_version != MCP_PROTOCOL_VERSION {
            anyhow::bail!(
                "MCP server selected unsupported protocol version `{}`; expected `{MCP_PROTOCOL_VERSION}`",
                result.protocol_version
            );
        }
        if !result.capabilities.is_object() {
            anyhow::bail!("MCP initialize capabilities must be an object");
        }
        if !result
            .capabilities
            .get("tools")
            .is_some_and(Value::is_object)
        {
            anyhow::bail!("MCP server did not advertise the tools capability");
        }
        self.notification("notifications/initialized", json!({}))
            .await
    }

    async fn list_tools(&mut self) -> Result<Vec<McpTool>> {
        let mut tools = Vec::new();
        let mut seen_tool_names = HashSet::new();
        let mut cursor = None;
        let mut seen_cursors = HashSet::new();
        for _ in 0..MAX_MCP_TOOL_PAGES {
            let params = cursor
                .as_ref()
                .map_or_else(|| json!({}), |cursor| json!({"cursor": cursor}));
            let value = self.request("tools/list", params).await?;
            let page: McpToolsListResult =
                serde_json::from_value(value).context("failed to parse MCP tools/list response")?;
            for tool in page.tools {
                if seen_tool_names.insert(tool.name.clone()) {
                    if tools.len() >= MAX_MCP_TOOLS {
                        anyhow::bail!("MCP tools/list exceeded {MAX_MCP_TOOLS} tools");
                    }
                    tools.push(tool);
                }
            }
            let Some(next) = page.next_cursor.filter(|next| !next.is_empty()) else {
                return Ok(tools);
            };
            if next.chars().count() > MAX_MCP_CURSOR_CHARS {
                anyhow::bail!("MCP tools/list cursor exceeded {MAX_MCP_CURSOR_CHARS} characters");
            }
            if !seen_cursors.insert(next.clone()) {
                anyhow::bail!("MCP tools/list repeated a pagination cursor");
            }
            cursor = Some(next);
        }
        anyhow::bail!("MCP tools/list exceeded {MAX_MCP_TOOL_PAGES} pages")
    }

    async fn call_tool(
        &mut self,
        name: &str,
        arguments: &Value,
        cancelled: Option<&Arc<AtomicBool>>,
    ) -> Result<String> {
        if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            anyhow::bail!("aborted");
        }
        let id = self.allocate_id()?;
        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": arguments,
            },
        });
        let queued = AtomicBool::new(false);
        let result = cancel::race_timeout(
            self.transport.request(id, "tools/call", &message, &queued),
            cancelled,
            MCP_REQUEST_TIMEOUT,
        )
        .await;
        let value = match result {
            Ok(result) => result?,
            Err(WaitError::Cancelled) => {
                self.transport.abandon(id).await;
                if queued.load(Ordering::Acquire) {
                    self.cancel_request(id, "User aborted MCP tool call").await;
                    anyhow::bail!(
                        "aborted after MCP tool request dispatch; side-effect outcome is indeterminate"
                    );
                }
                anyhow::bail!("aborted");
            }
            Err(WaitError::TimedOut) => {
                self.transport.abandon(id).await;
                if queued.load(Ordering::Acquire) {
                    self.cancel_request(id, "MCP tool call timed out").await;
                    anyhow::bail!(
                        "MCP tool `{name}` timed out after dispatch; side-effect outcome is indeterminate"
                    );
                }
                anyhow::bail!("MCP tool `{name}` timed out before dispatch");
            }
        };
        mcp_tool_result_to_output(&value)
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.allocate_id()?;
        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let queued = AtomicBool::new(false);
        self.transport.request(id, method, &message, &queued).await
    }

    fn allocate_id(&mut self) -> Result<u64> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .context("MCP request id exhausted")?;
        Ok(id)
    }

    async fn cancel_request(&self, id: u64, reason: &str) {
        let notification = self.notification(
            "notifications/cancelled",
            json!({
                "requestId": id,
                "reason": reason,
            }),
        );
        match timeout(MCP_CANCEL_WRITE_TIMEOUT, notification).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {
                self.diagnostics
                    .push(format!("failed to queue cancellation for MCP request {id}"))
                    .await;
            }
            Err(_) => {
                self.diagnostics
                    .push(format!(
                        "timed out queueing cancellation for MCP request {id}"
                    ))
                    .await;
            }
        }
    }

    async fn notification(&self, method: &str, params: Value) -> Result<()> {
        self.transport
            .notification(&json!({
                "jsonrpc": "2.0",
                "method": method,
                "params": params,
            }))
            .await
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-self.process_group, libc::SIGKILL);
        }
        let _ = self.child.start_kill();
    }
}

fn mcp_environment_names(extra: &[String]) -> Result<Vec<&str>> {
    let mut seen = HashSet::new();
    MCP_BASE_ENVIRONMENT
        .iter()
        .copied()
        .chain(extra.iter().map(String::as_str))
        .filter(|name| seen.insert(*name))
        .map(|name| {
            validate_environment_name(name)?;
            Ok(name)
        })
        .collect()
}

fn validate_environment_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.contains('=')
        || name.contains('\0')
        || !name
            .chars()
            .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
    {
        anyhow::bail!("invalid MCP environment variable name `{name}`");
    }
    Ok(())
}

fn collect_stderr(stderr: ChildStderr, diagnostics: DiagnosticRing) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        loop {
            match read_bounded_diagnostic_line(&mut reader, MAX_MCP_STDERR_LINE_BYTES).await {
                Ok(Some(line)) => {
                    if !line.iter().all(u8::is_ascii_whitespace) {
                        diagnostics
                            .push(format!(
                                "MCP server emitted {} bytes on stderr; content withheld",
                                line.len()
                            ))
                            .await;
                    }
                }
                Ok(None) => break,
                Err(_) => {
                    diagnostics
                        .push(format!(
                            "MCP server stderr line exceeded {MAX_MCP_STDERR_LINE_BYTES} bytes; content withheld"
                        ))
                        .await;
                    break;
                }
            }
        }
    });
}

async fn read_bounded_diagnostic_line<R>(
    reader: &mut R,
    limit: usize,
) -> io::Result<Option<Vec<u8>>>
where
    R: AsyncBufRead + Unpin,
{
    let mut output = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if output.is_empty() {
                Ok(None)
            } else {
                Ok(Some(output))
            };
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if output.len().saturating_add(take) > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "diagnostic line exceeds limit",
            ));
        }
        output.extend_from_slice(&available[..take]);
        reader.consume(take);
        if output.last() == Some(&b'\n') {
            return Ok(Some(output));
        }
    }
}

#[derive(Debug, Deserialize)]
struct McpInitializeResult {
    #[serde(rename = "protocolVersion")]
    protocol_version: String,
    capabilities: Value,
}

#[derive(Debug, Deserialize)]
struct McpToolsListResult {
    tools: Vec<McpTool>,
    #[serde(rename = "nextCursor")]
    next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct McpTool {
    name: String,
    description: Option<String>,
    #[serde(rename = "inputSchema")]
    input_schema: Option<Value>,
}

fn exposed_tool_name(server_name: &str, tool_name: &str) -> String {
    format!(
        "mcp__{}__{}",
        sanitize_name(server_name),
        sanitize_name(tool_name)
    )
}

fn bounded_tool_description(tool_name: &str, server_name: &str, description: &str) -> String {
    format!(
        "MCP tool `{}` from server `{}`. {}",
        tool_name,
        server_name,
        truncate_chars(description, MAX_MCP_DESCRIPTION_CHARS)
    )
}

fn bounded_input_schema(schema: Value) -> Value {
    if bounded_json_bytes(&schema, MAX_MCP_SCHEMA_BYTES).is_some() {
        return schema;
    }
    json!({
        "type": "object",
        "description": "MCP input schema omitted because it exceeded Ferrum's metadata size limit"
    })
}

fn bounded_json_bytes(value: &Value, max_bytes: usize) -> Option<usize> {
    bounded_json_bytes_inner(value, max_bytes).filter(|count| *count <= max_bytes)
}

fn bounded_json_bytes_inner(value: &Value, remaining: usize) -> Option<usize> {
    let used = match value {
        Value::Null => 4,
        Value::Bool(true) => 4,
        Value::Bool(false) => 5,
        Value::Number(number) => number.to_string().len(),
        Value::String(text) => bounded_json_string_bytes(text, remaining)?,
        Value::Array(items) => {
            let mut used = 2usize;
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    used = used.checked_add(1)?;
                }
                used = used.checked_add(bounded_json_bytes_inner(
                    item,
                    remaining.checked_sub(used)?,
                )?)?;
                if used > remaining {
                    return None;
                }
            }
            used
        }
        Value::Object(map) => {
            let mut used = 2usize;
            for (index, (key, item)) in map.iter().enumerate() {
                if index > 0 {
                    used = used.checked_add(1)?;
                }
                used = used.checked_add(bounded_json_string_bytes(
                    key,
                    remaining.checked_sub(used)?,
                )?)?;
                used = used.checked_add(1)?;
                used = used.checked_add(bounded_json_bytes_inner(
                    item,
                    remaining.checked_sub(used)?,
                )?)?;
                if used > remaining {
                    return None;
                }
            }
            used
        }
    };
    (used <= remaining).then_some(used)
}

fn bounded_json_string_bytes(text: &str, remaining: usize) -> Option<usize> {
    let mut used = 2usize;
    for ch in text.chars() {
        let encoded = match ch {
            '"' | '\\' | '\u{0008}' | '\u{0009}' | '\u{000a}' | '\u{000c}' | '\u{000d}' => 2,
            '\u{0000}'..='\u{001f}' => 6,
            _ => ch.len_utf8(),
        };
        used = used.checked_add(encoded)?;
        if used > remaining {
            return None;
        }
    }
    Some(used)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated} [truncated]")
    } else {
        truncated
    }
}

fn mcp_tool_result_to_output(value: &Value) -> Result<String> {
    let output = truncate_output(render_tool_result(value));
    if value
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(McpToolError(output).into());
    }
    Ok(output)
}

fn render_tool_result(value: &Value) -> String {
    let Some(content) = value.get("content").and_then(Value::as_array) else {
        return value.to_string();
    };
    let mut output = Vec::new();
    for item in content {
        match item.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    output.push(text.to_string());
                } else {
                    output.push("[invalid MCP text content]".to_string());
                }
            }
            Some(kind @ ("image" | "audio" | "resource" | "resource_link")) => {
                output.push(format!("[unsupported MCP content: {kind}]"));
            }
            Some(kind) => output.push(format!(
                "[unsupported MCP content: {}]",
                truncate_chars(kind, 100)
            )),
            None => output.push("[invalid MCP content item without type]".to_string()),
        }
    }
    if output.is_empty() {
        "[empty MCP content]".to_string()
    } else {
        output.join("\n")
    }
}

fn truncate_output(mut output: String) -> String {
    if output.chars().count() <= MAX_MCP_OUTPUT_CHARS {
        return output;
    }
    let tail = output
        .chars()
        .rev()
        .take(MAX_MCP_OUTPUT_CHARS)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    output.clear();
    output.push_str("[truncated MCP output]\n");
    output.push_str(&tail);
    output
}

fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use tokio::io::{AsyncWriteExt, duplex};

    #[test]
    fn huge_mcp_schema_does_not_require_serialized_schema() {
        let schema = json!({
            "type": "object",
            "properties": {
                "big": {"type": "string", "description": "x".repeat(MAX_MCP_SCHEMA_BYTES * 4)}
            }
        });
        let bounded_schema = bounded_input_schema(schema);
        assert_eq!(bounded_schema["type"], "object");
        assert!(
            bounded_schema["description"]
                .as_str()
                .unwrap()
                .contains("omitted")
        );
    }

    #[test]
    fn schema_budget_counts_serialized_escaping_bytes() {
        let value = json!({"quoted\"key": "\\\n\u{0000}é"});
        assert_eq!(
            bounded_json_bytes(&value, usize::MAX).unwrap(),
            serde_json::to_vec(&value).unwrap().len()
        );
        let escaped = json!({"description": "\"".repeat(MAX_MCP_SCHEMA_BYTES)});
        assert!(
            bounded_input_schema(escaped)["description"]
                .as_str()
                .unwrap()
                .contains("omitted")
        );
    }

    #[test]
    fn bounds_mcp_metadata() {
        let description = "x".repeat(MAX_MCP_DESCRIPTION_CHARS + 10);
        let bounded = bounded_tool_description("tool", "server", &description);
        assert!(bounded.contains("[truncated]"));
        assert!(bounded.chars().count() <= MAX_MCP_DESCRIPTION_CHARS + 100);

        let schema = json!({"type":"object","description":"x".repeat(MAX_MCP_SCHEMA_BYTES + 10)});
        let bounded_schema = bounded_input_schema(schema);
        assert_eq!(bounded_schema["type"], "object");
        assert!(
            bounded_schema["description"]
                .as_str()
                .unwrap()
                .contains("omitted")
        );
    }

    #[test]
    fn mcp_environment_excludes_provider_credentials_by_default() {
        let names = mcp_environment_names(&[]).unwrap();
        assert!(names.contains(&"PATH"));
        assert!(!names.contains(&"OPENAI_API_KEY"));
        assert!(!names.contains(&"ANTHROPIC_API_KEY"));
        assert!(!names.contains(&"SSH_AUTH_SOCK"));

        let extra = vec!["WEB_SEARCH_API_KEY".to_string()];
        assert!(
            mcp_environment_names(&extra)
                .unwrap()
                .contains(&"WEB_SEARCH_API_KEY")
        );
        assert!(mcp_environment_names(&["BAD=NAME".to_string()]).is_err());
    }

    #[test]
    fn mcp_tool_error_result_becomes_error() {
        let value = json!({
            "isError": true,
            "content": [{"type": "text", "text": "tool failed"}]
        });
        let error = mcp_tool_result_to_output(&value).unwrap_err();
        assert!(error.to_string().contains("tool failed"));
    }

    #[test]
    fn mcp_tool_success_result_stays_ok() {
        let value = json!({
            "content": [{"type": "text", "text": "tool ok"}]
        });
        assert_eq!(mcp_tool_result_to_output(&value).unwrap(), "tool ok");
    }

    #[test]
    fn mixed_mcp_content_reports_unsupported_items() {
        let value = json!({
            "content": [
                {"type": "text", "text": "visible"},
                {"type": "image", "mimeType": "image/png", "data": "secret-base64"},
                {"type": "resource", "resource": {"uri": "file:///secret"}}
            ]
        });
        let output = mcp_tool_result_to_output(&value).unwrap();
        assert!(output.contains("visible"));
        assert!(output.contains("[unsupported MCP content: image]"));
        assert!(output.contains("[unsupported MCP content: resource]"));
        assert!(!output.contains("secret-base64"));
        assert!(!output.contains("file:///secret"));
    }

    #[tokio::test]
    async fn cancelled_tool_call_sends_standard_mcp_notification() {
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("cancel_server.py");
        let marker = temp.path().join("cancelled.json");
        let request_marker = temp.path().join("requested");
        std::fs::write(
            &script,
            r#"#!/usr/bin/env python3
import json
import pathlib
import sys

marker = pathlib.Path(sys.argv[1])
request_marker = pathlib.Path(sys.argv[2])

def send(obj):
    sys.stdout.write(json.dumps(obj, separators=(",", ":")) + "\n")
    sys.stdout.flush()

for line in sys.stdin:
    msg = json.loads(line)
    method = msg.get("method")
    if method == "initialize":
        send({"jsonrpc":"2.0","id":msg["id"],"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"cancel-test","version":"1"}}})
    elif method == "notifications/initialized":
        continue
    elif method == "tools/list":
        send({"jsonrpc":"2.0","id":msg["id"],"result":{"tools":[{"name":"slow","description":"wait for cancellation","inputSchema":{"type":"object"}}]}})
    elif method == "tools/call":
        request_marker.write_text("called")
        continue
    elif method == "notifications/cancelled":
        marker.write_text(json.dumps(msg["params"]))
"#,
        )
        .unwrap();
        let config = McpServerConfig {
            name: "cancel".to_string(),
            command: "python3".to_string(),
            args: vec![
                script.display().to_string(),
                marker.display().to_string(),
                request_marker.display().to_string(),
            ],
            env: Vec::new(),
            enabled: true,
        };
        let mut manager = McpManager::start(&[config]).await.unwrap();
        let exposed_name = manager.definitions()[0].name.clone();
        let pre_cancelled = Arc::new(AtomicBool::new(true));
        let error = manager
            .call(&exposed_name, &json!({}), Some(&pre_cancelled))
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "aborted");
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(!request_marker.exists());

        let cancel = Arc::new(AtomicBool::new(false));
        let trigger = Arc::clone(&cancel);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            trigger.store(true, Ordering::Relaxed);
        });

        let error = manager
            .call(&exposed_name, &json!({}), Some(&cancel))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("outcome is indeterminate"));

        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        while !marker.exists() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let notification: Value =
            serde_json::from_str(&std::fs::read_to_string(marker).unwrap()).unwrap();
        assert!(
            notification
                .get("requestId")
                .and_then(Value::as_u64)
                .is_some()
        );
        assert_eq!(
            notification.get("reason").and_then(Value::as_str),
            Some("User aborted MCP tool call")
        );
    }

    #[tokio::test]
    async fn cancellation_mid_frame_does_not_corrupt_next_call() {
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("partial_frame_server.py");
        std::fs::write(
            &script,
            r#"#!/usr/bin/env python3
import json
import sys
import time

calls = 0

def send(obj):
    data = json.dumps(obj, separators=(",", ":")).encode()
    sys.stdout.buffer.write(f"Content-Length: {len(data)}\r\n\r\n".encode() + data)
    sys.stdout.buffer.flush()

def send_partial(obj):
    data = json.dumps(obj, separators=(",", ":")).encode()
    split = len(data) // 2
    sys.stdout.buffer.write(f"Content-Length: {len(data)}\r\n\r\n".encode() + data[:split])
    sys.stdout.buffer.flush()
    time.sleep(0.15)
    sys.stdout.buffer.write(data[split:])
    sys.stdout.buffer.flush()

for line in sys.stdin:
    msg = json.loads(line)
    method = msg.get("method")
    if method == "initialize":
        send({"jsonrpc":"2.0","id":msg["id"],"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"partial-test","version":"1"}}})
    elif method == "notifications/initialized":
        continue
    elif method == "tools/list":
        send({"jsonrpc":"2.0","id":msg["id"],"result":{"tools":[{"name":"partial","description":"partial frame test","inputSchema":{"type":"object"}}]}})
    elif method == "notifications/cancelled":
        continue
    elif method == "tools/call":
        calls += 1
        response = {"jsonrpc":"2.0","id":msg["id"],"result":{"content":[{"type":"text","text":f"call {calls}"}]}}
        if calls == 1:
            send_partial(response)
        else:
            send(response)
"#,
        )
        .unwrap();
        let config = McpServerConfig {
            name: "partial".to_string(),
            command: "python3".to_string(),
            args: vec![script.display().to_string()],
            env: Vec::new(),
            enabled: true,
        };
        let mut manager = McpManager::start(&[config]).await.unwrap();
        let exposed_name = manager.definitions()[0].name.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let trigger = Arc::clone(&cancel);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            trigger.store(true, Ordering::Release);
        });

        let error = manager
            .call(&exposed_name, &json!({}), Some(&cancel))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("outcome is indeterminate"));

        let output = manager.call(&exposed_name, &json!({}), None).await.unwrap();
        assert_eq!(output, "call 2");
    }

    #[tokio::test]
    async fn follows_bounded_tools_list_pagination() {
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("pagination_server.py");
        std::fs::write(
            &script,
            r#"#!/usr/bin/env python3
import json
import sys

def send(obj):
    sys.stdout.write(json.dumps(obj, separators=(",", ":")) + "\n")
    sys.stdout.flush()

for line in sys.stdin:
    msg = json.loads(line)
    method = msg.get("method")
    if method == "initialize":
        send({"jsonrpc":"2.0","id":msg["id"],"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"page-test","version":"1"}}})
    elif method == "notifications/initialized":
        continue
    elif method == "tools/list":
        cursor = msg.get("params", {}).get("cursor")
        if cursor is None:
            send({"jsonrpc":"2.0","id":msg["id"],"result":{"tools":[{"name":"first","inputSchema":{"type":"object"}}],"nextCursor":"page-2"}})
        else:
            send({"jsonrpc":"2.0","id":msg["id"],"result":{"tools":[{"name":"first","inputSchema":{"type":"object"}},{"name":"second","inputSchema":{"type":"object"}}]}})
"#,
        )
        .unwrap();
        let config = McpServerConfig {
            name: "pages".to_string(),
            command: "python3".to_string(),
            args: vec![script.display().to_string()],
            env: Vec::new(),
            enabled: true,
        };

        let manager = McpManager::start(&[config]).await.unwrap();
        let names = manager
            .definitions()
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["mcp__pages__first", "mcp__pages__second"]);
    }

    #[tokio::test]
    async fn notification_before_tools_list_response_is_recorded() {
        let script = std::env::temp_dir().join(format!(
            "ferrum_mcp_notify_server_{}.py",
            std::process::id()
        ));
        std::fs::write(
            &script,
            r#"#!/usr/bin/env python3
import json
import sys

def send(obj):
    data = json.dumps(obj, separators=(",", ":"))
    sys.stdout.write(f"Content-Length: {len(data)}\r\n\r\n{data}")
    sys.stdout.flush()

def read_message():
    header = sys.stdin.readline()
    if not header:
        return None
    return json.loads(header)

while True:
    msg = read_message()
    if msg is None:
        break
    method = msg.get("method")
    if method == "initialize":
        send({"jsonrpc":"2.0","id":msg["id"],"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"test","version":"1"}}})
    elif method == "notifications/initialized":
        continue
    elif method == "tools/list":
        send({"jsonrpc":"2.0","method":"notifications/message","params":{"level":"info","data":"ready"}})
        send({"jsonrpc":"2.0","id":msg["id"],"result":{"tools":[]}})
    else:
        send({"jsonrpc":"2.0","id":msg.get("id"),"error":{"code":-32601,"message":"unknown method"}})
"#,
        )
        .unwrap();
        let config = McpServerConfig {
            name: "notify".to_string(),
            command: "python3".to_string(),
            args: vec![script.display().to_string()],
            env: Vec::new(),
            enabled: true,
        };

        let manager = McpManager::start(&[config]).await.unwrap();
        let diagnostics = manager.servers[0].diagnostics.snapshot().await;

        assert!(
            diagnostics
                .iter()
                .any(|message| message == "received MCP notification")
        );
    }

    #[tokio::test]
    async fn rejects_unsupported_initialize_negotiation() {
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("bad_initialize_server.py");
        std::fs::write(
            &script,
            r#"#!/usr/bin/env python3
import json
import sys

for line in sys.stdin:
    msg = json.loads(line)
    if msg.get("method") == "initialize":
        response = {"jsonrpc":"2.0","id":msg["id"],"result":{"protocolVersion":"1900-01-01","capabilities":{"tools":{}},"serverInfo":{"name":"bad","version":"1"}}}
        sys.stdout.write(json.dumps(response, separators=(",", ":")) + "\n")
        sys.stdout.flush()
"#,
        )
        .unwrap();
        let config = McpServerConfig {
            name: "bad-init".to_string(),
            command: "python3".to_string(),
            args: vec![script.display().to_string()],
            env: Vec::new(),
            enabled: true,
        };
        let error = match McpServer::start(&config).await {
            Ok(_) => panic!("expected unsupported protocol version to fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("unsupported protocol version"));
    }

    #[tokio::test]
    async fn rejects_repeated_tools_list_cursor() {
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("cursor_loop_server.py");
        std::fs::write(
            &script,
            r#"#!/usr/bin/env python3
import json
import sys

def send(obj):
    sys.stdout.write(json.dumps(obj, separators=(",", ":")) + "\n")
    sys.stdout.flush()

for line in sys.stdin:
    msg = json.loads(line)
    if msg.get("method") == "initialize":
        send({"jsonrpc":"2.0","id":msg["id"],"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"loop","version":"1"}}})
    elif msg.get("method") == "notifications/initialized":
        continue
    elif msg.get("method") == "tools/list":
        send({"jsonrpc":"2.0","id":msg["id"],"result":{"tools":[],"nextCursor":"same"}})
"#,
        )
        .unwrap();
        let config = McpServerConfig {
            name: "loop".to_string(),
            command: "python3".to_string(),
            args: vec![script.display().to_string()],
            env: Vec::new(),
            enabled: true,
        };
        let mut server = McpServer::start(&config).await.unwrap();
        let error = server.list_tools().await.unwrap_err();
        assert!(error.to_string().contains("repeated a pagination cursor"));
    }

    #[tokio::test]
    async fn rejects_unbounded_stderr_line() {
        let (mut writer, reader) = duplex(MAX_MCP_STDERR_LINE_BYTES * 2);
        writer
            .write_all(&vec![b'x'; MAX_MCP_STDERR_LINE_BYTES + 1])
            .await
            .unwrap();
        writer.shutdown().await.unwrap();
        let error =
            read_bounded_diagnostic_line(&mut BufReader::new(reader), MAX_MCP_STDERR_LINE_BYTES)
                .await
                .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn failing_mcp_server_stderr_is_redacted_from_error() {
        let script = std::env::temp_dir().join(format!(
            "ferrum_mcp_stderr_server_{}.py",
            std::process::id()
        ));
        std::fs::write(
            &script,
            r#"#!/usr/bin/env python3
import sys
sys.stderr.write("startup exploded\n")
sys.stderr.flush()
"#,
        )
        .unwrap();
        let config = McpServerConfig {
            name: "stderr".to_string(),
            command: "python3".to_string(),
            args: vec![script.display().to_string()],
            env: Vec::new(),
            enabled: true,
        };

        let error = match McpServer::start(&config).await {
            Ok(_) => panic!("expected MCP startup to fail"),
            Err(error) => error,
        };

        let rendered = error.to_string();
        assert!(!rendered.contains("startup exploded"));
        assert!(rendered.contains("MCP"));
    }

    #[tokio::test]
    async fn manager_start_rejects_sanitized_tool_name_collisions() {
        let script = std::env::temp_dir().join("ferrum_mcp_collision_server.py");
        std::fs::write(
            &script,
            r#"#!/usr/bin/env python3
import json
import sys

TOOLS = sys.argv[1:]

def send(obj):
    data = json.dumps(obj, separators=(",", ":"))
    sys.stdout.write(f"Content-Length: {len(data)}\r\n\r\n{data}")
    sys.stdout.flush()

def read_message():
    header = sys.stdin.readline()
    if not header:
        return None
    if header.startswith("Content-Length:"):
        length = int(header.split(":", 1)[1].strip())
        while True:
            line = sys.stdin.readline()
            if line in ("\n", "\r\n", ""):
                break
        return json.loads(sys.stdin.read(length))
    return json.loads(header)

while True:
    msg = read_message()
    if msg is None:
        break
    method = msg.get("method")
    if method == "initialize":
        send({"jsonrpc":"2.0","id":msg["id"],"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"test","version":"1"}}})
    elif method == "notifications/initialized":
        continue
    elif method == "tools/list":
        send({"jsonrpc":"2.0","id":msg["id"],"result":{"tools":[{"name":name,"description":"test tool","inputSchema":{"type":"object"}} for name in TOOLS]}})
    else:
        send({"jsonrpc":"2.0","id":msg.get("id"),"error":{"code":-32601,"message":"unknown method"}})
"#,
        )
        .unwrap();
        let config = McpServerConfig {
            name: "server-a".to_string(),
            command: "python3".to_string(),
            args: vec![
                script.display().to_string(),
                "foo/bar".to_string(),
                "foo_bar".to_string(),
            ],
            env: Vec::new(),
            enabled: true,
        };
        let error = match McpManager::start(&[config]).await {
            Ok(_) => panic!("expected MCP collision to fail"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("MCP tool name collision after sanitization")
        );
    }
}
