#!/usr/bin/env bash
set -euo pipefail
python3 -m pytest -q
if git diff -- calculator.py test_calculator.py | grep -q .; then
  echo 'tracked code or tests were modified' >&2
  git diff -- calculator.py test_calculator.py >&2
  exit 1
fi
test ! -f .flake_once
