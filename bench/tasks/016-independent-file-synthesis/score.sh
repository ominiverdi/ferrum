#!/usr/bin/env bash
set -euo pipefail
run_dir="$1"; out="$run_dir/stdout.txt"; err="$run_dir/stderr.txt"; fail=0
ok(){ echo "ok: $1"; }
bad(){ echo "missing: $1"; fail=1; }
grep -Eiq 'services/reports\.toml' "$out" && ok 'cites reports path' || bad 'cites reports path'
grep -Eiq '(^|[^0-9])9000([^0-9]|$)' "$out" && ok 'reports actual timeout' || bad 'reports actual timeout'
grep -Eiq '(^|[^0-9])1200([^0-9]|$)' "$out" && ok 'reports expected timeout' || bad 'reports expected timeout'
grep -Eiq 'reports' "$out" && ok 'identifies reports service' || bad 'identifies reports service'
agent="$(sed -n 's/^agent=//p' "$run_dir/result.env" 2>/dev/null || true)"
case "$agent" in
  ferrum-*)
    grep -Eiq '\[tool:find\]|\[tool:ls\]' "$err" && ok 'locates service files' || bad 'locates service files'
    read_count=$(grep -Ec '^\[tool:read\]' "$err" || true)
    if [ "$read_count" -ge 6 ]; then ok 'reads service files'; else bad "reads service files ($read_count)"; fi
    python3 - "$err" <<'PY' && echo 'ok: has adjacent read batch' || { echo 'missing: has adjacent read batch'; exit 1; }
import sys
lines=open(sys.argv[1], encoding='utf-8').read().splitlines()
last=None
for i,line in enumerate(lines):
    if line.startswith('[tool:read]'):
        if last is not None and not any(l.startswith('[result:') for l in lines[last+1:i]):
            raise SystemExit(0)
        last=i
raise SystemExit(1)
PY
    ;;
  *)
    echo "skip: tool trace assertions unavailable for $agent text-mode transcript"
    ;;
esac
exit "$fail"
