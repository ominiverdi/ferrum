# Ferrum Project Spec

## Summary

Ferrum is a Linux-only, Rust-native coding agent. It is inspired by Pi's useful agent-harness ideas, but it is not a compatibility port.

The target is a small, predictable daily-driver CLI agent for local coding work.

## Principles

1. Linux first, Linux only for v1.
2. Barebone beats feature-complete.
3. Fast startup and low runtime overhead matter.
4. Tool correctness matters more than UI richness.
5. Provider logic stays explicit, testable, and thin.
6. Tool execution and file mutation stay provider-neutral in the core loop.
7. Sessions are durable, JSONL, and inspectable.
8. Configuration uses simple files and environment variables.
9. Secrets are never hardcoded, logged, or committed.

## Modes

### Print mode

Single-shot mode:

```bash
ferrum -p "summarize this repo"
cat file.rs | ferrum -p "review"
ferrum --provider openai-codex --model gpt-5.5 -p "review this repo"
ferrum --image screenshot.png -p "describe this image"
```

Behavior:

- Accept prompt args.
- Accept stdin.
- Accept provider/model/thinking/tool overrides from CLI.
- Accept repeated `--image` attachments.
- Print assistant output to stdout.
- Return non-zero on unrecoverable errors.
- Persist session entries.

### Interactive mode

Default mode:

```bash
ferrum
```

Behavior:

- Line-oriented REPL using `rustyline`.
- History stored under the Ferrum config directory.
- Session autosave.
- Provider errors are reported without exiting the REPL.
- Ctrl+D exits.
- Ctrl+C once clears/returns to prompt; double Ctrl+C exits.

Slash commands:

- `/quit`, `/exit`
- `/help`
- `/version`
- `/session`
- `/sessions`
- `/sessions <number|id-prefix|path>`
- `/sessions pick`
- `/sessions new`
- `/model [name]`
- `/models`
- `/provider [name]`
- `/providers`
- `/thinking [off|minimal|low|medium|high|xhigh]`
- `/skills`
- `/skill:<name> [args]`
- `/skill <name> [args]`
- `/image <path>`
- `/paste-image`
- `/compact`

Shell shortcuts:

- `!!<cmd>` runs a shell command and prints output only.
- `!<cmd>` runs a shell command and sends formatted output to the model.

Session resume:

- Resume flags apply to interactive mode. Print mode currently starts a fresh session.
- `ferrum --continue` resumes the latest JSONL session for the current directory.
- `ferrum --resume` resumes the latest JSONL session for the current directory.
- `ferrum --resume <path|id-prefix>` resumes a specific JSONL session.
- `ferrum --session <path|id-prefix>` opens a specific JSONL session.
- `/sessions` lists current-directory sessions with bracket numbers.
- `/sessions 2` opens entry `[2]` from the last list.
- `/sessions pick` provides a lightweight numbered picker with text filtering.
- `/sessions new` starts a fresh session.

## Configuration

Default config directory:

```text
~/.config/ferrum/
```

Main files:

```text
~/.config/ferrum/config.toml
~/.config/ferrum/auth.json
~/.config/ferrum/AGENTS.md
~/.config/ferrum/skills/
~/.local/share/ferrum/sessions/
~/.local/share/ferrum/history.txt
```

Config example:

```toml
provider = "openai-codex"
model = "gpt-5.5"
max_context_tokens = 256000
thinking = "off"

[tools]
allow = ["read", "grep", "find", "bash"]
deny = ["write", "edit"]

[providers.openai-codex]
type = "openai-codex"
base_url = "https://chatgpt.com/backend-api"
default_model = "gpt-5.5"

[models."gpt-5.5-small-context"]
actual_model = "gpt-5.5"
max_context_tokens = 6000

[[mcp.servers]]
name = "time"
command = "uvx"
args = ["mcp-server-time"]
enabled = true
```

Provider entries:

- `type = "openai-codex"` uses ChatGPT OAuth and the Codex Responses backend.
- `type = "openai-compatible"` uses Chat Completions with `base_url` and optional `api_key_env`.
- `type = "fake"` is for local tests/offline mode.

