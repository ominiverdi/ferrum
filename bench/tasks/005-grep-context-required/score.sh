#!/usr/bin/env bash
set -euo pipefail
run_dir="$1"
out="$run_dir/stdout.txt"
err="$run_dir/stderr.txt"
fail=0
ok(){ echo "ok: $1"; }
bad(){ echo "$1"; fail=1; }
grep -Eiq 'clipboard helper|unsupported xdotool|getwindowclassname' "$out" && ok 'reports cause from context line' || bad 'missing: reports cause from context line'
grep -Eiq 'CRITICAL_FAILURE paste failed|paste failed' "$out" && ok 'reports failure line' || bad 'missing: reports failure line'
! grep -Eiq 'node_modules|target/debug|dependency noise|build noise' "$out" && ok 'ignored noise absent from answer' || bad 'forbidden: ignored noise in answer'
agent="unknown"
if [ -f "$run_dir/result.env" ]; then
  agent="$(sed -n 's/^agent=//p' "$run_dir/result.env")"
fi

case "$agent" in
  ferrum-*)
    grep -Eiq '\[tool:grep\]' "$err" && ok 'uses native grep' || bad 'missing: uses native grep'
    ! grep -Eiq '\[tool:read\]|\[tool:bash\]' "$err" && ok 'does not use read/bash' || bad 'forbidden: used read or bash instead of grep context'
    grep -Eiq 'context:| -C |--context' "$err" && ok 'grep context requested/rendered' || bad 'missing: grep context requested/rendered'
    ;;
  *)
    echo "skip: tool trace assertions unavailable for $agent text-mode transcript"
    ;;
esac
exit "$fail"
