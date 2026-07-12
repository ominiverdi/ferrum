# Release Checklist

Codeberg is the primary source repository and release host. Releases should be created locally with `tea` and locally built assets. GitHub is kept as a mirror and backup binary release host.

Before public release:

```bash
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked
cargo build --locked --release
bash bench/test.sh
bash scripts/test-package-linux.sh
bash scripts/check-release-docs.sh
```

Check for accidental files:

```bash
git status --short
find . -maxdepth 2 -type f | sort
```

Do not commit generated or local files:

- `target/`
- `.env` files
- API keys
- OAuth credentials
- Vault credentials
- local session files

## Versioning

`release-version.txt` is the single source of truth for published install examples. Set the Cargo package version, update `Cargo.lock`, then synchronize every install surface:

```bash
scripts/sync-release-version.sh v0.7.2
scripts/check-release-docs.sh
```

The tag, `Cargo.toml`, generated binary, and `release-version.txt` must match exactly. The release workflow rejects non-stable-semver tags and mismatches before building assets.

## Prepare release tag

Create the annotated tag locally. Do not push it until the complete package set and clean-image tests pass:

```bash
version=v0.7.2
notes=/tmp/ferrum-${version}-notes.md

git tag -a "$version" -F "$notes"
```

In the primary local clone, `origin` should point to Codeberg and `github` should point to the GitHub mirror.

## Release assets

Build all Linux assets from committed source in the pinned compatibility container:

```bash
FERRUM_REPRODUCIBILITY_CHECK=1 scripts/package-linux.sh v0.7.2
```

Podman or Docker is required. The wrapper rejects dirty source, verifies tag/Cargo/binary version equality, rebuilds with `cargo build --locked --release`, derives architecture and runtime requirements, and publishes `dist/` only after all six assets succeed. It never accepts `target/release/ferrum` from the host and never modifies `Cargo.toml`.

The builder is pinned to Rust 1.90.0 on Debian 12. The GNU tarball therefore has a Debian 12 compatibility baseline (glibc 2.36 and OpenSSL 3); exact maximum GLIBC symbol, needed shared libraries, compiler versions, source commit, binary hash, and builder identity are embedded as `BUILD-PROVENANCE.txt`. Debian dependencies come from `dpkg-shlibdeps`; RPM requirements use `cargo-generate-rpm` builtin ELF analysis.

`FERRUM_REPRODUCIBILITY_CHECK=1` performs two clean builds with `SOURCE_DATE_EPOCH`, path remapping, disabled incremental compilation/build IDs, normalized tar ownership/timestamps/order, and deterministic gzip, then compares every asset hash before publishing.

The script writes assets to `dist/`:

```text
ferrum-v0.7.2-x86_64-unknown-linux-gnu.tar.gz
ferrum-v0.7.2-x86_64-unknown-linux-gnu.tar.gz.sha256
ferrum_0.7.2_amd64.deb
ferrum_0.7.2_amd64.deb.sha256
ferrum-0.7.2-1.x86_64.rpm
ferrum-0.7.2-1.x86_64.rpm.sha256
```

The tarball includes:

```text
ferrum
README.md
LICENSE
BUILD-PROVENANCE.txt
docs/ferrum.1
```

The Debian and RPM packages install:

```text
/usr/bin/ferrum
/usr/share/doc/ferrum/README.md
/usr/share/doc/ferrum/LICENSE
/usr/share/doc/ferrum/BUILD-PROVENANCE.txt
/usr/share/man/man1/ferrum.1.gz  # Debian
/usr/share/man/man1/ferrum.1     # RPM
```

No host RPM tooling is required; the pinned builder image contains the exact packaging tools.

Verify local packages:

```bash
cd dist
sha256sum -c ferrum-v0.7.2-x86_64-unknown-linux-gnu.tar.gz.sha256
sha256sum -c ferrum_0.7.2_amd64.deb.sha256
sha256sum -c ferrum-0.7.2-1.x86_64.rpm.sha256
dpkg-deb --info ferrum_0.7.2_amd64.deb
dpkg-deb --contents ferrum_0.7.2_amd64.deb | head
```

The GitHub release workflow repeats the locked validation, performs the two-build reproducibility check, installs the Debian and RPM packages in pinned clean Debian 12 and Fedora 43 images, records GitHub artifact attestations, and publishes the same six-file asset contract as Codeberg.

## Publish tag

Only after local validation succeeds, push Codeberg first and then the GitHub mirror:

```bash
git push origin main "$version"
git push github main "$version"
```

Pushing the `v*` tag to GitHub triggers `.github/workflows/release.yml`.

## Codeberg release

Create the Codeberg release with `tea` after pushing the tag:

```bash
version=v0.7.2
tea releases create "$version" \
  --title "Ferrum $version" \
  --note-file "/tmp/ferrum-${version}-notes.md" \
  --repo ominiverdi/ferrum
```

Upload release assets:

