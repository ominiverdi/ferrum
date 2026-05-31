#!/usr/bin/env bash
set -euo pipefail
mkdir -p home/.config/systemd/user home/.local/bin logs bin
cat > home/.config/systemd/user/pi-voice-key.service <<'UNIT'
[Unit]
Description=Pi voice key daemon

[Service]
ExecStart=/home/ominiverdi/.local/bin/pi-voice-key-daemon
Restart=always
Environment=DISPLAY=:0

[Install]
WantedBy=default.target
UNIT
cat > home/.local/bin/pi-voice-key-daemon <<'PY'
#!/usr/bin/env python3
from pathlib import Path
import subprocess

XDO = str(Path.home() / ".local/bin/xdotool")

def active_window_class():
    return subprocess.check_output([XDO, "getactivewindow", "getwindowclassname"], text=True).strip().lower()

def paste(text):
    subprocess.run([XDO, "type", text], check=True)
PY
cat > logs/pi-voice-key.journal <<'LOG'
May 30 06:57:46 minto pi-voice-key-daemon[1200]: recorded /tmp/pi-voice-key.wav
May 30 06:57:48 minto pi-voice-key-daemon[1200]: transcript: hello world
May 30 06:57:48 minto pi-voice-key-daemon[1200]: xdotool: Unknown command: getwindowclassname
May 30 06:57:48 minto pi-voice-key-daemon[1200]: paste failed with exit status 1
LOG
cat > bin/xdotool-help.txt <<'HELP'
Usage: xdotool <cmd> <args>

Available commands:
  getactivewindow
  getwindowfocus
  getwindowname
  getwindowpid
  key
  type
  sleep
  search
  windowactivate
  windowfocus
  windowraise

There is no command named getwindowclassname in this build.
Use xprop -id $(xdotool getactivewindow) WM_CLASS to inspect a window class.
HELP
