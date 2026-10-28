//! `StageClient`: the notesd verbs stage may call (reads, `comment-new`, `session-rename`
//! and the session and post deletes), and nothing else.
//! Requests go over one connection; `watch` holds its own.

use std::{
    io::{self, BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
};

use notes_proto::{
    posts::{
        AssetId, Comment, CommentListArgs, CommentNewArgs, IdArgs, Post, PostListArgs, PostRender, PostShowArgs,
        PostSummary, Session, SessionHit, SessionId, SessionSearchArgs,
    },
    to_line, ErrorBody, ErrorCode, Event, Request, Response, Verb, WatchArgs, PROTOCOL_VERSION,
};
use serde::de::DeserializeOwned;

use crate::app::{Reply, StageRequest};

pub struct StageClient {
    sock: PathBuf,
    conn: Option<(UnixStream, BufReader<UnixStream>)>,
    next: u64,
}

fn unreachable(e: impl std::fmt::Display) -> ErrorBody {
    ErrorBody { code: ErrorCode::DaemonUnreachable, message: format!("notesd: {e}"), hint: None, details: None }
}

fn connect(sock: &Path) -> io::Result<(UnixStream, BufReader<UnixStream>)> {
    let stream = UnixStream::connect(sock)?;
    let reader = BufReader::new(stream.try_clone()?);
    Ok((stream, reader))
}

impl StageClient {
    pub fn new(sock: PathBuf) -> StageClient {
        StageClient { sock, conn: None, next: 1 }
    }

    fn roundtrip(&mut self, verb: &Verb) -> io::Result<Response> {
        if self.conn.is_none() {
            self.conn = Some(connect(&self.sock)?);
        }
        let (stream, reader) = self.conn.as_mut().unwrap();
        let id = self.next;
        self.next += 1;
        let req = Request { v: PROTOCOL_VERSION, id, verb: verb.clone(), cwd: None, agent: None };
        stream.write_all(to_line(&req).as_bytes())?;
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "notesd closed the connection"));
        }
        serde_json::from_str(&line).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// One request; reconnects once if notesd restarted under us.
    fn call<T: DeserializeOwned>(&mut self, verb: Verb) -> Result<T, ErrorBody> {
        let response = match self.roundtrip(&verb) {
            Ok(r) => r,
            Err(_) => {
                self.conn = None;
                self.roundtrip(&verb).map_err(|e| {
                    self.conn = None;
                    unreachable(e)
                })?
            }
        };
        match response {
            Response::Ok { result, .. } => serde_json::from_value(result).map_err(|e| ErrorBody {
                code: ErrorCode::Other,
                message: format!("unexpected reply: {e}"),
                hint: None,
                details: None,
            }),
            Response::Err { error, .. } => Err(error),
            Response::Event { .. } => Err(unreachable("event on the request connection")),
        }
    }

    /// The latest committed event seq (from `doctor`), where the watch starts.
    pub fn latest_seq(&mut self) -> Result<u64, ErrorBody> {
        let doctor: notes_proto::DoctorResult = self.call(Verb::Doctor)?;
        Ok(doctor.seq.unwrap_or(0))
    }

    pub fn session_list(&mut self) -> Result<Vec<Session>, ErrorBody> {
        self.call(Verb::SessionList)
    }

    pub fn session_search(&mut self, args: SessionSearchArgs) -> Result<Vec<SessionHit>, ErrorBody> {
        self.call(Verb::SessionSearch(args))
    }

    pub fn post_list(&mut self, session: SessionId) -> Result<Vec<PostSummary>, ErrorBody> {
        self.call(Verb::PostList(PostListArgs { session: Some(session), limit: None }))
    }

    pub fn post_show(&mut self, args: PostShowArgs) -> Result<Post, ErrorBody> {
        self.call(Verb::PostShow(args))
    }

    pub fn post_render(&mut self, args: PostShowArgs) -> Result<PostRender, ErrorBody> {
        self.call(Verb::PostRender(args))
    }

    pub fn comment_new(&mut self, args: CommentNewArgs) -> Result<Comment, ErrorBody> {
        self.call(Verb::CommentNew(args))
    }

