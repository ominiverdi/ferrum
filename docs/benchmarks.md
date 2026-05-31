# Benchmarks

Ferrum includes a local benchmark harness under `bench/`.

The harness is intended for reproducible local comparisons and regression testing. It is not a formal leaderboard.

## Reproduce

Ferrum:

```bash
bench/run.sh ferrum-codex 014-large-context-navigation
```

Pi:

```bash
bench/run.sh pi-codex 014-large-context-navigation
```

OpenCode:

```bash
OPENCODE_MODEL=openai/gpt-5.5 bench/run.sh opencode 014-large-context-navigation
```

Summarize:

```bash
bench/report.sh
```

Runs default to:

```text
/tmp/ferrum-bench-runs
```

Override with `BENCH_RUN_ROOT` or pass a run root to `bench/report.sh`.

## Fairness controls

The current same-model comparison uses GPT-5.5 access for all agents:

- Ferrum: `openai-codex/gpt-5.5`
- Pi: `openai-codex/gpt-5.5`
- OpenCode: `openai/gpt-5.5`

The harness also controls tool surface where practical:

- Ferrum uses `--no-mcp`.
- Pi uses `--tools read,bash,edit,write,grep,find,ls`.
- Workspaces are outside the Ferrum repository by default to avoid parent context contamination.

OpenCode has its own native tool surface and prompt scaffolding, so comparisons are product-level rather than byte-identical request comparisons.

## Snapshot: 2026-05-31

Environment: local Linux workstation. One run per task. Same GPT-5.5 family access. Run root: `/tmp/ferrum-bench-runs`.

Tasks:

- `011-multi-file-refactor`
- `012-broken-cli-diagnosis`
- `014-large-context-navigation`
- `016-independent-file-synthesis`
- `020-flaky-test-discipline`

All agents passed all five tasks in this snapshot.

| agent | task | score | max RSS KB | elapsed |
|---|---:|---:|---:|---:|
| Ferrum | 011 | 0 | 32780 | 0:29.17 |
| Ferrum | 012 | 0 | 32640 | 0:25.31 |
| Ferrum | 014 | 0 | 32960 | 0:28.63 |
| Ferrum | 016 | 0 | 16296 | 0:06.78 |
| Ferrum | 020 | 0 | 32504 | 0:10.29 |
| Pi | 011 | 0 | 178632 | 0:38.19 |
| Pi | 012 | 0 | 179608 | 0:23.21 |
| Pi | 014 | 0 | 183328 | 0:22.75 |
| Pi | 016 | 0 | 181352 | 0:13.05 |
| Pi | 020 | 0 | 178364 | 0:22.08 |
| OpenCode | 011 | 0 | 444452 | 1:40.18 |
| OpenCode | 012 | 0 | 439820 | 1:33.76 |
| OpenCode | 014 | 0 | 441764 | 1:04.84 |
| OpenCode | 016 | 0 | 369504 | 0:19.33 |
| OpenCode | 020 | 0 | 407572 | 1:03.35 |

Aggregate:

| agent | pass | mean time | median time | mean RSS |
|---|---:|---:|---:|---:|
| Ferrum | 5/5 | 20.04s | 25.31s | 29.4 MB |
| Pi | 5/5 | 23.86s | 22.75s | 180.3 MB |
| OpenCode | 5/5 | 68.29s | 64.84s | 420.6 MB |

## Caveats

- Single-run timings are noisy. Prefer at least three runs per task and compare medians.
- Network/provider latency can dominate wall time.
- Tool traces differ by agent; some scoring checks are Ferrum-only where Pi/OpenCode text mode lacks comparable traces.
- The harness evaluates complete product behavior: prompt scaffolding, tool implementation, context handling, and runtime overhead all matter.
