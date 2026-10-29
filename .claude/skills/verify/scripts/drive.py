#!/usr/bin/env python3
"""Drive `stage` the way a user does, in a PTY that answers like kitty, and record
what reached the screen. One JSON line per run on stdout; evidence files under --out.

  drive.py --session ID [--mode cold|warm] [--runs N] [--keys 'PageUp@1.0,z@1.6']
           [--stage BIN] [--helper BIN] [--size 120x40] [--cell 10x20] [--quit-at 3.0]
           [--out DIR]

cold: a fresh temp HOME per run, so stage launches its own stage-helper (first
      launch cost included). warm: one temp HOME for all runs; run 0 launches the
      helper and is marked "prewarm". The real ~/Library/Caches/stage helper and
      the user's running stages are never touched. Every helper this script started
      is killed at the end (by its socket path, which only it knows).
"""
import argparse, base64, contextlib, fcntl, json, os, re, select, shutil, signal, struct, sys, tempfile, termios, time
from _posixshmem import shm_unlink

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "../../../.."))
KEYS = {"PageUp": b"\x1b[5~", "PageDown": b"\x1b[6~", "Up": b"\x1b[A", "Down": b"\x1b[B", "Esc": b"\x1b", "Enter": b"\r"}
APC = re.compile(rb"(?:\x1b\[(\d+);(\d+)H)?\x1b_G([^;\x1b]*)(?:;([^\x1b]*))?\x1b\\")

ap = argparse.ArgumentParser()
ap.add_argument("--session", required=True)
ap.add_argument("--mode", choices=["cold", "warm"], default="cold")
ap.add_argument("--runs", type=int, default=1)
ap.add_argument("--keys", default="", help="comma list of KEY@seconds; KEY is a char or one of " + ",".join(KEYS))
ap.add_argument("--stage", default=f"{ROOT}/target/release/stage")
ap.add_argument("--helper", default=f"{ROOT}/apps/Notes/.build/release/stage-helper")
ap.add_argument("--size", default="120x40")
ap.add_argument("--cell", default="10x20")
ap.add_argument("--quit-at", type=float, default=3.0)
ap.add_argument("--out", default=f"{ROOT}/target/verify")
a = ap.parse_args()
COLS, ROWS = map(int, a.size.split("x"))
CW, CH = map(int, a.cell.split("x"))
SOCK = os.path.expanduser("~/Library/Application Support/Notes/notesd.sock")  # before HOME changes
keys = []
for k in filter(None, a.keys.split(",")):
    name, at = k.rsplit("@", 1)
    keys.append((float(at), name, KEYS.get(name, name.encode())))
keys.sort()
os.makedirs(a.out, exist_ok=True)


def kv(raw):
    return dict(p.split(b"=", 1) for p in raw.split(b",") if b"=" in p)


