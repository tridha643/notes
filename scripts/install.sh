#!/bin/sh
# Build stage from this checkout and install it the way the release installer does.
set -eu
cd "$(dirname "$0")/.."
[ "$(uname -s)" = Darwin ] || { echo "stage runs on macOS only (see README.md#linux)" >&2; exit 1; }
dist="$(mktemp -d)/dist"
scripts/dist.sh "$dist"
sh install.sh "$dist"
rm -rf "$(dirname "$dist")"
