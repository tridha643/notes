#!/usr/bin/env python3
"""Compare native-scroll and normal-diff Stage screens in real Rex.

Run after cargo build && cargo build --release -p stage:
    python3 scripts/stage-scroll-e2e.py REX_SESSION

Only generated fixtures in a temporary HOME are used. Test windows stay in the
background and are closed on exit. No user vault or live notesd is touched.
"""
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time
from html.parser import HTMLParser


class StyledCells(HTMLParser):
    """Compare visible styles, not Rex's choice of HTML span boundaries.

    Foreground/bold/italic on spaces are invisible; backgrounds and decorations
    are not. Rex may omit trailing default blanks, which are normalized too.
    """
    def __init__(self):
        super().__init__()
        self.stack = [{}]
        self.cells = []

    def handle_starttag(self, tag, attrs):
        style = self.stack[-1].copy()
        for key, value in attrs:
            if key == "style":
                for declaration in value.split(";"):
                    if ":" in declaration:
                        key, value = declaration.split(":", 1)
                        style[key.strip()] = value.strip()
        self.stack.append(style)

    def handle_endtag(self, tag):
        if len(self.stack) > 1:
            self.stack.pop()

    def handle_data(self, data):
        for char in data:
            style = {k: v for k, v in self.stack[-1].items() if k in (
                "color", "background-color", "font-weight", "font-style", "text-decoration")}
            if char.isspace():
                for key in ("color", "font-weight", "font-style"):
                    style.pop(key, None)
            self.cells.append((char, sorted(style.items())))


def visible(html):
    parser = StyledCells()
    parser.feed(html)
    lines = [[]]
    for cell in parser.cells:
        if cell[0] == "\n":
            lines.append([])
        else:
            lines[-1].append(cell)
    for line in lines:
        while line and line[-1] == (" ", []):
            line.pop()
    return lines


def run(*args, **kwargs):
    return subprocess.check_output(args, text=True, timeout=10, **kwargs)


def main(session):
    root = Path(__file__).resolve().parent.parent
    base = Path(tempfile.mkdtemp(prefix="stage-scroll-e2e-", dir="/tmp"))
    env = dict(os.environ, HOME=str(base / "home"), NOTES_VAULT=str(base / "vault"),
               NOTES_DB=str(base / "state/notes.db"), NOTES_SOCK=str(base / "s.sock"))
    (base / "home").mkdir()
    (base / "vault").mkdir()
    daemon_log = open(base / "daemon.log", "w")
    daemon = subprocess.Popen([str(root / "target/debug/notesd")], env=env,
                              stdout=subprocess.DEVNULL, stderr=daemon_log)
    blocks = []

    def rex(*args):
        return run("rex", *args)

    def send(text):
        for block in blocks:
            rex("send", "-s", session, "-b", block, text)
        time.sleep(.04)  # allow the 8ms scroll batch and server presentation

    def resize(width, height):
        for block in blocks:
            rex("block", "call", "-s", session, "-b", block, "resize", json.dumps(dict(
                columns=width, rows=height, cell_width_px=23, cell_height_px=51)))
        time.sleep(.1)

    def capture(block):
        return rex("capture", "-s", session, "-b", block, "--format=html")

    def compare(label):
        frames = [capture(block) for block in blocks]
        if visible(frames[0]) != visible(frames[1]):
            for i, frame in enumerate(frames):
                (base / f"{label}-{i}.html").write_text(frame)
            raise AssertionError(f"Visible screen mismatch at {label}; artifacts: {base}")
        print(f"PASS styled screen: {label}")

    try:
        for _ in range(250):
            if (base / "s.sock").exists():
                break
            if daemon.poll() is not None:
                raise RuntimeError(f"notesd exited; inspect {base / 'daemon.log'}")
            time.sleep(.02)
        else:
            raise TimeoutError("isolated notesd did not start")
        fixture = base / "scroll.rs"
        fixture.write_text("\n".join(
            f'// Row {i:03d}: 界界 Unicode and highlighted Rust\n'
            f'let item_{i:03d} = "a long string that wraps onto the next line";'
            for i in range(130)))
        posted = run(str(root / "target/debug/notes"), "post", "code", str(fixture),
                     "--title", "Scroll A/B fixture", env=env)
        stage_session = re.search(r"session ([\w-]+)", posted)[1]
        for label, extra in (("optimized", []), ("reference", ["STAGE_NO_SCROLL_REGION=1"])):
            created = json.loads(rex(
                "window", "new", "-s", session, f"Stage test {label}", "--focus=false",
                "--shell=none", "--json", "--", "/usr/bin/env", f"HOME={base}/home",
                f"NOTES_SOCK={base}/s.sock", "STAGE_HELPER=/nonexistent", *extra,
                str(root / "target/release/stage"), "--session", stage_session))
            blocks.append(created["block_ids"][0])
        resize(49, 40)
        for _ in range(60):
            if all("Row 000" in capture(block) for block in blocks):
                break
            time.sleep(.05)
        else:
            raise TimeoutError("Stage did not render the fixture")
        send("g")
        compare("initial")
        for i in range(12):
            send("\x1b[<65;20;9M")
            compare(f"down-{i}")
        for i in range(6):
            send("\x1b[<64;20;9M")
            compare(f"up-{i}")
        for label, keys in (("burst", "\x1b[<65;20;9M" * 50), ("picker", "s"),
                            ("picker-close", "\x1b"), ("prompt", "c" + "comment " * 20 + "END"),
                            ("prompt-close", "\x1b")):
            send(keys)
            compare(label)
        for width, height in ((39, 12), (49, 19), (92, 19), (120, 40), (49, 40)):
            resize(width, height)
            send("g")
            compare(f"resize-{width}-{height}")
            # Aim inside the feed, not the sidebar on desktop-width panes.
            column = width - 10
            send(f"\x1b[<65;{column};9M" * 5)
            compare(f"resize-scroll-{width}-{height}")
            send(f"\x1b[<65;{column};9M" * 30)
            compare(f"resize-burst-{width}-{height}")
            send(f"\x1b[<64;{column};9M" * 20)
            compare(f"resize-reverse-{width}-{height}")
        send("G")
        compare("bottom")
        send("g")
        compare("top")
        print(f"All 46 screens match. Artifacts: {base}")
    finally:
        for block in blocks:
            try:
                rex("block", "close", "-s", session, block)
            except subprocess.SubprocessError:
                pass
        daemon.terminate()
        try:
            daemon.wait(timeout=5)
        except subprocess.TimeoutExpired:
            daemon.kill()
            daemon.wait()
        daemon_log.close()


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    main(sys.argv[1])
