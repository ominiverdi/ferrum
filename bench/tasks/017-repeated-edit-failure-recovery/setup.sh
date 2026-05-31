#!/usr/bin/env bash
set -euo pipefail
cat > workflow.py <<'PY'
def process_primary(job):
    status = "pending"
    if job.get("approved"):
        status = "ready"
    return {"lane": "primary", "status": status}


def process_secondary(job):
    status = "pending"
    if job.get("approved"):
        status = "ready"
    return {"lane": "secondary", "status": status}
PY
cat > test_workflow.py <<'PY'
from workflow import process_primary, process_secondary


def test_primary_stays_pending_when_not_approved():
    assert process_primary({"approved": False}) == {"lane": "primary", "status": "pending"}
    assert process_primary({"approved": True}) == {"lane": "primary", "status": "ready"}


def test_secondary_waits_for_review_when_not_approved():
    assert process_secondary({"approved": False}) == {"lane": "secondary", "status": "review"}
    assert process_secondary({"approved": True}) == {"lane": "secondary", "status": "ready"}
PY
cat > .gitignore <<'EOF2'
__pycache__/
*.pyc
.pytest_cache/
EOF2
