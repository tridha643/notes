#!/bin/bash
# Is this checkout worth driving? Read-only. Prints one line per check; exits 1 on a failure.
root=$(cd "$(dirname "$0")/../../../.." && pwd)
fail=0
ok() { echo "ok    $*"; }
bad() { echo "FAIL  $*"; fail=1; }

sock="$HOME/Library/Application Support/Notes/notesd.sock"
if notes sessions --json >/dev/null 2>&1; then ok "notesd answers ($sock)"; else bad "notesd unreachable at $sock (sandbox? run unsandboxed)"; fi

stage="$root/target/release/stage"
newest=$(find "$root/crates/stage/src" "$root/crates/proto/src" -name '*.rs' -newer "$stage" 2>/dev/null | head -1)
if [ ! -x "$stage" ]; then bad "no $stage: cargo build --release -p stage"
elif [ -n "$newest" ]; then bad "$stage is older than $newest: rebuild"
else ok "stage release build is current"; fi

helper="$root/apps/Notes/.build/release/stage-helper"
if [ -x "$helper" ] && [ -d "$(dirname "$helper")/Notes_NotesCore.bundle" ]; then ok "stage-helper with its pele bundle"
else bad "no release stage-helper + Notes_NotesCore.bundle: swift build -c release --package-path apps/Notes --product stage-helper"; fi

if id=$(notes sessions --json 2>/dev/null | python3 -c 'import sys,json; print(next(json.loads(l)["id"] for l in sys.stdin if json.loads(l).get("title")=="stage verify fixture"))' 2>/dev/null); then
  ok "fixture session $id"
else bad "no fixture: .claude/skills/verify/scripts/fixture.py"; fi

load=$(sysctl -n vm.loadavg | awk '{print $2}')
if awk "BEGIN{exit !($load < 5)}"; then ok "load $load"; else echo "warn  load $load: timings will be inflated"; fi

strays=$(pgrep -fl '/T/sv.*Caches/stage' || true)
if [ -z "$strays" ]; then ok "no stray verify helpers"; else bad "stray helpers from an earlier run: $strays"; fi
exit $fail
