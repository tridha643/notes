#!/usr/bin/env python3
"""Make (once) the "stage verify heavy" session of large Mermaid diagrams and print its id:
three real charts from pstack's /statechart corpus (if pstack is installed), then an
80-state statechart, a ~160-node architecture flowchart and a 30-class diagram in the
shapes pstack's /statechart and /architect produce. Like fixture.py, idempotent; it shows
in the user's stage sidebar; `notes session delete <id>` removes it."""
import glob, json, os, subprocess, sys, tempfile

TITLE = "stage verify heavy"
env = {**os.environ, "NOTES_OWNER": "stage-verify-heavy"}
env.pop("NOTES_SESSION", None)


def notes(*args):
    return subprocess.run(["notes", *args], env=env, capture_output=True, text=True, check=True).stdout


def existing():
    for line in notes("sessions", "--json").splitlines():
        s = json.loads(line)
        if s.get("title") == TITLE:
            return s["id"]


if found := existing():
    print(found)
    sys.exit(0)


def statechart(n):
    phases = ["Idle", "Queued", "Running", "Retrying", "Blocked", "Done", "Failed", "Cancelled"]
    lines = ["stateDiagram-v2", "    direction TB", "    [*] --> S0"]
    for i in range(n):
        lines.append(f'    S{i} : {phases[i % len(phases)]} step {i}')
        lines.append(f"    S{i} --> S{(i + 1) % n} : the worker finishes step {i}")
        if i % 3 == 0:
            lines.append(f"    S{i} --> S{(i * 7 + 3) % n} : a retry fires [after {i % 5 + 1} failures]")
        if i % 5 == 0:
            lines.append(f"    S{i} --> S{(i + n // 2) % n} : the user cancels")
    lines.append(f"    S{n - 1} --> [*]")
    return "\n".join(lines) + "\n"


def architecture(groups, per):
    lines = ["flowchart LR"]
    for g in range(groups):
        lines.append(f'    subgraph G{g}["service {g}: crates/svc{g}"]')
        for j in range(per):
            lines.append(f'        G{g}N{j}["svc{g}::module_{j}\\nowns {j % 4 + 1} tables"]')
        for j in range(per - 1):
            lines.append(f"        G{g}N{j} --> G{g}N{j + 1}")
        lines.append("    end")
    for g in range(groups):
        for j in range(0, per, 3):
            lines.append(f"    G{g}N{j} -. calls .-> G{(g + 1) % groups}N{(j * 2) % per}")
    return "\n".join(lines) + "\n"


def classes(n):
    lines = ["classDiagram"]
    for i in range(n):
        lines.append(f"    class Type{i} {{\n        +id: u64\n        +name: String\n        +step_{i}() Result\n    }}")
        if i:
            lines.append(f"    Type{i // 2} <|-- Type{i}")
        if i % 4 == 0 and i:
            lines.append(f"    Type{i} --> Type{(i * 3) % n} : uses")
    return "\n".join(lines) + "\n"


# One chart per corpus name (several pstack versions may be cached), largest first.
charts = {os.path.basename(os.path.dirname(p)): p for p in glob.glob(os.path.expanduser(
    "~/.claude/plugins/cache/pstack-claude/pstack/*/skills/statechart/scripts/test/corpus/*/chart.mmd"))}
corpus = sorted(charts.values(), key=os.path.getsize, reverse=True)[:3]
posts = [(f"pstack /statechart: {os.path.basename(os.path.dirname(p))}", open(p).read()) for p in corpus]
posts += [("Statechart: 80 states", statechart(80)), ("Architecture: 160 nodes", architecture(10, 16)), ("Class diagram: 30 types", classes(30))]
tmp = tempfile.mkdtemp(prefix="svheavy")
session = None
for title, src in posts:
    path = f"{tmp}/d.mmd"
    open(path, "w").write(src)
    args = ["post", "mermaid", path, "--title", title, "--json"]
    if session:
        notes(*args, "--session", session)
    else:
        notes(*args, "--session-title", TITLE)
        session = existing()
print(session)
