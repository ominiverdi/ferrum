use crate::{agent::tools::ToolDefinition, config::McpServerConfig};
use anyhow::{Context, Result};
use futures_util::future::join_all;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    io,
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
    sync::Mutex,
    time::timeout,
};

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
const MAX_MCP_OUTPUT_CHARS: usize = 20_000;
const MAX_MCP_FRAME_BYTES: usize = 10 * 1024 * 1024;
const MAX_MCP_DESCRIPTION_CHARS: usize = 2_000;
const MAX_MCP_SCHEMA_CHARS: usize = 8_000;
const MCP_START_TIMEOUT: Duration = Duration::from_secs(10);
const MCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
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

    pub async fn call(&mut self, exposed_name: &str, arguments: &Value) -> Result<String> {
        let (server_index, tool_name) = self
            .tool_routes
            .get(exposed_name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown MCP tool: {exposed_name}"))?;
        self.servers[server_index]
            .call_tool(&tool_name, arguments)
            .await
    }
}

struct McpServer {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    _child: Child,
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
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .with_context(|| format!("failed to start MCP server `{}`", config.name))?;

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
            stdin,
            stdout: BufReader::new(stdout),
            _child: child,
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
        self.request(
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
        self.notification("notifications/initialized", json!({}))
            .await
    }

    async fn list_tools(&mut self) -> Result<Vec<McpTool>> {
        let value = self.request("tools/list", json!({})).await?;
        serde_json::from_value::<McpToolsListResult>(value)
            .map(|result| result.tools)
            .context("failed to parse MCP tools/list response")
    }

    async fn call_tool(&mut self, name: &str, arguments: &Value) -> Result<String> {
        let value = timeout(
            MCP_REQUEST_TIMEOUT,
            self.request(
                "tools/call",
                json!({
                    "name": name,
                    "arguments": arguments,
                }),
            ),
        )
        .await
        .with_context(|| format!("MCP tool `{name}` timed out"))??;
        mcp_tool_result_to_output(&value)
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .await?;

        loop {
            let message = self.read_message().await?;
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                self.diagnostics
                    .push(format!(
                        "out-of-band JSON-RPC message while awaiting {method}: {message}"
                    ))
                    .await;
                continue;
            }
            if let Some(error) = message.get("error") {
                let context = self.diagnostics.context().await;
                anyhow::bail!("MCP {method} failed: {error}{context}");
            }
            return Ok(message.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    async fn notification(&mut self, method: &str, params: Value) -> Result<()> {
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
        .await
    }

    async fn write_message(&mut self, message: &Value) -> Result<()> {
        let frame = encode_message_line(message)?;
        if let Err(error) = self.stdin.write_all(&frame).await {
            return Err(self.write_error_context(error).await);
        }
        if let Err(error) = self.stdin.flush().await {
            return Err(self.write_error_context(error).await);
        }
        Ok(())
    }

    async fn write_error_context(&mut self, error: io::Error) -> anyhow::Error {
        match self._child.try_wait() {
            Ok(Some(status)) => {
                let context = self.diagnostics.context().await;
                anyhow::anyhow!(
                    "MCP server exited before request could be written: {status}: {error}{context}"
                )
            }
            Ok(None) => {
                let context = self.diagnostics.context().await;
                anyhow::anyhow!("failed to write MCP request to server stdin: {error}{context}")
            }
            Err(wait_error) => {
                let context = self.diagnostics.context().await;
                anyhow::anyhow!(
                    "failed to write MCP request to server stdin: {error}; failed to inspect MCP server status: {wait_error}{context}"
                )
            }
        }
    }

    async fn read_message(&mut self) -> Result<Value> {
        let mut line = String::new();
        let bytes = self.stdout.read_line(&mut line).await?;
        if bytes == 0 {
            let context = self.diagnostics.context().await;
            anyhow::bail!("MCP server closed stdout{context}");
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if !looks_like_mcp_header(trimmed) {
            return serde_json::from_str(trimmed).context("failed to parse MCP JSON-RPC line");
        }

        let mut headers = vec![trimmed.to_string()];
        loop {
            line.clear();
            let bytes = self.stdout.read_line(&mut line).await?;
            if bytes == 0 {
                let context = self.diagnostics.context().await;
                anyhow::bail!("MCP server closed stdout before body{context}");
            }
            let header = line.trim_end_matches(['\r', '\n']);
            if header.is_empty() {
                break;
            }
            headers.push(header.to_string());
        }
        let len = parse_content_length_headers(&headers)?;
        let mut body = vec![0; len];
        self.stdout.read_exact(&mut body).await?;
        serde_json::from_slice(&body).context("failed to parse MCP JSON-RPC message")
    }
}

fn collect_stderr(stderr: ChildStderr, diagnostics: DiagnosticRing) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    let text = line.trim_end_matches(['\r', '\n']).to_string();
                    if !text.is_empty() {
                        diagnostics.push(format!("stderr: {text}")).await;
                    }
                }
                Err(error) => {
                    diagnostics
                        .push(format!("stderr read failed: {error}"))
                        .await;
                    break;
                }
            }
        }
    });
}

#[derive(Debug, Deserialize)]
struct McpToolsListResult {
    tools: Vec<McpTool>,
}

#[derive(Debug, Deserialize)]
struct McpTool {
    name: String,
    description: Option<String>,
    #[serde(rename = "inputSchema")]
    input_schema: Option<Value>,
}

fn encode_message_line(message: &Value) -> Result<Vec<u8>> {
    let mut body = serde_json::to_vec(message)?;
    body.push(b'\n');
    Ok(body)
}

