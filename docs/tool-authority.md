# Tool authority policy

Ferrum runs on the host as the invoking Unix user. The execution policy reduces accidental and model-induced authority; it is not process isolation and does not make arbitrary programs safe.

## Threat model

Repository files, user prompts, provider output, MCP output, command output, skills, and generated compaction summaries may be hostile. User-owned CLI and configuration are trusted runtime authority. Safety tiers govern native mutation and shell execution; they do not make untrusted instructions authoritative.

Allowed executables can use system calls, configuration, plugins, build scripts, tests, network services, or races that are not represented by their argv. Use a container, VM, Landlock/bubblewrap wrapper, isolated credentials, or a dedicated Unix account when host isolation is required.

## Tier-independent invariants

Every tier retains:

- command byte, syntax-node, syntax-depth, output, time, and concurrency bounds;
- complete Bash parsing and rejection of malformed or incomplete syntax;
- cancellation, process-tree cleanup, bounded pipe draining, and honest outcome reporting;
- terminal sanitization and private output spooling;
- native mutation target-identity, symlink, hard-link, and atomic-replacement checks;
- rejection of privilege escalation, filesystem formatting, special permission bits, destructive root-level operands, raw device writes, and protected credential mutation;
- provider, MCP, session, and instruction-trust boundaries.

These are robustness and trust properties, not authority-tier restrictions. A review fix must not reduce `low` capability unless it changes this contract explicitly and adds tier-specific regression coverage.

## Writable roots

`medium` limits native `write`/`edit` and statically recognized shell mutations to `[tools].writable_roots`. Relative roots are resolved from Ferrum's working directory; the default is `["."]`. `low` bypasses writable roots. `high` rejects native and shell mutation.

Ferrum resolves roots and static targets through their nearest existing ancestor before checking containment. Existing symlink escapes, dangling symlinks, multiply linked regular-file targets, and protected credential targets are rejected. This is an authority check, not race-free filesystem isolation.

At `medium`, an out-of-root operation requires an explicit user decision: add a trusted root, move the operation, perform it outside Ferrum, or deliberately switch to `low`. Model text cannot change tiers or extend roots.

## Shell policy

Ferrum parses complete Bash input before execution. Here-document bodies are data; substitutions in expandable bodies remain executable syntax. Syntax byte/node/depth limits apply at every tier.

Tier contract:

| Authority | low | medium (default) | high |
| --- | --- | --- | --- |
| Direct executable or script path | allow | allow | inspection allowlist only |
| Development/build executable | allow | allow | deny |
| Network client | allow | allow | deny |
| Inline interpreter or shell payload | allow | deny | deny |
| Shell interpreter payload, function, or control flow | allow | deny | deny |
| Command/process substitution | allow | deny | deny |
| Dynamic executable/expansion | allow | deny | deny |
| Indirect executor or detached process | allow | deny | deny |
| Authority-changing environment assignment | allow | deny | deny |
| Native or shell mutation inside writable roots | allow | allow | deny |
| Native or shell mutation outside writable roots | allow | deny | deny |
| Malformed syntax or resource-limit violation | deny | deny | deny |
| Privilege escalation or explicit catastrophic shape | deny | deny | deny |
| Protected credential mutation | deny | deny | deny |

`low` is broad current-user host authority. It supports ordinary shell workflows, including `bash -lc`, scripts, `source`, `eval`, functions, control flow, command/process substitution, environment changes, dynamic commands, user installs, background processes, and mutations outside configured roots. Ferrum still parses the command and checks literal nested shell payloads and visible indirect commands for known catastrophic shapes, but low deliberately permits opaque authority. An equivalent operation can always be hidden inside an allowed interpreter or executable; low is not a security boundary against a hostile model or repository.

`medium` is the trusted-checkout development tier. It permits normal direct commands, build/test runners, network clients, and static mutations inside writable roots. It rejects command substitution, direct shell/interpreter payloads, unsupported compound authority, dynamic executable or mutation targets, authority-changing environment assignments, indirect executors, and detached launchers. Build scripts, tests, plugins, and unknown direct executables still run with the user's host authority.

`high` is inspection-only. It permits a conservative read-oriented shell command set and rejects native mutation, shell mutation, network clients, interpreters, builds, and unknown executables.

## Regression contract

The table-driven capability matrix in `src/tools/shell_guard.rs` covers representative commands across all three tiers. Additional tests cover:

- direct and nested catastrophic command shapes;
- shell launchers, wrappers, interpreters, functions, control flow, dynamic commands, and detachers;
- normalized paths, globbed/dynamic targets, redirections, and writable roots;
- here-document data and executable substitutions;
- native write/edit authority and protected credential targets;
- syntax byte/node/depth limits.

Future hardening should be classified as either a tier-independent invariant or an authority restriction. Authority restrictions require explicit low/medium/high expectations so review work cannot silently ratchet down `low`.
