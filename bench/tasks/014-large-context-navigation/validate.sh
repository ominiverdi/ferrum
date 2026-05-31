#!/usr/bin/env bash
set -euo pipefail
python3 -m pytest -q
if git diff -- test_menu.py docs legacy src/generated | grep -q .; then
  echo 'modified forbidden files' >&2
  exit 1
fi
grep -q 'Customer Billing' src/app/routing.py
