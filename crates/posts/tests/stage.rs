//! The session folder itself: file layout, journaled rollback, files changed
//! outside notesd, the one-time SQLite migration, and session search.

use std::{fs, path::PathBuf};

use notes_posts::*;
use notes_proto::posts::*;
use rusqlite::Connection;

fn temp_dir() -> PathBuf {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "notes-stage-{}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    dir
}

fn open(dir: &std::path::Path) -> Store {
    Store::open(Connection::open_in_memory().unwrap(), dir.to_path_buf()).unwrap()
}

fn md(text: &str) -> Part {
    Part::Markdown { markdown: text.into(), id: None }
}

fn publish(s: &Store, session: Option<&SessionId>, title: &str, text: &str) -> PostSummary {
    let args = PostNewArgs { session: session.cloned(), title: Some(title.into()), parts: vec![md(text)], ..Default::default() };
    post_new(s, &args, Some("claude"), Some("/repo")).unwrap().0.post
}

fn session_files(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".json"))
        .collect();
    names.sort();
    names
}

#[test]
fn one_named_file_per_session_survives_a_restart() {
    let dir = temp_dir();
    let s = open(&dir);
    let (session, _) = session_new(
        &s,
        &SessionNewArgs { agent: "claude".into(), title: Some("Fix login redirect!".into()), cwd: None },
    )
    .unwrap();
    let post = publish(&s, Some(&session.id), "Plan", "hello");
    let files = session_files(&dir);
    assert_eq!(files.len(), 1);
    let name = &files[0];
    assert!(name.ends_with(&format!("-fix-login-redirect-{}.json", session.id)), "{name}");
    let json: serde_json::Value = serde_json::from_slice(&fs::read(dir.join(name)).unwrap()).unwrap();
    assert_eq!(json["schema"], 1);
    assert_eq!(json["posts"][0]["title"], "Plan");

    // Renaming keeps the file name; a fresh Store reads everything back.
    session_rename(&s, &SessionRenameArgs { id: session.id.clone(), title: "Other".into() }).unwrap();
    assert_eq!(session_files(&dir), files);
    drop(s);
    let s = open(&dir);
    assert_eq!(session_get(&s, &session.id).unwrap().title.as_deref(), Some("Other"));
    let shown = post_show(&s, &PostShowArgs { id: post.id.clone(), version: None }).unwrap();
    assert!(matches!(&shown.parts[..], [Part::Markdown { markdown, .. }] if markdown == "hello"));
}

#[test]
fn rollback_restores_files_and_memory() {
    let dir = temp_dir();
    let s = open(&dir);
    let kept = publish(&s, None, "Kept", "a");
    let before = session_files(&dir);
    let bytes_before = fs::read(dir.join(&before[0])).unwrap();

    s.begin();
    publish(&s, None, "Dropped", "b");
    let (other, _) =
        session_new(&s, &SessionNewArgs { agent: "codex".into(), title: None, cwd: None }).unwrap();
    assert_eq!(session_files(&dir).len(), 2);
    s.rollback();

    assert_eq!(session_files(&dir), before);
    assert_eq!(fs::read(dir.join(&before[0])).unwrap(), bytes_before);
    assert!(session_get(&s, &other.id).is_err(), "rolled-back session is gone from memory");
    let posts = post_list(&s, &PostListArgs::default()).unwrap();
    assert_eq!(posts.iter().map(|p| p.id.clone()).collect::<Vec<_>>(), vec![kept.id]);

    // A committed unit keeps its writes.
    s.begin();
    publish(&s, None, "Committed", "c");
    s.commit();
    assert_eq!(post_list(&s, &PostListArgs::default()).unwrap().len(), 2);
}

#[test]
fn stray_temps_are_removed_and_unreadable_files_left_alone() {
    let dir = temp_dir();
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(".2026-01-01-x-abc.json.notesd-tmp"), b"half a write").unwrap();
    fs::write(dir.join("notes.json"), b"{ not json").unwrap();
    fs::write(dir.join("future.json"), br#"{"schema": 2, "id": "x"}"#).unwrap();
    fs::write(dir.join("readme.txt"), b"ignored").unwrap();
    let s = open(&dir);
    assert!(!dir.join(".2026-01-01-x-abc.json.notesd-tmp").exists());
    let doctor = s.doctor();
    let skipped: Vec<String> = serde_json::from_value(doctor["skipped"].clone()).unwrap();
    assert_eq!(skipped.len(), 2, "{skipped:?}");
    assert!(skipped.iter().any(|s| s.starts_with("notes.json:")));
    assert!(skipped.iter().any(|s| s.starts_with("future.json:") && s.contains("schema")));
    assert!(session_list(&s).unwrap().is_empty());

    // Writing other sessions never touches the unreadable files.
    publish(&s, None, "P", "x");
    assert_eq!(fs::read(dir.join("notes.json")).unwrap(), b"{ not json");
}

