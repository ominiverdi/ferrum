#!/usr/bin/env bash
set -euo pipefail
test -f worker.log
grep -q 'worker starting' worker.log
grep -q 'heartbeat ready' worker.log
grep -q 'worker stopped' worker.log
if pgrep -f "$(pwd)/worker.py" >/dev/null; then
  echo 'worker still running' >&2
  pgrep -af "$(pwd)/worker.py" >&2 || true
  exit 1
fi
