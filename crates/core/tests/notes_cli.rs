use std::process::{Command, Output, Stdio};

fn run_notes(db: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_notes-m1"))
        .env("NOTES_DB", db)
        .args(args)
        .output()
        .unwrap()
}

fn successful_stdout(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn cli_persists_create_list_show_and_append_across_processes() {
    let directory = tempfile::tempdir().unwrap();
    let db = directory.path().join("nested/notes.db");
    assert_eq!(successful_stdout(run_notes(&db, &["list"])), "");
    let id = successful_stdout(run_notes(&db, &["new", "Today's 🦀", "--body", "你好"]));
    let id = id.trim();
    assert_eq!(
        successful_stdout(run_notes(&db, &["list"])),
        format!("{id}\tToday's 🦀\n")
    );
    assert_eq!(
        successful_stdout(run_notes(&db, &["append", id, "\nWorld"])),
        format!("{id}\n")
    );
    assert_eq!(
        successful_stdout(run_notes(&db, &["show", id])),
        "Today's 🦀\n\n你好\nWorld\n"
    );
    let empty_id = successful_stdout(run_notes(&db, &["new", "Empty"]));
    assert_eq!(
        successful_stdout(run_notes(&db, &["show", empty_id.trim()])),
        "Empty\n\n\n"
    );
}

#[test]
fn cli_reports_missing_notes_and_invalid_arguments() {
    let directory = tempfile::tempdir().unwrap();
    let db = directory.path().join("notes.db");
    for args in [vec!["show", "999"], vec!["append", "999", "text"]] {
        let output = run_notes(&db, &args);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("Note not found: 999"));
    }
    let output = run_notes(&db, &["show", "invalid"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn cli_uses_default_database_under_home() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_notes-m1"))
        .env_remove("NOTES_DB")
        .env("HOME", home.path())
        .args(["new", "Default path"])
        .output()
        .unwrap();
    successful_stdout(output);
    assert!(home
        .path()
        .join(".local/share/notes-gpui/notes.db")
        .is_file());
}

#[test]
fn cli_preserves_markdown_arguments_starting_with_hyphens() {
    let directory = tempfile::tempdir().unwrap();
    let db = directory.path().join("notes.db");
    let id = successful_stdout(run_notes(&db, &["new", "- ideas", "--body", "- item"]));
    successful_stdout(run_notes(&db, &["append", id.trim(), "- [ ] todo"]));
    assert_eq!(
        successful_stdout(run_notes(&db, &["show", id.trim()])),
        "- ideas\n\n- item- [ ] todo\n"
    );
}

#[test]
fn cli_exits_quietly_when_output_pipe_closes() {
    let directory = tempfile::tempdir().unwrap();
    let db = directory.path().join("notes.db");
    let store = notes_core::NoteStore::open(&db).unwrap();
    let note = store
        .create_note("Large note", &"a".repeat(1024 * 1024))
        .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_notes-m1"))
        .env("NOTES_DB", &db)
        .args(["show", &note.id.to_string()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
}
