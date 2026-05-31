#!/usr/bin/env bash
set -euo pipefail
run_dir="$1"; out="$run_dir/stdout.txt"; err="$run_dir/stderr.txt"; diff="$run_dir/diff.patch"; fail=0
ok(){ echo "ok: $1"; }
bad(){ echo "missing: $1"; fail=1; }
grep -q '+    status = "queued"' "$diff" && ok 'sets beta queued' || bad 'sets beta queued'
! grep -E '^[+-].*alpha.*queued|^[+-].*queued.*alpha' "$diff" >/dev/null && ok 'alpha unchanged' || bad 'alpha unchanged'
! grep -q 'test_handlers.py' "$diff" && ok 'tests unchanged' || bad 'tests unchanged'
grep -Eiq 'pytest|passed' "$out" "$err" && ok 'ran tests' || bad 'ran tests'
agent="$(sed -n 's/^agent=//p' "$run_dir/result.env" 2>/dev/null || true)"
case "$agent" in
  ferrum-*)
    grep -Eiq '\[tool:edit\]' "$err" && ok 'uses edit' || bad 'uses edit'
    grep -Eiq '\[tool:bash\]' "$err" && ok 'uses bash tests' || bad 'uses bash tests'
    ;;
  *)
    echo "skip: tool trace assertions unavailable for $agent text-mode transcript"
    ;;
esac
exit "$fail"
