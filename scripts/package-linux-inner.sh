#!/usr/bin/env bash
set -euo pipefail
umask 022

version="${1:-}"
if [[ ! "$version" =~ ^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?$ ]]; then
  echo "usage: $0 vX.Y.Z" >&2
  exit 2
fi
plain_version="${version#v}"

: "${SOURCE_DATE_EPOCH:?SOURCE_DATE_EPOCH is required}"
: "${FERRUM_SOURCE_COMMIT:?FERRUM_SOURCE_COMMIT is required}"
: "${FERRUM_BUILDER_IMAGE:?FERRUM_BUILDER_IMAGE is required}"
[[ "$SOURCE_DATE_EPOCH" =~ ^[0-9]+$ ]] || { echo "invalid SOURCE_DATE_EPOCH" >&2; exit 2; }
[[ "$FERRUM_SOURCE_COMMIT" =~ ^[0-9a-f]{40}$ ]] || { echo "invalid FERRUM_SOURCE_COMMIT" >&2; exit 2; }

cargo_version="$(cargo metadata --locked --no-deps --format-version 1 \
  | sed -n 's/.*"name":"ferrum","version":"\([^"]*\)".*/\1/p' \
  | head -n1)"
if [[ -z "$cargo_version" || "$plain_version" != "$cargo_version" ]]; then
  echo "release version mismatch: tag=$version Cargo.toml=${cargo_version:-unknown}" >&2
  exit 1
fi

host_target="$(rustc -vV | sed -n 's/^host: //p')"
target="${FERRUM_RELEASE_TARGET:-$host_target}"
case "$target" in
  x86_64-unknown-linux-gnu)
    deb_arch="amd64"
    rpm_arch="x86_64"
    ;;
  aarch64-unknown-linux-gnu)
    deb_arch="arm64"
    rpm_arch="aarch64"
    ;;
  *)
    echo "unsupported release target: $target" >&2
    exit 1
    ;;
esac
if [[ "$target" != "$host_target" ]]; then
  echo "cross-packaging is not supported by this builder: target=$target host=$host_target" >&2
  exit 1
fi

out_dir="${FERRUM_PACKAGE_OUTPUT_DIR:-/output}"
if [[ ! -d "$out_dir" ]]; then
  echo "package output directory does not exist: $out_dir" >&2
  exit 1
fi
if find "$out_dir" -mindepth 1 -print -quit | grep -q .; then
  echo "package output directory must be empty: $out_dir" >&2
  exit 1
fi

export CARGO_INCREMENTAL=0
export SOURCE_DATE_EPOCH
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=/workspace=. -C link-arg=-Wl,--build-id=none"
cargo build --locked --release --target "$target"
binary="${CARGO_TARGET_DIR:-target}/$target/release/ferrum"
[[ -x "$binary" ]] || { echo "built binary missing: $binary" >&2; exit 1; }

binary_version="$($binary --version)"
if [[ "$binary_version" != "ferrum $plain_version" ]]; then
  echo "built binary version mismatch: expected 'ferrum $plain_version', got '$binary_version'" >&2
  exit 1
fi
binary_target="$(file -b "$binary")"
case "$target:$binary_target" in
  x86_64-unknown-linux-gnu:*x86-64*) ;;
  aarch64-unknown-linux-gnu:*aarch64*) ;;
  *) echo "built binary architecture mismatch: target=$target file=$binary_target" >&2; exit 1 ;;
esac

stage="$(mktemp -d /build/package-stage.XXXXXXXXXX)"
trap 'rm -rf "$stage"' EXIT
package="ferrum-${version}-${target}"
deb_name="ferrum_${plain_version}_${deb_arch}.deb"
rpm_name="ferrum-${plain_version}-1.${rpm_arch}.rpm"

# Derive Debian runtime dependencies from the controlled-build binary.
deb_root="$stage/debroot"
mkdir -p \
  "$deb_root/DEBIAN" \
  "$deb_root/usr/bin" \
  "$deb_root/usr/share/doc/ferrum" \
  "$deb_root/usr/share/man/man1" \
  "$stage/debian/debian"
install -m 0755 "$binary" "$deb_root/usr/bin/ferrum"
cat > "$stage/debian/debian/control" <<EOF
Source: ferrum
Section: utils
Priority: optional
Maintainer: ominiverdi
Standards-Version: 4.6.2

Package: ferrum
Architecture: ${deb_arch}
Description: Small Rust-native Linux coding agent
EOF
deb_depends="$(cd "$stage/debian" && dpkg-shlibdeps -O "$deb_root/usr/bin/ferrum" \
  | sed -n 's/^shlibs:Depends=//p')"
if [[ -z "$deb_depends" ]]; then
  echo "failed to derive Debian runtime dependencies" >&2
  exit 1
fi

needed="$(readelf -d "$binary" | sed -n 's/.*Shared library: \[\([^]]*\)\].*/\1/p' | paste -sd, -)"
glibc_max="$(readelf --version-info "$binary" \
  | grep -o 'GLIBC_[0-9][0-9.]*' \
  | sed 's/^GLIBC_//' \
  | sort -Vu \
  | tail -n1)"
