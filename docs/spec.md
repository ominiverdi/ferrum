# Ferrum Project Spec

## Summary
Ferrum is a Linux-only, Rust-native coding agent. It is inspired by Pi's minimal harness model, but intentionally drops TypeScript, extensions, npm packages, SDK compatibility, and cross-platform support.

The product target is a fast daily-driver CLI/TUI agent for local coding work.

## Principles
1. Linux first, Linux only for v1.
2. Barebone beats feature-complete.
3. Fast startup and low runtime overhead matter.
4. Tool correctness matters more than UI richness.
5. Provider logic should be explicit, testable, and isolated.
6. Sessions must be durable and inspectable.
7. Configuration should be simple files and environment variables.

## Modes

### Print Mode
Single-shot mode:

```bash
ferrum -p "summarize this repo"
cat file.rs | ferrum -p "review"
ferrum --provider opencode-go --model kimi-k2.6 -p "review this repo"
```

Requirements:
- Accept prompt args.
- Accept stdin.
- Accept provider/model overrides from CLI.
- Stream assistant output to stdout.
- Return non-zero on unrecoverable errors.

### Interactive Mode
Default mode:

```bash
ferrum
```

Requirements:
- Prompt editor.
- Streaming assistant output.
- Tool call/result display.
- Ctrl+C abort behavior.
- Session autosave.
- Minimal slash commands.

Initial slash commands:
- `/quit`
- `/model`
- `/provider`
- `/session`
- `/compact`

Session resume:
- `ferrum --resume` resumes the latest JSONL session.
- `ferrum --resume <path>` resumes a specific JSONL session.

## Configuration

Default config directory:

```text
~/.config/ferrum/
```

Initial files:

```text
~/.config/ferrum/config.toml
~/.config/ferrum/sessions/
```

Initial config keys:

```toml
provider = "openai-codex"
model = "gpt-5.3-codex"
max_context_tokens = 256000
thinking = "off" # off|minimal|low|medium|high|xhigh
```

Environment variables:
- `OPENAI_API_KEY`
- `OPENAI_BASE_URL`
- `OPENAI_CODEX_BASE_URL`
- `OPENCODE_API_KEY`
- `OPENCODE_GO_API_KEY_ENV`
- `OPENCODE_GO_BASE_URL`
- `MINIMAX_API_KEY`
- `MINIMAX_BASE_URL`
- `FERRUM_CONFIG_DIR`
- `FERRUM_OFFLINE`

No secrets are committed. No secrets are logged.

## Context Files
Ferrum loads context from `AGENTS.md` files:

1. Global: `~/.config/ferrum/AGENTS.md`
2. Parent directories walking from filesystem root to cwd
3. Current directory

Files are deduplicated, concatenated in load order, bounded in size, and included in the system prompt. More specific later files override earlier files when instructions conflict.

## Sessions
Sessions are JSONL files. They should remain human-inspectable and append-only where possible. Ferrum tracks approximate context size by text characters divided by four, plus message count and JSONL file bytes.

Default location:

```text
~/.config/ferrum/sessions/
```

Minimum entry types:
- `header`
- `message`
- `tool_result`
- `model_change`
- `compaction` later

Each entry should include:
- `id`
- `parent_id` where applicable
- `timestamp_ms`
- `type`

Tree-style branching is optional after v1. The schema should not prevent it.

## Agent Loop
Core loop:

1. Build context from system prompt, context files, session history, current user message, and tool definitions.
2. Send request to selected provider.
3. Stream assistant deltas to UI/stdout.
4. Accumulate final assistant message.
5. If assistant requested tools:
   - execute tools
   - append tool results
   - repeat provider request
6. If no tool calls, finish.
7. Persist messages and tool results to session.

Abort must cancel:
- active provider stream
- active tool execution where possible
- queued loop work

## Normalized Message Model

```rust
enum Role {
    System,
    User,
    Assistant,
    Tool,
}

enum ContentBlock {
    Text(String),
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
}

struct Message {
    role: Role,
    content: Vec<ContentBlock>,
}
```

Provider adapters translate between this normalized model and provider-specific payloads. Tool use remains provider-neutral in the core agent loop; adapters only serialize/deserialize provider-specific tool call formats.

## Providers

### v1 Providers
- OpenAI-compatible Chat Completions API
- OpenAI Codex / ChatGPT subscription OAuth backend
- OpenCode Go OpenAI-compatible Chat Completions models
- Minimax OpenAI-compatible API where available

Anthropic-compatible APIs are deferred until the OpenAI-compatible provider and generic tool loop are stable.

### Provider Responsibilities
- Serialize normalized messages and tools.
- Stream text deltas.
- Stream or return tool calls.
- Report usage when available.
- Normalize provider errors.
- Support cancellation through async abort/drop semantics.

Do not build a universal provider framework before two providers work end-to-end.

## Built-in Tools

### `read`
Read a text file with optional offset/limit. Output is truncated safely.

### `write`
Create or overwrite a file. Creates parent directories.

### `edit`
Exact text replacement. Each old text must match exactly once. Multiple non-overlapping edits supported. Preserve UTF-8 BOM and existing LF/CRLF line endings.

### `bash`
Run a shell command in cwd with timeout. Capture stdout/stderr/exit code. Output is truncated to a bounded tail. Long-running process management can come later.

### `grep`
Search file contents. Prefer `ripgrep` integration if available, with Rust fallback later if needed.

### `find`
Find files by name/glob.

### `ls`
List directory contents.

## Initial Repository Layout

```text
ferrum/
  Cargo.toml
  AGENTS.md
  docs/
    spec.md
    roadmap.md
  src/
    main.rs
    cli.rs
    config.rs
    agent/
      mod.rs
      loop.rs
      messages.rs
      tools.rs
    providers/
      mod.rs
      anthropic.rs
      openai.rs
    tools/
      mod.rs
      read.rs
      write.rs
      edit.rs
      bash.rs
      grep.rs
      find.rs
      ls.rs
    session/
      mod.rs
      jsonl.rs
      manager.rs
    tui/
      mod.rs
      app.rs
      editor.rs
      render.rs
```

## First Milestone
A minimal print-mode agent that can:

1. Read config/API key.
2. Send a prompt to Anthropic.
3. Stream text output.
4. Expose `read`, `ls`, `bash`, `write`, `edit`, `grep`, and `find` tools.
5. Execute tool calls and continue the loop.
6. Save a JSONL session.

## Quality Bar
- Unit tests for message conversion, edit tool, session JSONL, and provider stream parsing.
- Integration test with fake provider before real API tests.
- No secret values in logs or tests.
- Clear error messages.
