//! Runs the `notes` binary against a fake `notesd` on a temp unix socket and
//! checks the exact request line, stdout, stderr and exit code.

use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixListener,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    thread::{self, JoinHandle},
};

struct Run {
    /// The last request line; `requests` has every connection's, in order.
    request: String,
    requests: Vec<String>,
    cwd: PathBuf,
    out: Output,
}

/// Serve one connection: read its request line, reply with `responses`, close.
fn notes(args: &[&str], stdin: &str, responses: &[&str]) -> Run {
    serve(args, stdin, &[responses])
}

/// A file outside the run's cwd for verbs that read one (image, trace, upload).
fn temp_file(name: &str, bytes: &[u8]) -> PathBuf {
    let path = std::env::temp_dir().join(format!("notes-cli-{}-{name}", std::process::id()));
    fs::write(&path, bytes).unwrap();
    path
}

/// Serve one connection per `conns` entry, each replying with its responses.
fn serve(args: &[&str], stdin: &str, conns: &[&[&str]]) -> Run {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "notes-cli-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    let sock = dir.join("d.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let conns: Vec<Vec<String>> = conns
        .iter()
        .map(|responses| responses.iter().map(|r| format!("{r}\n")).collect())
        .collect();
    let daemon: JoinHandle<Vec<String>> = thread::spawn(move || {
        conns
            .into_iter()
            .map(|responses| {
                // `notes post` first asks for stage's card width; no stage has run here.
                let (stream, request) = loop {
                    let (stream, _) = listener.accept().unwrap();
                    let mut request = String::new();
                    BufReader::new(&stream).read_line(&mut request).unwrap();
                    if !request.contains(r#""verb":"setting-get""#) {
                        break (stream, request);
                    }
                    (&stream).write_all(format!("{}\n", ok("null")).as_bytes()).unwrap();
                };
                for response in responses {
                    (&stream).write_all(response.as_bytes()).unwrap();
                }
                request
            })
            .collect()
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_notes"))
        .args(args)
        .arg("--sock")
        .arg(&sock)
        .current_dir(&dir)
        .env("NOTES_AGENT", "tester")
        .env("NOTES_OWNER", "")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let requests = daemon.join().unwrap();
    fs::remove_dir_all(&dir).unwrap();
    Run {
        request: requests.last().cloned().unwrap_or_default(),
        requests,
        cwd: dir,
        out,
    }
}

fn stdout(run: &Run) -> &str {
    std::str::from_utf8(&run.out.stdout).unwrap()
}

fn stderr(run: &Run) -> &str {
    std::str::from_utf8(&run.out.stderr).unwrap()
}

#[test]
fn ok_response_prints_text_and_exits_zero() {
    let run = notes(
        &["edit", "work/plan", "--old", "foo", "--new", "bar"],
        "",
        &[r#"{"id":1,"ok":true,"result":{"path":"work/plan.md","version":43}}"#],
    );
    assert_eq!(
        run.request,
        format!(
            r#"{{"v":1,"id":1,"verb":"edit","args":{{"note":"work/plan","edits":[{{"old":"foo","new":"bar","all":false}}]}},"cwd":"{}","agent":"tester"}}"#,
            run.cwd.display()
        ) + "\n"
    );
    assert_eq!(stdout(&run), "work/plan.md v43\n");
    assert_eq!(stderr(&run), "");
    assert_eq!(run.out.status.code(), Some(0));
}

#[test]
fn stdin_text_search_and_read_output() {
    let run = notes(
        &["append", "plan", "-", "--under", "Todo"],
        "- milk\n",
        &[r#"{"id":1,"ok":true,"result":{"path":"plan.md","version":2}}"#],
    );
    assert!(
        run.request
            .contains(r#""verb":"append","args":{"note":"plan","text":"- milk\n","under":"Todo"}"#),
        "{}",
        run.request
    );

    let hits = r#"{"id":1,"ok":true,"result":[{"path":"a.md","line":3,"text":"todo x"},{"path":"b.md","line":1,"text":"todo y","after":["next"]}]}"#;
    let text = notes(&["search", "todo"], "", &[hits]);
    assert_eq!(
        stdout(&text),
        "a.md:3: todo x\nb.md:1: todo y\nb.md-2- next\n"
    );
    let json = notes(&["search", "todo", "--json"], "", &[hits]);
    assert_eq!(stdout(&json), "{\"path\":\"a.md\",\"line\":3,\"text\":\"todo x\"}\n{\"path\":\"b.md\",\"line\":1,\"text\":\"todo y\",\"after\":[\"next\"]}\n");

    let note = r#"{"id":1,"ok":true,"result":{"path":"p.md","title":"p","version":1,"body":"b\nc","line_count":3,"created_at":0,"updated_at":0}}"#;
    let read = notes(&["read", "p", "--lines", "2:3", "-n"], "", &[note]);
    assert!(
        read.request
            .contains(r#""args":{"note":"p","lines":{"start":2,"end":3}}"#),
        "{}",
        read.request
    );
    assert_eq!(stdout(&read), "     2\tb\n     3\tc\n");
}

#[test]
fn error_response_goes_to_stderr_with_hint_and_exit_code() {
    let error = r#"{"id":1,"ok":false,"error":{"code":"ambiguous","message":"\"foo\" matches 3 times (lines 4, 9, 30)","hint":"add surrounding text or pass --all"}}"#;
    let run = notes(
        &["edit", "plan", "--old", "foo", "--new", "bar"],
        "",
        &[error],
    );
    assert_eq!(stdout(&run), "");
    assert_eq!(stderr(&run), "notes: \"foo\" matches 3 times (lines 4, 9, 30)\nhint: add surrounding text or pass --all\n");
    assert_eq!(run.out.status.code(), Some(4));

    let conflict = r#"{"code":"version_conflict","message":"plan.md is at version 9, not 8","details":{"current_version":9}}"#;
    let run = notes(
        &["write", "plan", "-", "--if-version", "8", "--json"],
        "body",
        &[&format!(r#"{{"id":1,"ok":false,"error":{conflict}}}"#)],
    );
    assert_eq!(stdout(&run), format!("{conflict}\n"));
    assert_eq!(stderr(&run), "notes: plan.md is at version 9, not 8\nhint: current version is 9; re-read the note, or retry with --if-version 9\n");
    assert_eq!(run.out.status.code(), Some(5));
}

#[test]
fn watch_streams_events_as_ndjson() {
    let run = notes(
        &["watch", "--since", "3", "--kind", "note"],
        "",
        &[
            r#"{"id":1,"event":{"seq":4,"kind":"note-updated","path":"a.md","version":2}}"#,
            r#"{"id":1,"event":{"seq":5,"kind":"note-created","path":"b.md"}}"#,
        ],
    );
    assert!(
        run.request.starts_with(
            r#"{"v":1,"id":1,"verb":"watch","args":{"since":3,"kinds":["note"]},"cwd":"#
        ),
        "{}",
        run.request
    );
    assert_eq!(
        stdout(&run),
        "{\"seq\":4,\"kind\":\"note-updated\",\"path\":\"a.md\",\"version\":2}\n{\"seq\":5,\"kind\":\"note-created\",\"path\":\"b.md\"}\n"
    );
    assert_eq!(run.out.status.code(), Some(0));
}

#[test]
fn batch_sends_stdin_ops_and_prints_one_line_per_result() {
    let ops = "{\"verb\":\"mkdir\",\"args\":{\"folder\":\"w\"}}\n{\"verb\":\"new\",\"args\":{\"path\":\"w/a\"}}\n";
    let outcomes = [
        r#"{"ok":true,"result":null}"#,
        r#"{"ok":false,"error":{"code":"not_found","message":"w missing"}}"#,
    ];
    let run = notes(
        &["batch", "--atomic"],
        ops,
        &[&format!(
            r#"{{"id":1,"ok":true,"result":[{}]}}"#,
            outcomes.join(",")
        )],
    );
    assert!(
        run.request.contains(r#""verb":"batch","args":{"ops":[{"verb":"mkdir","args":{"folder":"w"}},{"verb":"new","args":{"path":"w/a","body":""}}],"atomic":true}"#),
        "{}",
        run.request
    );
    assert_eq!(stdout(&run), format!("{}\n{}\n", outcomes[0], outcomes[1]));
    // An atomic batch stops at its error, which is the last outcome.
    assert_eq!(
        stderr(&run),
        "notes: batch op 2: w missing\nhint: run `notes find <query>` to look up the path\n"
    );
    assert_eq!(run.out.status.code(), Some(3));
}

#[test]
fn batch_prints_every_op_then_exits_with_first_failure() {
    let ops = "{\"verb\":\"doctor\"}\n{\"verb\":\"doctor\"}\n{\"verb\":\"doctor\"}\n";
    let outcomes = [
        r#"{"ok":true,"result":null}"#,
        r#"{"ok":false,"error":{"code":"ambiguous","message":"x matches 2 notes","hint":"use the full path"}}"#,
        r#"{"ok":false,"error":{"code":"not_found","message":"y missing"}}"#,
    ];
    let run = notes(
        &["batch"],
        ops,
        &[&format!(
            r#"{{"id":1,"ok":true,"result":[{}]}}"#,
            outcomes.join(",")
        )],
    );
    assert_eq!(
        stdout(&run),
        format!("{}\n{}\n{}\n", outcomes[0], outcomes[1], outcomes[2])
    );
    assert_eq!(
        stderr(&run),
        "notes: batch op 2: x matches 2 notes\nhint: use the full path\n"
    );
    assert_eq!(run.out.status.code(), Some(4));

    let ok = notes(
        &["batch", "-q"],
        "{\"verb\":\"doctor\"}\n",
        &[r#"{"id":1,"ok":true,"result":[{"ok":true,"result":null}]}"#],
    );
    assert_eq!(
        (stdout(&ok), stderr(&ok), ok.out.status.code()),
        ("", "", Some(0))
    );
}

#[test]
fn search_fuzzy_flag_goes_on_the_wire() {
    let run = notes(
        &["search", "tood", "--fuzzy"],
        "",
        &[r#"{"id":1,"ok":true,"result":[]}"#],
    );
    assert!(run.request.contains(r#""verb":"search","args":{"query":"tood","context":0,"case_sensitive":false,"fuzzy":true}"#), "{}", run.request);
    let run = notes(
        &["search", "todo"],
        "",
        &[r#"{"id":1,"ok":true,"result":[]}"#],
    );
    assert!(run.request.contains(r#""fuzzy":false"#), "{}", run.request);
}

#[test]
fn agent_howto_and_completions_work_without_a_daemon() {
    let howto = Command::new(env!("CARGO_BIN_EXE_notes"))
        .args(["agent-howto", "--sock", "/nonexistent/notesd.sock"])
        .output()
        .unwrap();
    assert_eq!(howto.status.code(), Some(0));
    let text = String::from_utf8(howto.stdout).unwrap();
    assert!(
        text.starts_with("## Posting to stage (`notes post`)\n")
            && text.contains("notes wait --timeout 1")
            && text.contains("notes edit <ref> --old S --new S"),
        "{text}"
    );

    for (shell, marker) in [
        ("zsh", "#compdef notes"),
        ("bash", "_notes()"),
        ("fish", "complete -c notes"),
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_notes"))
            .args(["completions", shell])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "{shell}");
        assert!(
            String::from_utf8(out.stdout).unwrap().contains(marker),
            "{shell}"
        );
    }
}

#[test]
fn list_prints_folders_then_notes() {
    let entries = [
        r#"{"folder":"work"}"#,
        r#"{"path":"a.md","title":"a","version":1,"updated_at":0}"#,
    ];
    let response = format!(r#"{{"id":1,"ok":true,"result":[{}]}}"#, entries.join(","));
    assert_eq!(stdout(&notes(&["list"], "", &[&response])), "work/\na.md\n");
    assert_eq!(
        stdout(&notes(&["list", "--json"], "", &[&response])),
        format!("{}\n{}\n", entries[0], entries[1])
    );
}

#[test]
fn closed_connection_without_response_is_an_error() {
    let run = notes(&["doctor"], "", &[]);
    assert_eq!(
        run.request,
        format!(
            r#"{{"v":1,"id":1,"verb":"doctor","cwd":"{}","agent":"tester"}}"#,
            run.cwd.display()
        ) + "\n"
    );
    assert_eq!(run.out.status.code(), Some(1));
}

#[test]
fn unreachable_socket_exits_6() {
    let out = Command::new(env!("CARGO_BIN_EXE_notes"))
        .args(["doctor", "--sock", "/nonexistent/notesd.sock"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(6));
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.starts_with("notes: notesd unreachable at /nonexistent/notesd.sock: "),
        "{stderr}"
    );
    assert!(
        stderr.contains("\nhint: no socket, so notesd isn't running: start `notesd`"),
        "{stderr}"
    );

    // A socket file nobody listens on (notesd died) gets the stale-socket hint.
    let sock = std::env::temp_dir().join(format!("notes-cli-stale-{}.sock", std::process::id()));
    let _ = fs::remove_file(&sock);
    drop(UnixListener::bind(&sock).unwrap());
    let out = Command::new(env!("CARGO_BIN_EXE_notes"))
        .arg("doctor")
        .arg("--sock")
        .arg(&sock)
        .output()
        .unwrap();
    fs::remove_file(&sock).unwrap();
    assert_eq!(out.status.code(), Some(6));
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("\nhint: stale socket, nothing is listening"),
        "{stderr}"
    );
}

/// The full request line the CLI sends for `verb_and_args` from the run's cwd.
fn line(run: &Run, verb_and_args: &str) -> String {
    format!(
        r#"{{"v":1,"id":1,{verb_and_args},"cwd":"{}","agent":"tester"}}"#,
        run.cwd.display()
    ) + "\n"
}

fn ok(result: &str) -> String {
    format!(r#"{{"id":1,"ok":true,"result":{result}}}"#)
}

const POST_WRITE: &str = r#"{"post":{"id":"p1","session":"s1","title":"Plan","kinds":["markdown"],"version":1,"updated_at":0}}"#;

#[test]
fn post_prints_id_and_user_feedback_on_stderr() {
    let result = r#"{"post":{"id":"p1","session":"s1","title":"Plan","kinds":["markdown"],"version":1,"updated_at":0},"feedback":[{"id":"c7","seq":7,"session":"s1","post":"p1","author":"user","text":"looks\n  good","created_at":0}]}"#;
    let run = notes(
        &["post", "markdown", "-", "--title", "Plan"],
        "# Hi\n",
        &[&ok(result)],
    );
    assert_eq!(
        run.request,
        line(
            &run,
            r##""verb":"post-new","args":{"title":"Plan","parts":[{"kind":"markdown","markdown":"# Hi\n"}]}"##
        )
    );
    assert_eq!(stdout(&run), "p1 v1 · session s1 · markdown\n");
    assert_eq!(stderr(&run), "feedback: #7 user on post p1: looks good\n");
    assert_eq!(run.out.status.code(), Some(0));

    // Feedback is handed out once, so -q doesn't swallow it.
    let quiet = notes(&["post", "markdown", "-", "-q"], "x", &[&ok(result)]);
    assert_eq!(stdout(&quiet), "");
    assert_eq!(stderr(&quiet), "feedback: #7 user on post p1: looks good\n");
}

#[test]
fn post_image_uploads_the_asset_then_posts_it() {
    let file = temp_file("shot.png", b"\x89PNG");
    let name = file.file_name().unwrap().to_str().unwrap().to_owned();
    let asset =
        r#"{"id":"sha1","kind":"image","content_type":"image/png","byte_length":4,"created_at":0}"#;
    let run = serve(
        &[
            "post",
            "image",
            file.to_str().unwrap(),
            "--caption",
            "C",
            "--session",
            "s1",
        ],
        "",
        &[&[&ok(asset)], &[&ok(POST_WRITE)]],
    );
    fs::remove_file(&file).unwrap();
    assert_eq!(
        run.requests[0],
        line(
            &run,
            &format!(
                r#""verb":"asset-put","args":{{"session":"s1","kind":"image","content_type":"image/png","filename":"{name}","data_base64":"iVBORw=="}}"#
            )
        )
    );
    assert_eq!(
        run.requests[1],
        line(
            &run,
            r#""verb":"post-new","args":{"session":"s1","parts":[{"kind":"image","asset_id":"sha1","caption":"C"}]}"#
        )
    );
    assert_eq!(stdout(&run), "p1 v1 · session s1 · markdown\n");
}

#[test]
fn post_update_code_infers_language_and_title() {
    let file = temp_file("x.rs", b"fn main() {}\n");
    let name = file.file_name().unwrap().to_str().unwrap().to_owned();
    let run = notes(
        &[
            "post",
            "update",
            "p1",
            "code",
            file.to_str().unwrap(),
            "--line-start",
            "3",
        ],
        "",
        &[&ok(POST_WRITE)],
    );
    fs::remove_file(&file).unwrap();
    assert_eq!(
        run.request,
        line(
            &run,
            &format!(
                r#""verb":"post-update","args":{{"id":"p1","parts":[{{"kind":"code","code":"fn main() {{}}\n","language":"rust","title":"{name}","line_start":3}}]}}"#
            )
        )
    );
}

#[test]
fn comment_line_anchor_pins_the_current_post_version() {
    let post = r#"{"id":"p1","session":"s1","title":"t","parts":[],"version":3,"created_at":0,"updated_at":0}"#;
    let comment = r#"{"id":"c8","seq":8,"session":"s1","post":"p1","author":"tester","text":"fix this","created_at":0}"#;
    let run = serve(
        &["comment", "p1", "fix this", "--line", "4:9"],
        "",
        &[&[&ok(post)], &[&ok(comment)]],
    );
    assert_eq!(
        run.requests[0],
        line(&run, r#""verb":"post-show","args":{"id":"p1"}"#)
    );
    assert_eq!(
        run.requests[1],
        line(
            &run,
            r#""verb":"comment-new","args":{"post":"p1","text":"fix this","anchor":{"kind":"line-range","part":0,"post_version":3,"start_line":4,"end_line":9}}"#
        )
    );
    assert_eq!(stdout(&run), "#8 tester on post p1: fix this\n");

    // A failed lookup stops before the comment is sent.
    let missing = r#"{"id":1,"ok":false,"error":{"code":"not_found","message":"no post p9"}}"#;
    let run = notes(&["comment", "p9", "x", "--line", "1:1"], "", &[missing]);
    assert_eq!(run.requests.len(), 1);
    assert_eq!(run.out.status.code(), Some(3));
}

#[test]
fn sideshow_verbs_send_exact_requests_and_print_text() {
    let upload = temp_file("notes.txt", b"hi");
    let upload_name = upload.file_name().unwrap().to_str().unwrap().to_owned();
    let comment = |seq: u32, text: &str| {
        format!(
            r#"{{"id":"c{seq}","seq":{seq},"session":"s1","post":"p1","author":"user","text":"{text}","created_at":0}}"#
        )
    };
    let cases: Vec<(Vec<&str>, &str, String, String, String)> = vec![
        (vec!["wait", "--post", "p1", "--timeout", "5"], "", r#""verb":"wait","args":{"post":"p1","timeout_secs":5}"#.into(), comment(9, "ship it"), "#9 user on post p1: ship it\n".into()),
        (vec!["wait"], "", r#""verb":"wait","args":{"timeout_secs":120}"#.into(), "null".into(), "".into()),
        (
            vec!["comments", "--session", "s1", "--after", "3"],
            "",
            r#""verb":"comment-list","args":{"session":"s1","after_seq":3}"#.into(),
            format!("[{},{}]", comment(4, "a"), comment(5, "b")),
            "#4 user on post p1: a\n#5 user on post p1: b\n".into(),
        ),
        (
            vec!["sessions"],
            "",
            r#""verb":"session-list""#.into(),
            r#"[{"id":"s1","agent":"claude","title":"Fix","created_at":0,"last_active_at":0,"post_count":2}]"#.into(),
            "s1\tclaude\t2 posts\t1970-01-01 00:00Z\tFix\n".into(),
        ),
        (vec!["session", "rename", "s1", "New name"], "", r#""verb":"session-rename","args":{"id":"s1","title":"New name"}"#.into(), "null".into(), "".into()),
        (vec!["session", "delete", "s1"], "", r#""verb":"session-delete","args":{"id":"s1"}"#.into(), "null".into(), "".into()),
        (vec!["kits"], "", r#""verb":"kits""#.into(), r#"[{"id":"slides","label":"Slides","summary":"Deck","css":""}]"#.into(), "slides\tSlides: Deck\n".into()),
        (
            vec!["trace", "-", "--session", "s1"],
            "{\"label\":\"read\"}\n{\"label\":\"edit\",\"kind\":\"tool\"}\n",
            r#""verb":"trace-set","args":{"session":"s1","steps":[{"label":"read"},{"label":"edit","kind":"tool"}]}"#.into(),
            "null".into(),
            "".into(),
        ),
        (
            vec!["post", "list", "--session", "s1"],
            "",
            r#""verb":"post-list","args":{"session":"s1"}"#.into(),
            r#"[{"id":"p1","session":"s1","title":"Plan","kinds":["markdown","code"],"version":2,"updated_at":0}]"#.into(),
            "p1\tv2\tmarkdown,code\tPlan\n".into(),
        ),
        (
            vec!["post", "show", "p1", "--version", "2"],
            "",
            r#""verb":"post-show","args":{"id":"p1","version":2}"#.into(),
            r#"{"id":"p1","session":"s1","title":"Plan","parts":[{"kind":"markdown","markdown":"x","id":"intro"},{"kind":"code","code":"y"}],"version":2,"created_at":0,"updated_at":0}"#.into(),
            "p1\tv2\tPlan\n  0\tmarkdown\tintro\n  1\tcode\t-\n".into(),
        ),
        (vec!["post", "delete", "p1"], "", r#""verb":"post-delete","args":{"id":"p1"}"#.into(), "null".into(), "".into()),
        (vec!["post", "part", "p1", "rm", "1"], "", r#""verb":"post-part","args":{"id":"p1","op":"rm","target":"1"}"#.into(), POST_WRITE.into(), "p1 v1 · session s1 · markdown\n".into()),
        (
            vec!["post", "part", "p1", "set", "0", "terminal", "-", "--cols", "80", "--term-title", "cargo"],
            "\u{1b}[31mred\u{1b}[0m",
            r#""verb":"post-part","args":{"id":"p1","op":"set","target":"0","part":{"kind":"terminal","text":"\u001b[31mred\u001b[0m","cols":80,"title":"cargo"}}"#.into(),
            POST_WRITE.into(),
            "p1 v1 · session s1 · markdown\n".into(),
        ),
        (
            vec!["post", "html", "-", "--kit", "slides,issues"],
            "<h1>x</h1>",
            r#""verb":"post-new","args":{"parts":[{"kind":"html","html":"<h1>x</h1>","kits":["slides","issues"]}]}"#.into(),
            POST_WRITE.into(),
            "p1 v1 · session s1 · markdown\n".into(),
        ),
        (
            vec!["post", "json", "-", "--title", "Data"],
            r#"{"a":[1,null]}"#,
            r#""verb":"post-new","args":{"title":"Data","parts":[{"kind":"json","data":{"a":[1,null]}}]}"#.into(),
            POST_WRITE.into(),
            "p1 v1 · session s1 · markdown\n".into(),
        ),
        (
            vec!["upload", upload.to_str().unwrap()],
            "",
            format!(r#""verb":"asset-put","args":{{"kind":"file","content_type":"text/plain","filename":"{upload_name}","data_base64":"aGk="}}"#),
            r#"{"id":"sha2","kind":"file","content_type":"text/plain","byte_length":2,"created_at":0}"#.into(),
            "sha2\n".into(),
        ),
    ];
    for (args, stdin, request, result, text) in cases {
        let run = notes(&args, stdin, &[&ok(&result)]);
        assert_eq!(run.request, line(&run, &request), "{args:?}");
        assert_eq!(stdout(&run), text, "{args:?}");
        assert_eq!(
            (stderr(&run), run.out.status.code()),
            ("", Some(0)),
            "{args:?}"
        );
    }
    fs::remove_file(&upload).unwrap();
}
