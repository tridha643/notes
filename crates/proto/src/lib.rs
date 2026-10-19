//! Wire protocol shared by `notesd` and its clients (the `notes` CLI, the app).
//!
//! One JSON object per line, UTF-8, both directions. See docs/cli-spec.md.
//!
//! ```text
//! → {"v":1,"id":7,"verb":"edit","args":{"ref":"work/plan","old":"foo","new":"bar"}}
//! ← {"id":7,"ok":true,"result":{"path":"work/plan.md","version":43}}
//! ← {"id":7,"ok":false,"error":{"code":"ambiguous","message":"…","hint":"…"}}
//! ← {"id":7,"event":{"seq":12,"kind":"note-updated","path":"work/plan.md"}}
//! ```

use std::{env, path::PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 1;

/// Unix socket `notesd` listens on: `NOTES_SOCK`, else
/// `~/Library/Application Support/Notes/notesd.sock`.
pub fn default_socket_path() -> PathBuf {
    env::var_os("NOTES_SOCK")
        .map(PathBuf::from)
        .unwrap_or_else(|| app_support_dir().join("notesd.sock"))
}

/// Vault root holding the `.md` notes: `NOTES_VAULT`, else `~/Notes`.
pub fn default_vault_path() -> PathBuf {
    env::var_os("NOTES_VAULT")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join("Notes"))
}

/// Directory for the daemon's SQLite database and other private state.
pub fn app_support_dir() -> PathBuf {
    home_dir().join("Library/Application Support/Notes")
}

