//! Wire protocol shared by `notesd` and its clients (the `notes` CLI, `stage`).
//!
//! One JSON object per line, UTF-8, both directions. See docs/cli-spec.md.
//!
//! ```text
//! → {"v":1,"id":7,"verb":"post-list","args":{}}
//! ← {"id":7,"ok":true,"result":[…]}
//! ← {"id":7,"ok":false,"error":{"code":"not_found","message":"…","hint":"…"}}
//! ← {"id":7,"event":{"seq":12,"kind":"post-updated","id":"p1","session":"s1"}}
//! ```

use std::{env, path::PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod helper;
pub mod kits;
pub mod posts;
use posts::*;

pub const PROTOCOL_VERSION: u32 = 1;

/// Unix socket `notesd` listens on: `NOTES_SOCK`, else
/// `~/Library/Application Support/Notes/notesd.sock`.
pub fn default_socket_path() -> PathBuf {
    env::var_os("NOTES_SOCK")
        .map(PathBuf::from)
        .unwrap_or_else(|| app_support_dir().join("notesd.sock"))
}

/// Stage session directory: `NOTES_STAGE_DIR` (an absolute path), else `~/Desktop/Stage`.
/// An empty override keeps the default; selecting another directory never migrates sessions.
pub fn default_stage_path() -> PathBuf {
    env::var_os("NOTES_STAGE_DIR")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join("Desktop/Stage"))
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
    /// Agent name from `NOTES_AGENT`, recorded as the author of posts and comments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

/// Every verb and its arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "verb", content = "args", rename_all = "kebab-case")]
pub enum Verb {
    // Events
    Watch(WatchArgs),
    // Batch and meta
    Batch(BatchArgs),
    Manifest,
    Doctor,
    // Sessions, posts, comments, assets, traces, kits, settings (see posts.rs)
    SessionNew(SessionNewArgs),
    SessionList,
    SessionSearch(SessionSearchArgs),
    SessionRename(SessionRenameArgs),
    /// Moves to a trash kept 30 days; `purge` deletes for good.
    SessionDelete(SessionDeleteArgs),
    SessionRestore(IdArgs<SessionId>),
    PostNew(PostNewArgs),
    PostUpdate(PostUpdateArgs),
    PostPart(PostPartArgs),
    PostList(PostListArgs),
    PostShow(PostShowArgs),
    /// Daemon-side render (highlighting, diff hunks, ANSI) for a post version.
    PostRender(PostShowArgs),
    PostDelete(IdArgs<PostId>),
    CommentNew(CommentNewArgs),
    CommentList(CommentListArgs),
    CommentDelete(IdArgs<CommentId>),
    Wait(WaitArgs),
    AssetPut(AssetPutArgs),
    AssetGet(IdArgs<AssetId>),
    TraceSet(TraceSetArgs),
    TraceList(IdArgs<SessionId>),
    Kits,
    SettingGet(SettingGetArgs),
    SettingSet(SettingSetArgs),
}

/// 1-based inclusive line range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LineRange {
    pub start: u32,
    pub end: u32,
}

/// Stream events after `since` (exclusive); `kinds` empty means all.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
    /// Extra structured data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Usage,
    NotFound,
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
            ErrorCode::DaemonUnreachable => 6,
        }
    }
}

/// One event-log entry. Kinds: `session-created|updated|deleted`,
/// `post-created|updated|deleted`, `comment-created|deleted`, `trace-updated`,
/// `setting-changed`. Records set `id`/`session`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub seq: u64,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorResult {
    pub protocol: u32,
    pub daemon_version: String,
    pub socket: PathBuf,
    pub database: PathBuf,
    /// Render cache counters `{hits, misses, entries, bytes}` (hit rate = hits / (hits + misses)).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render_cache: Option<Value>,
    /// Latest committed event seq: start a `watch` here, then list, and nothing
    /// committed in between is missed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    /// Session folder `{path, sessions, skipped: [file…], migration}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<Value>,
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
    fn unit_verbs_and_defaults_parse() {
        let request: Request = serde_json::from_str(r#"{"v":1,"id":1,"verb":"doctor"}"#).unwrap();
        assert_eq!(request.verb, Verb::Doctor);
        let request: Request =
            serde_json::from_str(r#"{"v":1,"id":2,"verb":"watch","args":{}}"#).unwrap();
        assert_eq!(request.verb, Verb::Watch(WatchArgs::default()));
    }

    #[test]
    fn responses_round_trip_and_stay_distinguishable() {
        let ok = Response::ok(7, json!({"id": "p1"}));
        let err = Response::err(
            7,
            ErrorBody {
                code: ErrorCode::NotFound,
                message: "no such post".into(),
                hint: Some("run `notes post list`".into()),
                details: None,
            },
        );
        let event = Response::Event {
            id: 7,
            event: Event { seq: 12, kind: "post-updated".into(), path: None, version: Some(2), id: Some("p1".into()), session: Some("s1".into()) },
        };
        for response in [ok, err, event] {
            let line = to_line(&response);
            assert!(line.ends_with('\n') && !line[..line.len() - 1].contains('\n'));
            assert_eq!(serde_json::from_str::<Response>(&line).unwrap(), response);
        }
        assert_eq!(ErrorCode::NotFound.exit_code(), 3);
    }
}

#[cfg(test)]
mod posts_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sideshow_verbs_and_parts_use_kebab_kinds() {
        let request: Request = serde_json::from_value(json!({
            "v": 1, "id": 3, "verb": "post-new",
            "args": {"title": "Plan", "parts": [
                {"kind": "markdown", "markdown": "# Why"},
                {"kind": "diff", "patch": "--- a\n+++ b\n"},
                {"kind": "json", "data": {"a": [1, null]}},
                {"kind": "image", "asset_id": "abc"}
            ]}
        }))
        .unwrap();
        let Verb::PostNew(args) = &request.verb else { panic!("expected post-new") };
        assert_eq!(args.parts.len(), 4);
        assert!(matches!(&args.parts[1], Part::Diff { layout: DiffLayout::Unified, .. }));
        let wire = serde_json::to_value(&request).unwrap();
        assert_eq!(serde_json::from_value::<Request>(wire).unwrap(), request);

        let wait: Request = serde_json::from_str(r#"{"v":1,"id":4,"verb":"wait","args":{}}"#).unwrap();
        assert_eq!(wait.verb, Verb::Wait(WaitArgs::default()));
        let kits: Request = serde_json::from_str(r#"{"v":1,"id":5,"verb":"kits"}"#).unwrap();
        assert_eq!(kits.verb, Verb::Kits);
    }

    #[test]
    fn anchors_and_renders_round_trip() {
        let anchor: CommentAnchor = serde_json::from_value(json!({
            "kind": "line-range", "part": 1, "post_version": 2, "start_line": 4, "end_line": 9
        }))
        .unwrap();
        assert!(matches!(anchor, CommentAnchor::LineRange { start_line: 4, .. }));
        let render = PartRender::Code {
            language: Some("rust".into()),
            line_start: 1,
            lines: vec![vec![Span { text: "fn".into(), class: Some("keyword".into()) }]],
        };
        let wire = serde_json::to_value(&render).unwrap();
        assert_eq!(wire["kind"], "code");
        assert_eq!(serde_json::from_value::<PartRender>(wire).unwrap(), render);
    }
}
