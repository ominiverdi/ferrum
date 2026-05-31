#!/usr/bin/env bash
set -euo pipefail
run_dir="$1"; out="$run_dir/stdout.txt"; err="$run_dir/stderr.txt"; diff="$run_dir/diff.patch"; fail=0
ok(){ echo "ok: $1"; }
bad(){ echo "missing: $1"; fail=1; }
! grep -Eq '^(diff --git a/(calculator.py|test_calculator.py))' "$diff" && ok 'no code/test patch' || bad 'no code/test patch'
grep -Eiq 'transient|flaky|rerun|marker|no code change|no code changes|not need.*change' "$out" && ok 'explains no-code transient' || bad 'explains no-code transient'
grep -Eiq 'passed' "$out" "$err" && ok 'final tests passed' || bad 'final tests passed'
agent="$(sed -n 's/^agent=//p' "$run_dir/result.env" 2>/dev/null || true)"
case "$agent" in
  ferrum-*)
    pytest_runs=$(grep -Eic 'pytest' "$err" || true)
    if [ "$pytest_runs" -ge 2 ]; then ok "runs tests at least twice ($pytest_runs)"; else bad "runs tests at least twice ($pytest_runs)"; fi
    ! grep -Eiq '\[tool:edit\]|\[tool:write\]' "$err" && ok 'no edit/write tools' || bad 'no edit/write tools'
    ;;
  *)
    echo "skip: tool trace assertions unavailable for $agent text-mode transcript"
    ;;
esac
exit "$fail"
