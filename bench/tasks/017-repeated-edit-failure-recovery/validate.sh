#!/usr/bin/env bash
set -euo pipefail
python3 -m pytest -q
if git diff -- test_workflow.py | grep -q .; then
  echo 'tests were modified' >&2
  exit 1
fi
python3 - <<'PY'
from workflow import process_primary, process_secondary
assert process_primary({"approved": False}) == {"lane": "primary", "status": "pending"}
assert process_secondary({"approved": False}) == {"lane": "secondary", "status": "review"}
PY
