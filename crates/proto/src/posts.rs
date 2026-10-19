//! Sideshow's data model on our wire: agent sessions, posts made of typed
//! parts, anchored comments, content-addressed assets, traces and settings.
//! Mirrors docs/ref/sideshow/server/types.ts with snake_case field names.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---- Parts ---------------------------------------------------------------

/// One part of a post. Kinds match sideshow's surface kinds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
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
        asset_id: String,
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
        asset_id: Option<String>,
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
    pub id: String,
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
    pub id: String,
    pub session: String,
    pub title: String,
    pub parts: Vec<Part>,
    pub version: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostSummary {
    pub id: String,
    pub session: String,
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
    pub id: String,
    /// Monotonic per daemon; `wait` and `comment-list --after` use it.
    pub seq: u64,
    pub session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post: Option<String>,
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
    pub id: String,
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
    pub id: String,
    pub version: u64,
    pub parts: Vec<PartRender>,
}

// ---- Verb arguments ----------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionNewArgs {
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRenameArgs {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdArgs {
    pub id: String,
}

/// Create a post. With no `session`, the daemon uses (or creates) the
/// caller's session for `Request.agent` + `Request.cwd`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PostNewArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub parts: Vec<Part>,
}

/// New version of a post; omitted fields keep their current value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PostUpdateArgs {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parts: Option<Vec<Part>>,
}

/// Edit one part (new post version). `target` is a part id or 0-based index;
/// for `add` it is the insert position (default: append).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PostPartArgs {
    pub id: String,
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
pub struct PostListArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Show a post, optionally an older version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PostShowArgs {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommentNewArgs {
    /// A post id; its session is implied. Without it, `session` is required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<CommentAnchor>,
    /// `user` when the app posts on the person's behalf; defaults to `Request.agent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CommentListArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_seq: Option<u64>,
}

/// Block until a `user` comment arrives in the session (or on the post) after
/// what the agent has seen, then return it. `timeout_secs` 0 = no timeout.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WaitArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post: Option<String>,
    #[serde(default)]
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssetPutArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    pub kind: AssetKind,
    pub content_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    pub data_base64: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceSetArgs {
    pub session: String,
    pub steps: Vec<TraceStep>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingGetArgs {
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingSetArgs {
    pub key: String,
    pub value: String,
}
