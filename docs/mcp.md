# MCP

Ferrum supports a minimal MCP stdio client.

Status: early feature. It is useful for local stdio servers, but not a complete MCP implementation.

MCP is enabled by default for compatibility. Disable it for coding-only turns to avoid starting MCP servers and to keep MCP tool schemas out of model requests:

```bash
ferrum --no-mcp -p "fix this"
ferrum --mcp -p "use any configured MCP server if needed"
ferrum --mcp chrome-devtools web-search -p "use only selected MCP servers if needed"
```

Interactive:

```text
/mcp
/mcp on
/mcp off
/mcp status
/mcp list
```

`/mcp status` and `/mcp list` show configured servers after any `--mcp <server...>` narrowing, exposed MCP tools, total tools, and schema bytes before any `--tools` or `[tools]` narrowing for a model turn.

## Supported

- stdio servers
- `initialize`
- `notifications/initialized`
- `tools/list`
- `tools/call`
- tool discovery at first model turn
- MCP tool output truncation
- bounded MCP frame size before allocation
- namespaced MCP tool names
- sanitized-name collision rejection

## Not supported yet

- HTTP/SSE transports
- resource subscriptions
- prompts
- sampling
- per-tool confirmation prompts
- dynamic rediscovery while a session is running

## Configuration

Add servers to `~/.config/ferrum/config.toml`:

```toml
mcp_enabled = true

[[mcp.servers]]
name = "filesystem"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/ominiverdi/github"]
enabled = true
```

Multiple servers are supported:

```toml
[[mcp.servers]]
name = "server-a"
command = "server-a"
args = []

[[mcp.servers]]
name = "server-b"
command = "server-b"
args = ["--flag"]
```

## Tool names

Ferrum exposes MCP tools with a namespace when MCP is enabled and the active tool policy permits them:

```text
mcp__<server>__<tool>
```

Example:

```text
mcp__filesystem__read_file
```

Characters outside ASCII letters, digits, `_`, and `-` are replaced with `_`. If two configured tools would expose the same sanitized name, Ferrum rejects MCP startup instead of silently overwriting one route.

## Safety

MCP servers run as local child processes with your user permissions. Ferrum does not sandbox them.

Do not configure MCP servers with secrets unless you trust the server and its dependencies.

MCP tool output is truncated to a bounded tail before it is returned to the model.

MCP JSON-RPC frames with `Content-Length` larger than Ferrum's internal frame limit are rejected before allocating the body buffer. This protects the Ferrum process from oversized MCP frames, but it does not sandbox the MCP server process itself.
