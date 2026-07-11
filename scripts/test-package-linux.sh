#!/usr/bin/env bash
set -euo pipefail
umask 077

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
temp="$(mktemp -d)"
trap 'rm -rf "$temp"' EXIT

if "$root/scripts/package-linux.sh" not-a-version > "$temp/invalid.out" 2> "$temp/invalid.err"; then
  echo "package-linux.sh accepted an invalid version" >&2
  exit 1
fi
grep -Fq 'usage:' "$temp/invalid.err"

if FERRUM_PACKAGE_ALLOW_DIRTY=1 "$root/scripts/package-linux.sh" v9.9.9 \
  > "$temp/mismatch.out" 2> "$temp/mismatch.err"; then
  echo "package-linux.sh accepted a Cargo/version mismatch" >&2
  exit 1
fi
grep -Fq 'release version mismatch' "$temp/mismatch.err"

if SOURCE_DATE_EPOCH=1 FERRUM_SOURCE_COMMIT=0000000000000000000000000000000000000000 \
  FERRUM_BUILDER_IMAGE=test FERRUM_PACKAGE_OUTPUT_DIR="$temp" \
  "$root/scripts/package-linux-inner.sh" v9.9.9 \
  > "$temp/inner.out" 2> "$temp/inner.err"; then
  echo "package-linux-inner.sh accepted a Cargo/version mismatch" >&2
  exit 1
fi
grep -Fq 'release version mismatch' "$temp/inner.err"

partial_repo="$temp/partial-repo"
mkdir -p "$partial_repo/scripts" "$partial_repo/packaging/linux" "$partial_repo/src" "$partial_repo/dist"
cp "$root/scripts/package-linux.sh" "$partial_repo/scripts/"
printf '#!/usr/bin/env bash\n' > "$partial_repo/scripts/package-linux-inner.sh"
printf 'FROM scratch\n' > "$partial_repo/packaging/linux/Dockerfile"
printf '[package]\nname = "ferrum"\nversion = "0.1.0"\nedition = "2024"\n' > "$partial_repo/Cargo.toml"
printf 'fn main() {}\n' > "$partial_repo/src/main.rs"
(
  cd "$partial_repo"
  cargo generate-lockfile --quiet
  git init -q
  git config user.name test
  git config user.email test@invalid
  git add .
  git commit -qm baseline
)
printf 'plausible stale asset\n' > "$partial_repo/dist/ferrum-v0.1.0.tar.gz"
cat > "$temp/failing-runtime" <<'FAIL'
#!/usr/bin/env bash
exit 1
FAIL
chmod 0755 "$temp/failing-runtime"
if FERRUM_PACKAGE_ALLOW_DIRTY=1 PACKAGE_CONTAINER_RUNTIME="$temp/failing-runtime" \
  "$partial_repo/scripts/package-linux.sh" v0.1.0 \
  > "$temp/partial.out" 2> "$temp/partial.err"; then
  echo "package-linux.sh succeeded with a failing builder" >&2
  exit 1
fi
[[ ! -e "$partial_repo/dist" ]] || {
  echo "failed packaging left a plausible dist directory" >&2
  exit 1
}

before="$(sha256sum "$root/Cargo.toml" | cut -d' ' -f1)"
bash -n "$root/scripts/package-linux.sh" "$root/scripts/package-linux-inner.sh"
after="$(sha256sum "$root/Cargo.toml" | cut -d' ' -f1)"
[[ "$before" == "$after" ]] || { echo "package tests modified Cargo.toml" >&2; exit 1; }

grep -Fq 'cargo build --locked --release' "$root/scripts/package-linux-inner.sh"
grep -Fq 'dpkg-shlibdeps' "$root/scripts/package-linux-inner.sh"
grep -Fq -- '--auto-req builtin' "$root/scripts/package-linux-inner.sh"
grep -Fq 'mv -T -- "$stage" "$root/dist"' "$root/scripts/package-linux.sh"
grep -Eq '^FROM .+@sha256:[0-9a-f]{64}$' "$root/packaging/linux/Dockerfile"
grep -Fq 'snapshot.debian.org' "$root/packaging/linux/Dockerfile"

for workflow in "$root/.github/workflows/ci.yml" "$root/.github/workflows/release.yml"; do
  if grep -Eq 'uses: [^ ]+@(v[0-9]+|stable|main)([[:space:]#]|$)' "$workflow"; then
    echo "workflow contains a mutable action reference: $workflow" >&2
    exit 1
  fi
  while IFS= read -r command; do
    [[ "$command" == *'--locked'* ]] || {
      echo "workflow Cargo validation is missing --locked: $command" >&2
      exit 1
    }
  done < <(grep -E 'run: cargo (test|build|clippy)( |$)' "$workflow" || true)
done
grep -Fq 'files: dist/*' "$root/.github/workflows/release.yml"
