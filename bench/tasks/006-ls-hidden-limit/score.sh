#!/usr/bin/env bash
set -euo pipefail
run_dir="$1"; out="$run_dir/stdout.txt"; err="$run_dir/stderr.txt"; fail=0
check(){ grep -Eiq "$1" "$2" && echo "ok: $3" || { echo "missing: $3"; fail=1; }; }
agent="$(sed -n 's/^agent=//p' "$run_dir/result.env" 2>/dev/null || true)"
case "$agent" in
  ferrum-*)
    check '\[tool:ls\]' "$err" 'uses ls'
    check 'limit: 3' "$err" 'passes ls limit'
    check '\.config/' "$err" 'shows hidden directory with slash'
    check 'entries limit reached' "$err" 'shows limit notice'
    ;;
  *)
    check '\.config/' "$out" 'answer says hidden directory visible'
    check 'trailing /|suffix|directories have' "$out" 'answer says directory suffix visible'
    check 'limit notice|entries limit reached' "$out" 'answer mentions limit notice'
    ;;
esac
exit "$fail"
