#!/usr/bin/env bash
set -euo pipefail
run_dir="$1"; out="$run_dir/stdout.txt"; err="$run_dir/stderr.txt"; fail=0
agent="$(sed -n 's/^agent=//p' "$run_dir/result.env" 2>/dev/null || true)"
case "$agent" in
  ferrum-*)
    grep -Eiq '\[tool:bash\]' "$err" && echo ok:bash || { echo missing:bash; fail=1; }
    grep -Eiq 'Full output: /tmp/ferrum-bash-' "$err" && echo ok:full-output-path || { echo missing:full-output-path; fail=1; }
    ;;
  *)
    grep -Eiq '/tmp/.*bash.*\.log|full output path' "$out" && echo ok:full-output-path || { echo missing:full-output-path; fail=1; }
    ;;
esac
grep -Eiq 'line 6999' "$err" "$out" && echo ok:tail-visible || { echo missing:tail-visible; fail=1; }
exit "$fail"
