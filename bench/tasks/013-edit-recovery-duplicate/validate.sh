#!/usr/bin/env bash
set -euo pipefail
python3 -m pytest -q
if git diff -- test_handlers.py | grep -q .; then
  echo 'tests were modified' >&2
  exit 1
fi
grep -A4 'def handle_alpha' handlers.py | grep -q 'status = "pending"'
grep -A4 'def handle_beta' handlers.py | grep -q 'status = "queued"'
