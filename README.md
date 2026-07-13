# Ferrum

Ferrum is a small Rust-native coding agent for Linux.

It provides a simple CLI, local file and shell tools, image input, JSONL sessions, AGENTS.md context loading, configurable OpenAI-compatible providers, ChatGPT/Codex OAuth, Agent Skills, and a minimal MCP stdio bridge.

Ferrum is inspired by Pi's agent-harness ideas, but it is a separate Rust project. It does not aim to support Pi extensions, packages, themes, or SDK compatibility.

Status: early MVP. Useful for real work, still evolving.

![Ferrum demo](docs/assets/ferrum-demo.gif)

Primary repository and binary releases: https://codeberg.org/ominiverdi/ferrum

GitHub mirror and backup binary releases: https://github.com/ominiverdi/ferrum

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
- Built-in tools: `read`, writable-root-bound `write`/`edit`, structurally parsed tiered `bash`/`wait`, `grep`, `find`, `ls`
- Model-facing session history tools: `history_search`, `history_read`
- Tool exposure control with `--tools` and config allow/deny lists
- Semantic UI color palette with `~/.config/ferrum/colors.toml` and `/colors auto|on|off`
- Interactive completion and hints for slash commands, selected command arguments, `/image` paths, and `/skill:` names

## Install

### Linux binary

Download the latest release asset from Codeberg.

```bash
curl -L https://codeberg.org/ominiverdi/ferrum/releases/download/v0.7.2/ferrum-v0.7.2-x86_64-unknown-linux-gnu.tar.gz | tar xz
sudo install -Dm755 ferrum-v0.7.2-x86_64-unknown-linux-gnu/ferrum /usr/local/bin/ferrum
sudo install -Dm644 ferrum-v0.7.2-x86_64-unknown-linux-gnu/docs/ferrum.1 /usr/local/share/man/man1/ferrum.1
sudo mandb 2>/dev/null || true
ferrum --help
man ferrum
```

Optional checksum verification:

```bash
curl -LO https://codeberg.org/ominiverdi/ferrum/releases/download/v0.7.2/ferrum-v0.7.2-x86_64-unknown-linux-gnu.tar.gz
curl -LO https://codeberg.org/ominiverdi/ferrum/releases/download/v0.7.2/ferrum-v0.7.2-x86_64-unknown-linux-gnu.tar.gz.sha256
sha256sum -c ferrum-v0.7.2-x86_64-unknown-linux-gnu.tar.gz.sha256
```

### From source

Install with Cargo:

```bash
git clone https://codeberg.org/ominiverdi/ferrum.git
cd ferrum
cargo install --locked --path .
ferrum --help
```

Install the local man page from a source checkout:

```bash
sudo install -Dm644 docs/ferrum.1 /usr/local/share/man/man1/ferrum.1
mandb 2>/dev/null || true
man ferrum
```


## Quick start

Run a one-shot prompt:

```bash
ferrum -p "summarize this repo"
```

Pipe input as the prompt:

```bash
echo "summarize this repo" | ferrum -p
```

Pipe input plus extra instruction:

```bash
cat src/main.rs | ferrum -p "review this file"
```

Attach an image:

```bash
ferrum --image ./screenshot.png -p "describe this image"
```

Limit exposed tools:

```bash
ferrum --tools read grep find -p "inspect this repo"
ferrum --no-tools -p "answer without tools"
```

Start an interactive session:

```bash
ferrum
```

Start the bounded official ACP v1 stdio baseline:

```bash
ferrum acp
```

See [`docs/acp.md`](docs/acp.md) for supported methods and current interoperability limits.

Resume the latest interactive session:

```bash
ferrum --resume
ferrum --continue
```

Use a named print-mode session for recurring jobs that need prior context:

```bash
ferrum --session port-audit --tools bash -p "compare current open ports with prior observations"
```

`--session NAME -p ...` creates `NAME.jsonl` on first use and resumes it on later runs. In interactive mode, `--session REF` opens an existing session by JSONL path or id prefix. See [`docs/sessions.md`](docs/sessions.md).

