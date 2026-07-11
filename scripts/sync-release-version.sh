#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 1 || ! "$1" =~ ^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
  echo "usage: $0 vX.Y.Z" >&2
  exit 2
fi
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
old="$(cat release-version.txt)"
new="$1"
[[ "$old" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "invalid current release version" >&2; exit 1; }
old_plain="${old#v}"
new_plain="${new#v}"
for file in README.md docs/release.md docs/ferrum.1.md docs/ferrum.1; do
  sed -i "s/${old_plain//./\\.}/${new_plain}/g" "$file"
done
printf '%s\n' "$new" > release-version.txt
scripts/check-release-docs.sh
printf 'Synchronized release install documentation to %s\n' "$new"
