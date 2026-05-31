#!/usr/bin/env bash
set -euo pipefail
run_dir="$1"
out="$run_dir/stdout.txt"
fail=0
require() {
  local pattern="$1"
  local label="$2"
  if ! grep -Eiq "$pattern" "$out"; then
    echo "missing: $label"
    fail=1
  else
    echo "ok: $label"
  fi
}
reject() {
  local pattern="$1"
  local label="$2"
  if grep -Eiq "$pattern" "$out"; then
    echo "forbidden: $label"
    fail=1
  else
    echo "ok absent: $label"
  fi
}
require 'getwindowclassname' 'mentions failing subcommand'
require 'unknown command|unsupported|invalid|no command named|not (a )?(valid|supported)' 'says command is unsupported/unknown/invalid'
require 'xdotool-help\.txt|journal|log line|Unknown command' 'cites local evidence'
require 'xprop|WM_CLASS' 'mentions xprop/WM_CLASS replacement'
reject 'getwindowclassname is (a )?(normal|valid|supported)' 'does not claim getwindowclassname is normal/valid/supported'
exit "$fail"