## Minimal config

Ferrum reads user configuration from `~/.config/ferrum/config.toml`. A project may add a restrictive `.ferrum/config.toml` for tool, root, skill, MCP, safety, and turn-limit policy; it cannot change providers or authentication. See [`docs/config.md`](docs/config.md).

An optional system prompt override can live at `~/.config/ferrum/system.md`.

```toml
provider = "openai-codex"
model = "gpt-5.5"
thinking = "off"
safety = "medium"
max_context_tokens = 256000

[tools]
allow = ["read", "grep", "find", "bash", "wait"]
deny = ["write", "edit"]
writable_roots = ["."]

[providers.openai-codex]
type = "openai-codex"
base_url = "https://chatgpt.com/backend-api"
default_model = "gpt-5.5"

[providers.example-openai-compatible]
type = "openai-compatible"
base_url = "https://example.com/v1"
api_key_env = "EXAMPLE_API_KEY"
default_model = "example-model"

[models."gpt-5.5-small-context"]
actual_model = "gpt-5.5"
max_context_tokens = 6000
```

Login for ChatGPT/Codex OAuth:

```bash
ferrum login openai
```

`ferrum login --help` lists accepted provider spellings. The same OAuth flow is available inside an interactive session as `/login openai`; `openai-codex` is accepted as an alias in both places.

If no provider is configured, Ferrum says explicitly that it is using the fake demo provider and points to `~/.config/ferrum/config.toml` instead of silently presenting fake output as a normal backend.

OpenAI-compatible providers use environment-backed keys. Do not put secret values in `config.toml`.

## Colors

Ferrum supports a small semantic UI color palette. Use `color = "auto"` in `config.toml` to colorize only on terminals, and override palette entries in `~/.config/ferrum/colors.toml`:

```toml
prompt = "DeepSkyBlue1"
tool = "bold LightSkyBlue3"
error = "OrangeRed1"
diff_added = "SpringGreen1"
diff_removed = "DeepPink1"
```

See [`docs/colors.md`](docs/colors.md) for all palette keys and supported color values. Ferrum accepts xterm 256-color table names such as `DeepSkyBlue1`, `Orange3`, and `SpringGreen1`.

Reusable palettes can live in `~/.config/ferrum/color-palettes/*.toml`. Ferrum ships 24 built-in palettes and writes them to that directory on first run if it does not exist. In interactive mode, `/palette` shows the current palette, `/palettes` lists palette files, and `/palette <name>` validates and applies one live.

## System prompt override

Ferrum has an embedded default system prompt. To fully override it, create:

```text
~/.config/ferrum/system.md
```

Ferrum reads this file when starting or resuming a session. If the file is absent, the embedded default is used.

Supported placeholders:

```text
{{ferrum_version}}
{{provider}}
{{model}}
{{provider_model}}
{{thinking}}
{{cwd}}
{{config_dir}}
{{max_context_tokens}}
{{max_tool_rounds}}
{{mcp_enabled}}
{{diff_mode}}
```

When `max_tool_rounds = 0`, `{{max_tool_rounds}}` renders as an annotated adaptive mode rather than a bare zero, so the prompt cannot mistake it for disabled tool use.

Do not put secrets in `system.md`. If you override the prompt, keep any runtime metadata and tool guidance you want Ferrum to preserve.

In active interactive turns, `Esc` aborts the current model/tool turn and returns to the prompt. `Ctrl-C` also aborts foreground tool execution such as `wait`.

## Interactive commands

```text
/help
/version
/session
/title [text]
/goal [text|clear]
/new
/sessions
/sessions pick
/sessions del
/sessions new
/model [name]
/models
/login <provider>
/usage [day|week|month]
/provider [name]
/providers
/mcp [on|off|status|list]
/thinking [off|minimal|low|medium|high|xhigh]
/diff [unified|compact|full|words|side_by_side]
/colors [auto|on|off]
/palette [name]
/palettes
/image <path>
/image-paste
/paste-image
/skills
/skill <name> [args]
/skill:<name> [args]
/compact
/quit
/exit
```

