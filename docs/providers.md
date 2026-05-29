# Providers

## OpenAI Codex / ChatGPT subscription

Authentication uses OAuth and stores credentials in:

```text
~/.config/ferrum/auth.json
```

The auth file is created with user-only write permissions where possible.

Login:

```bash
ferrum login openai
```

Config:

```toml
provider = "openai-codex"
model = "gpt-5.3-codex"

[providers.openai-codex]
type = "openai-codex"
base_url = "https://chatgpt.com/backend-api"
default_model = "gpt-5.5"
```

Endpoint default:

```text
https://chatgpt.com/backend-api
```

Override:

```bash
export OPENAI_CODEX_BASE_URL=...
```

`/models` uses the live Codex catalog endpoint:

```text
GET https://chatgpt.com/backend-api/codex/models?client_version=<version>
```

The compatibility version defaults to the current tested Codex CLI version and can be overridden:

```bash
export FERRUM_CODEX_CLIENT_VERSION=0.135.0
```

## OpenCode Go

OpenCode Go is OpenAI-compatible for these documented models:

- `glm-5.1`
- `glm-5`
- `kimi-k2.5`
- `kimi-k2.6`
- `deepseek-v4-pro`
- `deepseek-v4-flash`
- `mimo-v2.5`
- `mimo-v2.5-pro`

Config:

```toml
[providers.opencode-go]
type = "openai-compatible"
base_url = "https://opencode.ai/zen/go/v1"
api_key_env = "OPENCODE_API_KEY"
default_model = "kimi-k2.6"
```

Run:

```bash
export OPENCODE_API_KEY=...
ferrum --provider opencode-go --model kimi-k2.6 -p "hello"
```

Default base URL:

```text
https://opencode.ai/zen/go/v1
```

Some OpenCode Go models use Anthropic `/messages`; Ferrum does not support that adapter yet.

## OpenAI-compatible

```bash
export OPENAI_API_KEY=...
export OPENAI_BASE_URL=https://api.openai.com/v1
ferrum --provider openai --model gpt-4.1 -p "hello"
```

## MiniMax

Config:

```toml
[providers.minimax]
type = "openai-compatible"
base_url = "https://api.minimax.io/v1"
api_key_env = "MINIMAX_API_KEY"
default_model = "MiniMax-M2"
```

Ferrum reads a MiniMax API key from `MINIMAX_API_KEY`.

Default base URL:

```text
https://api.minimax.io/v1
```

Override with `MINIMAX_BASE_URL` if needed.

```bash
export MINIMAX_API_KEY=...
ferrum --provider minimax --model <model> -p "hello"
```

## Tool support

Tool calling is implemented through Ferrum's normalized tool loop.

Verified:

- OpenAI Codex Responses
- OpenAI-compatible Chat Completions via OpenCode Go

Providers that do not implement compatible tool calls may still answer text-only requests.
