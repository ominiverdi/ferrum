# Tools

Ferrum tools are provider-neutral. Providers only translate tool definitions and tool calls to/from their API format. Execution happens in the core agent loop.

Native tools are available by default, then narrowed by `--tools` and `[tools]` config policy. MCP stdio tools can be added through config and are exposed as `mcp__<server>__<tool>` when MCP is enabled and permitted by the active tool policy. Use `--no-mcp` or `/mcp off` to disable MCP tools for coding-only turns.

Interactive mode renders tool calls in a readable multiline format and prints a bounded preview of tool results. Full tool results remain in the model/session context unless the underlying tool output itself was bounded.

For providers that support streaming, Ferrum streams provider events live. If thinking is enabled and the provider returns displayable reasoning text, Ferrum streams that provider-supplied thinking before the assistant answer; it does not synthesize thinking or expose hidden chain-of-thought. Press `Esc` or `Ctrl-C` during an active interactive operation to immediately abort the current model request, retry delay, `/models` lookup, MCP call, foreground tool, or `!`/`!!` shell command and return to the prompt.

When a turn continues after tool execution, Ferrum prints a simple separator before the post-tool assistant response.

## Tool exposure policy

CLI:

```bash
ferrum --tools read grep find
ferrum --no-tools
```

Semantics:

```text
--tools omitted        => default available tools
--no-tools             => no tools exposed to the model
--tools read grep find => exactly those tools, subject to config policy
```

Config:

```toml
[tools]
allow = ["read", "grep", "find", "bash", "wait"]
deny = ["write", "edit"]
```

`allow` is optional. When present, it is the maximum allowed tool set. `deny` removes tools from the default or requested set. Ferrum fails before the model request if `--tools` requests an unknown, denied, or not-allowed tool.

Ferrum stores the resolved tool list in session metadata for visibility and audit. Resuming or switching sessions uses the current process/config tool policy, so newly added default tools appear automatically unless `--tools`, `--no-tools`, `[tools] allow`, or `[tools] deny` limits them.

Ferrum also exposes lightweight session-history tools by default:

- `history_search`: search the current session JSONL, including entries archived before compaction.
- `history_read`: read rendered session entries by JSONL line number.

These tools are model-facing only; there is no slash command for them. They are meant for cases where the model needs to recover prior details from the current conversation without keeping all old text in the active context window.

If a provider emits a call for a non-exposed tool, Ferrum returns a tool error such as `Tool 'write' is not available` instead of executing it.

Interactive shell shortcuts are separate from model tools: `!cmd` and `!!cmd` are user-invoked commands handled by Ferrum, not tools exposed to the model.

## history_search

Search the current session history. This includes active messages and messages archived before compaction.

Input:

```json
{
  "query": "OUT_OF_MEMORY",
  "literal": true,
  "ignore_case": true,
  "limit": 10
}
```

Notes:

- `query` is required.
- `literal` defaults to `true`; set it to `false` to treat `query` as a regular expression.
- `ignore_case` defaults to `true`.
- `limit` defaults to `10` and is capped at `50`.
- Output includes JSONL line numbers, active/archived status, role, and a snippet.
- Search is scoped to the current session file only, not all sessions.

Example output:

```text
line 133 archived tool: sacct says OUT_OF_MEMORY on node n004
line 188 active assistant: likely memory request issue...
```

## history_read

Read rendered current-session history entries by JSONL line number. Use this after `history_search` when surrounding context is needed.

Input:

```json
{
  "offset": 120,
  "limit": 30
}
```

Notes:

- `offset` is a 1-based JSONL line number.
- `limit` defaults to `20` and is capped at `100`.
- Output is rendered as role/content text, not raw JSONL.
- Search/read line numbers are stable for the session file.

## read

Read a text file.

Input:

```json
{
  "path": "src/main.rs",
  "offset": 1,
  "limit": 100
}
```

Notes:

- `offset` is 1-based.
- Output is bounded.

## write

Create or overwrite a file under a configured writable root. Creates parent directories. `[tools].writable_roots` defaults to the working directory.

```json
{
  "path": "notes/example.txt",
  "content": "hello\n"
}
```

## edit

Exact text replacement under a configured writable root.

```json
{
  "path": "src/main.rs",
  "edits": [
    {
      "old_text": "old",
      "new_text": "new"
    }
  ]
}
```

Interactive output renders `edit` calls with the configured diff mode. This is display-only; edit matching and application semantics are unchanged.

Color highlighting is controlled separately with `/colors` and `color = "auto|on|off"`. Ferrum colors only its own `edit` diff rendering, not arbitrary tool output.

### Edit diff display modes

`/diff` shows the current mode. `/diff <mode>` changes it for the current session. `/colors` shows the current color mode. `/colors <auto|on|off>` changes it for the current session.

Modes:

- `unified`: patch-style line diff with normal context. Color highlights removals, additions, and hunk headers.
- `compact`: patch-style line diff with less context. Same coloring as unified.
- `full`: full old/new replacement blocks, with old content colored red and new content colored green.
- `words`: word-level changes marked as `[-removed-]` and `{+added+}`, with removals colored red and additions colored green.
- `side_by_side`: old and new text in two columns, with changed left rows colored red and changed right rows colored green.

