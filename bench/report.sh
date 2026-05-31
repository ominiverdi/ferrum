#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
runs_root="${1:-${BENCH_RUN_ROOT:-/tmp/ferrum-bench-runs}}"
printf '%-22s %-30s %-8s %-8s %-8s %-12s %-12s %s\n' agent task exit validate score max_rss_kb elapsed run
for f in "$runs_root"/*/result.env; do
  [ -e "$f" ] || exit 0
  unset agent task exit_code validate_code score_code max_rss_kb elapsed run_dir
  # shellcheck disable=SC1090
  source "$f"
  printf '%-22s %-30s %-8s %-8s %-8s %-12s %-12s %s\n' \
    "${agent:-}" "${task:-}" "${exit_code:-}" "${validate_code:-}" "${score_code:-na}" "${max_rss_kb:-}" "${elapsed:-}" "${run_dir:-}"
done
