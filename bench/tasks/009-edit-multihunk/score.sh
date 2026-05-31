#!/usr/bin/env bash
set -euo pipefail
run_dir="$1"; out="$run_dir/stdout.txt"; err="$run_dir/stderr.txt"; fail=0
agent="$(sed -n 's/^agent=//p' "$run_dir/result.env" 2>/dev/null || true)"
case "$agent" in
  ferrum-*)
    grep -Eiq '\[tool:edit\]' "$err" && echo ok:edit || { echo missing:edit; fail=1; }
    grep -Eiq 'edits: 2' "$err" && echo ok:two-edits || { echo missing:two-edits; fail=1; }
    grep -Eq -- '--- old|\+\+\+ new|@@' "$err" && echo ok:diff || { echo missing:diff; fail=1; }
    ! grep -Eiq '\[tool:bash\]' "$err" && echo ok:no-bash || { echo forbidden:bash; fail=1; }
    ;;
  *)
    grep -Eiq 'two edits|2 edits|one edit tool call' "$out" && echo ok:answer-edit-count || { echo missing:answer-edit-count; fail=1; }
    ;;
esac
exit "$fail"
