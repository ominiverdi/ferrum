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
echo "summarize this repo" | ferrum -p
cat file.rs | ferrum -p "review"
ferrum --provider openai-codex --model gpt-5.5 -p "review this repo"
ferrum --image screenshot.png -p "describe this image"
```

Behavior:

- Accept prompt args.
- Accept stdin as the prompt when `-p`/`--print` is passed without a value.
- Append piped stdin to an explicit print prompt.
- Accept provider/model/thinking/safety/tool overrides from CLI.
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
- `/title [text]``
- `/sessions`
- `/sessions pick`
- `/sessions del`
- `/sessions new`
- `/model [name]`
- `/models`
- `/usage [day|week|month]`
- `/provider [name]`
- `/providers`
- `/mcp [on|off|status|list]`
- `/colors [auto|on|off]`
- `/thinking [off|minimal|low|medium|high|xhigh]`
- `/safety [low|medium|high]`
- `/diff [unified|compact|full|words|side_by_side]`
- `/skills`
- `/skill:<name> [args]`
- `/skill <name> [args]`
- `/image <path>`
- `/image-paste`
- `/paste-image`
- `/compact`

Shell shortcuts:

- `!!<cmd>` runs a shell command and prints output only.
- `!<cmd>` runs a shell command and sends formatted output to the model.
- Prefix either command body with `--timeout-seconds=N` to select a foreground timeout from 1 through 600 seconds; the default is 120 seconds.

Session resume:

- `ferrum --continue` resumes the latest JSONL session for the current directory.
- `ferrum --resume` resumes the latest JSONL session for the current directory.
- `ferrum --resume <path|id-prefix>` resumes a specific JSONL session.
- `ferrum --session <name> -p <prompt>` resumes or creates a named print-mode session.
- `ferrum --session <path|id-prefix>` opens a specific JSONL session in interactive mode.
- Resumed interactive sessions show the last 40 visible conversation lines before prompting.
- `/sessions` lists current-directory sessions.
- `/sessions pick` provides a lightweight numbered picker with text filtering.
- `/sessions del` provides a deletion picker.
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
allow = ["read", "grep", "find", "bash", "wait"]
deny = ["write", "edit"]
writable_roots = ["."]

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
env = ["PATH", "HOME"]
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

Files are deduplicated and streamed only up to the remaining 128 KiB aggregate budget before inclusion in the system prompt. More-specific files are selected first for budgeting, then rendered in normal general-to-specific order so later instructions override earlier conflicts.

Ferrum also injects runtime context describing current version, provider, provider model, thinking level, safety level, writable roots, cwd, config dir, and supported interactive commands. The embedded default runtime system prompt can be fully overridden with `~/.config/ferrum/system.md`; Ferrum renders known `{{placeholder}}` values from current runtime config and leaves unknown placeholders unchanged.

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

Messages use stable JSON content blocks and include text, tool calls/results, and image blocks where applicable. Metadata entries store title, thinking level, safety level, diff mode, color mode, and resolved tool lists. Timestamps are `u64` milliseconds.

Sessions remain human-inspectable and append-oriented. Anonymous names include UUID entropy. Records are bounded to 16 MiB, pre-serialized with their newline, and appended under an exclusive advisory lock; readers take a shared lock and stream bounded records. A writer repairs an incomplete trailing record under lock before appending. Successful appends are flushed and synced, and creation syncs the session directory. Header-only sessions are retained, while automatic latest-session selection skips abandoned anonymous headers. Session/config switches validate a candidate session and resolve candidate provider/model state before committing either in-memory transition. Future branching/forking must preserve backward compatibility.

## Agent loop

Core loop:

1. Build context from runtime system prompt, context files, skills summary, session history, current user message, and the active tool definitions.
2. Send request to selected provider.
3. Receive final assistant message.
4. Display assistant text with `<think>...</think>` blocks hidden and untrusted terminal control protocols removed while preserving raw session content.
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

1. Regenerate immutable runtime, repository-instruction, and skill-index system messages from current process state.
2. Keep recent non-system conversation, up to a recent-context token budget.
3. Avoid retaining orphan tool results whose corresponding tool calls were summarized away.
4. Summarize older non-system messages with the current provider model using a structured summary prompt.
5. Store the summary as a `compaction` session entry and insert it as untrusted user-role context.
6. Retain only the newest generated summary and discard transient generated system messages.
7. For manual `/compact`, skip if there is nothing old enough to summarize or if the resulting context is not smaller.
8. For automatic over-budget compaction, fall back to a local heuristic summary if model-generated compaction fails.

Automatic compaction runs as a preflight before every provider request, including each tool-loop continuation and forced final synthesis. The projection uses the larger of provider-informed current usage and the local message estimate, includes pending messages and active tool schemas, and reserves 16,384 tokens on larger context windows (95% on smaller windows). If compaction cannot bring the projected request below that safe threshold, Ferrum fails with an explicit context-budget error instead of knowingly sending an over-budget request.

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

Provider HTTP status bodies and non-streaming responses use bounded collectors with idle and total deadlines. Streaming uses incremental bounded SSE decoding, a 60-second initial-response deadline, a 90-second chunk-idle deadline, and no total duration cap while data remains active. UTF-8 is preserved across network chunks; malformed framing, parser failures, missing terminal events, and exceeded field/response budgets fail the turn. Context-overflow recovery is triggered only by typed provider status/event signals.

Authenticated non-loopback `http://` base URLs are rejected unless the provider explicitly sets `allow_insecure_http = true`. Loopback and authless HTTP providers remain supported.

### OpenAI Codex / ChatGPT

