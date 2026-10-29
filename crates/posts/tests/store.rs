use std::{
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use base64::{engine::general_purpose::STANDARD, Engine};
use notes_posts::*;
use notes_proto::{posts::*, ErrorCode};
use rusqlite::Connection;

/// A store over an in-memory database and a fresh temp session folder.
fn db() -> Store {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "notes-posts-{}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let conn = Connection::open_in_memory().unwrap();
    migrate(&conn).unwrap(); // idempotent: Store::open runs it again
    Store::open(conn, dir).unwrap()
}

fn md(text: &str) -> Part {
    Part::Markdown {
        markdown: text.into(),
        id: None,
    }
}

fn code(text: &str, lang: &str) -> Part {
    Part::Code {
        code: text.into(),
        language: Some(lang.into()),
        title: None,
        line_start: None,
        id: None,
    }
}

fn new_post(conn: &Store, session: Option<&str>, parts: Vec<Part>) -> PostWriteResult {
    let args = PostNewArgs {
        session: session.map(Into::into),
        title: Some("T".into()),
        parts,
        ..Default::default()
    };
    post_new(conn, &args, Some("claude"), Some("/repo"))
        .unwrap()
        .0
}

fn delete(id: &str, purge: bool) -> SessionDeleteArgs {
    SessionDeleteArgs { id: id.into(), purge }
}

fn code_of(result: Result<impl std::fmt::Debug>) -> ErrorCode {
    result.expect_err("expected an error").code
}

fn ids(conn: &Store, post: &str) -> Vec<String> {
    let p = post_show(
        conn,
        &PostShowArgs {
            id: post.into(),
            version: None,
        },
    )
    .unwrap();
    p.parts
        .iter()
        .map(|p| {
            serde_json::to_value(p).unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

fn comment(conn: &Store, post: &str, author: &str, text: &str) -> Comment {
    let args = CommentNewArgs {
        post: Some(post.into()),
        session: None,
        text: text.into(),
        anchor: None,
        author: Some(author.into()),
    };
    comment_new(conn, &args, Some("claude")).unwrap().0
}

fn put(conn: &Store, bytes: &[u8], filename: &str) -> AssetInfo {
    let args = AssetPutArgs {
        session: None,
        kind: AssetKind::Image,
        content_type: "image/png".into(),
        filename: Some(filename.into()),
        data_base64: STANDARD.encode(bytes),
    };
    asset_put(conn, &args).unwrap()
}

#[test]
fn sessions_crud_with_counts_and_events() {
    let conn = db();
    let (s, ev) = session_new(
        &conn,
        &SessionNewArgs {
            agent: "user".into(),
            title: Some("  ".into()),
            cwd: None,
        },
    )
    .unwrap();
    assert_eq!(
        (s.agent.as_str(), s.title.as_deref()),
        ("agent", None),
        "`user` is reserved; blank titles drop"
    );
    assert_eq!((ev[0].kind.as_str(), ev[0].seq), ("session-created", 0));
    new_post(&conn, Some(&s.id), vec![md("a")]);
    new_post(&conn, Some(&s.id), vec![md("b")]);
    let (renamed, ev) = session_rename(
        &conn,
        &SessionRenameArgs {
            id: s.id.clone(),
            title: "Plan".into(),
        },
    )
    .unwrap();
    assert_eq!(
        (
            renamed.title.as_deref(),
            renamed.post_count,
            ev[0].kind.as_str()
        ),
        (Some("Plan"), 2, "session-updated")
    );
    assert_eq!(session_list(&conn).unwrap()[0].id, s.id);
    assert_eq!(
        code_of(session_rename(
            &conn,
            &SessionRenameArgs {
                id: "nope".into(),
                title: "x".into()
            }
        )),
        ErrorCode::NotFound
    );
    let (_, ev) = session_delete(&conn, &delete(&s.id, false)).unwrap();
    assert_eq!(ev[0].kind, "session-deleted");
    assert_eq!(code_of(session_get(&conn, &s.id)), ErrorCode::NotFound);
    assert_eq!(post_list(&conn, &PostListArgs::default()).unwrap().len(), 0);
    assert_eq!(code_of(session_delete(&conn, &delete("nope", false))), ErrorCode::NotFound);
}

#[test]
fn each_conversation_gets_its_own_session_in_the_same_folder() {
    let conn = db();
    let post = |owner: &str, session_title: Option<&str>| {
        let args = PostNewArgs {
            parts: vec![md("x")],
            owner: Some(owner.into()),
            session_title: session_title.map(Into::into),
            ..Default::default()
        };
        post_new(&conn, &args, Some("claude"), Some("/repo")).unwrap().0.post.session
    };
    let a = post("pid1:100", Some("Auth refactor"));
    assert_eq!(post("pid1:100", Some("ignored later")), a, "same conversation, same thread");
    let b = post("pid2:200", None);
    assert_ne!(b, a, "another conversation in the same folder starts its own thread");
    let s = session_get(&conn, &a).unwrap();
    assert_eq!((s.title.as_deref(), s.post_count), (Some("Auth refactor"), 2));
    assert_eq!(owned_session(&conn, "pid2:200"), Some(b));
    assert_eq!(owned_session(&conn, "pid3:300"), None);
}

#[test]
fn post_new_uses_the_callers_implicit_session() {
    let conn = db();
    let args = PostNewArgs {
        session: None,
        title: None,
        parts: vec![md("x")],
        ..Default::default()
    };
    let (a, ev) = post_new(&conn, &args, Some("claude"), Some("/repo")).unwrap();
    assert_eq!(
        ev.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
        ["session-created", "post-created"]
    );
    assert_eq!(
        (a.post.title.as_str(), a.post.version, a.post.kinds.clone()),
        ("Untitled", 1, vec!["markdown".to_owned()])
    );
    let (b, ev) = post_new(&conn, &args, Some("claude"), Some("/repo")).unwrap();
    assert_eq!(
        b.post.session, a.post.session,
        "same agent + cwd reuses the session"
    );
    assert_eq!(ev.len(), 1);
    let (c, _) = post_new(&conn, &args, Some("claude"), Some("/other")).unwrap();
    let (d, _) = post_new(&conn, &args, Some("codex"), Some("/repo")).unwrap();
    let (e, _) = post_new(&conn, &args, None, None).unwrap();
    assert_ne!(c.post.session, a.post.session);
    assert_ne!(d.post.session, a.post.session);
    assert_eq!(session_get(&conn, &e.post.session).unwrap().agent, "agent");
    assert_eq!(session_get(&conn, &a.post.session).unwrap().post_count, 2);
    let missing = PostNewArgs {
        session: Some("nope".into()),
        ..args
    };
    assert_eq!(
        code_of(post_new(&conn, &missing, None, None)),
        ErrorCode::NotFound
    );
}

#[test]
fn validation_rules_reject_bad_parts() {
    let conn = db();
    let bad = |parts: Vec<Part>| {
        let err = post_new(
            &conn,
            &PostNewArgs {
                session: None,
                title: None,
                parts,
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::Usage, "{}", err.message);
        err.message
    };
    assert!(bad(vec![]).contains("at least one part"));
    assert!(bad(vec![Part::Html {
        html: "<b>".into(),
        kits: vec!["nope".into()],
        id: None
    }])
    .contains("unknown kit \"nope\" — known: issues, slides"));
    let diff = |patch: Option<&str>, files: Vec<DiffFile>| Part::Diff {
        patch: patch.map(Into::into),
        files,
        layout: DiffLayout::Unified,
        id: None,
    };
    assert!(bad(vec![diff(None, vec![])]).contains("\"patch\" or non-empty \"files\""));
    assert!(bad(vec![md("ok"), diff(Some("not a patch"), vec![])])
        .starts_with("parts[1].patch: did not parse"));
    let unnamed = DiffFile {
        filename: " ".into(),
        before: "a".into(),
        after: "b".into(),
        language: None,
    };
    assert!(bad(vec![diff(None, vec![unnamed])]).contains("files[0].filename"));
    assert!(bad(vec![Part::Image {
        asset_id: "abc".into(),
        alt: None,
        caption: None,
        id: None
    }])
    .contains("no asset \"abc\""));
    let trace = |steps: Vec<TraceStep>, asset_id: Option<&str>| Part::Trace {
        steps,
        asset_id: asset_id.map(Into::into),
        title: None,
        id: None,
    };
    assert!(bad(vec![trace(vec![], None)]).contains("\"asset_id\" or non-empty \"steps\""));
    assert!(bad(vec![trace(vec![], Some("def"))]).contains("no asset \"def\""));
    let zero = Part::Code {
        code: "x".into(),
        language: None,
        title: None,
        line_start: Some(0),
        id: None,
    };
    assert!(bad(vec![zero]).contains("line_start"));
    assert!(bad(vec![Part::Mermaid {
        mermaid: "%% only a comment\n".into(),
        id: None
    }])
    .contains("no diagram type"));
    assert!(bad(vec![md(&"x".repeat(MAX_PART_BYTES + 1))]).contains("over the 2097152-byte limit"));
    let big = md(&"x".repeat(MAX_PART_BYTES - 10));
    assert!(bad(vec![big; 9]).contains("over the 16777216-byte limit"));

    // The valid forms of the same kinds pass.
    let png = put(&conn, b"png-bytes", "a.png");
    let ok = vec![
        Part::Html {
            html: "<b>".into(),
            kits: vec!["issues".into(), "slides".into()],
            id: None,
        },
        diff(Some("--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n"), vec![]),
        Part::Image {
            asset_id: png.id.clone(),
            alt: None,
            caption: None,
            id: None,
        },
        trace(
            vec![TraceStep {
                label: "ran".into(),
                kind: None,
                detail: None,
                ts: None,
            }],
            None,
        ),
        trace(vec![], Some(&png.id)),
        Part::Mermaid {
            mermaid: "%% c\nflowchart TD\n a-->b".into(),
            id: None,
        },
        Part::Json {
            data: serde_json::json!(null),
            id: None,
        },
        Part::Terminal {
            text: "".into(),
            cols: Some(80),
            title: None,
            id: None,
        },
    ];
    new_post(&conn, None, ok);
}

#[test]
fn titles_truncate_and_part_ids_are_assigned_and_kept() {
    let conn = db();
    let mut given = md("b");
    if let Part::Markdown { id, .. } = &mut given {
        *id = Some("mine".into());
    }
    let args = PostNewArgs {
        session: None,
        title: Some("é".repeat(600)),
        parts: vec![md("a"), given.clone(), given],
        ..Default::default()
    };
    let (r, _) = post_new(&conn, &args, None, None).unwrap();
    assert_eq!(r.post.title.chars().count(), MAX_TITLE);
    let ids = ids(&conn, &r.post.id);
    assert_eq!(ids[1], "mine");
    assert!(
        ids[0].len() == 11 && ids[2].len() == 11 && ids[2] != "mine",
        "missing and duplicate ids get fresh ones: {ids:?}"
    );
    assert!(!ids[0].starts_with(['-', '_']));
}

#[test]
fn updates_version_posts_and_keep_history() {
    let conn = db();
    let p = new_post(&conn, None, vec![md("v1")]).post;
    let (r, ev) = post_update(
        &conn,
        &PostUpdateArgs {
            id: p.id.clone(),
            title: Some("Renamed".into()),
            parts: None,
        },
    )
    .unwrap();
    assert_eq!(
        (
            r.post.version,
            r.post.title.as_str(),
            ev[0].kind.as_str(),
            ev[0].version
        ),
        (2, "Renamed", "post-updated", Some(2))
    );
    let v2 = post_show(
        &conn,
        &PostShowArgs {
            id: p.id.clone(),
            version: None,
        },
    )
    .unwrap();
    assert_eq!(
        v2.parts,
        vec![Part::Markdown {
            markdown: "v1".into(),
            id: Some(ids(&conn, &p.id)[0].clone())
        }]
    );
    post_update(
        &conn,
        &PostUpdateArgs {
            id: p.id.clone(),
            title: None,
            parts: Some(vec![code("fn x() {}", "rust")]),
        },
    )
    .unwrap();
    let v1 = post_show(
        &conn,
        &PostShowArgs {
            id: p.id.clone(),
            version: Some(1),
        },
    )
    .unwrap();
    assert_eq!(
        (v1.title.as_str(), v1.version, part_kind(&v1.parts[0])),
        ("T", 1, "markdown")
    );
    let v3 = post_show(
        &conn,
        &PostShowArgs {
            id: p.id.clone(),
            version: Some(3),
        },
    )
    .unwrap();
    assert_eq!(
        (v3.title.as_str(), part_kind(&v3.parts[0])),
        ("Renamed", "code")
    );
    assert_eq!(
        code_of(post_update(
            &conn,
            &PostUpdateArgs {
                id: p.id.clone(),
                title: None,
                parts: Some(vec![])
            }
        )),
        ErrorCode::Usage
    );
    assert_eq!(
        code_of(post_update(
            &conn,
            &PostUpdateArgs {
                id: "nope".into(),
                title: None,
                parts: None
            }
        )),
        ErrorCode::NotFound
    );

    // Every version is kept: the session folder never prunes history.
    for _ in 0..25 {
        post_update(
            &conn,
            &PostUpdateArgs {
                id: p.id.clone(),
                title: None,
                parts: None,
            },
        )
        .unwrap();
    }
    let current = 3 + 25;
    assert!(post_show(
        &conn,
        &PostShowArgs {
            id: p.id.clone(),
            version: Some(1)
        }
    )
    .is_ok());
    let gone = post_show(
        &conn,
        &PostShowArgs {
            id: p.id.clone(),
            version: Some(current + 1),
        },
    )
    .unwrap_err();
    assert_eq!(gone.code, ErrorCode::NotFound);
    assert!(gone.message.contains(&format!("current is {current}")));
}

#[test]
fn part_ops_by_id_and_index_make_new_versions() {
    let conn = db();
    let p = new_post(&conn, None, vec![md("a"), md("b")]).post;
    let op = |op: PartOp, target: Option<&str>, part: Option<Part>| {
        post_part(
            &conn,
            &PostPartArgs {
                id: p.id.clone(),
                op,
                target: target.map(Into::into),
                part,
            },
        )
    };
    let texts = || -> Vec<String> {
        post_show(
            &conn,
            &PostShowArgs {
                id: p.id.clone(),
                version: None,
            },
        )
        .unwrap()
        .parts
        .iter()
        .map(|part| match part {
            Part::Markdown { markdown, .. } => markdown.clone(),
            other => part_kind(other).to_owned(),
        })
        .collect()
    };
    let [a, b] = <[String; 2]>::try_from(ids(&conn, &p.id)).unwrap();

    let (r, _) = op(PartOp::Add, None, Some(md("c"))).unwrap();
    assert_eq!(
        (r.post.version, texts()),
        (2, vec!["a".into(), "b".into(), "c".into()])
    );
    op(PartOp::Add, Some("0"), Some(md("first"))).unwrap();
    op(PartOp::Add, Some(&b), Some(md("before-b"))).unwrap();
    op(PartOp::Add, Some("5"), Some(md("end"))).unwrap(); // index == len appends
    assert_eq!(texts(), ["first", "a", "before-b", "b", "c", "end"]);

    op(PartOp::Set, Some(&a), Some(code("x", "rust"))).unwrap();
    op(PartOp::Set, Some("0"), Some(md("FIRST"))).unwrap();
    assert_eq!(texts(), ["FIRST", "code", "before-b", "b", "c", "end"]);
    assert_eq!(ids(&conn, &p.id)[1], a, "set keeps the part id");

    op(PartOp::Rm, Some(&b), None).unwrap();
    let (r, _) = op(PartOp::Rm, Some("0"), None).unwrap();
    assert_eq!(texts(), ["code", "before-b", "c", "end"]);
    assert_eq!(r.post.version, 9);
    assert_eq!(r.post.kinds, ["code", "markdown", "markdown", "markdown"]);

    assert_eq!(
        code_of(op(PartOp::Rm, Some("nope"), None)),
        ErrorCode::NotFound
    );
    assert_eq!(
        code_of(op(PartOp::Set, Some("9"), Some(md("x")))),
        ErrorCode::NotFound
    );
    assert_eq!(
        code_of(op(PartOp::Add, Some("99"), Some(md("x")))),
        ErrorCode::NotFound
    );
    assert_eq!(code_of(op(PartOp::Add, None, None)), ErrorCode::Usage);
    assert_eq!(code_of(op(PartOp::Rm, None, None)), ErrorCode::Usage);
    assert_eq!(
        code_of(op(
            PartOp::Set,
            Some("0"),
            Some(Part::Image {
                asset_id: "zz".into(),
                alt: None,
                caption: None,
                id: None
            })
        )),
        ErrorCode::Usage
    );
    for _ in 0..3 {
        op(PartOp::Rm, Some("0"), None).unwrap();
    }
    assert!(
        code_of(op(PartOp::Rm, Some("0"), None)) == ErrorCode::Usage,
        "the last part can't be removed"
    );
}

#[test]
fn post_writes_piggyback_unseen_user_comments_once() {
    let conn = db();
    let p = new_post(&conn, None, vec![md("a")]).post;
    let first = comment(&conn, &p.id, "user", "looks wrong");
    comment(&conn, &p.id, "claude", "agent note");
    let second = comment(&conn, &p.id, "user", "  also this  ");
    let update = || {
        post_update(
            &conn,
            &PostUpdateArgs {
                id: p.id.clone(),
                title: None,
                parts: None,
            },
        )
        .unwrap()
        .0
    };
    let r = update();
    assert_eq!(
        r.feedback.iter().map(|c| c.seq).collect::<Vec<_>>(),
        [first.seq, second.seq]
    );
    assert_eq!(r.feedback[1].text, "also this");
    assert!(update().feedback.is_empty(), "delivered once");
    // Another post in the same session gets later feedback too.
    let third = comment(&conn, &p.id, "user", "and another");
    let r = new_post(&conn, Some(&p.session), vec![md("b")]);
    assert_eq!(
        r.feedback.iter().map(|c| c.id.clone()).collect::<Vec<_>>(),
        [third.id]
    );
    let r = post_part(
        &conn,
        &PostPartArgs {
            id: p.id.clone(),
            op: PartOp::Add,
            target: None,
            part: Some(md("c")),
        },
    )
    .unwrap()
    .0;
    assert!(r.feedback.is_empty());
}

#[test]
fn comments_anchor_list_delete_and_wait_cursor() {
    let conn = db();
    let p = new_post(&conn, None, vec![md("a"), code("fn a() {}\n", "rust")]).post;
    let new = |args: CommentNewArgs| comment_new(&conn, &args, Some("claude"));
    let base = CommentNewArgs {
        post: Some(p.id.clone()),
        session: None,
        text: "hi".into(),
        anchor: None,
        author: None,
    };

    let (c, ev) = new(base.clone()).unwrap();
    assert_eq!(
        (c.author.as_str(), c.session.as_str(), ev[0].kind.as_str()),
        ("claude", p.session.as_str(), "comment-created")
    );
    let anchored = new(CommentNewArgs {
        anchor: Some(CommentAnchor::LineRange {
            part: 1,
            post_version: 99,
            start_line: 5,
            end_line: 2,
            file: None,
        }),
        author: Some("user".into()),
        ..base.clone()
    })
    .unwrap()
    .0;
    assert_eq!(
        anchored.anchor,
        Some(CommentAnchor::LineRange {
            part: 1,
            post_version: 1,
            start_line: 5,
            end_line: 5,
            file: None
        }),
        "pinned to the current version; end clamps to start"
    );
    let bad_anchor = |anchor| {
        code_of(new(CommentNewArgs {
            anchor: Some(anchor),
            ..base.clone()
        }))
    };
    assert_eq!(
        bad_anchor(CommentAnchor::Point {
            part: 2,
            post_version: 1,
            x: 0.5,
            y: 0.5
        }),
        ErrorCode::Usage
    );
    assert_eq!(
        bad_anchor(CommentAnchor::Rect {
            part: 0,
            post_version: 1,
            x: 0.1,
            y: 0.1,
            w: 1.5,
            h: 0.1
        }),
        ErrorCode::Usage
    );
    assert_eq!(
        bad_anchor(CommentAnchor::Point {
            part: 0,
            post_version: 1,
            x: f64::NAN,
            y: 0.0
        }),
        ErrorCode::Usage
    );
    assert_eq!(
        bad_anchor(CommentAnchor::LineRange {
            part: 0,
            post_version: 1,
            start_line: 0,
            end_line: 1,
            file: None
        }),
        ErrorCode::Usage
    );
    assert_eq!(
        code_of(new(CommentNewArgs {
            text: "   ".into(),
            ..base.clone()
        })),
        ErrorCode::Usage
    );
    assert_eq!(
        code_of(new(CommentNewArgs {
            post: None,
            ..base.clone()
        })),
        ErrorCode::Usage
    );
    assert_eq!(
        code_of(new(CommentNewArgs {
            post: Some("nope".into()),
            ..base.clone()
        })),
        ErrorCode::NotFound
    );
    assert_eq!(
        code_of(new(CommentNewArgs {
            session: Some("other".into()),
            ..base.clone()
        })),
        ErrorCode::Usage
    );
    let long = new(CommentNewArgs {
        text: "y".repeat(MAX_COMMENT_TEXT + 50),
        ..base.clone()
    })
    .unwrap()
    .0;
    assert_eq!(long.text.len(), MAX_COMMENT_TEXT);
    let on_session = new(CommentNewArgs {
        post: None,
        session: Some(p.session.clone()),
        author: Some("user".into()),
        ..base.clone()
    })
    .unwrap()
    .0;
    assert_eq!(on_session.post, None);

    let all = comment_list(
        &conn,
        &CommentListArgs {
            session: Some(p.session.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        all.iter().map(|c| c.seq).collect::<Vec<_>>(),
        [c.seq, anchored.seq, long.seq, on_session.seq]
    );
    let after = comment_list(
        &conn,
        &CommentListArgs {
            post: Some(p.id.clone()),
            after_seq: Some(anchored.seq),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(after.iter().map(|c| c.seq).collect::<Vec<_>>(), [long.seq]);

    // wait: user comments only, after the agent's cursor, which advances.
    let next = |post: Option<&str>| {
        next_user_comment(&conn, Some(&p.session), post, None)
            .unwrap()
            .map(|c| c.seq)
    };
    assert_eq!(next(Some(&p.id)), Some(anchored.seq));
    assert_eq!(next(None), Some(on_session.seq));
    assert_eq!(next(None), None);
    assert_eq!(
        next_user_comment(&conn, None, Some(&p.id), Some(0))
            .unwrap()
            .map(|c| c.seq),
        Some(anchored.seq),
        "explicit cursor"
    );
    assert_eq!(
        code_of(next_user_comment(&conn, None, None, None)),
        ErrorCode::Usage
    );
    assert_eq!(
        code_of(next_user_comment(&conn, Some("nope"), None, None)),
        ErrorCode::NotFound
    );

    let ev = comment_delete(&conn, &c.id).unwrap();
    assert_eq!(
        (ev[0].kind.as_str(), ev[0].session.as_deref()),
        ("comment-deleted", Some(p.session.as_str()))
    );
    assert_eq!(code_of(comment_delete(&conn, &c.id)), ErrorCode::NotFound);

    let ev = post_delete(&conn, &p.id).unwrap();
    assert_eq!(ev[0].kind, "post-deleted");
    let left = comment_list(&conn, &CommentListArgs::default()).unwrap();
    assert_eq!(
        left.iter().map(|c| c.seq).collect::<Vec<_>>(),
        [on_session.seq],
        "post comments go with the post"
    );
    assert_eq!(
        code_of(post_show(
            &conn,
            &PostShowArgs {
                id: p.id.clone(),
                version: Some(1)
            }
        )),
        ErrorCode::NotFound
    );
}

#[test]
fn wait_blocks_on_the_signal_until_a_user_comment_lands() {
    let conn = db();
    let p = new_post(&conn, None, vec![md("a")]).post;
    let conn = Arc::new(Mutex::new(conn));
    let signal = Arc::new(CommentSignal::default());

    let started = Instant::now();
    let none = signal
        .wait_for(Some(Duration::from_millis(50)), || {
            next_user_comment(&conn.lock().unwrap(), Some(&p.session), None, None)
        })
        .unwrap();
    assert!(none.is_none());
    assert!(started.elapsed() >= Duration::from_millis(50));

    let writer = {
        let (conn, signal, post) = (conn.clone(), signal.clone(), p.id.clone());
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(30));
            comment(
                &conn.lock().unwrap(),
                &post,
                "claude",
                "agent comments don't wake it for good",
            );
            signal.notify();
            thread::sleep(Duration::from_millis(30));
            comment(&conn.lock().unwrap(), &post, "user", "reply");
            signal.notify();
        })
    };
    let got = signal
        .wait_for(None, || {
            next_user_comment(&conn.lock().unwrap(), Some(&p.session), None, None)
        })
        .unwrap();
    assert_eq!(got.map(|c| c.text), Some("reply".into()));
    writer.join().unwrap();
}

#[test]
fn assets_dedupe_by_content_hash() {
    let conn = db();
    let a = put(&conn, b"hello", "first.png");
    assert_eq!(
        a.id,
        "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
    );
    assert_eq!(
        (a.byte_length, a.kind, a.content_type.as_str()),
        (5, AssetKind::Image, "image/png")
    );
    let again = put(&conn, b"hello", "second.png");
    assert_eq!(again, a, "dedupe keeps the original record");
    let stored = std::fs::read_dir(conn.root().join("assets")).unwrap().count();
    assert_eq!(stored, 2, "one asset: its bytes and its metadata file");
    let got = asset_get(&conn, &a.id).unwrap();
    assert_eq!(
        (got.info, STANDARD.decode(got.data_base64).unwrap()),
        (a, b"hello".to_vec())
    );
    assert_eq!(code_of(asset_get(&conn, "nope")), ErrorCode::NotFound);

    let args = |data: String| AssetPutArgs {
        session: None,
        kind: AssetKind::File,
        content_type: "".into(),
        filename: None,
        data_base64: data,
    };
    assert_eq!(
        asset_put(&conn, &args(STANDARD.encode(b"x")))
            .unwrap()
            .content_type,
        "application/octet-stream"
    );
    assert_eq!(
        code_of(asset_put(&conn, &args(String::new()))),
        ErrorCode::Usage
    );
    assert_eq!(
        code_of(asset_put(&conn, &args("not base64!".into()))),
        ErrorCode::Usage
    );
    let too_big = asset_put(
        &conn,
        &args(STANDARD.encode(vec![0u8; MAX_ASSET_BYTES + 1])),
    )
    .unwrap_err();
    assert!(too_big.message.contains("exceeds"), "{}", too_big.message);
    let in_missing = AssetPutArgs {
        session: Some("nope".into()),
        ..args(STANDARD.encode(b"y"))
    };
    assert_eq!(code_of(asset_put(&conn, &in_missing)), ErrorCode::NotFound);
}

#[test]
fn session_delete_keeps_assets_other_sessions_reference() {
    let conn = db();
    let (gone, _) = session_new(
        &conn,
        &SessionNewArgs {
            agent: "a".into(),
            title: None,
            cwd: None,
        },
    )
    .unwrap();
    let (kept, _) = session_new(
        &conn,
        &SessionNewArgs {
            agent: "b".into(),
            title: None,
            cwd: None,
        },
    )
    .unwrap();
    let upload = |data: &[u8]| {
        let args = AssetPutArgs {
            session: Some(gone.id.clone()),
            kind: AssetKind::Image,
            content_type: "image/png".into(),
            filename: None,
            data_base64: STANDARD.encode(data),
        };
        asset_put(&conn, &args).unwrap().id
    };
    let (private, shared) = (upload(b"private"), upload(b"shared"));
    let image = |id: &str| Part::Image {
        asset_id: id.into(),
        alt: None,
        caption: None,
        id: None,
    };
    new_post(&conn, Some(&gone.id), vec![image(&private)]);
    new_post(&conn, Some(&kept.id), vec![image(&shared)]);
    session_delete(&conn, &delete(&gone.id, true)).unwrap();
    assert_eq!(code_of(asset_get(&conn, &private)), ErrorCode::NotFound);
    assert!(asset_get(&conn, &shared).is_ok());
}

#[test]
fn traces_replace_truncate_and_cap() {
    let conn = db();
    let (s, _) = session_new(
        &conn,
        &SessionNewArgs {
            agent: "a".into(),
            title: None,
            cwd: None,
        },
    )
    .unwrap();
    let step = |label: String| TraceStep {
        label,
        kind: Some("k".repeat(100)),
        detail: Some("d".repeat(5000)),
        ts: Some("t".into()),
    };
    let steps: Vec<_> = (0..MAX_TRACE_STEPS + 5)
        .map(|i| step(format!("{i}")))
        .collect();
    let (count, ev) = trace_set(
        &conn,
        &TraceSetArgs {
            session: s.id.clone(),
            steps,
        },
    )
    .unwrap();
    assert_eq!(
        (count, ev[0].kind.as_str()),
        (MAX_TRACE_STEPS, "trace-updated")
    );
    let stored = trace_list(&conn, &s.id).unwrap();
    assert_eq!(
        (stored.len(), stored[0].label.as_str()),
        (MAX_TRACE_STEPS, "5"),
        "keeps the newest"
    );
    assert_eq!(
        (
            stored[0].kind.as_ref().unwrap().len(),
            stored[0].detail.as_ref().unwrap().len()
        ),
        (40, 4000)
    );
    trace_set(
        &conn,
        &TraceSetArgs {
            session: s.id.clone(),
            steps: vec![step("x".repeat(900))],
        },
    )
    .unwrap();
    let stored = trace_list(&conn, &s.id).unwrap();
    assert_eq!((stored.len(), stored[0].label.len()), (1, 500));
    assert_eq!(code_of(trace_list(&conn, "nope")), ErrorCode::NotFound);
    assert_eq!(
        code_of(trace_set(
            &conn,
            &TraceSetArgs {
                session: "nope".into(),
                steps: vec![]
            }
        )),
        ErrorCode::NotFound
    );
}

#[test]
fn settings_and_kits() {
    let conn = db();
    assert_eq!(setting_get(&conn, "theme").unwrap(), None);
    let ev = setting_set(
        &conn,
        &SettingSetArgs {
            key: "theme".into(),
            value: "dark".into(),
        },
    )
    .unwrap();
    assert_eq!(
        (ev[0].kind.as_str(), ev[0].id.as_deref()),
        ("setting-changed", Some("theme"))
    );
    setting_set(
        &conn,
        &SettingSetArgs {
            key: "theme".into(),
            value: "light".into(),
        },
    )
    .unwrap();
    assert_eq!(
        setting_get(&conn, "theme").unwrap().as_deref(),
        Some("light")
    );
    assert_eq!(
        code_of(setting_set(
            &conn,
            &SettingSetArgs {
                key: " ".into(),
                value: "x".into()
            }
        )),
        ErrorCode::Usage
    );

    let kits = kits();
    assert_eq!(
        kits.iter().map(|k| k.id.as_str()).collect::<Vec<_>>(),
        KIT_IDS
    );
    assert!(
        kits[0].css.starts_with("\n.stack{display:flex"),
        "core helpers ship with each kit"
    );
    assert!(kits[0].css.contains(".badge.ok{") && kits[0].js.is_none());
    assert!(
        kits[1].css.contains(".deck>.slide.on{")
            && kits[1]
                .js
                .as_deref()
                .unwrap()
                .contains("e.key==='ArrowRight'")
    );
}

#[test]
fn post_list_orders_filters_and_limits() {
    let conn = db();
    let a = new_post(&conn, None, vec![md("a")]).post;
    let b = new_post(&conn, None, vec![md("b"), code("x", "go")]).post;
    let (other, _) = session_new(
        &conn,
        &SessionNewArgs {
            agent: "x".into(),
            title: None,
            cwd: None,
        },
    )
    .unwrap();
    let c = new_post(&conn, Some(&other.id), vec![md("c")]).post;
    let all = post_list(&conn, &PostListArgs::default()).unwrap();
    assert_eq!(
        all.iter().map(|p| p.id.clone()).collect::<Vec<_>>(),
        [c.id.clone(), b.id.clone(), a.id.clone()]
    );
    assert_eq!(all[1].kinds, ["markdown", "code"]);
    let one = post_list(
        &conn,
        &PostListArgs {
            session: Some(a.session.clone()),
            limit: Some(1),
        },
    )
    .unwrap();
    assert_eq!(one.iter().map(|p| p.id.clone()).collect::<Vec<_>>(), [b.id]);
    assert_eq!(
        code_of(post_list(
            &conn,
            &PostListArgs {
                session: Some("nope".into()),
                limit: None
            }
        )),
        ErrorCode::NotFound
    );
}

#[test]
fn post_render_end_to_end() {
    let conn = db();
    let patch = "diff --git a/a.py b/a.py\n--- a/a.py\n+++ b/a.py\n@@ -1,2 +1,2 @@\n def f():\n-    return 1\n+    return 2\n";
    let parts = vec![
        md("# intro"),
        code("const x: number = 1;\n", "ts"),
        Part::Diff {
            patch: Some(patch.into()),
            files: vec![],
            layout: DiffLayout::Split,
            id: None,
        },
        Part::Terminal {
            text: "\x1b[32mPASS\x1b[0m 3 tests\n".into(),
            cols: None,
            title: None,
            id: None,
        },
    ];
    let p = new_post(&conn, None, parts).post;
    let cache = RenderCache::with_config(true, 1 << 20);
    let r = post_render(
        &conn,
        &cache,
        &PostShowArgs {
            id: p.id.clone(),
            version: None,
        },
    )
    .unwrap();
    assert_eq!(r.parts[0], PartRender::Native);
    let PartRender::Code {
        language, lines, ..
    } = &r.parts[1]
    else {
        panic!("code")
    };
    assert_eq!(language.as_deref(), Some("typescript"));
    assert!(
        lines[0]
            .iter()
            .any(|s| s.text == "const" && s.class.as_deref() == Some("keyword")),
        "{lines:?}"
    );
    let PartRender::Diff { layout, files } = &r.parts[2] else {
        panic!("diff")
    };
    assert_eq!(
        (
            *layout,
            files[0].filename.as_str(),
            files[0].language.as_deref()
        ),
        (DiffLayout::Split, "a.py", Some("python"))
    );
    let removed = &files[0].hunks[0].lines[1];
    assert_eq!(
        (removed.kind, removed.old_line, removed.new_line),
        (DiffLineKind::Removed, Some(2), None)
    );
    assert!(removed
        .spans
        .iter()
        .any(|s| s.text == "return" && s.class.as_deref() == Some("keyword")));
    let PartRender::Terminal { lines } = &r.parts[3] else {
        panic!("terminal")
    };
    assert_eq!(
        lines[0][0],
        Span {
            text: "PASS".into(),
            class: Some("fg-green".into())
        }
    );
    assert_eq!((cache.stats().hits, cache.stats().misses), (0, 3));

    // A new version that changes one part re-renders only that part.
    post_part(
        &conn,
        &PostPartArgs {
            id: p.id.clone(),
            op: PartOp::Set,
            target: Some("3".into()),
            part: Some(Part::Terminal {
                text: "done\n".into(),
                cols: None,
                title: None,
                id: None,
            }),
        },
    )
    .unwrap();
    let r2 = post_render(
        &conn,
        &cache,
        &PostShowArgs {
            id: p.id.clone(),
            version: None,
        },
    )
    .unwrap();
    assert_eq!(
        (r2.version, cache.stats().hits, cache.stats().misses),
        (2, 2, 4)
    );
    let v1 = PostShowArgs {
        id: p.id,
        version: Some(1),
    };
    let old = post_render(&conn, &cache, &v1).unwrap();
    assert_eq!(old.parts, r.parts);
    assert_eq!(cache.stats().misses, 4);
    // The wire-JSON path serves the same render, straight from the cache.
    let json = post_render_json(&conn, &cache, &v1).unwrap();
    assert_eq!(serde_json::from_str::<PostRender>(&json).unwrap(), old);
    assert_eq!(cache.stats().misses, 4);
    assert_eq!(
        code_of(post_render_json(
            &conn,
            &cache,
            &PostShowArgs {
                id: "nope".into(),
                version: None
            }
        )),
        ErrorCode::NotFound
    );
}

/// A session with one post (an image asset) and one comment.
fn session_with_asset(conn: &Store, bytes: &[u8]) -> (SessionId, AssetId) {
    let (s, _) = session_new(conn, &SessionNewArgs { agent: "a".into(), title: Some("T".into()), cwd: None }).unwrap();
    let args = AssetPutArgs {
        session: Some(s.id.clone()),
        kind: AssetKind::Image,
        content_type: "image/png".into(),
        filename: None,
        data_base64: STANDARD.encode(bytes),
    };
    let asset = asset_put(conn, &args).unwrap().id;
    let image = Part::Image { asset_id: asset.clone(), alt: None, caption: None, id: None };
    let post = new_post(conn, Some(&s.id), vec![image]).post;
    let comment = CommentNewArgs { post: Some(post.id), session: None, text: "keep me".into(), anchor: None, author: None };
    comment_new(conn, &comment, None).unwrap();
    (s.id, asset)
}

#[test]
fn session_trash_restore_round_trip_keeps_posts_comments_and_assets() {
    let conn = db();
    let (id, asset) = session_with_asset(&conn, b"pixels");
    let (r, ev) = session_delete(&conn, &delete(&id, false)).unwrap();
    assert_eq!((r.trashed, r.purged, r.already, ev[0].kind.as_str()), (true, false, false, "session-deleted"));
    assert_eq!(code_of(session_get(&conn, &id)), ErrorCode::NotFound);
    assert!(session_list(&conn).unwrap().is_empty());
    assert!(asset_get(&conn, &asset).is_ok());
    // A second delete changes nothing and says so.
    let (r, ev) = session_delete(&conn, &delete(&id, false)).unwrap();
    assert_eq!((r.trashed, r.already, ev.len()), (true, true, 0));

    let (r, ev) = session_restore(&conn, &id).unwrap();
    assert_eq!((r.restored, r.already, ev[0].kind.as_str()), (true, false, "session-restored"));
    assert_eq!(session_get(&conn, &id).unwrap().post_count, 1);
    assert_eq!(comment_list(&conn, &CommentListArgs { session: Some(id.clone()), ..Default::default() }).unwrap().len(), 1);
    assert!(asset_get(&conn, &asset).is_ok());
    // Restoring a live session changes nothing.
    let (r, ev) = session_restore(&conn, &id).unwrap();
    assert_eq!((r.already, ev.len()), (true, 0));
    assert_eq!(code_of(session_restore(&conn, "nope")), ErrorCode::NotFound);
}

#[test]
fn session_purge_deletes_live_and_trashed_sessions_with_their_assets() {
    let conn = db();
    let (live, live_asset) = session_with_asset(&conn, b"pixels");
    let (r, ev) = session_delete(&conn, &delete(&live, true)).unwrap();
    assert_eq!((r.purged, r.trashed, ev[0].kind.as_str()), (true, false, "session-deleted"));
    assert_eq!(code_of(asset_get(&conn, &live_asset)), ErrorCode::NotFound);
    assert_eq!(code_of(session_restore(&conn, &live)), ErrorCode::NotFound);
    // Purging twice is not_found: no tombstone is kept.
    assert_eq!(code_of(session_delete(&conn, &delete(&live, true))), ErrorCode::NotFound);

    let (trashed, asset) = session_with_asset(&conn, b"other");
    session_delete(&conn, &delete(&trashed, false)).unwrap();
    let (r, ev) = session_delete(&conn, &delete(&trashed, true)).unwrap();
    assert_eq!((r.purged, ev.len()), (true, 0));
    assert_eq!(code_of(asset_get(&conn, &asset)), ErrorCode::NotFound);
    assert_eq!(code_of(session_restore(&conn, &trashed)), ErrorCode::NotFound);
}

#[test]
fn trash_purge_waits_30_days_then_deletes_unreferenced_assets() {
    let conn = db();
    let (id, asset) = session_with_asset(&conn, b"pixels");
    session_delete(&conn, &delete(&id, false)).unwrap();
    let day = 86_400;
    let soon = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    trash_purge(&conn, soon + 29 * day).unwrap();
    assert!(asset_get(&conn, &asset).is_ok());
    assert!(session_restore(&conn, &id).is_ok());
    session_delete(&conn, &delete(&id, false)).unwrap();
    trash_purge(&conn, soon + 31 * day).unwrap();
    assert_eq!(code_of(asset_get(&conn, &asset)), ErrorCode::NotFound);
    assert_eq!(code_of(session_restore(&conn, &id)), ErrorCode::NotFound);
}

#[test]
fn trashed_sessions_survive_a_reopen_and_keep_the_assets_others_share() {
    let conn = db();
    let (id, asset) = session_with_asset(&conn, b"pixels");
    // Another live session references the same bytes; purging the owner keeps them.
    let (other, _) = session_new(&conn, &SessionNewArgs { agent: "b".into(), title: None, cwd: None }).unwrap();
    new_post(&conn, Some(&other.id), vec![Part::Image { asset_id: asset.clone(), alt: None, caption: None, id: None }]);
    session_delete(&conn, &delete(&id, false)).unwrap();
    session_delete(&conn, &delete(&other.id, true)).unwrap();
    assert!(asset_get(&conn, &asset).is_ok(), "the trashed session still references it");
    let reopened = Store::open(Connection::open_in_memory().unwrap(), conn.root().to_path_buf()).unwrap();
    assert!(session_list(&reopened).unwrap().is_empty());
    assert!(session_restore(&reopened, &id).is_ok());
}
