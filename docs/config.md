# Configuration

Ferrum reads config from:

```text
~/.config/ferrum/config.toml
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

[providers.openai-codex]
type = "openai-codex"
base_url = "https://chatgpt.com/backend-api"
default_model = "gpt-5.5"

[providers.opencode-go]
type = "openai-compatible"
base_url = "https://opencode.ai/zen/go/v1"
api_key_env = "OPENCODE_API_KEY"
default_model = "kimi-k2.6"

[providers.minimax]
type = "openai-compatible"
base_url = "https://api.minimax.io/v1"
api_key_env = "MINIMAX_API_KEY"
default_model = "MiniMax-M2"

[[mcp.servers]]
name = "example"
command = "example-mcp-server"
args = []
enabled = true
```

### provider

The active provider name. In normal use this should match a key under `[providers]`.

Ferrum still accepts legacy built-in names for compatibility, but `/providers` lists only providers configured in `config.toml`.

### providers

Configured providers live under `[providers.<name>]`.

Fields:

- `type`: `openai-codex`, `openai-compatible`, or `fake`
- `base_url`: provider endpoint
- `api_key_env`: environment variable for `openai-compatible` providers
- `default_model`: model selected when `/provider <name>` switches to this provider, and used at startup when top-level `model` is omitted

### model

Provider-specific model id.

Examples:

```toml
model = "gpt-5.5"
model = "kimi-k2.6"
model = "gpt-4.1"
```

### max_context_tokens

Approximate context budget. Ferrum estimates tokens as:

```text
text characters / 4
```

Default:

```toml
max_context_tokens = 256000
```

Ferrum warns at 80% and compacts automatically at the limit.

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

Enable explicitly:

```bash
ferrum --mcp -p "debug this browser issue"
```

Interactive:

```text
/mcp
/mcp on
/mcp off
/mcp status
```

When MCP is off, Ferrum does not start configured MCP servers and does not expose MCP tool schemas to the model. Native tools remain available.

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
