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

The auth file is created with user-only write permissions where possible. Updates fail closed if existing storage is malformed, lock and reread the latest JSON before merging the Codex credential, write through a random private temporary file, sync it, atomically rename it, and sync the containing directory. Unrelated provider entries are preserved across concurrent updates.

Ferrum refreshes credentials before expiry. Refresh is single-flight across cooperating Ferrum processes, preserves the existing refresh token when the server omits a rotated token, and retries one HTTP 401 with a freshly resolved credential. OAuth HTTP requests and callback handling have deadlines and size limits. The callback listener ignores unrelated or wrong-state localhost requests while waiting for the valid state/code, and falls back from registered port 1455 to registered port 1457 if needed.

Login:

```bash
ferrum login openai
```

Run `ferrum login --help` to list supported login provider names. `openai-codex` is accepted as an alias. In an interactive session, `/login openai` runs the same OAuth flow; after it succeeds, `/provider openai-codex` selects the authenticated provider for that session.

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

Ferrum queries the latest stable Codex CLI release for each `/models` command and uses that version for model discovery. If release discovery fails, it uses the current tested Codex CLI version. If an automatically discovered version receives a typed client-version compatibility rejection, Ferrum retries model discovery with the tested fallback. Set `FERRUM_CODEX_CLIENT_VERSION` to bypass release discovery and force a specific compatibility version; explicit overrides are never silently replaced:

```bash
export FERRUM_CODEX_CLIENT_VERSION=0.144.0
```

Ferrum applies a 60-second initial-response deadline, bounded idle and total deadlines while collecting non-streaming/status bodies, and a 90-second idle deadline between streaming chunks. Streaming has no total duration limit while chunks continue arriving.

Codex retries rejected requests for HTTP 408, 429, and 5xx responses and retries connection failures up to three times. `Retry-After` is honored up to 60 seconds. Each retry prints its reason, delay, and retry count; exhaustion prints an explicit final summary with retries and total attempts. Initial-response timeouts and failures after response streaming begins are not retried because the provider may already be processing the request or may have emitted partial visible output.

Provider bodies, SSE lines/events, aggregate response bytes, output text, reasoning, tool calls, and tool arguments are bounded. Ferrum incrementally decodes UTF-8, accepts standard SSE field syntax, stops reading at the protocol terminal event (`response.completed` for Codex or `[DONE]` for OpenAI-compatible chat streams), and rejects malformed or prematurely closed streams instead of turning them into empty replies.

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
# Required only to send credentials to a non-loopback http:// URL:
# allow_insecure_http = true
```

`streaming` and `stream_usage` default to `true` for OpenAI-compatible providers. If a provider rejects OpenAI's `stream_options.include_usage`, Ferrum retries the rejected request once without usage options and records estimated usage when provider usage is absent. Set `stream_usage = false` for providers known to reject usage-in-streaming options, to skip that compatibility retry. Set `streaming = false` for providers with incompatible streaming responses.

Ferrum allows cleartext `http://` for loopback providers and authless endpoints. An authenticated non-loopback `http://` provider is rejected by default because it would expose the bearer credential in transit. Prefer HTTPS. If a trusted deployment explicitly requires authenticated remote cleartext HTTP, set `allow_insecure_http = true` in that provider entry.

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

The picker combines this live result with configured aliases scoped to the selected provider. An alias without `provider` appears when its `actual_model` is present in the live result. `/providers` exposes the complete set of aliases without `provider` under the `providerless` entry.

Ferrum does not silently guess static model lists.

## Tool support

Tool calling is implemented through Ferrum's normalized tool loop. Providers only translate tool definitions and tool calls.

Verified:

- OpenAI Codex Responses
- OpenAI-compatible Chat Completions providers that implement OpenAI-style tools

Providers that do not implement compatible tool calls may still answer text-only requests.
