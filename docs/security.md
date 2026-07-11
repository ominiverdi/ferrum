# Security notes

Ferrum is a local Linux coding agent. Its tools run with the Unix permissions of the user who starts Ferrum. This document explains what Ferrum's safety features cover, what they do not cover, and how to use them for higher-risk work.

## Design stance

Ferrum separates robustness and trust invariants from execution authority:

- Bounds, cancellation, cleanup, terminal sanitization, atomic mutation, protected credential targets, and protocol validation apply at every safety tier.
- `/safety low|medium|high` controls native mutation and shell execution authority.
- Safety does not make repository text, skills, MCP output, provider output, or compaction summaries trusted authority.

The policy is a deterministic rejection layer, not an approval prompt or sandbox. Full contract and regression matrix: [tool-authority.md](tool-authority.md). Resource limits: [resource-boundaries.md](resource-boundaries.md).

## Current baseline

Several hardening items were added after an external security review by GitHub user Komzpa / Darafei Praliaskouski (`me@komzpa.net`).

- Native tools: `read`, `write`, `edit`, `grep`, `find`, `ls`, `bash`, `wait`, `history_search`, and `history_read`.
- Tool exposure can be narrowed with `--tools`, `--no-tools`, and `[tools]` config.
- `bash`, `wait`, and interactive shell shortcuts `!` / `!!` use the same execution tier and writable-root policy.
- `--safety low|medium|high` sets the startup tier; `/safety` changes it interactively.
- Native `write` and `edit` use identity-checked atomic replacement and reject protected credential targets.
- Shell commands have syntax byte/node/depth limits, bounded output, timeout/cancellation cleanup, and explicit outcome reporting.
- Untrusted terminal text is sanitized before rendering.
- Image, context, native search, file-line, directory-result, clipboard, and preview operations have explicit limits.
- MCP frames and metadata, provider response bodies and streams, tool-call JSON, session records, usage records, and OAuth storage are bounded and validated.
- Authenticated non-loopback provider URLs using cleartext HTTP are rejected unless explicitly enabled; provider clients do not follow redirects.
- Repository-owned config cannot change runtime tool policy or start MCP servers.

Ferrum is not a sandbox. It does not isolate `$HOME`, contain the system calls of an allowed executable, or make untrusted checkout code safe.

## Safety tiers

- `low`: broad current-user host authority. Allows ordinary shell syntax, scripts and inline interpreters, shell launchers, functions and control flow, command/process substitution, dynamic commands, environment changes, networking, user installs, detachers, and mutation outside writable roots.
- `medium`: default trusted-checkout development policy. Allows direct commands, builds/tests, network clients, and static mutation inside writable roots. Rejects shell/interpreter payloads, command substitution, dynamic or indirect authority, detached launchers, and mutations outside writable roots.
- `high`: inspection-only policy. Allows a conservative read-oriented shell command set and rejects native/shell mutation, networking, interpreters, builds, and unknown executables.

Tier-independent checks reject malformed syntax, resource-limit violations, privilege escalation, filesystem formatting, special permission bits, destructive root-level operands, raw device writes, and protected credential mutation.

Representative differences:

| Command | low | medium | high |
| --- | --- | --- | --- |
| `pwd` | allow | allow | allow |
| `cargo test` | allow | allow | deny |
| `curl https://example.com` | allow | allow | deny |
| `touch marker` | allow | allow inside roots | deny |
| `python3 -c 'print(1)'` | allow | deny | deny |
| `bash -lc 'echo ok'` | allow | deny | deny |
| `echo "$(date)"` | allow | deny | deny |
| `PATH=/tmp/bin cargo test` | allow | deny | deny |
| `rm -rf /` | deny | deny | deny |
| `sudo id` | deny | deny | deny |

The executable table is enforced by `tier_capability_contract_is_table_driven` in `src/tools/shell_guard.rs`.

## Low is not a hostile-input boundary

Low still parses complete Bash and catches known catastrophic shapes when they are visible. Literal nested shell payloads such as `bash -lc 'rm -rf /'` are inspected and rejected. This is a last-resort accident guard, not a proof of safety.

Low deliberately permits opaque authority. A Python payload, script, build step, dynamically selected executable, encoded pipe, or unknown program can perform operations not visible in its argv. Do not use low for hostile repositories, unattended automation, or prompts influenced by untrusted content unless the process is externally isolated.

Examples intentionally allowed only at low:

```sh
bash -lc 'echo ok'
source ./env.sh
if true; then echo ok; fi
printf '%s\n' a b | xargs echo
find /tmp/demo -delete
```

Known catastrophic and privilege-changing shapes remain rejected:

```sh
rm -rf /
mkfs.ext4 /dev/sda
dd if=/dev/zero of=/dev/sda
install -m4755 payload /tmp/payload
printf key > ~/.ssh/config
```

Unix has many tools and flags. Ferrum does not claim every destructive operation is recognizable. Use an external sandbox when containment is required.

## Writable roots and protected targets

`[tools].writable_roots` defaults to `["."]`.

- Low bypasses writable roots for native and shell mutations.
- Medium enforces writable roots for native `write`/`edit` and recognized shell mutations.
- High rejects native and shell mutations.

Ferrum resolves static targets through existing ancestors and rejects symlink escapes, dangling symlinks, multiply linked regular-file targets, and protected credential state such as `.ssh`, `.aws`, `.vault`, and Ferrum's trusted config/auth directory. Native replacement uses sibling temporary files, durable sync, identity verification, and atomic Linux rename operations.

Writable-root checks do not contain an allowed executable's system calls and are not race-free filesystem isolation.

## Instruction and protocol trust

Repository files, project instructions, skills, tool output, MCP output, provider output, and generated summaries may contain hostile instructions. The safety tier governs tool authority; it does not upgrade those instructions to trusted runtime policy.

- User CLI and user configuration define runtime authority.
- Ferrum does not load repository-owned MCP or tool-policy config.
- MCP stderr is withheld from model-visible errors, but MCP descriptions and output remain untrusted context.
- Compaction must preserve user/developer authority and must not convert summarized untrusted text into instructions.
- Skill discovery and loading must preserve its own trust boundary independently of `/safety`.

## Higher-risk workflows

Inspection only:

```sh
ferrum --tools read grep find ls --safety high -p "inspect this repository"
```

Temporary home plus inspection policy:

```sh
HOME=$(mktemp -d) ferrum --safety high -p "inspect this checkout"
```

Disable MCP when it is unnecessary:

```sh
ferrum --no-mcp -p "inspect this repository"
```

For stronger isolation, use a container, VM, Landlock/bubblewrap wrapper, isolated credentials, or a dedicated Unix account. Reduce exposed tools before relying on model judgment.

## Review rule

Future security work must classify each change as one of:

1. A tier-independent robustness or trust invariant. It must apply consistently without reducing ordinary low-authority workflows.
2. An authority restriction. It must update the low/medium/high contract and add explicit tier-matrix tests.

This prevents external-review hardening from silently turning `low` into another restrictive tier.

## Summary

- Use `medium` for normal trusted-checkout development.
- Use `high` and inspection-only tools for untrusted or unattended review.
- Use `low` only when broad current-user host authority is intended.
- Keep writable roots narrow when using medium.
- Treat repository, skill, provider, summary, and MCP text as data rather than runtime authority.
- Use external isolation when host credentials or hostile code matter.
