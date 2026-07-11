#!/usr/bin/env bash
set -euo pipefail
umask 077
runs_root="${1:-${BENCH_RUN_ROOT:-${XDG_RUNTIME_DIR:-/tmp}/ferrum-bench-runs-${UID}}}"
printf '%-20s %-30s %-6s %-8s %-6s %-7s %-7s %-7s %-12s %-12s %s\n' \
  agent task exit validate score agent_to val_to score_to max_rss_kb elapsed run
for f in "$runs_root"/*/result.env; do
  [[ -e "$f" ]] || exit 0
  unset agent task exit_code validate_code score_code agent_timed_out validate_timed_out \
    score_timed_out max_rss_kb elapsed run_dir
  while IFS= read -r line || [[ -n "$line" ]]; do
    key="${line%%=*}"
    value="${line#*=}"
    case "$key" in
      agent) agent="$value" ;;
      task) task="$value" ;;
      exit_code) exit_code="$value" ;;
      validate_code) validate_code="$value" ;;
      score_code) score_code="$value" ;;
      agent_timed_out) agent_timed_out="$value" ;;
      validate_timed_out) validate_timed_out="$value" ;;
      score_timed_out) score_timed_out="$value" ;;
      max_rss_kb) max_rss_kb="$value" ;;
      elapsed) elapsed="$value" ;;
      run_dir) run_dir="$value" ;;
    esac
  done < "$f"
  printf '%-20s %-30s %-6s %-8s %-6s %-7s %-7s %-7s %-12s %-12s %s\n' \
    "${agent:-}" "${task:-}" "${exit_code:-}" "${validate_code:-}" \
    "${score_code:-na}" "${agent_timed_out:-na}" "${validate_timed_out:-na}" \
    "${score_timed_out:-na}" "${max_rss_kb:-}" "${elapsed:-}" "${run_dir:-}"
done
