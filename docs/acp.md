# ACP stdio baseline

Ferrum provides an incremental official Agent Client Protocol v1 transport:

```bash
ferrum acp
```

The process reads newline-delimited JSON-RPC 2.0 messages from stdin and writes protocol messages only to stdout. Sanitized diagnostics use stderr.

## Current methods

Supported:

- `initialize`
- `session/new`
- `session/list`
- `session/load`
- `session/resume`
- `session/close`
- `session/delete`
- `session/prompt`
- `session/cancel` notification
- `$/cancel_request` notification
- `session/update` notifications for:
  - `agent_message_chunk`
  - `agent_thought_chunk`
  - `tool_call`
  - `tool_call_update`
  - `usage_update`

Prompt responses use the official `stopReason` values. Text, resource-link, and validated image prompt blocks are accepted. Audio and embedded-resource prompt blocks are not advertised or accepted.

Session IDs are Ferrum's durable JSONL session IDs. Listing is newest-first, supports absolute `cwd` filtering and opaque cursor pagination, and returns bounded pages. Loading replays persisted user, agent, thought, and tool updates before returning; resuming activates the same history without replay. The request `cwd` must match the persisted canonical directory. Session provider, model, thinking, safety, and tool metadata follows the normal Ferrum restoration rules; explicit ACP-process CLI overrides remain authoritative. Active sessions must be idle before close and must be closed before deletion.

Each active ACP session owns an independent Ferrum agent session and canonical absolute working directory. Ferrum's configured safety tier, tool selection, writable roots, credential protection, shell guards, containment, and other tier-independent checks remain in force.

## Client-supplied MCP servers

`session/new`, `session/load`, and `session/resume` accept official ACP stdio MCP definitions. Each definition requires a unique name, an absolute executable path, bounded arguments, and bounded explicit environment name/value pairs. HTTP and SSE MCP are unadvertised and rejected.

Client servers are merged with the locally configured, enabled MCP servers for that session. An exact server-name collision or a tool-name collision after Ferrum's normal sanitization fails session setup rather than shadowing either server or tool. Client definitions are connection input and are not written to JSONL session files; a client must supply them again when loading or resuming after process restart.

MCP children start with `env_clear()`. Ferrum adds its documented minimal runtime environment and then the explicit ACP variables; ambient provider credentials are not inherited. Explicit values are treated as secrets and redacted from MCP errors, tool metadata, and tool output. MCP initialization, pagination, cancellation, stderr redaction, process-group/cgroup containment, and output/schema bounds are the same as for configured MCP servers. Closing a session, a failed setup, or ACP process teardown drops that session's manager and kills its MCP process tree.

Supplying an executable definition authorizes Ferrum to start that process. Expose `ferrum acp` only to a trusted editor or bridge that controls MCP definitions; do not forward untrusted chat content into session setup fields. `mcp_enabled = false` rejects client-supplied servers. Dynamic tools still pass through Ferrum's configured tool selection and execution policy.

## Resource bounds

The stdio adapter bounds input lines, decoded request structure, prompt text, output lines, queued output, active session count, listed-session page size and payload, loaded-history entries and bytes, active turns, tool update content, tool input metadata, client MCP server counts, names, commands, arguments, environment entries, and aggregate environment bytes. MCP transport framing, pagination, tool counts, names, descriptions, schemas, stderr, and output are independently bounded. Output writes are serialized. EOF cancels active turns before session and child-process cleanup.

## Deliberate baseline limits

This milestone does not accept additional workspace directories. HTTP/SSE MCP remains tracked separately. Unsupported ACP methods return JSON-RPC `method not found` errors.

Do not describe or register this build as fully ACP-compatible until the interoperability suite is complete.
