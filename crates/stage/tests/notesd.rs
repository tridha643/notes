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
    posts::{CommentNewArgs, DiffLayout, Part, PostId, PostNewArgs, PostUpdateArgs, PostWriteResult, SessionId},
    to_line, Event, Request, Response, Verb, PROTOCOL_VERSION,
};
use stage::{
    app::{App, Effect, Msg, StageRequest},
    client::{next_request, StageClient, Watch},
};

fn requests(fx: Vec<Effect>) -> impl Iterator<Item = StageRequest> {
    fx.into_iter().filter_map(|e| match e {
        Effect::Request(r) => Some(r),
        _ => None,
    })
}

/// Answer the app's requests in the request worker's order until none are left; returns them.
fn settle(app: &mut App, client: &mut StageClient, fx: Vec<Effect>) -> Vec<StageRequest> {
    let mut queue: Vec<StageRequest> = requests(fx).collect();
    let mut asked = Vec::new();
    while let Some(r) = next_request(&mut queue) {
        queue.extend(requests(app.update(Msg::Reply(client.perform(&r)))));
        asked.push(r);
    }
    asked
}

/// As `settle`, but stop when only renders are left (the worker runs them last) and return them.
fn settle_plain(app: &mut App, client: &mut StageClient, fx: Vec<Effect>) -> Vec<Effect> {
    let mut queue: Vec<StageRequest> = requests(fx).collect();
    while let Some(r) = next_request(&mut queue) {
        if matches!(r, StageRequest::PostRender(_)) {
            queue.push(r);
            break;
        }
        queue.extend(requests(app.update(Msg::Reply(client.perform(&r)))));
    }
    queue.into_iter().map(Effect::Request).collect()
}

fn feed_text(app: &mut App) -> String {
    app.feed().lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n")
}

fn pump(app: &mut App, client: &mut StageClient, msg: Msg) {
    let fx = app.update(msg);
    settle(app, client, fx);
}

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
            ..Default::default()
        }))
    }
}

fn events(w: &mut Watch, n: usize) -> Vec<Event> {
    (0..n).map(|_| w.next_event().expect("watch ended")).collect()
}

#[test]
fn plain_cards_read_before_their_render_and_replayed_events_change_nothing() {
    let Some(d) = notesd() else { return };
    let patch = "diff --git a/f.rs b/f.rs\n--- a/f.rs\n+++ b/f.rs\n@@ -1 +1 @@\n-old\n+new\n";
    let first: PostWriteResult = d.agent(Verb::PostNew(PostNewArgs {
        title: Some("the diff".into()),
        parts: vec![Part::Diff { patch: Some(patch.into()), files: vec![], layout: DiffLayout::Unified, id: None }],
        ..Default::default()
    }));
    let session = first.post.session.clone();
    d.post(Some(&session), "two");

    let mut watch = Watch::open(&d.sock(), None).unwrap();
    let mut client = StageClient::new(d.sock());
    let mut app = App::new(None);
    let start = app.start();
    let renders = settle_plain(&mut app, &mut client, start);
    assert_eq!(app.open.as_ref(), Some(&session));
    assert_eq!(app.posts.len(), 2);
    let plain = feed_text(&mut app);
    assert!(plain.contains("diff --git a/f.rs") && plain.contains("two"), "both posts read before any render:\n{plain}");
    settle(&mut app, &mut client, renders);
    let styled = feed_text(&mut app);
    assert!(styled.contains("f.rs  +1 −1") && !styled.contains("diff --git"), "the render replaced the raw patch:\n{styled}");

    // A new version of the shown post, then a third post, arrive live.
    let two = app.shown.clone().unwrap();
    let _: PostWriteResult = d.agent(Verb::PostUpdate(PostUpdateArgs { id: two.clone(), title: Some("two!".into()), parts: None }));
    d.post(Some(&session), "three");
    for e in events(&mut watch, 2) {
        pump(&mut app, &mut client, Msg::Event(e));
    }
    assert_eq!(app.posts.len(), 3);
    assert_eq!(app.shown.as_ref().map(|p| p.as_str()), app.posts.last().map(|p| p.id.as_str()));
    let live = feed_text(&mut app);
    assert!(live.contains("two!") && live.contains("three"), "{live}");

    // Replaying every event from the start leaves the feed as it is and loads nothing.
    let mut again = Watch::open(&d.sock(), Some(0)).unwrap();
    let mut asked = Vec::new();
    for e in events(&mut again, 5) {
        let fx = app.update(Msg::Event(e));
        asked.extend(settle(&mut app, &mut client, fx));
    }
    assert_eq!(feed_text(&mut app), live);
    assert!(!asked.iter().any(|r| matches!(r, StageRequest::PostShow(_) | StageRequest::PostRender(_))), "{asked:?}");
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
    settle(&mut app, &mut client, start);
    // The user comments through stage, then stage refreshes comments (as it
    // does on every comment event).
    client
        .comment_new(CommentNewArgs { post: Some(p.id.clone()), session: None, text: "nit".into(), anchor: None, author: Some("user".into()) })
        .unwrap();
    client.comment_list(p.session.clone()).unwrap();
    let again = app.start();
    settle(&mut app, &mut client, again);
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
    settle(&mut app, &mut client, start);
    for c in "/walrus".chars() {
        pump(&mut app, &mut client, Msg::Key(crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char(c))));
    }
    pump(&mut app, &mut client, Msg::Key(crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Enter)));
    assert_eq!(app.open.as_ref(), Some(&a));
}