`/new` and `/sessions new` both start a fresh session.

`/goal` shows one session-scoped note, `/goal <text>` replaces it, and `/goal clear` removes it. The note is limited to 4096 bytes, persists with the session, and does not trigger model work.

Interactive mode also supports command completion and hints via Tab for slash commands, selected command arguments, `/palette`, `/skill:`, and `/image` paths. After `/models` succeeds, its provider model ids are available to `/model <Tab>` completion until the active provider changes.

Input beginning with `/` in column zero is always handled by Ferrum. Unknown slash commands are rejected locally instead of being sent to the model. Prefix slash-leading text with a space to send it as a model prompt; Ferrum removes that escape whitespace before sending it.

Shell shortcuts:

```text
!<cmd>                              run shell command and send output to model
!!<cmd>                             run shell command and print output only
! --timeout-seconds=600 <cmd>       select a 1-600 second foreground timeout
```

## Documentation

- Architecture/spec: [`docs/spec.md`](docs/spec.md)
- Configuration: [`docs/config.md`](docs/config.md)
- Colors: [`docs/colors.md`](docs/colors.md)
- Providers: [`docs/providers.md`](docs/providers.md)
- Tools: [`docs/tools.md`](docs/tools.md)
- Resource-boundary design: [`docs/resource-boundaries.md`](docs/resource-boundaries.md)
- Safety notes: [`docs/security.md`](docs/security.md) for execution tiers, writable roots, MCP caveats, host-filesystem risk, and higher-risk workflow guidance
- Sessions: [`docs/sessions.md`](docs/sessions.md)
- Usage accounting: [`docs/usage.md`](docs/usage.md)
- Context accounting and compaction boundaries: [`docs/context-accounting.md`](docs/context-accounting.md)
- Images: [`docs/images.md`](docs/images.md)
- MCP: [`docs/mcp.md`](docs/mcp.md) for stdio server configuration, tool-name namespacing, frame limits, and safety notes
- ACP v1 support and interoperability: [`docs/acp.md`](docs/acp.md)
- Zed External Agent setup: [`docs/zed.md`](docs/zed.md)
- Provider secrets investigation: [`docs/provider-secrets.md`](docs/provider-secrets.md)
- Skills: [`docs/skills.md`](docs/skills.md)
- Roadmap: [`docs/roadmap.md`](docs/roadmap.md)
- Benchmarks: [`docs/benchmarks.md`](docs/benchmarks.md)
- Release process: [`docs/release.md`](docs/release.md)
- Codeberg workflow: [`docs/codeberg.md`](docs/codeberg.md)

## Safety notes

- API keys are read from environment variables or provider OAuth storage.
- Tools run with your local user permissions.
- `bash`, `write`, and `edit` can mutate files. `low` grants broad current-user shell authority and bypasses writable roots; `medium` limits native and recognized shell mutations to configured writable roots; `high` rejects mutation.
- Ferrum has no model-grantable confirmation prompt. At `medium`, a denied path requires a user config change or an action outside Ferrum.
- Use `/safety low|medium|high` to choose structural shell execution policy for `bash`, `wait`, and shell shortcuts. It does not enable tools, and it is not a sandbox. See [`docs/security.md`](docs/security.md), [`docs/tool-authority.md`](docs/tool-authority.md), and [`docs/tools.md`](docs/tools.md#bash).
- Provider, MCP, tool, repository, session, image, and filename text is treated as untrusted terminal data and sanitized before rendering. Native inspection and image paths have bounded input/output, count, cancellation, or deadline contracts documented in [`docs/resource-boundaries.md`](docs/resource-boundaries.md).
- Use `--tools` and `[tools] allow`/`deny` to control which tools are exposed to the model.
- See [`docs/security.md`](docs/security.md) for security research notes and Ferrum's current posture.

## Development

```bash
cargo fmt --check
cargo test
cargo build --release
```

## License

MIT. See `LICENSE`.
