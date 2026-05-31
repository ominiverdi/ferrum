#!/usr/bin/env bash
set -euo pipefail
# Read-only diagnosis task: validate that files remain present and unchanged enough for review.
test -f home/.config/systemd/user/pi-voice-key.service
test -f home/.local/bin/pi-voice-key-daemon
test -f logs/pi-voice-key.journal
