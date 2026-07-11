#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
version="$(cat release-version.txt)"
if [[ ! "$version" =~ ^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
  echo "invalid release-version.txt: $version" >&2
  exit 1
fi
cargo_version="$(cargo metadata --locked --no-deps --format-version 1 \
  | sed -n 's/.*"name":"ferrum","version":"\([^"]*\)".*/\1/p' \
  | head -n1)"
if [[ "$version" != "v$cargo_version" ]]; then
  echo "release-version.txt/Cargo.toml mismatch: $version vs v${cargo_version:-unknown}" >&2
  exit 1
fi

status=0
for file in README.md docs/release.md docs/ferrum.1.md docs/ferrum.1; do
  while IFS= read -r found; do
    [[ -z "$found" || "$found" == "$version" ]] && continue
    echo "$file contains stale install/release version $found; expected $version" >&2
    status=1
  done < <(grep -Eo 'v[0-9]+\.[0-9]+\.[0-9]+' "$file" | sort -u || true)
done

expected=(
  "ferrum-${version}-x86_64-unknown-linux-gnu.tar.gz"
  "ferrum-${version}-x86_64-unknown-linux-gnu.tar.gz.sha256"
  "ferrum_${version#v}_amd64.deb"
  "ferrum_${version#v}_amd64.deb.sha256"
  "ferrum-${version#v}-1.x86_64.rpm"
  "ferrum-${version#v}-1.x86_64.rpm.sha256"
)
for asset in "${expected[@]}"; do
  if ! grep -Fq "$asset" docs/release.md; then
    echo "docs/release.md is missing required asset $asset" >&2
    status=1
  fi
done
exit "$status"