binary_sha256="$(sha256sum "$binary" | cut -d' ' -f1)"
rustc_version="$(rustc --version)"
cargo_tool_version="$(cargo --version)"
cat > "$stage/BUILD-PROVENANCE.txt" <<EOF
Ferrum release build provenance
version=${version}
source_commit=${FERRUM_SOURCE_COMMIT}
source_date_epoch=${SOURCE_DATE_EPOCH}
builder_image=${FERRUM_BUILDER_IMAGE}
rustc=${rustc_version}
cargo=${cargo_tool_version}
target=${target}
binary_sha256=${binary_sha256}
glibc_max_symbol=${glibc_max:-none}
needed_libraries=${needed:-none}
debian_depends=${deb_depends}
rpm_auto_requires=builtin
reproducibility_flags=CARGO_INCREMENTAL=0,remap-path-prefix,build-id=none,normalized-archive-metadata
EOF

# Portable archive for the documented Debian 12 / glibc 2.36 baseline and newer.
tar_root="$stage/$package"
mkdir -p "$tar_root/docs"
install -m 0755 "$binary" "$tar_root/ferrum"
install -m 0644 README.md LICENSE "$stage/BUILD-PROVENANCE.txt" "$tar_root/"
install -m 0644 docs/ferrum.1 "$tar_root/docs/"
find "$tar_root" -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} +
(
  cd "$stage"
  tar --sort=name --format=gnu --mtime="@$SOURCE_DATE_EPOCH" \
    --owner=0 --group=0 --numeric-owner -cf - "$package" \
    | gzip -9n > "$out_dir/${package}.tar.gz"
)

# Debian package with dependencies derived by dpkg-shlibdeps.
install -m 0644 README.md LICENSE "$stage/BUILD-PROVENANCE.txt" \
  "$deb_root/usr/share/doc/ferrum/"
gzip -9n -c docs/ferrum.1 > "$deb_root/usr/share/man/man1/ferrum.1.gz"
installed_size="$(du -sk "$deb_root/usr" | cut -f1)"
cat > "$deb_root/DEBIAN/control" <<EOF
Package: ferrum
Version: ${plain_version}
Section: utils
Priority: optional
Architecture: ${deb_arch}
Maintainer: ominiverdi
Installed-Size: ${installed_size}
Depends: ${deb_depends}
Homepage: https://codeberg.org/ominiverdi/ferrum
Description: Small Rust-native Linux coding agent
 Ferrum is a small, fast, Rust-native Linux coding agent with
 provider-neutral tools, JSONL sessions, MCP support, image input,
 usage accounting, and OpenAI-compatible providers.
EOF
find "$deb_root" -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} +
dpkg-deb --build --root-owner-group "$deb_root" "$out_dir/$deb_name"

# RPM metadata is supplied on the command line; Cargo.toml is never modified.
rpm_assets="$stage/rpm-assets"
mkdir -p "$rpm_assets"
install -m 0755 "$binary" "$rpm_assets/ferrum"
install -m 0644 README.md LICENSE docs/ferrum.1 "$stage/BUILD-PROVENANCE.txt" "$rpm_assets/"
rpm_metadata="$(cat <<EOF
release = "1"
auto-req = "builtin"

[[assets]]
source = "$rpm_assets/ferrum"
dest = "/usr/bin/ferrum"
mode = "0755"

[[assets]]
source = "$rpm_assets/README.md"
dest = "/usr/share/doc/ferrum/README.md"
doc = true
mode = "0644"

[[assets]]
source = "$rpm_assets/LICENSE"
dest = "/usr/share/doc/ferrum/LICENSE"
doc = true
mode = "0644"

[[assets]]
source = "$rpm_assets/BUILD-PROVENANCE.txt"
dest = "/usr/share/doc/ferrum/BUILD-PROVENANCE.txt"
doc = true
mode = "0644"

[[assets]]
source = "$rpm_assets/ferrum.1"
dest = "/usr/share/man/man1/ferrum.1"
mode = "0644"
EOF
)"
cargo generate-rpm \
  --arch "$rpm_arch" \
  --target "$target" \
  --target-dir "${CARGO_TARGET_DIR:-target}" \
  --auto-req builtin \
  --source-date "$SOURCE_DATE_EPOCH" \
  --set-metadata "$rpm_metadata" \
  --output "$out_dir"
[[ -f "$out_dir/$rpm_name" ]] || { echo "RPM output missing: $rpm_name" >&2; exit 1; }
if ! rpm -qpR "$out_dir/$rpm_name" | grep -q 'libc\.so\.6'; then
  echo "RPM has no derived libc runtime requirement" >&2
  exit 1
fi

(
  cd "$out_dir"
  sha256sum "${package}.tar.gz" > "${package}.tar.gz.sha256"
  sha256sum "$deb_name" > "${deb_name}.sha256"
  sha256sum "$rpm_name" > "${rpm_name}.sha256"
)

expected=(
  "${package}.tar.gz"
  "${package}.tar.gz.sha256"
  "$deb_name"
  "${deb_name}.sha256"
  "$rpm_name"
  "${rpm_name}.sha256"
)
for asset in "${expected[@]}"; do
  [[ -s "$out_dir/$asset" ]] || { echo "missing release asset: $asset" >&2; exit 1; }
done
asset_count="$(find "$out_dir" -maxdepth 1 -type f | wc -l)"
[[ "$asset_count" -eq 6 ]] || { echo "unexpected release asset count: $asset_count" >&2; exit 1; }

printf 'Built verified %s assets for %s (%s, glibc <= %s)\n' \
  "$asset_count" "$version" "$target" "${glibc_max:-unknown}"
