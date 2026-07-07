# Release Checklist

Codeberg is the primary source repository and release host. Releases should be created locally with `tea` and locally built assets. GitHub is kept as a mirror and backup binary release host.

Before public release:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
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

Set the next version in `Cargo.toml`, `Cargo.lock`, and install docs.

Example:

```toml
version = "0.6.5"
```

## Tag release

Use annotated tags and push Codeberg first, then the GitHub mirror:

```bash
version=v0.6.5
notes=/tmp/ferrum-${version}-notes.md

git tag -a "$version" -F "$notes"
git push origin main "$version"
git push github main "$version"
```

In the primary local clone, `origin` should point to Codeberg and `github` should point to the GitHub mirror. Pushing a `v*` tag to GitHub triggers `.github/workflows/release.yml` and uploads backup binary assets to the GitHub release.

## Release assets

Build and package the Linux x86_64 assets locally after validation:

```bash
cargo build --release
scripts/package-linux.sh v0.6.5
```

The script writes assets to `dist/`:

```text
ferrum-v0.6.5-x86_64-unknown-linux-gnu.tar.gz
ferrum-v0.6.5-x86_64-unknown-linux-gnu.tar.gz.sha256
ferrum_0.6.5_amd64.deb
ferrum_0.6.5_amd64.deb.sha256
ferrum-0.6.5-1.x86_64.rpm
ferrum-0.6.5-1.x86_64.rpm.sha256
```

The tarball includes:

```text
ferrum
README.md
LICENSE
docs/ferrum.1
```

The Debian and RPM packages install:

```text
/usr/bin/ferrum
/usr/share/doc/ferrum/README.md
/usr/share/doc/ferrum/LICENSE
/usr/share/man/man1/ferrum.1.gz  # Debian
/usr/share/man/man1/ferrum.1     # RPM
```

RPM packaging requires `cargo-generate-rpm`:

```bash
cargo install cargo-generate-rpm
```

Verify local packages:

```bash
cd dist
sha256sum -c ferrum-v0.6.5-x86_64-unknown-linux-gnu.tar.gz.sha256
sha256sum -c ferrum_0.6.5_amd64.deb.sha256
sha256sum -c ferrum-0.6.5-1.x86_64.rpm.sha256
dpkg-deb --info ferrum_0.6.5_amd64.deb
dpkg-deb --contents ferrum_0.6.5_amd64.deb | head
```

## Codeberg release

Create the Codeberg release with `tea` after pushing the tag:

```bash
version=v0.6.5
tea releases create "$version" \
  --title "Ferrum $version" \
  --note-file "/tmp/ferrum-${version}-notes.md" \
  --repo ominiverdi/ferrum
```

Upload release assets:

```bash
version=v0.6.5
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
version=v0.6.5
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
curl -L https://codeberg.org/ominiverdi/ferrum/releases/download/v0.6.5/ferrum-v0.6.5-x86_64-unknown-linux-gnu.tar.gz | tar xz
sudo install -Dm755 ferrum-v0.6.5-x86_64-unknown-linux-gnu/ferrum /usr/local/bin/ferrum
sudo install -Dm644 ferrum-v0.6.5-x86_64-unknown-linux-gnu/docs/ferrum.1 /usr/local/share/man/man1/ferrum.1
sudo mandb 2>/dev/null || true
ferrum --help
man ferrum
```

Debian/Ubuntu:

```bash
curl -LO https://codeberg.org/ominiverdi/ferrum/releases/download/v0.6.5/ferrum_0.6.5_amd64.deb
sudo apt install ./ferrum_0.6.5_amd64.deb
ferrum --help
```

Fedora/RHEL/openSUSE:

```bash
curl -LO https://codeberg.org/ominiverdi/ferrum/releases/download/v0.6.5/ferrum-0.6.5-1.x86_64.rpm
sudo dnf install ./ferrum-0.6.5-1.x86_64.rpm
ferrum --help
```

Use `sudo zypper install ./ferrum-0.6.5-1.x86_64.rpm` on openSUSE.

From source, use Cargo:

```bash
git clone https://codeberg.org/ominiverdi/ferrum.git
cd ferrum
cargo install --path .
ferrum --help
```

Optional checksum verification:

```bash
curl -LO https://codeberg.org/ominiverdi/ferrum/releases/download/v0.6.5/ferrum-v0.6.5-x86_64-unknown-linux-gnu.tar.gz
curl -LO https://codeberg.org/ominiverdi/ferrum/releases/download/v0.6.5/ferrum-v0.6.5-x86_64-unknown-linux-gnu.tar.gz.sha256
sha256sum -c ferrum-v0.6.5-x86_64-unknown-linux-gnu.tar.gz.sha256
```

## CI

Codeberg Forgejo Actions validates pushes with `.forgejo/workflows/ci.yml` on the hosted `codeberg-medium` runner. The workflow checks formatting, runs `cargo clippy --all-targets -- -D warnings`, and runs `cargo test --release` in `rust:1.90`.

GitHub Actions may remain configured for mirror CI and optional backup release asset builds. Codeberg releases created locally with `tea` are the primary release path.

## License

Ferrum is MIT licensed. See `LICENSE`.
