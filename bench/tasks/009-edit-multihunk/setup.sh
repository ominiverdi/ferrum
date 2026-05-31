#!/usr/bin/env bash
set -euo pipefail
cat > app.py <<'PY'
def one():
    return "old-one"

def two():
    return "old-two"
PY
