//! `notes`: thin client for `notesd`. Every call is one request line over the
//! unix socket and one response line back (or a stream of event lines for
//! `watch`). It never touches the vault or SQLite. See docs/cli-spec.md.

// Errors are `ErrorBody` straight off the wire and happen at most once per process.
#![allow(clippy::result_large_err)]

use std::{
    env,
    fmt::Write as _,
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::{Parser, Subcommand};
use notes_proto::{
    default_socket_path, to_line, AppendArgs, BatchArgs, DeleteArgs, DiffArgs, DiffResult,
    DoctorResult, EditArgs, ErrorBody, ErrorCode, FindArgs, FindHit, FolderArgs, HistoryArgs,
    HistoryEntry, LineRange, ListArgs, ListEntry, ListSort, MoveArgs, NewArgs, Note, NoteArgs,
    PrependArgs, ReadArgs, Replacement, Request, Response, RevertArgs, SearchArgs, SearchHit,
    Verb, WatchArgs, WriteArgs, WriteResult, PROTOCOL_VERSION,
};
use serde_json::Value;

#[derive(Debug, Parser)]
#[command(name = "notes", version, about = "Plain markdown notes, served by notesd")]
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

#[derive(Debug, Subcommand)]
enum Cmd {
    /// List notes and subfolders in a folder (vault root by default)
    List {
        folder: Option<String>,
        #[arg(short, long)]
        recursive: bool,
        /// updated, created or title
        #[arg(long, default_value = "updated", value_parser = parse_sort)]
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
    /// Every verb, its args, output schema and exit codes, as JSON
    Manifest,
    /// Socket and database paths, daemon reachable, versions
    Doctor,
}

fn parse_sort(s: &str) -> Result<ListSort, String> {
    serde_json::from_value(Value::from(s)).map_err(|_| "expected updated, created or title".into())
}

fn parse_lines(s: &str) -> Result<LineRange, String> {
    let (start, end) = s.split_once(':').ok_or("expected A:B")?;
    let num = |n: &str| n.parse::<u32>().map_err(|e| format!("{n:?}: {e}"));
    Ok(LineRange { start: num(start)?, end: num(end)? })
}

fn fail(code: ErrorCode, message: impl Into<String>, hint: &str) -> ErrorBody {
    ErrorBody { code, message: message.into(), hint: Some(hint.into()), details: None }
}

fn read_all(source: &str, stdin: &mut dyn Read) -> Result<String, ErrorBody> {
    let text = if source == "-" {
        let mut text = String::new();
        stdin.read_to_string(&mut text).map(|_| text)
    } else {
        fs::read_to_string(source)
    };
    text.map_err(|e| fail(ErrorCode::Other, format!("can't read {source}: {e}"), "check the path, or pass - and pipe the text in"))
}

/// `TEXT|-` arguments: literal text, or stdin when it's `-`.
fn text_arg(arg: String, stdin: &mut dyn Read) -> Result<String, ErrorBody> {
    if arg == "-" { read_all("-", stdin) } else { Ok(arg) }
}

fn to_verb(cmd: Cmd, if_version: Option<u64>, stdin: &mut dyn Read) -> Result<Verb, ErrorBody> {
    if if_version.is_some() && !matches!(cmd, Cmd::Edit { .. } | Cmd::Append { .. } | Cmd::Prepend { .. } | Cmd::Write { .. }) {
        return Err(fail(ErrorCode::Usage, "--if-version only applies to edit, append, prepend and write", "drop --if-version"));
    }
    Ok(match cmd {
        Cmd::List { folder, recursive, sort, limit } => Verb::List(ListArgs { folder, recursive, sort, limit }),
        Cmd::Find { query, limit } => Verb::Find(FindArgs { query, limit }),
        Cmd::Search { query, folder, context, case, limit } => {
            Verb::Search(SearchArgs { query, folder, context, case_sensitive: case, fuzzy: false, limit })
        }
        Cmd::Read { note, lines, .. } => Verb::Read(ReadArgs { note, lines }),
        Cmd::New { path, text } => {
            let body = text.map(|t| text_arg(t, stdin)).transpose()?.unwrap_or_default();
            Verb::New(NewArgs { path, body })
        }
        Cmd::Edit { note, old, new, all, from } => {
            let edits = match (old, new, from) {
                (Some(old), Some(new), None) => vec![Replacement { old, new, all }],
                (None, None, Some(source)) => serde_json::from_str(&read_all(&source, stdin)?).map_err(|e| {
                    fail(ErrorCode::Usage, format!("bad edits JSON: {e}"), r#"pass a JSON array like [{"old":"foo","new":"bar"}]"#)
                })?,
                _ => return Err(fail(ErrorCode::Usage, "edit needs --old and --new, or --from", "run `notes help edit`")),
            };
            Verb::Edit(EditArgs { note, edits, if_version })
        }
        Cmd::Append { note, text, under } => Verb::Append(AppendArgs { note, text: text_arg(text, stdin)?, under, if_version }),
        Cmd::Prepend { note, text } => Verb::Prepend(PrependArgs { note, text: text_arg(text, stdin)?, if_version }),
        Cmd::Write { note, text, force } => {
            if if_version.is_none() && !force {
                return Err(fail(ErrorCode::Usage, "write replaces the whole note", "pass --if-version N (from `notes read --json`) or --force"));
            }
            Verb::Write(WriteArgs { note, body: text_arg(text, stdin)?, if_version, force })
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
                        fail(ErrorCode::Usage, format!("batch line {}: {e}", i + 1), r#"each stdin line is one {"verb":…,"args":…} op"#)
                    })
                })
                .collect::<Result<_, _>>()?;
            Verb::Batch(BatchArgs { ops, atomic })
        }
        Cmd::Manifest => Verb::Manifest,
        Cmd::Doctor => Verb::Doctor,
    })
}

