#!/usr/bin/env python3
"""Bounded load test for notesd's stage path: publish, read and burst-update a session
of mixed posts (markdown, code, json) from 1/4/16 concurrent clients.

    scripts/daemon-load.py --bin target/release/notesd [--json out.json]

Every case runs a fresh notesd on a temp HOME, NOTES_DB, NOTES_VAULT and NOTES_SOCK
(the session folder is $HOME/Desktop/Stage), never the real vault or the installed daemon.
Each client is its own process with one persistent connection, like stage's client.
"""

import argparse
import base64
import json
import multiprocessing as mp
import os
import random
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time

CODE = {
    "rust": "fn fib(n: u64) -> u64 {{\n    match n {{ 0 | 1 => n, _ => fib(n - 1) + fib(n - 2) }}\n}}\n// {i}\n",
    "python": "def walk(tree, depth=0):\n    for k, v in tree.items():\n        print('  ' * depth + str(k))\n        walk(v, depth + 1)  # {i}\n",
    "typescript": "export function clamp(x: number, lo = 0, hi = 1): number {{\n  return Math.min(hi, Math.max(lo, x)); // {i}\n}}\n",
}
# Few distinct code bodies, so renders hit the cache instead of highlighting thousands of snippets.
CODE_VARIANTS = 8


def new_id():
    s = base64.urlsafe_b64encode(os.urandom(8)).decode().rstrip("=")
    return "0" + s[1:] if s[0] in "-_" else s


def part(i, rev=0):
    kind = i % 3
    if kind == 0:
        body = "\n".join(f"- item {i}.{j} rev {rev}: the quick brown fox jumps" for j in range(20))
        return {"kind": "markdown", "markdown": f"## Post {i}\n\n{body}\n"}
    if kind == 1:
        lang = list(CODE)[i % len(CODE)]
        return {"kind": "code", "language": lang, "code": CODE[lang].format(i=(i + rev) % CODE_VARIANTS) * 10}
    return {"kind": "json", "data": {"post": i, "rev": rev, "rows": [{"k": j, "v": f"value {j}"} for j in range(15)]}}


def seed_session(stage_dir, posts):
    """Write one session file with `posts` mixed posts; notesd loads it at startup."""
    sid, now = new_id(), int(time.time())
    data = {
        "schema": 1, "id": sid, "agent": "load", "title": "load", "created_at": now,
        "last_active_at": now, "agent_seq": 0, "comments": [],
        "posts": [
            {"id": new_id(), "title": f"post {i}", "version": 1, "created_at": now,
             "updated_at": now, "parts": [part(i)]}
            for i in range(posts)
        ],
    }
    os.makedirs(stage_dir, exist_ok=True)
    with open(os.path.join(stage_dir, f"seed-{sid}.json"), "w") as f:
        json.dump(data, f, indent=2)
    return sid, [p["id"] for p in data["posts"]]


class Conn:
    def __init__(self, sock):
        self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.s.connect(sock)
        self.f = self.s.makefile("rb")
        self.next = 0

    def send(self, verb, args=None):
        self.next += 1
        req = {"v": 1, "id": self.next, "verb": verb, "agent": "load"}
        if args is not None:
            req["args"] = args
        self.s.sendall((json.dumps(req) + "\n").encode())

    def call(self, verb, args=None):
        self.send(verb, args)
        line = self.f.readline()
        if not line:
            raise ConnectionError("notesd closed the connection")
        return json.loads(line)


def timed(conn, stats, errors, name, verb, args=None):
    t = time.perf_counter_ns()
    try:
        r = conn.call(verb, args)
        ok = r.get("ok") is True
    except Exception as e:  # counted, not fatal: the run reports every error
        r, ok = {"error": str(e)}, False
    stats.setdefault(name, []).append(time.perf_counter_ns() - t)
    if not ok:
        errors.append(f"{name}: {r.get('error')}")
    return r


