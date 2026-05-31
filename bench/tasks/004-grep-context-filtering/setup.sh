#!/usr/bin/env bash
set -euo pipefail
mkdir -p home/.config/systemd/user home/.local/bin logs node_modules/pkg target/debug src
cat > .gitignore <<'GITIGNORE'
node_modules/
target/
GITIGNORE
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
cat > home/.config/systemd/user/pi-voice-key.service <<'UNIT'
[Service]
ExecStart=/home/ominiverdi/.local/bin/pi-voice-key-daemon
Environment=DISPLAY=:0
UNIT
cat > logs/pi-voice-key.journal <<'LOG'
May 30 06:57:48 minto pi-voice-key-daemon[1200]: xdotool: Unknown command: getwindowclassname
May 30 06:57:48 minto pi-voice-key-daemon[1200]: paste failed with exit status 1
LOG
cat > node_modules/pkg/noise.py <<'PY'
# ignored dependency noise
print("getwindowclassname")
PY
cat > target/debug/noise.log <<'LOG'
getwindowclassname target noise
LOG
cat > src/notes.txt <<'TXT'
This file is unrelated.
TXT
