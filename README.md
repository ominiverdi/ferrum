# Ferrum

Ferrum is a small Rust-native coding agent for Linux.

It provides a simple CLI, local file and shell tools, image input, JSONL sessions, AGENTS.md context loading, configurable OpenAI-compatible providers, ChatGPT/Codex OAuth, Agent Skills, and a minimal MCP stdio bridge.

Ferrum is inspired by Pi's agent-harness ideas, but it is a separate Rust project. It does not aim to support Pi extensions, packages, themes, or SDK compatibility.

Status: early MVP. Useful for real work, still evolving.

## Features

- Linux-native CLI
- Print mode and interactive mode with live streamed responses
- JSONL sessions with resume
- AGENTS.md context loading
- Configurable context budget and thinking level
- Provider-supplied thinking display for supported models
- Image input with optional terminal previews
- Agent Skills-style instruction packages
- Minimal MCP stdio tool bridge
- OpenAI Codex / ChatGPT OAuth provider
- OpenAI-compatible providers for remote APIs and local servers
- Config-backed provider registry
- Live model listing for supported providers
- Built-in tools: `read`, `write`, `edit`, `bash`, `grep`, `find`, `ls`

## Install

### Linux binary

Download the latest release asset from GitHub:

```bash
curl -L https://github.com/ominiverdi/ferrum/releases/download/v0.4.8/ferrum-v0.4.8-x86_64-unknown-linux-gnu.tar.gz | tar xz
sudo mv ferrum-v0.4.8-x86_64-unknown-linux-gnu/ferrum /usr/local/bin/
ferrum --help
```

Optional checksum verification:

```bash
curl -LO https://github.com/ominiverdi/ferrum/releases/download/v0.4.8/ferrum-v0.4.8-x86_64-unknown-linux-gnu.tar.gz
curl -LO https://github.com/ominiverdi/ferrum/releases/download/v0.4.8/ferrum-v0.4.8-x86_64-unknown-linux-gnu.tar.gz.sha256
sha256sum -c ferrum-v0.4.8-x86_64-unknown-linux-gnu.tar.gz.sha256
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

Attach an image:

```bash
ferrum --image ./screenshot.png -p "describe this image"
```

Start an interactive session:

```bash
ferrum
```

Resume the latest session:

```bash
ferrum --resume
ferrum --continue
```

## Minimal config

Ferrum reads config from `~/.config/ferrum/config.toml`.

```toml
provider = "llama-local"
model = "gemma-4-E4B-it-Q8_0"
thinking = "off"
max_context_tokens = 256000

[providers.llama-local]
type = "openai-compatible"
base_url = "http://localhost:8080/v1"
api_key_env = "LLAMA_API_KEY"
default_model = "gemma-4-E4B-it-Q8_0"

[providers.openai-codex]
type = "openai-codex"
base_url = "https://chatgpt.com/backend-api"
default_model = "gpt-5.5"

[providers.example-openai-compatible]
type = "openai-compatible"
base_url = "https://example.com/v1"
api_key_env = "EXAMPLE_API_KEY"
default_model = "example-model"
```

Login for ChatGPT/Codex OAuth:

```bash
ferrum login openai
```

OpenAI-compatible providers use environment-backed keys. Do not put secret values in `config.toml`.

In active interactive turns, `Esc` aborts the current model/tool turn and returns to the prompt.

## Interactive commands

```text
/help
/session
/title [text]
/sessions
/sessions 2
/sessions pick
/sessions new
/model [name]
/models
/provider [name]
/providers
/thinking [level]
/diff [unified|compact|full|words|side_by_side]
/image <path>
/paste-image
/skills
/skill:<name> [args]
/compact
/quit
```

Shell shortcuts:

```text
!<cmd>   run shell command and send output to model
!!<cmd>  run shell command and print output only
```

## Documentation

- Architecture/spec: [`docs/spec.md`](docs/spec.md)
- Configuration: [`docs/config.md`](docs/config.md)
- Providers: [`docs/providers.md`](docs/providers.md)
- Tools: [`docs/tools.md`](docs/tools.md)
- Sessions: [`docs/sessions.md`](docs/sessions.md)
- Images: [`docs/images.md`](docs/images.md)
- MCP: [`docs/mcp.md`](docs/mcp.md)
- Skills: [`docs/skills.md`](docs/skills.md)
- Roadmap: [`docs/roadmap.md`](docs/roadmap.md)
- Benchmarks: [`docs/benchmarks.md`](docs/benchmarks.md)
- Release process: [`docs/release.md`](docs/release.md)

## Safety notes

- API keys are read from environment variables or provider OAuth storage.
- Tools run with your local user permissions.
- `bash`, `write`, and `edit` can mutate files.
- Ferrum currently has no per-tool confirmation prompts. Tool calls execute directly in both print and interactive mode.

## Development

```bash
cargo fmt --check
cargo test
cargo build --release
```

## License

MIT. See `LICENSE`.
