# MCP

Ferrum supports a minimal MCP stdio client.

Status: early feature. It is useful for local stdio servers, but not a complete MCP implementation.

## Supported

- stdio servers
- `initialize`
- `notifications/initialized`
- `tools/list`
- `tools/call`
- tool discovery at first model turn
- MCP tool output truncation
- namespaced MCP tool names

## Not supported yet

- HTTP/SSE transports
- resource subscriptions
- prompts
- sampling
- per-tool confirmation policy
- dynamic rediscovery while a session is running

## Configuration

Add servers to `~/.config/ferrum/config.toml`:

```toml
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

Ferrum exposes MCP tools with a namespace:

```text
mcp__<server>__<tool>
```

Example:

```text
mcp__filesystem__read_file
```

Characters outside ASCII letters, digits, `_`, and `-` are replaced with `_`.

## Safety

MCP servers run as local child processes with your user permissions. Ferrum does not sandbox them.

Do not configure MCP servers with secrets unless you trust the server and its dependencies.

MCP tool output is truncated to a bounded tail before it is returned to the model.