def worker(role, sock, sid, post_ids, deadline, seed, out):
    rng = random.Random(seed)
    conn, stats, errors = Conn(sock), {}, []
    i = 100_000 * seed
    while time.time() < deadline:
        if role == "publisher":
            i += 1
            timed(conn, stats, errors, "post-new", "post-new",
                  {"session": sid, "title": f"load {i}", "parts": [part(i)]})
        elif role == "reader":
            # What stage does on an event: list the session, then show + render one post.
            timed(conn, stats, errors, "post-list", "post-list", {"session": sid})
            pid = rng.choice(post_ids)
            timed(conn, stats, errors, "post-show", "post-show", {"id": pid})
            timed(conn, stats, errors, "post-render", "post-render", {"id": pid})
        elif role == "burster":
            # Bursts of 20 back-to-back updates to recent posts, then 100 ms quiet.
            for _ in range(20):
                i += 1
                pid = post_ids[-1 - rng.randrange(min(10, len(post_ids)))]
                timed(conn, stats, errors, "post-update", "post-update",
                      {"id": pid, "parts": [part(i, rev=i)]})
            time.sleep(0.1)
    out.put((role, stats, errors[:20], len(errors)))


def watch_latency(sock, sid, deadline, out):
    """Time from sending a post-new to its post-created event on a watch stream."""
    w = Conn(sock)
    w.send("watch", {"kinds": ["post-created"]})
    pub = Conn(sock)
    # The event can beat the response, so match whichever side lands second.
    sent, arrived, lat, lock = {}, {}, [], threading.Lock()

    def drain():
        while True:
            line = w.f.readline()
            if not line:
                return
            ev = json.loads(line).get("event") or {}
            t = time.perf_counter_ns()
            with lock:
                if ev.get("id") in sent:
                    lat.append(t - sent.pop(ev["id"]))
                else:
                    arrived[ev.get("id")] = t

    threading.Thread(target=drain, daemon=True).start()
    i = 0
    while time.time() < deadline:
        i += 1
        t = time.perf_counter_ns()
        r = pub.call("post-new", {"session": sid, "title": f"watched {i}", "parts": [part(i)]})
        if r.get("ok"):
            pid = r["result"]["post"]["id"]
            with lock:
                if pid in arrived:
                    lat.append(arrived.pop(pid) - t)
                else:
                    sent[pid] = t
        time.sleep(0.02)
    time.sleep(0.3)
    out.put(("watch", {"publish->event": lat}, [], 0))


def pct(xs, p):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, max(0, int(round(p / 100 * len(xs) + 0.5)) - 1))] / 1e6 if xs else None


def rss_kb(pid):
    try:
        return int(subprocess.check_output(["ps", "-o", "rss=", "-p", str(pid)]).strip())
    except Exception:
        return 0