#[cfg(test)]
fn parse_content_length(header: &str) -> Result<usize> {
    parse_content_length_headers(&[header.to_string()])
}

fn parse_content_length_headers(headers: &[String]) -> Result<usize> {
    let mut length = None;
    for header in headers {
        let Some((name, value)) = header.split_once(':') else {
            anyhow::bail!("invalid MCP header: {header}");
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            length = Some(value.trim().parse::<usize>()?);
        }
    }
    let len = length.context("MCP framed message missing Content-Length")?;
    if len > MAX_MCP_FRAME_BYTES {
        anyhow::bail!("MCP message Content-Length {len} exceeds limit {MAX_MCP_FRAME_BYTES}");
    }
    Ok(len)
}

fn looks_like_mcp_header(line: &str) -> bool {
    line.split_once(':')
        .is_some_and(|(name, _)| !name.trim().is_empty() && !name.trim_start().starts_with('{'))
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
    if bounded_json_chars(&schema, MAX_MCP_SCHEMA_CHARS).is_some() {
        return schema;
    }
    json!({
        "type": "object",
        "description": "MCP input schema omitted because it exceeded Ferrum's metadata size limit"
    })
}

fn bounded_json_chars(value: &Value, max_chars: usize) -> Option<usize> {
    bounded_json_chars_inner(value, max_chars).filter(|count| *count <= max_chars)
}

fn bounded_json_chars_inner(value: &Value, remaining: usize) -> Option<usize> {
    let used = match value {
        Value::Null => 4,
        Value::Bool(true) => 4,
        Value::Bool(false) => 5,
        Value::Number(number) => number.to_string().chars().count(),
        Value::String(text) => text.chars().count().saturating_add(2),
        Value::Array(items) => {
            let mut used = 2usize;
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    used = used.checked_add(1)?;
                }
                used = used.checked_add(bounded_json_chars_inner(
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
                used = used.checked_add(key.chars().count().saturating_add(3))?;
                used = used.checked_add(bounded_json_chars_inner(
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
    if used > remaining {
        return None;
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
    if let Some(content) = value.get("content").and_then(Value::as_array) {
        let mut text = String::new();
        for item in content {
            if item.get("type").and_then(Value::as_str) == Some("text")
                && let Some(part) = item.get("text").and_then(Value::as_str)
            {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(part);
            }
        }
        if !text.is_empty() {
            return text;
        }
    }
    value.to_string()
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

    #[test]
    fn accepts_lowercase_content_length() {
        assert_eq!(parse_content_length("content-length: 2").unwrap(), 2);
    }

    #[test]
    fn accepts_extra_headers_around_content_length() {
        let headers = vec![
            "X-Test: before".to_string(),
            "Content-Length: 3".to_string(),
            "Another: after".to_string(),
        ];
        assert_eq!(parse_content_length_headers(&headers).unwrap(), 3);
    }

    #[test]
    fn rejects_missing_content_length_in_framed_message() {
        let headers = vec!["X-Test: nope".to_string()];
        let error = parse_content_length_headers(&headers).unwrap_err();
        assert!(error.to_string().contains("missing Content-Length"));
    }

    #[test]
    fn huge_mcp_schema_does_not_require_serialized_schema() {
        let schema = json!({
            "type": "object",
            "properties": {
                "big": {"type": "string", "description": "x".repeat(MAX_MCP_SCHEMA_CHARS * 4)}
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
    fn rejects_oversized_content_length_before_allocation() {
        let header = format!("Content-Length: {}", MAX_MCP_FRAME_BYTES + 1);
        let error = parse_content_length(&header).unwrap_err();
        assert!(error.to_string().contains("exceeds limit"));
    }

    #[test]
    fn accepts_content_length_at_limit() {
        let header = format!("Content-Length: {MAX_MCP_FRAME_BYTES}");
        assert_eq!(parse_content_length(&header).unwrap(), MAX_MCP_FRAME_BYTES);
    }

    #[test]
    fn write_message_uses_json_line() {
        let message = json!({"jsonrpc":"2.0","id":1,"method":"ping"});
        let line = encode_message_line(&message).unwrap();
        let body = serde_json::to_vec(&message).unwrap();
        assert_eq!(&line[..line.len() - 1], body.as_slice());
        assert_eq!(line.last(), Some(&b'\n'));
    }

    #[test]
    fn bounds_mcp_metadata() {
        let description = "x".repeat(MAX_MCP_DESCRIPTION_CHARS + 10);
        let bounded = bounded_tool_description("tool", "server", &description);
        assert!(bounded.contains("[truncated]"));
        assert!(bounded.chars().count() <= MAX_MCP_DESCRIPTION_CHARS + 100);

        let schema = json!({"type":"object","description":"x".repeat(MAX_MCP_SCHEMA_CHARS + 10)});
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
        send({"jsonrpc":"2.0","id":msg["id"],"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"test","version":"1"}}})
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
            enabled: true,
        };

        let manager = McpManager::start(&[config]).await.unwrap();
        let diagnostics = manager.servers[0].diagnostics.snapshot().await;

        assert!(
            diagnostics
                .iter()
                .any(|message| message.contains("notifications/message"))
        );
    }

    #[tokio::test]
    async fn failing_mcp_server_stderr_appears_in_error() {
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
            enabled: true,
        };

        let error = match McpServer::start(&config).await {
            Ok(_) => panic!("expected MCP startup to fail"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("startup exploded"));
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
        send({"jsonrpc":"2.0","id":msg["id"],"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"test","version":"1"}}})
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
