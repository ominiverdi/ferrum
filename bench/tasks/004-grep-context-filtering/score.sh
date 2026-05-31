#!/usr/bin/env bash
set -euo pipefail
run_dir="$1"
out="$run_dir/stdout.txt"
err="$run_dir/stderr.txt"
fail=0
require_out() {
  local pattern="$1" label="$2"
  if grep -Eiq "$pattern" "$out"; then echo "ok: $label"; else echo "missing: $label"; fail=1; fi
}
reject_out() {
  local pattern="$1" label="$2"
  if grep -Eiq "$pattern" "$out"; then echo "forbidden: $label"; fail=1; else echo "ok absent: $label"; fi
}
require_err() {
  local pattern="$1" label="$2"
  if grep -Eiq "$pattern" "$err"; then echo "ok: $label"; else echo "missing: $label"; fail=1; fi
}
require_out 'home/.local/bin/pi-voice-key-daemon|pi-voice-key-daemon' 'mentions daemon file'
require_out 'logs/pi-voice-key.journal|journal' 'mentions journal file'
require_out 'active_window_class|daemon function|function' 'includes surrounding function context'
require_out 'Unknown command|getwindowclassname' 'mentions journal failure'
reject_out 'node_modules|target/debug|dependency noise|target noise' 'does not report ignored dependency/build noise'
require_err '\[tool:grep\]' 'uses native grep tool'
# Pre-fix weakness marker: current grep has no context argument, so models often compensate with reads/bash.
# This is informational unless a broad bash grep/find appears.
if grep -Eiq '\[tool:bash\].*(grep|find)|grep -R|find \. ' "$err"; then
  echo "forbidden: broad bash filesystem search"
  fail=1
else
  echo "ok absent: broad bash filesystem search"
fi
exit "$fail"
