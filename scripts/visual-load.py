#!/usr/bin/env python3
"""Bounded, isolated stage-helper stress test. No notesd, real posts or vault writes.

Measures helper request -> SVG/first RGBA frame (NOT terminal-visible latency).
Consumes/unlinks every received shm object and checks actual HTML pixels, dimensions,
per-page frame ordering and errors. RSS is helper-only; WebKit XPC RSS is not included.
Run after: cd apps/Notes && swift build -c release --product stage-helper
"""
import argparse
import ctypes
import json
import mmap
import os
from pathlib import Path
import queue
import subprocess
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
LIBC = ctypes.CDLL("/usr/lib/libSystem.B.dylib", use_errno=True)
LIBC.shm_open.argtypes = [ctypes.c_char_p, ctypes.c_int, ctypes.c_uint]
LIBC.shm_open.restype = ctypes.c_int
LIBC.shm_unlink.argtypes = [ctypes.c_char_p]


def pixels(reply):
    """Read pixels before unlinking, just as the terminal would."""
    name = reply["shm"].encode()
    fd = LIBC.shm_open(name, os.O_RDONLY, 0)
    try:
        if fd < 0:
            raise OSError(ctypes.get_errno(), "shm_open")
        size = reply["w"] * reply["h"] * 4
        assert 0 < size <= 64 * 1024 * 1024, f"invalid dimensions: {reply}"
        # macOS rounds POSIX shared-memory objects up to a VM page.
        assert size <= os.fstat(fd).st_size < size + mmap.PAGESIZE, "frame buffer size mismatch"
        with mmap.mmap(fd, size, access=mmap.ACCESS_READ) as data:
            # Several points, including opposite edges; retain no large pixel copies.
            offsets = (0, (reply["w"] - 1) * 4, size // 2 // 4 * 4, size - 4)
            return [tuple(data[i:i + 4]) for i in offsets]
    finally:
        if fd >= 0:
            os.close(fd)
        LIBC.shm_unlink(name)


class Helper:
    def __init__(self, binary, home):
        self.messages = queue.Queue()
        env = dict(os.environ, HOME=str(home))
        self.log = open(home / "helper.log", "w")
        self.process = subprocess.Popen([str(binary)], stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=self.log, env=env)
        self.reader = threading.Thread(target=self.read, daemon=True)
        self.reader.start()
        self.peak_rss_kib = 0
        self.stop = threading.Event()
        self.sampler = threading.Thread(target=self.sample, daemon=True)
        self.sampler.start()

    def read(self):
        try:
            for line in self.process.stdout:
                self.messages.put((time.perf_counter_ns(), json.loads(line)))
        except Exception as error:
            self.messages.put((time.perf_counter_ns(), {"error": str(error), "ok": False}))
        finally:
            self.messages.put((time.perf_counter_ns(), {"error": "helper EOF", "ok": False}))

    def sample(self):
        while not self.stop.is_set():
            result = subprocess.run(["ps", "-o", "rss=", "-p", str(self.process.pid)],
                                    capture_output=True, text=True)
            try:
                self.peak_rss_kib = max(self.peak_rss_kib, int(result.stdout.strip()))
            except ValueError:
                pass
            self.stop.wait(.2)

    def send(self, request):
        self.process.stdin.write((json.dumps(request) + "\n").encode())
        self.process.stdin.flush()

    def next(self, deadline):
        at, reply = self.messages.get(timeout=max(.001, deadline - time.monotonic()))
        if reply.get("ok") is False:
            raise RuntimeError(reply)
        return at, reply

    def close(self):
        self.stop.set()
        self.sampler.join(timeout=2)
        self.process.stdin.close()
        try:
            self.process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
        self.reader.join(timeout=2)
        while not self.messages.empty():
            _, reply = self.messages.get_nowait()
            if "shm" in reply:
                LIBC.shm_unlink(reply["shm"].encode())
        self.process.stdout.close()
        self.log.close()


def summary(values):
    ordered = sorted(values)
    def percentile(p):
        return round(ordered[min(len(ordered) - 1, int((len(ordered) - 1) * p + .5))], 3)
    return {"n": len(values), "p50_ms": percentile(.5), "p95_ms": percentile(.95),
            "p99_ms": percentile(.99), "max_ms": round(max(values), 3)}


def load(binary, concurrency, count, timeout):
    with tempfile.TemporaryDirectory(prefix="stage-visual-load-") as tmp:
        home = Path(tmp)
        helper = Helper(binary, home)
        try:
            deadline = time.monotonic() + timeout
            helper.send({"op": "warm", "id": 0})
            helper.next(deadline)
            pending, timings, sent, complete, peak_pending = {}, {"html": [], "mermaid": []}, 0, 0, 0
            start = time.perf_counter()
            while complete < count:
                while sent < count and len(pending) < concurrency:
                    sent += 1
                    ident = sent
                    kind = "html" if ident % 2 else "mermaid"
                    if kind == "html":
                        rgb = ((ident * 37) % 256, (ident * 67) % 256, (ident * 97) % 256)
                        width, height = 320 + ident % 4 * 80, 120 + ident % 5 * 30
                        path = home / f"page-{ident}.html"
                        path.write_text(f"<!doctype html><style>html,body{{margin:0}}div{{height:{height}px;background:rgb{rgb}}}</style><div></div>")
                        req = {"op": "html-open", "id": ident, "path": str(path), "width": width,
                               "max_height": 600, "scale": 2, "fps": 60}
                        expected = (width * 2, height * 2, (*rgb, 255))
                    else:
                        nodes = 12 + ident % 20
                        source = "flowchart TD\n" + "\n".join(f"N{i}[item {ident}-{i}] --> N{i + 1}" for i in range(nodes))
                        req = {"op": "mermaid", "id": ident, "source": source, "max_width": 500, "font_size": 14}
                        expected = None
                    pending[ident] = (kind, time.perf_counter_ns(), expected)
                    helper.send(req)
                    peak_pending = max(peak_pending, len(pending))
                try:
                    at, reply = helper.next(deadline)
                except queue.Empty as error:
                    raise TimeoutError(f"helper timed out: sent={sent}, completed={complete}, pending={list(pending)}, exit={helper.process.poll()}") from error
                ident = reply.get("id")
                if ident not in pending:
                    if "shm" in reply:
                        pixels(reply)
                    continue
                kind, began, expected = pending[ident]
                if kind == "html" and "frame" in reply:
                    got = pixels(reply)
                    width, height, rgba = expected
                    assert (reply["w"], reply["h"]) == (width, height), f"wrong page size: {reply}"
                    assert reply["frame"] == 1, f"first frame out of order: {reply}"
                    assert all(all(abs(a - b) <= 2 for a, b in zip(pixel, rgba)) for pixel in got), f"wrong/blank pixels: page {ident}, wanted {rgba}, got {got}"
                    helper.send({"op": "html-close", "id": ident})
                elif kind == "mermaid" and "svg" in reply:
                    assert "<svg" in reply["svg"] and "var(" not in reply["svg"], "unresolved SVG"
                    assert reply["width"] > 0 and reply["height"] > 0
                else:
                    continue
                timings[kind].append((at - began) / 1e6)
                del pending[ident]
                complete += 1
            elapsed = time.perf_counter() - start
            return {"concurrency": concurrency, "requests": count, "completed": complete,
                    "errors": 0, "peak_pending": peak_pending, "seconds": round(elapsed, 3),
                    "requests_per_second": round(count / elapsed, 1),
                    "helper_peak_rss_mib": round(helper.peak_rss_kib / 1024, 1),
                    **{kind: summary(values) for kind, values in timings.items()}}
        finally:
            helper.close()


def update_burst(binary, count, timeout):
    """Queue patches and script-changing reloads without waiting between revisions."""
    with tempfile.TemporaryDirectory(prefix="stage-update-load-") as tmp:
        home = Path(tmp)
        helper = Helper(binary, home)
        try:
            paths = []
            for n in range(count + 1):
                path = home / f"revision-{n}.html"
                rgb = ((n * 37) % 256, (n * 67) % 256, (n * 97) % 256)
                path.write_text(f"<body style='margin:0'><div id='box' style='height:120px;background:rgb{rgb}'></div><script>window.revision={n // 4}</script></body>")
                paths.append(path)
            deadline = time.monotonic() + timeout
            helper.send({"op": "html-open", "id": 1, "path": str(paths[0]), "width": 320,
                         "max_height": 600, "scale": 1, "fps": 60})
            _, first = helper.next(deadline)
            pixels(first)
            helper.send({"op": "html-ack", "id": 1, "frame": first["frame"]})
            started = time.perf_counter_ns()
            sent, ack_ms = [], []
            for path in paths[1:]:
                sent.append(time.perf_counter_ns())
                helper.send({"op": "html-update", "id": 1, "path": str(path)})
            frames = 0
            while True:
                at, reply = helper.next(deadline)
                if "patched" in reply:
                    ack_ms.append((at - sent[len(ack_ms)]) / 1e6)
                if "shm" in reply:
                    got = pixels(reply)
                    frames += 1
                    if len(ack_ms) == count:
                        expected = (*rgb, 255)  # last authored revision
                        assert (reply["w"], reply["h"]) == (320, 120)
                        assert all(all(abs(a - b) <= 2 for a, b in zip(pixel, expected)) for pixel in got), f"last revision lost: {got} != {expected}"
                        return {"scenario": "update-burst", "updates": count, "replies": len(ack_ms),
                                "frames": frames, "errors": 0,
                                "final_pixels_ms": round((at - started) / 1e6, 3),
                                "ack": summary(ack_ms)}
                    helper.send({"op": "html-ack", "id": 1, "frame": reply["frame"]})
        finally:
            helper.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--helper", type=Path, default=ROOT / "apps/Notes/.build/release/stage-helper")
    parser.add_argument("--concurrency", nargs="+", type=int, default=[1, 4, 16])
    parser.add_argument("--requests", type=int, default=48)
    parser.add_argument("--timeout", type=float, default=30)
    parser.add_argument("--updates", type=int, default=0, help="also test a burst of 1..1000 HTML revisions")
    args = parser.parse_args()
    if not args.helper.is_file():
        parser.error("build stage-helper first")
    if not 0 <= args.updates <= 1000:
        parser.error("updates must be 0..1000")
    if not 2 <= args.requests <= 1000 or any(not 1 <= n <= 32 for n in args.concurrency):
        parser.error("requests must be 2..1000, concurrency 1..32")
    for concurrency in args.concurrency:
        print(json.dumps(load(args.helper.resolve(), concurrency, args.requests, args.timeout)), flush=True)
    if args.updates:
        print(json.dumps(update_burst(args.helper.resolve(), args.updates, args.timeout)), flush=True)


if __name__ == "__main__":
    main()
