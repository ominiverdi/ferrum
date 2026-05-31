#!/usr/bin/env bash
set -euo pipefail
cat > loop.txt <<'TXT'
This file exists so the fake provider can repeatedly read it.
TXT
