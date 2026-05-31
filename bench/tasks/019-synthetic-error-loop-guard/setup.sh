#!/usr/bin/env bash
set -euo pipefail
cat > existing.txt <<'TXT'
The fake provider intentionally reads missing files for this task.
TXT
