#!/usr/bin/env bash
set -euo pipefail
run_dir="$1"; out="$run_dir/stdout.txt"; err="$run_dir/stderr.txt"; fail=0
ok(){ echo "ok: $1"; }
bad(){ echo "missing: $1"; fail=1; }
grep -Eiq '^\[tool:write\]' "$err" && ok 'write tool emitted' || bad 'write tool emitted'
grep -Eiq '^\[tool:read\]' "$err" && ok 'read tool emitted' || bad 'read tool emitted'
grep -Eiq 'ready from mixed batch' "$err" && ok 'read observed written content' || bad 'read observed written content'
grep -Eiq 'mixed batch read observed ready from mixed batch' "$out" && ok 'final confirms read saw content' || bad 'final confirms read saw content'
python3 - "$err" <<'PY' && echo 'ok: sequential write result before read call' || { echo 'missing: sequential write result before read call'; exit 1; }
import sys
lines=open(sys.argv[1], encoding='utf-8').read().splitlines()
write = next(i for i,l in enumerate(lines) if l.startswith('[tool:write]'))
write_result = next(i for i,l in enumerate(lines) if l.startswith('[result:write'))
read = next(i for i,l in enumerate(lines) if l.startswith('[tool:read]'))
read_result = next(i for i,l in enumerate(lines) if l.startswith('[result:read'))
raise SystemExit(0 if write < write_result < read < read_result else 1)
PY
exit "$fail"
