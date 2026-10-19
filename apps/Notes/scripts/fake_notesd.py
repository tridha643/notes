#!/usr/bin/env python3
"""Stand-in notesd for running the app without the Rust daemon.

    python3 apps/Notes/scripts/fake_notesd.py /tmp/notes.sock [--autosend]
    NOTES_SOCK=/tmp/notes.sock build/Notes.app/Contents/MacOS/Notes

Keeps notes and sideshow records in memory and prints every request line to
stdout. Seeds one agent session ("s1") holding a post of every part kind, a
session trace, and a comment. Signals simulate other clients:

    SIGUSR1  another client edits "welcome.md"
    SIGUSR2  the agent publishes v2 of the code post, adds a post and comments

--autosend makes the seeded html part call sendPrompt() on load, so a run can
check the html bridge end to end from the request log.
"""
import base64, difflib, hashlib, json, os, re, signal, socket, struct, sys, threading, time, zlib

path = sys.argv[1]
autosend = "--autosend" in sys.argv
notes = {
    "welcome.md": "# Welcome\n\nType here. **Bold**, *italic*, `code`.\n\n```\nfenced code\n```\n",
    "work/plan.md": "# Plan\n\n- ship the app\n",
}
versions = {p: 1 for p in notes}
watchers, lock, seq = [], threading.RLock(), [0]
now = int(time.time())

# ---- sideshow state ----------------------------------------------------------
sessions, posts, comments, assets, traces, settings = {}, {}, [], {}, {}, {}
counter = [0]


def next_id(prefix):
    counter[0] += 1
    return f"{prefix}{counter[0]}"


def event(kind, p=None, **ids):
    seq[0] += 1
    body = {"seq": seq[0], "kind": kind}
    if p is not None:
        body.update(path=p, version=versions.get(p))
    body.update({k: v for k, v in ids.items() if v is not None})
    line = json.dumps({"id": 0, "event": body}) + "\n"
    for conn in list(watchers):
        try:
            conn.sendall(line.encode())
        except OSError:
            watchers.remove(conn)


def summary(p):
    return {"path": p, "title": p.rsplit("/", 1)[-1][:-3], "version": versions[p], "updated_at": int(time.time())}


def resolve(ref):
    if ref == "@last":
        return "welcome.md"
    return next((p for p in (ref, ref + ".md") if p in notes), None)


def fail(code, message, details=None):
    raise KeyError((code, message, details))


def touch(sid):
    sessions[sid]["last_active_at"] = int(time.time())
    sessions[sid]["post_count"] = sum(1 for p in posts.values() if p["session"] == sid)
    event("session-updated", id=sid, session=sid)


def post_summary(p):
    cur = p["versions"][-1]
    return {"id": p["id"], "session": p["session"], "title": cur["title"], "kinds": [x["kind"] for x in cur["parts"]],
            "version": len(p["versions"]), "updated_at": cur["at"]}


def post_at(pid, version=None):
    p = posts.get(pid) or fail("not_found", f"no post {pid}")
    v = version or len(p["versions"])
    if not 1 <= v <= len(p["versions"]):
        fail("not_found", f"post {pid} has no version {v}")
    cur = p["versions"][v - 1]
    return {"id": pid, "session": p["session"], "title": cur["title"], "parts": cur["parts"], "version": v,
            "created_at": p["created_at"], "updated_at": cur["at"]}


def write_post(sid, title, parts, pid=None):
    at = int(time.time())
    if pid is None:
        pid = next_id("p")
        posts[pid] = {"id": pid, "session": sid, "created_at": at, "versions": []}
        kind = "post-created"
    else:
        kind = "post-updated"
    posts[pid]["versions"].append({"title": title, "parts": parts, "at": at})
    event(kind, id=pid, session=sid, version=len(posts[pid]["versions"]))
    touch(sid)
    return {"post": post_summary(posts[pid])}


def add_comment(sid, pid, author, text, anchor=None):
    c = {"id": next_id("c"), "seq": counter[0], "session": sid, "author": author, "text": text, "created_at": int(time.time())}
    if pid:
        c["post"] = pid
    if anchor:
        c["anchor"] = anchor
    comments.append(c)
    event("comment-created", id=c["id"], session=sid)
    touch(sid)
    return c


