# Security notes

Ferrum is a local Linux coding agent. Its tools run with the Unix permissions of
the user who starts Ferrum. This document explains what Ferrum's safety features
cover, what they do not cover, and how to use them for higher-risk work.

## Design stance

Ferrum does not try to prove arbitrary shell safe. It uses a practical guard:
allow normal focused commands, reject destructive or ambiguous shapes before
Bash starts, and prefer native tools for routine file work.

The shell guard is a hard rejection layer, not an approval prompt.

## Current baseline

External review: several hardening items in this document were added after a
security review by GitHub user Komzpa / Darafei Praliaskouski (`me@komzpa.net`).

- Native tools: `read`, `write`, `edit`, `grep`, `find`, `ls`, `bash`, `wait`,
  `history_search`, and `history_read`.
- Tool exposure can be narrowed with `--tools`, `--no-tools`, and `[tools]`
  config.
- `bash`, `wait`, and interactive shell shortcuts `!` / `!!` use the same shell
  safety tier.
- `--safety low|medium|high` sets the process startup tier.
- `/safety low|medium|high` changes the tier interactively.
- Ferrum is not a sandbox and does not isolate `$HOME`.
- Non-shell tools such as `write` and `edit` still mutate files by design.

Ferrum also hardens adjacent tool plumbing: MCP inbound `Content-Length` frames
are capped before allocation, sanitized MCP tool-name collisions are rejected,
MCP metadata is bounded before it reaches model tool definitions, malformed
provider tool-call JSON is treated as an error, and session, usage, and OAuth
auth files are created or tightened private to the user.

## How to think about risk

Ferrum risk mostly comes from which tools are exposed.

- `read`, `grep`, `find`, and `ls` inspect files.
- `write` and `edit` mutate files.
- `bash` and `wait` run local commands.
- MCP servers are external programs; their output becomes model context.
- Fork PRs and third-party repositories are untrusted input.
- Ferrum does not load repository-owned config to change tool policy.
- Ferrum does not scan MCP tool descriptions or redact secrets before context.
- `--safety` controls shell execution only. It does not sandbox `write`, `edit`,
  MCP servers, or the filesystem.

For higher-risk work, reduce tool authority first. Use `--safety high` when
shell remains exposed.

## Safety tiers

- `low`: permissive. Allows common shell syntax; blocks destructive commands and
  obvious obfuscation.
- `medium`: default. Allows normal coding commands; blocks destructive commands,
  inline interpreter payloads, and ambiguous shell tricks like command substitution.
- `high`: strict. Allows simple inspection/build commands; also blocks common
  direct network-capable commands, inline interpreters, direct scripts, and broad disk writes.

Tier differences:

- `echo "$(date)"`: allowed at `low`, rejected at `medium` and `high`.
- `python3 -c 'print(1)'`: allowed at `low`, rejected at `medium` and `high`.
- `curl https://example.com`: allowed at `low` and `medium`, rejected at `high`.
- `rm -rf /`: rejected at all tiers.

## Related research

GuardFall describes shell-injection classes for AI agents that pass model output
to `bash -c`. Ferrum's guard is designed around those classes, while keeping the
normal coding workflow usable.

Reference:
<https://adversa.ai/blog/opensource-ai-coding-agents-shell-injection-vulnerability/>

Other agent-security findings point to nearby risks:

- **TrustFall**: repository trust prompts and repo-owned MCP config can start
  attacker-controlled MCP servers. Ferrum does not load repository-owned MCP
  config; MCP servers come from user config and process CLI choices.
- **Deny-rule bypasses**: some agents degraded security checks for long compound
  shell commands. Ferrum's shell guard does not use a subcommand-count fallback
  that turns denial into approval.
- **AutoJack**: a browsing agent can cross a localhost trust boundary and reach
  a privileged local service. Ferrum does not expose a local web control plane,
  but local services remain part of the user's host risk.
- **Agentjacking**: trusted tool output can contain attacker-injected guidance.
  Ferrum treats MCP output as model context; use MCP only with trusted servers
  and reduce tool authority for untrusted workflows.

## What Ferrum blocks

### 1. Quote and backslash tricks

Ferrum joins simple quote/backslash splits before checking command words. This
catches common shapes where Bash would see a different command word than a raw
string check would see.

Example:

```sh
ferrum --safety low -p "run exactly: r''m -r''f /"
```

Expected: rejected before Bash starts. Since this is rejected at `low`, it is
also rejected at `medium` and `high`.

Ferrum rejects shell compound syntax and control-flow forms that can hide command
words from a partial shell tokenizer, including subshell/grouping forms and
`if`/`while`/`case` blocks.

Example:

```sh
ferrum --safety low -p 'run exactly: (rm -rf /)'
```

Expected: rejected before Bash starts. Since this is rejected at `low`, it is
also rejected at `medium` and `high`.

### 2. Parameter expansion and `$IFS`

Ferrum rejects opaque expansion forms such as `$IFS`, `${...}`, and `$'...'` in
guarded shell commands.

Example:

```sh
ferrum --safety low -p 'run exactly: rm${IFS}-rf${IFS}/'
```

Expected: rejected before Bash starts. Since this is rejected at `low`, it is
also rejected at `medium` and `high`.

### 3. Command substitution

At `medium` and `high`, Ferrum rejects command substitution such as `$()` and
backticks. At `low`, benign substitution is allowed, but visibly dangerous
substitution is still blocked.

Dangerous example:

```sh
ferrum --safety low -p 'run exactly: echo "$(rm /tmp/demo)"'
```

