#!/usr/bin/env python3
"""Load test for stage against a real notesd, with stage in a PTY.

Every scenario runs its own notesd on a temp HOME/NOTES_VAULT/NOTES_DB/NOTES_SOCK
(your vault, database and ~/Desktop/Stage are never touched), starts stage in a
PTY sized like a pane, publishes over the socket from separate processes, and
records every byte stage writes with its read time. Afterwards a small VT
parser replays those bytes into a screen grid at each synchronized-update end
(CSI ?2026l), so "visible" means the post's body token was in a painted frame.

Latencies per post, all from just before its post-new is written:
  ack:       notesd's reply.
  event:     an independent watch client receives the post's event (notesd's share).
  row:       STAGE_LATENCY_LOG, the first paint after the post's summary joined
             stage's post list. Its body may still say "loading…" or be off screen.
             event→row is the same minus the event time (stage's share).
  visible:   the first painted frame that shows the post's body token. Posts that
             follow mode scrolls past within a burst are never on screen; those are
             counted as "skipped", and the page-through check (`j` over every post
             after the run) proves each one's body did load.
Keys: every ~250 ms during the run and its drain, `:` then Esc; key latency is until
the keybar shows (or drops) the command prompt. Also reported: stage's max RSS and
CPU (wait4), STAGE_LOOP_TRACE update/draw times, cold start (spawn → newest body).
A scenario passes when every publish succeeded, every post reached the latency log,
the header's post count matches notesd, every body was on screen in the walk, and
stage quit cleanly. Each live phase stops at 30 s.

Usage:
  scripts/stage-load.py [--stage BIN] [--notesd BIN] [--only NAME,...] [--out FILE] [--keep] [--sample S]
"""

import argparse
import fcntl
import json
import os
import re
import secrets
import select
import shutil
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import termios
import time
import unicodedata
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ROWS, COLS, CELL_W, CELL_H = 40, 120, 8, 16
BODY = re.compile(r"x([0-9a-f]{10})")
SCENARIO_LIMIT_S = 30

SCENARIOS = {
    # posts before stage starts, posts during, publisher processes, total rate (posts/s, 0 = flat out),
    # and post-update calls on the newest post during
    "paced": dict(preload=1, live=300, publishers=1, rate=20, patches=0),
    "burst": dict(preload=1, live=1000, publishers=8, rate=0, patches=0),
    "large-100": dict(preload=100, live=100, publishers=2, rate=20, patches=0),
    "large-1000": dict(preload=1000, live=200, publishers=4, rate=20, patches=0),
    "patch": dict(preload=20, live=0, publishers=1, rate=20, patches=200),
}


# ---- publishing (runs in its own process: `stage-load.py _publish spec.json out.json`) ----

def parts_for(i, tok, kind):
    body = f"x{tok}"
    if kind == "markdown":
        lines = [f"post {i} {body}", ""] + [f"- item {j}: some **bold** and `code` words" for j in range(1 + i % 12)]
        return [{"kind": "markdown", "markdown": "\n".join(lines)}]
    if kind == "code":
        code = f'let id = "{body}";\n' + "".join(f"fn f{j}(a: u32) -> u32 {{ a * {j} + 1 }}\n" for j in range(2 + i % 20))
        return [{"kind": "markdown", "markdown": f"code {i}"}, {"kind": "code", "code": code, "language": "rust"}]
    return [{"kind": "json", "data": {"token": body, "i": i, "items": [{"k": j, "v": [j, j * 2]} for j in range(1 + i % 6)]}}]


KINDS = ["markdown", "code", "json"]


class Conn:
    def __init__(self, path):
        self.s = socket.socket(socket.AF_UNIX)
        self.s.settimeout(10)
        self.s.connect(path)
        self.r = self.s.makefile("rb")
        self.n = 0

    def call(self, verb, args):
        self.n += 1
        line = json.dumps({"v": 1, "id": self.n, "verb": verb, "args": args, "agent": "load"}) + "\n"
        t = time.time_ns()
        self.s.sendall(line.encode())
        reply = json.loads(self.r.readline())
        return t, time.time_ns(), reply