#[test]
fn unknown_fields_make_a_file_unreadable() {
    let dir = temp_dir();
    let s = open(&dir);
    publish(&s, None, "P", "x");
    let name = session_files(&dir).remove(0);
    let mut json: serde_json::Value = serde_json::from_slice(&fs::read(dir.join(&name)).unwrap()).unwrap();
    json["surprise"] = serde_json::json!(true);
    fs::write(dir.join(&name), serde_json::to_vec(&json).unwrap()).unwrap();
    drop(s);
    let s = open(&dir);
    assert!(session_list(&s).unwrap().is_empty());
    assert_eq!(s.doctor()["skipped"].as_array().unwrap().len(), 1);
}

#[test]
fn refresh_picks_up_files_added_and_removed_outside() {
    let dir = temp_dir();
    let s = open(&dir);
    let post = publish(&s, None, "P", "x");
    assert!(s.refresh().unwrap().is_empty(), "our own writes aren't outside changes");

    // Copy the session to another folder, delete it here: the store drops it.
    let name = session_files(&dir).remove(0);
    let copy = temp_dir();
    fs::create_dir_all(&copy).unwrap();
    fs::copy(dir.join(&name), copy.join(&name)).unwrap();
    fs::remove_file(dir.join(&name)).unwrap();
    let events = s.refresh().unwrap();
    assert_eq!(events.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(), vec!["session-deleted"]);
    assert!(post_show(&s, &PostShowArgs { id: post.id.clone(), version: None }).is_err());

    // Drop it back in: it's a session again.
    fs::copy(copy.join(&name), dir.join(&name)).unwrap();
    let events = s.refresh().unwrap();
    assert_eq!(events.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(), vec!["session-created"]);
    assert!(post_show(&s, &PostShowArgs { id: post.id, version: None }).is_ok());
}

/// The pre-JSON schema, as the old `migrate` created it.
const LEGACY: &str = "
CREATE TABLE sessions (id TEXT PRIMARY KEY, agent TEXT NOT NULL, title TEXT, cwd TEXT,
    created_at INTEGER NOT NULL, last_active_at INTEGER NOT NULL, agent_seq INTEGER NOT NULL DEFAULT 0);
