//! Sideshow's data model on our wire: agent sessions, posts made of typed
//! parts, anchored comments, content-addressed assets, traces and settings.
//! Mirrors docs/ref/sideshow/server/types.ts with snake_case field names.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---- Ids -------------------------------------------------------------------

/// Typed record ids. On the wire each is a plain JSON string (`transparent`), so
/// the protocol is unchanged; in Rust a post id can't be passed as a session id.
macro_rules! id_type {
    ($($(#[$doc:meta])* $name:ident),* $(,)?) => {$(
        $(#[$doc])*
        #[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl std::ops::Deref for $name {
            type Target = str;
            fn deref(&self) -> &str {
                &self.0
            }
        }
        impl std::borrow::Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
        impl From<String> for $name {
            fn from(s: String) -> Self {
                $name(s)
            }
        }
        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                $name(s.to_owned())
            }
        }
        impl From<$name> for String {
            fn from(id: $name) -> String {
                id.0
            }
        }
        impl PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool {
                self.0 == other
            }
        }
        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self.0 == *other
            }
        }
    )*};
}

id_type!(SessionId, PostId, CommentId, AssetId);

// ---- Parts ---------------------------------------------------------------

/// One part of a post. Kinds match sideshow's surface kinds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[serde(deny_unknown_fields)]
pub enum Part {
    /// Agent-authored markup, rendered sandboxed (WKWebView) with optional kits.
    Html {
        html: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        kits: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    Markdown {
        markdown: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// Mermaid source, rendered to SVG (pele in JavaScriptCore).
    Mermaid {
        mermaid: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// A unified patch and/or explicit before/after files; `patch` wins when both are set.
    Diff {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        patch: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<DiffFile>,
        #[serde(default)]
        layout: DiffLayout,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    Image {
        asset_id: AssetId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        alt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// A step timeline: inline `steps` and/or a larger uploaded trace asset.
    Trace {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        steps: Vec<TraceStep>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        asset_id: Option<AssetId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// Terminal output; ANSI SGR escapes are rendered as styles.
    Terminal {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cols: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// Any JSON value, shown as a collapsible tree.
    Json {
        data: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    Code {
        code: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        line_start: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffFile {
    pub filename: String,
    pub before: String,
    pub after: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiffLayout {
    #[default]
    Unified,
    Split,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceStep {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts: Option<String>,
}

// ---- Records -------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Unix seconds.
    pub created_at: i64,
    pub last_active_at: i64,
    pub post_count: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Post {
    pub id: PostId,
    pub session: SessionId,
    pub title: String,
    pub parts: Vec<Part>,
    pub version: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostSummary {
    pub id: PostId,
    pub session: SessionId,
    pub title: String,
    /// Kind of each part, in order.
    pub kinds: Vec<String>,
    pub version: u64,
    pub updated_at: i64,
}

/// Result of every post write. `feedback` carries user comments in the
/// session the agent hasn't seen yet, so agents hear replies without `wait`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PostWriteResult {
    pub post: PostSummary,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub feedback: Vec<Comment>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Comment {
    pub id: CommentId,
    /// Monotonic per daemon; `wait` and `comment-list --after` use it.
    pub seq: u64,
    pub session: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post: Option<PostId>,
    /// `user` for the person in the app, otherwise the agent name.
    pub author: String,
    pub text: String,
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<CommentAnchor>,
}

/// Where on a post a comment points. `part` is the 0-based part index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[serde(deny_unknown_fields)]
pub enum CommentAnchor {
    Point { part: u32, post_version: u64, x: f64, y: f64 },
    Rect { part: u32, post_version: u64, x: f64, y: f64, w: f64, h: f64 },
    LineRange {
        part: u32,
        post_version: u64,
        start_line: u32,
        end_line: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AssetKind {
    Image,
    Trace,
    File,
}

/// Asset metadata. `id` is the SHA-256 of the bytes, so uploads dedupe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetInfo {
    pub id: AssetId,
    pub kind: AssetKind,
    pub content_type: String,
    pub byte_length: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetData {
    pub info: AssetInfo,
    pub data_base64: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kit {
    pub id: String,
    pub label: String,
    pub summary: String,
    pub css: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub js: Option<String>,
}

// ---- Daemon-side render output ---------------------------------------------
//
// The daemon does the heavy work (syntax highlighting, diff hunking, ANSI
// parsing) and caches it by content hash; the app only lays out spans.
// Markdown, mermaid, html, json, image and trace parts render in the app.

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub text: String,
    /// Token class (`keyword`, `string`, `comment`, `number`, `function`,
    /// `type`, ...) or for terminals an SGR style like `fg-red bold`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
}

pub type StyledLine = Vec<Span>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiffLineKind {
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_line: Option<u32>,
    pub spans: StyledLine,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffHunk {
    pub header: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffFileRender {
    pub filename: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub added: u32,
    pub removed: u32,
    pub hunks: Vec<DiffHunk>,
}

/// Render output for one part, in post order. Parts the app renders itself
/// come back as `Native`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum PartRender {
    Code { language: Option<String>, line_start: u32, lines: Vec<StyledLine> },
    Diff { layout: DiffLayout, files: Vec<DiffFileRender> },
    Terminal { lines: Vec<StyledLine> },
    Native,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostRender {
    pub id: PostId,
    pub version: u64,
    pub parts: Vec<PartRender>,
}

// ---- Verb arguments ----------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionNewArgs {
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// Sessions whose title, agent or cwd matches `query`, or whose posts or
/// comments contain it (case-insensitive). Best matches first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSearchArgs {
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// One `session-search` result. `post` and `text` locate the first content
/// match; a title-only match leaves them empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionHit {
    pub session: Session,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post: Option<PostId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionRenameArgs {
    pub id: SessionId,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdArgs<T> {
    pub id: T,
}

/// Create a post. With no `session`, it joins the calling conversation's
/// session (`owner`), or without an owner the newest for `Request.agent` +
/// `Request.cwd`; failing both, it starts one.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostNewArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub parts: Vec<Part>,
    /// Title for the session this post creates; ignored when it joins one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_title: Option<String>,
    /// The calling conversation (the CLI sets it). With no `session`, the post
    /// joins this conversation's session, or starts one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}

/// New version of a post; omitted fields keep their current value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostUpdateArgs {
    pub id: PostId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parts: Option<Vec<Part>>,
}

/// Edit one part (new post version). `target` is a part id or 0-based index;
/// for `add` it is the insert position (default: append).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostPartArgs {
    pub id: PostId,
    pub op: PartOp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<Part>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PartOp {
    Add,
    Set,
    Rm,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostListArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Show a post, optionally an older version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostShowArgs {
    pub id: PostId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommentNewArgs {
    /// A post id; its session is implied. Without it, `session` is required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post: Option<PostId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<CommentAnchor>,
    /// `user` when the app posts on the person's behalf; defaults to `Request.agent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommentListArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post: Option<PostId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_seq: Option<u64>,
}

/// Block until a `user` comment arrives in the session (or on the post) after
/// what the agent has seen, then return it. `timeout_secs` 0 = no timeout.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaitArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    /// With no `session`: wait on this conversation's session (see `PostNewArgs::owner`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post: Option<PostId>,
    #[serde(default)]
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetPutArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    pub kind: AssetKind,
    pub content_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    pub data_base64: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceSetArgs {
    pub session: SessionId,
    pub steps: Vec<TraceStep>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingGetArgs {
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingSetArgs {
    pub key: String,
    pub value: String,
}
