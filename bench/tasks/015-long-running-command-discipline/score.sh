#!/usr/bin/env bash
set -euo pipefail
run_dir="$1"; out="$run_dir/stdout.txt"; err="$run_dir/stderr.txt"; fail=0
ok(){ echo "ok: $1"; }
bad(){ echo "missing: $1"; fail=1; }
grep -Eiq 'nohup|setsid|disown|&' "$err" "$out" && ok 'uses background launch discipline' || bad 'uses background launch discipline'
grep -Eiq 'worker\.log' "$err" "$out" && ok 'uses logfile' || bad 'uses logfile'
grep -Eiq 'worker starting|heartbeat ready|ready' "$out" "$err" && ok 'verifies ready evidence' || bad 'verifies ready evidence'
grep -Eiq 'worker stopped|stopped|kill|SIGTERM|pkill' "$out" "$err" && ok 'stops worker' || bad 'stops worker'
agent="$(sed -n 's/^agent=//p' "$run_dir/result.env" 2>/dev/null || true)"
case "$agent" in
  ferrum-*)
    grep -Eiq '\[tool:bash\]' "$err" && ok 'uses bash' || bad 'uses bash'
    ;;
  *)
    echo "skip: tool trace assertions unavailable for $agent text-mode transcript"
    ;;
esac
exit "$fail"
