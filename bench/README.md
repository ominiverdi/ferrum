# Ferrum benchmark harness

Local benchmark harness for Ferrum, Pi, and OpenCode.

The harness measures:

- command exit code
- fixture validation result
- task score result
- wall-clock time via `/usr/bin/time -v`
- maximum resident set size via `/usr/bin/time -v`
- stdout/stderr transcript
- final git diff

## Run directory

By default, runs are created with unpredictable names under a current-user-private directory:

```text
${XDG_RUNTIME_DIR:-/tmp}/ferrum-bench-runs-$UID
```

Override the private parent directory when needed:

```bash
BENCH_RUN_ROOT=/path/to/private/runs bench/run.sh ferrum-codex 014-large-context-navigation
```

The harness sets `umask 077`, rejects symlink run roots, forces mode `0700`, and uses `mktemp -d` for every run. Do not commit run outputs. They can include private source, absolute paths, prompts, and full transcripts.

## Agent profiles

Ferrum, same ChatGPT/Codex access:

```bash
bench/run.sh ferrum-codex 014-large-context-navigation
```

Ferrum with only the `bash` tool and no MCP:

```bash
bench/run.sh ferrum-codex-bash 014-large-context-navigation
```

Pi, same ChatGPT/Codex access and equivalent core tools:

```bash
BENCH_AGENT_HOME=/path/to/pi-benchmark-home \
  bench/run.sh pi-codex 014-large-context-navigation
```

OpenCode:

```bash
BENCH_AGENT_HOME=/path/to/opencode-benchmark-home \
  OPENCODE_MODEL=openai/gpt-5.5 bench/run.sh opencode 014-large-context-navigation
```

Defaults:

- Ferrum: `openai-codex/gpt-5.5`, `--no-mcp`
- Ferrum bash-only: `openai-codex/gpt-5.5`, `--no-mcp --tools bash`
- Pi: `openai-codex/gpt-5.5`, `--tools read,bash,edit,write,grep,find,ls`
- OpenCode: explicit `OPENCODE_MODEL`

Pi and OpenCode runs require an explicit, current-user-owned dedicated home with no group/other access so ambient user configuration cannot silently affect comparisons:

```bash
BENCH_AGENT_HOME=/path/to/pi-benchmark-home bench/run.sh pi-codex 014-large-context-navigation
BENCH_AGENT_HOME=/path/to/opencode-benchmark-home \
  OPENCODE_MODEL=openai/gpt-5.5 bench/run.sh opencode 014-large-context-navigation
```

Ferrum runs use isolated config/data directories. Online Ferrum runs copy only `auth.json` into that private config directory for the duration of the agent process, then remove the copy. Override its source with `BENCH_FERRUM_AUTH_FILE`.

Model overrides:

```bash
FERRUM_MODEL=gpt-5.5 bench/run.sh ferrum-codex 011-multi-file-refactor
FERRUM_MODEL=gpt-5.5 bench/run.sh ferrum-codex-bash 011-multi-file-refactor
BENCH_AGENT_HOME=/path/to/pi-benchmark-home PI_MODEL=gpt-5.5 \
  bench/run.sh pi-codex 011-multi-file-refactor
BENCH_AGENT_HOME=/path/to/opencode-benchmark-home \
  OPENCODE_MODEL=openai/gpt-5.5 bench/run.sh opencode 011-multi-file-refactor
```

Offline deterministic Ferrum runs:

```bash
FERRUM_OFFLINE=1 FERRUM_FAKE_SCRIPT=repeat_read bench/run.sh ferrum-codex 018-synthetic-loop-guard
FERRUM_OFFLINE=1 FERRUM_FAKE_SCRIPT=repeat_read bench/run.sh ferrum-codex-bash 018-synthetic-loop-guard
FERRUM_OFFLINE=1 FERRUM_FAKE_SCRIPT=missing_read bench/run.sh ferrum-codex 019-synthetic-error-loop-guard
FERRUM_OFFLINE=1 FERRUM_FAKE_SCRIPT=mixed_write_read bench/run.sh ferrum-codex 021-mixed-batch-ordering
```

## Task layout

Each task directory contains:

```text
prompt.md      prompt sent to the agent
setup.sh       creates the isolated fixture workspace
validate.sh    validates the final workspace state
score.sh       optional transcript/diff scoring
```

## Run output

Each run creates:

```text
<private-run-root>/run.<agent>.<task>.<random>/
  work/          isolated task workspace
  config/        isolated Ferrum configuration
  data/          isolated Ferrum state
  home/          isolated Ferrum home
  prompt.md
  command.txt
  provenance.txt executable hash/version, source/task hashes, resolved profile, timeouts
  stdout.txt
  stderr.txt
  time.txt       /usr/bin/time -v output
  diff.patch
  validate.txt
  score.txt
  result.env     outcomes plus explicit timeout flags
```

Summarize runs:

```bash
bench/report.sh
bench/report.sh "${XDG_RUNTIME_DIR:-/tmp}/ferrum-bench-runs-$UID"
BENCH_RUN_ROOT=/path/to/private/runs bench/report.sh
```

## Timeouts

Setup, agent, validator, and scorer have separate foreground deadlines. Defaults:

```text
BENCH_SETUP_TIMEOUT_SECONDS=120
BENCH_AGENT_TIMEOUT_SECONDS=900
BENCH_VALIDATE_TIMEOUT_SECONDS=120
BENCH_SCORE_TIMEOUT_SECONDS=120
```

Each accepts 1-86400 seconds. `result.env` records agent, validator, and scorer timeout outcomes independently.

## Fairness controls

- Ferrum uses `--no-mcp` so configured MCP tools do not inflate tool schemas.
- Ferrum bash-only uses `--no-mcp --tools bash` to test model behavior with a single shell tool surface.
- Pi is invoked with the same core tool names enabled.
- Agent processes receive an explicit environment allowlist rather than the complete invoking environment.
- Provenance records executable hashes/versions, source and task hashes, provider/model/tool selections, home isolation mode, and all deadlines without recording credential values.
- Run workspaces and artifacts are current-user private and default outside the Ferrum repository to avoid parent context contamination.
- Scoring separates fixture validation from transcript/diff assertions.
- Some trace assertions are Ferrum-only because Pi/OpenCode text modes do not expose identical tool traces.

## Caveats

Single runs are noisy. For serious comparisons, run each task at least three times and compare medians.

This harness compares full agent behavior, not just model quality. Prompt scaffolding, tool implementation, tool rendering, and session/context behavior differ by agent.
