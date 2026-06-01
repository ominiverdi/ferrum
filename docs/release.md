# Release Checklist

Before public release:

```bash
cargo fmt --check
cargo test
cargo build --release
```

Check for accidental files:

```bash
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

Update `Cargo.toml`:

```toml
version = "0.4.9"
```

## Tag release

```bash
git tag -a v0.4.9 -m "Ferrum v0.4.9"
git push origin main
git push origin v0.4.9
```

Pushing a `v*` tag triggers `.github/workflows/release.yml`.

## Release assets

The release workflow builds Linux x86_64 and uploads:

```text
ferrum-v0.4.9-x86_64-unknown-linux-gnu.tar.gz
ferrum-v0.4.9-x86_64-unknown-linux-gnu.tar.gz.sha256
```

The tarball includes:

```text
ferrum
README.md
LICENSE
```

Manual packaging equivalent:

```bash
version=v0.4.9
target=x86_64-unknown-linux-gnu
package="ferrum-${version}-${target}"
mkdir -p "$package"
cp target/release/ferrum "$package/"
cp README.md LICENSE "$package/"
tar -czf "${package}.tar.gz" "$package"
sha256sum "${package}.tar.gz" > "${package}.tar.gz.sha256"
```

## Install docs

Release notes should include:

```bash
curl -L https://github.com/ominiverdi/ferrum/releases/download/v0.4.9/ferrum-v0.4.9-x86_64-unknown-linux-gnu.tar.gz | tar xz
sudo mv ferrum-v0.4.9-x86_64-unknown-linux-gnu/ferrum /usr/local/bin/
ferrum --help
```

## License

Ferrum is MIT licensed. See `LICENSE`.
