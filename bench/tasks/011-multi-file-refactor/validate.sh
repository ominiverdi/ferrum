#!/usr/bin/env bash
set -euo pipefail
python3 -m pytest -q
if git diff -- test_totals.py | grep -q .; then
  echo 'tests were modified' >&2
  exit 1
fi
test -f pricing.py
grep -q 'from pricing import discounted_price' calc.py
grep -q 'from pricing import discounted_price' invoice.py
grep -q 'from pricing import discounted_price' quotes.py
! grep -R "price - (price \* percent / 100)" -n calc.py invoice.py quotes.py
