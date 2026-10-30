//! `notesd`: owns the vault, SQLite and fff search, and serves the line protocol
//! (crates/proto) on a unix socket. One thread per connection, one blocking kqueue
//! watcher thread; nothing wakes up while nobody is talking and the vault is still.

mod ops;
mod search;
mod store;
mod text;
mod watcher;

use std::{
    env,
    ffi::OsStr,
    fs,
    fs::Permissions,
    io::{self, BufRead, BufReader, Read, Write},
    os::{
        fd::{FromRawFd, RawFd},
        raw::{c_char, c_int},
        unix::{
            ffi::OsStrExt,
            fs::{FileTypeExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

use notes_proto::{
    BatchArgs, BatchOutcome, ErrorBody, ErrorCode, False, PROTOCOL_VERSION, Request, Response,
    True, Verb, WatchArgs, to_line,
};
use serde_json::Value;
use store::Daemon;

#[derive(Debug, Clone)]
pub struct Fail(pub ErrorBody);
pub type R<T> = Result<T, Fail>;

pub fn fail(code: ErrorCode, message: impl Into<String>, hint: Option<&str>) -> Fail {
    Fail(text::error(code, message, hint))
}

impl From<ErrorBody> for Fail {
    fn from(e: ErrorBody) -> Self {
        Fail(e)
    }
}

impl From<io::Error> for Fail {
    fn from(e: io::Error) -> Self {
        fail(ErrorCode::Other, e.to_string(), None)
    }
}

impl From<rusqlite::Error> for Fail {
    fn from(e: rusqlite::Error) -> Self {
        fail(
            ErrorCode::Other,
            format!("database: {e}"),
            Some("run `notes doctor`"),
        )
    }
}

fn call(d: &Daemon, verb: &Verb, author: &str, cwd: Option<&str>) -> R<Value> {
    match verb {
        // Blocks this connection's thread on the comment condvar, outside the write lock.
        Verb::Wait(a) => {
            let timeout =
                (a.timeout_secs > 0).then(|| std::time::Duration::from_secs(a.timeout_secs));
            let found = d
                .comments
                .wait_for(timeout, || {
                    d.unit(author, |tx| {
                        // No session: this conversation's; it hasn't posted yet, so nothing to wait on.
                        let session = match (&a.session, &a.owner) {
                            (Some(id), _) => Some(id.clone()),
                            (None, Some(owner)) => match notes_posts::owned_session(tx.store, owner) {
                                Some(id) => Some(id),
                                None => return Ok(None),
                            },
                            (None, None) => None,
                        };
                        notes_posts::next_user_comment(
                            tx.store,
                            session.as_deref(),
                            a.post.as_deref(),
                            None,
                        )
                        .map_err(Fail)
                    })
                    .map_err(|Fail(e)| e)
                })
                .map_err(Fail)?;
            Ok(serde_json::to_value(found).expect("comment serializes"))
        }
        // Lock-free: fff has its own RwLock and these never touch SQLite.
        // (A folder-scoped search goes through the unit below, which validates the folder.)
        Verb::Find(a) => Ok(serde_json::to_value(d.search.find(a)?).expect("hits serialize")),
        Verb::Search(a) if a.folder.as_deref().is_none_or(|f| f.trim().is_empty()) => {
            Ok(serde_json::to_value(d.search.search(a, None)?).expect("hits serialize"))
        }
        Verb::Manifest => Ok(ops::manifest()),
        Verb::Batch(b) => {
            Ok(serde_json::to_value(batch(d, b, author, cwd)).expect("outcomes serialize"))
        }
        _ => d.unit(author, |tx| {
            tx.cwd = cwd.map(str::to_owned);
            tx.op(verb)
        }),
    }
}

/// The read verbs stage calls on every change, answered with JSON text written from their
/// types (outside the lock where it can be) instead of the `Value` tree `call` builds.
/// The response line is the same.
fn call_json(d: &Daemon, verb: &Verb, author: &str) -> Option<R<String>> {
    use notes_posts as p;
    fn text(v: impl serde::Serialize) -> String {
        serde_json::to_string(&v).expect("results serialize")
    }
    Some(match verb {
        Verb::PostList(a) => d.unit(author, |tx| p::post_list(tx.store, a).map_err(Fail)).map(text),
        Verb::PostShow(a) => d.unit(author, |tx| p::post_show(tx.store, a).map_err(Fail)).map(text),
        Verb::PostRender(a) => {
            d.unit(author, |tx| p::post_render_json(tx.store, &d.render, a).map_err(Fail))
        }
        _ => return None,
    })
}

fn batch(d: &Daemon, b: &BatchArgs, author: &str, cwd: Option<&str>) -> Vec<BatchOutcome> {
    let ok = |result| BatchOutcome::Ok { ok: True, result };
    let err = |Fail(error)| BatchOutcome::Err { ok: False, error };
    if !b.atomic {
        return b
            .ops
            .iter()
            .map(|op| match op {
                Verb::Batch(_) => err(fail(ErrorCode::Usage, "batches can't nest", None)),
                op => call(d, op, author, cwd).map_or_else(err, ok),
            })
            .collect();
    }
    // One SQLite transaction and one file journal for the whole batch.
    let mut outcomes = Vec::new();
    let result = d.unit(author, |tx| {
        tx.cwd = cwd.map(str::to_owned);
        for op in &b.ops {
            match tx.op(op) {
                Ok(v) => outcomes.push(ok(v)),
                Err(e) => {
                    outcomes.push(err(e.clone()));
                    return Err(e);
                }
            }
        }
        Ok(())
    });
    if let Err(e) = result
        && matches!(outcomes.last(), None | Some(BatchOutcome::Ok { .. }))
    {
        outcomes.push(err(e)); // the commit itself failed
    }
    outcomes
}

fn wanted(kinds: &[String], kind: &str) -> bool {
    kinds.is_empty()
        || kinds.iter().any(|k| {
            kind == k
                || kind
                    .strip_prefix(k.as_str())
                    .is_some_and(|rest| rest.starts_with('-'))
        })
}

/// Stream events after `since` until the client hangs up. A reader thread blocks on the
/// socket and wakes us on EOF, so a quiet watch costs no wakeups.
fn watch(
    d: &Arc<Daemon>,
    id: u64,
    args: &WatchArgs,
    mut out: &UnixStream,
    mut reader: BufReader<UnixStream>,
) {
    let closed = Arc::new(AtomicBool::new(false));
    let (daemon, flag) = (d.clone(), closed.clone());
    let spawned = thread::Builder::new()
        .name("notesd-watch-eof".into())
        .spawn(move || {
            let mut sink = [0u8; 512];
            while matches!(reader.read(&mut sink), Ok(n) if n > 0) {}
            flag.store(true, Ordering::SeqCst);
            daemon.wake_waiters();
        });
    if spawned.is_err() {
        return;
    }
    const PAGE: u32 = 1000;
    let mut last = args.since.unwrap_or_else(|| d.latest_seq());
    loop {
        let Ok(events) = d.events_after(last, PAGE) else {
            return;
        };
        for event in &events {
            last = event.seq;
            if wanted(&args.kinds, &event.kind) {
                let line = to_line(&Response::Event {
                    id,
                    event: event.clone(),
                });
                if out.write_all(line.as_bytes()).is_err() {
                    return;
                }
            }
        }
        if events.len() < PAGE as usize {
            d.wait_for_event(last, || closed.load(Ordering::SeqCst));
        }
        if closed.load(Ordering::SeqCst) {
            return;
        }
    }
}

/// Envelope keys a request may carry. `Request` flattens `verb`, which serde can't
/// combine with `deny_unknown_fields`, so the envelope is checked by hand; the
/// verb's `args` structs reject unknown fields themselves.
const ENVELOPE_KEYS: [&str; 6] = ["v", "id", "verb", "args", "cwd", "agent"];

fn parse_request(line: &str) -> Result<Request, String> {
    let value: Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
    if let Some(key) = value
        .as_object()
        .and_then(|o| o.keys().find(|k| !ENVELOPE_KEYS.contains(&k.as_str())))
    {
        return Err(format!("unknown field `{key}` in the request envelope"));
    }
    serde_json::from_value(value).map_err(|e| e.to_string())
}

fn serve(d: Arc<Daemon>, stream: UnixStream) {
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_half);
    let mut out = &stream;
    let mut line = String::new();
    loop {
        line.clear();
        if !matches!(reader.read_line(&mut line), Ok(n) if n > 0) {
            return;
        }
        if line.trim().is_empty() {
            continue;
        }
        let response = match parse_request(&line) {
            Err(e) => {
                let id = serde_json::from_str::<Value>(&line)
                    .ok()
                    .and_then(|v| v["id"].as_u64())
                    .unwrap_or(0);
                Response::err(
                    id,
                    text::error(
                        ErrorCode::Usage,
                        format!("bad request: {e}"),
                        Some("see `notes manifest`"),
                    ),
                )
            }
            Ok(req) if req.v != PROTOCOL_VERSION => Response::err(
                req.id,
                text::error(
                    ErrorCode::Usage,
                    format!(
                        "protocol v{} isn't supported; notesd speaks v{PROTOCOL_VERSION}",
                        req.v
                    ),
                    None,
                ),
            ),
            Ok(Request {
                id,
                verb: Verb::Watch(args),
                ..
            }) => return watch(&d, id, &args, &stream, reader),
            Ok(req) => {
                let author = req
                    .agent
                    .as_deref()
                    .filter(|a| !a.trim().is_empty())
                    .unwrap_or("human");
                let cwd = req.cwd.as_deref().and_then(|c| c.to_str());
                match call_json(&d, &req.verb, author) {
                    // What `to_line` writes for `Response::Ok`, with the result spliced in.
                    Some(Ok(json)) => {
                        let reply = format!("{{\"id\":{},\"ok\":true,\"result\":{json}}}\n", req.id);
                        if out.write_all(reply.as_bytes()).is_err() {
                            return;
                        }
                        continue;
                    }
                    Some(Err(Fail(e))) => Response::err(req.id, e),
                    None => match call(&d, &req.verb, author, cwd) {
                        Ok(result) => Response::Ok {
                            id: req.id,
                            ok: True,
                            result,
                        },
                        Err(Fail(e)) => Response::err(req.id, e),
                    },
                }
            }
        };
        if out.write_all(to_line(&response).as_bytes()).is_err() {
            return;
        }
    }
}

unsafe extern "C" {
    /// liblaunch (libSystem): the sockets launchd opened for this job's `Sockets` key.
    fn launch_activate_socket(name: *const c_char, fds: *mut *mut c_int, cnt: *mut usize) -> c_int;
}

/// The `Listeners` socket when launchd started us; `None` otherwise (ESRCH outside launchd).
fn launchd_socket() -> Option<RawFd> {
    let (mut fds, mut count): (*mut c_int, usize) = (std::ptr::null_mut(), 0);
    if unsafe { launch_activate_socket(c"Listeners".as_ptr(), &mut fds, &mut count) } != 0
        || fds.is_null()
    {
        return None;
    }
    let all = unsafe { std::slice::from_raw_parts(fds, count) }.to_vec();
    unsafe { libc::free(fds.cast()) };
    for &extra in all.iter().skip(1) {
        unsafe { libc::close(extra) };
    }
    all.first().copied()
}

fn listen(path: &Path) -> io::Result<UnixListener> {
    if let Some(fd) = launchd_socket() {
        return Ok(unsafe { UnixListener::from_raw_fd(fd) });
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    if let Ok(meta) = fs::symlink_metadata(path) {
        if !meta.file_type().is_socket() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "path exists and isn't a socket",
            ));
        }
        if UnixStream::connect(path).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "another notesd is already serving it",
            ));
        }
        fs::remove_file(path)?; // stale socket from a crashed daemon
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, Permissions::from_mode(0o600))?;
    Ok(listener)
}

