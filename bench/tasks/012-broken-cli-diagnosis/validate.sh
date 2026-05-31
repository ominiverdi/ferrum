#!/usr/bin/env bash
set -euo pipefail
python3 -m pytest -q
if git diff -- test_tinydeploy.py | grep -q .; then
  echo 'tests were modified' >&2
  exit 1
fi
grep -q 'config.update(json.load(fh))' tinydeploy.py
grep -q 'TINYDEPLOY_ENDPOINT' tinydeploy.py