CREATE TABLE posts (id TEXT PRIMARY KEY, session TEXT NOT NULL, title TEXT NOT NULL,
    parts TEXT NOT NULL, kinds TEXT NOT NULL, version INTEGER NOT NULL,
    created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE post_versions (post TEXT NOT NULL, version INTEGER NOT NULL, title TEXT NOT NULL,
    parts TEXT NOT NULL, at INTEGER NOT NULL, PRIMARY KEY (post, version));
CREATE TABLE asset_refs (asset TEXT NOT NULL, post TEXT NOT NULL, PRIMARY KEY (asset, post));
CREATE TABLE comments (seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE,
    session TEXT NOT NULL, post TEXT, author TEXT NOT NULL, text TEXT NOT NULL,
    created_at INTEGER NOT NULL, anchor TEXT);
CREATE TABLE assets (id TEXT PRIMARY KEY, session TEXT, kind TEXT NOT NULL, content_type TEXT NOT NULL,
    byte_length INTEGER NOT NULL, filename TEXT, data BLOB NOT NULL,
    created_at INTEGER NOT NULL, last_accessed INTEGER NOT NULL);
CREATE TABLE trace_steps (session TEXT NOT NULL, seq INTEGER NOT NULL, kind TEXT, label TEXT NOT NULL,
    detail TEXT, ts TEXT, PRIMARY KEY (session, seq));
INSERT INTO sessions VALUES ('s1', 'claude', 'Old work', '/repo', 100, 300, 1);
INSERT INTO posts VALUES ('p1', 's1', 'Second title', '[{\"kind\":\"markdown\",\"markdown\":\"v2\",\"id\":\"a\"}]', 'markdown', 2, 100, 200);
INSERT INTO post_versions VALUES ('p1', 1, 'First title', '[{\"kind\":\"markdown\",\"markdown\":\"v1\",\"id\":\"a\"}]', 100);
INSERT INTO comments (id, session, post, author, text, created_at, anchor)
    VALUES ('c1', 's1', 'p1', 'user', 'looks good', 250,
            '{\"kind\":\"line-range\",\"part\":0,\"post_version\":2,\"start_line\":1,\"end_line\":1}');
INSERT INTO comments (id, session, post, author, text, created_at, anchor) VALUES ('c2', 's1', NULL, 'user', 'and?', 260, NULL);
INSERT INTO assets VALUES ('2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824', 's1', 'image', 'image/png', 5, 'h.png', X'68656c6c6f', 100, 100);
INSERT INTO trace_steps VALUES ('s1', 0, 'tool', 'ran tests', 'ok', NULL);
INSERT INTO settings VALUES ('theme', 'dark');
";

#[test]
fn migration_moves_every_session_record_and_drops_the_tables() {
    let dir = temp_dir();
    let conn = Connection::open_in_memory().unwrap();
    migrate(&conn).unwrap();
    conn.execute_batch(LEGACY).unwrap();
    let s = Store::open(conn, dir.clone()).unwrap();

    assert!(s.doctor()["migration"].as_str().unwrap().starts_with("moved 1 sessions"));
    let session = session_get(&s, "s1").unwrap();
    assert_eq!(
        (session.agent.as_str(), session.title.as_deref(), session.cwd.as_deref(), session.post_count),
        ("claude", Some("Old work"), Some("/repo"), 1)
    );
    let v2 = post_show(&s, &PostShowArgs { id: "p1".into(), version: None }).unwrap();
    let v1 = post_show(&s, &PostShowArgs { id: "p1".into(), version: Some(1) }).unwrap();
    assert_eq!((v2.title.as_str(), v2.version, v1.title.as_str()), ("Second title", 2, "First title"));
    let comments = comment_list(&s, &CommentListArgs::default()).unwrap();
    assert_eq!(comments.iter().map(|c| (c.seq, c.id.as_str())).collect::<Vec<_>>(), vec![(1, "c1"), (2, "c2")]);
    assert!(matches!(comments[0].anchor, Some(CommentAnchor::LineRange { .. })));
    assert_eq!(trace_list(&s, "s1").unwrap()[0].label, "ran tests");
    let asset = asset_get(&s, "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824").unwrap();
    assert_eq!(asset.info.filename.as_deref(), Some("h.png"));
    // The cursor came along: c1 was seen, c2 is the next feedback.
    let next = next_user_comment(&s, Some("s1"), None, None).unwrap().unwrap();
    assert_eq!(next.id, "c2");

    let tables: Vec<String> = s
        .db
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert_eq!(tables, vec!["render_cache", "settings"]);
    assert_eq!(setting_get(&s, "theme").unwrap().as_deref(), Some("dark"));

    // Reopening with nothing left to move is a no-op.
    drop(s);
    let s = open(&dir);
    assert_eq!(s.doctor()["migration"], "none needed");
    assert_eq!(session_list(&s).unwrap().len(), 1);
}

#[test]
fn search_ranks_names_before_content() {
    let dir = temp_dir();
    let s = open(&dir);
    let (named, _) = session_new(
        &s,
        &SessionNewArgs { agent: "claude".into(), title: Some("Redirect bug".into()), cwd: None },
    )
    .unwrap();
    let (other, _) =
        session_new(&s, &SessionNewArgs { agent: "codex".into(), title: Some("Misc".into()), cwd: None }).unwrap();
    let post = publish(&s, Some(&other.id), "Notes", "first line\nthe REDIRECT loops here");

    let hits = session_search(&s, &SessionSearchArgs { query: "redirect".into(), limit: None }).unwrap();
    assert_eq!(hits.iter().map(|h| h.session.id.clone()).collect::<Vec<_>>(), vec![named.id, other.id]);
    assert_eq!(hits[0].post, None);
    assert_eq!(hits[1].post, Some(post.id));
    assert_eq!(hits[1].text.as_deref(), Some("the REDIRECT loops here"));

    assert!(session_search(&s, &SessionSearchArgs { query: "nothing like it".into(), limit: None }).unwrap().is_empty());
    assert_eq!(
        session_search(&s, &SessionSearchArgs { query: "  ".into(), limit: None }).unwrap_err().code,
        notes_proto::ErrorCode::Usage
    );
}
