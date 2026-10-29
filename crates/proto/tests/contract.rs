//! Wire contract: one golden request line per verb and one golden response per
//! sideshow result shape, in tests/fixtures/. Rust must round-trip each byte for
//! byte; Notes.app's Swift tests decode the same files. A changed fixture is a
//! contract change, so it shows up in review. Regenerate with
//! `UPDATE_FIXTURES=1 cargo test -p notes-proto --test contract`.

use std::{fs, path::PathBuf};

use notes_proto::{posts::*, *};
use serde_json::{json, Value};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// The wire name of every verb. Exhaustive on purpose: a new verb doesn't compile
/// until it has a name here, and `every_verb_has_a_request_fixture` then needs its sample.
fn name(verb: &Verb) -> &'static str {
    match verb {
        Verb::List(_) => "list",
        Verb::Find(_) => "find",
        Verb::Search(_) => "search",
        Verb::Read(_) => "read",
        Verb::New(_) => "new",
        Verb::Edit(_) => "edit",
        Verb::Append(_) => "append",
        Verb::Prepend(_) => "prepend",
        Verb::Write(_) => "write",
        Verb::Move(_) => "move",
        Verb::Mkdir(_) => "mkdir",
        Verb::Rmdir(_) => "rmdir",
        Verb::Delete(_) => "delete",
        Verb::Restore(_) => "restore",
        Verb::History(_) => "history",
        Verb::Diff(_) => "diff",
        Verb::Revert(_) => "revert",
        Verb::Watch(_) => "watch",
        Verb::Batch(_) => "batch",
        Verb::Manifest => "manifest",
        Verb::Doctor => "doctor",
        Verb::SessionNew(_) => "session-new",
        Verb::SessionList => "session-list",
        Verb::SessionSearch(_) => "session-search",
        Verb::SessionRename(_) => "session-rename",
        Verb::SessionDelete(_) => "session-delete",
        Verb::SessionRestore(_) => "session-restore",
        Verb::PostNew(_) => "post-new",
        Verb::PostUpdate(_) => "post-update",
        Verb::PostPart(_) => "post-part",
        Verb::PostList(_) => "post-list",
        Verb::PostShow(_) => "post-show",
        Verb::PostRender(_) => "post-render",
        Verb::PostDelete(_) => "post-delete",
        Verb::CommentNew(_) => "comment-new",
        Verb::CommentList(_) => "comment-list",
        Verb::CommentDelete(_) => "comment-delete",
        Verb::Wait(_) => "wait",
        Verb::AssetPut(_) => "asset-put",
        Verb::AssetGet(_) => "asset-get",
        Verb::TraceSet(_) => "trace-set",
        Verb::TraceList(_) => "trace-list",
        Verb::Kits => "kits",
        Verb::SettingGet(_) => "setting-get",
        Verb::SettingSet(_) => "setting-set",
    }
}

const VERB_COUNT: usize = 45;

fn md(text: &str) -> Part {
    Part::Markdown { markdown: text.into(), id: Some("p0".into()) }
}

