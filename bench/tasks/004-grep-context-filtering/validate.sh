#!/usr/bin/env bash
set -euo pipefail
test -f home/.local/bin/pi-voice-key-daemon
test -f logs/pi-voice-key.journal
test -f .gitignore
