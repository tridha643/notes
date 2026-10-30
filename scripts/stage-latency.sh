#!/bin/sh
# Publish→painted latency for stage: 100 posts into one session, p50/p95, fails if p95 ≥ 250 ms.
# Runs its own notesd on temp dirs (your vault and ~/Desktop/Stage are untouched) and stage in
# a pty via script(1). Publish times are taken right before each post-new request is written
# to the socket; paint times come from stage's STAGE_LATENCY_LOG.
set -eu
cd "$(dirname "$0")/.."
[ ! -f "$HOME/.cargo/env" ] || . "$HOME/.cargo/env"
cargo build -q --release -p notesd -p stage
dir=$(mktemp -d /tmp/stl.XXXXXX)
trap 'kill $stage_pid $notesd_pid 2>/dev/null; rm -rf "$dir"' EXIT
mkdir -p "$dir/vault" "$dir/home"
export NOTES_VAULT="$dir/vault" NOTES_DB="$dir/state/notes.db" NOTES_SOCK="$dir/s.sock" HOME="$dir/home"
target/release/notesd 2>/dev/null &
notesd_pid=$!
python3 - "$dir" <<'PY'
import json, os, socket, sys, time
sock = os.environ["NOTES_SOCK"]
for _ in range(3000):
    try:
        socket.socket(socket.AF_UNIX).connect(sock); break
    except OSError: time.sleep(0.01)
def post(session, i):
    s = socket.socket(socket.AF_UNIX); s.connect(sock)
    args = {"title": f"latency {i}", "parts": [{"kind": "markdown", "markdown": f"post {i}"}]}
    if session: args["session"] = session
    line = json.dumps({"v": 1, "id": i, "verb": "post-new", "args": args, "agent": "latency"}) + "\n"
    t = time.time_ns(); s.sendall(line.encode())
    r = json.loads(s.makefile().readline())["result"]["post"]
    return r, t
first, _ = post(None, 0)
open(f"{sys.argv[1]}/session", "w").write(first["session"])
PY
session=$(cat "$dir/session")
sleep 1000 2>/dev/null | STAGE_LATENCY_LOG="$dir/painted" script -q /dev/null sh -c "stty rows 40 cols 120; exec target/release/stage --session $session" >/dev/null &
stage_pid=$!
# Publish once stage has painted the session, so startup isn't counted as post latency.
for _ in $(seq 100); do [ -s "$dir/painted" ] && break; sleep 0.05; done
python3 - "$dir" "$session" <<'PY'
import json, os, socket, sys, time
d, session = sys.argv[1], sys.argv[2]
sock = os.environ["NOTES_SOCK"]
sent = {}
for i in range(1, 101):
    s = socket.socket(socket.AF_UNIX); s.connect(sock)
    args = {"session": session, "title": f"latency {i}", "parts": [{"kind": "markdown", "markdown": f"post {i}"}]}
    t = time.time_ns()
    s.sendall((json.dumps({"v": 1, "id": i, "verb": "post-new", "args": args, "agent": "latency"}) + "\n").encode())
    sent[json.loads(s.makefile().readline())["result"]["post"]["id"]] = t
    time.sleep(0.02)
time.sleep(1)
painted = {}
for line in open(f"{d}/painted"):
    pid, version, ns = line.split()
    painted.setdefault(pid, int(ns))
ms = sorted((painted[p] - t) / 1e6 for p, t in sent.items() if p in painted)
missing = len(sent) - len(ms)
if not ms:
    sys.exit("stage painted none of the posts")
p50, p95 = ms[len(ms) // 2], ms[min(len(ms) - 1, round(len(ms) * 0.95) - 1)]
print(f"publish→painted over {len(ms)} posts: p50 {p50:.1f} ms, p95 {p95:.1f} ms, max {ms[-1]:.1f} ms" + (f", {missing} never painted" if missing else ""))
sys.exit(1 if p95 >= 250 or missing else 0)
PY
