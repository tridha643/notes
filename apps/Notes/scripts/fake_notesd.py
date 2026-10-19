#!/usr/bin/env python3
"""Stand-in notesd for running the app without the Rust daemon.

    python3 apps/Notes/scripts/fake_notesd.py /tmp/notes.sock
    NOTES_SOCK=/tmp/notes.sock build/Notes.app/Contents/MacOS/Notes

Keeps notes in memory, prints every request line to stdout, and on SIGUSR1
changes "welcome.md" as if another client wrote it (sends a watch event).
"""
import json, os, signal, socket, sys, threading, time

path = sys.argv[1]
notes = {
    "welcome.md": "# Welcome\n\nType here. **Bold**, *italic*, `code`.\n\n```\nfenced code\n```\n",
    "work/plan.md": "# Plan\n\n- ship the app\n",
}
versions = {p: 1 for p in notes}
watchers, lock, seq = [], threading.Lock(), [0]


def summary(p):
    return {"path": p, "title": p.rsplit("/", 1)[-1][:-3], "version": versions[p], "updated_at": int(time.time())}


def resolve(ref):
    if ref == "@last":
        return "welcome.md"
    return next((p for p in (ref, ref + ".md") if p in notes), None)


def event(kind, p):
    seq[0] += 1
    line = json.dumps({"id": 0, "event": {"seq": seq[0], "kind": kind, "path": p, "version": versions.get(p)}}) + "\n"
    for conn in list(watchers):
        try:
            conn.sendall(line.encode())
        except OSError:
            watchers.remove(conn)


def answer(verb, a):
    if verb == "list":
        folders = sorted({p.rsplit("/", 1)[0] for p in notes if "/" in p})
        return [{"folder": f} for f in folders] + [summary(p) for p in sorted(notes)]
    if verb in ("read", "write", "delete"):
        p = resolve(a["note"])
        if p is None:
            raise KeyError(("not_found", f"no note {a['note']}", None))
        if verb == "read":
            return {**summary(p), "body": notes[p], "line_count": notes[p].count("\n"), "created_at": 0}
        if verb == "delete":
            del notes[p]
            event("note-deleted", p)
            return {"path": p, "version": versions[p]}
        if a.get("if_version") not in (None, versions[p]) and not a.get("force"):
            raise KeyError(("version_conflict", f"{p} is at version {versions[p]}", {"current_version": versions[p]}))
        notes[p], versions[p] = a["body"], versions[p] + 1
        event("note-updated", p)
        return {"path": p, "version": versions[p]}
    if verb == "new":
        p = a["path"] if a["path"].endswith(".md") else a["path"] + ".md"
        notes[p], versions[p] = a.get("body", ""), 1
        event("note-created", p)
        return {"path": p, "version": 1}
    raise KeyError(("usage", f"unknown verb {verb}", None))


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


signal.signal(signal.SIGUSR1, external_edit)
if os.path.exists(path):
    os.unlink(path)
server = socket.socket(socket.AF_UNIX)
server.bind(path)
server.listen()
print(f"fake notesd on {path} (pid {os.getpid()})", flush=True)
while True:
    conn, _ = server.accept()
    threading.Thread(target=serve, args=(conn,), daemon=True).start()
