#!/usr/bin/env bash
set -euo pipefail
test -f existing.txt
if git diff -- . | grep -q .; then
  echo 'files were modified' >&2
  git diff -- . >&2
  exit 1
fi
