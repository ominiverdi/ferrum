#!/usr/bin/env bash
set -euo pipefail
run_dir="$1"; out="$run_dir/stdout.txt"; err="$run_dir/stderr.txt"; diff="$run_dir/diff.patch"; fail=0
check(){ if "$@"; then echo "ok: $*"; else echo "missing: $*"; fail=1; fi; }
check grep -q 'pricing.py' "$diff"
check grep -Eq 'def discounted_price' "$diff"
check grep -Eq '^\+from pricing import discounted_price' "$diff"
agent="$(sed -n 's/^agent=//p' "$run_dir/result.env" 2>/dev/null || true)"
case "$agent" in
  ferrum-*)
    check grep -Eiq '\[tool:read\]|\[tool:grep\]|\[tool:find\]' "$err"
    check grep -Eiq '\[tool:bash\]' "$err"
    ;;
  *)
    echo "skip: tool trace assertions unavailable for $agent text-mode transcript"
    ;;
esac
check grep -Eiq 'pytest|tests? pass|passed' "$out" "$err"
changed_files=$(git -C "$run_dir/work" diff --name-only | grep -v '^__pycache__/' | wc -l)
if [ "$changed_files" -le 4 ]; then echo "ok: minimal changed files"; else echo "too many changed files: $changed_files"; fail=1; fi
exit "$fail"
