# ACP investigation

This document captures an initial investigation of an ACP-style stdio JSON-RPC server mode for Ferrum.

## Reference material

Before implementing ACP, review these upstream resources:

- `sacp` crate docs: https://docs.rs/sacp/11.0.0/sacp/index.html
- `sacp-cookbook` guide: https://docs.rs/sacp-cookbook/latest/sacp_cookbook/building_an_agent/index.html

These may reduce protocol-design work if they match the external ACP shape Ferrum needs. At minimum, they provide useful prior art for message types, stdio server loops, session handling, and event streaming.

## Goal

Add a long-lived machine-driven mode such as:

```bash
ferrum acp
```

The purpose is to let external clients drive Ferrum sessions through a stable stdio protocol instead of invoking one-shot print mode repeatedly.

## Why this matters

Ferrum already has most of the core agent capabilities needed by an external bridge:

- named/resumable JSONL sessions
- print mode and interactive mode
- AGENTS.md context loading
- tool allow/deny policy
- MCP support
- skills
- provider/model config
- cancellation primitives in interactive mode

But `ferrum --session <id> -p "..."` is not enough for a rich bridge because it lacks:

- streaming assistant text
- structured thought/tool call/tool result events
- long-lived process efficiency
- protocol-level cancellation
- slash-command forwarding
- structured error responses

## What ACP would change architecturally

ACP would become a third runtime surface beside:

- interactive REPL
- one-shot print mode

That means Ferrum would need a stronger separation between:

- transport/UI concerns
- agent-core behavior
- event emission

## Recommended architecture direction

### 1. Introduce an internal event model

Today many runtime events are rendered directly to the terminal. ACP would need them as structured events.

Suggested direction:

```rust
pub enum AgentEvent {
    TextDelta { session_id: String, content: String },
    ThoughtDelta { session_id: String, content: String },
    ToolCall { session_id: String, tool_name: String, args: serde_json::Value },
    ToolResult {
        session_id: String,
        tool_name: String,
        result: String,
        is_error: bool,
    },
    Done { session_id: String },
    Error { session_id: String, message: String },
}
```

Then:

- interactive mode renders `AgentEvent` values to the terminal
- ACP mode serializes them to JSON-RPC notifications

This is likely the most important prerequisite.

### 2. Keep transport separate from the core loop

ACP should not be implemented by scraping Ferrum terminal output. Instead, the core turn execution should emit structured events to a sink/callback.

Suggested split:

- agent core: session state + turn execution
- terminal renderer: current human-facing output
- ACP server: JSON-RPC request/response + event serialization

### 3. Reuse session primitives

Ferrum already has good session mechanics:

- named sessions
- resume/switch/delete
- cwd-aware session listing
- JSONL persistence

ACP can build on that by lazily opening sessions on demand in one long-lived process.

## Minimum useful ACP slice

A first iteration does not need to implement everything.

Recommended first milestone:

- `initialize`
- `session/new`
- `session/prompt`
- streamed notifications for:
  - `text`
  - `thought`
  - `done`
  - `error`

This is enough to prove the server mode and support a basic bridge.

## Tool events

Second milestone:

- emit `tool_call`
- emit `tool_result`

Ferrum already has clean tool execution points, so this should be feasible once the internal event model exists.

## Slash command forwarding

Ferrum slash commands are currently REPL-side behavior, not a generic protocol layer.

ACP should not blindly expose every slash command.

Possible safe subset:

- `/compact`
- `/session`
- `/model`
- `/provider`
- `/thinking`
- `/diff`
- `/colors`

Commands such as `/quit` or picker-driven commands should remain terminal-only.

A future ACP method like `command/run` could forward only a whitelisted subset.

## Cancellation

Ferrum already has cancellation primitives for in-flight turns.

ACP should likely support:

```text
session/cancel
```

Implementation idea:

- maintain `session_id -> in_flight_abort_token`
- cancellation flips the token
- normal event stream reports completion/error cleanly

