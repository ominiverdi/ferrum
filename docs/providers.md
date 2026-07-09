# Providers

Ferrum supports two provider families today:

- OpenAI Codex / ChatGPT through OAuth and the Codex Responses backend.
- OpenAI-compatible Chat Completions providers for remote APIs and local servers.

Provider entries are configured in `~/.config/ferrum/config.toml` under `[providers.<name>]`. Provider names are arbitrary config keys; Ferrum does not hardcode vendor-specific aliases. API-key providers should reference an environment variable with `api_key_env`; do not put secret values in config.

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
model = "gpt-5.5"

[providers.openai-codex]
type = "openai-codex"
base_url = "https://chatgpt.com/backend-api"
default_model = "gpt-5.5"
```

`/models` uses the live Codex catalog endpoint:

```text
GET https://chatgpt.com/backend-api/codex/models?client_version=<version>
```

Ferrum queries the latest stable Codex CLI release for each `/models` command and uses that version for model discovery. If the release lookup or model request fails, it falls back to the current tested Codex CLI version. Set `FERRUM_CODEX_CLIENT_VERSION` to bypass release discovery and force a specific compatibility version:

```bash
export FERRUM_CODEX_CLIENT_VERSION=0.144.0
```

You can override the base URL with:

```bash
export OPENAI_CODEX_BASE_URL=...
```

That environment variable is for legacy shorthand provider resolution. Prefer explicit `[providers.openai-codex]` config for normal use.

## OpenAI-compatible providers

Use `type = "openai-compatible"` for providers that expose an OpenAI Chat Completions-compatible `/chat/completions` API.

Config shape:

```toml
provider = "example"

[providers.example]
type = "openai-compatible"
base_url = "https://example.com/v1"
api_key_env = "EXAMPLE_API_KEY"
default_model = "example-model"
streaming = true
stream_usage = true
```

`streaming` and `stream_usage` default to `true` for OpenAI-compatible providers. If a provider rejects OpenAI's `stream_options.include_usage`, Ferrum retries the request once without usage options and records estimated usage when provider usage is absent. Set `stream_usage = false` for providers known to reject usage-in-streaming options, to skip that retry path. Set `streaming = false` for providers with incompatible streaming responses.

If top-level `model` is omitted, Ferrum uses the selected provider's `default_model`. A top-level `model` still takes precedence.

Run:

```bash
export EXAMPLE_API_KEY=...
ferrum -p "hello"
```

Examples include user-defined presets for OpenCode Go, MiniMax, OpenAI-compatible proxies, LM Studio, vLLM, llama.cpp, and Ollama-compatible `/v1` servers.

### Local llama.cpp example

```toml
provider = "llama-local"

[providers.llama-local]
type = "openai-compatible"
base_url = "http://localhost:8080/v1"
api_key_env = "LLAMA_API_KEY"
default_model = "gemma-4-E4B-it-Q8_0"
```

```bash
export LLAMA_API_KEY=dummy
ferrum -p "hello"
```

The exact model id must match the model exposed by your local server.


### Authless local inference example

```toml
[providers.local-infer]
type = "openai-compatible"
base_url = "http://localhost:8192"
default_model = "qwen3.6-27b"
```

```bash
ferrum --provider local-infer --model qwen3.6-27b -p "hello"
```

This is useful for local OpenAI-compatible servers that do not require authentication.

### Generic OpenAI-compatible example

```toml
[providers.openai-compat-example]
type = "openai-compatible"
base_url = "https://example.com/v1"
api_key_env = "EXAMPLE_API_KEY"
default_model = "example-model"
```

```bash
export EXAMPLE_API_KEY=...
ferrum --provider openai-compat-example --model example-model -p "hello"
```

### Config-defined vendor preset example

```toml
[providers.minimax]
type = "openai-compatible"
base_url = "https://api.minimax.io/v1"
api_key_env = "MINIMAX_API_KEY"
default_model = "MiniMax-M2"
```

```bash
export MINIMAX_API_KEY=...
ferrum --provider minimax --model MiniMax-M2 -p "hello"
```

For authless local servers, omit `api_key_env`.

## Live model listing

`/models` queries the selected provider live where supported:

- OpenAI Codex: `GET /codex/models?client_version=<version>`.
- OpenAI-compatible providers: `GET /models`.
- Fake provider: local `fake` model.

Ferrum does not silently guess static model lists.

## Tool support

Tool calling is implemented through Ferrum's normalized tool loop. Providers only translate tool definitions and tool calls.

Verified:

- OpenAI Codex Responses
- OpenAI-compatible Chat Completions providers that implement OpenAI-style tools

Providers that do not implement compatible tool calls may still answer text-only requests.
