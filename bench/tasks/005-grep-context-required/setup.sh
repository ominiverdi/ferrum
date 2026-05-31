#!/usr/bin/env bash
set -euo pipefail
mkdir -p logs node_modules target
{
  echo '000 prelude'
  echo '001 cause: clipboard helper invoked unsupported xdotool subcommand getwindowclassname'
  echo '002 CRITICAL_FAILURE paste failed'
  for i in $(seq 3 160); do
    printf '%03d CRITICAL_FAILURE repeated noisy failure without cause\n' "$i"
  done
} > logs/app.log
mkdir -p node_modules/pkg target/debug
printf 'CRITICAL_FAILURE dependency noise\n' > node_modules/pkg/noise.log
printf 'CRITICAL_FAILURE build noise\n' > target/debug/noise.log
cat > .gitignore <<'EOF2'
node_modules/
target/
EOF2
