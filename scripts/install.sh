#!/bin/sh
# Build stage from this checkout and install it the way the release installer does.
set -eu
cd "$(dirname "$0")/.."
[ "$(uname -s)" = Darwin ] || { echo "stage runs on macOS only (see README.md#linux)" >&2; exit 1; }
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
scripts/dist.sh "$tmp/dist"
sh install.sh "$tmp/dist"