/// An asset's bytes (images in image parts). Read-only.
    pub fn asset_get(&mut self, id: AssetId) -> Result<Vec<u8>, ErrorBody> {
        use base64::{engine::general_purpose::STANDARD, Engine};
        let data: notes_proto::posts::AssetData = self.call(Verb::AssetGet(notes_proto::posts::IdArgs { id }))?;
        STANDARD.decode(data.data_base64.trim()).map_err(|e| unreachable(format!("asset data: {e}")))
    }

    pub fn comment_list(&mut self, session: SessionId) -> Result<Vec<Comment>, ErrorBody> {
        self.call(Verb::CommentList(CommentListArgs { session: Some(session), post: None, after_seq: None }))
    }

    /// Perform one of the app's requests and turn the answer into a reply.
    pub fn perform(&mut self, req: &StageRequest) -> Reply {
        let failed = |e| Reply::Failed(req.clone(), e);
        match req.clone() {
            StageRequest::SessionList => self.session_list().map_or_else(failed, Reply::Sessions),
            StageRequest::SessionSearch(a) => {
                let q = a.query.clone();
                self.session_search(a).map_or_else(failed, |h| Reply::Hits(q, h))
            }
            StageRequest::PostList(s) => self.post_list(s.clone()).map_or_else(failed, |p| Reply::Posts(s, p)),
            StageRequest::PostShow(a) => self.post_show(a).map_or_else(failed, Reply::Post),
            StageRequest::PostRender(a) => self.post_render(a).map_or_else(failed, Reply::Render),
            StageRequest::CommentNew(a) => self.comment_new(a).map_or_else(failed, Reply::Commented),
            StageRequest::CommentList(s) => self.comment_list(s.clone()).map_or_else(failed, |c| Reply::Comments(s, c)),
            StageRequest::SessionRename(a) => self.saved(Verb::SessionRename(a), req),
            StageRequest::SessionDelete(id) => self.saved(Verb::SessionDelete(IdArgs { id }), req),
            StageRequest::PostDelete(id) => self.saved(Verb::PostDelete(IdArgs { id }), req),
            StageRequest::CardCols(n) => {
                let set = notes_proto::posts::SettingSetArgs { key: notes_proto::posts::STAGE_CARD_COLS.into(), value: n.to_string() };
                self.saved(Verb::SettingSet(set), req)
            }
        }
    }

    /// A write whose answer stage ignores; the watch brings the change back.
    fn saved(&mut self, verb: Verb, req: &StageRequest) -> Reply {
        self.call::<serde::de::IgnoredAny>(verb).map_or_else(|e| Reply::Failed(req.clone(), e), |_| Reply::Saved)
    }
}

/// The watch connection: events for posts, comments and sessions.
pub struct Watch {
    reader: BufReader<UnixStream>,
    _stream: UnixStream,
}

impl Watch {
    pub fn open(sock: &Path, since: Option<u64>) -> io::Result<Watch> {
        let (mut stream, reader) = connect(sock)?;
        let verb = Verb::Watch(WatchArgs {
            since,
            kinds: vec!["post".into(), "comment".into(), "session".into()],
        });
        let req = Request { v: PROTOCOL_VERSION, id: 0, verb, cwd: None, agent: None };
        stream.write_all(to_line(&req).as_bytes())?;
        Ok(Watch { reader, _stream: stream })
    }

    /// Block for the next event; `None` when notesd hangs up.
    pub fn next_event(&mut self) -> Option<Event> {
        let mut line = String::new();
        loop {
            line.clear();
            if self.reader.read_line(&mut line).ok()? == 0 {
                return None;
            }
            match serde_json::from_str(&line) {
                Ok(Response::Event { event, .. }) => return Some(event),
                Ok(Response::Err { .. }) => return None,
                _ => continue,
            }
        }
    }
}