def run_case(binary, scenario, posts, clients, duration):
    tmp = tempfile.mkdtemp(prefix="ndl-", dir="/tmp")  # short: sun_path is 104 bytes
    home = os.path.join(tmp, "home")
    env = dict(os.environ, HOME=home, NOTES_VAULT=os.path.join(tmp, "vault"),
               NOTES_DB=os.path.join(tmp, "state", "notes.db"), NOTES_SOCK=os.path.join(tmp, "s.sock"))
    env.pop("NOTES_AGENT", None)
    real_home = os.path.expanduser("~")
    assert not env["NOTES_SOCK"].startswith(real_home) and env["HOME"] != real_home
    sid, post_ids = seed_session(os.path.join(home, "Desktop", "Stage"), posts)
    log = open(os.path.join(tmp, "notesd.log"), "w")
    proc = subprocess.Popen([binary], env=env, stdout=log, stderr=log)
    try:
        for _ in range(200):
            try:
                if Conn(env["NOTES_SOCK"]).call("doctor").get("ok"):
                    break
            except OSError:
                time.sleep(0.05)
        else:
            raise RuntimeError("notesd didn't come up")
        rss0 = rss_kb(proc.pid)
        out, procs = mp.Queue(), []
        deadline = time.time() + duration
        roles = {
            "publish": ["publisher"] * clients,
            "read": ["reader"] * clients,
            "mixed": ["burster"] + ["reader"] * clients,
        }[scenario]
        for n, role in enumerate(roles):
            procs.append(mp.Process(target=worker, args=(role, env["NOTES_SOCK"], sid, post_ids, deadline, n + 1, out)))
        if scenario == "mixed":
            procs.append(mp.Process(target=watch_latency, args=(env["NOTES_SOCK"], sid, deadline, out)))
        for p in procs:
            p.start()
        peak = rss0
        while time.time() < deadline:
            peak = max(peak, rss_kb(proc.pid))
            time.sleep(0.25)
        results = [out.get(timeout=duration + 30) for _ in procs]
        for p in procs:
            p.join()
        rss1 = rss_kb(proc.pid)
        peak = max(peak, rss1)
        stage_dir = os.path.join(home, "Desktop", "Stage")
        file_bytes = sum(os.path.getsize(os.path.join(stage_dir, f)) for f in os.listdir(stage_dir) if f.endswith(".json"))
        alive = proc.poll() is None
    finally:
        proc.terminate()
        proc.wait()
        log.close()
    merged, errors, nerr = {}, [], 0
    for _, stats, errs, n in results:
        for k, v in stats.items():
            merged.setdefault(k, []).extend(v)
        errors += errs
        nerr += n
    ops = {
        k: {"n": len(v), "per_s": round(len(v) / duration, 1), "p50": pct(v, 50), "p95": pct(v, 95),
            "p99": pct(v, 99), "max": max(v) / 1e6 if v else None}
        for k, v in sorted(merged.items())
    }
    shutil.rmtree(tmp, ignore_errors=True)
    return {"scenario": scenario, "posts": posts, "clients": clients, "duration_s": duration,
            "ops": ops, "errors": nerr, "error_samples": errors[:5], "daemon_alive": alive,
            "rss_mb": {"start": round(rss0 / 1024, 1), "peak": round(peak / 1024, 1), "end": round(rss1 / 1024, 1)},
            "session_file_mb": round(file_bytes / 2**20, 2)}


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bin", required=True, help="notesd binary to test")
    ap.add_argument("--scenarios", default="publish,read,mixed")
    ap.add_argument("--sizes", default="100,1000", help="posts seeded into the session")
    ap.add_argument("--clients", default="1,4,16")
    ap.add_argument("--duration", type=float, default=5.0, help="seconds per case (max 30)")
    ap.add_argument("--json", help="write every case's result here")
    a = ap.parse_args()
    assert 0 < a.duration <= 30, "keep each case under 30 s"
    binary = os.path.abspath(a.bin)
    cases = []
    print(f"{'scenario':8} {'posts':>5} {'cl':>3}  {'op':15} {'n':>6} {'/s':>8} {'p50':>8} {'p95':>8} {'p99':>8} {'max':>8}  err  rss MB (start/peak/end)")
    for scenario in a.scenarios.split(","):
        for posts in map(int, a.sizes.split(",")):
            for clients in map(int, a.clients.split(",")):
                r = run_case(binary, scenario, posts, clients, a.duration)
                cases.append(r)
                rss = r["rss_mb"]
                for op, s in r["ops"].items():
                    print(f"{scenario:8} {posts:>5} {clients:>3}  {op:15} {s['n']:>6} {s['per_s']:>8} "
                          f"{s['p50'] or 0:>8.2f} {s['p95'] or 0:>8.2f} {s['p99'] or 0:>8.2f} {s['max'] or 0:>8.2f}  "
                          f"{r['errors']:>3}  {rss['start']}/{rss['peak']}/{rss['end']}")
                if r["errors"] or not r["daemon_alive"]:
                    print(f"  ! errors {r['error_samples']} alive={r['daemon_alive']}", file=sys.stderr)
                sys.stdout.flush()
    if a.json:
        with open(a.json, "w") as f:
            json.dump({"bin": binary, "cases": cases}, f, indent=1)


if __name__ == "__main__":
    main()