Secrets:

- API keys are read from environment variables named by `api_key_env`.
- ChatGPT/Codex OAuth credentials are stored in `auth.json` with user-only permissions where possible.
- Secret values must not be committed or logged.

Environment variables:

- `FERRUM_CONFIG_DIR`
- `FERRUM_OFFLINE`
- `FERRUM_CODEX_CLIENT_VERSION`
- Provider-specific env vars referenced by `api_key_env`

## Context files

Ferrum loads context from `AGENTS.md` and `agents.md` files:

1. Global: `~/.config/ferrum/AGENTS.md` or `~/.config/ferrum/agents.md`
2. Parent directories walking from filesystem root to cwd
3. Current directory

Files are deduplicated, bounded, and included in the system prompt. More specific later files override earlier instructions when conflicts exist.

Ferrum also injects runtime context describing current version, provider, provider model, thinking level, cwd, config dir, and supported interactive commands. The embedded default runtime system prompt can be fully overridden with `~/.config/ferrum/system.md`; Ferrum renders known `{{placeholder}}` values from current runtime config and leaves unknown placeholders unchanged.

## Sessions

Sessions are JSONL files under:

```text
~/.local/share/ferrum/sessions/
```

Ferrum moves a legacy `~/.config/ferrum/sessions/` directory into the data directory at startup and removes the old directory after the move completes. Ferrum also moves legacy `~/.config/ferrum/history.txt` to `~/.local/share/ferrum/history.txt`.

Current persisted entry types:

- `header`
- `message`
- `metadata`
- `compaction`

Messages use stable JSON content blocks and include text, tool calls/results, and image blocks where applicable. Metadata entries store title, thinking level, diff mode, and resolved tool lists. Timestamps are `u64` milliseconds.

Sessions should remain human-inspectable and append-oriented. Future branching/forking must preserve backward compatibility.

## Agent loop

Core loop:

1. Build context from runtime system prompt, context files, skills summary, session history, current user message, and the active tool definitions.
2. Send request to selected provider.
3. Receive final assistant message.
4. Display assistant text with `<think>...</think>` blocks hidden from terminal output while preserving raw session content.
5. If assistant requested tools:
   - render tool calls in readable multiline terminal format
   - execute tools in the core loop
   - append tool results
   - print a bounded result preview
   - repeat provider request
6. If the per-turn tool-round budget is exhausted, make one final no-tools provider request asking for findings and next steps.
7. If no tool calls, finish.
8. Persist user, assistant, and tool messages to session.

Provider adapters serialize and parse provider-specific payloads only. They do not execute tools.

## Compaction

Ferrum compaction is Pi-inspired but simpler:

1. Preserve system messages.
2. Keep recent non-system conversation, up to a recent-context token budget.
3. Avoid retaining orphan tool results whose corresponding tool calls were summarized away.
4. Summarize older non-system messages with the current provider model using a structured summary prompt.
5. Store the summary as a `compaction` session entry and insert it as system context.
6. For manual `/compact`, skip if there is nothing old enough to summarize or if the resulting context is not smaller.
7. For automatic over-budget compaction, fall back to a local heuristic summary if model-generated compaction fails.

Automatic compaction starts at 95% of the active context budget. Ferrum emits context-pressure warnings before that point and reports when compaction still leaves the session above budget.

The summary format tracks goal, constraints, progress, blockers, key decisions, next steps, and critical context.

## Normalized message model

```rust
enum Role {
    System,
    User,
    Assistant,
    Tool,
}

enum ContentBlock {
    Text { text: String },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
    Image {
        mime_type: String,
        data_base64: String,
        source: Option<String>,
    },
}

struct Message {
    role: Role,
    content: Vec<ContentBlock>,
}
```

## Providers

### OpenAI Codex / ChatGPT

