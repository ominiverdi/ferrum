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

Each ACP session owns an independent Ferrum agent session and canonical absolute working directory. Ferrum's configured safety tier, tool selection, writable roots, credential protection, shell guards, containment, and other tier-independent checks remain in force.

## Resource bounds

The stdio adapter bounds input lines, decoded request structure, prompt text, output lines, queued output, session count, active turns, tool update content, and tool input metadata. Output writes are serialized. EOF cancels active turns before session and child-process cleanup.

## Deliberate baseline limits

This milestone does not yet accept client-supplied MCP servers or additional workspace directories. HTTP/SSE MCP remains tracked separately. Unsupported ACP methods return JSON-RPC `method not found` errors.

Do not describe or register this build as fully ACP-compatible until the required stdio MCP work and interoperability suite are complete.