## MCP considerations

Ferrum ACP and MCP should coexist, but the first ACP version should probably avoid dynamic per-request MCP reconfiguration.

Recommended initial rule:

- ACP uses current config/runtime MCP policy only
- no per-session `mcpServers` override yet

Reason:

- simpler lifecycle management
- avoids accidental policy bypass
- easier to reason about long-lived MCP manager state

## Security model

ACP clients must be treated as untrusted.

Ferrum should continue to enforce:

- tool allow/deny rules
- MCP enablement rules
- slash-command whitelisting
- server-side validation of tool execution

The client must not be trusted to enforce permissions.

## Protocol shape

Exact OpenCode ACP compatibility would be useful, but a Ferrum-specific ACP-like protocol is acceptable if it is:

- line-delimited stdio JSON-RPC
- stable and documented
- expressive enough for text/thought/tool/done/error streaming

Recommended approach:

- define a small Ferrum ACP v1 first
- keep method/event naming close to the expected bridge model
- document any deviations clearly

## Code organization

A likely implementation shape:

```text
src/acp/
  mod.rs
  protocol.rs
  server.rs
  session.rs
```

And some refactoring in `src/agent/` to route structured events to either:

- terminal renderer
- ACP transport

## Testing implications

ACP needs integration tests beyond current unit coverage.

Likely tests:

- initialize request/response
- create/resume named session
- prompt request with streamed text events
- tool event emission
- cancellation
- malformed JSON-RPC error handling
- session persistence across ACP requests

## Recommended implementation order

### Phase 1

Refactor toward an internal `AgentEvent` stream without changing user-visible behavior.

### Phase 2

Add `ferrum acp` with:

- `initialize`
- `session/new`
- `session/prompt`
- `text` / `thought` / `done` / `error`

### Phase 3

Add:

- `tool_call`
- `tool_result`

### Phase 4

Add control-plane methods:

- `session/cancel`
- `session/list`
- `session/delete`
- optional `command/run`

## Performance and memory considerations

ACP should be treated as an opt-in runtime surface:

```bash
ferrum acp
```

Normal interactive and print modes should not pay a meaningful runtime cost when ACP is unused.

### Likely memory impact

ACP mode will likely have a higher baseline resident set size than one-shot print mode because it keeps more state alive in one long-lived process, such as:

- session state
- provider client state
- optional MCP manager state
- cached model/session metadata

This is acceptable for ACP mode, but should not spill over into normal CLI usage.

### Likely performance impact

ACP can improve bridge-style workloads because it avoids repeated process startup and repeated config/session bootstrap for every prompt.

Possible wins:

- no repeated process startup
- no repeated provider/auth initialization
- hot in-memory session reuse
- cleaner streaming/event handling for external clients

Possible regressions if designed poorly:

- JSON-RPC serialization overhead in the normal CLI path
- unnecessary queues/threads when ACP is unused
- too much abstraction/indirection in the core loop

### Design rules to avoid regressions

- ACP must remain a separate mode, not a mandatory runtime wrapper around all Ferrum execution.
- Shared event plumbing should be close to zero-cost when no ACP/event sink is attached.
- Session loading in ACP should be lazy.
- Normal CLI rendering should not require protocol framing layers.

### Benchmark expectations

If ACP work starts, existing Ferrum benchmark profiles should be rerun after the core refactor to ensure:

- no meaningful wall-time regression in normal Ferrum CLI benchmarks
- no unexpected RSS growth in normal Ferrum CLI benchmarks
- no accidental increase in model/tool round counts

ACP-specific benchmarks can then be added separately for long-lived multi-turn bridge workflows.

## Recommendation

This is a worthwhile feature if Ferrum is expected to serve as an external harness for chat bridges.

But the first step should be the event-model refactor, not the JSON-RPC shell. That is the key architectural foundation that makes both ACP and future transports cleaner.