def publish(spec_path, out_path):
    spec = json.loads(Path(spec_path).read_text())
    conn = Conn(spec["sock"])
    out = []
    while time.time_ns() < spec["start_ns"]:
        time.sleep(0.0005)
    gap = 1e9 / spec["rate"] if spec["rate"] else 0
    for n, item in enumerate(spec["items"]):
        if time.time_ns() > spec["deadline_ns"]:
            break
        if gap:
            due = spec["start_ns"] + n * gap
            while time.time_ns() < due:
                time.sleep(min(0.002, max(0, (due - time.time_ns()) / 1e9)))
        parts = parts_for(item["i"], item["tok"], item["kind"])
        if item.get("patch"):
            t, ack, r = conn.call("post-update", {"id": item["patch"], "parts": parts})
        else:
            args = {"title": f"p{item['i']:04d}", "parts": parts}
            if spec.get("session"):
                args["session"] = spec["session"]
            t, ack, r = conn.call("post-new", args)
        res = r.get("result") or {}
        post = res.get("post") or {}
        out.append({**item, "id": post.get("id"), "version": post.get("version"), "session": post.get("session"),
                    "t": t, "ack": ack, "err": None if "result" in r else r.get("error")})
    Path(out_path).write_text(json.dumps(out))


def watch(sock, out_path):
    """An independent watch client: `id version arrival_ns` per post event, to split
    publish→event (notesd) from event→paint (stage)."""
    s = socket.socket(socket.AF_UNIX)
    s.connect(sock)
    s.sendall((json.dumps({"v": 1, "id": 0, "verb": "watch", "args": {"kinds": ["post"]}}) + "\n").encode())
    with open(out_path, "w") as out:
        for line in s.makefile("rb"):
            t = time.time_ns()
            e = json.loads(line)
            e = e.get("event", e)
            if e.get("id") and e.get("version"):
                out.write(f"{e['id']} {e['version']} {t}\n")
                out.flush()


# ---- PTY reader (own process: `stage-load.py _read master rec evfd`) -----------------

def read_pty(master, rec_path, ev_fd):
    """Record every chunk with its read time, answer the probe's queries (cell size,
    then DA1, so stage stops waiting and doesn't eat our first keys), and signal
    each frame end on `ev_fd`. Kitty's query goes unanswered: text cards only."""
    master, ev_fd = int(master), int(ev_fd)
    rec = open(rec_path, "wb")
    carry, answered = b"", set()
    while True:
        try:
            d = os.read(master, 1 << 16)
        except OSError:
            break
        if not d:
            break
        t = time.time_ns()
        rec.write(struct.pack("<QI", t, len(d)))
        rec.write(d)
        s = carry + d
        for q, a in ((b"\x1b[16t", f"\x1b[6;{CELL_H};{CELL_W}t".encode()), (b"\x1b[c", b"\x1b[?62;22c")):
            if q in s and q not in answered:
                answered.add(q)
                os.write(master, a)
        n = s.count(b"\x1b[?2026l") - carry.count(b"\x1b[?2026l")
        if n:
            os.write(ev_fd, b"f" * n)
        carry = s[-8:]
    rec.close()


# ---- screen replay -------------------------------------------------------------------

TOKEN = re.compile(
    rb"\x1b\[([0-9;?<>=]*)[ -/]*([@-~])"      # CSI
    rb"|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)"     # OSC
    rb"|\x1b[_P^][\s\S]*?\x1b\\"               # APC / DCS / PM
    rb"|\x1b[\s\S]"                              # other ESC x
    rb"|[\r\n\x08]"
    rb"|[^\x1b\r\n\x08]+"
)