/// Send one request and print what comes back; events stream until the daemon closes.
fn call(sock: &Path, request: &Request, json: bool, quiet: bool, number: bool) -> Result<(), ErrorBody> {
    let unreachable = |e: io::Error| {
        fail(ErrorCode::DaemonUnreachable, format!("notesd unreachable at {}: {e}", sock.display()), "start notesd, or point --sock / NOTES_SOCK at its socket")
    };
    let mut stream = UnixStream::connect(sock).map_err(unreachable)?;
    stream.write_all(to_line(request).as_bytes()).map_err(unreachable)?;
    let mut reader = BufReader::new(stream);
    let mut out = io::stdout().lock();
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).map_err(unreachable)? == 0 {
            if matches!(request.verb, Verb::Watch(_)) {
                return Ok(());
            }
            return Err(fail(ErrorCode::Other, "notesd closed the connection without a response", "run `notes doctor`"));
        }
        let response = serde_json::from_str(&line).map_err(|e| {
            fail(ErrorCode::Other, format!("bad response from notesd: {e}"), "the CLI and notesd may speak different protocol versions; run `notes doctor`")
        })?;
        match response {
            Response::Event { event, .. } => {
                // A closed stdout (e.g. `notes watch | head`) ends the stream quietly.
                if !quiet && writeln!(out, "{}", serde_json::to_string(&event).expect("event serializes")).is_err() {
                    return Ok(());
                }
            }
            Response::Ok { result, .. } => {
                if !quiet {
                    let _ = out.write_all(render(&request.verb, result, json, number).as_bytes());
                }
                return Ok(());
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
        Verb::New(_) => _ = writeln!(out, "{}", serde_json::from_value::<WriteResult>(result.clone()).ok()?.path),
        Verb::History(_) => {
            let entries: Vec<HistoryEntry> = serde_json::from_value(result.clone()).ok()?;
            for entry in &entries {
                _ = writeln!(out, "v{}\t{}\t{}", entry.version, utc(entry.at), entry.author);
            }
        }
        Verb::Diff(_) => out = serde_json::from_value::<DiffResult>(result.clone()).ok()?.unified,
        Verb::Doctor => {
            let d: DoctorResult = serde_json::from_value(result.clone()).ok()?;
            _ = writeln!(out, "cli       {}", env!("CARGO_PKG_VERSION"));
            _ = writeln!(out, "notesd    {} (protocol {})", d.daemon_version, d.protocol);
            _ = writeln!(out, "socket    {}", d.socket.display());
            _ = writeln!(out, "vault     {}", d.vault.display());
            _ = writeln!(out, "database  {}", d.database.display());
            _ = writeln!(out, "notes     {}", d.notes);
        }
        Verb::Manifest => out = serde_json::to_string_pretty(result).ok()? + "\n",
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
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}Z", rem / 3600, rem % 3600 / 60)
}

