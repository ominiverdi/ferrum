#!/usr/bin/env bash
set -euo pipefail
for i in $(seq 1 200); do printf 'line %03d\n' "$i"; done > big.txt