def put_asset(kind, content_type, data, filename=None):
    aid = hashlib.sha256(data).hexdigest()
    assets[aid] = ({"id": aid, "kind": kind, "content_type": content_type, "byte_length": len(data), "created_at": int(time.time()),
                    **({"filename": filename} if filename else {})}, data)
    return assets[aid][0]


# ---- post-render: highlighting, diff hunks, ANSI ---------------------------------
KEYWORDS = set("fn let mut pub use struct enum impl match if else for while return in def class import from as func var "
               "self Self true false nil None const static async await try throw guard case switch final public private".split())
TOKEN = re.compile(r'(?P<comment>//.*|#.*)|(?P<string>"(?:[^"\\]|\\.)*"|\'(?:[^\'\\]|\\.)*\')|(?P<number>\b\d[\d_.]*\b)'
                   r'|(?P<word>[A-Za-z_][A-Za-z0-9_]*)|(?P<punct>[{}()\[\];,.:<>=+\-*/&|!?]+)|(?P<space>\s+)|(?P<other>.)')


def spans(line):
    out = []
    for m in TOKEN.finditer(line):
        kind, text = m.lastgroup, m.group()
        if kind == "word":
            after = line[m.end():m.end() + 1]
            kind = "keyword" if text in KEYWORDS else "type" if text[0].isupper() else "function" if after == "(" else None
        elif kind in ("space", "other"):
            kind = None
        elif kind == "punct":
            kind = "punctuation"
        if out and out[-1].get("class") == kind:
            out[-1]["text"] += text
        else:
            out.append({"text": text, **({"class": kind} if kind else {})})
    return out


def hunk_files(patch):
    files, cur, hunk, old, new = [], None, None, 0, 0
    for line in patch.split("\n"):
        if line.startswith("+++ "):
            name = line[4:].strip()
            cur = {"filename": name[2:] if name.startswith("b/") else name, "added": 0, "removed": 0, "hunks": []}
            files.append(cur)
        elif line.startswith("--- ") or line.startswith("diff ") or line.startswith("index "):
            continue
        elif line.startswith("@@") and cur is not None:
            m = re.match(r"@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@", line)
            old, new = int(m.group(1)), int(m.group(2))
            hunk = {"header": line, "lines": []}
            cur["hunks"].append(hunk)
        elif hunk is not None and line[:1] in ("+", "-", " "):
            body = line[1:]
            if line[0] == "+":
                hunk["lines"].append({"kind": "added", "new_line": new, "spans": spans(body)})
                new += 1
                cur["added"] += 1
            elif line[0] == "-":
                hunk["lines"].append({"kind": "removed", "old_line": old, "spans": spans(body)})
                old += 1
                cur["removed"] += 1
            else:
                hunk["lines"].append({"kind": "context", "old_line": old, "new_line": new, "spans": spans(body)})
                old += 1
                new += 1
    return files


SGR = {30: "black", 31: "red", 32: "green", 33: "yellow", 34: "blue", 35: "magenta", 36: "cyan", 37: "white"}


def ansi_lines(text):
    lines, style = [], []
    for raw in text.rstrip("\n").split("\n"):
        line, pos = [], 0
        for m in re.finditer(r"\x1b\[([0-9;]*)m", raw):
            if m.start() > pos:
                line.append({"text": raw[pos:m.start()], **({"class": " ".join(style)} if style else {})})
            for code in [int(c or 0) for c in m.group(1).split(";")]:
                if code == 0:
                    style = []
                elif code == 1:
                    style.append("bold")
                elif code in SGR:
                    style = [s for s in style if not s.startswith("fg-")] + ["fg-" + SGR[code]]
                elif code in range(90, 98):
                    style = [s for s in style if not s.startswith("fg-")] + ["fg-bright-" + SGR[code - 60]]
            pos = m.end()
        if pos < len(raw) or not line:
            line.append({"text": raw[pos:], **({"class": " ".join(style)} if style else {})})
        lines.append(line)
    return lines


def render_part(part):
    k = part["kind"]
    if k == "code":
        return {"kind": "code", "language": part.get("language"), "line_start": part.get("line_start", 1),
                "lines": [spans(l) for l in part["code"].rstrip("\n").split("\n")]}
    if k == "diff":
        patch = part.get("patch")
        if not patch:
            patch = "".join("".join(difflib.unified_diff(f["before"].splitlines(True), f["after"].splitlines(True),
                                                         "a/" + f["filename"], "b/" + f["filename"])) for f in part.get("files", []))
        return {"kind": "diff", "layout": part.get("layout", "unified"), "files": hunk_files(patch)}
    if k == "terminal":
        return {"kind": "terminal", "lines": ansi_lines(part["text"])}
    return {"kind": "native"}


