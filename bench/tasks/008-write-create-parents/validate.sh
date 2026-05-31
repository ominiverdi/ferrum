#!/usr/bin/env bash
set -euo pipefail
test "$(cat nested/config/example.txt)" = $'alpha\nbeta'