fn samples() -> Vec<Verb> {
    let note = || "work/plan".to_string();
    vec![
        Verb::List(ListArgs { folder: Some("work".into()), recursive: true, sort: ListSort::Title, limit: Some(10) }),
        Verb::Find(FindArgs { query: "plan".into(), limit: Some(5) }),
        Verb::Search(SearchArgs { query: "todo".into(), folder: None, context: 2, case_sensitive: false, fuzzy: true, limit: None }),
        Verb::Read(ReadArgs { note: note(), lines: Some(LineRange { start: 1, end: 3 }) }),
        Verb::New(NewArgs { path: "work/new".into(), body: "# New\n".into() }),
        Verb::Edit(EditArgs { note: note(), edits: vec![Replacement { old: "a".into(), new: "b".into(), all: false }], if_version: Some(3) }),
        Verb::Append(AppendArgs { note: note(), text: "- c\n".into(), under: Some("Todo".into()), if_version: None }),
        Verb::Prepend(PrependArgs { note: note(), text: "top\n".into(), if_version: None }),
        Verb::Write(WriteArgs { note: note(), body: "all\n".into(), if_version: Some(4), force: false }),
        Verb::Move(MoveArgs { note: note(), to: "archive/".into() }),
        Verb::Mkdir(FolderArgs { folder: "archive".into() }),
        Verb::Rmdir(FolderArgs { folder: "archive".into() }),
        Verb::Delete(DeleteArgs { note: note(), purge: false }),
        Verb::Restore(NoteArgs { note: note() }),
        Verb::History(HistoryArgs { note: note(), limit: Some(5) }),
        Verb::Diff(DiffArgs { note: note(), from: Some(1), to: None }),
        Verb::Revert(RevertArgs { note: note(), to: 2 }),
        Verb::Watch(WatchArgs { since: Some(12), kinds: vec!["post".into(), "comment".into()] }),
        Verb::Batch(BatchArgs { ops: vec![Verb::Doctor, Verb::SessionList], atomic: true }),
        Verb::Manifest,
        Verb::Doctor,
        Verb::SessionNew(SessionNewArgs { agent: "claude".into(), title: Some("Fix login".into()), cwd: Some("/repo".into()) }),
        Verb::SessionList,
        Verb::SessionSearch(SessionSearchArgs { query: "redirect".into(), limit: Some(20) }),
        Verb::SessionRename(SessionRenameArgs { id: "s1".into(), title: "Renamed".into() }),
        Verb::SessionDelete(SessionDeleteArgs { id: "s1".into(), purge: false }),
        Verb::SessionRestore(IdArgs { id: "s1".into() }),
        Verb::PostNew(PostNewArgs { session: Some("s1".into()), title: Some("Plan".into()), parts: vec![md("# Plan")], ..Default::default() }),
        Verb::PostUpdate(PostUpdateArgs { id: "p1".into(), title: None, parts: Some(vec![md("v2")]) }),
        Verb::PostPart(PostPartArgs { id: "p1".into(), op: PartOp::Set, target: Some("0".into()), part: Some(md("set")) }),
        Verb::PostList(PostListArgs { session: Some("s1".into()), limit: Some(50) }),
        Verb::PostShow(PostShowArgs { id: "p1".into(), version: Some(2) }),
        Verb::PostRender(PostShowArgs { id: "p1".into(), version: None }),
        Verb::PostDelete(IdArgs { id: "p1".into() }),
        Verb::CommentNew(CommentNewArgs {
            post: Some("p1".into()),
            session: None,
            text: "looks good".into(),
            anchor: Some(CommentAnchor::LineRange { part: 0, post_version: 2, start_line: 3, end_line: 4, file: None }),
            author: Some("user".into()),
        }),
        Verb::CommentList(CommentListArgs { session: Some("s1".into()), post: None, after_seq: Some(7) }),
        Verb::CommentDelete(IdArgs { id: "c1".into() }),
        Verb::Wait(WaitArgs { session: Some("s1".into()), post: None, timeout_secs: 120, owner: None }),
        Verb::AssetPut(AssetPutArgs { session: Some("s1".into()), kind: AssetKind::Image, content_type: "image/png".into(), filename: Some("h.png".into()), data_base64: "aGVsbG8=".into() }),
        Verb::AssetGet(IdArgs { id: "2cf24dba".into() }),
        Verb::TraceSet(TraceSetArgs { session: "s1".into(), steps: vec![TraceStep { label: "ran tests".into(), kind: Some("tool".into()), detail: None, ts: None }] }),
        Verb::TraceList(IdArgs { id: "s1".into() }),
        Verb::Kits,
        Verb::SettingGet(SettingGetArgs { key: "theme".into() }),
        Verb::SettingSet(SettingSetArgs { key: "theme".into(), value: "dark".into() }),
    ]
}

fn session() -> Session {
    Session {
        id: "s1".into(),
        agent: "claude".into(),
        title: Some("Fix login".into()),
        cwd: Some("/repo".into()),
        created_at: 1_761_400_000,
        last_active_at: 1_761_400_100,
        post_count: 1,
    }
}

fn summary() -> PostSummary {
    PostSummary {
        id: "p1".into(),
        session: "s1".into(),
        title: "Plan".into(),
        kinds: vec!["markdown".into()],
        version: 2,
        updated_at: 1_761_400_100,
    }
}

