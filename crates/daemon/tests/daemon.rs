//! Runs the real `notesd` binary on a temp NOTES_DB and a short /tmp socket
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
        fs::create_dir_all(&dir).unwrap();
        let child = Notesd::spawn(&dir);
        Notesd { dir, child }.ready()
    }

    fn spawn(dir: &std::path::Path) -> Child {
        Self::command(dir).spawn().unwrap()
    }

    fn command(dir: &std::path::Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_notesd"));
        command.env("NOTES_DB", dir.join("state/notes.db"))
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

    /// kill -9, then start a fresh daemon on the same database and socket.
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
fn watch_streams_after_since_filtered_by_kind() {
    let d = Notesd::new();
    let post = d.ok("post-new", json!({ "title": "P", "parts": [md("x")] }));
    d.ok("comment-new", json!({ "post": post["post"]["id"], "text": "hi" }));
    let mut all = d.watch(json!({ "since": 0 }));
    let first = next_event(&mut all);
    assert_eq!((first["seq"].clone(), first["kind"].clone()), (json!(1), json!("session-created")));

    let mut comments = d.watch(json!({ "since": 0, "kinds": ["comment"] }));
    assert_eq!(next_event(&mut comments)["kind"], "comment-created");
    d.ok("post-update", json!({ "id": post["post"]["id"], "title": "Q" }));
    d.ok("comment-new", json!({ "post": post["post"]["id"], "text": "again" }));
    assert_eq!(next_event(&mut comments)["kind"], "comment-created", "post events are filtered out");
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
fn deleting_a_session_purges_trash_past_its_30_days() {
    let d = Notesd::new();
    let old = d.ok("post-new", json!({ "title": "Old", "parts": [md("x")] }))["post"]["session"].clone();
    d.ok("session-delete", json!({ "id": old }));
    let trash = d.stage().join(".trash");
    let file = fs::read_dir(&trash).unwrap().next().unwrap().unwrap().path();
    let name = file.file_name().unwrap().to_string_lossy().into_owned();
    fs::rename(&file, trash.join(format!("0--{}", name.split_once("--").unwrap().1))).unwrap();

    let new = d.ok("session-new", json!({ "agent": "claude", "title": "New" }))["id"].clone();
    d.ok("session-delete", json!({ "id": new }));
    d.err("not_found", "session-restore", json!({ "id": old }));
    d.ok("session-restore", json!({ "id": new }));
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
fn verbs_report_their_documented_error_codes() {
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
