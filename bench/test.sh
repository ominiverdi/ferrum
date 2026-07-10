#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
temp="$(mktemp -d)"
trap 'rm -rf "$temp"' EXIT
mkdir -p "$temp/runs/run"
cat > "$temp/runs/run/result.env" <<'RESULT'
agent=$(touch exploited)
task=001-multiline-edit
run_dir=/tmp/example
exit_code=0
validate_code=0
score_code=0
max_rss_kb=1
elapsed=0:01
RESULT

(
  cd "$temp"
  "$root/bench/report.sh" "$temp/runs"
) > "$temp/report.txt"

if [ -e "$temp/exploited" ]; then
  echo "bench/report.sh executed result data" >&2
  exit 1
fi
grep -Fq '$(touch exploited)' "$temp/report.txt"

if "$root/bench/run.sh" ferrum-codex ../../outside > "$temp/run.out" 2> "$temp/run.err"; then
  echo "bench/run.sh accepted a traversing task ID" >&2
  exit 1
fi
grep -Fq 'invalid task ID' "$temp/run.err"

bash -n "$root/bench/run.sh" "$root/bench/report.sh"
