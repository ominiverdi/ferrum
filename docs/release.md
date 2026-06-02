# Release Checklist

Codeberg is the primary source repository. GitHub is currently kept as a mirror and binary release host because the existing release workflow uploads GitHub release assets.

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
version = "0.4.12"
```

## Tag release

Use annotated tags and push Codeberg first, then the GitHub mirror:

```bash
version=v0.4.12
notes=/tmp/ferrum-${version}-notes.md

git tag -a "$version" -F "$notes"
git push origin main "$version"
git push github main "$version"
```

In the primary local clone, `origin` should point to Codeberg and `github` should point to the GitHub mirror. Pushing a `v*` tag to GitHub triggers `.github/workflows/release.yml` and uploads binary assets to the GitHub release.

## Release assets

The GitHub release workflow builds Linux x86_64 and uploads:

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

Manual packaging equivalent:

```bash
version=v0.4.12
target=x86_64-unknown-linux-gnu
package="ferrum-${version}-${target}"
mkdir -p "$package"
cp target/release/ferrum "$package/"
cp README.md LICENSE "$package/"
tar -czf "${package}.tar.gz" "$package"
sha256sum "${package}.tar.gz" > "${package}.tar.gz.sha256"
```

## Verify GitHub release

After the GitHub workflow completes:

```bash
gh release view "$version" --repo ominiverdi/ferrum --json tagName,isDraft,assets,url
mkdir -p /tmp/ferrum-release-check
cd /tmp/ferrum-release-check
gh release download "$version" --repo ominiverdi/ferrum --pattern '*.tar.gz' --pattern '*.sha256'
sha256sum -c ferrum-${version}-x86_64-unknown-linux-gnu.tar.gz.sha256
```

## Install docs

Release notes should include:

```bash
curl -L https://github.com/ominiverdi/ferrum/releases/download/v0.4.12/ferrum-v0.4.12-x86_64-unknown-linux-gnu.tar.gz | tar xz
sudo mv ferrum-v0.4.12-x86_64-unknown-linux-gnu/ferrum /usr/local/bin/
ferrum --help
```

## Codeberg releases

Source tags are pushed to Codeberg. Binary assets are still hosted on GitHub until Codeberg release automation is proven.

If creating Codeberg assets manually, use `tea release create` with locally built assets and verify download/checksum before linking users to them.

## License

Ferrum is MIT licensed. See `LICENSE`.
