# Configuration

Ferrum reads config from:

```text
~/.config/ferrum/config.toml
```

Ferrum stores runtime data under:

```text
~/.local/share/ferrum/sessions/
~/.local/share/ferrum/history.txt
```

Optional system prompt override:

```text
~/.config/ferrum/system.md
```

Override config directory:

```bash
export FERRUM_CONFIG_DIR=/path/to/config
```

## Keys

```toml
provider = "openai-codex"
model = "gpt-5.5"
max_context_tokens = 256000
max_tool_rounds = 0
thinking = "off"
mcp_enabled = true
diff_mode = "unified"

[tools]
allow = ["read", "grep", "find", "bash"]
deny = ["write", "edit"]

[providers.openai-codex]
type = "openai-codex"
base_url = "https://chatgpt.com/backend-api"
default_model = "gpt-5.5"

[providers.local-infer]
type = "openai-compatible"
base_url = "http://localhost:8192"
default_model = "qwen3.6-27b"

[providers.example-compat]
type = "openai-compatible"
base_url = "https://example.com/v1"
api_key_env = "EXAMPLE_API_KEY"
default_model = "example-model"
streaming = true
stream_usage = true

[models."gpt-5.5-small-context"]
actual_model = "gpt-5.5"
max_context_tokens = 6000

[models.example-tuned]
provider = "example-compat"
actual_model = "example-model"
max_context_tokens = 100000

[[mcp.servers]]
name = "example"
command = "example-mcp-server"
args = []
enabled = true
```

### system.md

Ferrum has an embedded default system prompt. If `system.md` exists in the config directory, Ferrum uses it instead. This is a full override, not an appended instruction block.

```text
~/.config/ferrum/system.md
```

Ferrum reads the file when starting a new session or resuming/switching sessions, so the latest saved version is used for future runtime context.

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

Unknown placeholders are left unchanged. If the file exists but cannot be read, Ferrum fails clearly.

Do not put secrets in `system.md`. A custom system prompt controls Ferrum's behavior; omitting runtime metadata or tool guidance can degrade agent behavior.

### provider

The active provider name. In normal use this should match a key under `[providers]`.

Provider names like `local`, `minimax`, or `opencode-go` are just config keys. Ferrum does not hardcode vendor-specific provider aliases; define any provider preset you want in `config.toml`.

### providers

Configured providers live under `[providers.<name>]`.

The `<name>` is arbitrary user config. It can be a generic label like `local` or a vendor preset name like `minimax` if you define it in config.

Fields:

- `type`: `openai-codex`, `openai-compatible`, or `fake`
- `base_url`: provider endpoint
- `api_key_env`: optional environment variable for `openai-compatible` providers; when omitted, Ferrum sends no `Authorization` header
- `default_model`: model selected when `/provider <name>` switches to this provider, and used at startup when top-level `model` is omitted
- `streaming`: optional OpenAI-compatible streaming toggle; defaults to `true`
- `stream_usage`: optional `stream_options.include_usage` toggle for OpenAI-compatible streaming; defaults to `true`

Set `stream_usage = false` when an OpenAI-compatible provider supports streaming but rejects usage-in-streaming options. Ferrum will still record estimated usage when provider usage is absent.

### model

Selected Ferrum model name. This can be either a provider-specific model id or a configured model alias under `[models]`.

Examples:

```toml
model = "gpt-5.5"
model = "example-model"
model = "gpt-4.1"
model = "gpt-5.5-small-context"
```

### models

Optional model aliases live under `[models.<name>]`. Quote names that contain dots or hyphens:

```toml
[models."gpt-5.5-small-context"]
actual_model = "gpt-5.5"
max_context_tokens = 6000

[models.example-tuned]
provider = "example-compat"
actual_model = "example-model"
max_context_tokens = 100000
```

Fields:

- `provider`: optional provider switch when this alias is selected
- `actual_model`: model id sent to the provider; defaults to the alias name
- `max_context_tokens`: model-specific operating context budget

This lets each model or alias use a tuned context budget while preserving friendly names for interactive `/model` selection.

### max_context_tokens

`max_context_tokens` is the fallback operating context budget. A model alias can override it with `[models.<name>].max_context_tokens`.

Ferrum estimates tokens as:

```text
text characters / 4
```

Default:

```toml
max_context_tokens = 256000
```

Ferrum warns as context usage rises: 75-84% every 5%, 85-91% every 3%, and 92-94% every 1%. It compacts automatically at 95%, before the configured budget is fully exhausted. `/session` shows estimated tokens, max context tokens, and context usage percent.

### max_tool_rounds

Tool-loop safety mode.

Default:

```toml
max_tool_rounds = 0
```