fn comment() -> Comment {
    Comment {
        id: "c1".into(),
        seq: 8,
        session: "s1".into(),
        post: Some("p1".into()),
        author: "user".into(),
        text: "looks good".into(),
        created_at: 1_761_400_200,
        anchor: Some(CommentAnchor::LineRange { part: 0, post_version: 2, start_line: 3, end_line: 4, file: None }),
    }
}

/// (fixture name, result value) for each sideshow result shape.
fn responses() -> Vec<(&'static str, Value)> {
    vec![
        ("session-new", v(session())),
        ("session-list", v(vec![session()])),
        ("session-search", v(vec![SessionHit { session: session(), post: Some("p1".into()), text: Some("the redirect loops".into()) }])),
        ("session-delete", v(SessionDeleteResult { id: "s1".into(), trashed: true, purged: false, already: false })),
        ("session-restore", v(SessionRestoreResult { id: "s1".into(), restored: true, already: false })),
        ("post-new", v(PostWriteResult { post: summary(), feedback: vec![comment()] })),
        ("post-list", v(vec![summary()])),
        ("post-show", v(Post { id: "p1".into(), session: "s1".into(), title: "Plan".into(), parts: vec![md("# Plan")], version: 2, created_at: 1_761_400_000, updated_at: 1_761_400_100 })),
        ("post-render", v(PostRender {
            id: "p1".into(),
            version: 2,
            parts: vec![
                PartRender::Native,
                PartRender::Code { language: Some("rust".into()), line_start: 1, lines: vec![vec![Span { text: "fn".into(), class: Some("keyword".into()) }]] },
            ],
        })),
        ("comment-new", v(comment())),
        ("comment-list", v(vec![comment()])),
        ("asset-put", v(AssetInfo { id: "2cf24dba".into(), kind: AssetKind::Image, content_type: "image/png".into(), byte_length: 5, filename: Some("h.png".into()), created_at: 1_761_400_000 })),
        ("trace-list", v(vec![TraceStep { label: "ran tests".into(), kind: Some("tool".into()), detail: None, ts: None }])),
    ]
}

fn v(result: impl serde::Serialize) -> Value {
    serde_json::to_value(result).expect("result serializes")
}

fn check(file: &str, line: &str) {
    let path = fixtures().join(file);
    if std::env::var_os("UPDATE_FIXTURES").is_some() {
        fs::create_dir_all(fixtures()).unwrap();
        fs::write(&path, line).unwrap();
        return;
    }
    let golden = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e} (regenerate with UPDATE_FIXTURES=1)", path.display()));
    // Compared as JSON: key order isn't part of the contract (and gpui, elsewhere
    // in the workspace, turns on serde_json's preserve_order).
    let parse = |s: &str| serde_json::from_str::<Value>(s).expect("fixture is JSON");
    assert_eq!(parse(line), parse(&golden), "{file} changed: the wire contract moved");
}

#[test]
fn every_verb_has_a_request_fixture_that_round_trips() {
    let samples = samples();
    assert_eq!(samples.len(), VERB_COUNT, "one sample per verb");
    let mut names: Vec<_> = samples.iter().map(name).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), VERB_COUNT, "no verb sampled twice");
    for verb in samples {
        let request = Request { v: PROTOCOL_VERSION, id: 7, verb: verb.clone(), cwd: None, agent: Some("claude".into()) };
        let line = to_line(&request);
        check(&format!("{}.request.json", name(&verb)), &line);
        let back: Request = serde_json::from_str(&line).unwrap();
        assert_eq!(back, request, "{}", name(&verb));
    }
}

#[test]
fn sideshow_responses_round_trip() {
    for (verb, result) in responses() {
        let line = to_line(&Response::ok(7, &result));
        check(&format!("{verb}.response.json"), &line);
        let back: Response = serde_json::from_str(&line).unwrap();
        assert_eq!(back, Response::ok(7, &result));
    }
}

