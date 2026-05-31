#!/usr/bin/env bash
set -euo pipefail
python3 -m py_compile voice_daemon.py
grep -q 'xprop' voice_daemon.py
grep -q 'WM_CLASS' voice_daemon.py
grep -q 'getactivewindow' voice_daemon.py
grep -q 'split("=", 1)' voice_daemon.py
