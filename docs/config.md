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
- `default_model`: model selected when `/provider <name>` switches to this provider

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
