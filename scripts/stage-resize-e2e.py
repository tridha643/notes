#!/usr/bin/env python3
"""Check resize recovery in disposable, background Rex panes.

    cargo build
    python3 scripts/stage-resize-e2e.py REX_SESSION [STAGE_BINARY]

Uses an isolated notesd, HOME, session store, and generated text/PNG fixture.
SIGSTOP/SIGCONT on only the test Stage process makes coalesced SIGWINCH
reproducible: shrink then grow before Stage can observe the intermediate size.
No app selection, global install, shared helper, or shared daemon is used.
"""
import json
import os
from pathlib import Path
import re
import signal
import struct
import subprocess
import sys
import tempfile
import time
import zlib


def run(*args, **kwargs):
    return subprocess.check_output(args, text=True, timeout=10, **kwargs)


def write_png(path):
    """A small RGBA fixture exercises Kitty retransmission without a helper."""
    def chunk(kind, data):
        return (struct.pack(">I", len(data)) + kind + data
                + struct.pack(">I", zlib.crc32(kind + data)))
    pixels = (b"\0" + b"\x89\xb4\xfa\xff" * 100) * 40
    path.write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">2I5B", 100, 40, 8, 6, 0, 0, 0))
                     + chunk(b"IDAT", zlib.compress(pixels)) + chunk(b"IEND", b""))


def main(session, binary=None):
    root = Path(__file__).resolve().parent.parent
    binary = Path(binary or root / "target/debug/stage").resolve()
    base = Path(tempfile.mkdtemp(prefix="stage-resize-e2e-", dir="/tmp"))
    env = dict(os.environ, HOME=str(base / "home"), NOTES_VAULT=str(base / "vault"),
               NOTES_DB=str(base / "state/notes.db"), NOTES_SOCK=str(base / "s.sock"),
               NOTES_STAGE_DIR=str(base / "sessions"), STAGE_HELPER="/nonexistent",
               STAGE_TRACE=str(base / "graphics.log"))
    for name in ("home", "vault", "sessions"):
        (base / name).mkdir()
    daemon_log = open(base / "daemon.log", "w")
    daemon = subprocess.Popen([str(root / "target/debug/notesd")], env=env,
                              stdout=subprocess.DEVNULL, stderr=daemon_log)
    blocks = []

    def rex(*args):
        return run("rex", *args)

    def call(block, method, args=None):
        return json.loads(rex("block", "call", "-s", session, "-b", block,
                              method, json.dumps(args or {})))

    def resize(block, width, height):
        call(block, "resize", dict(columns=width, rows=height,
                                   cell_width_px=14, cell_height_px=30))
        actual = call(block, "size")
        assert (actual["columns"], actual["rows"]) == (width, height), "Rex did not apply background resize"

    def capture(block):
        return rex("capture", "-s", session, "-b", block, "--format=html")

    try:
        for _ in range(250):
            if (base / "s.sock").exists():
                break
            if daemon.poll() is not None:
                raise RuntimeError(f"notesd exited; inspect {base / 'daemon.log'}")
            time.sleep(.02)
        else:
            raise TimeoutError("isolated notesd did not start")
        fixture = base / "fixture.rs"
        fixture.write_text("\n".join(
            f'// Row {i:03d}: resize regression 界界\n'
            f'let item_{i:03d} = "content wrapping across narrow and wide panes";'
            for i in range(80)))
        image = base / "fixture.png"
        write_png(image)
        posted = run(str(root / "target/debug/notes"), "post", "image", str(image),
                     "--code", str(fixture), "--title", "Resize fixture", env=env)
        stage_session = re.search(r"session ([\w-]+)", posted)[1]
        created = json.loads(rex(
            "window", "new", "-s", session, "Stage resize regression", "--focus=false",
            "--shell=none", "--json", "--", "/usr/bin/env",
            *[f"{k}={env[k]}" for k in ("HOME", "NOTES_SOCK", "NOTES_STAGE_DIR", "STAGE_HELPER", "STAGE_TRACE")],
            str(binary), "--session", stage_session))
        block = created["block_ids"][0]
        blocks.append(block)
        resize(block, 80, 24)
        for _ in range(100):
            if "Row 000" in capture(block) and "place " in (base / "graphics.log").read_text():
                break
            time.sleep(.05)
        else:
            raise TimeoutError("Stage did not render both text and PNG")
        process = call(block, "process")["child"]
        assert Path(process["path"]).resolve() == binary, "refusing to signal an unexpected process"
        pid = process["pid"]

        for width, height in ((80, 24), (49, 19), (120, 40)):
            resize(block, width, height)
            time.sleep(.2)
            before = capture(block)
            assert "Row 000" in before
            (base / f"{width}-before.html").write_text(before)
            trace_before = (base / "graphics.log").read_text()
            try:
                os.kill(pid, signal.SIGSTOP)
                for _ in range(100):
                    if "T" in run("ps", "-o", "state=", "-p", str(pid)):
                        break
                    time.sleep(.01)
                else:
                    raise TimeoutError("test Stage process did not stop")
                resize(block, 30, 10)
                resize(block, width, height)
            finally:
                os.kill(pid, signal.SIGCONT)
            for _ in range(60):
                after = capture(block)
                if after == before:
                    break
                time.sleep(.05)
            (base / f"{width}-after.html").write_text(after)
            assert after == before, f"screen not restored at {width}x{height}; artifacts: {base}"
            trace = (base / "graphics.log").read_text()[len(trace_before):]
            assert "clear: sending 1 images again" in trace, "PNG was not retransmitted after the clear"
            assert "place " in trace, "PNG was not placed after the clear"
            print(f"PASS {width}x{height}: coalesced resize restores styled text and retransmits PNG")
        print(f"Artifacts: {base}\nGPU presentation is not checked by terminal captures.")
    finally:
        for block in blocks:
            try:
                rex("block", "close", "-s", session, block)
            except subprocess.SubprocessError as error:
                print(f"Could not close test block {block}: {error}", file=sys.stderr)
        daemon.terminate()
        try:
            daemon.wait(timeout=5)
        except subprocess.TimeoutExpired:
            daemon.kill()
            daemon.wait()
        daemon_log.close()


if __name__ == "__main__":
    if len(sys.argv) not in (2, 3):
        sys.exit(__doc__)
    main(*sys.argv[1:])
