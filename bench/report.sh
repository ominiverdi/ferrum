#!/usr/bin/env bash
set -euo pipefail
runs_root="${1:-${BENCH_RUN_ROOT:-/tmp/ferrum-bench-runs}}"
printf '%-22s %-30s %-8s %-8s %-8s %-12s %-12s %s\n' agent task exit validate score max_rss_kb elapsed run
for f in "$runs_root"/*/result.env; do
  [ -e "$f" ] || exit 0
  unset agent task exit_code validate_code score_code max_rss_kb elapsed run_dir
  while IFS= read -r line || [ -n "$line" ]; do
    key="${line%%=*}"
    value="${line#*=}"
    case "$key" in
      agent) agent="$value" ;;
      task) task="$value" ;;
      exit_code) exit_code="$value" ;;
      validate_code) validate_code="$value" ;;
      score_code) score_code="$value" ;;
      max_rss_kb) max_rss_kb="$value" ;;
      elapsed) elapsed="$value" ;;
      run_dir) run_dir="$value" ;;
    esac
  done < "$f"
  printf '%-22s %-30s %-8s %-8s %-8s %-12s %-12s %s\n' \
    "${agent:-}" "${task:-}" "${exit_code:-}" "${validate_code:-}" "${score_code:-na}" "${max_rss_kb:-}" "${elapsed:-}" "${run_dir:-}"
done
