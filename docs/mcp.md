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
- validated initialize protocol/capability negotiation
- paginated `tools/list` with page, cursor, and tool-count limits
- `tools/call`
- standard `notifications/cancelled` for aborted or timed-out tool calls
- persistent reader/writer tasks so caller cancellation cannot split MCP frames
- bounded JSONL lines, headers, framed bodies, and stderr lines
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
- server-originated requests such as sampling; Ferrum returns JSON-RPC method-not-found
- model consumption of MCP image, audio, or resource content; unsupported items are reported explicitly

## Configuration

Add servers to `~/.config/ferrum/config.toml`:

```toml
mcp_enabled = true

[[mcp.servers]]
name = "filesystem"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/ominiverdi/github"]
env = ["PATH", "HOME"]
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
env = ["PATH"]
```

MCP children start with a filtered environment containing only common process and
desktop-session variables such as `PATH`, `HOME`, locale variables, `TERM`, XDG
paths, and display/session-bus addresses. `env` is an additional explicit
allowlist of variable names copied from Ferrum's environment when the server
starts. Add only variables the server requires. Provider keys, OAuth tokens,
SSH agent sockets, and unrelated ambient credentials are not inherited unless
named explicitly.

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

When an interactive MCP tool call is aborted with `Esc` or `Ctrl-C`, or reaches Ferrum's MCP request timeout, Ferrum stops waiting and queues the standard `notifications/cancelled` notification with the in-flight request ID. Cancellation is best-effort: MCP servers may ignore the notification or finish before receiving it. Once a request has been dispatched, Ferrum reports its side-effect outcome as indeterminate rather than implying that cancellation rolled it back.

Ferrum writes MCP stdio requests as newline-delimited JSON for compatibility with local stdio servers. Incoming messages may be newline-delimited JSON or `Content-Length` frames. A persistent reader owns stdout and completes every frame even when the caller stops waiting; a persistent writer serializes complete outbound frames. JSONL lines, individual headers, aggregate headers, framed bodies, and stderr lines all have byte limits applied while reading. A framing or I/O failure quarantines that server transport.

MCP stderr content is not included in tool errors or model context. Errors expose only bounded summaries indicating that diagnostics were withheld. MCP child processes receive a small baseline of non-secret process/session variables plus variables explicitly named in their per-server allowlist. Child process groups are terminated when their server is dropped, although separately detached descendants can still escape process-group cleanup.

MCP tool descriptions and input schemas are bounded before they become model-visible tool definitions. Oversized descriptions are truncated; oversized schemas are replaced with a small generic object schema noting omission.
