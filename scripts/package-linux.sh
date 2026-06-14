#!/usr/bin/env bash
set -euo pipefail

version="${1:-}"
if [[ -z "$version" ]]; then
  version="v$(cargo metadata --no-deps --format-version 1 | sed -n 's/.*"version":"\([^"]*\)".*/\1/p' | head -n1)"
fi

if [[ ! "$version" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "usage: $0 [vX.Y.Z]" >&2
  exit 2
fi

plain_version="${version#v}"
target="x86_64-unknown-linux-gnu"
out_dir="dist"
package="ferrum-${version}-${target}"

if [[ ! -x target/release/ferrum ]]; then
  echo "target/release/ferrum not found; run cargo build --release first" >&2
  exit 1
fi

rm -rf "$out_dir"
mkdir -p "$out_dir"

# Portable tarball.
rm -rf "$out_dir/$package"
mkdir -p "$out_dir/$package/docs"
cp target/release/ferrum "$out_dir/$package/"
cp README.md LICENSE "$out_dir/$package/"
cp docs/ferrum.1 "$out_dir/$package/docs/"
(
  cd "$out_dir"
  tar -czf "${package}.tar.gz" "$package"
  sha256sum "${package}.tar.gz" > "${package}.tar.gz.sha256"
)
rm -rf "$out_dir/$package"

# Debian package.
deb_root="$out_dir/debroot"
deb_name="ferrum_${plain_version}_amd64.deb"
rm -rf "$deb_root"
mkdir -p \
  "$deb_root/DEBIAN" \
  "$deb_root/usr/bin" \
  "$deb_root/usr/share/doc/ferrum" \
  "$deb_root/usr/share/man/man1"
install -m 0755 target/release/ferrum "$deb_root/usr/bin/ferrum"
install -m 0644 README.md "$deb_root/usr/share/doc/ferrum/README.md"
install -m 0644 LICENSE "$deb_root/usr/share/doc/ferrum/LICENSE"
gzip -9c docs/ferrum.1 > "$deb_root/usr/share/man/man1/ferrum.1.gz"
installed_size="$(du -sk "$deb_root/usr" | cut -f1)"
cat > "$deb_root/DEBIAN/control" <<EOF
Package: ferrum
Version: ${plain_version}
Section: utils
Priority: optional
Architecture: amd64
Maintainer: ominiverdi
Installed-Size: ${installed_size}
Homepage: https://codeberg.org/ominiverdi/ferrum
Description: Small Rust-native Linux coding agent
 Ferrum is a small, fast, Rust-native Linux coding agent with
 provider-neutral tools, JSONL sessions, MCP support, image input,
 usage accounting, and OpenAI-compatible providers.
EOF
dpkg-deb --build --root-owner-group "$deb_root" "$out_dir/$deb_name"
(
  cd "$out_dir"
  sha256sum "$deb_name" > "${deb_name}.sha256"
)
rm -rf "$deb_root"

# RPM package. cargo-generate-rpm currently reads metadata from Cargo.toml,
# so use a temporary metadata file and restore Cargo.toml immediately after.
rpm_name="ferrum-${plain_version}-1.x86_64.rpm"
if command -v cargo-generate-rpm >/dev/null 2>&1 || cargo generate-rpm --version >/dev/null 2>&1; then
  tmp_cargo="$(mktemp)"
  cp Cargo.toml "$tmp_cargo"
  cat >> Cargo.toml <<'EOF'

[package.metadata.generate-rpm]
auto-req = "disabled"

[[package.metadata.generate-rpm.assets]]
source = "target/release/ferrum"
dest = "/usr/bin/ferrum"
mode = "0755"

[[package.metadata.generate-rpm.assets]]
source = "README.md"
dest = "/usr/share/doc/ferrum/README.md"
doc = true
mode = "0644"

[[package.metadata.generate-rpm.assets]]
source = "LICENSE"
dest = "/usr/share/doc/ferrum/LICENSE"
doc = true
mode = "0644"

[[package.metadata.generate-rpm.assets]]
source = "docs/ferrum.1"
dest = "/usr/share/man/man1/ferrum.1"
mode = "0644"
EOF
  restore_cargo() {
    cp "$tmp_cargo" Cargo.toml
    rm -f "$tmp_cargo"
  }
  trap restore_cargo EXIT
  cargo generate-rpm --output "$out_dir"
  restore_cargo
  trap - EXIT
  (
    cd "$out_dir"
    sha256sum "$rpm_name" > "${rpm_name}.sha256"
  )
else
  echo "cargo-generate-rpm not found; install with: cargo install cargo-generate-rpm" >&2
  exit 1
fi

printf 'Built release assets in %s:\n' "$out_dir"
ls -lh "$out_dir" | sed -n '1,20p'
