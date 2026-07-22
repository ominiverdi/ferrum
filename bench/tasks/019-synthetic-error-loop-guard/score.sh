#!/usr/bin/env bash
set -euo pipefail
run_dir="$1"; out="$run_dir/stdout.txt"; err="$run_dir/stderr.txt"; fail=0
ok(){ echo "ok: $1"; }
bad(){ echo "missing: $1"; fail=1; }
read_count=$(grep -Ec '^\[tool:read\]' "$err" || true)
if [ "$read_count" -ge 8 ]; then ok "missing reads ($read_count)"; else bad "missing reads ($read_count)"; fi
error_count=$(grep -Ec '^\[result:read error' "$err" || true)
if [ "$error_count" -ge 8 ]; then ok "read errors ($error_count)"; else bad "read errors ($error_count)"; fi
grep -Eiq '\[loop-guard\] 5 consecutive tool errors' "$err" && ok 'error nudge emitted' || bad 'error nudge emitted'
grep -Eiq '\[loop-guard\] 8 consecutive tool errors; requesting final response' "$err" && ok 'error force final emitted' || bad 'error force final emitted'
grep -Eiq 'final after missing read loop guard' "$out" && ok 'final response emitted' || bad 'final response emitted'
exit "$fail"