- Auth: `ferrum login openai` OAuth.
- Backend: `https://chatgpt.com/backend-api/codex/responses`.
- Model listing: live `GET /codex/models?client_version=<version>`.
- Supports reasoning effort mapping and tool calls.
- Supports image input for compatible models.
- Retries explicit 408, 429, and 5xx rejections and connection failures up to three times, honoring bounded `Retry-After`; ambiguous initial timeouts and failures after streaming starts are not replayed.

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
allow = ["read", "grep", "find", "bash", "wait"]
deny = ["write", "edit"]
writable_roots = ["."]
```

`allow` is optional and caps the available tool set. `deny` removes tools. Invalid requested tools fail before the model request. Resolved tool lists are stored in session metadata for visibility and audit, but resume uses the current process/config policy so new default tools can appear automatically unless limited by policy.

Default native tools include file/shell tools plus model-facing session history tools:

```text
read write edit bash wait grep find ls history_search history_read
```

The history tools are current-session only and include archived pre-compaction JSONL entries. They are exposed to the model as tools, not as slash commands.

### `read`

Read a text file with optional offset/limit. Reading is bounded to 50 KiB even for one giant line; leading blank lines are preserved and counted.

### `write`

Create or atomically replace a file. `medium` limits targets to configured writable roots; `low` bypasses writable roots; `high` rejects mutation. Creates parent directories, preserves existing permissions, and rejects protected system/credential, symlink, or changed target identities.

### `edit`

Exact text replacement. `medium` limits targets to configured writable roots; `low` bypasses writable roots; `high` rejects mutation. Each old text must match exactly once. Multiple non-overlapping edits supported. Preserve UTF-8 BOM and existing LF/CRLF line endings. Ferrum rejects protected system/credential targets and commits through synced sibling-temp plus identity-checked atomic replacement.

### `bash`

Run a shell command in cwd. Capture stdout/stderr and return an explicit exited/timed-out/cancelled outcome, exit status, output-completeness state, cleanup errors, containment mode, and residual-descendant state. Output is incrementally bounded in memory and spooled to a private runtime file when large. Stdin is closed, stdout/stderr are piped, and the command runs in its own process group. Ferrum uses a delegated cgroup-v2 child where available so timeout/abort can terminate descendants that leave the process group; otherwise it reports the process-group fallback and unknown residual state. Pipe draining has a deadline and incomplete output is marked explicitly. Before execution, Ferrum parses complete Bash syntax with byte/node/depth limits and applies `/safety low|medium|high`. Low grants broad current-user shell and mutation authority; medium permits direct trusted-checkout development with writable-root enforcement while rejecting dynamic/indirect authority; high permits conservative inspection only. Malformed syntax, explicit catastrophic/privilege operations, protected credential mutation, and resource-limit violations fail closed at every tier. This is policy, not process isolation.

### `wait`

Wait in the foreground, then run a bash command using the same execution tier, writable-root policy, and cleanup path. The wait duration is capped at 30 minutes. Interactive users can abort with `Esc` or `Ctrl-C`. Exposed only when `bash` is available.

### `grep`

Search file contents. Uses streamed ripgrep JSON when available, with a streaming Rust fallback that preserves regex and literal matching semantics. Both paths enforce global match/output limits, bounded lines, cancellation, and a 10-second deadline; fallback file symlinks are rejected.

### `find`

Find files by name/glob with cancellation and a 10-second traversal deadline.

### `history_search`

Search rendered current-session JSONL entries, including text, tool calls, tool results, image metadata, and entries archived before compaction. Returns matching snippets with JSONL line numbers and active/archived status. Literal case-insensitive search is the default; regex search is available.

### `history_read`

Read rendered current-session entries by 1-based JSONL line number. Intended as follow-up after `history_search`. Output is role/content text, not raw JSONL.

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

Characters outside ASCII letters, digits, `_`, and `-` are sanitized to `_`; sanitized-name collisions are rejected. Ferrum writes newline-delimited JSON to MCP stdio servers for compatibility. A persistent reader accepts bounded newline-delimited JSON or `Content-Length` frames without allowing caller cancellation to split framing; a persistent writer serializes complete outbound frames. JSONL lines, individual and aggregate headers, framed bodies, and stderr lines are bounded while reading. Initialize negotiation and paginated tool discovery are validated and bounded. MCP children inherit a filtered baseline of common process/session variables plus variables explicitly named by the server's `env` allowlist; provider credentials are excluded by default. Tool output and model-visible metadata are bounded before being returned to or sent to the model.

HTTP/SSE MCP transports are deferred.

Provider adapters fail a turn when a provider returns malformed JSON for tool-call arguments instead of silently converting the input to `null`.

## Images

Ferrum supports PNG, JPEG, and WebP input.

Sources:

- CLI `--image <PATH>`
- Interactive `/image <path>`
- Interactive `/paste-image`
- pasted file paths
- `data:image/...;base64,...`

Images are validated through bounded PNG/JPEG/WebP decoding and stored inline in session JSONL as base64 content blocks. A turn is limited to 8 images / 20 MiB decoded; retained session context is limited to 32 images / 64 MiB decoded, with corresponding encoded budgets. Multi-image attachment is transactional. Terminal previews use a private copy of validated bytes and run `chafa` under output and time limits; otherwise Ferrum prints metadata.

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
    atomic_file.rs
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
    terminal_text.rs
    tools/
      bash.rs
      edit.rs
      find.rs
      grep.rs
      ls.rs
      mod.rs
      path.rs
      read.rs
      shell_guard.rs
      shell_policy.rs
      write.rs
      write_policy.rs
```

## Quality bar

- Unit tests for message conversion, edit behavior, session JSONL, skills, context loading, and path handling.
- Local smoke tests before release.
- No secret values in logs or tests.
- Clear errors over silent fallback.
- No publishing without local validation and explicit user approval.
