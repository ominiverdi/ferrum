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
model = "gpt-5.3-codex"
max_context_tokens = 256000
thinking = "off"

[[mcp.servers]]
name = "example"
command = "example-mcp-server"
args = []
enabled = true
```

### provider

Supported values:

- `fake`
- `openai-codex`
- `openai`
- `openai-compatible`
- `opencode-go`
- `minimax`
- `llama`

### model

Provider-specific model id.

Examples:

```toml
model = "gpt-5.3-codex"
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
```

OpenAI-compatible:

```text
OPENAI_API_KEY
OPENAI_BASE_URL
```

OpenAI Codex:

```text
OPENAI_CODEX_BASE_URL
```

OpenCode Go:

```text
OPENCODE_API_KEY
OPENCODE_GO_BASE_URL
OPENCODE_GO_API_KEY_ENV
```

MiniMax:

```text
MINIMAX_API_KEY
MINIMAX_BASE_URL
```

`MINIMAX_BASE_URL` defaults to `https://api.minimax.io/v1`.

llama.cpp:

```text
LLAMA_API_KEY
LLAMA_BASE_URL
```

`LLAMA_BASE_URL` defaults to `http://localhost:8080/v1`.
