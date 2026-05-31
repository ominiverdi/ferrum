#!/usr/bin/env bash
set -euo pipefail
run_dir="$1"; out="$run_dir/stdout.txt"; err="$run_dir/stderr.txt"; diff="$run_dir/diff.patch"; fail=0
ok(){ echo "ok: $1"; }
bad(){ echo "missing: $1"; fail=1; }
grep -q 'src/app/routing.py' "$diff" && ok 'patches routing.py' || bad 'patches routing.py'
grep -q '+    "billing": {"path": "/billing", "label": "Customer Billing"}' "$diff" && ok 'sets customer billing label' || bad 'sets customer billing label'
! grep -Eq '^(diff --git a/(docs|legacy|src/generated|test_menu.py))' "$diff" && ok 'avoids forbidden files' || bad 'avoids forbidden files'
grep -Eiq 'src/app/routing.py|routing.py' "$out" && ok 'explains relevant file' || bad 'explains relevant file'
grep -Eiq 'pytest|passed' "$out" "$err" && ok 'ran tests' || bad 'ran tests'
agent="$(sed -n 's/^agent=//p' "$run_dir/result.env" 2>/dev/null || true)"
case "$agent" in
  ferrum-*)
    grep -Eiq '\[tool:grep\]|\[tool:find\]|\[tool:ls\]' "$err" && ok 'uses search/list tools' || bad 'uses search/list tools'
    grep -Eiq '\[tool:bash\]' "$err" && ok 'uses bash tests' || bad 'uses bash tests'
    ;;
  *)
    echo "skip: tool trace assertions unavailable for $agent text-mode transcript"
    ;;
esac
exit "$fail"