# ---- seed ------------------------------------------------------------------------
def png(w, h):
    rows = b"".join(b"\x00" + bytes(v for x in range(w) for v in (40 + x * 180 // w, 90 + y * 120 // h, 200, 255)) for y in range(h))
    chunk = lambda t, d: struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d))
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b"")


KITS = [{"id": "issues", "label": "Issues", "summary": "Cards, badges, chips, rollup bars",
         "css": ".badge{display:inline-flex;padding:2px 9px;border-radius:999px;font:500 12px/1.4 var(--font-sans);"
                "background:var(--color-background-secondary);color:var(--color-text-secondary)}"
                ".badge.ok{background:var(--color-background-success);color:var(--color-text-success)}"},
        {"id": "slides", "label": "Slides", "summary": "A stepped deck", "css": ".deck{display:grid}",
         "js": "document.documentElement.dataset.slides='1';"}]


def iso(t):
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(t))


def seed():
    sessions["s1"] = {"id": "s1", "agent": "claude", "title": "Ship the posts view", "cwd": "/tmp/repo",
                      "created_at": now - 3600, "last_active_at": now, "post_count": 0}
    sessions["s2"] = {"id": "s2", "agent": "codex", "title": "Daemon review", "created_at": now - 7200,
                      "last_active_at": now - 5000, "post_count": 0}
    img = put_asset("image", "image/png", png(320, 180), "gradient.png")
    trace_lines = "\n".join(json.dumps(s) for s in [
        {"label": "read posts.rs", "kind": "tool", "detail": "crates/proto/src/posts.rs (447 lines)", "ts": iso(now - 900)},
        {"label": "cargo test -p notes-proto", "kind": "bash", "detail": "test result: ok. 6 passed", "ts": iso(now - 880)},
        "plain string step"])
    trace = put_asset("trace", "application/x-ndjson", trace_lines.encode(), "trace.jsonl")
    sendprompt = "<script>window.addEventListener('load',function(){sendPrompt('ship it from html');});</script>" if autosend else ""
    kinds = [
        ("Why the posts view", [{"kind": "markdown", "markdown": "# Posts in the app\n\nEvery part kind renders **natively**, with `post-render` doing the heavy lifting.\n\n- code, diff, terminal: daemon spans\n- mermaid: pele in JavaScriptCore\n"}]),
        ("Parser", [{"kind": "code", "language": "rust", "title": "posts.rs", "line_start": 120,
                     "code": "pub fn render(post: &Post) -> PostRender {\n    // highlight once per version\n    let parts = post.parts.iter().map(render_part).collect();\n    PostRender { id: post.id.clone(), version: post.version, parts }\n}\n"}]),
        ("The change", [{"kind": "diff", "patch": "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,4 +1,5 @@\n use serde::Serialize;\n-fn old() -> u32 { 1 }\n+fn new() -> u32 { 2 }\n+// added line\n fn same() {}\n fn tail() {}\n"},
                        {"kind": "diff", "layout": "split", "files": [{"filename": "notes.txt", "before": "alpha\nbeta\ngamma\n", "after": "alpha\nBETA\ngamma\ndelta\n"}]}]),
        ("Test run", [{"kind": "terminal", "title": "cargo test", "cols": 80,
                       "text": "\x1b[32m   Compiling\x1b[0m notes-proto v0.1.0\n\x1b[1mrunning 6 tests\x1b[0m\ntest anchors ... \x1b[32mok\x1b[0m\ntest renders ... \x1b[31mFAILED\x1b[0m\n"}]),
        ("Flow", [{"kind": "mermaid", "mermaid": "flowchart LR\n  A[Agent] -->|post-new| B(notesd)\n  B --> C{App}\n  C -->|comment| A"},
                  {"kind": "mermaid", "mermaid": "sequenceDiagram\n  Agent->>notesd: post-new\n  notesd-->>App: post-created\n  App->>notesd: post-render"}]),
        ("Screenshot", [{"kind": "image", "asset_id": img["id"], "alt": "a gradient", "caption": "Figure 1: a gradient"}]),
        ("Payload", [{"kind": "json", "data": {"zeta": 1, "alpha": [1, 2.5, None, True, "x"], "nested": {"user_id": 7, "big": list(range(250))}}}]),
        ("Agent trace", [{"kind": "trace", "title": "Inline steps", "steps": [{"label": "plan", "kind": "think", "detail": "decide on NSTableView", "ts": iso(now - 600)}, {"label": "edit Posts.swift", "kind": "tool"}]},
                         {"kind": "trace", "asset_id": trace["id"]}]),
        ("Status board", [{"kind": "html", "kits": ["issues"], "html": "<h3 style='margin:0 0 8px'>Status</h3><span class='badge ok'>passing</span> "
                           "<button onclick=\"sendPrompt('looks good')\">Send prompt</button> <a href='https://example.com'>link</a>" + sendprompt}]),
    ]
    for title, parts in kinds:
        write_post("s1", title, parts)
    write_post("s2", "Review notes", [{"kind": "markdown", "markdown": "Looks fine."}])
    first = next(iter(posts))
    add_comment("s1", first, "claude", "Ready for review.")
    add_comment("s1", "p3", "user", "Why u32?", {"kind": "line-range", "part": 0, "post_version": 1, "start_line": 2, "end_line": 2, "file": "src/lib.rs"})
    traces["s1"] = [{"label": "Build the posts view", "kind": "prompt", "ts": iso(now - 3000)},
                    {"label": "Reading the proto first.", "kind": "say", "ts": iso(now - 2990)},
                    {"label": "cat posts.rs", "kind": "bash", "detail": "447 lines", "ts": iso(now - 2980)},
                    {"label": "Done: every kind renders.", "kind": "say", "ts": iso(now - 2970)}]
    for p in posts.values():  # spread creation times so the trace interleaves
        p["created_at"] = now - 2900 + int(p["id"][1:]) * 10