fn home_dir() -> PathBuf {
    env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// One request line from a client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub v: u32,
    pub id: u64,
    #[serde(flatten)]
    pub verb: Verb,
    /// Client working directory; lets relative paths resolve like the caller expects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// Agent name from `NOTES_AGENT`, recorded as the author in history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

/// Every P0 verb and its arguments. A `note` field is a note ref: a
/// vault-relative path with or without `.md`, a unique bare title, or `@last`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "verb", content = "args", rename_all = "kebab-case")]
pub enum Verb {
    // Read
    List(ListArgs),
    Find(FindArgs),
    Search(SearchArgs),
    Read(ReadArgs),
    // Write
    New(NewArgs),
    Edit(EditArgs),
    Append(AppendArgs),
    Prepend(PrependArgs),
    Write(WriteArgs),
    Move(MoveArgs),
    Mkdir(FolderArgs),
    Rmdir(FolderArgs),
    Delete(DeleteArgs),
    Restore(NoteArgs),
    // History
    History(HistoryArgs),
    Diff(DiffArgs),
    Revert(RevertArgs),
    // Events
    Watch(WatchArgs),
    // Batch and meta
    Batch(BatchArgs),
    Manifest,
    Doctor,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ListArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    #[serde(default)]
    pub recursive: bool,
    #[serde(default)]
    pub sort: ListSort,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ListSort {
    #[default]
    Updated,
    Created,
    Title,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FindArgs {
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchArgs {
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// Lines of context around each hit (`-C`).
    #[serde(default)]
    pub context: u32,
    /// Force case-sensitive matching (default is smart case).
    #[serde(default)]
    pub case_sensitive: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadArgs {
    pub note: String,
    /// 1-based inclusive line range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<LineRange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineRange {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewArgs {
    pub path: String,
    #[serde(default)]
    pub body: String,
}

/// Exact find-and-replace edits, applied all-or-nothing. Each `old` must match
/// exactly once unless `all` is set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditArgs {
    pub note: String,
    pub edits: Vec<Replacement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_version: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Replacement {
    pub old: String,
    pub new: String,
    #[serde(default)]
    pub all: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppendArgs {
    pub note: String,
    pub text: String,
    /// Append to the end of the section under this heading instead of the note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub under: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_version: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrependArgs {
    pub note: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_version: Option<u64>,
}

/// Replace the whole body. Requires `if_version` unless `force`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WriteArgs {
    pub note: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_version: Option<u64>,
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MoveArgs {
    pub note: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FolderArgs {
    pub folder: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeleteArgs {
    pub note: String,
    /// Delete for good instead of moving to trash.
    #[serde(default)]
    pub purge: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoteArgs {
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryArgs {
    pub note: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Unified diff between two versions; defaults to previous → current.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiffArgs {
    pub note: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RevertArgs {
    pub note: String,
    pub to: u64,
}

/// Stream events after `since` (exclusive); `kinds` empty means all.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WatchArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<String>,
}

/// Several verbs over one connection. `atomic` runs them in one transaction
/// and rolls back on the first failure. Nested batches are rejected.
/// The result is a `Vec<BatchOutcome>`, one per op in order; an atomic batch
/// stops at the first error, so its last outcome is that error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchArgs {
    pub ops: Vec<Verb>,
    #[serde(default)]
    pub atomic: bool,
}

/// One response line from the daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Response {
    Ok { id: u64, ok: True, result: Value },
    Err { id: u64, ok: False, error: ErrorBody },
    Event { id: u64, event: Event },
}

impl Response {
    pub fn ok(id: u64, result: impl Serialize) -> Self {
        let result = serde_json::to_value(result).expect("result serializes to JSON");
        Response::Ok { id, ok: True, result }
    }

    pub fn err(id: u64, error: ErrorBody) -> Self {
        Response::Err { id, ok: False, error }
    }
}

/// One op's outcome inside a `batch` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum BatchOutcome {
    Ok { ok: True, result: Value },
    Err { ok: False, error: ErrorBody },
}

/// Literal `true` / `false` markers so `ok` round-trips exactly in untagged responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct True;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct False;

macro_rules! bool_marker {
    ($ty:ident, $value:literal) => {
        impl Serialize for $ty {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_bool($value)
            }
        }
        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                match bool::deserialize(d)? {
                    $value => Ok($ty),
                    _ => Err(serde::de::Error::custom(concat!("expected ", $value))),
                }
            }
        }
    };
}
bool_marker!(True, true);
bool_marker!(False, false);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Extra structured data, e.g. candidate paths. `version_conflict` always
    /// carries `{"current_version": N}`; build it with [`ErrorBody::version_conflict`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl ErrorBody {
    pub fn version_conflict(path: &str, expected: u64, current_version: u64) -> Self {
        ErrorBody {
            code: ErrorCode::VersionConflict,
            message: format!("{path} is at version {current_version}, not {expected}"),
            hint: Some(format!("re-read the note, or retry with --if-version {current_version}")),
            details: Some(serde_json::json!({ "current_version": current_version })),
        }
    }

    /// `current_version` from a `version_conflict` error's details.
    pub fn current_version(&self) -> Option<u64> {
        self.details.as_ref()?.get("current_version")?.as_u64()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Usage,
    NotFound,
    /// A bare title matched several notes, or an edit's `old` matched more than once.
    Ambiguous,
    VersionConflict,
    /// Set by clients when the socket can't be reached; the daemon never sends it.
    DaemonUnreachable,
    Other,
}

impl ErrorCode {
    /// CLI exit code from docs/cli-spec.md.
    pub fn exit_code(self) -> u8 {
        match self {
            ErrorCode::Other => 1,
            ErrorCode::Usage => 2,
            ErrorCode::NotFound => 3,
            ErrorCode::Ambiguous => 4,
            ErrorCode::VersionConflict => 5,
            ErrorCode::DaemonUnreachable => 6,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub seq: u64,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u64>,
}

// Result shapes, one per verb family. Paths are vault-relative with `.md`.

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteSummary {
    pub path: String,
    pub title: String,
    pub version: u64,
    /// Unix seconds.
    pub updated_at: i64,
}

/// One `list` result element; the result is a `Vec<ListEntry>`, folders first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ListEntry {
    Folder { folder: String },
    Note(NoteSummary),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FindHit {
    pub path: String,
    pub score: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    pub path: String,
    /// 1-based.
    pub line: u32,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub before: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub after: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub path: String,
    pub title: String,
    pub version: u64,
    pub body: String,
    pub line_count: u32,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteResult {
    pub path: String,
    pub version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub version: u64,
    pub at: i64,
    /// `human`, an agent name, or `external` for edits made outside the daemon.
    pub author: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffResult {
    pub path: String,
    pub from: u64,
    pub to: u64,
    pub unified: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorResult {
    pub protocol: u32,
    pub daemon_version: String,
    pub socket: PathBuf,
    pub vault: PathBuf,
    pub database: PathBuf,
    pub notes: u64,
}

/// Serialize one message as a protocol line (JSON + `\n`).
pub fn to_line(message: &impl Serialize) -> String {
    let mut line = serde_json::to_string(message).expect("protocol message serializes to JSON");
    line.push('\n');
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_matches_spec_wire_shape() {
        let request = Request {
            v: PROTOCOL_VERSION,
            id: 7,
            verb: Verb::Edit(EditArgs {
                note: "work/plan".into(),
                edits: vec![Replacement { old: "foo".into(), new: "bar".into(), all: false }],
                if_version: None,
            }),
            cwd: None,
            agent: None,
        };
        let wire: Value = serde_json::from_str(&to_line(&request)).unwrap();
        assert_eq!(
            wire,
            json!({"v":1,"id":7,"verb":"edit","args":{"note":"work/plan","edits":[{"old":"foo","new":"bar","all":false}]}})
        );
        assert_eq!(serde_json::from_value::<Request>(wire).unwrap(), request);
    }

    #[test]
    fn unit_verbs_and_defaults_parse() {
        let request: Request = serde_json::from_str(r#"{"v":1,"id":1,"verb":"doctor"}"#).unwrap();
        assert_eq!(request.verb, Verb::Doctor);
        let request: Request =
            serde_json::from_str(r#"{"v":1,"id":2,"verb":"list","args":{}}"#).unwrap();
        assert_eq!(request.verb, Verb::List(ListArgs::default()));
    }

    #[test]
    fn responses_round_trip_and_stay_distinguishable() {
        let ok = Response::ok(7, WriteResult { path: "work/plan.md".into(), version: 43 });
        let err = Response::err(
            7,
            ErrorBody {
                code: ErrorCode::Ambiguous,
                message: "\"foo\" matches 3 times (lines 4, 9, 30)".into(),
                hint: Some("add surrounding text or pass --all".into()),
                details: None,
            },
        );
        let event = Response::Event {
            id: 7,
            event: Event { seq: 12, kind: "note-updated".into(), path: Some("work/plan.md".into()), version: Some(44) },
        };
        for response in [ok, err, event] {
            let line = to_line(&response);
            assert!(line.ends_with('\n') && !line[..line.len() - 1].contains('\n'));
            assert_eq!(serde_json::from_str::<Response>(&line).unwrap(), response);
        }
        let wire: Value = serde_json::from_str(r#"{"id":7,"ok":false,"error":{"code":"version_conflict","message":"x","details":{"current_version":9}}}"#).unwrap();
        let Response::Err { error, .. } = serde_json::from_value(wire).unwrap() else { panic!("expected error") };
        assert_eq!(error.code.exit_code(), 5);
        assert_eq!(error.current_version(), Some(9));
        assert_eq!(ErrorBody::version_conflict("a.md", 3, 4).current_version(), Some(4));
    }

    #[test]
    fn list_entries_and_batch_outcomes_are_flat_lines() {
        let entries = vec![
            ListEntry::Folder { folder: "work".into() },
            ListEntry::Note(NoteSummary { path: "a.md".into(), title: "a".into(), version: 1, updated_at: 0 }),
        ];
        let wire = serde_json::to_value(&entries).unwrap();
        assert_eq!(wire[0], json!({"folder":"work"}));
        assert_eq!(wire[1]["path"], "a.md");
        assert_eq!(serde_json::from_value::<Vec<ListEntry>>(wire).unwrap(), entries);

        let outcomes: Vec<BatchOutcome> = serde_json::from_str(
            r#"[{"ok":true,"result":{"path":"a.md","version":2}},{"ok":false,"error":{"code":"not_found","message":"b"}}]"#,
        )
        .unwrap();
        assert!(matches!(outcomes[0], BatchOutcome::Ok { .. }));
        assert!(matches!(&outcomes[1], BatchOutcome::Err { error, .. } if error.code == ErrorCode::NotFound));
    }
}
