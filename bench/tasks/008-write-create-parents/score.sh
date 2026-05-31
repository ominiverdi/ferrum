#!/usr/bin/env bash
set -euo pipefail
run_dir="$1"; err="$run_dir/stderr.txt"; fail=0
agent="$(sed -n 's/^agent=//p' "$run_dir/result.env" 2>/dev/null || true)"
case "$agent" in
  ferrum-*)
    grep -Eiq '\[tool:write\]' "$err" && echo ok:write || { echo missing:write; fail=1; }
    ! grep -Eiq '\[tool:bash\]' "$err" && echo ok:no-bash || { echo forbidden:bash; fail=1; }
    grep -Eiq 'content: 2 lines' "$err" && echo ok:preview || { echo missing:write-preview; fail=1; }
    ;;
  *)
    echo "skip: tool trace assertions unavailable for $agent text-mode transcript"
    ;;
esac
exit "$fail"