def frames(rec_path):
    """Yield (time_ns, screen rows) at each synchronized-update end."""
    data, marks = bytearray(), []
    with open(rec_path, "rb") as f:
        while h := f.read(12):
            t, n = struct.unpack("<QI", h)
            data += f.read(n)
            marks.append((len(data), t))
    grid = [[" "] * COLS for _ in range(ROWS)]
    row = col = 0
    mi = 0
    for m in TOKEN.finditer(bytes(data)):
        tok = m.group(0)
        if tok[0] == 0x1B:
            final = m.group(2)
            if final is None:
                continue
            params = m.group(1).decode()
            if final == b"H" or final == b"f":
                p = [int(x) if x else 1 for x in params.split(";")] if params else [1, 1]
                row, col = min(ROWS, p[0]) - 1, min(COLS, p[1] if len(p) > 1 else 1) - 1
            elif final == b"J" and params in ("2", "3"):
                grid = [[" "] * COLS for _ in range(ROWS)]
            elif final == b"K":
                a, b = {"": (col, COLS), "0": (col, COLS), "1": (0, col + 1), "2": (0, COLS)}.get(params, (col, COLS))
                grid[row][a:b] = [" "] * (b - a)
            elif final == b"G":
                col = min(COLS, int(params or 1)) - 1
            elif final == b"C":
                col = min(COLS - 1, col + int(params or 1))
            elif final == b"l" and params == "?2026":
                end = m.end()
                while mi < len(marks) and marks[mi][0] < end:
                    mi += 1
                yield marks[min(mi, len(marks) - 1)][1], ["".join(r) for r in grid]
            continue
        if tok == b"\r":
            col = 0
        elif tok == b"\n":
            row = min(ROWS - 1, row + 1)
        elif tok == b"\x08":
            col = max(0, col - 1)
        else:
            for ch in tok.decode("utf-8", "replace"):
                if unicodedata.combining(ch):
                    continue
                if col < COLS:
                    grid[row][col] = ch
                col += 2 if unicodedata.east_asian_width(ch) in "WF" else 1


# ---- one scenario -----------------------------------------------------------------------