# ---- verbs -----------------------------------------------------------------------
def answer(verb, a):
    if verb == "list":
        folders = sorted({p.rsplit("/", 1)[0] for p in notes if "/" in p})
        return [{"folder": f} for f in folders] + [summary(p) for p in sorted(notes)]
    if verb in ("read", "write", "delete"):
        p = resolve(a["note"])
        if p is None:
            fail("not_found", f"no note {a['note']}")
        if verb == "read":
            return {**summary(p), "body": notes[p], "line_count": notes[p].count("\n"), "created_at": 0}
        if verb == "delete":
            del notes[p]
            event("note-deleted", p)
            return {"path": p, "version": versions[p]}
        if a.get("if_version") not in (None, versions[p]) and not a.get("force"):
            fail("version_conflict", f"{p} is at version {versions[p]}", {"current_version": versions[p]})
        notes[p], versions[p] = a["body"], versions[p] + 1
        event("note-updated", p)
        return {"path": p, "version": versions[p]}
    if verb == "new":
        p = a["path"] if a["path"].endswith(".md") else a["path"] + ".md"
        notes[p], versions[p] = a.get("body", ""), 1
        event("note-created", p)
        return {"path": p, "version": 1}
    # sideshow
    if verb == "session-new":
        sid = next_id("s")
        sessions[sid] = {"id": sid, "agent": a["agent"], **({"title": a["title"]} if a.get("title") else {}),
                         "created_at": int(time.time()), "last_active_at": int(time.time()), "post_count": 0}
        event("session-created", id=sid, session=sid)
        return sessions[sid]
    if verb == "session-list":
        return sorted(sessions.values(), key=lambda s: -s["last_active_at"])
    if verb == "session-rename":
        (sessions.get(a["id"]) or fail("not_found", "no session"))["title"] = a["title"]
        event("session-updated", id=a["id"], session=a["id"])
        return None
    if verb == "session-delete":
        sessions.pop(a["id"], None) or fail("not_found", "no session")
        for pid in [p for p, v in posts.items() if v["session"] == a["id"]]:
            del posts[pid]
        event("session-deleted", id=a["id"], session=a["id"])
        return None
    if verb == "post-new":
        sid = a.get("session") or "s1"
        return write_post(sid, a.get("title") or "Untitled", a["parts"])
    if verb == "post-update":
        cur = post_at(a["id"])
        return write_post(cur["session"], a.get("title") or cur["title"], a.get("parts") or cur["parts"], a["id"])
    if verb == "post-part":
        cur = post_at(a["id"])
        parts = list(cur["parts"])
        t = a.get("target")
        idx = next((i for i, p in enumerate(parts) if p.get("id") == t), None) if t and not t.isdigit() else (int(t) if t else None)
        if a["op"] == "add":
            parts.insert(len(parts) if idx is None else idx, a["part"])
        elif a["op"] == "set":
            parts[idx] = a["part"]
        else:
            del parts[idx]
        return write_post(cur["session"], cur["title"], parts, a["id"])
    if verb == "post-list":  # like notesd: ORDER BY updated_at DESC, rowid DESC
        found = [post_summary(p) for p in reversed(list(posts.values())) if a.get("session") in (None, p["session"])]
        return sorted(found, key=lambda s: -s["updated_at"])[: a.get("limit") or None]
    if verb == "post-show":
        return post_at(a["id"], a.get("version"))
    if verb == "post-render":
        cur = post_at(a["id"], a.get("version"))
        return {"id": cur["id"], "version": cur["version"], "parts": [render_part(p) for p in cur["parts"]]}
    if verb == "post-delete":
        p = posts.pop(a["id"], None) or fail("not_found", "no post")
        event("post-deleted", id=a["id"], session=p["session"])
        touch(p["session"])
        return None
    if verb == "comment-new":
        sid = posts[a["post"]]["session"] if a.get("post") in posts else a.get("session") or fail("usage", "post or session required")
        return add_comment(sid, a.get("post"), a.get("author") or "agent", a["text"], a.get("anchor"))
    if verb == "comment-list":
        return [c for c in comments if a.get("session") in (None, c["session"]) and a.get("post") in (None, c.get("post"))
                and c["seq"] > (a.get("after_seq") or 0)]
    if verb == "comment-delete":
        c = next((c for c in comments if c["id"] == a["id"]), None) or fail("not_found", "no comment")
        comments.remove(c)
        event("comment-deleted", id=c["id"], session=c["session"])
        return None
    if verb == "wait":
        return next((c for c in reversed(comments) if c["author"] == "user" and a.get("post") in (None, c.get("post"))), None)
    if verb == "asset-put":
        return put_asset(a["kind"], a["content_type"], base64.b64decode(a["data_base64"]), a.get("filename"))
    if verb == "asset-get":
        info, data = assets.get(a["id"]) or fail("not_found", "asset evicted")
        return {"info": info, "data_base64": base64.b64encode(data).decode()}
    if verb == "trace-set":
        traces[a["session"]] = a["steps"]
        event("trace-updated", session=a["session"])
        return None
    if verb == "trace-list":
        return traces.get(a["id"], [])
    if verb == "kits":
        return KITS
    if verb == "setting-get":
        return settings.get(a["key"])
    if verb == "setting-set":
        settings[a["key"]] = a["value"]
        event("setting-changed", id=a["key"])
        return None
    fail("usage", f"unknown verb {verb}")


