# Codeberg workflow

Codeberg is Ferrum's primary forge.

Use `tea` for routine Codeberg operations when possible.

## Issues

List issues:

```bash
tea issues ls --repo ominiverdi/ferrum
```

View one issue:

```bash
tea issues 5 --repo ominiverdi/ferrum --comments
```

Create an issue non-interactively:

```bash
tea issues create \
  --repo ominiverdi/ferrum \
  --login codeberg.org \
  --title "Issue title" \
  --description "Issue body"
```

Reply to an issue or PR non-interactively:

```bash
tea comment 5 "Reply text" --repo ominiverdi/ferrum --login codeberg.org
tea comment 7 "PR reply text" --repo ominiverdi/ferrum --login codeberg.org
```

For multiline comments, write the body to a temporary file and pass it as the positional comment argument:

```bash
cat > /tmp/ferrum-comment.md <<'EOF'
Comment body.
EOF
tea comment 5 "$(cat /tmp/ferrum-comment.md)" --repo ominiverdi/ferrum --login codeberg.org
```

Close an issue:

```bash
tea issues close 5 --repo ominiverdi/ferrum --login codeberg.org
```

`tea comment` is singular. There is no `tea comments create` command.

If a fuller issue comment workflow is not available in the current environment, draft the exact reply text for the user instead of pretending it was posted.

## Pull requests

List pull requests:

```bash
tea pr ls --repo ominiverdi/ferrum
```

View a pull request:

```bash
tea pr 1 --repo ominiverdi/ferrum --comments
```

If `tea` cannot post review comments non-interactively, provide the exact review text for manual posting.

For diff inspection, fetching the PR ref locally is often useful:

```bash
git fetch origin refs/pull/1/head:pr-1
git diff main...pr-1
```

## Releases

List releases:

```bash
tea releases ls --repo ominiverdi/ferrum
```

Create a release entry and upload assets in one command:

```bash
version=vX.Y.Z
plain_version=${version#v}
target=x86_64-unknown-linux-gnu
package="ferrum-${version}-${target}"

tea releases create "$version" \
  --title "Ferrum $version" \
  --note-file "/tmp/ferrum-${version}-notes.md" \
  --asset "dist/${package}.tar.gz" \
  --asset "dist/${package}.tar.gz.sha256" \
  --asset "dist/ferrum_${plain_version}_amd64.deb" \
  --asset "dist/ferrum_${plain_version}_amd64.deb.sha256" \
  --asset "dist/ferrum-${plain_version}-1.x86_64.rpm" \
  --asset "dist/ferrum-${plain_version}-1.x86_64.rpm.sha256" \
  --repo ominiverdi/ferrum \
  --login codeberg.org
```

If the release already exists and only missing assets need to be uploaded:

```bash
version=vX.Y.Z
plain_version=${version#v}
target=x86_64-unknown-linux-gnu
package="ferrum-${version}-${target}"

tea releases assets create "$version" \
  "dist/${package}.tar.gz" \
  "dist/${package}.tar.gz.sha256" \
  "dist/ferrum_${plain_version}_amd64.deb" \
  "dist/ferrum_${plain_version}_amd64.deb.sha256" \
  "dist/ferrum-${plain_version}-1.x86_64.rpm" \
  "dist/ferrum-${plain_version}-1.x86_64.rpm.sha256" \
  --repo ominiverdi/ferrum \
  --login codeberg.org
```

Verify uploaded assets:

```bash
tea releases assets ls "$version" --repo ominiverdi/ferrum
```

## Mirrors

Normal source pushes go to both remotes:

```bash
git push origin main
git push github main
```

Tagged releases go to both remotes too, but Codeberg remains the primary release host.