`0` means adaptive loop guard: Ferrum does not stop normal long tasks at a low fixed round count. It nudges or stops only when behavior looks pathological, such as repeated identical tool calls or many consecutive tool errors. A hard emergency safety limit still applies internally.

Set a positive value to restore an explicit fixed round cap for debugging or benchmarks:

```bash
FERRUM_MAX_TOOL_ROUNDS=16 ferrum -p "finish this larger refactor"
```

### mcp_enabled

Global MCP switch.

Default:

```toml
mcp_enabled = true
```

Disable MCP for a single process:

```bash
ferrum --no-mcp -p "fix this without external MCP tools"
```

Enable all configured MCP servers explicitly:

```bash
ferrum --mcp -p "debug this browser issue"
```

Enable only selected configured MCP servers:

```bash
ferrum --mcp chrome-devtools web-search -p "debug this browser issue"
```

Interactive:

```text
/mcp
/mcp on
/mcp off
/mcp status
```

When MCP is off, Ferrum does not start configured MCP servers and does not expose MCP tool schemas to the model. Native tools remain available.

### tools

Tool exposure can be narrowed per process:

```bash
ferrum --tools read grep find -p "inspect this repo"
ferrum --no-tools -p "answer without tools"
```

Semantics:

```text
--tools omitted       => default available tools
--no-tools            => no tools
--tools read grep find => exactly those tools, subject to config policy
```

Config policy:

```toml
[tools]
allow = ["read", "grep", "find", "bash"]
deny = ["write", "edit"]
```

`allow` is optional. When present, it is the maximum allowed tool set. `deny` removes tools from the default or requested set. If `--tools` requests an unknown, denied, or not-allowed tool, Ferrum fails before the model request.

Ferrum stores the resolved tool list in session metadata. Resuming or switching sessions restores that session's tool list unless the process was started with an explicit `--tools` override.

### Colors

Ferrum supports terminal color control for interactive diff rendering.

Config:

```toml
color = "auto"
```

Supported values:

```text
auto|on|off
```

Interactive:

```text
/colors
/colors on
/colors off
/colors auto
```

Semantics:
- `auto`: colorize only when stderr is a terminal
- `on`: force color output
- `off`: disable color output

Color mode is stored in session metadata and restored on resume or session switch.

### diff_mode

Controls how `edit` tool calls are rendered in interactive output. This is display-only and does not change edit semantics.

Supported values:

```text
unified|compact|full|words|side_by_side
```

Default:

```toml
diff_mode = "unified"
```

Interactive:

```text
/diff
/diff compact
/diff side_by_side
```

### thinking

Supported values:

```text
off|minimal|low|medium|high|xhigh
```

Default:

```toml
thinking = "off"
```

CLI override:

```bash
ferrum --thinking high -p "reason about this"
```

Interactive:

```text
/thinking high
```

Ferrum stores thinking level in session metadata. Resuming or switching sessions restores that session's thinking level unless the process was started with an explicit `--thinking` override.

For supported streaming providers, displayable provider-supplied thinking is shown live in interactive mode. Ferrum does not invent thinking text; no thinking block is shown when the provider sends none.

## MCP servers

Ferrum supports MCP stdio servers configured in TOML:

```toml
[[mcp.servers]]
name = "filesystem"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/ominiverdi/github"]
enabled = true
```

Only stdio MCP is supported initially. HTTP/SSE MCP is not implemented.

Discovered MCP tools are exposed with namespaced names:

```text
mcp__filesystem__read_file
```

## Environment variables

Core:

```text
FERRUM_CONFIG_DIR
FERRUM_OFFLINE
FERRUM_CODEX_CLIENT_VERSION
FERRUM_MAX_TOOL_ROUNDS
```

Development/testing:

```text
FERRUM_FAKE_SCRIPT
FERRUM_METRICS
```

`FERRUM_FAKE_SCRIPT` is only used with the fake provider for deterministic local harness tests. Current scripts: `repeat_read`, `missing_read`, and `mixed_write_read`.

`FERRUM_METRICS=1` prints per-request model/tool metrics to stderr, including message bytes, tool schema bytes, estimated payload tokens, model latency, tool latency, and result bytes.

OpenAI-compatible providers read API keys from the environment variable named by `api_key_env`.

Example:

```toml
[providers.example]
type = "openai-compatible"
base_url = "https://example.com/v1"
api_key_env = "EXAMPLE_API_KEY"
default_model = "example-model"
```

```bash
export EXAMPLE_API_KEY=...
```

Legacy shorthand provider names still support these environment variables:

```text
OPENAI_API_KEY
OPENAI_BASE_URL
OPENAI_CODEX_BASE_URL
OPENCODE_API_KEY
OPENCODE_GO_BASE_URL
OPENCODE_GO_API_KEY_ENV
MINIMAX_API_KEY
MINIMAX_BASE_URL
```
