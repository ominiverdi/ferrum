#!/usr/bin/env bash
set -euo pipefail
cat > voice_daemon.py <<'PY'
from pathlib import Path
import subprocess


def active_window_class():
    try:
        return subprocess.check_output([str(Path.home() / ".local/bin/xdotool"), "getactivewindow", "getwindowclassname"], text=True, stderr=subprocess.DEVNULL).strip().lower()
    except Exception:
        return ""


def should_paste_with_clipboard():
    return "ghostty" in active_window_class()
PY
