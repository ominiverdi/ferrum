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

By default, runs are written outside the repository:

```text
/tmp/ferrum-bench-runs
```

Override:

```bash
BENCH_RUN_ROOT=/path/to/runs bench/run.sh ferrum-codex 014-large-context-navigation
```

Do not commit run outputs. They can include absolute paths and full transcripts.

## Agent profiles

Ferrum, same ChatGPT/Codex access:

```bash
bench/run.sh ferrum-codex 014-large-context-navigation
```

Pi, same ChatGPT/Codex access and equivalent core tools:

```bash
bench/run.sh pi-codex 014-large-context-navigation
```

OpenCode:

```bash
OPENCODE_MODEL=openai/gpt-5.5 bench/run.sh opencode 014-large-context-navigation
```

Defaults:

- Ferrum: `openai-codex/gpt-5.5`, `--no-mcp`
- Pi: `openai-codex/gpt-5.5`, `--tools read,bash,edit,write,grep,find,ls`
- OpenCode: explicit `OPENCODE_MODEL`

Model overrides:

```bash
FERRUM_MODEL=gpt-5.5 bench/run.sh ferrum-codex 011-multi-file-refactor
PI_MODEL=gpt-5.5 bench/run.sh pi-codex 011-multi-file-refactor
OPENCODE_MODEL=openai/gpt-5.5 bench/run.sh opencode 011-multi-file-refactor
```

Offline deterministic Ferrum runs:

```bash
FERRUM_OFFLINE=1 FERRUM_FAKE_SCRIPT=repeat_read bench/run.sh ferrum-codex 018-synthetic-loop-guard
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
<run-root>/<timestamp>-<agent>-<task>/
  work/          isolated task workspace
  prompt.md
  command.txt
  stdout.txt
  stderr.txt
  time.txt       /usr/bin/time -v output
  diff.patch
  validate.txt
  score.txt
  result.env
```

Summarize runs:

```bash
bench/report.sh
bench/report.sh /tmp/ferrum-bench-runs
BENCH_RUN_ROOT=/tmp/ferrum-bench-runs bench/report.sh
```

## Fairness controls

- Ferrum uses `--no-mcp` so configured MCP tools do not inflate tool schemas.
- Pi is invoked with the same core tool names enabled.
- Run workspaces default outside the Ferrum repository to avoid parent context contamination.
- Scoring separates fixture validation from transcript/diff assertions.
- Some trace assertions are Ferrum-only because Pi/OpenCode text modes do not expose identical tool traces.

## Caveats

Single runs are noisy. For serious comparisons, run each task at least three times and compare medians.

This harness compares full agent behavior, not just model quality. Prompt scaffolding, tool implementation, tool rendering, and session/context behavior differ by agent.
