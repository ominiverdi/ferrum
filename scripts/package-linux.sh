#!/usr/bin/env bash
set -euo pipefail
umask 077

version="${1:-}"
if [[ ! "$version" =~ ^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?$ ]]; then
  echo "usage: $0 vX.Y.Z" >&2
  exit 2
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

plain_version="${version#v}"
cargo_version="$(cargo metadata --locked --no-deps --format-version 1 \
  | sed -n 's/.*"name":"ferrum","version":"\([^"]*\)".*/\1/p' \
  | head -n1)"
if [[ -z "$cargo_version" || "$plain_version" != "$cargo_version" ]]; then
  echo "release version mismatch: tag=$version Cargo.toml=${cargo_version:-unknown}" >&2
  exit 1
fi
if [[ -n "${FERRUM_RELEASE_TAG:-}" && "$FERRUM_RELEASE_TAG" != "$version" ]]; then
  echo "release tag mismatch: argument=$version environment=$FERRUM_RELEASE_TAG" >&2
  exit 1
fi
if [[ "${FERRUM_PACKAGE_ALLOW_DIRTY:-0}" != "1" ]]; then
  if [[ -n "$(git status --porcelain=v1 --untracked-files=normal)" ]]; then
    echo "refusing to package a dirty checkout; commit current source first" >&2
    exit 1
  fi
  if ! tag_commit="$(git rev-list -n1 "$version" 2>/dev/null)" \
    || [[ -z "$tag_commit" || "$tag_commit" != "$(git rev-parse HEAD)" ]]; then
    echo "release tag $version must exist and identify the current commit" >&2
    exit 1
  fi
fi

source_commit="$(git rev-parse --verify HEAD)"
source_date_epoch="${SOURCE_DATE_EPOCH:-$(git show -s --format=%ct HEAD)}"
[[ "$source_commit" =~ ^[0-9a-f]{40}$ ]] || { echo "invalid source commit" >&2; exit 1; }
[[ "$source_date_epoch" =~ ^[0-9]+$ ]] || { echo "invalid SOURCE_DATE_EPOCH" >&2; exit 1; }

runtime="${PACKAGE_CONTAINER_RUNTIME:-}"
if [[ -z "$runtime" ]]; then
  if command -v podman >/dev/null 2>&1; then
    runtime="podman"
  elif command -v docker >/dev/null 2>&1; then
    runtime="docker"
  else
    echo "podman or docker is required for controlled release packaging" >&2
    exit 1
  fi
fi
command -v "$runtime" >/dev/null 2>&1 || { echo "container runtime not found: $runtime" >&2; exit 1; }

builder_dockerfile="packaging/linux/Dockerfile"
builder_definition_sha="$(sha256sum "$builder_dockerfile" scripts/package-linux-inner.sh | sha256sum | cut -d' ' -f1)"
builder_tag="ferrum-release-builder:${builder_definition_sha:0:16}"
rm -rf "$root/dist"
"$runtime" build --pull=false -q -t "$builder_tag" -f "$builder_dockerfile" packaging/linux >/dev/null
builder_provenance="rust-1.90.0-bookworm@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f+definition:${builder_definition_sha}"

stage="$(mktemp -d "$root/.dist-stage.XXXXXXXXXX")"
second_stage=""
cleanup() {
  rm -rf "$stage"
  if [[ -n "$second_stage" ]]; then
    rm -rf "$second_stage"
  fi
}
trap cleanup EXIT

run_packager() {
  local output="$1"
  "$runtime" run --rm \
    --network=host \
    -e "SOURCE_DATE_EPOCH=$source_date_epoch" \
    -e "FERRUM_SOURCE_COMMIT=$source_commit" \
    -e "FERRUM_BUILDER_IMAGE=$builder_provenance" \
    -v "$root:/workspace:ro" \
    -v "$output:/output" \
    "$builder_tag" "$version"
}

run_packager "$stage"

if [[ "${FERRUM_REPRODUCIBILITY_CHECK:-0}" == "1" ]]; then
  second_stage="$(mktemp -d "$root/.dist-repro.XXXXXXXXXX")"
  run_packager "$second_stage"
  first_manifest="$(mktemp)"
  second_manifest="$(mktemp)"
  trap 'rm -f "$first_manifest" "$second_manifest"; cleanup' EXIT
  (cd "$stage" && sha256sum * | sort -k2) > "$first_manifest"
  (cd "$second_stage" && sha256sum * | sort -k2) > "$second_manifest"
  if ! cmp -s "$first_manifest" "$second_manifest"; then
    echo "release assets are not reproducible across two clean builds" >&2
    diff -u "$first_manifest" "$second_manifest" >&2 || true
    exit 1
  fi
  rm -f "$first_manifest" "$second_manifest"
  trap cleanup EXIT
fi

mv -T -- "$stage" "$root/dist"
stage=""
trap cleanup EXIT
printf 'Published complete release asset set to %s/dist\n' "$root"
ls -l "$root/dist"