def pct(xs, p):
    if not xs:
        return None
    xs = sorted(xs)
    return round(xs[min(len(xs) - 1, max(0, -(-len(xs) * p // 100) - 1))], 1)


def summary(xs):
    return {"n": len(xs), "p50": pct(xs, 50), "p95": pct(xs, 95), "p99": pct(xs, 99), "max": round(max(xs), 1) if xs else None}


def wait_sock(path, proc, timeout=10):
    end = time.time() + timeout
    while time.time() < end:
        if proc.poll() is not None:
            sys.exit(f"notesd exited with {proc.returncode}")
        try:
            socket.socket(socket.AF_UNIX).connect(path)
            return
        except OSError:
            time.sleep(0.01)
    sys.exit("notesd never opened its socket")


def run_publishers(d, sock, session, items, publishers, rate, deadline_ns, tag):
    start_ns = time.time_ns() + 200_000_000
    procs = []
    for k in range(publishers):
        spec = d / f"{tag}-{k}.spec"
        spec.write_text(json.dumps({"sock": sock, "session": session, "items": items[k::publishers],
                                    "rate": rate / publishers if rate else 0, "start_ns": start_ns, "deadline_ns": deadline_ns}))
        procs.append((subprocess.Popen([sys.executable, __file__, "_publish", str(spec), str(d / f"{tag}-{k}.out")]), d / f"{tag}-{k}.out"))
    return procs


def collect(procs, timeout=65):
    out = []
    deadline = time.monotonic() + timeout
    for p, path in procs:
        p.wait(timeout=max(.01, deadline - time.monotonic()))
        if p.returncode:
            raise RuntimeError(f"publisher exited {p.returncode}: {path}")
        out += json.loads(path.read_text())
    return out


class Frames:
    """Live frame-end notifications from the reader process."""

    def __init__(self, fd):
        self.fd = fd

    def drain(self, timeout):
        got = 0
        while select.select([self.fd], [], [], timeout)[0]:
            got += len(os.read(self.fd, 4096))
            timeout = 0
        return got

    def quiet(self, gap=0.15, limit=20):
        """Wait until no frame has ended for `gap` seconds."""
        end = time.time() + limit
        while time.time() < end and self.drain(gap):
            pass


def scenario(name, cfg, stage_bin, notesd_bin, keep, sample):
    d = Path(tempfile.mkdtemp(prefix=f"stage-load-{name}-"))
    home = d / "home"
    (home / "Desktop").mkdir(parents=True)
    (d / "vault").mkdir()
    sock = str(d / "s.sock")
    env = {**os.environ, "HOME": str(home), "NOTES_VAULT": str(d / "vault"), "NOTES_DB": str(d / "state/notes.db"), "NOTES_SOCK": sock,
           "STAGE_LATENCY_LOG": str(d / "painted"), "STAGE_LOOP_TRACE": str(d / "loop"), "STAGE_HELPER": str(d / "no-stage-helper"), "TERM": "xterm-256color"}
    for k in ("NOTES_OWNER", "CLAUDE_SESSION_ID", "STAGE_TRACE"):
        env.pop(k, None)
    notesd = subprocess.Popen([notesd_bin], env=env, stdout=subprocess.DEVNULL, stderr=open(d / "notesd.err", "wb"))
    stage = reader = watcher = sampler = None
    procs = pre_procs = []
    result = {"scenario": name, **cfg}
    try:
        wait_sock(sock, notesd)
        # Seed and preload before stage starts: the first post makes the session.
        _, _, first = Conn(sock).call("post-new", {"title": "p0000", "parts": parts_for(0, secrets.token_hex(5), "markdown")})
        session = first["result"]["post"]["session"]
        tok = iter(range(1, 10**9))
        pre = [{"i": next(tok), "tok": secrets.token_hex(5), "kind": KINDS[n % 3]} for n in range(cfg["preload"] - 1)]
        t0 = time.time()
        pre_procs = run_publishers(d, sock, session, pre, 8, 0, time.time_ns() + 60 * 10**9, "pre")
        preloaded = collect(pre_procs)
        result["preload_s"] = round(time.time() - t0, 2)

        master, slave = os.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, COLS * CELL_W, ROWS * CELL_H))
        ev_r, ev_w = os.pipe()
        reader = subprocess.Popen([sys.executable, __file__, "_read", str(master), str(d / "pty.rec"), str(ev_w)], pass_fds=(master, ev_w))
        os.close(ev_w)
        fr = Frames(ev_r)

        def ctty():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)

        spawn_ns = time.time_ns()
        stage = subprocess.Popen([stage_bin, "--session", session], env=env, stdin=slave, stdout=slave, stderr=open(d / "stage.err", "wb"),
                                 preexec_fn=ctty, close_fds=True)
        os.close(slave)
        # Ready once stage has painted the session's rows and gone quiet, so startup isn't post latency.
        while not (d / "painted").exists() or not (d / "painted").stat().st_size:
            if stage.poll() is not None or time.time_ns() - spawn_ns > 30 * 10**9:
                sys.exit(f"stage never painted the session (see {d})")
            fr.drain(0.01)
        fr.quiet(gap=0.3, limit=30)
        ready_ns = time.time_ns()

        keys = []  # (t_ns, key)

        def key(k):
            keys.append((time.time_ns(), k))
            os.write(master, k)

        live = [{"i": next(tok), "tok": secrets.token_hex(5), "kind": KINDS[n % 3]} for n in range(cfg["live"])]
        if cfg["patches"]:
            target = Conn(sock).call("post-list", {"session": session, "limit": 1})[2]["result"][0]["id"]
            live = [{"i": 9000 + n, "tok": secrets.token_hex(5), "kind": KINDS[n % 3], "patch": target} for n in range(cfg["patches"])]
        deadline = time.time_ns() + SCENARIO_LIMIT_S * 10**9
        watcher = subprocess.Popen([sys.executable, __file__, "_watch", sock, str(d / "events")])
        time.sleep(0.1)
        procs = run_publishers(d, sock, session, live, cfg["publishers"], cfg["rate"], deadline, "live")
        if sample:
            sampler = subprocess.Popen(["/usr/bin/sample", str(stage.pid), str(sample), "1", "-mayDie", "-file", str(d / "stage.sample")],
                                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        probes = []
        rows_due = cfg["preload"] + cfg["live"]

        def behind():
            """Publishers still running, or stage hasn't painted every row yet (probe through the drain too)."""
            if time.time_ns() >= deadline:
                return False
            if any(p.poll() is None for p, _ in procs):
                return True
            return (d / "painted").read_text().count("\n") < rows_due and time.time_ns() < deadline

        while behind():
            fr.drain(0.05)
            key(b":")
            probes.append(len(keys) - 1)
            fr.drain(0.1)
            key(b"\x1b")
            fr.drain(0.1)
        pubs = preloaded + collect(procs, timeout=2)
        if sample:
            sampler.wait()
        fr.quiet(gap=0.5, limit=30)

        # Page through every post from the top: each `j` brings the next title up.
        daemon_posts = Conn(sock).call("post-list", {"session": session})[2]["result"]
        n_posts = len(daemon_posts)
        walk_start = time.time_ns()
        key(b"g")
        fr.drain(1.0)
        fr.quiet(gap=0.05)
        walk_keys = 0
        walk_deadline = time.time() + 60
        while walk_keys < n_posts + 2 and time.time() < walk_deadline:
            key(b"j")
            walk_keys += 1
            fr.drain(1.0)
        fr.quiet(gap=0.2)
        walk_end = time.time_ns()
        key(b"q")
        result["stage_exit"], ru = reap(stage, 5)
        result["stage_max_rss_mb"] = round(ru.ru_maxrss / 2**20, 1)
        result["stage_cpu_s"] = round(ru.ru_utime + ru.ru_stime, 2)
    finally:
        # A hung writer must not leave publishers alive or make a bounded test hang.
        for child in [p for p, _ in pre_procs + procs] + ([sampler] if sampler else []):
            if child.poll() is None:
                child.kill()
            child.wait()
        if stage and stage.returncode is None:
            stage.kill()
            reap(stage, 5)
        notesd.send_signal(signal.SIGTERM)
        try:
            notesd.wait(5)
        except subprocess.TimeoutExpired:
            notesd.kill()
        if reader:
            reader.wait(5)
        if watcher:
            watcher.kill()

    return analyse(d, result, pubs, keys, probes, spawn_ns, ready_ns, walk_start, walk_end, walk_keys, n_posts, daemon_posts, keep)


def reap(proc, timeout):
    """Wait for `proc` with wait4, for its rusage (max RSS, CPU); kill it at `timeout`."""
    end = time.time() + timeout
    while True:
        pid, status, ru = os.wait4(proc.pid, os.WNOHANG)
        if pid:
            proc.returncode = os.waitstatus_to_exitcode(status)
            return proc.returncode, ru
        if time.time() > end:
            proc.kill()
            end = time.time() + 5
        time.sleep(0.01)


def analyse(d, result, pubs, keys, probes, spawn_ns, ready_ns, walk_start, walk_end, walk_keys, n_posts, daemon_posts, keep):
    errs = [p for p in pubs if p["err"]]
    ok = [p for p in pubs if not p["err"]]
    by_tok = {p["tok"]: p for p in ok}
    painted = {}
    if (d / "painted").exists():
        for line in (d / "painted").read_text().splitlines():
            pid, ver, ns = line.split()
            painted.setdefault((pid, int(ver)), int(ns))

    events = {}
    if (d / "events").exists():
        for line in (d / "events").read_text().splitlines():
            pid, ver, ns = line.split()
            events.setdefault((pid, int(ver)), int(ns))
    first_seen, last_frame_rows = {}, None
    keybar = []  # (t, keybar row)
    n_frames = 0
    frame_times = []
    for t, rows in frames(d / "pty.rec"):
        n_frames += 1
        frame_times.append(t)
        for m in BODY.finditer("\n".join(rows)):
            first_seen.setdefault(m.group(1), t)
        keybar.append((t, rows[-1]))
        last_frame_rows = rows

    def ms(a, b):
        return (a - b) / 1e6

    live = [p for p in ok if p["t"] >= ready_ns]
    ack = [ms(p["ack"], p["t"]) for p in live]
    event = [ms(events[(p["id"], p["version"])], p["t"]) for p in live if (p["id"], p["version"]) in events]
    stage_row = [ms(painted[k], events[k]) for k in ((p["id"], p["version"]) for p in live) if k in painted and k in events]
    row = [ms(painted[(p["id"], p["version"])], p["t"]) for p in live if (p["id"], p["version"]) in painted]
    visible = [ms(first_seen[p["tok"]], p["t"]) for p in live if p["tok"] in first_seen and first_seen[p["tok"]] < walk_start]
    never_row = [p for p in live if (p["id"], p["version"]) not in painted and not p.get("patch")]
    patches = [p for p in live if p.get("patch")]
    # A patched post shows only its newest body, so only the final patch must be on screen in the walk.
    must_see = [p for p in ok if not p.get("patch")] + (patches[-1:] if patches else [])
    unseen = [p["i"] for p in must_see if p["tok"] not in first_seen]

    # Only attribute a frame before the next key. A colon cancelled by Esc before
    # paint is unanswered, NOT answered by a different probe hundreds of ms later.
    def answered(t, want, until):
        for ft, bar in keybar:
            if ft >= until:
                break
            if ft >= t and ("enter run" in bar) == want:
                return ms(ft, t)
        return None

    colon = [answered(keys[i][0], True, keys[i + 1][0]) for i in probes if i + 1 < len(keys)]
    esc = [answered(keys[i + 1][0], False, keys[i + 2][0] if i + 2 < len(keys) else walk_start)
           for i in probes if i + 1 < len(keys)]
    walk_lat = []
    wk = [t for t, k in keys if k == b"j"]
    fi = 0
    for t in wk:
        while fi < len(frame_times) and frame_times[fi] < t:
            fi += 1
        if fi < len(frame_times):
            walk_lat.append(ms(frame_times[fi], t))

    loop = {"update": [], "draw": [], "bootstrap": None}
    if (d / "loop").exists():
        for line in (d / "loop").read_text().splitlines():
            w = line.split()
            if w[0] == "bootstrap":
                loop["bootstrap"] = float(w[1])
            elif w[0] == "draw":
                loop["draw"].append(float(w[1]))
            elif w[0] == "update":
                loop["update"].append(float(w[3]))

    # Cold start: spawn → first frame showing the body of the newest post stage started with
    # (it opens on that one). post-list is newest first.
    tok_of = {p["id"]: p["tok"] for p in ok}
    newest = next((tok_of[p["id"]] for p in daemon_posts if p["id"] in tok_of and by_tok[tok_of[p["id"]]]["t"] < ready_ns), None)
    cold = ms(first_seen[newest], spawn_ns) if newest in first_seen and first_seen[newest] < ready_ns else None

    header = last_frame_rows[0] if last_frame_rows else ""
    m = re.search(r"· (\d+) posts", header)
    result.update({
        "published": len(ok), "publish_errors": len(errs), "daemon_posts": n_posts,
        "live_posts": len(live), "live_window_s": round(ms(max((p["ack"] for p in live), default=ready_ns), min((p["t"] for p in live), default=ready_ns)) / 1000, 2),
        "throughput_posts_s": None,
        "ack_ms": summary(ack), "event_ms": summary(event), "event_to_row_ms": summary(stage_row), "row_ms": summary(row), "visible_ms": summary(visible),
        "visible_skipped": len([p for p in live if p["tok"] not in first_seen or first_seen[p["tok"]] >= walk_start]),
        "never_in_latency_log": len(never_row),
        "walk": {"keys": walk_keys, "s": round(ms(walk_end, walk_start) / 1000, 2), "key_to_frame_ms": summary(walk_lat)},
        "unseen_after_walk": len(unseen), "unseen_sample": unseen[:10],
        "header_posts": int(m.group(1)) if m else None,
        "probe_colon_ms": summary([x for x in colon if x is not None]), "probe_esc_ms": summary([x for x in esc if x is not None]),
        "probe_attempts": len(colon) + len(esc),
        "probe_unanswered": sum(x is None for x in colon + esc),
        "cold_start_ms": round(cold, 1) if cold else None, "frames": n_frames,
        "loop_update_ms": summary(loop["update"]), "loop_draw_ms": summary(loop["draw"]), "bootstrap_ms": loop["bootstrap"],
        "dir": str(d),
    })
    if live:
        win = result["live_window_s"]
        result["throughput_posts_s"] = round(len(live) / win, 1) if win else None
        # Time from the last publish ack until stage has painted every live row.
        last_row = max((painted.get((p["id"], p["version"]), 0) for p in live), default=0)
        result["drain_after_last_ack_ms"] = round(ms(last_row, max(p["ack"] for p in live)), 1) if last_row else None
    # Every post-new is one post (plus the seed); patches add versions, not posts.
    requested = result["preload"] - 1 + result["live"] + result["patches"]
    result["requested_publishes"] = requested
    result["correct"] = (len(pubs) == requested and not errs and not never_row and not unseen and result["header_posts"] == n_posts
                         and len(ok) - len(patches) + 1 == n_posts and result["stage_exit"] == 0)
    if not keep and result["correct"]:
        shutil.rmtree(d, ignore_errors=True)
        result["dir"] = None
    return result


def brief(r):
    f = lambda s: f"{s['p50']}/{s['p95']}/{s['p99']}/{s['max']}" if s and s["n"] else "-"
    return (f"{r['scenario']:<11} {'ok' if r['correct'] else 'FAIL'}  posts {r['daemon_posts']} ({r['throughput_posts_s']}/s live)"
            f"  ack {f(r['ack_ms'])}  event {f(r['event_ms'])}  event→row {f(r['event_to_row_ms'])}  row {f(r['row_ms'])}  visible {f(r['visible_ms'])} (skipped {r['visible_skipped']})  key {f(r['probe_colon_ms'])} (unanswered {r['probe_unanswered']}/{r['probe_attempts']})"
            f"  update {f(r['loop_update_ms'])}  draw {f(r['loop_draw_ms'])}  cold {r['cold_start_ms']}  rss {r['stage_max_rss_mb']} MB"
            f"  cpu {r['stage_cpu_s']} s  unseen {r['unseen_after_walk']}" + (f"  kept {r['dir']}" if r["dir"] else ""))


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "_publish":
        return publish(*sys.argv[2:4])
    if len(sys.argv) > 1 and sys.argv[1] == "_watch":
        return watch(*sys.argv[2:4])
    if len(sys.argv) > 1 and sys.argv[1] == "_read":
        return read_pty(*sys.argv[2:5])
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--stage", default=str(ROOT / "target/release/stage"))
    ap.add_argument("--notesd", default=str(ROOT / "target/release/notesd"))
    ap.add_argument("--only", default=",".join(SCENARIOS))
    ap.add_argument("--out")
    ap.add_argument("--sample", type=int, metavar="SECONDS", help="profile stage with sample(1) from the live phase's start (keeps the dir)")
    ap.add_argument("--keep", action="store_true", help="keep temp dirs (always kept when a check fails)")
    a = ap.parse_args()
    results = []
    for name in a.only.split(","):
        r = scenario(name, SCENARIOS[name], a.stage, a.notesd, a.keep or bool(a.sample), a.sample)
        results.append(r)
        print(brief(r), flush=True)
    if a.out:
        Path(a.out).write_text(json.dumps(results, indent=1))
    sys.exit(0 if all(r["correct"] for r in results) else 1)


if __name__ == "__main__":
    main()
