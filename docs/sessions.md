# Sessions

Ferrum stores sessions as JSONL under:

```text
~/.config/ferrum/sessions/
```

Each line is a JSON object.

Entry types:

- `header`
- `message`
- `compaction`

Sessions are append-oriented and human-inspectable.

## Resume

Resume latest session:

```bash
ferrum --resume
```

Resume a specific session:

```bash
ferrum --resume ~/.config/ferrum/sessions/<file>.jsonl
```

## Interactive session commands

```text
/session
/compact
```

`/session` shows:

- path
- message count
- character count
- estimated tokens
- max context tokens
- file size
- model
- provider

## Size tracking

Ferrum estimates tokens as:

```text
text characters / 4
```

This is approximate but useful enough for compaction thresholds.

Default max context:

```toml
max_context_tokens = 256000
```

Ferrum warns at 80% and compacts automatically at the configured limit.

## Compaction

`/compact` summarizes older conversation with the current provider/model, keeps recent context, and stores a `compaction` entry. The summary is loaded back as system context when the session is resumed.

Manual compaction is skipped when there is nothing old enough to summarize or when the resulting context would not be smaller. Automatic compaction can force a fallback local summary if model-generated compaction fails while the session is over budget.
