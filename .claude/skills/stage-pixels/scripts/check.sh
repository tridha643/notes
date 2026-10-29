#!/bin/bash
# Model-check StagePixels.tla with TLC. Usage: check.sh [SPEC.tla [CONFIG.cfg]]
# (a mutated copy of the spec can be checked by passing its path).
set -euo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
spec=${1:-$here/model/StagePixels.tla}
cfg=${2:-${spec%.tla}.cfg}
version=1.8.0
sha=fc6878c603f0fdc18b9fe3e5bf51860cb90ab5b1154e3c371660c036d6bdd63e
jar="$HOME/.cache/notes-gpui/tla2tools-$version.jar"
if [ ! -f "$jar" ]; then
  mkdir -p "$(dirname "$jar")"
  curl -fsSL -o "$jar" "https://github.com/tlaplus/tlaplus/releases/download/v$version/tla2tools.jar"
fi
echo "$sha  $jar" | shasum -a 256 -c --quiet
java=$( (command -v brew >/dev/null && [ -x "$(brew --prefix openjdk 2>/dev/null)/bin/java" ] && echo "$(brew --prefix openjdk)/bin/java") || echo java)
out=$(mktemp -d)
cp "$spec" "$out/"; cp "$cfg" "$out/$(basename "${spec%.tla}").cfg"
cd "$out"
"$java" -XX:+UseParallelGC -cp "$jar" tlc2.TLC -workers auto -deadlock -config "$(basename "${spec%.tla}").cfg" "$(basename "$spec")" | tee tlc.out | grep -E "Error:|Invariant .* is violated|states generated|No error|^[0-9]+\. |^State [0-9]+|^/\\\\ " || true
grep -q "No error has been found" tlc.out && echo "PASS (output: $out/tlc.out)" || { echo "FAIL (output: $out/tlc.out)"; exit 1; }