def serve(conn):
    for raw in conn.makefile("rb"):
        print(raw.decode().rstrip(), flush=True)
        req = json.loads(raw)
        if req["verb"] == "watch":
            watchers.append(conn)
            continue
        with lock:
            try:
                reply = {"id": req["id"], "ok": True, "result": answer(req["verb"], req.get("args", {}))}
            except KeyError as e:
                code, message, details = e.args[0]
                reply = {"id": req["id"], "ok": False, "error": {"code": code, "message": message, **({"details": details} if details else {})}}
        conn.sendall((json.dumps(reply) + "\n").encode())


def external_edit(*_):
    with lock:
        notes["welcome.md"] += "\nEdited elsewhere.\n"
        versions["welcome.md"] += 1
        event("note-updated", "welcome.md")


def agent_activity(*_):
    with lock:
        cur = post_at("p2")
        code = dict(cur["parts"][0], code=cur["parts"][0]["code"] + "// v%d\n" % (cur["version"] + 1))
        write_post("s1", cur["title"], [code], "p2")
        write_post("s1", "Follow-up", [{"kind": "markdown", "markdown": "A new post arrived over **watch**."}])
        add_comment("s1", "p2", "claude", "Updated the parser.")


seed()
signal.signal(signal.SIGUSR1, external_edit)
signal.signal(signal.SIGUSR2, agent_activity)
if os.path.exists(path):
    os.unlink(path)
server = socket.socket(socket.AF_UNIX)
server.bind(path)
server.listen()
print(f"fake notesd on {path} (pid {os.getpid()})", flush=True)
while True:
    conn, _ = server.accept()
    threading.Thread(target=serve, args=(conn,), daemon=True).start()
