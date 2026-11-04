#!/usr/bin/env bash
# Rebuilds the `dragology` html kit: crates/proto/src/kits/dragology.js (one minified script)
# and dragology-LICENSES (each bundled package, its version and license text).
#
# preact/compat stands in for react + react-dom, and the library's debug printer
# (@joshuahhh/pretty-print, which drags in prettier) is stubbed, which keeps the bundle small.
# Bump the pinned versions here, rerun, and commit the two outputs.
set -euo pipefail

out="$(cd "$(dirname "$0")/.." && pwd)/crates/proto/src/kits"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
cd "$tmp"

npm init -y >/dev/null
npm install --silent --no-audit --no-fund dragology@0.1.7 preact@10.29.8 htm@3.1.1 esbuild@0.24.2

# The npm tarball ships without its license; this is the repo's (declarative-dragging/dragology).
cat > node_modules/dragology/LICENSE <<'EOF'
MIT License

Copyright (c) 2025 Dragology contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
EOF

echo 'export const PrettyPrint = () => null;' > pretty-stub.js

# Runs after the part's html (stage appends kit scripts there), so the agent's
# <script type="text/dragology"> blocks, run last and in order, find the globals.
cat > entry.js <<'EOF'
import * as React from "preact/compat";
import { createRoot } from "preact/compat/client";
import htm from "htm";
import * as Dragology from "dragology";

window.dragology = {
  ...Dragology,
  React,
  html: htm.bind(React.createElement),
  mount: (el, draggable, props) =>
    createRoot(el).render(React.createElement(Dragology.DraggableRenderer, { draggable, ...props })),
};

for (const source of document.querySelectorAll('script[type="text/dragology"]')) {
  const run = document.createElement("script");
  run.text = source.text;
  document.documentElement.append(run);
  run.remove();
}
EOF

# Dragology ticks on every animation frame for as long as it is mounted, which would keep stage's
# frame timer snapshotting a page that is standing still. Patch its loop to park after a tick that
# finds nothing to animate and to wake on pointer events. Pinned to 0.1.7's minified source; a
# bump that moves these strings fails the build instead of shipping the idle loop.
cat > build.js <<'EOF'
const fs = require("fs");
const idle = {
  name: "dragology-idle",
  setup(build) {
    build.onLoad({ filter: /dragology\/dist-lib\/index\.js$/ }, (args) => {
      let src = fs.readFileSync(args.path, "utf8");
      const swap = (from, to) => {
        if (!src.includes(from)) throw new Error(`dragology's frame loop changed, repatch: ${from}`);
        src = src.replace(from, to);
      };
      swap("let e4,n2=()=>{t2.current(),e4=requestAnimationFrame(n2)};return e4=requestAnimationFrame(n2),()=>{cancelAnimationFrame(e4)}",
        'let e4=0,n2=()=>{e4=0,t2.current()!=null&&(e4=requestAnimationFrame(n2))},wake=()=>{e4||(e4=requestAnimationFrame(n2))},ev=["pointerdown","pointermove","pointerup","click"];' +
        "ev.forEach(x=>addEventListener(x,wake,true)),wake();return()=>{cancelAnimationFrame(e4),ev.forEach(x=>removeEventListener(x,wake,true))}");
      swap("e4&&x2(e4)})", "e4&&x2(e4);return e4})");
      return { contents: src, loader: "js" };
    });
  },
};
require("esbuild").build({
  entryPoints: ["entry.js"], bundle: true, minify: true, format: "iife", legalComments: "none",
  alias: { react: "preact/compat", "react/jsx-runtime": "preact/jsx-runtime", "@joshuahhh/pretty-print": "./pretty-stub.js" },
  define: { "process.env.NODE_ENV": '"production"' },
  plugins: [idle], metafile: true, outfile: process.argv[2], logLevel: "warning",
}).then((r) => fs.writeFileSync("meta.json", JSON.stringify(r.metafile)));
EOF
node build.js "$out/dragology.js"

if grep -qE '</script|<!--' "$out/dragology.js"; then
  echo "bundle contains </script or <!--, which would end its inline <script>" >&2
  exit 1
fi

node - "$out/dragology-LICENSES" <<'EOF'
const fs = require("fs"), path = require("path");
const meta = JSON.parse(fs.readFileSync("meta.json", "utf8"));
const names = new Set();
for (const input of Object.keys(meta.inputs)) {
  const m = input.match(/^node_modules\/((?:@[^/]+\/)?[^/]+)\//);
  if (m) names.add(m[1]);
}
const sections = [...names].sort().map((name) => {
  const dir = path.join("node_modules", name);
  const pkg = JSON.parse(fs.readFileSync(path.join(dir, "package.json"), "utf8"));
  const file = fs.readdirSync(dir).find((f) => /^licen[cs]e(\.|$)/i.test(f));
  if (!file) throw new Error(`${name} ships no license file`);
  return `${name}@${pkg.version}\n${"-".repeat(40)}\n${fs.readFileSync(path.join(dir, file), "utf8").trim()}\n`;
});
fs.writeFileSync(process.argv[2],
  "Packages bundled into dragology.js, regenerated by scripts/vendor-dragology.sh.\n\n" + sections.join("\n\n"));
EOF

ls -l "$out/dragology.js" "$out/dragology-LICENSES"
