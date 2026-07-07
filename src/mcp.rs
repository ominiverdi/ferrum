use crate::{agent::tools::ToolDefinition, config::McpServerConfig};
use anyhow::{Context, Result};
use futures_util::future::join_all;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::HashMap, io, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::timeout,
};

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
const MAX_MCP_OUTPUT_CHARS: usize = 20_000;
const MAX_MCP_FRAME_BYTES: usize = 10 * 1024 * 1024;
const MAX_MCP_DESCRIPTION_CHARS: usize = 2_000;
const MAX_MCP_SCHEMA_CHARS: usize = 8_000;
const MCP_START_TIMEOUT: Duration = Duration::from_secs(10);
const MCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

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
}

impl McpServer {
    async fn start(config: &McpServerConfig) -> Result<Self> {
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
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
        let mut server = Self {
            stdin,
            stdout: BufReader::new(stdout),
            _child: child,
            next_id: 1,
        };
        server.initialize().await?;
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
        Ok(truncate_output(render_tool_result(&value)))
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
                continue;
            }
            if let Some(error) = message.get("error") {
                anyhow::bail!("MCP {method} failed: {error}");
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
                anyhow::anyhow!(
                    "MCP server exited before request could be written: {status}: {error}"
                )
            }
            Ok(None) => anyhow::anyhow!("failed to write MCP request to server stdin: {error}"),
            Err(wait_error) => anyhow::anyhow!(
                "failed to write MCP request to server stdin: {error}; failed to inspect MCP server status: {wait_error}"
            ),
        }
    }

    async fn read_message(&mut self) -> Result<Value> {
        let mut line = String::new();
        let bytes = self.stdout.read_line(&mut line).await?;
        if bytes == 0 {
            anyhow::bail!("MCP server closed stdout");
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if !trimmed.starts_with("Content-Length:") {
            return serde_json::from_str(trimmed).context("failed to parse MCP JSON-RPC line");
        }

        let len = parse_content_length(trimmed)?;
        loop {
            line.clear();
            let bytes = self.stdout.read_line(&mut line).await?;
            if bytes == 0 {
                anyhow::bail!("MCP server closed stdout before body");
            }
            if line.trim_end_matches(['\r', '\n']).is_empty() {
                break;
            }
        }
        let mut body = vec![0; len];
        self.stdout.read_exact(&mut body).await?;
        serde_json::from_slice(&body).context("failed to parse MCP JSON-RPC message")
    }
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

fn parse_content_length(header: &str) -> Result<usize> {
    let len = header
        .strip_prefix("Content-Length:")
        .context("MCP message missing Content-Length")?
        .trim()
        .parse::<usize>()?;
    if len > MAX_MCP_FRAME_BYTES {
        anyhow::bail!("MCP message Content-Length {len} exceeds limit {MAX_MCP_FRAME_BYTES}");
    }
    Ok(len)
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
    let text = schema.to_string();
    if text.chars().count() <= MAX_MCP_SCHEMA_CHARS {
        return schema;
    }
    json!({
        "type": "object",
        "description": "MCP input schema omitted because it exceeded Ferrum's metadata size limit"
    })
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
