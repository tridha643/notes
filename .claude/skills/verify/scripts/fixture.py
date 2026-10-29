#!/usr/bin/env python3
"""Make (once) the "stage verify fixture" session and print its id. Idempotent: an
existing session with that title is reused. Writes through the real notesd, so the
session shows in the user's stage sidebar; `notes session delete <id>` removes it.

Posts, oldest first: 8 Mermaid diagrams (two of them taller than a screen), one
HTML card, then a last post with a Mermaid diagram and markdown (stage opens there).
"""
import json, os, subprocess, sys, tempfile

TITLE = "stage verify fixture"
env = {**os.environ, "NOTES_OWNER": "stage-verify-fixture"}  # its own owner, so its own session
env.pop("NOTES_SESSION", None)


def notes(*args, stdin=None):
    return subprocess.run(["notes", *args], env=env, input=stdin, capture_output=True, text=True, check=True).stdout


def existing():
    for line in notes("sessions", "--json").splitlines():
        s = json.loads(line)
        if s.get("title") == TITLE:
            return s["id"]


if found := existing():
    print(found)
    sys.exit(0)


def flow(i, steps):
    body = "\n".join(f"  s{j}[Step {j} of diagram {i}] --> s{j + 1}[Step {j + 1}]" for j in range(steps))
    return f"flowchart TD\n{body}\n"


tmp = tempfile.mkdtemp(prefix="svfix")
session = None
posts = [(f"Diagram {i}", flow(i, 14 if i in (3, 6) else 4)) for i in range(1, 9)]
for title, src in posts:
    path = f"{tmp}/d.mmd"
    open(path, "w").write(src)
    args = ["post", "mermaid", path, "--title", title, "--json"]
    if session:
        notes(*args, "--session", session)
    else:
        notes(*args, "--session-title", TITLE)
        session = existing()
html = f"{tmp}/p.html"
open(html, "w").write("<body style='margin:0;background:#1e1e2e;color:#cdd6f4;font:14px Helvetica'><div style='padding:12px;height:180px'>static html card</div></body>")
notes("post", "html", html, "--title", "Static page", "--session", session, "--json")
last = f"{tmp}/last.mmd"
open(last, "w").write(flow(9, 3))
md = f"{tmp}/last.md"
open(md, "w").write("The last post: a diagram and some prose.\n")
notes("post", "mermaid", last, "--md", md, "--title", "Last post", "--session", session, "--json")
print(session)
