#!/usr/bin/env bash
set -euo pipefail
cat > handlers.py <<'PY'
def handle_alpha(event):
    status = "pending"
    if event.get("enabled"):
        status = "ready"
    return {"name": "alpha", "status": status}


def handle_beta(event):
    status = "pending"
    if event.get("enabled"):
        status = "ready"
    return {"name": "beta", "status": status}
PY
cat > test_handlers.py <<'PY'
from handlers import handle_alpha, handle_beta


def test_alpha_unchanged():
    assert handle_alpha({"enabled": True}) == {"name": "alpha", "status": "ready"}
    assert handle_alpha({"enabled": False}) == {"name": "alpha", "status": "pending"}


def test_beta_uses_queued_status_when_disabled():
    assert handle_beta({"enabled": True}) == {"name": "beta", "status": "ready"}
    assert handle_beta({"enabled": False}) == {"name": "beta", "status": "queued"}
PY
cat > .gitignore <<'EOF2'
__pycache__/
*.pyc
.pytest_cache/
EOF2
