# Usage accounting

Ferrum records model token usage in `usage.jsonl` under the Ferrum data directory, normally:

```text
~/.local/share/ferrum/usage.jsonl
```

The interactive command is:

```text
/usage
/usage day
/usage week
/usage month
```

Periods mean:

- `day`: last 24 hours
- `week`: last 7 days
- `month`: last 30 days

The usage table is grouped by provider and model.

```text
provider/model                    req exact/est      input     output   cached      total
openai-codex/gpt-5.5                1       0/1     10_306      2_159        0     12_465
```

Columns:

- `req`: completed model responses recorded by Ferrum
- `exact/est`: provider-reported records / Ferrum-estimated records
- `input`: input tokens
- `output`: output tokens
- `cached`: cached input tokens reported by providers when available
- `total`: total tokens reported by the provider, or Ferrum's normalized input/output total for estimated records

## Exact vs estimated usage

Ferrum prefers provider-reported usage when providers return it. Those records count as `exact`.

When a provider does not return usage, Ferrum records an estimated usage entry instead. Those records count as `est`.

Estimated usage is based on Ferrum's local request/response size estimate. It is useful for trend tracking, but it is not a billing-grade source of truth.

Provider-reported totals can differ from `input + output + cached` depending on the provider's accounting schema. Ferrum shows cached input separately so cache behavior is visible instead of folding it into another column.

## Streaming usage

OpenAI-compatible streaming can request provider usage with:

```toml
[providers.example]
type = "openai-compatible"
streaming = true
stream_usage = true
```

`stream_usage` defaults to `true` for OpenAI-compatible providers. If a provider rejects `stream_options.include_usage`, Ferrum retries that request once without streaming usage options and records estimated usage if the provider still omits usage. Set:

```toml
stream_usage = false
```

for providers known to reject usage-in-streaming options, to skip the retry path.

OpenAI Codex streaming usage is parsed from completed response events when the Codex backend includes a `usage` object. If the backend omits usage, Ferrum records an estimated entry.

## Context accounting

Ferrum also uses usage information for context pressure when available.

The `/session` command reports:

```text
context_tokens: 13864
context_source: usage+estimate
```

`context_source` values:

- `usage+estimate`: last post-compaction assistant usage plus estimates for messages after that response
- `estimate`: no usage-bearing assistant response is available, so Ferrum used local estimates only
- `estimate_after_compaction`: the latest available provider usage is before the latest compaction boundary, so Ferrum used local estimates for current context pressure

This is intentionally separate from `/usage` totals:

- `/usage` tracks completed model requests for cost/accounting trends
- `/session` tracks the current active context size for compaction pressure

These numbers do not need to match exactly.

See [`context-accounting.md`](context-accounting.md) for the design note on how compaction boundaries affect current-context token accounting.
