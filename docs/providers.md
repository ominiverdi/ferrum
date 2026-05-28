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
```

Endpoint default:

```text
https://chatgpt.com/backend-api
```

Override:

```bash
export OPENAI_CODEX_BASE_URL=...
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

Config or CLI:

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
