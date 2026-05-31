#!/usr/bin/env bash
set -euo pipefail
run_dir="$1"; out="$run_dir/stdout.txt"; err="$run_dir/stderr.txt"; diff="$run_dir/diff.patch"; fail=0
ok(){ echo "ok: $1"; }
bad(){ echo "missing: $1"; fail=1; }
grep -Eiq 'precedence|override|order|after config|config.*env|env.*config' "$out" && ok 'explains precedence root cause' || bad 'explains precedence root cause'
grep -q 'TINYDEPLOY_ENDPOINT' "$diff" && ok 'patch handles endpoint env' || bad 'patch handles endpoint env'
grep -q 'TINYDEPLOY_RETRIES' "$diff" && ok 'patch handles retries env' || bad 'patch handles retries env'
! grep -q 'test_tinydeploy.py' "$diff" && ok 'tests unchanged' || bad 'tests unchanged'
grep -Eiq 'pytest|passed' "$out" "$err" && ok 'ran tests' || bad 'ran tests'
agent="$(sed -n 's/^agent=//p' "$run_dir/result.env" 2>/dev/null || true)"
case "$agent" in
  ferrum-*)
    grep -Eiq '\[tool:read\]|\[tool:grep\]|\[tool:find\]' "$err" && ok 'uses inspection tools' || bad 'uses inspection tools'
    grep -Eiq '\[tool:bash\]' "$err" && ok 'uses bash for tests' || bad 'uses bash for tests'
    ;;
  *)
    echo "skip: tool trace assertions unavailable for $agent text-mode transcript"
    ;;
esac
exit "$fail"