- Auth: `ferrum login openai` OAuth.
- Backend: `https://chatgpt.com/backend-api/codex/responses`.
- Model listing: live `GET /codex/models?client_version=<version>`.
- Supports reasoning effort mapping and tool calls.
- Supports image input for compatible models.

### OpenAI-compatible

- Chat Completions wire format.
- Configured through `[providers.<name>]` with `type = "openai-compatible"`.
- Supports remote APIs and local `/v1` servers when they implement compatible chat, tool, and image semantics.
- Examples include user-defined presets for OpenCode Go, MiniMax, OpenAI-compatible proxies, LM Studio, vLLM, and Ollama-compatible `/v1` servers.

### Fake

- Local deterministic provider for tests/offline mode.

### Deferred provider work

- Anthropic-compatible `/messages` adapter.
- Provider-specific compatibility flags after verification with real providers.
- Richer streaming and usage reporting.

## Built-in tools

Tool exposure is controlled before provider requests:

```text
--tools omitted        => default available tools
--no-tools             => no tools
--tools read grep find => exactly those tools, subject to config policy
```

Config policy:

```toml
[tools]
allow = ["read", "grep", "find", "bash"]
deny = ["write", "edit"]
```

`allow` is optional and caps the available tool set. `deny` removes tools. Invalid requested tools fail before the model request. Resolved tool lists are stored in session metadata and restored on resume unless `--tools` is explicitly provided.

### `read`

Read a text file with optional offset/limit. Output is truncated safely.

### `write`

Create or overwrite a file. Creates parent directories.

### `edit`

Exact text replacement. Each old text must match exactly once. Multiple non-overlapping edits supported. Preserve UTF-8 BOM and existing LF/CRLF line endings.

### `bash`

Run a shell command in cwd. Capture stdout/stderr/exit code. Output is truncated to a bounded tail.

### `grep`

Search file contents.

### `find`

Find files by name/glob.

### `ls`

List directory contents.

## MCP

Ferrum supports stdio MCP servers configured under `[[mcp.servers]]`.

Implemented methods:

- `initialize`
- `notifications/initialized`
- `tools/list`
- `tools/call`

MCP tool names are exposed as:

```text
mcp__<server>__<tool>
```

HTTP/SSE MCP transports are deferred.

## Images

Ferrum supports PNG, JPEG, and WebP input.

Sources:

- CLI `--image <PATH>`
- Interactive `/image <path>`
- Interactive `/paste-image`
- pasted file paths
- `data:image/...;base64,...`

Images are stored inline in session JSONL as base64 content blocks. Terminal previews use `chafa` when installed; otherwise Ferrum prints metadata.

## Skills

Ferrum discovers Agent Skills-style instruction packages.

Locations:

```text
~/.config/ferrum/skills/
~/.agents/skills/
.ferrum/skills/
.agents/skills/
```

Skills use `SKILL.md` with frontmatter:

```yaml
---
name: example-skill
description: What this skill is for.
---
```

At startup, Ferrum adds only skill metadata to the system prompt. `/skill:<name> [args]` expands the full skill body into a Pi-style `<skill>` block and immediately runs a model turn with that expanded prompt.

Skills are instructions, not trusted code. Ferrum does not automatically run skill scripts.

## Repository layout

Current high-level layout:

```text
ferrum/
  Cargo.toml
  AGENTS.md
  docs/
  src/
    main.rs
    cli.rs
    config.rs
    context.rs
    mcp.rs
    skills.rs
    agent/
      messages.rs
      mod.rs
      tools.rs
    auth/
      mod.rs
      openai_codex.rs
    providers/
      fake.rs
      mod.rs
      openai.rs
    session/
      jsonl.rs
      mod.rs
    tools/
      bash.rs
      edit.rs
      find.rs
      grep.rs
      ls.rs
      mod.rs
      path.rs
      read.rs
      write.rs
```

## Quality bar

- Unit tests for message conversion, edit behavior, session JSONL, skills, context loading, and path handling.
- Local smoke tests before release.
- No secret values in logs or tests.
- Clear errors over silent fallback.
- No publishing without local validation and explicit user approval.
