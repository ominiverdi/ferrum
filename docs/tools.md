# Tools

Ferrum tools are provider-neutral. Providers only translate tool definitions and tool calls to/from their API format. Execution happens in the core agent loop.

Native tools are always available. MCP stdio tools can be added through config and are exposed as `mcp__<server>__<tool>`.

Interactive mode renders tool calls in a readable multiline format and prints a bounded preview of tool results. Full tool results remain in the model/session context unless the underlying tool output itself was bounded.

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

Interactive and print output renders `edit` calls as a plain unified diff for readability.

Rules:

- `old_text` must not be empty.
- Each `old_text` must match exactly once.
- Multiple edits must not overlap.
- Edits are matched against the original file, not incrementally.
- UTF-8 BOM and LF/CRLF line endings are preserved.

## bash

Run a bash command in cwd with timeout.

```json
{
  "command": "cargo test",
  "timeout_seconds": 120
}
```

Output includes status, timeout flag, stdout, and stderr. Large output is truncated to a bounded tail.

## grep

Search file contents under a path.

```json
{
  "pattern": "OpenAiCodexProvider",
  "path": "src"
}
```

Uses `rg` if available, with a Rust fallback.

## find

Find files by filename substring and/or extension.

```json
{
  "path": "src",
  "name": "openai",
  "extension": "rs"
}
```

Skips dotdirs and `target`.

## ls

List directory contents.

```json
{
  "path": "."
}
```

## Tool loop budget

Ferrum limits tool rounds per user turn. If the budget is exhausted, Ferrum makes one final no-tools model call asking the assistant to summarize findings and next steps instead of returning a raw loop-limit error.

## Safety

- Tools run with local user permissions.
- `write`, `edit`, and `bash` can mutate files.
- Ferrum currently has no per-tool confirmation prompts. Tool calls execute directly in both print and interactive mode.
- Secrets must not be written, printed, logged, or committed.