Aliases for `side_by_side`:

```text
side
split
side-by-side
```

The selected mode is session-persistent and restored on resume or session switch. It only affects display for `edit` tool calls.

Rules:

- `old_text` must not be empty.
- Each `old_text` must match exactly once.
- Multiple edits must not overlap.
- Edits are matched against the original file, not incrementally.
- UTF-8 BOM and LF/CRLF line endings are preserved.

## bash

Run a focused bash command in cwd with timeout. Before execution, Ferrum parses the complete Bash input with a real syntax grammar and applies the selected execution tier plus configured writable roots. Parse errors, dynamic executable positions, unsupported authority forms, and ambiguous wrappers fail closed. Here-document bodies are data, not command text.

The policy recursively inspects supported wrappers such as `env`, `command`, `nice`, and `timeout`; rejects shell interpreter relaunch, dynamic executables, process substitution, `xargs`, dangerous normalized/globbed operands, and sensitive-path writes; and limits recognized mutations to `[tools].writable_roots`. At `high`, a conservative inspection command set replaces the normal development policy.

Allowed executables still run with the user's host authority. In particular, build tools and tests can execute checkout code at `low` and `medium`. This is not a sandbox; see [tool-authority.md](tool-authority.md).

```json
{
  "command": "cargo test",
  "timeout_seconds": 120
}
```

Output includes status, timeout flag, stdout, and stderr. Large output is truncated to a bounded tail. When output is truncated, Ferrum saves the full stream to a temporary file and includes its path in the result.

`bash` runs with stdin closed, stdout/stderr piped, and its own process group. On timeout or abort, Ferrum terminates the whole process group so child processes such as `ssh` or `cloudflared` do not keep consuming the terminal.

For broad filesystem exploration, prefer the native `find`, `grep`, and `ls` tools. If shell `find`/`grep` is necessary, prune noisy directories such as `.git`, `target`, and `node_modules`.

## wait

Wait in the foreground, then run a bash command in cwd using the same execution tier, writable roots, and command cleanup path as `bash`.

```json
{
  "seconds": 240,
  "command": "date",
  "timeout_seconds": 30
}
```

`seconds` is capped at 30 minutes. `timeout_seconds` has the same cap as `bash`. Interactive mode shows a lightweight progress line during the wait. Press `Esc` or `Ctrl-C` to abort the wait or the command and return to the prompt.

`wait` is foreground-only. It blocks the current Ferrum session while waiting and running, but the result is persisted like any other tool result. It is exposed only when `bash` is available, because it is delayed bash rather than a separate execution capability.

## grep

Search file contents under a path, including hidden config directories while skipping noisy dependency/build directories.

```json
{
  "pattern": "OpenAiCodexProvider",
  "path": "src",
  "glob": "**/*.rs",
  "ignore_case": false,
  "literal": false,
  "context": 2,
  "limit": 100
}
```

Supports optional glob filtering, case-insensitive search, literal matching, context lines, and match limits. Uses `rg` if available, with a Rust fallback that preserves regex-vs-literal semantics.

## find

Find files by glob pattern and/or legacy filename substring/extension filters.

Pi-like glob search:

```json
{
  "path": ".",
  "pattern": "**/*.service",
  "limit": 1000
}
```

Legacy filters are still supported:

```json
{
  "path": "src",
  "name": "openai",
  "extension": "rs"
}
```

Searches hidden config directories, respects `.gitignore`/ignore files, returns paths relative to the search root, and skips noisy dependency/build directories such as `.git`, `target`, and `node_modules`.

## ls

List directory contents, including dotfiles. Directories have a `/` suffix and entries are sorted case-insensitively.

```json
{
  "path": ".",
  "limit": 500
}
```

## Tool loop behavior

Ferrum defaults to an adaptive loop guard instead of a low fixed tool-round cap. It lets normal long tasks continue while watching for pathological behavior:

- repeated identical tool calls
- many consecutive tool errors
- an internal hard safety limit

When the guard sees suspicious behavior, Ferrum first injects a corrective system nudge. If the behavior continues, Ferrum makes one final no-tools model call asking the assistant to summarize findings and next steps. Guard events are printed to stderr as `[loop-guard] ...`.

Set `max_tool_rounds` to a positive value to restore an explicit fixed cap for debugging or benchmarks.

## Parallel tool execution

Ferrum runs safe read-only built-in tool batches in parallel when all tool calls in the model's batch are one of:

- `read`
- `ls`
- `grep`
- `find`

Results are appended in the original model-requested order. Mixed or mutating batches stay sequential, including:

- `bash`
- `wait`
- `write`
- `edit`
- MCP tools

## Safety

- Tools run with local user permissions.
- `write`, `edit`, `bash`, and `wait` can mutate files.
- `write`, `edit`, and recognized shell mutations are limited to user-configured writable roots.
- Ferrum has no model-grantable confirmation prompt. A denied root requires the user to change trusted config or perform the action outside Ferrum.
- Use `--tools` and `[tools] allow`/`deny` to control which tools are exposed to the model.
- Secrets must not be written, printed, logged, or committed.
