//! Runs the real `notesd` binary on a temp vault, temp NOTES_DB and a short /tmp socket
//! (macOS caps socket paths at 104 bytes), and speaks the line protocol to it.

use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

struct Notesd {
    dir: PathBuf,
    child: Child,
}

impl Notesd {
    fn new() -> Notesd {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = PathBuf::from(format!(
            "/tmp/nd-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("vault")).unwrap();
        let child = Notesd::spawn(&dir);
        Notesd { dir, child }.ready()
    }

    fn spawn(dir: &std::path::Path) -> Child {
        Self::command(dir).spawn().unwrap()
    }

    fn command(dir: &std::path::Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_notesd"));
        command.env("NOTES_VAULT", dir.join("vault"))
            .env("NOTES_DB", dir.join("state/notes.db"))
            .env("NOTES_SOCK", dir.join("s.sock"))
            // Never inherit a real session directory into a test daemon.
            .env_remove("NOTES_STAGE_DIR")
            .env("HOME", dir.join("home"))
            .stderr(Stdio::null());
        command
    }

    fn ready(self) -> Notesd {
        let start = Instant::now();
        while UnixStream::connect(self.sock()).is_err() {
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "notesd never listened"
            );
            thread::sleep(Duration::from_millis(10));
        }
        self.ok("doctor", Value::Null);
        self
    }

    /// kill -9, then start a fresh daemon on the same vault, database and socket.
    fn crash_and_restart(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
        self.restart();
    }

    fn restart(&mut self) {
        self.child = Notesd::spawn(&self.dir);
        let start = Instant::now();
        while UnixStream::connect(self.sock()).is_err() {
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "notesd never came back"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn sock(&self) -> PathBuf {
        self.dir.join("s.sock")
    }

    fn vault(&self) -> PathBuf {
        self.dir.join("vault")
    }

    fn request(verb: &str, args: Value, agent: Option<&str>) -> String {
        let mut r = json!({ "v": 1, "id": 7, "verb": verb });
        if !args.is_null() {
            r["args"] = args;
        }
        if let Some(agent) = agent {
            r["agent"] = json!(agent);
        }
        format!("{r}\n")
    }

    fn call_as(&self, agent: Option<&str>, verb: &str, args: Value) -> Value {
        let mut stream = UnixStream::connect(self.sock()).unwrap();
        stream
            .write_all(Notesd::request(verb, args, agent).as_bytes())
            .unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        let response: Value =
            serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line:?}"));
        assert_eq!(response["id"], 7);
        response
    }

    fn ok(&self, verb: &str, args: Value) -> Value {
        let r = self.call_as(None, verb, args);
        assert_eq!(r["ok"], true, "{verb}: {r}");
        r["result"].clone()
    }

    /// The error object, after checking its code.
    fn err(&self, code: &str, verb: &str, args: Value) -> Value {
        let r = self.call_as(None, verb, args);
        assert_eq!(r["ok"], false, "{verb} should fail: {r}");
        assert_eq!(r["error"]["code"], code, "{verb}: {r}");
        r["error"].clone()
    }

    fn body(&self, note: &str) -> String {
        self.ok("read", json!({ "note": note }))["body"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// Open a watch stream; lines are read with a timeout so a broken watcher fails the test.
    fn watch(&self, args: Value) -> BufReader<UnixStream> {
        let mut stream = UnixStream::connect(self.sock()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
            .write_all(Notesd::request("watch", args, None).as_bytes())
            .unwrap();
        BufReader::new(stream)
    }
}

fn next_event(watch: &mut BufReader<UnixStream>) -> Value {
    let mut line = String::new();
    watch.read_line(&mut line).expect("event within 10 s");
    let r: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(r["id"], 7);
    r["event"].clone()
}

impl Drop for Notesd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn read_verbs_and_refs() {
    let d = Notesd::new();
    d.ok(
        "new",
        json!({ "path": "work/plan", "body": "# Plan\n\n## Todo\n- a\n" }),
    );
    d.ok(
        "new",
        json!({ "path": "home/plan.md", "body": "groceries\n" }),
    );
    d.ok(
        "new",
        json!({ "path": "solo", "body": "one\ntwo\nthree\n" }),
    );

    let list = d.ok("list", json!({ "sort": "title" }));
    assert_eq!(list[0], json!({ "folder": "home" }));
    assert_eq!(list[1], json!({ "folder": "work" }));
    assert_eq!(list[2]["path"], "solo.md");
    assert_eq!(
        d.ok("list", json!({ "folder": "work" }))[0]["path"],
        "work/plan.md"
    );
    let all = d.ok("list", json!({ "recursive": true }));
    assert_eq!(all.as_array().unwrap().len(), 5, "{all}");
    d.err("not_found", "list", json!({ "folder": "nope" }));

    let read = d.ok(
        "read",
        json!({ "note": "solo", "lines": { "start": 2, "end": 3 } }),
    );
    assert_eq!(read["body"], "two\nthree\n");
    assert_eq!(read["line_count"], 3);
    assert_eq!(read["version"], 1);
    assert_eq!(d.body("work/plan.md"), d.body("work/plan"));
    assert_eq!(d.ok("read", json!({ "note": "@last" }))["path"], "solo.md");
    let e = d.err("ambiguous", "read", json!({ "note": "plan" }));
    assert_eq!(
        e["details"]["candidates"],
        json!(["home/plan.md", "work/plan.md"])
    );
    d.err("not_found", "read", json!({ "note": "missing" }));
    d.err(
        "usage",
        "read",
        json!({ "note": "solo", "lines": { "start": 0, "end": 1 } }),
    );

    // Escaping the vault: `..`, absolute paths, symlinks.
    fs::create_dir_all(d.dir.join("outside")).unwrap();
    fs::write(d.dir.join("outside/secret.md"), "secret").unwrap();
    std::os::unix::fs::symlink(d.dir.join("outside"), d.vault().join("link")).unwrap();
    d.err("usage", "read", json!({ "note": "../outside/secret" }));
    d.err("usage", "read", json!({ "note": "/etc/hosts" }));
    d.err("usage", "read", json!({ "note": "link/secret" }));
    d.err("usage", "new", json!({ "path": "link/new", "body": "x" }));
    assert!(!d.dir.join("outside/new.md").exists());

    let found = d.ok("find", json!({ "query": "plan" }));
    let paths: Vec<_> = found
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["path"].as_str().unwrap())
        .collect();
    assert!(
        paths.contains(&"work/plan.md") && paths.contains(&"home/plan.md"),
        "{found}"
    );
    assert_eq!(
        d.ok("find", json!({ "query": "zzzzunfindable999" })),
        json!([])
    );

    let hits = d.ok("search", json!({ "query": "Todo", "context": 1 }));
    assert_eq!(
        hits,
        json!([{ "path": "work/plan.md", "line": 3, "text": "## Todo", "before": [""], "after": ["- a"] }])
    );
    assert_eq!(
        d.ok("search", json!({ "query": "groceries", "folder": "work" })),
        json!([])
    );
    assert_eq!(
        d.ok("search", json!({ "query": "groceries", "folder": "home" }))[0]["path"],
        "home/plan.md"
    );
    assert_eq!(
        d.ok("search", json!({ "query": "todo", "case_sensitive": true })),
        json!([])
    );
    // A typo misses unless fuzzy is asked for.
    assert_eq!(d.ok("search", json!({ "query": "grocereis" })), json!([]));
    assert_eq!(
        d.ok("search", json!({ "query": "grocereis", "fuzzy": true }))[0]["path"],
        "home/plan.md"
    );
    d.err(
        "usage",
        "search",
        json!({ "query": "x", "folder": "../up" }),
    );

    let doctor = d.ok("doctor", Value::Null);
    assert_eq!(doctor["notes"], 3);
    assert_eq!(doctor["protocol"], 1);
    let manifest = d.ok("manifest", Value::Null);
    assert_eq!(manifest["verbs"].as_array().unwrap().len(), 23);

    // Protocol errors.
    let r = d.call_as(None, "no-such-verb", json!({}));
    assert_eq!(r["error"]["code"], "usage");
}

#[test]
fn write_verbs() {
    let d = Notesd::new();
    assert_eq!(
        d.ok(
            "new",
            json!({ "path": "a/b/plan", "body": "# Plan\n## Todo\n- a\n## Done\n" })
        ),
        json!({ "path": "a/b/plan.md", "version": 1 })
    );
    assert!(d.vault().join("a/b/plan.md").is_file());
    d.err("usage", "new", json!({ "path": "a/b/plan" }));

    d.ok(
        "append",
        json!({ "note": "plan", "text": "- b", "under": "Todo" }),
    );
    let e = d.err(
        "ambiguous",
        "edit",
        json!({ "note": "plan", "edits": [{ "old": "- ", "new": "* " }] }),
    );
    assert_eq!(e["message"], "\"- \" matches 2 times (lines 3, 4)");
    assert_eq!(e["hint"], "add surrounding text or pass --all");
    // All-or-nothing: the first edit would apply, the second fails, nothing lands.
    d.err("not_found", "edit", json!({ "note": "plan", "edits": [{ "old": "# Plan", "new": "# P" }, { "old": "nope", "new": "x" }] }));
    assert_eq!(d.body("plan"), "# Plan\n## Todo\n- a\n- b\n## Done\n");
    let v = d.ok("edit", json!({ "note": "plan", "edits": [{ "old": "- ", "new": "* ", "all": true }, { "old": "# Plan", "new": "# P" }] }));
    assert_eq!(v["version"], 3);
    assert_eq!(d.body("plan"), "# P\n## Todo\n* a\n* b\n## Done\n");

    d.ok("prepend", json!({ "note": "plan", "text": "top" }));
    d.ok("append", json!({ "note": "plan", "text": "end" }));
    assert_eq!(
        d.body("plan"),
        "top\n# P\n## Todo\n* a\n* b\n## Done\nend\n"
    );
    d.err(
        "not_found",
        "append",
        json!({ "note": "plan", "text": "x", "under": "Nope" }),
    );

    d.err("usage", "write", json!({ "note": "plan", "body": "x" }));
    let e = d.err(
        "version_conflict",
        "write",
        json!({ "note": "plan", "body": "x", "if_version": 2 }),
    );
    assert_eq!(e["details"], json!({ "current_version": 5 }));
    d.err(
        "version_conflict",
        "append",
        json!({ "note": "plan", "text": "x", "if_version": 1 }),
    );
    assert_eq!(
        d.ok(
            "write",
            json!({ "note": "plan", "body": "whole\n", "if_version": 5 })
        )["version"],
        6
    );
    assert_eq!(
        d.ok(
            "write",
            json!({ "note": "plan", "body": "forced\n", "force": true })
        )["version"],
        7
    );
    // Unchanged content doesn't make a version.
    assert_eq!(
        d.ok(
            "write",
            json!({ "note": "plan", "body": "forced\n", "force": true })
        )["version"],
        7
    );

    // move renames and rewrites [[links]] to the note.
    d.ok(
        "new",
        json!({ "path": "ref", "body": "see [[plan]] and [[a/b/plan|it]]\n" }),
    );
    assert_eq!(
        d.ok("move", json!({ "note": "plan", "to": "c/renamed" }))["path"],
        "c/renamed.md"
    );
    assert_eq!(d.body("ref"), "see [[renamed]] and [[renamed|it]]\n");
    d.ok("mkdir", json!({ "folder": "inbox" }));
    assert_eq!(
        d.ok("move", json!({ "note": "renamed", "to": "inbox" }))["path"],
        "inbox/renamed.md"
    );
    d.err("usage", "move", json!({ "note": "renamed", "to": "ref" }));
    d.err("not_found", "move", json!({ "note": "ghost", "to": "x" }));

    assert_eq!(d.ok("mkdir", json!({ "folder": "e/f" })), json!("e/f/"));
    d.err("usage", "rmdir", json!({ "folder": "e" }));
    d.ok("rmdir", json!({ "folder": "e/f" }));
    d.ok("rmdir", json!({ "folder": "e" }));
    assert!(!d.vault().join("e").exists());
    d.err("not_found", "rmdir", json!({ "folder": "e" }));
    d.err("usage", "mkdir", json!({ "folder": "../x" }));

    // delete → trash → restore keeps history; purge is for good.
    d.ok("delete", json!({ "note": "renamed" }));
    assert!(!d.vault().join("inbox/renamed.md").exists());
    d.err("not_found", "read", json!({ "note": "renamed" }));
    assert_eq!(
        d.ok("restore", json!({ "note": "renamed" })),
        json!({ "path": "inbox/renamed.md", "version": 8 })
    );
    assert_eq!(d.body("renamed"), "forced\n");
    d.err("not_found", "restore", json!({ "note": "renamed" }));
    d.ok("delete", json!({ "note": "ref", "purge": true }));
    d.err("not_found", "restore", json!({ "note": "ref" }));
    d.err("not_found", "delete", json!({ "note": "ref" }));
}

#[test]
fn history_diff_revert() {
    let d = Notesd::new();
    d.ok("new", json!({ "path": "n", "body": "one\n" }));
    d.call_as(
        Some("claude"),
        "append",
        json!({ "note": "n", "text": "two" }),
    );
    let h = d.ok("history", json!({ "note": "n" }));
    assert_eq!(h[0]["version"], 2);
    assert_eq!(h[0]["author"], "claude");
    assert_eq!(h[1]["author"], "human");
    assert_eq!(
        d.ok("history", json!({ "note": "n", "limit": 1 }))
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let diff = d.ok("diff", json!({ "note": "n" }));
    assert_eq!(
        diff["unified"],
        "--- n.md@v1\n+++ n.md@v2\n@@ -1 +1,2 @@\n one\n+two\n"
    );
    assert_eq!(
        (diff["from"].clone(), diff["to"].clone()),
        (json!(1), json!(2))
    );
    d.err("not_found", "diff", json!({ "note": "n", "from": 9 }));

    assert_eq!(
        d.ok("revert", json!({ "note": "n", "to": 1 }))["version"],
        3
    );
    assert_eq!(d.body("n"), "one\n");
    d.err("not_found", "revert", json!({ "note": "n", "to": 42 }));
}

#[test]
fn batch_plain_and_atomic() {
    let d = Notesd::new();
    let out = d.ok(
        "batch",
        json!({ "ops": [
        { "verb": "new", "args": { "path": "a", "body": "x" } },
        { "verb": "read", "args": { "note": "nope" } },
        { "verb": "new", "args": { "path": "b" } },
    ] }),
    );
    assert_eq!(
        out[0],
        json!({ "ok": true, "result": { "path": "a.md", "version": 1 } })
    );
    assert_eq!(out[1]["error"]["code"], "not_found");
    assert_eq!(out[2]["ok"], true);

    // Atomic: the edit and new folder roll back, on disk and in SQLite.
    let out = d.ok(
        "batch",
        json!({ "atomic": true, "ops": [
        { "verb": "append", "args": { "note": "a", "text": "y" } },
        { "verb": "new", "args": { "path": "dir/c", "body": "c" } },
        { "verb": "edit", "args": { "note": "a", "edits": [{ "old": "missing", "new": "z" }] } },
        { "verb": "new", "args": { "path": "never" } },
    ] }),
    );
    assert_eq!(
        out.as_array().unwrap().len(),
        3,
        "stops at the first error: {out}"
    );
    assert_eq!(out[2]["error"]["code"], "not_found");
    assert_eq!(d.body("a"), "x");
    assert_eq!(d.ok("read", json!({ "note": "a" }))["version"], 1);
    assert!(!d.vault().join("dir").exists());
    d.err("not_found", "read", json!({ "note": "dir/c" }));
    assert_eq!(
        d.ok("history", json!({ "note": "a" }))
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let out = d.ok(
        "batch",
        json!({ "atomic": true, "ops": [
        { "verb": "append", "args": { "note": "a", "text": "y" } },
        { "verb": "new", "args": { "path": "dir/c", "body": "c" } },
    ] }),
    );
    assert_eq!(out[1]["result"]["path"], "dir/c.md");
    assert_eq!(d.body("a"), "x\ny\n");
    let nested = d.ok(
        "batch",
        json!({ "ops": [{ "verb": "batch", "args": { "ops": [] } }] }),
    );
    assert_eq!(nested[0]["error"]["code"], "usage");
}

#[test]
fn watch_streams_after_since_filtered_by_kind() {
    let d = Notesd::new();
    d.ok("mkdir", json!({ "folder": "f" }));
    d.ok("new", json!({ "path": "f/a" }));
    let mut all = d.watch(json!({ "since": 0 }));
    let first = next_event(&mut all);
    assert_eq!(
        (
            first["seq"].clone(),
            first["kind"].clone(),
            first["path"].clone()
        ),
        (json!(1), json!("folder-created"), json!("f"))
    );
    assert_eq!(next_event(&mut all)["kind"], "note-created");

    let mut notes = d.watch(json!({ "since": 1, "kinds": ["note"] }));
    assert_eq!(next_event(&mut notes)["path"], "f/a.md");
    d.ok("append", json!({ "note": "a", "text": "x" }));
    d.ok("mkdir", json!({ "folder": "g" }));
    d.ok("delete", json!({ "note": "a" }));
    let updated = next_event(&mut notes);
    assert_eq!(
        (updated["kind"].clone(), updated["version"].clone()),
        (json!("note-updated"), json!(2))
    );
    assert_eq!(
        next_event(&mut notes)["kind"],
        "note-deleted",
        "folder events are filtered out"
    );
    assert_eq!(next_event(&mut all)["kind"], "note-updated");
}

#[test]
fn concurrent_edits_to_different_parts_both_land() {
    let d = Notesd::new();
    let body: String = (0..25).map(|i| format!("[a{i:02}] [b{i:02}]\n")).collect();
    d.ok("new", json!({ "path": "shared", "body": body }));
    thread::scope(|s| {
        for side in ["a", "b"] {
            let d = &d;
            s.spawn(move || {
                for i in 0..25 {
                    let old = format!("[{side}{i:02}]");
                    let r = d.call_as(Some(side), "edit", json!({ "note": "shared", "edits": [{ "old": old, "new": old.to_uppercase() }] }));
                    assert_eq!(r["ok"], true, "{r}");
                }
            });
        }
    });
    let expected: String = (0..25).map(|i| format!("[A{i:02}] [B{i:02}]\n")).collect();
    assert_eq!(d.body("shared"), expected);
    assert_eq!(d.ok("read", json!({ "note": "shared" }))["version"], 51);
}

/// Latest (version, author, body) straight from SQLite: `read`/`history` would sync the note
/// themselves and hide a broken watcher.
fn recorded(d: &Notesd, path: &str) -> Option<(u64, String, String)> {
    let db = rusqlite::Connection::open(d.dir.join("state/notes.db")).unwrap();
    db.query_row(
        "SELECT h.version, h.author, h.body FROM history h JOIN notes n ON n.id = h.note_id
         WHERE n.path = ?1 ORDER BY h.version DESC LIMIT 1",
        [path],
        |r| Ok((r.get::<_, i64>(0)? as u64, r.get(1)?, r.get(2)?)),
    )
    .ok()
}

/// Read events for `path` until SQLite holds `body` for it; returns the events seen.
fn until_recorded(
    d: &Notesd,
    watch: &mut BufReader<UnixStream>,
    path: &str,
    body: &str,
) -> Vec<Value> {
    let mut seen = Vec::new();
    loop {
        let event = next_event(watch);
        if event["path"] != path {
            continue;
        }
        seen.push(event.clone());
        // The event that carries the recorded version: the watcher saw the final content.
        if let Some((version, _, b)) = recorded(d, path)
            && b == body
            && event["version"] == version
        {
            break;
        }
    }
    seen
}

#[test]
fn outside_edits_become_external_versions_and_are_searchable() {
    let d = Notesd::new();
    d.ok("new", json!({ "path": "w/ext", "body": "before\n" }));
    // Events so far: folder-created (1), note-created (2).
    let mut watch = d.watch(json!({ "since": 2, "kinds": ["note"] }));

    // Editor-style save: write elsewhere, rename over the note. Exactly one new version.
    fs::write(d.dir.join("save.tmp"), "after sentinelqx42\n").unwrap();
    fs::rename(d.dir.join("save.tmp"), d.vault().join("w/ext.md")).unwrap();
    let seen = until_recorded(&d, &mut watch, "w/ext.md", "after sentinelqx42\n");
    assert_eq!(seen.last().unwrap()["kind"], "note-updated");
    assert_eq!(recorded(&d, "w/ext.md").unwrap().0, 2);
    assert_eq!(recorded(&d, "w/ext.md").unwrap().1, "external");
    assert_eq!(
        d.ok("search", json!({ "query": "sentinelqx42" }))[0]["path"],
        "w/ext.md"
    );

    // In-place write (truncate, then write): may record an intermediate version, but the
    // final content always lands, authored external.
    fs::write(d.vault().join("w/ext.md"), "inplace sentinelqy43\n").unwrap();
    until_recorded(&d, &mut watch, "w/ext.md", "inplace sentinelqy43\n");
    let h = d.ok("history", json!({ "note": "ext" }));
    assert!(
        h.as_array()
            .unwrap()
            .iter()
            .all(|e| e["version"] == 1 || e["author"] == "external"),
        "{h}"
    );
    assert_eq!(
        d.ok("search", json!({ "query": "sentinelqy43" }))[0]["path"],
        "w/ext.md"
    );
    assert_eq!(
        d.ok("search", json!({ "query": "sentinelqx42" })),
        json!([])
    );

    // A new file and a delete made outside the daemon.
    fs::write(d.vault().join("w/born.md"), "newbornzz7\n").unwrap();
    let seen = until_recorded(&d, &mut watch, "w/born.md", "newbornzz7\n");
    assert_eq!(seen[0]["kind"], "note-created");
    assert_eq!(
        d.ok("search", json!({ "query": "newbornzz7" }))[0]["path"],
        "w/born.md"
    );
    assert_eq!(
        d.ok("find", json!({ "query": "born" }))[0]["path"],
        "w/born.md"
    );
    fs::remove_file(d.vault().join("w/born.md")).unwrap();
    while next_event(&mut watch)["kind"] != "note-deleted" {}
    assert_eq!(d.ok("search", json!({ "query": "newbornzz7" })), json!([]));
    assert_eq!(
        d.ok("restore", json!({ "note": "born" }))["path"],
        "w/born.md"
    );
}

#[test]
fn kill_9_during_large_write_never_tears_the_note() {
    let mut d = Notesd::new();
    let old = "o".repeat(40 << 20);
    d.ok("new", json!({ "path": "big", "body": old }));
    let tmp = d.vault().join(".big.md.notesd-tmp");
    // None: kill the moment the temp file appears, i.e. mid-write before the rename.
    for (round, delay_ms) in [None, Some(0u64), Some(15), Some(40), Some(80), Some(150)]
        .into_iter()
        .enumerate()
    {
        let current = d.body("big");
        let new = format!("{round}").repeat(40 << 20);
        let mut stream = UnixStream::connect(d.sock()).unwrap();
        let line = Notesd::request(
            "write",
            json!({ "note": "big", "body": new, "force": true }),
            None,
        );
        let writer = thread::spawn(move || {
            let _ = stream.write_all(line.as_bytes());
        });
        match delay_ms {
            Some(ms) => thread::sleep(Duration::from_millis(ms)),
            None => {
                let start = Instant::now();
                while !tmp.exists() {
                    assert!(
                        start.elapsed() < Duration::from_secs(60),
                        "write never started"
                    );
                    thread::sleep(Duration::from_micros(200));
                }
            }
        }
        d.child.kill().unwrap();
        d.child.wait().unwrap();
        let torn_tmp = tmp.exists();
        d.restart();
        let _ = writer.join();

        let on_disk = fs::read_to_string(d.vault().join("big.md")).unwrap();
        assert!(
            on_disk == current || on_disk == new,
            "round {round}: torn note ({} bytes)",
            on_disk.len()
        );
        eprintln!(
            "kill {delay_ms:?}: {} body survived, partial temp file left: {torn_tmp}",
            if on_disk == new { "new" } else { "old" }
        );
        let read = d.ok("read", json!({ "note": "big" }));
        assert_eq!(read["body"].as_str().unwrap(), on_disk);
        let leftovers: Vec<_> = fs::read_dir(d.vault())
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(
            leftovers,
            ["big.md"],
            "temp files are cleaned up on restart"
        );
    }
    assert_eq!(d.ok("doctor", Value::Null)["notes"], 1);
}

#[test]
fn trash_older_than_30_days_is_purged_on_startup() {
    let mut d = Notesd::new();
    d.ok("new", json!({ "path": "old" }));
    d.ok("new", json!({ "path": "recent" }));
    d.ok("delete", json!({ "note": "old" }));
    d.ok("delete", json!({ "note": "recent" }));
    let db = rusqlite::Connection::open(d.dir.join("state/notes.db")).unwrap();
    db.execute(
        "UPDATE trash SET deleted_at = deleted_at - 31 * 86400 WHERE path = 'old.md'",
        [],
    )
    .unwrap();
    drop(db);
    d.crash_and_restart();
    d.err("not_found", "restore", json!({ "note": "old" }));
    d.ok("restore", json!({ "note": "recent" }));
}

impl Notesd {
    fn stage(&self) -> PathBuf {
        self.dir.join("home/Desktop/Stage")
    }

    fn session_files(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.stage())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".json"))
            .collect();
        names.sort();
        names
    }
}

fn md(text: &str) -> Value {
    json!({ "kind": "markdown", "markdown": text })
}

#[test]
fn sessions_live_in_the_desktop_folder_and_doctor_reports_it() {
    let d = Notesd::new();
    let post = d.ok("post-new", json!({ "title": "Plan", "parts": [md("hi")] }));
    let session = post["post"]["session"].as_str().unwrap();
    let files = d.session_files();
    assert_eq!(files.len(), 1);
    assert!(files[0].ends_with(&format!("-{session}.json")), "{files:?}");
    let doctor = d.ok("doctor", Value::Null);
    assert_eq!(doctor["stage"]["sessions"], 1);
    assert_eq!(doctor["stage"]["path"], d.stage().to_string_lossy().as_ref());
    assert_eq!(doctor["stage"]["skipped"], json!([]));

    // Survives a crash and restart.
    let mut d = d;
    d.crash_and_restart();
    let shown = d.ok("post-show", json!({ "id": post["post"]["id"] }));
    assert_eq!(shown["parts"][0]["markdown"], "hi");
}

#[test]
fn configured_stage_directory_works_without_desktop_and_survives_restart() {
    let mut d = Notesd::new();
    d.child.kill().unwrap();
    d.child.wait().unwrap();
    // A regular file makes Desktop/Stage impossible to open. The override must
    // avoid that path altogether, rather than trying it and falling back.
    fs::remove_dir_all(d.dir.join("home/Desktop")).unwrap();
    fs::write(d.dir.join("home/Desktop"), "leave the original Desktop untouched").unwrap();
    let stage = d.dir.join("Application Support/Stage/sessions");
    d.child = Notesd::command(&d.dir).env("NOTES_STAGE_DIR", &stage).spawn().unwrap();
    let mut d = d.ready();
    let post = d.ok("post-new", json!({ "title": "Remote host", "parts": [md("persisted")] }));
    let doctor = d.ok("doctor", Value::Null);
    assert_eq!(doctor["stage"]["path"], stage.to_string_lossy().as_ref());
    assert_eq!(doctor["stage"]["sessions"], 1);
    assert!(fs::read_dir(&stage).unwrap().any(|entry| entry.unwrap().path().extension().is_some_and(|ext| ext == "json")));
    d.child.kill().unwrap();
    d.child.wait().unwrap();
    d.child = Notesd::command(&d.dir).env("NOTES_STAGE_DIR", &stage).spawn().unwrap();
    let d = d.ready();
    assert_eq!(d.ok("post-show", json!({ "id": post["post"]["id"] }))["parts"][0]["markdown"], "persisted");
    assert_eq!(fs::read_to_string(d.dir.join("home/Desktop")).unwrap(), "leave the original Desktop untouched");
}

#[test]
fn stage_reads_answer_the_same_alone_and_in_a_batch() {
    let d = Notesd::new();
    let code = json!({ "kind": "code", "language": "rust", "code": "fn main() {}\n" });
    let post = d.ok("post-new", json!({ "title": "T", "parts": [md("hi"), code] }));
    let (id, session) = (&post["post"]["id"], &post["post"]["session"]);
    let reads = [
        ("post-list", json!({ "session": session })),
        ("post-show", json!({ "id": id })),
        ("post-render", json!({ "id": id })),
    ];
    let ops: Vec<Value> = reads.iter().map(|(verb, args)| json!({ "verb": verb, "args": args })).collect();
    let batched = d.ok("batch", json!({ "ops": ops }));
    for (i, (verb, args)) in reads.iter().enumerate() {
        assert_eq!(d.ok(verb, args.clone()), batched[i]["result"], "{verb}");
    }
    assert_eq!(d.ok("post-render", json!({ "id": id }))["parts"][1]["kind"], "code");
}

#[test]
fn atomic_batch_rolls_back_session_files() {
    let d = Notesd::new();
    d.ok("post-new", json!({ "title": "Kept", "parts": [md("a")] }));
    let before = d.session_files();
    let bytes = fs::read(d.stage().join(&before[0])).unwrap();
    let outcomes = d.ok(
        "batch",
        json!({ "atomic": true, "ops": [
            { "verb": "post-new", "args": { "title": "Dropped", "parts": [md("b")] } },
            { "verb": "session-new", "args": { "agent": "codex" } },
            { "verb": "post-show", "args": { "id": "missing" } },
        ]}),
    );
    assert_eq!(outcomes.as_array().unwrap().len(), 3);
    assert_eq!(outcomes[2]["error"]["code"], "not_found");
    assert_eq!(d.session_files(), before);
    assert_eq!(fs::read(d.stage().join(&before[0])).unwrap(), bytes);
    let posts = d.ok("post-list", json!({}));
    assert_eq!(posts.as_array().unwrap().len(), 1, "{posts}");
}

#[test]
fn session_files_removed_or_added_outside_become_events() {
    let d = Notesd::new();
    let post = d.ok("post-new", json!({ "title": "P", "parts": [md("x")] }));
    let session = post["post"]["session"].clone();
    let name = d.session_files().remove(0);
    let saved = fs::read(d.stage().join(&name)).unwrap();
    // Start from doctor's seq: a watch with no `since` begins wherever notesd is when it
    // registers, which can be after the delete below.
    let since = d.ok("doctor", Value::Null)["seq"].clone();
    let mut watch = d.watch(json!({ "since": since, "kinds": ["session"] }));

    fs::remove_file(d.stage().join(&name)).unwrap();
    let e = next_event(&mut watch);
    assert_eq!((e["kind"].as_str(), &e["id"]), (Some("session-deleted"), &session));
    d.err("not_found", "post-show", json!({ "id": post["post"]["id"] }));

    fs::write(d.stage().join(&name), &saved).unwrap();
    let e = next_event(&mut watch);
    assert_eq!((e["kind"].as_str(), &e["id"]), (Some("session-created"), &session));
    d.ok("post-show", json!({ "id": post["post"]["id"] }));
}

#[test]
fn session_delete_trashes_and_restore_brings_it_back_idempotently() {
    let d = Notesd::new();
    let post = d.ok("post-new", json!({ "title": "P", "parts": [md("x")] }));
    let (session, post_id) = (post["post"]["session"].clone(), post["post"]["id"].clone());
    let since = d.ok("doctor", Value::Null)["seq"].clone();
    let mut watch = d.watch(json!({ "since": since, "kinds": ["session"] }));

    let r = d.ok("session-delete", json!({ "id": session }));
    assert_eq!((&r["trashed"], &r["already"]), (&json!(true), &json!(false)), "{r}");
    assert_eq!(next_event(&mut watch)["kind"], "session-deleted");
    assert!(d.session_files().is_empty());
    d.err("not_found", "post-show", json!({ "id": post_id }));
    let r = d.ok("session-delete", json!({ "id": session }));
    assert_eq!((&r["trashed"], &r["already"]), (&json!(true), &json!(true)), "{r}");

    let r = d.ok("session-restore", json!({ "id": session }));
    assert_eq!((&r["restored"], &r["already"]), (&json!(true), &json!(false)), "{r}");
    assert_eq!(next_event(&mut watch)["kind"], "session-restored");
    d.ok("post-show", json!({ "id": post_id }));
    assert_eq!(d.ok("session-restore", json!({ "id": session }))["already"], true);

    let r = d.ok("session-delete", json!({ "id": session, "purge": true }));
    assert_eq!((&r["purged"], &r["trashed"]), (&json!(true), &json!(false)), "{r}");
    d.err("not_found", "session-restore", json!({ "id": session }));
    d.err("not_found", "session-delete", json!({ "id": session, "purge": true }));
}

#[test]
fn session_search_finds_titles_then_content() {
    let d = Notesd::new();
    let named = d.ok("session-new", json!({ "agent": "claude", "title": "Redirect bug" }));
    let other = d.ok("session-new", json!({ "agent": "codex", "title": "Misc" }));
    d.ok(
        "post-new",
        json!({ "session": other["id"], "title": "Notes", "parts": [md("the redirect loops")] }),
    );
    let hits = d.ok("session-search", json!({ "query": "redirect" }));
    let ids: Vec<&Value> = hits.as_array().unwrap().iter().map(|h| &h["session"]["id"]).collect();
    assert_eq!(ids, vec![&named["id"], &other["id"]]);
    assert_eq!(hits[1]["text"], "the redirect loops");
    d.err("usage", "session-search", json!({ "query": "" }));
}

#[test]
fn unknown_fields_are_rejected_with_their_name() {
    let d = Notesd::new();
    let e = d.err("usage", "post-list", json!({ "sesion": "typo" }));
    assert!(e["message"].as_str().unwrap().contains("sesion"), "{e}");
    let e = d.err(
        "usage",
        "post-new",
        json!({ "parts": [{ "kind": "markdown", "markdown": "x", "colour": "red" }] }),
    );
    assert!(e["message"].as_str().unwrap().contains("colour"), "{e}");

    // The envelope is checked too.
    let mut stream = UnixStream::connect(d.sock()).unwrap();
    stream
        .write_all(b"{\"v\":1,\"id\":7,\"verb\":\"session-list\",\"extra\":1}\n")
        .unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    let r: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(r["error"]["code"], "usage", "{r}");
    assert!(r["error"]["message"].as_str().unwrap().contains("extra"), "{r}");
}

#[test]
fn sideshow_verbs_report_their_documented_error_codes() {
    let d = Notesd::new();
    for (verb, args) in [
        ("post-show", json!({ "id": "nope" })),
        ("post-render", json!({ "id": "nope" })),
        ("post-update", json!({ "id": "nope", "title": "t" })),
        ("post-delete", json!({ "id": "nope" })),
        ("session-rename", json!({ "id": "nope", "title": "t" })),
        ("session-delete", json!({ "id": "nope" })),
        ("session-delete", json!({ "id": "nope", "purge": true })),
        ("session-restore", json!({ "id": "nope" })),
        ("comment-delete", json!({ "id": "nope" })),
        ("asset-get", json!({ "id": "nope" })),
        ("trace-list", json!({ "id": "nope" })),
        ("post-list", json!({ "session": "nope" })),
    ] {
        d.err("not_found", verb, args);
    }
    for (verb, args) in [
        ("post-new", json!({ "parts": [] })),
        ("comment-new", json!({ "text": "x" })),
        ("comment-new", json!({ "session": "s", "text": " " })),
        ("session-search", json!({ "query": " " })),
        ("post-new", json!({})),
    ] {
        d.err("usage", verb, args);
    }
}
