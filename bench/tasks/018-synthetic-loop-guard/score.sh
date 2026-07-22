#!/usr/bin/env bash
set -euo pipefail
run_dir="$1"; out="$run_dir/stdout.txt"; err="$run_dir/stderr.txt"; fail=0
ok(){ echo "ok: $1"; }
bad(){ echo "missing: $1"; fail=1; }
read_count=$(grep -Ec '^\[tool:read\]' "$err" || true)
if [ "$read_count" -ge 7 ]; then ok "repeated reads ($read_count)"; else bad "repeated reads ($read_count)"; fi
grep -Eiq '\[loop-guard\] same tool call repeated 4 times' "$err" && ok 'nudge emitted' || bad 'nudge emitted'
grep -Eiq '\[loop-guard\] same tool call repeated 7 times .*requesting final response' "$err" && ok 'force final emitted' || bad 'force final emitted'
grep -Eiq 'final after repeated read loop guard' "$out" && ok 'final response emitted' || bad 'final response emitted'
exit "$fail"
