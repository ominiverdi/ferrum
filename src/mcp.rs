use crate::{agent::tools::ToolDefinition, config::McpServerConfig};
use anyhow::{Context, Result};
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

        for server_config in configs.iter().filter(|server| server.enabled) {
            let mut server = match timeout(MCP_START_TIMEOUT, McpServer::start(server_config)).await
            {
                Ok(Ok(server)) => server,
                Ok(Err(error)) => {
                    eprintln!("[mcp] failed to start `{}`: {error}", server_config.name);
                    continue;
                }
                Err(_) => {
                    eprintln!("[mcp] timed out starting `{}`", server_config.name);
                    continue;
                }
            };
            let server_index = servers.len();
            let listed_tools = match timeout(MCP_REQUEST_TIMEOUT, server.list_tools()).await {
                Ok(Ok(tools)) => tools,
                Ok(Err(error)) => {
                    eprintln!(
                        "[mcp] failed to list tools for `{}`: {error}",
                        server_config.name
                    );
                    continue;
                }
                Err(_) => {
                    eprintln!("[mcp] timed out listing tools for `{}`", server_config.name);
                    continue;
                }
            };
            for tool in listed_tools {
                let exposed_name = format!(
                    "mcp__{}__{}",
                    sanitize_name(&server_config.name),
                    sanitize_name(&tool.name)
                );
                tool_routes.insert(exposed_name.clone(), (server_index, tool.name.clone()));
                tools.push(ToolDefinition {
                    name: exposed_name,
                    description: format!(
                        "MCP tool `{}` from server `{}`. {}",
                        tool.name,
                        server_config.name,
                        tool.description.unwrap_or_default()
                    ),
                    input_schema: tool
                        .input_schema
                        .unwrap_or_else(|| json!({"type":"object"})),
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
        let body = serde_json::to_vec(message)?;
        if let Err(error) = self.stdin.write_all(&body).await {
            return Err(self.write_error_context(error).await);
        }
        if let Err(error) = self.stdin.write_all(b"\n").await {
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

        let len = trimmed
            .strip_prefix("Content-Length:")
            .context("MCP message missing Content-Length")?
            .trim()
            .parse::<usize>()?;
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

fn render_tool_result(value: &Value) -> String {
    if let Some(content) = value.get("content").and_then(Value::as_array) {
        let mut text = String::new();
        for item in content {
            if item.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(part) = item.get("text").and_then(Value::as_str) {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(part);
                }
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
