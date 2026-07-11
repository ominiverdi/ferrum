#!/usr/bin/env bash
set -euo pipefail
umask 077

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
agent_timed_out=0
validate_timed_out=0
score_timed_out=0
max_rss_kb=1
elapsed=0:01
RESULT

(
  cd "$temp"
  "$root/bench/report.sh" "$temp/runs"
) > "$temp/report.txt"
if [[ -e "$temp/exploited" ]]; then
  echo "bench/report.sh executed result data" >&2
  exit 1
fi
grep -Fq '$(touch exploited)' "$temp/report.txt"

if "$root/bench/run.sh" ferrum-codex ../../outside > "$temp/run.out" 2> "$temp/run.err"; then
  echo "bench/run.sh accepted a traversing task ID" >&2
  exit 1
fi
grep -Fq 'invalid task ID' "$temp/run.err"

if BENCH_AGENT_TIMEOUT_SECONDS=invalid "$root/bench/run.sh" ferrum-codex \
  001-multiline-edit > "$temp/timeout.out" 2> "$temp/timeout.err"; then
  echo "bench/run.sh accepted an invalid timeout" >&2
  exit 1
fi
grep -Fq 'BENCH_AGENT_TIMEOUT_SECONDS must be an integer' "$temp/timeout.err"

mkdir -p "$temp/bin" "$temp/bench-runs"
cat > "$temp/bin/ferrum" <<'FAKE'
#!/usr/bin/env bash
if [[ "${1:-}" == "--version" ]]; then
  echo "ferrum benchmark-fake"
  exit 0
fi
sleep 30
FAKE
chmod 0755 "$temp/bin/ferrum"
if PATH="$temp/bin:$PATH" FERRUM_OFFLINE=1 BENCH_RUN_ROOT="$temp/bench-runs" \
  BENCH_AGENT_TIMEOUT_SECONDS=1 BENCH_VALIDATE_TIMEOUT_SECONDS=10 \
  BENCH_SCORE_TIMEOUT_SECONDS=10 \
  "$root/bench/run.sh" ferrum-codex 001-multiline-edit \
  > "$temp/timed-run.out" 2> "$temp/timed-run.err"; then
  echo "timed benchmark unexpectedly succeeded" >&2
  exit 1
fi
result="$(find "$temp/bench-runs" -mindepth 2 -maxdepth 2 -name result.env -print -quit)"
[[ -n "$result" ]] || { echo "timed benchmark did not write result.env" >&2; exit 1; }
grep -Fq 'agent_timed_out=1' "$result"
grep -Eq '^exit_code=(124|137)$' "$result"
run_dir="$(dirname "$result")"
[[ "$(stat -c '%a' "$run_dir")" == "700" ]] || {
  echo "benchmark run directory is not private" >&2
  exit 1
}
[[ -s "$run_dir/provenance.txt" ]] || { echo "missing benchmark provenance" >&2; exit 1; }

bash -n "$root/bench/run.sh" "$root/bench/report.sh" "$root/bench/test.sh"
