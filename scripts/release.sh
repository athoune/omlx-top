#!/usr/bin/env bash
#
# Cut a release: bump the crate version, refresh Cargo.lock, run the checks,
# commit, and create the annotated tag. The tag is what the CI release workflow
# watches, so pushing it publishes the binaries.
#
# Usage: scripts/release.sh <version> [--push]
#
#   <version>   0.2.0 or v0.2.0
#   --push      push main and the tag (the workflow builds and publishes)
#
set -euo pipefail

usage() {
    sed -n '3,11p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
}

version="${1:-}"
[ -n "$version" ] || usage

push=false
case "${2:-}" in
    --push) push=true ;;
    "") ;;
    *) usage ;;
esac

version="${version#v}"
if ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]]; then
    echo "invalid version '$version': expected X.Y.Z (a leading v is accepted)" >&2
    exit 2
fi
tag="v$version"

cd "$(git rev-parse --show-toplevel)"

branch="$(git rev-parse --abbrev-ref HEAD)"
if [ "$branch" != "main" ]; then
    echo "must be on main (currently on $branch)" >&2
    exit 1
fi

if [ -n "$(git status --porcelain)" ]; then
    echo "working tree is not clean; commit or stash first" >&2
    git status --short >&2
    exit 1
fi

if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
    echo "tag $tag already exists" >&2
    exit 1
fi

current="$(perl -0ne 'print $1 if /^version = "([^"]+)"/m' Cargo.toml)"
if [ -z "$current" ]; then
    echo "could not read the version from Cargo.toml" >&2
    exit 1
fi

if [ "$current" = "$version" ]; then
    echo "Cargo.toml is already at $version; tagging the current commit"
    skip_commit=true
else
    echo "version: $current -> $version"
    skip_commit=false

    perl -0pi -e "s/^version = \"[^\"]*\"/version = \"$version\"/m" Cargo.toml

    # Cargo.lock records the workspace member version too; metadata refreshes it
    # without a build and without touching the network when the cache is warm.
    cargo metadata --format-version 1 >/dev/null
    if ! grep -A1 '^name = "omlx-top"$' Cargo.lock | grep -q "\"$version\""; then
        echo "Cargo.lock was not refreshed to $version" >&2
        exit 1
    fi
fi

echo "checking formatting, lints and tests"
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test

# Pre-flight for the CI publish: package and verify the exact crate contents.
# --allow-dirty because the version bump is not committed yet; nothing is sent.
cargo publish --dry-run --allow-dirty

if ! $skip_commit; then
    git add Cargo.toml Cargo.lock
    git commit -m "chore: release $tag"
fi
git tag -a "$tag" -m "omlx-top $tag"

notes="docs/release-notes/$tag.md"
if [ ! -f "$notes" ]; then
    echo "warning: $notes does not exist; the workflow will use generated notes" >&2
fi

if $push; then
    git push origin main
    git push origin "$tag"
    echo "pushed $tag: the release workflow is now building the assets"
else
    echo "tagged $tag locally. To publish:"
    echo "  git push origin main && git push origin $tag"
fi
