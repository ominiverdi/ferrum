#!/usr/bin/env bash
set -euo pipefail
test -f home/.config/systemd/user/pi-voice-key.service
test -f home/.local/bin/pi-voice-key-daemon
test -f logs/pi-voice-key.journal
test -f bin/xdotool-help.txt
