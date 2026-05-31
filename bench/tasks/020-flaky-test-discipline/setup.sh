#!/usr/bin/env bash
set -euo pipefail
cat > calculator.py <<'PY'
def add(a, b):
    return a + b


def multiply(a, b):
    return a * b
PY
cat > test_calculator.py <<'PY'
from pathlib import Path
from calculator import add, multiply


def test_flaky_environment_marker_only_once():
    marker = Path(".flake_once")
    if marker.exists():
        marker.unlink()
        assert False, "transient fixture marker; rerun tests"


def test_calculator_math_is_correct():
    assert add(2, 3) == 5
    assert multiply(4, 5) == 20
PY
cat > .flake_once <<'TXT'
first test run should fail and remove this marker
TXT
cat > .gitignore <<'EOF2'
__pycache__/
*.pyc
.pytest_cache/
.flake_once
EOF2
