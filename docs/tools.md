# Tools

Ferrum tools are provider-neutral. Providers only translate tool definitions and tool calls to/from their API format. Execution happens in the core agent loop.

Native tools are always available. MCP stdio tools can be added through config and are exposed as `mcp__<server>__<tool>` when MCP is enabled. Use `--no-mcp` or `/mcp off` to disable MCP tools for coding-only turns.

Interactive mode renders tool calls in a readable multiline format and prints a bounded preview of tool results. Full tool results remain in the model/session context unless the underlying tool output itself was bounded.

For providers that support streaming, Ferrum streams provider events live. If thinking is enabled and the provider returns displayable reasoning text, Ferrum streams that provider-supplied thinking before the assistant answer; it does not synthesize thinking or expose hidden chain-of-thought. Press `Esc` during an active interactive turn to abort the current model/tool turn and return to the prompt.

When a turn continues after tool execution, Ferrum prints a simple separator before the post-tool assistant response.

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

Create or overwrite a file. Creates parent directories.

```json
{
  "path": "notes/example.txt",
  "content": "hello\n"
}
```

## edit

Exact text replacement.

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

Interactive output renders `edit` calls with the configured diff mode. Use `/diff` to view the current mode and `/diff unified|compact|full|words|side_by_side` to switch modes locally. This is display-only; edit matching and application semantics are unchanged.

Rules:

- `old_text` must not be empty.
- Each `old_text` must match exactly once.
- Multiple edits must not overlap.
- Edits are matched against the original file, not incrementally.
- UTF-8 BOM and LF/CRLF line endings are preserved.

## bash

Run a focused bash command in cwd with timeout.

```json
{
  "command": "cargo test",
  "timeout_seconds": 120
}
```

Output includes status, timeout flag, stdout, and stderr. Large output is truncated to a bounded tail. When output is truncated, Ferrum saves the full stream to a temporary file and includes its path in the result.

For broad filesystem exploration, prefer the native `find`, `grep`, and `ls` tools. If shell `find`/`grep` is necessary, prune noisy directories such as `.git`, `target`, and `node_modules`.

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

Supports optional glob filtering, case-insensitive search, literal matching, context lines, and match limits. Uses `rg` if available, with a Rust fallback.

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
- `write`
- `edit`
- MCP tools

## Safety

- Tools run with local user permissions.
- `write`, `edit`, and `bash` can mutate files.
- Ferrum currently has no per-tool confirmation prompts. Tool calls execute directly in both print and interactive mode.
- Secrets must not be written, printed, logged, or committed.
