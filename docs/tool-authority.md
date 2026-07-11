# Tool authority policy

Ferrum runs on the host as the invoking Unix user. This policy reduces accidental and model-induced authority; it is not process isolation and does not make arbitrary programs safe.

## Threat model

Repository files, user prompts, provider output, MCP output, command output, and generated compaction summaries may be hostile. User-owned CLI and configuration are trusted authority. Ferrum must reject shell text it cannot parse structurally, dynamic executable positions, hidden execution through wrappers, and native mutations outside configured writable roots when the selected tier enforces them.

The policy protects against commands whose dangerous authority is visible in the submitted syntax. An allowed executable can still use system calls, configuration, plugins, build scripts, tests, network services, or races that are not represented by its argv. Use a container, VM, Landlock/bubblewrap wrapper, isolated credentials, or a dedicated Unix account when host isolation is required.

## Writable roots

At `medium` and `high`, native `write` and `edit` and statically recognized shell mutations are limited to `[tools].writable_roots`. Relative roots are resolved from Ferrum's working directory. The default is `["."]`. `low` bypasses this boundary for both native and shell mutations.

Ferrum resolves each root and target through its nearest existing ancestor before checking containment. Existing symlinks that resolve outside every root, dangling symlinks, and multiply linked regular-file targets are rejected. This is an authority check, not a complete race-free filesystem transaction; atomic replacement and stronger identity guarantees are tracked separately.

At `medium` or `high`, a rejected path requires an explicit user decision: add the intended trusted path to `writable_roots`, move the operation under an existing root, perform it outside Ferrum, or deliberately switch to `low`. The model cannot change tiers or extend roots.

## Shell parsing and execution policy

Ferrum parses the complete Bash input into a syntax tree before execution. Parse errors and unsupported compound forms fail closed. Here-document bodies are data; executable substitutions inside expandable here-documents are still inspected.

All tiers:

- reject dynamic executable names and process substitution;
- recursively inspect supported wrappers and reject unknown wrapper options;
- reject shell interpreter relaunch, `eval`, `exec`, `source`, `xargs`, privilege escalation, filesystem formatting, destructive root/home operands, and sensitive credential targets;
- normalize literal mutation paths and reject dynamic/globbed mutation operands.

`medium` and `high` apply writable roots to recognized shell mutations. `low` permits directory changes with `cd` and bypasses writable roots.

Tier contract:

| Authority | low | medium (default) | high |
| --- | --- | --- | --- |
| Direct inspection executable | allow | allow | inspection allowlist only |
| Command substitution | inspect nested commands | deny | deny |
| Inline interpreter payload | allow as explicit broad authority | deny | deny |
| Direct static development/build executable | allow | allow as explicit checkout-code authority | deny |
| Direct network client | allow | allow | deny |
| Statically recognized mutation inside writable roots | allow | allow | deny |
| Statically recognized mutation outside writable roots | allow | deny | deny |
| Dynamic/indirect executable or mutation target | deny | deny | deny |

`low` is explicit broad host authority, including native and shell mutation outside configured roots, not an unsafe-syntax bypass. `medium` supports normal trusted-checkout development but does not contain build scripts, tests, plugins, or unknown executables. `high` is for inspection and rejects commands whose effects cannot be established conservatively.

## Structural test matrix

The regression matrix covers:

- leading assignments, array assignments, dynamic executable expansion, quote concatenation, chains, pipelines, and malformed syntax;
- `env`, `command`, `nice`, `timeout`, detaching wrappers, shell launchers, interpreters, build runners, and unknown wrapper options;
- normalized paths, quoted literals, absolute and relative destructive globs, redirections, and configured roots;
- here-doc data versus executable command substitution;
- each tier's direct command, network, interpreter, build, and mutation decisions;
- native write/edit inside a root, lexical escape, existing symlink escape, and explicit additional roots.

The guard is a deterministic denial layer. Ferrum does not silently downgrade a denied command, infer approval from model text, or claim that an allowed command is sandboxed.
