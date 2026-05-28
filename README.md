# Ferrum

Ferrum is a small Rust-native coding agent for Linux.

It provides a simple CLI, local file and shell tools, JSONL sessions, AGENTS.md context loading, OpenAI-compatible providers, ChatGPT/Codex OAuth, and a minimal MCP stdio bridge.

Ferrum is inspired by Pi's agent-harness ideas, but it is a separate Rust project. It does not aim to support Pi extensions, packages, themes, or SDK compatibility.

Status: early MVP. Useful for real work, still evolving.

## Features

- Linux-native CLI
- Print mode and interactive mode
- JSONL sessions with resume
- AGENTS.md context loading
- Configurable context budget and thinking level
- Minimal MCP stdio tool bridge
- OpenAI Codex / ChatGPT OAuth provider
- OpenAI-compatible Chat Completions provider
- OpenCode Go preset
- Minimax OpenAI-compatible preset
- Built-in tools:
  - `read`
  - `write`
  - `edit`
  - `bash`
  - `grep`
  - `find`
  - `ls`

## Install

### Linux binary

Download the latest release asset from GitHub:

```bash
curl -L https://github.com/ominiverdi/ferrum/releases/download/v0.2.1/ferrum-v0.2.1-x86_64-unknown-linux-gnu.tar.gz | tar xz
sudo mv ferrum-v0.2.1-x86_64-unknown-linux-gnu/ferrum /usr/local/bin/
ferrum --help
```

Optional checksum verification:

```bash
curl -LO https://github.com/ominiverdi/ferrum/releases/download/v0.2.1/ferrum-v0.2.1-x86_64-unknown-linux-gnu.tar.gz
curl -LO https://github.com/ominiverdi/ferrum/releases/download/v0.2.1/ferrum-v0.2.1-x86_64-unknown-linux-gnu.tar.gz.sha256
sha256sum -c ferrum-v0.2.1-x86_64-unknown-linux-gnu.tar.gz.sha256
```

### From source

```bash
git clone https://github.com/ominiverdi/ferrum.git
cd ferrum
cargo install --path .
ferrum --help
```

## Quick start

Run a one-shot prompt:

```bash
ferrum -p "summarize this repo"
```

Pipe input:

```bash
cat src/main.rs | ferrum -p "review this file"
```

Start an interactive session:

```bash
ferrum
```

Resume the latest session:

```bash
ferrum --resume
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

[[mcp.servers]]
name = "filesystem"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/me/projects"]
enabled = true
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

Set an API key:

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

Optional overrides:

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

## Interactive commands

```text
/help
/session
/model [name]
/provider [name]
/thinking [level]
/compact
/quit
```

## MCP

Ferrum supports local MCP stdio servers configured in `config.toml`.

Example:

```toml
[[mcp.servers]]
name = "filesystem"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/me/projects"]
enabled = true
```

Discovered MCP tools are exposed as:

```text
mcp__<server>__<tool>
```

See `docs/mcp.md` for details.

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

Use `/session` to view the current session path, message count, approximate token count, and file size.

## Safety notes

- API keys are read from environment variables or provider OAuth storage.
- Tools run with your local user permissions.
- `bash`, `write`, and `edit` can mutate files.
- Print mode does not ask for mutation confirmations.

## Development

```bash
cargo fmt --check
cargo test
cargo build --release
```

Docs:

- `docs/spec.md`
- `docs/roadmap.md`
- `docs/providers.md`
- `docs/config.md`
- `docs/tools.md`
- `docs/sessions.md`
- `docs/mcp.md`

## License

MIT. See `LICENSE`.
