# Ferrum

Ferrum is a small Rust-native Linux coding agent inspired by Pi's minimal harness model.
It is not a Pi port: no TypeScript runtime, no extension system, no npm compatibility, and no SDK compatibility.

Current status: early MVP. Useful, but not hardened.

## Features

- Linux CLI coding agent
- Print mode and basic interactive mode
- OpenAI Codex / ChatGPT OAuth provider
- OpenAI-compatible Chat Completions provider
- OpenCode Go preset
- Minimax OpenAI-compatible preset
- JSONL sessions with resume
- AGENTS.md context loading
- Built-in tools:
  - `read`
  - `write`
  - `edit`
  - `bash`
  - `grep`
  - `find`
  - `ls`

## Install

From source:

```bash
cargo install --path .
```

Then:

```bash
ferrum --help
```

## Configuration

Default config path:

```text
~/.config/ferrum/config.toml
```

Example:

```toml
provider = "openai-codex"
model = "gpt-5.3-codex"
max_context_tokens = 256000
thinking = "off"
```

Thinking levels:

```text
off|minimal|low|medium|high|xhigh
```

CLI overrides:

```bash
ferrum --provider opencode-go --model kimi-k2.6 --thinking minimal -p "hello"
```

## Providers

### OpenAI Codex / ChatGPT subscription

Login:

```bash
ferrum login openai
```

Config:

```toml
provider = "openai-codex"
model = "gpt-5.3-codex"
```

### OpenCode Go

Set your key outside the repo:

```bash
export OPENCODE_API_KEY=...
```

Run:

```bash
ferrum --provider opencode-go --model kimi-k2.6 -p "hello"
```

Default endpoint:

```text
https://opencode.ai/zen/go/v1
```

Override envs:

```bash
export OPENCODE_GO_BASE_URL=...
export OPENCODE_GO_API_KEY_ENV=MY_KEY_ENV_NAME
```

### OpenAI-compatible

```bash
export OPENAI_API_KEY=...
export OPENAI_BASE_URL=https://api.openai.com/v1
ferrum --provider openai --model gpt-4.1 -p "hello"
```

### Minimax

```bash
export MINIMAX_API_KEY=...
export MINIMAX_BASE_URL=...
ferrum --provider minimax --model <model> -p "hello"
```

## Usage

Print mode:

```bash
ferrum -p "summarize this repo"
cat src/main.rs | ferrum -p "review this file"
```

Interactive mode:

```bash
ferrum
```

Interactive commands:

```text
/help
/session
/model [name]
/provider [name]
/thinking [level]
/compact
/quit
```

Resume latest session:

```bash
ferrum --resume
```

Resume a specific session:

```bash
ferrum --resume ~/.config/ferrum/sessions/<file>.jsonl
```

## Context files

Ferrum loads `AGENTS.md` files in this order:

1. `~/.config/ferrum/AGENTS.md`
2. parent directories from filesystem root to cwd
3. cwd `AGENTS.md`

Files are deduplicated, bounded, and included in the system prompt.

## Sessions

Sessions are JSONL files under:

```text
~/.config/ferrum/sessions/
```

Use `/session` to view the current path and approximate context size.

## Safety notes

- Do not put secrets in this repo.
- API keys should come from environment variables or provider OAuth storage.
- Tool execution has your local user permissions.
- `bash`, `write`, and `edit` can mutate files.
- Print mode does not ask for mutation confirmations.

## Development

```bash
cargo test
cargo fmt
cargo run -- --help
```

Docs:

- `docs/spec.md`
- `docs/roadmap.md`
- `docs/providers.md`
- `docs/config.md`
- `docs/tools.md`
- `docs/sessions.md`

## License

MIT. See `LICENSE`.