#[test]
fn unknown_fields_in_args_and_parts_are_rejected() {
    for (bad, field) in [
        (json!({"v":1,"id":1,"verb":"post-list","args":{"sesion":"s1"}}), "sesion"),
        (json!({"v":1,"id":1,"verb":"post-new","args":{"parts":[{"kind":"markdown","markdown":"x","colour":"red"}]}}), "colour"),
        (json!({"v":1,"id":1,"verb":"comment-new","args":{"text":"x","anchor":{"kind":"point","part":0,"post_version":1,"x":0.5,"y":0.5,"z":1}}}), "z"),
        (json!({"v":1,"id":1,"verb":"edit","args":{"note":"n","edits":[{"old":"a","new":"b","every":true}]}}), "every"),
    ] {
        let e = serde_json::from_value::<Request>(bad).unwrap_err().to_string();
        assert!(e.contains(field), "{field}: {e}");
    }
}

/// stage ↔ stage-helper: one fixture per request and reply shape in fixtures/helper/.
/// Requests round-trip and reject unknown fields; the Swift mirror decodes the same files.
mod helper {
    use super::*;
    use notes_proto::helper::*;

    fn read(file: &str) -> String {
        let path = fixtures().join("helper").join(file);
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    fn files(suffix: &str) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(fixtures().join("helper"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.ends_with(suffix))
            .collect();
        names.sort();
        names
    }

    fn json(s: &str) -> Value {
        serde_json::from_str(s).expect("fixture is JSON")
    }

    /// One sample per `HelperRequest` variant; exhaustive so a new op needs a fixture.
    fn request_name(req: &HelperRequest) -> &'static str {
        match req {
            HelperRequest::Warm { .. } => "warm",
            HelperRequest::Mermaid { .. } => "mermaid",
            HelperRequest::Image { .. } => "image",
            HelperRequest::HtmlOpen { .. } => "html-open",
            HelperRequest::HtmlUpdate { .. } => "html-update",
            HelperRequest::HtmlAck { .. } => "html-ack",
            HelperRequest::HtmlPause { .. } => "html-pause",
            HelperRequest::HtmlResume { .. } => "html-resume",
            HelperRequest::HtmlClose { .. } => "html-close",
            HelperRequest::HtmlResize { .. } => "html-resize",
        }
    }

    #[test]
    fn requests_round_trip_and_reject_unknown_fields() {
        let names = files(".request.json");
        assert_eq!(names.len(), 10, "one fixture per op");
        let mut seen = Vec::new();
        for file in names {
            let line = read(&file);
            let req: HelperRequest = serde_json::from_str(&line).unwrap_or_else(|e| panic!("{file}: {e}"));
            assert_eq!(format!("{}.request.json", request_name(&req)), file);
            assert_eq!(serde_json::to_value(&req).unwrap(), json(&line), "{file}");
            seen.push(request_name(&req));
            let mut extra = json(&line);
            extra["bogus"] = json!(1);
            assert!(serde_json::from_value::<HelperRequest>(extra).is_err(), "{file} accepted an unknown field");
            let mut missing = json(&line);
            missing.as_object_mut().unwrap().remove("id");
            assert!(serde_json::from_value::<HelperRequest>(missing).is_err(), "{file} accepted no id");
        }
        seen.dedup();
        assert_eq!(seen.len(), 10);
        assert!(serde_json::from_str::<HelperRequest>(r#"{"op":"nope","id":1}"#).is_err());
    }

    #[test]
    fn replies_round_trip_into_their_variant() {
        let expect = |file: &str, reply: &HelperReply| {
            let ok = match reply {
                HelperReply::Frame(_) => "frame",
                HelperReply::Failed(f) if f.id.is_none() => "failed-no-id",
                HelperReply::Failed(_) => "failed",
                HelperReply::Mermaid(_) => "mermaid",
                HelperReply::Patched(_) => "patched",
                HelperReply::Image(_) => "image",
                HelperReply::Done(_) => "done",
            };
            assert_eq!(format!("{ok}.reply.json"), file);
        };
        let names = files(".reply.json");
        assert_eq!(names.len(), 7);
        for file in names {
            let line = read(&file);
            let reply: HelperReply = serde_json::from_str(&line).unwrap_or_else(|e| panic!("{file}: {e}"));
            expect(&file, &reply);
            assert_eq!(serde_json::to_value(&reply).unwrap(), json(&line), "{file}");
        }
        for bad in [r#"{"id":1,"ok":false,"svg":"","width":1.0,"height":1.0}"#, r#"{"id":1,"ok":true,"error":"x"}"#] {
            assert!(serde_json::from_str::<HelperReply>(bad).is_err(), "{bad}");
        }
    }
}
