# Sessions

Ferrum stores sessions as JSONL under:

```text
~/.local/share/ferrum/sessions/
```

Each line is a JSON object.

Entry types:

- `header`
- `message`
- `metadata`
- `compaction`

Sessions are append-oriented and human-inspectable. New session files are created with user-private permissions (`0600`), and Ferrum tightens existing session file permissions on open when possible. Anonymous filenames include a timestamp plus UUID entropy.

Each JSONL append is serialized before locking, bounded to 16 MiB, written with its newline while holding an exclusive advisory file lock, flushed, and synced before Ferrum reports success. Readers take a shared lock and process bounded records incrementally. When a writer opens or appends to a session, it removes an incomplete trailing record under the exclusive lock before adding new data. Complete prior records remain intact.

## Resume and named sessions

Continue the latest session for the current directory in interactive mode:

```bash
ferrum --continue
```

Resume the latest session for the current directory in interactive mode:

```bash
ferrum --resume
```

Resume a specific existing session by JSONL path or id prefix:

```bash
ferrum --resume ~/.local/share/ferrum/sessions/<file>.jsonl
ferrum --session <id-prefix>
```

### Print-mode named sessions

Print mode can resume or create a named session with `--session`:

```bash
ferrum --session auth-scan -p "first prompt"
ferrum --session auth-scan -p "second prompt with prior context"
```

If `auth-scan` exists, Ferrum loads it before running the prompt. If it does not exist, Ferrum creates `auth-scan.jsonl` in the session data directory. User-defined session ids may contain only letters, digits, `.`, `_`, and `-`; they must not start with `.`.

This is useful for recurring jobs that need memory across runs:

```bash
ferrum --session port-audit --tools bash -p '
Run ss -tulpen.
Compare current listeners with prior observations in this session.
Report added or removed externally exposed ports.
'
```

`--session` behaves differently in interactive mode: it opens an existing session by path or id prefix. It does not create a new named session there. To create a named session for automation, use print mode once with `--session NAME -p ...`, or provide an explicit JSONL path.

### Cron notes

`--session NAME -p ...` is suitable for cron if the same Unix user runs each job and the session id is stable. Ferrum stores sessions under `$FERRUM_DATA_DIR/sessions` when `FERRUM_DATA_DIR` is set, otherwise `$XDG_DATA_HOME/ferrum/sessions`, otherwise `~/.local/share/ferrum/sessions`.

For system crontabs, set `FERRUM_CONFIG_DIR` and `FERRUM_DATA_DIR` explicitly or run Ferrum as the intended user. Otherwise `~` may resolve to root's home and the job may use different config, auth, and session storage than your interactive shell.

Set a title when starting or resuming a session:

```bash
ferrum --title "Issue triage"
ferrum --title "Quick check" -p "summarize this repo"
```

## Interactive session commands

```text
/session
/title [text]
/new
/sessions
/sessions pick
/sessions del
/sessions new
/compact
```

`/session` shows the current session status.

When an interactive session is resumed with `--resume`, `--continue`, or `--session REF`, Ferrum prints the last 40 visible conversation lines before prompting. This is UI-only: it does not add anything to model context and does not create a new model turn.

`/sessions` lists recent sessions for the current directory. `/sessions pick` opens a lightweight numbered picker where entering a number opens that session and entering text filters the list. `/sessions del` opens a deletion picker. `/new` and `/sessions new` both start a fresh session.

Header-only and metadata-only sessions are retained so a failed start or state transition never unlinks an active file handle. `/sessions` hides old empty sessions by default, while still showing the current empty session so you can see where you are. Automatic latest-session selection skips abandoned anonymous header-only sessions.

`/title` shows the current session title. `/title <text>` sets an explicit title used by `/sessions`. `--title <text>` sets the title when starting, resuming, or running a print-mode session. If no title is set, Ferrum falls back to a title inferred from the first user message.

Thinking level is stored in session metadata. New sessions record the current thinking level, and `/thinking <level>` appends an updated level. Resuming or switching sessions restores the session thinking level unless the process was started with an explicit `--thinking` override. Provider-supplied thinking content and replay signatures are stored in message history when the provider sends them.

Safety level is stored in session metadata. New sessions record the current safety level, and `/safety <level>` appends an updated level. Resuming restores the session safety level unless the process was started with an explicit `--safety` override.

Diff mode is also stored in session metadata. New sessions record the current `diff_mode`, and `/diff <mode>` appends an updated mode. Resuming or switching sessions restores that session's edit diff rendering mode.

Color mode is stored in session metadata too. `/colors <auto|on|off>` appends an updated color mode, and resuming or switching sessions restores it.

Interactive input supports completion and hints for slash commands, selected command arguments, `/skill:`, and `/image` paths.

The resolved tool list is stored in session metadata for visibility and audit. Resuming or switching sessions uses the current process/config tool policy, so newly added default tools appear automatically unless `--tools`, `--no-tools`, `[tools] allow`, or `[tools] deny` limits them.

Model-facing history tools, `history_search` and `history_read`, stream bounded current-session JSONL records by line number, including tool calls/results and entries archived before compaction. They stop when their result limit is met rather than loading the complete session into memory. They are tools only; there is no slash command for them. See [`tools.md`](tools.md).

## Durability checkpoints

A successfully returned session append is a durability checkpoint: the complete JSON record and newline have been flushed and synced. Session creation also syncs the containing session directory after its header is durable. Normal exit and session switching issue an additional sync checkpoint before dropping the old session. Filesystem or hardware behavior can still affect guarantees below the operating system's `fsync` contract.

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
- safety

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

Ferrum warns as context usage rises: 75-84% every 5%, 85-91% every 3%, and 92-94% every 1%. Before every provider request, including each tool-loop continuation and forced final synthesis, it projects the request from provider-informed usage, local message estimates, pending messages, and active tool definitions. Automatic compaction keeps a 16,384-token reserve on larger context windows and uses the 95% threshold on smaller windows. Ferrum refuses a request with a clear context-budget error if compaction cannot reduce it below the safe threshold.

## Compaction

`/compact` summarizes older conversation with the current provider model, keeps recent context, and stores a `compaction` entry. Retained recent messages are re-appended after that entry so resuming reconstructs the same active context. If mid-turn compaction summarizes earlier tool rounds, Ferrum retains the current user request explicitly.

Generated summaries are untrusted conversation data and are loaded with the user role, never as runtime authority. After every compaction and resume, Ferrum regenerates immutable runtime, repository-instruction, and skill-index system messages from current trusted process state. Only the newest generated summary remains active; older generated summaries and transient generated system messages are discarded.

Manual compaction is skipped when there is nothing old enough to summarize or when the resulting context would not be smaller. Automatic compaction can force a fallback local summary if model-generated compaction fails while the session is over budget. Ferrum avoids retaining orphan tool results whose matching tool calls were summarized away.

See [`context-accounting.md`](context-accounting.md) for the design note on compaction boundaries and stale provider usage.