fn run() -> Result<(), String> {
    let socket = notes_proto::default_socket_path();
    // Bind first: clients connecting during startup queue in the backlog instead of failing.
    let listener =
        listen(&socket).map_err(|e| format!("can't listen on {}: {e}", socket.display()))?;
    // Under launchd the plist names the socket; report the one we actually serve.
    // (launchd's fd reports the whole sun_path, NUL padding included.)
    let socket = listener
        .local_addr()
        .ok()
        .and_then(|a| {
            let bytes = a.as_pathname()?.as_os_str().as_bytes();
            let path = bytes.split(|&b| b == 0).next().unwrap_or_default();
            Some(PathBuf::from(OsStr::from_bytes(path)))
        })
        .unwrap_or(socket);
    let vault = notes_proto::default_vault_path();
    let vault = fs::create_dir_all(&vault)
        .and_then(|()| vault.canonicalize())
        .map_err(|e| format!("vault {}: {e}", vault.display()))?;
    let db_path = env::var_os("NOTES_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| notes_proto::app_support_dir().join("notes.db"));
    let stage = notes_proto::default_stage_path();
    let daemon =
        Arc::new(Daemon::open(vault, db_path, socket, stage).map_err(|Fail(e)| e.message)?);
    // Watch before reconciling, so nothing changed in between goes unseen.
    watcher::start(daemon.clone()).map_err(|e| format!("vault watcher: {e}"))?;
    if let Err(e) = watcher::start_stage(daemon.clone()) {
        eprintln!("notesd: session folder watcher: {e}");
    }
    daemon.reconcile().map_err(|Fail(e)| e.message)?;
    eprintln!(
        "notesd: serving {} on {}",
        daemon.vault.display(),
        daemon.socket.display()
    );
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let d = daemon.clone();
                if let Err(e) = thread::Builder::new()
                    .name("notesd-conn".into())
                    .spawn(move || serve(d, stream))
                {
                    eprintln!("notesd: can't spawn connection thread: {e}");
                }
            }
            Err(e) => eprintln!("notesd: accept: {e}"),
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("notesd: {e}");
            ExitCode::FAILURE
        }
    }
}
