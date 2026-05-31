#!/usr/bin/env bash
set -euo pipefail
grep -q 'new-one' app.py
grep -q 'new-two' app.py
! grep -q 'old-one\|old-two' app.py
