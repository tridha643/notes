//! `notes`: thin client for `notesd`. Every call is one request line over the
//! unix socket and one response line back (or a stream of event lines for
//! `watch`). It never touches the vault or SQLite. See docs/cli-spec.md.

// Errors are `ErrorBody` straight off the wire and happen at most once per process.
#![allow(clippy::result_large_err)]

mod lint;
mod owner;

use std::{
    env,
    fmt::Write as _,
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use notes_proto::posts::{
    AssetId, AssetInfo, AssetKind, AssetPutArgs, Comment, CommentAnchor, CommentListArgs, CommentNewArgs,
    DiffLayout, IdArgs, Kit, Part, PartOp, Post, PostListArgs, PostNewArgs, PostPartArgs,
    PostId, PostShowArgs, PostSummary, PostUpdateArgs, PostWriteResult, Session, SessionHit,
    SessionDeleteArgs, SessionDeleteResult, SessionId, SessionRenameArgs, SessionRestoreResult,
    SessionSearchArgs, TraceSetArgs, TraceStep, WaitArgs,
};
use notes_proto::{
    default_socket_path, to_line, AppendArgs, BatchArgs, BatchOutcome, DeleteArgs, DiffArgs,
    DiffResult, DoctorResult, EditArgs, ErrorBody, ErrorCode, FindArgs, FindHit, FolderArgs,
    HistoryArgs, HistoryEntry, LineRange, ListArgs, ListEntry, ListSort, MoveArgs, NewArgs, Note,
    NoteArgs, PrependArgs, ReadArgs, Replacement, Request, Response, RevertArgs, SearchArgs,
    SearchHit, Verb, WatchArgs, WriteArgs, WriteResult, PROTOCOL_VERSION,
};
use serde::de::DeserializeOwned;
use serde_json::Value;

#[derive(Debug, Parser)]
#[command(
    name = "notes",
    version,
    about = "Plain markdown notes, served by notesd"
)]
struct Cli {
    /// Print the raw result; lists print one JSON object per line
    #[arg(long, global = true)]
    json: bool,
    /// Print nothing on success; rely on the exit code
    #[arg(short, long, global = true)]
    quiet: bool,
    /// Socket path (default: NOTES_SOCK, else ~/Library/Application Support/Notes/notesd.sock)
    #[arg(long, global = true, value_name = "PATH")]
    sock: Option<PathBuf>,
    /// On edit/append/prepend/write: fail with exit 5 unless the note is at version N
    #[arg(long, global = true, value_name = "N")]
    if_version: Option<u64>,
    #[command(subcommand)]
    cmd: Cmd,
}

