#!/usr/bin/env bash
set -euo pipefail
test -f logs/app.log
test -f .gitignore