```bash
version=v0.7.2
tea releases assets create "$version" \
  dist/ferrum-${version}-x86_64-unknown-linux-gnu.tar.gz \
  dist/ferrum-${version}-x86_64-unknown-linux-gnu.tar.gz.sha256 \
  dist/ferrum_${version#v}_amd64.deb \
  dist/ferrum_${version#v}_amd64.deb.sha256 \
  dist/ferrum-${version#v}-1.x86_64.rpm \
  dist/ferrum-${version#v}-1.x86_64.rpm.sha256 \
  --repo ominiverdi/ferrum
```

If the release already exists, upload only missing assets.

Verify Codeberg assets:

```bash
version=v0.7.2
plain_version=${version#v}
target=x86_64-unknown-linux-gnu
package="ferrum-${version}-${target}"
mkdir -p /tmp/ferrum-codeberg-release-check
cd /tmp/ferrum-codeberg-release-check
for file in \
  "${package}.tar.gz" \
  "${package}.tar.gz.sha256" \
  "ferrum_${plain_version}_amd64.deb" \
  "ferrum_${plain_version}_amd64.deb.sha256" \
  "ferrum-${plain_version}-1.x86_64.rpm" \
  "ferrum-${plain_version}-1.x86_64.rpm.sha256"; do
  curl -fsSLO "https://codeberg.org/ominiverdi/ferrum/releases/download/${version}/${file}"
done
sha256sum -c "${package}.tar.gz.sha256"
sha256sum -c "ferrum_${plain_version}_amd64.deb.sha256"
sha256sum -c "ferrum-${plain_version}-1.x86_64.rpm.sha256"
tar -tzf "${package}.tar.gz" | head
dpkg-deb --info "ferrum_${plain_version}_amd64.deb" | head
```

## Verify GitHub mirror release

After the GitHub workflow completes:

```bash
gh release view "$version" --repo ominiverdi/ferrum --json tagName,isDraft,assets,url
mkdir -p /tmp/ferrum-github-release-check
cd /tmp/ferrum-github-release-check
gh release download "$version" --repo ominiverdi/ferrum --pattern '*.tar.gz' --pattern '*.deb' --pattern '*.rpm' --pattern '*.sha256'
sha256sum -c ferrum-${version}-x86_64-unknown-linux-gnu.tar.gz.sha256
sha256sum -c ferrum_${version#v}_amd64.deb.sha256
sha256sum -c ferrum-${version#v}-1.x86_64.rpm.sha256
```
## Install docs

Release notes should include Codeberg primary install commands.

Tarball:

```bash
curl -L https://codeberg.org/ominiverdi/ferrum/releases/download/v0.7.2/ferrum-v0.7.2-x86_64-unknown-linux-gnu.tar.gz | tar xz
sudo install -Dm755 ferrum-v0.7.2-x86_64-unknown-linux-gnu/ferrum /usr/local/bin/ferrum
sudo install -Dm644 ferrum-v0.7.2-x86_64-unknown-linux-gnu/docs/ferrum.1 /usr/local/share/man/man1/ferrum.1
sudo mandb 2>/dev/null || true
ferrum --help
man ferrum
```

Debian/Ubuntu:

```bash
curl -LO https://codeberg.org/ominiverdi/ferrum/releases/download/v0.7.2/ferrum_0.7.2_amd64.deb
sudo apt install ./ferrum_0.7.2_amd64.deb
ferrum --help
```

Fedora/RHEL/openSUSE:

```bash
curl -LO https://codeberg.org/ominiverdi/ferrum/releases/download/v0.7.2/ferrum-0.7.2-1.x86_64.rpm
sudo dnf install ./ferrum-0.7.2-1.x86_64.rpm
ferrum --help
```

Use `sudo zypper install ./ferrum-0.7.2-1.x86_64.rpm` on openSUSE.

From source, use Cargo:

```bash
git clone https://codeberg.org/ominiverdi/ferrum.git
cd ferrum
cargo install --locked --path .
ferrum --help
```

Optional checksum verification:

```bash
curl -LO https://codeberg.org/ominiverdi/ferrum/releases/download/v0.7.2/ferrum-v0.7.2-x86_64-unknown-linux-gnu.tar.gz
curl -LO https://codeberg.org/ominiverdi/ferrum/releases/download/v0.7.2/ferrum-v0.7.2-x86_64-unknown-linux-gnu.tar.gz.sha256
sha256sum -c ferrum-v0.7.2-x86_64-unknown-linux-gnu.tar.gz.sha256
```

## CI

GitHub Actions provides pinned mirror CI through `.github/workflows/ci.yml`. Release actions are pinned by commit, the Rust toolchain is 1.90.0, dependency auditing is required, and the release builder/test images use immutable digests. Codeberg Forgejo Actions is intentionally not configured because hosted runner availability is too inconsistent for the project workflow. Local locked validation remains required before every push and release.

Codeberg remains the primary release host; create releases locally with `tea`. GitHub publishes an attested backup release containing the same six assets.

## License

Ferrum is MIT licensed. See `LICENSE`.