// Parsed once per process, so the size of the largest variant doesn't matter.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Subcommand)]
enum Cmd {
    /// List notes and subfolders in a folder (vault root by default)
    List {
        folder: Option<String>,
        #[arg(short, long)]
        recursive: bool,
        /// updated, created or title
        #[arg(long, default_value = "updated", value_parser = kebab::<ListSort>)]
        sort: ListSort,
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Fuzzy path search
    Find {
        query: String,
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Content search, results as path:line: text
    Search {
        query: String,
        folder: Option<String>,
        /// Lines of context around each hit
        #[arg(short = 'C', default_value_t = 0)]
        context: u32,
        /// Case-sensitive (default is smart case)
        #[arg(long)]
        case: bool,
        /// Typo-tolerant matching (off by default, so a miss stays a miss)
        #[arg(long)]
        fuzzy: bool,
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Print a note's body
    #[command(alias = "show")]
    Read {
        note: String,
        /// 1-based inclusive range, A:B
        #[arg(long, value_parser = parse_lines)]
        lines: Option<LineRange>,
        /// Prefix line numbers like `cat -n`
        #[arg(short = 'n')]
        number: bool,
    },
    /// Create a note, making folders as needed
    New {
        path: String,
        /// Body text, or - for stdin
        text: Option<String>,
    },
    /// Exact find-and-replace (--old/--new), or several edits from JSON (--from)
    Edit {
        note: String,
        #[arg(long, requires = "new")]
        old: Option<String>,
        #[arg(long, requires = "old")]
        new: Option<String>,
        /// Replace every match of --old
        #[arg(long, requires = "old")]
        all: bool,
        /// JSON [{"old":…,"new":…}] file, or - for stdin
        #[arg(long, value_name = "FILE", conflicts_with_all = ["old", "new", "all"], required_unless_present = "old")]
        from: Option<String>,
    },
    /// Add text to the end of a note or of a section
    Append {
        note: String,
        /// Text, or - for stdin
        text: String,
        #[arg(long, value_name = "HEADING")]
        under: Option<String>,
    },
    /// Add text at the top of a note
    Prepend { note: String, text: String },
    /// Replace the whole body; requires --if-version or --force
    Write {
        note: String,
        /// Body text, or - for stdin
        text: String,
        #[arg(long)]
        force: bool,
    },
    /// Move or rename a note
    Move { note: String, to: String },
    /// Create a folder
    Mkdir { folder: String },
    /// Remove an empty folder
    Rmdir { folder: String },
    /// Move a note to trash, or delete it for good with --purge
    Delete {
        note: String,
        #[arg(long)]
        purge: bool,
    },
    /// Bring a note back from trash
    Restore { note: String },
    /// Versions with time and author
    History {
        note: String,
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Unified diff between versions (default: previous to current)
    Diff {
        note: String,
        #[arg(long)]
        from: Option<u64>,
        #[arg(long)]
        to: Option<u64>,
    },
    /// Restore an earlier version as a new version
    Revert {
        note: String,
        #[arg(long)]
        to: u64,
    },
    /// Stream events as NDJSON, resuming after --since
    Watch {
        #[arg(long, value_name = "SEQ")]
        since: Option<u64>,
        /// Comma-separated kinds, e.g. note,post,comment
        #[arg(long, value_delimiter = ',')]
        kind: Vec<String>,
    },
    /// Run NDJSON {"verb":…,"args":…} ops from stdin over one connection
    Batch {
        #[arg(long)]
        atomic: bool,
    },
    /// Publish a sideshow post (`post <kind> <file|->`), or update, list and show posts
    Post {
        /// Post title
        #[arg(long, global = true)]
        title: Option<String>,
        /// Session (default: NOTES_SESSION, else this conversation's own session)
        #[arg(long, global = true)]
        session: Option<SessionId>,
        /// Title for the session, if this post starts one: name the task
        #[arg(long, global = true)]
        session_title: Option<String>,
        #[command(flatten)]
        more: MoreParts,
        #[command(subcommand)]
        cmd: PostCmd,
    },
    /// Store a file as an asset; prints its id
    Upload {
        file: String,
        /// image, trace or file (default: image for image files, else file)
        #[arg(long, value_parser = kebab::<AssetKind>)]
        kind: Option<AssetKind>,
        #[arg(long)]
        session: Option<SessionId>,
    },
    /// Reply on a post
    Comment {
        post: PostId,
        /// Text, or - for stdin
        text: String,
        /// Anchor the comment to lines A:B of a part
        #[arg(long, value_parser = parse_lines, value_name = "A:B")]
        line: Option<LineRange>,
        /// Part index the --line anchor points at (default 0)
        #[arg(long, requires = "line")]
        part: Option<u32>,
    },
    /// Comments in a session or on a post
    Comments {
        #[arg(long)]
        post: Option<PostId>,
        #[arg(long)]
        session: Option<SessionId>,
        /// Only comments after this seq
        #[arg(long, value_name = "SEQ")]
        after: Option<u64>,
    },
    /// Block until the user comments, then print it
    Wait {
        #[arg(long)]
        post: Option<PostId>,
        #[arg(long)]
        session: Option<SessionId>,
        /// Seconds to wait; 0 waits forever
        #[arg(long, default_value_t = 120)]
        timeout: u64,
    },
    /// List agent sessions
    Sessions,
    /// Rename or delete a session
    Session {
        #[command(subcommand)]
        cmd: SessionCmd,
    },
    /// Replace a session's step trace from JSON or JSONL steps
    Trace {
        /// [{"label":…}] array or one step per line, or - for stdin
        file: String,
        #[arg(long)]
        session: SessionId,
    },
    /// List the opt-in kits for html parts
    Kits,
    /// Every verb, its args, output schema and exit codes, as JSON
    Manifest,
    /// Socket and database paths, daemon reachable, versions
    Doctor,
    /// How agents post to stage and use notes (read it before your first post)
    AgentHowto,
    /// A block to paste into AGENTS.md or CLAUDE.md that points agents at stage
    Setup,
    /// Design guide for stage posts; read it before your first post
    Guide,
    /// Shell completion script
    Completions { shell: Shell },
}

/// More parts in the same post, after its main one, in this order: prose,
/// diagrams, code, patches, terminal output, html. Each flag repeats.
#[derive(Debug, clap::Args)]
struct MoreParts {
    /// Add a markdown part
    #[arg(long = "md", global = true, value_name = "FILE")]
    markdown: Vec<String>,
    /// Add a mermaid part
    #[arg(long, global = true, value_name = "FILE")]
    mermaid: Vec<String>,
    /// Add a code part (language from the file name)
    #[arg(long, global = true, value_name = "FILE")]
    code: Vec<String>,
    /// Add a diff part
    #[arg(long, global = true, value_name = "FILE")]
    diff: Vec<String>,
    /// Add a terminal part
    #[arg(long, global = true, value_name = "FILE")]
    terminal: Vec<String>,
    /// Add an html part
    #[arg(long, global = true, value_name = "FILE")]
    html: Vec<String>,
}

impl MoreParts {
    fn kinds(self) -> Vec<PartKind> {
        let MoreParts { markdown, mermaid, code, diff, terminal, html } = self;
        let mut kinds: Vec<PartKind> = markdown.into_iter().map(|file| PartKind::Markdown { file }).collect();
        kinds.extend(mermaid.into_iter().map(|file| PartKind::Mermaid { file }));
        kinds.extend(code.into_iter().map(|file| PartKind::Code { file, language: None, filename: None, line_start: None }));
        kinds.extend(diff.into_iter().map(|file| PartKind::Diff { file, layout: DiffLayout::Unified }));
        kinds.extend(terminal.into_iter().map(|file| PartKind::Terminal { file, cols: None, term_title: None }));
        kinds.extend(html.into_iter().map(|file| PartKind::Html { file, kit: Vec::new() }));
        kinds
    }
}

#[derive(Debug, Subcommand)]
enum PostCmd {
    #[command(flatten)]
    New(PartKind),
    /// New version of a post (same card): a new title and/or this one part
    Update {
        id: PostId,
        #[command(subcommand)]
        part: Option<PartKind>,
    },
    /// Add, replace or remove one part
    Part {
        id: PostId,
        #[command(subcommand)]
        op: PartCmd,
    },
    /// List posts
    List {
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Show a post and its parts
    Show {
        id: PostId,
        #[arg(long)]
        version: Option<u64>,
    },
    /// Delete a post
    Delete { id: PostId },
}

#[derive(Debug, Subcommand)]
enum PartCmd {
    /// Insert a part (default: at the end)
    Add {
        /// Insert position: index or part id
        #[arg(long, global = true, value_name = "N")]
        at: Option<String>,
        #[command(subcommand)]
        part: PartKind,
    },
    /// Replace part N (index or part id)
    Set {
        target: String,
        #[command(subcommand)]
        part: PartKind,
    },
    /// Remove part N (index or part id)
    Rm { target: String },
}

#[derive(Debug, Subcommand)]
enum SessionCmd {
    Rename { id: SessionId, title: String },
    /// Move a session to the trash (kept 30 days), or delete it for good with --purge
    Delete {
        id: SessionId,
        #[arg(long)]
        purge: bool,
    },
    /// Bring a session back from the trash
    Restore { id: SessionId },
    /// Find sessions by title, agent or cwd, then by post and comment text
    Search {
        query: String,
        #[arg(long)]
        limit: Option<u32>,
    },
}

/// One post part read from a file, or - for stdin.
#[derive(Debug, Subcommand)]
enum PartKind {
    /// Markdown prose
    Markdown { file: String },
    /// Source code; the language comes from the file extension
    Code {
        file: String,
        #[arg(long)]
        language: Option<String>,
        /// Label in the code header (default: the file name)
        #[arg(long)]
        filename: Option<String>,
        /// Line number the excerpt starts at
        #[arg(long)]
        line_start: Option<u32>,
    },
    /// A unified or git patch
    Diff {
        file: String,
        /// unified or split
        #[arg(long, default_value = "unified", value_parser = kebab::<DiffLayout>)]
        layout: DiffLayout,
    },
    /// Terminal output; ANSI colors are kept
    Terminal {
        file: String,
        #[arg(long)]
        cols: Option<u32>,
        /// Label in the terminal window chrome
        #[arg(long)]
        term_title: Option<String>,
    },
    /// Any JSON, shown as a collapsible tree
    Json { file: String },
    /// Mermaid diagram source
    Mermaid { file: String },
    /// An image file, uploaded as an asset first
    Image {
        file: String,
        #[arg(long)]
        caption: Option<String>,
        #[arg(long)]
        alt: Option<String>,
    },
    /// A trace file, uploaded as an asset first
    Trace { file: String },
    /// Sandboxed HTML
    Html {
        file: String,
        /// Opt into a kit (repeatable; see `notes kits`)
        #[arg(long, value_delimiter = ',')]
        kit: Vec<String>,
    },
}

/// Lets `to_verb` make a request whose result it needs (asset upload, post version).
type Ask<'a> = dyn FnMut(Verb) -> Result<Value, ErrorBody> + 'a;

const AGENT_HOWTO: &str = r#"## Posting to stage (`notes post`)

stage is a terminal pane beside the user that shows your posts as cards. Each
conversation gets its own session, a thread of posts that never mixes with
another conversation's; you don't pick or create it. These notes never override
system, developer or user instructions.

1. Read `notes guide` once, before your first post.
2. Post: `notes post <kind> <file|-> --title "Short title"`. Kinds: markdown,
   code, diff, terminal, json, mermaid, image, html. On your first post add
   `--session-title "The task"` (it names the thread; later posts ignore it).
   More parts on the same card: `--md F --mermaid F --code F --diff F
   --terminal F --html F`. It prints `<post-id> v1 · session <id> · <kinds>`.
3. Revise a card with `notes post update <post-id> <kind> <file|->` (a new
   version of the same card); don't post a near-duplicate.
4. Feedback: comments the user leaves print on stderr as `feedback:` lines after
   your next post or update; treat them as messages from the user. Between posts,
   check with `notes wait --timeout 1` before you finish a turn or start a big
   change. If your harness shows background output, keep
   `notes wait --timeout 600` running and re-arm it after each comment.
5. Clean up: `notes sessions` lists sessions; `notes session delete <id>` moves one to
   the trash (kept 30 days, safe to repeat), `notes session restore <id>` brings it
   back, `notes session delete <id> --purge` deletes it for good.

## Notes (`notes` CLI)

Notes are plain `.md` files in folders under one vault (`~/Notes`). Change them
through `notes`, not by editing files: writes are atomic, versioned and checked.

- A `<ref>` is `folder/title` (with or without `.md`), a unique bare title, or `@last`.
- Find: `notes find <query>` (paths), `notes search <query> [FOLDER] [-C N]`
  (content, `path:line: text`; add `--fuzzy` for typo-tolerant matching).
- Read: `notes read <ref> [--lines A:B] [-n]`; `--json` adds `version`.
- Edit: `notes edit <ref> --old S --new S` (exact text, must match once unless
  `--all`), or several at once with `--from edits.json` (`[{"old":…,"new":…}]`).
- Add: `notes new <path> TEXT|-`, `notes append <ref> TEXT|- [--under HEADING]`,
  `notes prepend <ref> TEXT|-`.
- Replace: `notes write <ref> - --if-version N` (N from `read --json`).
- Many ops, one call: `notes batch [--atomic] < ops.ndjson`, one
  `{"verb":…,"args":…}` per line.
- `--json` prints machine output (NDJSON for lists). Exit codes: 0 ok, 2 usage,
  3 not found, 4 ambiguous (be more specific), 5 version conflict (re-read, retry),
  6 notesd unreachable. Errors print a `hint:` line on stderr.
- Set `NOTES_AGENT=<your name>` so history records you as the author.
"#;

/// `notes setup`: the block to paste into AGENTS.md or CLAUDE.md.
const SETUP: &str = r#"## Visual previews (stage)

The user keeps stage open in a terminal pane beside you. It shows posts you
publish with the `notes` CLI: diagrams, plans, diffs to review, terminal output,
live html. Use it when showing beats telling. Before your first post, run
`notes agent-howto` (how to post and read the user's comments) and `notes guide`
(how posts should look).
"#;

/// `NOTES_SESSION`: a session to use when `--session` isn't given.
fn env_session() -> Option<SessionId> {
    env::var("NOTES_SESSION").ok().filter(|s| !s.is_empty()).map(SessionId::from)
}

/// The card width stage last reported, if it has run.
fn card_cols(ask: &mut Ask<'_>) -> Option<usize> {
    let key = notes_proto::posts::SettingGetArgs { key: notes_proto::posts::STAGE_CARD_COLS.into() };
    ask(Verb::SettingGet(key)).ok()?.as_str()?.parse().ok()
}

/// Print the design guide's warnings for a post about to be sent.
fn warn(title: Option<&str>, parts: &[Part], ask: &mut Ask<'_>) {
    for w in lint::check(title, parts, card_cols(ask).unwrap_or(lint::CARD_COLS)) {
        eprintln!("warning: {w} (see `notes guide`)");
    }
}

/// Parse a flag value as one of a proto enum's kebab-case names.
fn kebab<T: DeserializeOwned>(s: &str) -> Result<T, String> {
    serde_json::from_value(Value::from(s)).map_err(|e| e.to_string())
}

fn parse_lines(s: &str) -> Result<LineRange, String> {
    let (start, end) = s.split_once(':').ok_or("expected A:B")?;
    let num = |n: &str| n.parse::<u32>().map_err(|e| format!("{n:?}: {e}"));
    Ok(LineRange {
        start: num(start)?,
        end: num(end)?,
    })
}

fn fail(code: ErrorCode, message: impl Into<String>, hint: &str) -> ErrorBody {
    ErrorBody {
        code,
        message: message.into(),
        hint: Some(hint.into()),
        details: None,
    }
}

fn read_all(source: &str, stdin: &mut dyn Read) -> Result<String, ErrorBody> {
    let text = if source == "-" {
        let mut text = String::new();
        stdin.read_to_string(&mut text).map(|_| text)
    } else {
        fs::read_to_string(source)
    };
    text.map_err(|e| {
        fail(
            ErrorCode::Other,
            format!("can't read {source}: {e}"),
            "check the path, or pass - and pipe the text in",
        )
    })
}

/// `TEXT|-` arguments: literal text, or stdin when it's `-`.
fn text_arg(arg: String, stdin: &mut dyn Read) -> Result<String, ErrorBody> {
    if arg == "-" {
        read_all("-", stdin)
    } else {
        Ok(arg)
    }
}

/// Standard padded base64 (RFC 4648) for asset uploads.
fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | (u32::from(b) << (16 - 8 * i)));
        for i in 0..4 {
            out.push(if i <= chunk.len() {
                ABC[((n >> (18 - 6 * i)) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    out
}

/// Language id for a code part, from the file extension (sideshow's table).
fn language_for(file: &str) -> Option<String> {
    let ext = Path::new(file).extension()?.to_str()?.to_ascii_lowercase();
    let language = match ext.as_str() {
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "jsx",
        "py" => "python",
        "rb" => "ruby",
        "go" => "go",
        "rs" => "rust",
        "java" => "java",
        "kt" => "kotlin",
        "swift" => "swift",
        "c" | "h" => "c",
        "cpp" | "cc" | "hpp" => "cpp",
        "cs" => "csharp",
        "php" => "php",
        "sh" | "bash" | "zsh" | "fish" => "bash",
        "yml" | "yaml" => "yaml",
        "toml" => "toml",
        "json" | "jsonl" => "json",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" => "scss",
        "sql" => "sql",
        "md" | "markdown" => "markdown",
        "lua" => "lua",
        _ => return None,
    };
    Some(language.into())
}

fn content_type_for(file: &str) -> &'static str {
    let ext = Path::new(file)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("json") => "application/json",
        Some("jsonl" | "ndjson") => "application/x-ndjson",
        Some("txt" | "log") => "text/plain",
        Some("md") => "text/markdown",
        Some("html" | "htm") => "text/html",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
}

fn asset_put(
    file: &str,
    kind: Option<AssetKind>,
    session: Option<SessionId>,
) -> Result<Verb, ErrorBody> {
    if file == "-" {
        return Err(fail(
            ErrorCode::Usage,
            "uploads read a file, not stdin",
            "save the bytes to a file and pass its path",
        ));
    }
    let bytes = fs::read(file).map_err(|e| {
        fail(
            ErrorCode::Other,
            format!("can't read {file}: {e}"),
            "check the path",
        )
    })?;
    let content_type = content_type_for(file);
    let default_kind = if content_type.starts_with("image/") {
        AssetKind::Image
    } else {
        AssetKind::File
    };
    Ok(Verb::AssetPut(AssetPutArgs {
        session,
        kind: kind.unwrap_or(default_kind),
        content_type: content_type.into(),
        filename: Path::new(file)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned()),
        data_base64: base64(&bytes),
    }))
}

/// Build one post part; image and trace files are uploaded as assets first.
fn build_part(
    kind: PartKind,
    session: &Option<SessionId>,
    stdin: &mut dyn Read,
    ask: &mut Ask<'_>,
) -> Result<Part, ErrorBody> {
    let mut upload = |file: &str, kind| -> Result<AssetId, ErrorBody> {
        let info = ask(asset_put(file, Some(kind), session.clone())?)?;
        serde_json::from_value::<AssetInfo>(info)
            .map(|info| info.id)
            .map_err(bad)
    };
    Ok(match kind {
        PartKind::Markdown { file } => Part::Markdown {
            markdown: read_all(&file, stdin)?,
            id: None,
        },
        PartKind::Code {
            file,
            language,
            filename,
            line_start,
        } => Part::Code {
            code: read_all(&file, stdin)?,
            language: language.or_else(|| language_for(&file)),
            title: filename.or_else(|| {
                (file != "-")
                    .then(|| Path::new(&file).file_name())
                    .flatten()
                    .map(|n| n.to_string_lossy().into_owned())
            }),
            line_start,
            id: None,
        },
        PartKind::Diff { file, layout } => Part::Diff {
            patch: Some(read_all(&file, stdin)?),
            files: Vec::new(),
            layout,
            id: None,
        },
        PartKind::Terminal {
            file,
            cols,
            term_title,
        } => Part::Terminal {
            text: read_all(&file, stdin)?,
            cols,
            title: term_title,
            id: None,
        },
        PartKind::Json { file } => Part::Json {
            data: serde_json::from_str(&read_all(&file, stdin)?).map_err(|e| {
                fail(
                    ErrorCode::Usage,
                    format!("bad JSON in {file}: {e}"),
                    "a json part takes any valid JSON value",
                )
            })?,
            id: None,
        },
        PartKind::Mermaid { file } => Part::Mermaid {
            mermaid: read_all(&file, stdin)?,
            id: None,
        },
        PartKind::Image { file, caption, alt } => Part::Image {
            asset_id: upload(&file, AssetKind::Image)?,
            alt,
            caption,
            id: None,
        },
        PartKind::Trace { file } => Part::Trace {
            steps: Vec::new(),
            asset_id: Some(upload(&file, AssetKind::Trace)?),
            title: None,
            id: None,
        },
        PartKind::Html { file, kit } => Part::Html {
            html: read_all(&file, stdin)?,
            kits: kit,
            id: None,
        },
    })
}

/// Trace steps from a JSON array, or JSONL with one step per line.
fn trace_steps(input: &str) -> Result<Vec<TraceStep>, ErrorBody> {
    if let Ok(steps) = serde_json::from_str(input) {
        return Ok(steps);
    }
    input
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(i, line)| {
            serde_json::from_str(line).map_err(|e| {
                fail(
                    ErrorCode::Usage,
                    format!("trace line {}: {e}", i + 1),
                    r#"pass a JSON array, or one {"label":…} step per line"#,
                )
            })
        })
        .collect()
}

fn to_verb(
    cmd: Cmd,
    if_version: Option<u64>,
    stdin: &mut dyn Read,
    ask: &mut Ask<'_>,
) -> Result<Verb, ErrorBody> {
    if if_version.is_some()
        && !matches!(
            cmd,
            Cmd::Edit { .. } | Cmd::Append { .. } | Cmd::Prepend { .. } | Cmd::Write { .. }
        )
    {
        return Err(fail(
            ErrorCode::Usage,
            "--if-version only applies to edit, append, prepend and write",
            "drop --if-version",
        ));
    }
    Ok(match cmd {
        Cmd::List {
            folder,
            recursive,
            sort,
            limit,
        } => Verb::List(ListArgs {
            folder,
            recursive,
            sort,
            limit,
        }),
        Cmd::Find { query, limit } => Verb::Find(FindArgs { query, limit }),
        Cmd::Search {
            query,
            folder,
            context,
            case,
            fuzzy,
            limit,
        } => Verb::Search(SearchArgs {
            query,
            folder,
            context,
            case_sensitive: case,
            fuzzy,
            limit,
        }),
        Cmd::Read { note, lines, .. } => Verb::Read(ReadArgs { note, lines }),
        Cmd::New { path, text } => {
            let body = text
                .map(|t| text_arg(t, stdin))
                .transpose()?
                .unwrap_or_default();
            Verb::New(NewArgs { path, body })
        }
        Cmd::Edit {
            note,
            old,
            new,
            all,
            from,
        } => {
            let edits = match (old, new, from) {
                (Some(old), Some(new), None) => vec![Replacement { old, new, all }],
                (None, None, Some(source)) => serde_json::from_str(&read_all(&source, stdin)?)
                    .map_err(|e| {
                        fail(
                            ErrorCode::Usage,
                            format!("bad edits JSON: {e}"),
                            r#"pass a JSON array like [{"old":"foo","new":"bar"}]"#,
                        )
                    })?,
                _ => {
                    return Err(fail(
                        ErrorCode::Usage,
                        "edit needs --old and --new, or --from",
                        "run `notes help edit`",
                    ))
                }
            };
            Verb::Edit(EditArgs {
                note,
                edits,
                if_version,
            })
        }
        Cmd::Append { note, text, under } => Verb::Append(AppendArgs {
            note,
            text: text_arg(text, stdin)?,
            under,
            if_version,
        }),
        Cmd::Prepend { note, text } => Verb::Prepend(PrependArgs {
            note,
            text: text_arg(text, stdin)?,
            if_version,
        }),
        Cmd::Write { note, text, force } => {
            if if_version.is_none() && !force {
                return Err(fail(
                    ErrorCode::Usage,
                    "write replaces the whole note",
                    "pass --if-version N (from `notes read --json`) or --force",
                ));
            }
            Verb::Write(WriteArgs {
                note,
                body: text_arg(text, stdin)?,
                if_version,
                force,
            })
        }
        Cmd::Move { note, to } => Verb::Move(MoveArgs { note, to }),
        Cmd::Mkdir { folder } => Verb::Mkdir(FolderArgs { folder }),
        Cmd::Rmdir { folder } => Verb::Rmdir(FolderArgs { folder }),
        Cmd::Delete { note, purge } => Verb::Delete(DeleteArgs { note, purge }),
        Cmd::Restore { note } => Verb::Restore(NoteArgs { note }),
        Cmd::History { note, limit } => Verb::History(HistoryArgs { note, limit }),
        Cmd::Diff { note, from, to } => Verb::Diff(DiffArgs { note, from, to }),
        Cmd::Revert { note, to } => Verb::Revert(RevertArgs { note, to }),
        Cmd::Watch { since, kind } => Verb::Watch(WatchArgs { since, kinds: kind }),
        Cmd::Batch { atomic } => {
            let input = read_all("-", stdin)?;
            let ops = input
                .lines()
                .enumerate()
                .filter(|(_, line)| !line.trim().is_empty())
                .map(|(i, line)| {
                    serde_json::from_str(line).map_err(|e| {
                        fail(
                            ErrorCode::Usage,
                            format!("batch line {}: {e}", i + 1),
                            r#"each stdin line is one {"verb":…,"args":…} op"#,
                        )
                    })
                })
                .collect::<Result<_, _>>()?;
            Verb::Batch(BatchArgs { ops, atomic })
        }
        Cmd::Post {
            title,
            session,
            session_title,
            more,
            cmd,
        } => match cmd {
            PostCmd::New(kind) => {
                let session = session.or_else(env_session);
                let parts = std::iter::once(kind)
                    .chain(more.kinds())
                    .map(|k| build_part(k, &session, stdin, ask))
                    .collect::<Result<Vec<_>, _>>()?;
                warn(title.as_deref(), &parts, ask);
                Verb::PostNew(PostNewArgs { parts, session, title, session_title, owner: owner::key() })
            }
            PostCmd::Update { id, part } => {
                let kinds: Vec<PartKind> = part.into_iter().chain(more.kinds()).collect();
                let parts = if kinds.is_empty() {
                    None
                } else {
                    Some(kinds.into_iter().map(|k| build_part(k, &session, stdin, ask)).collect::<Result<Vec<_>, _>>()?)
                };
                warn(title.as_deref(), parts.as_deref().unwrap_or_default(), ask);
                Verb::PostUpdate(PostUpdateArgs { id, title, parts })
            }
            PostCmd::Part { id, op } => {
                let (op, target, part) = match op {
                    PartCmd::Add { at, part } => (PartOp::Add, at, Some(part)),
                    PartCmd::Set { target, part } => (PartOp::Set, Some(target), Some(part)),
                    PartCmd::Rm { target } => (PartOp::Rm, Some(target), None),
                };
                let part = part
                    .map(|p| build_part(p, &session, stdin, ask))
                    .transpose()?;
                warn(title.as_deref(), part.as_slice(), ask);
                Verb::PostPart(PostPartArgs {
                    id,
                    op,
                    target,
                    part,
                })
            }
            PostCmd::List { limit } => Verb::PostList(PostListArgs { session, limit }),
            PostCmd::Show { id, version } => Verb::PostShow(PostShowArgs { id, version }),
            PostCmd::Delete { id } => Verb::PostDelete(IdArgs { id }),
        },
        Cmd::Upload {
            file,
            kind,
            session,
        } => asset_put(&file, kind, session)?,
        Cmd::Comment {
            post,
            text,
            line,
            part,
        } => {
            let anchor = match line {
                None => None,
                Some(lines) => {
                    // An anchor pins a post version, so look up the current one.
                    let shown = ask(Verb::PostShow(PostShowArgs {
                        id: post.clone(),
                        version: None,
                    }))?;
                    let post_version = shown["version"]
                        .as_u64()
                        .ok_or_else(|| bad("post-show result has no version"))?;
                    Some(CommentAnchor::LineRange {
                        part: part.unwrap_or(0),
                        post_version,
                        start_line: lines.start,
                        end_line: lines.end,
                        file: None,
                    })
                }
            };
            Verb::CommentNew(CommentNewArgs {
                post: Some(post),
                session: None,
                text: text_arg(text, stdin)?,
                anchor,
                author: None,
            })
        }
        Cmd::Comments {
            post,
            session,
            after,
        } => Verb::CommentList(CommentListArgs {
            session,
            post,
            after_seq: after,
        }),
        Cmd::Wait {
            post,
            session,
            timeout,
        } => Verb::Wait(WaitArgs {
            session: session.or_else(env_session),
            owner: owner::key(),
            post,
            timeout_secs: timeout,
        }),
        Cmd::Sessions => Verb::SessionList,
        Cmd::Session {
            cmd: SessionCmd::Rename { id, title },
        } => Verb::SessionRename(SessionRenameArgs { id, title }),
        Cmd::Session {
            cmd: SessionCmd::Delete { id, purge },
        } => Verb::SessionDelete(SessionDeleteArgs { id, purge }),
        Cmd::Session {
            cmd: SessionCmd::Restore { id },
        } => Verb::SessionRestore(IdArgs { id }),
        Cmd::Session {
            cmd: SessionCmd::Search { query, limit },
        } => Verb::SessionSearch(SessionSearchArgs { query, limit }),
        Cmd::Trace { file, session } => Verb::TraceSet(TraceSetArgs {
            session,
            steps: trace_steps(&read_all(&file, stdin)?)?,
        }),
        Cmd::Kits => Verb::Kits,
        Cmd::Manifest => Verb::Manifest,
        Cmd::Doctor => Verb::Doctor,
        Cmd::AgentHowto | Cmd::Guide | Cmd::Setup | Cmd::Completions { .. } => {
            unreachable!("offline verbs are handled in main")
        }
    })
}

fn unreachable(sock: &Path, e: io::Error) -> ErrorBody {
    let hint = match e.kind() {
        io::ErrorKind::NotFound => "no socket, so notesd isn't running: start `notesd` (or check its launchd job with `launchctl list | grep notesd`), or point --sock / NOTES_SOCK at its socket",
        io::ErrorKind::ConnectionRefused => "stale socket, nothing is listening: restart notesd (`launchctl list | grep notesd` shows its job)",
        _ => "start notesd, or point --sock / NOTES_SOCK at its socket",
    };
    fail(
        ErrorCode::DaemonUnreachable,
        format!("notesd unreachable at {}: {e}", sock.display()),
        hint,
    )
}

fn bad(e: impl std::fmt::Display) -> ErrorBody {
    fail(
        ErrorCode::Other,
        format!("bad response from notesd: {e}"),
        "the CLI and notesd may speak different protocol versions; run `notes doctor`",
    )
}

fn closed() -> ErrorBody {
    fail(
        ErrorCode::Other,
        "notesd closed the connection without a response",
        "run `notes doctor`",
    )
}

/// Connect and send one request line.
fn send(sock: &Path, request: &Request) -> Result<BufReader<UnixStream>, ErrorBody> {
    let mut stream = UnixStream::connect(sock).map_err(|e| unreachable(sock, e))?;
    stream
        .write_all(to_line(request).as_bytes())
        .map_err(|e| unreachable(sock, e))?;
    Ok(BufReader::new(stream))
}

/// Read one response line; `None` once the daemon closes the connection.
fn receive(
    reader: &mut BufReader<UnixStream>,
    sock: &Path,
    line: &mut String,
) -> Result<Option<Response>, ErrorBody> {
    line.clear();
    if reader.read_line(line).map_err(|e| unreachable(sock, e))? == 0 {
        return Ok(None);
    }
    serde_json::from_str(line).map(Some).map_err(bad)
}

/// One request whose result the CLI needs before the main one.
fn ask(sock: &Path, request: &Request) -> Result<Value, ErrorBody> {
    match receive(&mut send(sock, request)?, sock, &mut String::new())? {
        Some(Response::Ok { result, .. }) => Ok(result),
        Some(Response::Err { error, .. }) => Err(error),
        Some(Response::Event { .. }) | None => Err(closed()),
    }
}

/// One comment on one line: `#seq author on post ID: text`.
fn comment_line(c: &Comment) -> String {
    let text = c.text.split_whitespace().collect::<Vec<_>>().join(" ");
    let on = c
        .post
        .as_deref()
        .map_or_else(|| "the session".into(), |p| format!("post {p}"));
    format!("#{} {} on {on}: {text}", c.seq, c.author)
}

/// Send one request and print what comes back; events stream until the daemon closes.
/// Returns the exit code: 0, or the first failed batch op's.
fn call(
    sock: &Path,
    request: &Request,
    json: bool,
    quiet: bool,
    number: bool,
) -> Result<u8, ErrorBody> {
    let mut reader = send(sock, request)?;
    let mut out = io::stdout().lock();
    let mut line = String::new();
    loop {
        let Some(response) = receive(&mut reader, sock, &mut line)? else {
            if matches!(request.verb, Verb::Watch(_)) {
                return Ok(0);
            }
            return Err(closed());
        };
        match response {
            Response::Event { event, .. } => {
                // A closed stdout (e.g. `notes watch | head`) ends the stream quietly.
                if !quiet
                    && writeln!(
                        out,
                        "{}",
                        serde_json::to_string(&event).expect("event serializes")
                    )
                    .is_err()
                {
                    return Ok(0);
                }
            }
            Response::Ok { result, .. } => {
                let outcomes: Vec<BatchOutcome> = if matches!(request.verb, Verb::Batch(_)) {
                    serde_json::from_value(result.clone()).map_err(bad)?
                } else {
                    Vec::new()
                };
                // User replies ride on post writes and the daemon hands each out
                // once, so they go to stderr even with -q or --json.
                if matches!(
                    request.verb,
                    Verb::PostNew(_) | Verb::PostUpdate(_) | Verb::PostPart(_)
                ) {
                    let feedback = result.get("feedback").cloned().unwrap_or_default();
                    for c in serde_json::from_value::<Vec<Comment>>(feedback).unwrap_or_default() {
                        eprintln!("feedback: {}", comment_line(&c));
                    }
                }
                if !quiet {
                    let _ = out.write_all(render(&request.verb, result, json, number).as_bytes());
                    let _ = out.flush();
                }
                // Every op's line is printed first; the exit code is the first failed op's.
                let failed =
                    outcomes
                        .into_iter()
                        .enumerate()
                        .find_map(|(i, outcome)| match outcome {
                            BatchOutcome::Err { error, .. } => Some(ErrorBody {
                                message: format!("batch op {}: {}", i + 1, error.message),
                                ..error
                            }),
                            BatchOutcome::Ok { .. } => None,
                        });
                return Ok(failed.map_or(0, |error| report(&error, false)));
            }
            Response::Err { error, .. } => return Err(error),
        }
    }
}

fn render(verb: &Verb, result: Value, json: bool, number: bool) -> String {
    // `batch` is NDJSON either way: one `BatchOutcome` per op.
    if json || matches!(verb, Verb::Batch(_)) {
        let items = match result {
            Value::Array(items) => items,
            other => vec![other],
        };
        return items.iter().map(|item| format!("{item}\n")).collect();
    }
    text(verb, &result, number).unwrap_or_else(|| format!("{result}\n"))
}

/// Readable output per verb; `None` when the result isn't the expected shape.
fn text(verb: &Verb, result: &Value, number: bool) -> Option<String> {
    let mut out = String::new();
    match verb {
        Verb::List(_) => {
            for entry in serde_json::from_value::<Vec<ListEntry>>(result.clone()).ok()? {
                match entry {
                    ListEntry::Folder { folder } => _ = writeln!(out, "{folder}/"),
                    ListEntry::Note(note) => _ = writeln!(out, "{}", note.path),
                }
            }
        }
        Verb::Find(_) => {
            let hits: Vec<FindHit> = serde_json::from_value(result.clone()).ok()?;
            for hit in &hits {
                _ = writeln!(out, "{}", hit.path);
            }
        }
        Verb::Search(_) => {
            let hits: Vec<SearchHit> = serde_json::from_value(result.clone()).ok()?;
            for hit in &hits {
                let first = hit.line.saturating_sub(hit.before.len() as u32);
                for (text, n) in hit.before.iter().zip(first..) {
                    _ = writeln!(out, "{}-{n}- {text}", hit.path);
                }
                _ = writeln!(out, "{}:{}: {}", hit.path, hit.line, hit.text);
                for (text, n) in hit.after.iter().zip(hit.line + 1..) {
                    _ = writeln!(out, "{}-{n}- {text}", hit.path);
                }
            }
        }
        Verb::Read(args) => {
            let note: Note = serde_json::from_value(result.clone()).ok()?;
            if number {
                let start = args.lines.map_or(1, |l| l.start);
                for (line, n) in note.body.lines().zip(start..) {
                    _ = writeln!(out, "{n:6}\t{line}");
                }
            } else {
                out = note.body;
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
            }
        }
        Verb::New(_) => {
            _ = writeln!(
                out,
                "{}",
                serde_json::from_value::<WriteResult>(result.clone())
                    .ok()?
                    .path
            )
        }
        Verb::History(_) => {
            let entries: Vec<HistoryEntry> = serde_json::from_value(result.clone()).ok()?;
            for entry in &entries {
                _ = writeln!(
                    out,
                    "v{}\t{}\t{}",
                    entry.version,
                    utc(entry.at),
                    entry.author
                );
            }
        }
        Verb::Diff(_) => {
            out = serde_json::from_value::<DiffResult>(result.clone())
                .ok()?
                .unified
        }
        Verb::Doctor => {
            let d: DoctorResult = serde_json::from_value(result.clone()).ok()?;
            _ = writeln!(out, "cli       {}", env!("CARGO_PKG_VERSION"));
            _ = writeln!(
                out,
                "notesd    {} (protocol {})",
                d.daemon_version, d.protocol
            );
            _ = writeln!(out, "socket    {}", d.socket.display());
            _ = writeln!(out, "vault     {}", d.vault.display());
            _ = writeln!(out, "database  {}", d.database.display());
            _ = writeln!(out, "notes     {}", d.notes);
            if let Some(stage) = &d.stage {
                let path = stage["path"].as_str().unwrap_or("");
                _ = writeln!(out, "sessions  {} in {path}", stage["sessions"]);
                if let Some(error) = stage["error"].as_str() {
                    _ = writeln!(out, "          error: {error}");
                }
                for skipped in stage["skipped"].as_array().into_iter().flatten() {
                    _ = writeln!(out, "          skipped {}", skipped.as_str().unwrap_or(""));
                }
                if let Some(m) = stage["migration"].as_str().filter(|m| m.starts_with("failed")) {
                    _ = writeln!(out, "          migration {m}");
                }
            }
        }
        Verb::Manifest => out = serde_json::to_string_pretty(result).ok()? + "\n",
        Verb::PostNew(_) | Verb::PostUpdate(_) | Verb::PostPart(_) => {
            let w: PostWriteResult = serde_json::from_value(result.clone()).ok()?;
            let p = &w.post;
            _ = writeln!(out, "{} v{} · session {} · {}", p.id, p.version, p.session, p.kinds.join(", "));
        }
        Verb::PostList(_) => {
            for p in serde_json::from_value::<Vec<PostSummary>>(result.clone()).ok()? {
                let kinds = p.kinds.join(",");
                _ = writeln!(out, "{}\tv{}\t{kinds}\t{}", p.id, p.version, p.title);
            }
        }
        Verb::PostShow(_) => {
            let post: Post = serde_json::from_value(result.clone()).ok()?;
            _ = writeln!(out, "{}\tv{}\t{}", post.id, post.version, post.title);
            for (i, part) in post.parts.iter().enumerate() {
                let part = serde_json::to_value(part).ok()?;
                let id = part["id"].as_str().unwrap_or("-");
                _ = writeln!(out, "  {i}\t{}\t{id}", part["kind"].as_str()?);
            }
        }
        Verb::AssetPut(_) => {
            let info: AssetInfo = serde_json::from_value(result.clone()).ok()?;
            _ = writeln!(out, "{}", info.id);
        }
        Verb::CommentNew(_) | Verb::Wait(_) if !result.is_null() => {
            let c: Comment = serde_json::from_value(result.clone()).ok()?;
            _ = writeln!(out, "{}", comment_line(&c));
        }
        Verb::CommentList(_) => {
            for c in serde_json::from_value::<Vec<Comment>>(result.clone()).ok()? {
                _ = writeln!(out, "{}", comment_line(&c));
            }
        }
        Verb::SessionList => {
            for s in serde_json::from_value::<Vec<Session>>(result.clone()).ok()? {
                let title = s.title.as_deref().unwrap_or("");
                let active = utc(s.last_active_at);
                _ = writeln!(
                    out,
                    "{}\t{}\t{} posts\t{active}\t{title}",
                    s.id, s.agent, s.post_count
                );
            }
        }
        Verb::SessionRename(_) => {
            if let Ok(s) = serde_json::from_value::<Session>(result.clone()) {
                _ = writeln!(out, "{}\t{}", s.id, s.title.as_deref().unwrap_or(""));
            }
        }
        Verb::SessionDelete(_) => {
            let r: SessionDeleteResult = serde_json::from_value(result.clone()).ok()?;
            let state = if r.purged { "purged" } else { "trashed" };
            _ = writeln!(out, "{}\t{state}{}", r.id, if r.already { " (already)" } else { "" });
        }
        Verb::SessionRestore(_) => {
            let r: SessionRestoreResult = serde_json::from_value(result.clone()).ok()?;
            _ = writeln!(out, "{}\trestored{}", r.id, if r.already { " (already)" } else { "" });
        }
        Verb::SessionSearch(_) => {
            for h in serde_json::from_value::<Vec<SessionHit>>(result.clone()).ok()? {
                let title = h.session.title.as_deref().unwrap_or("");
                let found = h.text.as_deref().unwrap_or("");
                _ = writeln!(out, "{}\t{}\t{title}\t{found}", h.session.id, h.session.agent);
            }
        }
        Verb::Kits => {
            for k in serde_json::from_value::<Vec<Kit>>(result.clone()).ok()? {
                _ = writeln!(out, "{}\t{}: {}", k.id, k.label, k.summary);
            }
        }
        _ => match result {
            Value::Null => {}
            Value::String(s) => _ = writeln!(out, "{s}"),
            _ => {
                let w: WriteResult = serde_json::from_value(result.clone()).ok()?;
                _ = writeln!(out, "{} v{}", w.path, w.version);
            }
        },
    }
    Some(out)
}

/// Unix seconds as `YYYY-MM-DD HH:MMZ` (Hinnant's civil-from-days).
fn utc(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let (era, doe) = (z.div_euclid(146_097), z.rem_euclid(146_097));
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60
    )
}

fn report(error: &ErrorBody, json: bool) -> u8 {
    if json {
        let _ = writeln!(
            io::stdout(),
            "{}",
            serde_json::to_string(error).expect("error serializes")
        );
    }
    let retry = error.current_version().map(|n| {
        format!("current version is {n}; re-read the note, or retry with --if-version {n}")
    });
    let hint = retry
        .as_deref()
        .or(error.hint.as_deref())
        .unwrap_or(match error.code {
            ErrorCode::Usage => "run `notes help`",
            ErrorCode::NotFound => "run `notes find <query>` to look up the path",
            ErrorCode::Ambiguous => "use the full vault path, or add surrounding text to --old",
            ErrorCode::VersionConflict => {
                "run `notes read --json <ref>` for the current version, then retry"
            }
            ErrorCode::DaemonUnreachable => {
                "start notesd, or point --sock / NOTES_SOCK at its socket"
            }
            ErrorCode::Other => "run `notes doctor`",
        });
    eprintln!("notes: {}\nhint: {hint}", error.message);
    error.code.exit_code()
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::AgentHowto => {
            let _ = io::stdout().write_all(AGENT_HOWTO.as_bytes());
            return ExitCode::SUCCESS;
        }
        Cmd::Setup => {
            let _ = io::stdout().write_all(SETUP.as_bytes());
            return ExitCode::SUCCESS;
        }
        Cmd::Guide => {
            let mut out = include_str!("guide.md").to_owned();
            let sock = cli.sock.clone().unwrap_or_else(default_socket_path);
            let request = |verb| Request { v: PROTOCOL_VERSION, id: 1, verb, cwd: None, agent: None };
            if let Some(cols) = card_cols(&mut |verb| ask(&sock, &request(verb))) {
                out.push_str(&format!(
                    "\n## Right now\n\nThe user's stage card is {cols} cells wide. Write for that width: a\ntable row, code line or diagram label wider than {cols} wraps.\n"
                ));
            }
            let _ = io::stdout().write_all(out.as_bytes());
            return ExitCode::SUCCESS;
        }
        Cmd::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "notes", &mut io::stdout());
            return ExitCode::SUCCESS;
        }
        _ => {}
    }
    let number = matches!(cli.cmd, Cmd::Read { number: true, .. });
    let sock = cli.sock.unwrap_or_else(default_socket_path);
    let (cwd, agent) = (
        env::current_dir().ok(),
        env::var("NOTES_AGENT").ok().filter(|a| !a.is_empty()),
    );
    let request = |verb| Request {
        v: PROTOCOL_VERSION,
        id: 1,
        verb,
        cwd: cwd.clone(),
        agent: agent.clone(),
    };
    let outcome = to_verb(cli.cmd, cli.if_version, &mut io::stdin(), &mut |verb| {
        ask(&sock, &request(verb))
    })
    .and_then(|verb| call(&sock, &request(verb), cli.json, cli.quiet, number));
    match outcome {
        Ok(code) => ExitCode::from(code),
        Err(error) => ExitCode::from(report(&error, cli.json)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verb(args: &[&str], stdin: &str) -> Result<Verb, ErrorBody> {
        let cli =
            Cli::try_parse_from(std::iter::once("notes").chain(args.iter().copied())).unwrap();
        to_verb(cli.cmd, cli.if_version, &mut stdin.as_bytes(), &mut |v| match v {
            // Lint asks for stage's card width and falls back without a daemon.
            Verb::SettingGet(_) => Err(closed()),
            _ => panic!("unit tests have no daemon"),
        })
    }

    fn usage_code(args: &[&str]) -> ErrorCode {
        verb(args, "").unwrap_err().code
    }

    #[test]
    fn read_and_list_args() {
        assert_eq!(
            verb(
                &["list", "work", "-r", "--sort", "title", "--limit", "5"],
                ""
            )
            .unwrap(),
            Verb::List(ListArgs {
                folder: Some("work".into()),
                recursive: true,
                sort: ListSort::Title,
                limit: Some(5)
            })
        );
        assert_eq!(
            verb(&["list"], "").unwrap(),
            Verb::List(ListArgs::default())
        );
        assert_eq!(
            verb(&["show", "plan", "--lines", "2:5", "-n"], "").unwrap(),
            Verb::Read(ReadArgs {
                note: "plan".into(),
                lines: Some(LineRange { start: 2, end: 5 })
            })
        );
        assert_eq!(
            verb(&["search", "todo", "work", "-C", "2", "--case"], "").unwrap(),
            Verb::Search(SearchArgs {
                query: "todo".into(),
                folder: Some("work".into()),
                context: 2,
                case_sensitive: true,
                fuzzy: false,
                limit: None
            })
        );
        let Verb::Search(args) = verb(&["search", "todo", "--fuzzy", "--limit", "3"], "").unwrap()
        else {
            panic!("expected search")
        };
        assert!(args.fuzzy && args.limit == Some(3));
        assert!(Cli::try_parse_from(["notes", "read", "x", "--lines", "5"]).is_err());
        assert!(Cli::try_parse_from(["notes", "list", "--sort", "size"]).is_err());
    }

    #[test]
    fn edit_args() {
        assert_eq!(
            verb(
                &[
                    "edit",
                    "plan",
                    "--old",
                    "a",
                    "--new",
                    "b",
                    "--all",
                    "--if-version",
                    "3"
                ],
                ""
            )
            .unwrap(),
            Verb::Edit(EditArgs {
                note: "plan".into(),
                edits: vec![Replacement {
                    old: "a".into(),
                    new: "b".into(),
                    all: true
                }],
                if_version: Some(3)
            })
        );
        let from_stdin = verb(
            &["edit", "plan", "--from", "-"],
            r#"[{"old":"a","new":"b"},{"old":"c","new":"d","all":true}]"#,
        )
        .unwrap();
        let Verb::Edit(EditArgs { edits, .. }) = from_stdin else {
            panic!("expected edit")
        };
        assert_eq!(edits.len(), 2);
        assert!(edits[1].all);
        assert_eq!(
            verb(&["edit", "plan", "--from", "-"], "{oops")
                .unwrap_err()
                .code,
            ErrorCode::Usage
        );
        assert!(Cli::try_parse_from(["notes", "edit", "plan"]).is_err());
        assert!(Cli::try_parse_from(["notes", "edit", "plan", "--old", "a"]).is_err());
        assert!(Cli::try_parse_from([
            "notes", "edit", "plan", "--old", "a", "--new", "b", "--from", "-"
        ])
        .is_err());
    }

    #[test]
    fn write_family_args() {
        assert_eq!(
            verb(&["append", "plan", "-", "--under", "Todo"], "- milk\n").unwrap(),
            Verb::Append(AppendArgs {
                note: "plan".into(),
                text: "- milk\n".into(),
                under: Some("Todo".into()),
                if_version: None
            })
        );
        assert_eq!(
            verb(&["new", "work/plan"], "").unwrap(),
            Verb::New(NewArgs {
                path: "work/plan".into(),
                body: String::new()
            })
        );
        assert_eq!(usage_code(&["write", "plan", "-"]), ErrorCode::Usage);
        assert_eq!(
            verb(&["write", "plan", "-", "--if-version", "7"], "new body").unwrap(),
            Verb::Write(WriteArgs {
                note: "plan".into(),
                body: "new body".into(),
                if_version: Some(7),
                force: false
            })
        );
        assert_eq!(
            usage_code(&["move", "a", "b", "--if-version", "1"]),
            ErrorCode::Usage
        );
        assert_eq!(
            verb(&["delete", "a", "--purge"], "").unwrap(),
            Verb::Delete(DeleteArgs {
                note: "a".into(),
                purge: true
            })
        );
        assert_eq!(
            verb(&["revert", "a", "--to", "4"], "").unwrap(),
            Verb::Revert(RevertArgs {
                note: "a".into(),
                to: 4
            })
        );
    }

    #[test]
    fn watch_and_batch_args() {
        assert_eq!(
            verb(&["watch", "--since", "12", "--kind", "note,post"], "").unwrap(),
            Verb::Watch(WatchArgs {
                since: Some(12),
                kinds: vec!["note".into(), "post".into()]
            })
        );
        let ops = "{\"verb\":\"read\",\"args\":{\"note\":\"a\"}}\n\n{\"verb\":\"doctor\"}\n";
        assert_eq!(
            verb(&["batch", "--atomic"], ops).unwrap(),
            Verb::Batch(BatchArgs {
                ops: vec![
                    Verb::Read(ReadArgs {
                        note: "a".into(),
                        lines: None
                    }),
                    Verb::Doctor
                ],
                atomic: true
            })
        );
        let err = verb(&["batch"], "{\"verb\":\"doctor\"}\nnope\n").unwrap_err();
        assert!(err.message.starts_with("batch line 2:"), "{}", err.message);
    }

    #[test]
    fn more_parts_follow_the_main_one() {
        let dir = std::env::temp_dir().join(format!("notes-more-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let (flow, patch) = (dir.join("flow.mmd"), dir.join("fix.patch"));
        fs::write(&flow, "flowchart TD\nA-->B\n").unwrap();
        fs::write(&patch, "--- a/x\n+++ b/x\n").unwrap();
        let Verb::PostNew(args) = verb(
            &["post", "markdown", "-", "--diff", patch.to_str().unwrap(), "--mermaid", flow.to_str().unwrap()],
            "# Why",
        )
        .unwrap() else {
            panic!("not a post-new")
        };
        let kinds: Vec<_> = args.parts.iter().map(|p| serde_json::to_value(p).unwrap()["kind"].clone()).collect();
        assert_eq!(kinds, ["markdown", "mermaid", "diff"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn post_args() {
        assert_eq!(
            verb(&["post", "markdown", "-", "--title", "Plan"], "# Hi").unwrap(),
            Verb::PostNew(PostNewArgs {
                session: None,
                title: Some("Plan".into()),
                parts: vec![Part::Markdown {
                    markdown: "# Hi".into(),
                    id: None
                }],
                session_title: None,
                owner: owner::key(),
            })
        );
        assert_eq!(
            verb(
                &["post", "part", "p1", "add", "--at", "0", "diff", "-", "--layout", "split"],
                "--- a\n"
            )
            .unwrap(),
            Verb::PostPart(PostPartArgs {
                id: "p1".into(),
                op: PartOp::Add,
                target: Some("0".into()),
                part: Some(Part::Diff {
                    patch: Some("--- a\n".into()),
                    files: vec![],
                    layout: DiffLayout::Split,
                    id: None
                })
            })
        );
        assert_eq!(
            verb(&["post", "update", "p1", "--title", "New"], "").unwrap(),
            Verb::PostUpdate(PostUpdateArgs {
                id: "p1".into(),
                title: Some("New".into()),
                parts: None
            })
        );
        assert_eq!(
            verb(&["post", "json", "-"], "{oops").unwrap_err().code,
            ErrorCode::Usage
        );
        assert!(Cli::try_parse_from(["notes", "post", "gif", "x"]).is_err());
        assert!(Cli::try_parse_from(["notes", "comment", "p1", "hi", "--part", "1"]).is_err());
    }

    #[test]
    fn base64_trace_steps_and_languages() {
        let vectors = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (plain, encoded) in vectors {
            assert_eq!(base64(plain.as_bytes()), encoded);
        }
        assert_eq!(base64(&[0xff, 0xfe, 0x00]), "//4A");
        let array = trace_steps(r#"[{"label":"a"},{"label":"b","kind":"tool"}]"#).unwrap();
        let jsonl =
            trace_steps("{\"label\":\"a\"}\n\n{\"label\":\"b\",\"kind\":\"tool\"}\n").unwrap();
        assert_eq!(array, jsonl);
        assert_eq!(array[1].kind.as_deref(), Some("tool"));
        assert!(trace_steps("{\"label\":\"a\"}\nnope\n")
            .unwrap_err()
            .message
            .starts_with("trace line 2:"));
        assert_eq!(language_for("src/Main.RS").as_deref(), Some("rust"));
        assert_eq!(language_for("-"), None);
        assert_eq!(content_type_for("a.PNG"), "image/png");
    }

    #[test]
    fn utc_formats_unix_seconds() {
        assert_eq!(utc(0), "1970-01-01 00:00Z");
        assert_eq!(utc(1_700_000_000), "2023-11-14 22:13Z");
        assert_eq!(utc(951_782_400), "2000-02-29 00:00Z");
    }
}
