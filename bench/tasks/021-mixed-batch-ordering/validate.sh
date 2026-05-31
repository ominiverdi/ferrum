#!/usr/bin/env bash
set -euo pipefail
test "$(cat generated.txt)" = 'ready from mixed batch'
