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

Do not commit:

- `target/`
- `.env` files
- API keys
- OAuth credentials
- Vault credentials
- session files unless intentionally adding examples with no secrets

## Versioning

Update `Cargo.toml`:

```toml
version = "0.2.1"```

## License

Ferrum is MIT licensed. See `LICENSE`.

## Suggested tag

```bash
git tag v0.2.1
git push origin v0.2.1
```
