#!/usr/bin/env bash
set -euo pipefail
cat > worker.py <<'PY'
import signal
import sys
import time

running = True

def stop(signum, frame):
    global running
    running = False

signal.signal(signal.SIGTERM, stop)
print("worker starting", flush=True)
while running:
    print("heartbeat ready", flush=True)
    time.sleep(0.2)
print("worker stopped", flush=True)
PY
cat > README.md <<'MD'
# worker fixture

The worker is a foreground process by default. Agents must not run it attached in a way that blocks the turn.
MD
