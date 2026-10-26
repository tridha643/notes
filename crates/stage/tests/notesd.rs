//! stage against a real notesd (target/debug/notesd) on a temp vault, database,
//! socket and HOME (so ~/Desktop/Stage lands in the temp dir). If notesd isn't
//! built, each test prints SKIP and returns; build it with `cargo build -p notesd`.

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

use notes_proto::{
    posts::{CommentNewArgs, Part, PostId, PostNewArgs, PostUpdateArgs, PostWriteResult, SessionId},
    to_line, Event, Request, Response, Verb, PROTOCOL_VERSION,
};
use stage::{
    app::{App, Msg},
    client::{StageClient, Watch},
    perform, pump,
};

struct Notesd {
    dir: PathBuf,
    child: Child,
}

impl Drop for Notesd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn notesd() -> Option<Notesd> {
    let bin = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/notesd");
    if !bin.exists() {
        eprintln!("SKIP: {} isn't built; run `cargo build -p notesd`", bin.display());
        return None;
    }
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = PathBuf::from(format!("/tmp/st-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("vault")).unwrap();
    fs::create_dir_all(dir.join("home")).unwrap();
    let child = Command::new(bin)
        .env("NOTES_VAULT", dir.join("vault"))
        .env("NOTES_DB", dir.join("state/notes.db"))
        .env("NOTES_SOCK", dir.join("s.sock"))
        .env("HOME", dir.join("home"))
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let d = Notesd { dir, child };
    let start = Instant::now();
    while UnixStream::connect(d.sock()).is_err() {
        assert!(start.elapsed() < Duration::from_secs(30), "notesd never listened");
        thread::sleep(Duration::from_millis(10));
    }
    Some(d)
}

impl Notesd {
    fn sock(&self) -> PathBuf {
        self.dir.join("s.sock")
    }

    /// An agent's request (stage's own client can't write posts, by design).
    fn agent<T: serde::de::DeserializeOwned>(&self, verb: Verb) -> T {
        let mut s = UnixStream::connect(self.sock()).unwrap();
        let req = Request { v: PROTOCOL_VERSION, id: 1, verb, cwd: Some("/repo".into()), agent: Some("claude".into()) };
        s.write_all(to_line(&req).as_bytes()).unwrap();
        let mut line = String::new();
        BufReader::new(s).read_line(&mut line).unwrap();
        match serde_json::from_str(&line).unwrap() {
            Response::Ok { result, .. } => serde_json::from_value(result).unwrap(),
            other => panic!("{other:?}"),
        }
    }

    fn post(&self, session: Option<&SessionId>, md: &str) -> PostWriteResult {
        self.agent(Verb::PostNew(PostNewArgs {
            session: session.cloned(),
            title: Some(md.into()),
            parts: vec![Part::Markdown { markdown: md.into(), id: None }],
        }))
    }
}

fn events(w: &mut Watch, n: usize) -> Vec<Event> {
    (0..n).map(|_| w.next_event().expect("watch ended")).collect()
}

#[test]
fn bootstrap_and_live_updates_build_each_version_once() {
    let Some(d) = notesd() else { return };
    let first = d.post(None, "one");
    let session = first.post.session.clone();
    d.post(Some(&session), "two");

    let mut watch = Watch::open(&d.sock(), None).unwrap();
    let mut client = StageClient::new(d.sock());
    let mut app = App::new(None);
    let start = app.start();
    perform(&mut app, &mut client, start);
    assert_eq!(app.open.as_ref(), Some(&session));
    assert_eq!(app.posts.len(), 2);
    assert_eq!(app.builds, 2, "the feed builds every post once");

    // A new version of the shown post, then a third post: each built once.
    let two = app.shown.clone().unwrap();
    let _: PostWriteResult = d.agent(Verb::PostUpdate(PostUpdateArgs { id: two.clone(), title: Some("two!".into()), parts: None }));
    d.post(Some(&session), "three");
    for e in events(&mut watch, 2) {
        pump(&mut app, &mut client, Msg::Event(e));
    }
    assert_eq!(app.posts.len(), 3);
    assert_eq!(app.shown.as_ref().map(|p| p.as_str()), app.posts.last().map(|p| p.id.as_str()));
    let keys: Vec<_> = app.loaded.keys().cloned().collect();
    assert_eq!(app.builds, keys.len(), "{keys:?}");
    // Replaying the same events changes nothing.
    let builds = app.builds;
    let mut again = Watch::open(&d.sock(), Some(0)).unwrap();
    for e in events(&mut again, 4) {
        pump(&mut app, &mut client, Msg::Event(e));
    }
    assert_eq!(app.builds, builds);
}

#[test]
fn watch_resumes_after_last_seq_without_gaps_or_repeats() {
    let Some(d) = notesd() else { return };
    let mut w = Watch::open(&d.sock(), None).unwrap();
    thread::sleep(Duration::from_millis(50));
    let s = d.post(None, "a").post.session;
    let seen = events(&mut w, 2); // session-created, post-created
    let last = seen.last().unwrap().seq;
    drop(w);
    let b = d.post(Some(&s), "b").post.id;
    let c = d.post(Some(&s), "c").post.id;
    let mut w = Watch::open(&d.sock(), Some(last)).unwrap();
    let got = events(&mut w, 2);
    let ids: Vec<PostId> = got.iter().map(|e| e.id.clone().unwrap().into()).collect();
    assert_eq!(ids, [b, c]);
    assert!(got.iter().all(|e| e.seq > last));
}

#[test]
fn stage_never_consumes_the_agents_feedback() {
    let Some(d) = notesd() else { return };
    let p = d.post(None, "look at this").post;
    let mut client = StageClient::new(d.sock());
    let mut app = App::new(Some(p.session.clone()));
    let start = app.start();
    perform(&mut app, &mut client, start);
    // The user comments through stage, then stage refreshes comments (as it
    // does on every comment event).
    client
        .comment_new(CommentNewArgs { post: Some(p.id.clone()), session: None, text: "nit".into(), anchor: None, author: Some("user".into()) })
        .unwrap();
    client.comment_list(p.session.clone()).unwrap();
    let again = app.start();
    perform(&mut app, &mut client, again);
    // The agent's next write still carries the comment as feedback.
    let next = d.post(Some(&p.session), "fixed");
    assert_eq!(next.feedback.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(), ["nit"]);
}

#[test]
fn search_finds_sessions_by_title_and_content() {
    let Some(d) = notesd() else { return };
    let a = d.post(None, "the walrus plan").post.session;
    let mut client = StageClient::new(d.sock());
    let hits = client
        .session_search(notes_proto::posts::SessionSearchArgs { query: "walrus".into(), limit: None })
        .unwrap();
    assert_eq!(hits.iter().map(|h| &h.session.id).collect::<Vec<_>>(), [&a]);
    // The app's `/` flow opens it.
    let mut app = App::new(None);
    let start = app.start();
    perform(&mut app, &mut client, start);
    for c in "/walrus".chars() {
        pump(&mut app, &mut client, Msg::Key(crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char(c))));
    }
    pump(&mut app, &mut client, Msg::Key(crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Enter)));
    assert_eq!(app.open.as_ref(), Some(&a));
}
