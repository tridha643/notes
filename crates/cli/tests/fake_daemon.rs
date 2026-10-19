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
    request: String,
    cwd: PathBuf,
    out: Output,
}

/// Serve one connection: read its request line, reply with `responses`, close.
fn notes(args: &[&str], stdin: &str, responses: &[&str]) -> Run {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!("notes-cli-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    let sock = dir.join("d.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let responses: Vec<String> = responses.iter().map(|r| format!("{r}\n")).collect();
    let daemon: JoinHandle<String> = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(&stream).read_line(&mut request).unwrap();
        for response in responses {
            (&stream).write_all(response.as_bytes()).unwrap();
        }
        request
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_notes"))
        .args(args)
        .arg("--sock")
        .arg(&sock)
        .current_dir(&dir)
        .env("NOTES_AGENT", "tester")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let request = daemon.join().unwrap();
    fs::remove_dir_all(&dir).unwrap();
    Run { request, cwd: dir, out }
}

fn stdout(run: &Run) -> &str {
    std::str::from_utf8(&run.out.stdout).unwrap()
}

fn stderr(run: &Run) -> &str {
    std::str::from_utf8(&run.out.stderr).unwrap()
}

#[test]
fn ok_response_prints_text_and_exits_zero() {
    let run = notes(&["edit", "work/plan", "--old", "foo", "--new", "bar"], "", &[r#"{"id":1,"ok":true,"result":{"path":"work/plan.md","version":43}}"#]);
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
    let run = notes(&["append", "plan", "-", "--under", "Todo"], "- milk\n", &[r#"{"id":1,"ok":true,"result":{"path":"plan.md","version":2}}"#]);
    assert!(run.request.contains(r#""verb":"append","args":{"note":"plan","text":"- milk\n","under":"Todo"}"#), "{}", run.request);

    let hits = r#"{"id":1,"ok":true,"result":[{"path":"a.md","line":3,"text":"todo x"},{"path":"b.md","line":1,"text":"todo y","after":["next"]}]}"#;
    let text = notes(&["search", "todo"], "", &[hits]);
    assert_eq!(stdout(&text), "a.md:3: todo x\nb.md:1: todo y\nb.md-2- next\n");
    let json = notes(&["search", "todo", "--json"], "", &[hits]);
    assert_eq!(stdout(&json), "{\"path\":\"a.md\",\"line\":3,\"text\":\"todo x\"}\n{\"path\":\"b.md\",\"line\":1,\"text\":\"todo y\",\"after\":[\"next\"]}\n");

    let note = r#"{"id":1,"ok":true,"result":{"path":"p.md","title":"p","version":1,"body":"b\nc","line_count":3,"created_at":0,"updated_at":0}}"#;
    let read = notes(&["read", "p", "--lines", "2:3", "-n"], "", &[note]);
    assert!(read.request.contains(r#""args":{"note":"p","lines":{"start":2,"end":3}}"#), "{}", read.request);
    assert_eq!(stdout(&read), "     2\tb\n     3\tc\n");
}

#[test]
fn error_response_goes_to_stderr_with_hint_and_exit_code() {
    let error = r#"{"id":1,"ok":false,"error":{"code":"ambiguous","message":"\"foo\" matches 3 times (lines 4, 9, 30)","hint":"add surrounding text or pass --all"}}"#;
    let run = notes(&["edit", "plan", "--old", "foo", "--new", "bar"], "", &[error]);
    assert_eq!(stdout(&run), "");
    assert_eq!(stderr(&run), "notes: \"foo\" matches 3 times (lines 4, 9, 30)\nhint: add surrounding text or pass --all\n");
    assert_eq!(run.out.status.code(), Some(4));

    let conflict = r#"{"id":1,"ok":false,"error":{"code":"version_conflict","message":"plan.md is at version 9"}}"#;
    let run = notes(&["write", "plan", "-", "--if-version", "8", "--json"], "body", &[conflict]);
    assert_eq!(stdout(&run), "{\"code\":\"version_conflict\",\"message\":\"plan.md is at version 9\"}\n");
    assert_eq!(stderr(&run), "notes: plan.md is at version 9\nhint: run `notes read --json <ref>` for the current version, then retry\n");
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
    assert!(run.request.starts_with(r#"{"v":1,"id":1,"verb":"watch","args":{"since":3,"kinds":["note"]},"cwd":"#), "{}", run.request);
    assert_eq!(
        stdout(&run),
        "{\"seq\":4,\"kind\":\"note-updated\",\"path\":\"a.md\",\"version\":2}\n{\"seq\":5,\"kind\":\"note-created\",\"path\":\"b.md\"}\n"
    );
    assert_eq!(run.out.status.code(), Some(0));
}

#[test]
fn batch_sends_stdin_ops_and_prints_one_line_per_result() {
    let ops = "{\"verb\":\"mkdir\",\"args\":{\"folder\":\"w\"}}\n{\"verb\":\"new\",\"args\":{\"path\":\"w/a\"}}\n";
    let run = notes(&["batch", "--atomic"], ops, &[r#"{"id":1,"ok":true,"result":[null,{"path":"w/a.md","version":1}]}"#]);
    assert!(
        run.request.contains(r#""verb":"batch","args":{"ops":[{"verb":"mkdir","args":{"folder":"w"}},{"verb":"new","args":{"path":"w/a","body":""}}],"atomic":true}"#),
        "{}",
        run.request
    );
    assert_eq!(stdout(&run), "null\n{\"path\":\"w/a.md\",\"version\":1}\n");
}

#[test]
fn closed_connection_without_response_is_an_error() {
    let run = notes(&["doctor"], "", &[]);
    assert_eq!(run.request, format!(r#"{{"v":1,"id":1,"verb":"doctor","cwd":"{}","agent":"tester"}}"#, run.cwd.display()) + "\n");
    assert_eq!(run.out.status.code(), Some(1));
}

#[test]
fn unreachable_socket_exits_6() {
    let out = Command::new(env!("CARGO_BIN_EXE_notes")).args(["doctor", "--sock", "/nonexistent/notesd.sock"]).output().unwrap();
    assert_eq!(out.status.code(), Some(6));
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.starts_with("notes: notesd unreachable at /nonexistent/notesd.sock: "), "{stderr}");
    assert!(stderr.ends_with("\nhint: start notesd, or point --sock / NOTES_SOCK at its socket\n"), "{stderr}");
}
