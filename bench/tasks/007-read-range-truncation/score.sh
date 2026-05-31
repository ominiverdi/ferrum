#!/usr/bin/env bash
set -euo pipefail
run_dir="$1"; out="$run_dir/stdout.txt"; err="$run_dir/stderr.txt"; fail=0
grep -Eiq 'line 010' "$out" && echo ok:first || { echo missing:first; fail=1; }
grep -Eiq 'line 014' "$out" && echo ok:last || { echo missing:last; fail=1; }
agent="$(sed -n 's/^agent=//p' "$run_dir/result.env" 2>/dev/null || true)"
case "$agent" in
  ferrum-*)
    grep -Eiq '\[tool:read\]' "$err" && echo ok:read || { echo missing:read; fail=1; }
    ! grep -Eiq '\[tool:bash\]' "$err" && echo ok:no-bash || { echo forbidden:bash; fail=1; }
    ;;
  *)
    echo "skip: tool trace assertions unavailable for $agent text-mode transcript"
    ;;
esac
exit "$fail"