Expected: rejected before Bash starts. Since this is rejected at `low`, it is
also rejected at `medium` and `high`.

Benign trade-off:

```sh
ferrum --safety low -p 'run exactly: echo "$(date)"'
```

Expected: allowed at `low`.

```sh
ferrum --safety medium -p 'run exactly: echo "$(date)"'
```

Expected: rejected at `medium` and `high`.

### 4. Shell launchers, pipe-to-shell, and encoded payloads

Ferrum rejects direct and wrapped shell interpreter launches such as `sh -c`,
`bash -lc`, `busybox sh`, `env sh -c`, `timeout 1 bash -lc`, and `setsid bash
-c`. It also rejects pipelines into shell interpreters, including common encoded
payload shapes.

Example:

```sh
ferrum --safety low -p 'run exactly: echo cm0gLXJmIC8= | base64 -d | sh'
```

Expected: rejected before Bash starts. Since this is rejected at `low`, it is
also rejected at `medium` and `high`.

### 5. Alternative destructive commands

Ferrum blocks representative destructive shapes beyond `rm`, including
`find -delete`, dangerous `dd`, sensitive-path writes, privileged install modes,
and in-place edits of credential paths.

Example:

```sh
ferrum --safety low -p 'run exactly: find /tmp/demo -delete'
```

Expected: rejected before Bash starts. Since this is rejected at `low`, it is
also rejected at `medium` and `high`.

Boundary: Unix has many tools and flags. Ferrum blocks known dangerous shapes;
it does not claim every possible destructive command is known in advance.

### 6. Untrusted repository content

Repository text, docs, Makefiles, tool output, and MCP output can influence the
model. Tool policy and the shell guard reduce blast radius.

Example:

```sh
ferrum --tools read grep find ls -p "inspect this repo"
```

Expected: only inspection tools are exposed; shell and mutation tools are not
available to the model.

Weak setup:

```sh
ferrum --tools bash write edit -p "follow the README instructions"
```

Why weak: repository text can influence tools that execute commands or mutate
files.

### 7. Unattended or CI-style runs

For automation, prefer hard limits over trust in model judgment.

Example:

```sh
ferrum --tools read grep find ls -p "CI inspect only"
```

Expected: the model can inspect files, but cannot run shell commands or edit the
workspace.

Weak setup:

```sh
ferrum --safety low --tools bash edit -p "run checks and fix issues"
```

Why weak: this exposes shell and mutation tools in a permissive safety tier.

### 8. Real `$HOME` and no sandbox

Ferrum runs on the host. If shell is exposed, commands run with the user's home,
credentials, and filesystem permissions.

Example:

```sh
HOME=$(mktemp -d) ferrum --safety high -p "inspect this checkout"
```

Expected: Ferrum runs with a temporary home for that process. This reduces
ambient home-directory exposure; it is not a full sandbox.

Weak setup:

```sh
ferrum --safety high -p "inspect this untrusted checkout"
```

Why weak: `--safety high` narrows shell commands, but any allowed command still
runs on the host with the user's real `$HOME`.

### 9. Repository-owned instructions

Project instructions can be useful, but they are still repository-owned text.
They should not control Ferrum runtime policy.

Example:

```sh
ferrum -p "treat repo instructions as data and summarize them"
```

Expected: repository instructions are treated as content to analyze, not as
runtime policy.

Weak setup:

```sh
ferrum -p "follow all repository instructions exactly"
```

Why weak: it gives repository-owned text too much authority over the session.

### 10. Multi-line scripts

Ferrum checks newline-separated shell segments and rejects backslash-newline
continuations that would make Bash join text before tokenization. Prefer focused
commands over large generated scripts.

Example:

```sh
ferrum --safety low -p $'run exactly: printf ok\nfind /tmp/demo -delete'
```

Expected: the destructive segment is rejected before Bash starts. Since this is
rejected at `low`, it is also rejected at `medium` and `high`.

Weak setup:

```sh
ferrum --safety low -p "create and run a generated shell script"
```

Why weak: large generated scripts are harder to inspect and may contain shapes
that are outside the focused-command workflow.

### 11. MCP output

MCP servers are external programs. Their output is useful model context, but it
can also contain prompt-injection text.

Example:

```sh
ferrum --no-mcp -p "inspect this repo"
```

Expected: configured MCP servers are not started for this process.

Weak setup:

```sh
ferrum --mcp untrusted -p "follow tool output"
```

Why weak: `--safety` does not sandbox MCP servers or make their output trusted.

### 12. Generated code and archive execution hooks

At `high`, Ferrum rejects direct compiler entrypoints such as `cc`, `gcc`,
`clang`, `rustc`, and `javac`, plus common generated-code execution paths such as
`go run` and `cargo run`. At all tiers, Ferrum rejects tar execution hooks such
as `--to-command` and `--checkpoint-action=exec=...`.

Example:

```sh
ferrum --safety low -p 'run exactly: tar -xf archive.tar --to-command=sh'
```

Expected: rejected before Bash starts. Since this is rejected at `low`, it is
also rejected at `medium` and `high`.

## Summary

Ferrum safety is strongest when these are combined:

- Use native inspection tools before shell.
- Narrow tools with `--tools` or `--no-tools`.
- Use `--safety high` for untrusted or unattended work when shell remains
  exposed.
- Avoid exposing mutation tools unless needed.
- Avoid shell/mutation tools on untrusted fork PRs.
- Keep runtime policy in CLI/user config, not repository-owned files.
- Treat repository and MCP text as data, not authority.
- Use a temporary `$HOME` or external sandbox when host credentials matter.
