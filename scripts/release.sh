#!/bin/sh
# Publish a GitHub release: scripts/release.sh v0.1.0
# Builds on this Mac, uploads stage-macos-arm64.tar.gz, which install.sh downloads.
set -eu
tag="$1"
cd "$(dirname "$0")/.."
case "$tag" in v[0-9]*) ;; *) echo "usage: scripts/release.sh vX.Y.Z" >&2; exit 2 ;; esac
[ -z "$(git status --porcelain)" ] || { echo "commit or stash changes first" >&2; exit 1; }
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
scripts/dist.sh "$tmp/dist"
tar -czf "$tmp/stage-macos-arm64.tar.gz" -C "$tmp/dist" bin skills
gh release create "$tag" "$tmp/stage-macos-arm64.tar.gz" install.sh --target "$(git rev-parse HEAD)" --title "stage $tag" --generate-notes
