#!/usr/bin/env bash
set -euo pipefail
cat > .gitignore <<'EOF'
__pycache__/
*.pyc
EOF
cat > calc.py <<'PY'
def apply_discount(price, percent):
    return round(price - (price * percent / 100), 2)

def subtotal(items):
    total = 0
    for item in items:
        total += apply_discount(item["price"], item.get("discount", 0))
    return round(total, 2)
PY
cat > invoice.py <<'PY'
def discounted_price(price, percent):
    return round(price - (price * percent / 100), 2)

def invoice_total(lines):
    total = 0
    for line in lines:
        total += discounted_price(line["price"], line.get("discount", 0))
    return round(total, 2)
PY
cat > quotes.py <<'PY'
def quote_discount(price, percent):
    return round(price - (price * percent / 100), 2)

def quote_total(lines):
    total = 0
    for line in lines:
        total += quote_discount(line["price"], line.get("discount", 0))
    return round(total, 2)
PY
cat > test_totals.py <<'PY'
from calc import subtotal
from invoice import invoice_total
from quotes import quote_total
from pricing import discounted_price

LINES = [
    {"price": 19.995, "discount": 10},
    {"price": 5.005, "discount": 0},
]

def test_totals_share_precise_discount_rounding():
    assert subtotal(LINES) == 23.01
    assert invoice_total(LINES) == 23.01
    assert quote_total(LINES) == 23.01

def test_discount_helper_rounds_half_up_to_cents():
    assert discounted_price(19.995, 10) == 18.0
PY
