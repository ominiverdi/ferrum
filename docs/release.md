# Release Checklist

Codeberg is the primary source repository and release host. Releases should be created locally with `tea` and locally built assets. GitHub is kept as a mirror and backup binary release host.

Before public release:

```bash
cargo fmt --check
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
version = "0.4.17"
```

## Tag release

Use annotated tags and push Codeberg first, then the GitHub mirror:

```bash
version=v0.4.17
notes=/tmp/ferrum-${version}-notes.md

git tag -a "$version" -F "$notes"
git push origin main "$version"
git push github main "$version"
```

In the primary local clone, `origin` should point to Codeberg and `github` should point to the GitHub mirror. Pushing a `v*` tag to GitHub triggers `.github/workflows/release.yml` and uploads backup binary assets to the GitHub release.

## Release assets

Build and package the Linux x86_64 asset locally after validation:

```text
ferrum-${version}-x86_64-unknown-linux-gnu.tar.gz
ferrum-${version}-x86_64-unknown-linux-gnu.tar.gz.sha256
```

The tarball includes:

```text
ferrum
README.md
LICENSE
```

Manual packaging:

```bash
version=v0.4.17
target=x86_64-unknown-linux-gnu
package="ferrum-${version}-${target}"
mkdir -p "$package"
cp target/release/ferrum "$package/"
cp README.md LICENSE "$package/"
tar -czf "${package}.tar.gz" "$package"
sha256sum "${package}.tar.gz" > "${package}.tar.gz.sha256"
```

## Codeberg release

Create the Codeberg release with `tea` after pushing the tag:

```bash
version=v0.4.17
tea releases create "$version" \
  --title "Ferrum $version" \
  --note-file "/tmp/ferrum-${version}-notes.md" \
  --repo ominiverdi/ferrum
```

Upload release assets:

```bash
version=v0.4.17
target=x86_64-unknown-linux-gnu
package="ferrum-${version}-${target}"

tea releases assets create "$version" \
  "${package}.tar.gz" \
  "${package}.tar.gz.sha256" \
  --repo ominiverdi/ferrum
```

If the release already exists, upload only missing assets.

Verify Codeberg assets:

```bash
version=v0.4.17
target=x86_64-unknown-linux-gnu
package="ferrum-${version}-${target}"
mkdir -p /tmp/ferrum-codeberg-release-check
cd /tmp/ferrum-codeberg-release-check
curl -fsSLO "https://codeberg.org/ominiverdi/ferrum/releases/download/${version}/${package}.tar.gz"
curl -fsSLO "https://codeberg.org/ominiverdi/ferrum/releases/download/${version}/${package}.tar.gz.sha256"
sha256sum -c "${package}.tar.gz.sha256"
tar -tzf "${package}.tar.gz" | head
```

## Verify GitHub mirror release

After the GitHub workflow completes:

```bash
gh release view "$version" --repo ominiverdi/ferrum --json tagName,isDraft,assets,url
mkdir -p /tmp/ferrum-github-release-check
cd /tmp/ferrum-github-release-check
gh release download "$version" --repo ominiverdi/ferrum --pattern '*.tar.gz' --pattern '*.sha256'
sha256sum -c ferrum-${version}-x86_64-unknown-linux-gnu.tar.gz.sha256
```

## Install docs

Release notes should include Codeberg primary install commands:

```bash
curl -L https://codeberg.org/ominiverdi/ferrum/releases/download/v0.4.17/ferrum-v0.4.17-x86_64-unknown-linux-gnu.tar.gz | tar xz
sudo mv ferrum-v0.4.17-x86_64-unknown-linux-gnu/ferrum /usr/local/bin/
ferrum --help
```

From source, use Cargo:

```bash
git clone https://codeberg.org/ominiverdi/ferrum.git
cd ferrum
cargo install --path .
ferrum --help
```

Optional checksum verification:

```bash
curl -LO https://codeberg.org/ominiverdi/ferrum/releases/download/v0.4.17/ferrum-v0.4.17-x86_64-unknown-linux-gnu.tar.gz
curl -LO https://codeberg.org/ominiverdi/ferrum/releases/download/v0.4.17/ferrum-v0.4.17-x86_64-unknown-linux-gnu.tar.gz.sha256
sha256sum -c ferrum-v0.4.17-x86_64-unknown-linux-gnu.tar.gz.sha256
```

## CI

Codeberg Forgejo Actions validates pushes with `.forgejo/workflows/ci.yml` on the hosted `codeberg-medium` runner. The workflow checks formatting and runs `cargo test --release` in `rust:1.90`.

GitHub Actions may remain configured for mirror CI and optional backup release asset builds. Codeberg releases created locally with `tea` are the primary release path.

## License

Ferrum is MIT licensed. See `LICENSE`.