fn report(error: &ErrorBody, json: bool) -> u8 {
    if json {
        let _ = writeln!(io::stdout(), "{}", serde_json::to_string(error).expect("error serializes"));
    }
    let retry = error.current_version().map(|n| format!("current version is {n}; re-read the note, or retry with --if-version {n}"));
    let hint = retry.as_deref().or(error.hint.as_deref()).unwrap_or(match error.code {
        ErrorCode::Usage => "run `notes help`",
        ErrorCode::NotFound => "run `notes find <query>` to look up the path",
        ErrorCode::Ambiguous => "use the full vault path, or add surrounding text to --old",
        ErrorCode::VersionConflict => "run `notes read --json <ref>` for the current version, then retry",
        ErrorCode::DaemonUnreachable => "start notesd, or point --sock / NOTES_SOCK at its socket",
        ErrorCode::Other => "run `notes doctor`",
    });
    eprintln!("notes: {}\nhint: {hint}", error.message);
    error.code.exit_code()
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let number = matches!(cli.cmd, Cmd::Read { number: true, .. });
    let sock = cli.sock.unwrap_or_else(default_socket_path);
    let outcome = to_verb(cli.cmd, cli.if_version, &mut io::stdin()).and_then(|verb| {
        let request = Request {
            v: PROTOCOL_VERSION,
            id: 1,
            verb,
            cwd: env::current_dir().ok(),
            agent: env::var("NOTES_AGENT").ok().filter(|a| !a.is_empty()),
        };
        call(&sock, &request, cli.json, cli.quiet, number)
    });
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => ExitCode::from(report(&error, cli.json)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verb(args: &[&str], stdin: &str) -> Result<Verb, ErrorBody> {
        let cli = Cli::try_parse_from(std::iter::once("notes").chain(args.iter().copied())).unwrap();
        to_verb(cli.cmd, cli.if_version, &mut stdin.as_bytes())
    }

    fn usage_code(args: &[&str]) -> ErrorCode {
        verb(args, "").unwrap_err().code
    }

    #[test]
    fn read_and_list_args() {
        assert_eq!(
            verb(&["list", "work", "-r", "--sort", "title", "--limit", "5"], "").unwrap(),
            Verb::List(ListArgs { folder: Some("work".into()), recursive: true, sort: ListSort::Title, limit: Some(5) })
        );
        assert_eq!(verb(&["list"], "").unwrap(), Verb::List(ListArgs::default()));
        assert_eq!(
            verb(&["show", "plan", "--lines", "2:5", "-n"], "").unwrap(),
            Verb::Read(ReadArgs { note: "plan".into(), lines: Some(LineRange { start: 2, end: 5 }) })
        );
        assert_eq!(
            verb(&["search", "todo", "work", "-C", "2", "--case"], "").unwrap(),
            Verb::Search(SearchArgs { query: "todo".into(), folder: Some("work".into()), context: 2, case_sensitive: true, fuzzy: false, limit: None })
        );
        assert!(Cli::try_parse_from(["notes", "read", "x", "--lines", "5"]).is_err());
        assert!(Cli::try_parse_from(["notes", "list", "--sort", "size"]).is_err());
    }

    #[test]
    fn edit_args() {
        assert_eq!(
            verb(&["edit", "plan", "--old", "a", "--new", "b", "--all", "--if-version", "3"], "").unwrap(),
            Verb::Edit(EditArgs { note: "plan".into(), edits: vec![Replacement { old: "a".into(), new: "b".into(), all: true }], if_version: Some(3) })
        );
        let from_stdin = verb(&["edit", "plan", "--from", "-"], r#"[{"old":"a","new":"b"},{"old":"c","new":"d","all":true}]"#).unwrap();
        let Verb::Edit(EditArgs { edits, .. }) = from_stdin else { panic!("expected edit") };
        assert_eq!(edits.len(), 2);
        assert!(edits[1].all);
        assert_eq!(verb(&["edit", "plan", "--from", "-"], "{oops").unwrap_err().code, ErrorCode::Usage);
        assert!(Cli::try_parse_from(["notes", "edit", "plan"]).is_err());
        assert!(Cli::try_parse_from(["notes", "edit", "plan", "--old", "a"]).is_err());
        assert!(Cli::try_parse_from(["notes", "edit", "plan", "--old", "a", "--new", "b", "--from", "-"]).is_err());
    }

    #[test]
    fn write_family_args() {
        assert_eq!(
            verb(&["append", "plan", "-", "--under", "Todo"], "- milk\n").unwrap(),
            Verb::Append(AppendArgs { note: "plan".into(), text: "- milk\n".into(), under: Some("Todo".into()), if_version: None })
        );
        assert_eq!(verb(&["new", "work/plan"], "").unwrap(), Verb::New(NewArgs { path: "work/plan".into(), body: String::new() }));
        assert_eq!(usage_code(&["write", "plan", "-"]), ErrorCode::Usage);
        assert_eq!(
            verb(&["write", "plan", "-", "--if-version", "7"], "new body").unwrap(),
            Verb::Write(WriteArgs { note: "plan".into(), body: "new body".into(), if_version: Some(7), force: false })
        );
        assert_eq!(usage_code(&["move", "a", "b", "--if-version", "1"]), ErrorCode::Usage);
        assert_eq!(verb(&["delete", "a", "--purge"], "").unwrap(), Verb::Delete(DeleteArgs { note: "a".into(), purge: true }));
        assert_eq!(verb(&["revert", "a", "--to", "4"], "").unwrap(), Verb::Revert(RevertArgs { note: "a".into(), to: 4 }));
    }

    #[test]
    fn watch_and_batch_args() {
        assert_eq!(
            verb(&["watch", "--since", "12", "--kind", "note,post"], "").unwrap(),
            Verb::Watch(WatchArgs { since: Some(12), kinds: vec!["note".into(), "post".into()] })
        );
        let ops = "{\"verb\":\"read\",\"args\":{\"note\":\"a\"}}\n\n{\"verb\":\"doctor\"}\n";
        assert_eq!(
            verb(&["batch", "--atomic"], ops).unwrap(),
            Verb::Batch(BatchArgs { ops: vec![Verb::Read(ReadArgs { note: "a".into(), lines: None }), Verb::Doctor], atomic: true })
        );
        let err = verb(&["batch"], "{\"verb\":\"doctor\"}\nnope\n").unwrap_err();
        assert!(err.message.starts_with("batch line 2:"), "{}", err.message);
    }

    #[test]
    fn utc_formats_unix_seconds() {
        assert_eq!(utc(0), "1970-01-01 00:00Z");
        assert_eq!(utc(1_700_000_000), "2023-11-14 22:13Z");
        assert_eq!(utc(951_782_400), "2000-02-29 00:00Z");
    }
}
