#!/bin/sh
# Build release binaries and lay them out as the release tarball does: OUT/bin, OUT/skills.
set -eu
out="$1"
cd "$(dirname "$0")/.."
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
for tool in cargo swift; do
  command -v "$tool" >/dev/null || { echo "missing $tool: install Rust (https://rustup.rs) and Xcode" >&2; exit 1; }
done
cargo build --release -p notesd -p notes-cli -p stage
swift build -c release --package-path apps/Notes --product stage-helper
swiftbin="$(swift build -c release --package-path apps/Notes --show-bin-path)"
rm -rf "$out" && mkdir -p "$out/bin" "$out/skills"
install -m 755 target/release/notesd target/release/notes target/release/stage "$swiftbin/stage-helper" "$out/bin/"
cp -R "$swiftbin"/*.bundle "$out/bin/"
cp -R skills/stage "$out/skills/stage"
