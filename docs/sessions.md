# Sessions

Ferrum stores sessions as JSONL under:

```text
~/.config/ferrum/sessions/
```

Each line is a JSON object.

Entry types:

- `header`
- `message`
- `metadata`
- `compaction`

Sessions are append-oriented and human-inspectable.

## Resume

Continue the latest session for the current directory in interactive mode:

```bash
ferrum --continue
```

Resume the latest session for the current directory in interactive mode:

```bash
ferrum --resume
```

Print mode currently starts a fresh session even if resume flags are present.

Resume a specific session by path or id prefix:

```bash
ferrum --resume ~/.config/ferrum/sessions/<file>.jsonl
ferrum --session <id-prefix>
```

## Interactive session commands

```text
/session
/title [text]
/sessions
/sessions 2
/sessions pick
/sessions new
/compact
```

`/sessions` lists recent sessions for the current directory with bracket numbers. `/sessions 2` opens entry `[2]` from the last list. `/sessions pick` opens a lightweight numbered picker where entering a number opens that session and entering text filters the list. `/sessions new` starts a fresh session.

Empty interactive sessions are removed automatically when you quit, switch sessions, or start a new session. A session is kept once it contains at least one message beyond the header. `/sessions` hides old empty sessions by default, while still showing the current empty session so you can see where you are.

`/title` shows the current session title. `/title <text>` sets an explicit title used by `/sessions`. If no title is set, Ferrum falls back to a title inferred from the first user message.

Thinking level is stored in session metadata. New sessions record the current thinking level, and `/thinking <level>` appends an updated level. Resuming or switching sessions restores the session thinking level unless the process was started with an explicit `--thinking` override. Provider-supplied thinking content and replay signatures are stored in message history when the provider sends them.

Diff mode is also stored in session metadata. New sessions record the current `diff_mode`, and `/diff <mode>` appends an updated mode. Resuming or switching sessions restores that session's edit diff rendering mode.

The resolved tool list is stored in session metadata. Resuming or switching sessions restores that session's tool list unless the process was started with an explicit `--tools` override.

`/session` shows:

- path
- message count
- character count
- estimated tokens
- max context tokens
- context usage percent
- file size
- model
- provider model, when different from model
- provider
- thinking

## Size tracking

Ferrum estimates tokens as:

```text
text characters / 4
```

This is approximate but useful enough for compaction thresholds.

Default fallback max context:

```toml
max_context_tokens = 256000
```

Model aliases can override the fallback budget:

```toml
[models."gpt-5.5-small-context"]
actual_model = "gpt-5.5"
max_context_tokens = 6000
```

Ferrum warns as context usage rises: 75-84% every 5%, 85-91% every 3%, and 92-94% every 1%. It compacts automatically at 95% so the compaction request has headroom.

## Compaction

`/compact` summarizes older conversation with the current provider model, keeps recent context, and stores a `compaction` entry. The summary is loaded back as system context when the session is resumed.

Manual compaction is skipped when there is nothing old enough to summarize or when the resulting context would not be smaller. Automatic compaction can force a fallback local summary if model-generated compaction fails while the session is over budget. Ferrum avoids retaining orphan tool results whose matching tool calls were summarized away.