def run(n, home):
    ev = f"{a.out}/{a.mode}-{n}"
    os.makedirs(ev, exist_ok=True)
    env = {**os.environ, "HOME": home, "NOTES_SOCK": SOCK, "STAGE_HELPER": a.helper,
           "STAGE_TRACE": f"{ev}/stage-trace.log", "STAGE_LOOP_TRACE": f"{ev}/loop-trace.log",
           "STAGE_HELPER_TRACE": "1", "STAGE_HELPER_LOG": f"{ev}/helper.log", "TERM": "xterm-256color"}
    pid, fd = os.forkpty()
    if pid == 0:
        err = os.open(f"{ev}/stderr.log", os.O_WRONLY | os.O_CREAT | os.O_TRUNC)
        os.dup2(err, 2)
        os.execve(a.stage, [a.stage, "--session", a.session], env)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, COLS * CW, ROWS * CH))
    t0 = time.monotonic()
    raw = bytearray()
    # live: what kitty would show now (id -> placement), after places and deletes.
    sizes, placements, live, pending_keys, sent = {}, [], {}, list(keys), []
    held = {}  # image id -> bytes the terminal stores (freed by d=I / d=A, not by d=i)
    final = None
    memory = None
    quit_at = None
    while True:
        now = time.monotonic() - t0
        if select.select([fd], [], [], 0.02)[0]:
            try:
                chunk = os.read(fd, 1 << 16)
            except OSError:
                break
            if not chunk:
                break
            raw += chunk
            # Answer stage's probes like kitty: graphics OK, cell size, DA1.
            if b"\x1b_Gi=31" in chunk:
                os.write(fd, b"\x1b_Gi=31;OK\x1b\\")
            if b"\x1b[16t" in chunk:
                os.write(fd, f"\x1b[6;{CH};{CW}t".encode())
            if b"\x1b[c" in chunk:
                os.write(fd, b"\x1b[?62;22;52c")
            for m in APC.finditer(chunk):
                k = kv(m.group(3))
                if k.get(b"t") == b"s" and m.group(4):  # kitty unlinks shm it reads; so do we
                    with contextlib.suppress(OSError):
                        shm_unlink(base64.b64decode(m.group(4)).decode())
                if b"s" in k and b"i" in k:
                    sizes[int(k[b"i"])] = (int(k[b"s"]), int(k[b"v"]))
                    held[int(k[b"i"])] = int(k[b"s"]) * int(k[b"v"]) * 4
                elif k.get(b"t") == b"f" and b"i" in k:
                    held[int(k[b"i"])] = 0  # a PNG file; size unknown here
                if k.get(b"a") == b"d" and k.get(b"d") == b"I" and b"i" in k:
                    held.pop(int(k[b"i"]), None)
                if k.get(b"a") == b"d" and k.get(b"d") == b"A":
                    held.clear()
                if k.get(b"a") == b"d" and b"i" in k:
                    live.pop(int(k[b"i"]), None)
                if k.get(b"a") in (b"p", b"T") and b"i" in k and b"c" in k:
                    i = int(k[b"i"])
                    w, h, c, r = (int(k.get(x, b"0")) for x in (b"w", b"h", b"c", b"r"))
                    p = {"t_ms": round(now * 1000), "id": i, "row": int(m.group(1) or 0), "col": int(m.group(2) or 0),
                         "cells": [c, r], "src": [int(k.get(b"x", b"0")), int(k.get(b"y", b"0")), w, h],
                         "raster": sizes.get(i), "one_to_one": (w, h) == (c * CW, r * CH)}
                    placements.append(p)
                    live[i] = p
        while pending_keys and now >= pending_keys[0][0]:
            at, name, seq = pending_keys.pop(0)
            os.write(fd, seq)
            sent.append({"key": name, "t_ms": round(now * 1000)})
        if quit_at is None and now >= a.quit_at:
            final = [dict(p) for p in live.values()]
            rss = lambda pids: sum(int(x) for x in os.popen(f"ps -o rss= -p {','.join(pids)} 2>/dev/null").read().split()) if pids else 0
            helpers = os.popen(f"pgrep -f '{home}/Library/Caches/stage/'").read().split()
            memory = {"stage_rss_mb": round(rss([str(pid)]) / 1024, 1), "helper_rss_mb": round(rss(helpers) / 1024, 1),
                      "terminal_images": len(held), "terminal_image_mb": round(sum(held.values()) / 2**20, 1)}
            os.write(fd, b"q")
            quit_at = now
        if quit_at is not None and now > quit_at + 0.5:
            break
    # Bounded: q, then TERM, then KILL; never an unbounded wait on the child.
    stopped = "q"
    for sig in (None, signal.SIGTERM, signal.SIGKILL):
        if sig:
            with contextlib.suppress(ProcessLookupError):
                os.kill(pid, sig)
            stopped = sig.name
        if any(os.waitpid(pid, os.WNOHANG)[0] or time.sleep(0.05) for _ in range(20)):
            break
    os.close(fd)
    with open(f"{ev}/pty.bin", "wb") as f:
        f.write(raw)
    first = {}
    for p in placements:
        first.setdefault(p["id"], p)
    after_keys = []
    for s in sent:
        moved = [p["t_ms"] - s["t_ms"] for p in placements if p["t_ms"] >= s["t_ms"]]
        new = [p["t_ms"] - s["t_ms"] for p in first.values() if p["t_ms"] >= s["t_ms"]]
        after_keys.append({**s, "first_placement_ms": min(moved) if moved else None, "first_new_image_ms": min(new) if new else None})
    final = final if final is not None else list(live.values())
    err = open(f"{ev}/stderr.log").read()
    result = {"run": n, "mode": a.mode, "prewarm": a.mode == "warm" and n == 0, "session": a.session, "stopped_by": stopped,
              "first_placement_ms": min((p["t_ms"] for p in placements), default=None),
              "images_placed": len(first), "rasters": err.count("resvg "),
              "all_one_to_one": all(p["one_to_one"] for p in placements),
              # The proof for geometry: what is on screen at the end, not what was ever sent.
              "on_screen_at_end": [{k: p[k] for k in ("id", "row", "col", "cells", "src", "one_to_one")} for p in final],
              "memory_at_quit": memory, "keys": after_keys, "placements": placements, "evidence": ev}
    with open(f"{ev}/result.json", "w") as f:
        json.dump(result, f, indent=1)
    print(json.dumps({k: v for k, v in result.items() if k != "placements"}))


def kill_helper(home):
    os.system(f"pkill -f '{home}/Library/Caches/stage/' >/dev/null 2>&1")


# Short temp dirs: a unix socket path (HOME/Library/Caches/stage/helper-vN.sock) caps at 104 bytes.
shared = tempfile.mkdtemp(prefix="sv") if a.mode == "warm" else None
try:
    for n in range(a.runs):
        home = shared or tempfile.mkdtemp(prefix="sv")
        try:
            run(n, home)
        finally:
            if not shared:
                kill_helper(home)
                shutil.rmtree(home, ignore_errors=True)
finally:
    if shared:
        kill_helper(shared)
        shutil.rmtree(shared, ignore_errors=True)
