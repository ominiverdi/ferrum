#!/usr/bin/env bash
set -euo pipefail
mkdir -p docs legacy src/app src/generated vendor logs
cat > .gitignore <<'EOF2'
__pycache__/
*.pyc
.pytest_cache/
vendor/
logs/
EOF2
cat > README.md <<'MD'
# routing fixture

Legacy note: route labels used to be defined in `legacy/routes_old.py`. Do not use this for the current app.
MD
cat > legacy/routes_old.py <<'PY'
ROUTE_LABELS = {
    "home": "Homepage",
    "billing": "Billing",
}
PY
for i in $(seq 1 80); do
  cat > "docs/noise_${i}.md" <<MD
# Noise ${i}
The display route label may appear in documentation, legacy examples, or generated code.
This file is not imported by the app.
MD
done
for i in $(seq 1 40); do
  cat > "src/generated/route_stub_${i}.py" <<PY
# generated stub ${i}
ROUTE_LABEL = "Generated ${i}"
PY
done
cat > src/app/routing.py <<'PY'
ROUTES = {
    "home": {"path": "/", "label": "Home"},
    "billing": {"path": "/billing", "label": "Billing"},
}


def label_for(route_name):
    return ROUTES[route_name]["label"]
PY
cat > src/app/menu.py <<'PY'
from .routing import label_for


def menu_labels(route_names):
    return [label_for(name) for name in route_names]
PY
cat > test_menu.py <<'PY'
from src.app.menu import menu_labels


def test_billing_label_is_customer_billing():
    assert menu_labels(["home", "billing"]) == ["Home", "Customer Billing"]
PY
