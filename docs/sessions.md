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

Continue the latest session for the current directory:

```bash
ferrum --continue
```

Resume the latest session for the current directory:

```bash
ferrum --resume
```

Resume a specific session by path or id prefix:

```bash
ferrum --resume ~/.config/ferrum/sessions/<file>.jsonl
ferrum --session <id-prefix>
```

## Interactive session commands

```text
/session
/sessions
/sessions 2
/sessions pick
/sessions new
/compact
```

`/sessions` lists recent sessions for the current directory with bracket numbers. `/sessions 2` opens entry `[2]` from the last list. `/sessions pick` opens a lightweight numbered picker where entering a number opens that session and entering text filters the list. `/sessions new` starts a fresh session.

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
