//! Every sideshow verb over the session folder (json_store.rs). Mirrors
//! docs/ref/sideshow/server/sqlStore.ts and the flow functions in app.ts.
//! Settings stay in SQLite.

use std::{
    cmp::Reverse,
    fs,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{engine::general_purpose::STANDARD, Engine};
use notes_proto::{posts::*, Event};
use rusqlite::{params, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::{
    json_store::{AssetMeta, PostRec, SessionFile, Store, VersionRec, SCHEMA},
    not_found, usage,
    validate::{
        clip, part_id, part_id_mut, part_kind, validate_parts, MAX_ASSET_BYTES, MAX_COMMENT_TEXT,
        MAX_STEP_DETAIL, MAX_STEP_KIND, MAX_STEP_LABEL, MAX_TITLE, MAX_TRACE_STEPS,
    },
    Db, Result, Written,
};

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// 8 random bytes as 11 url-safe base64 chars, never starting with `-`/`_`
/// (they'd read as CLI flags). Sideshow's newId.
fn new_id(s: &Store) -> Result<String> {
    let bytes: Vec<u8> = s
        .db
        .prepare_cached("SELECT randomblob(8)")
        .and_then(|mut st| st.query_row([], |r| r.get(0)))
        .db()?;
    let mut id = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    if id.starts_with(['-', '_']) {
        id.replace_range(..1, "0");
    }
    Ok(id)
}

pub(crate) fn event(kind: &str, id: &str, session: Option<&str>, version: Option<u64>) -> Event {
    Event {
        seq: 0,
        kind: kind.into(),
        path: None,
        version,
        id: Some(id.into()),
        session: session.map(Into::into),
    }
}

fn trimmed(s: Option<&str>, max: usize) -> Option<String> {
    s.map(|s| clip(s.trim(), max))
        .filter(|s| !s.is_empty())
        .map(Into::into)
}

/// "user" is the trust label for the human, so no session agent may claim it.
fn reserved_agent(agent: Option<&str>) -> String {
    match trimmed(agent, MAX_TITLE).as_deref() {
        None | Some("user") => "agent".into(),
        Some(a) => a.into(),
    }
}

fn session_missing(id: &str) -> crate::ErrorBody {
    not_found(format!("session \"{id}\" not found"))
}

fn post_missing(id: &str) -> crate::ErrorBody {
    not_found(format!("post \"{id}\" not found"))
}

/// A copy of a session's file contents, to change and [`Store::save`].
fn load(s: &Store, id: &str) -> Result<SessionFile> {
    s.stage()
        .sessions
        .get(id)
        .map(|e| e.data.clone())
        .ok_or_else(|| session_missing(id))
}

/// `f` on a session in place, for verbs that only read it.
fn read<T>(s: &Store, id: &str, f: impl FnOnce(&SessionFile) -> T) -> Result<T> {
    let stage = s.stage();
    let entry = stage.sessions.get(id).ok_or_else(|| session_missing(id))?;
    Ok(f(&entry.data))
}

/// `f` on the session holding `post` and the post's index in it, in place.
fn read_post<T>(s: &Store, post: &str, f: impl FnOnce(&SessionFile, usize) -> T) -> Result<T> {
    let stage = s.stage();
    let file = stage
        .index
        .post_session(post)
        .and_then(|id| stage.sessions.get(id))
        .map(|e| &e.data)
        .ok_or_else(|| post_missing(post))?;
    let at = file
        .posts
        .iter()
        .position(|p| p.id == post)
        .ok_or_else(|| post_missing(post))?;
    Ok(f(file, at))
}

/// The session holding `post`, and the post's index in it.
fn load_post(s: &Store, post: &str) -> Result<(SessionFile, usize)> {
    read_post(s, post, |file, at| (file.clone(), at))
}

// ---- Sessions ------------------------------------------------------------------

fn session_of(f: &SessionFile) -> Session {
    Session {
        id: f.id.clone(),
        agent: f.agent.clone(),
        title: f.title.clone(),
        cwd: f.cwd.clone(),
        created_at: f.created_at,
        last_active_at: f.last_active_at,
        post_count: f.posts.len() as u32,
    }
}

pub fn session_get(s: &Store, id: &str) -> Result<Session> {
    read(s, id, session_of)
}

fn create_session(
    s: &Store,
    agent: Option<&str>,
    title: Option<&str>,
    cwd: Option<&str>,
    owner: Option<&str>,
) -> Result<SessionFile> {
    let at = now();
    Ok(SessionFile {
        schema: SCHEMA,
        id: new_id(s)?.into(),
        agent: reserved_agent(agent),
        title: trimmed(title, MAX_TITLE),
        cwd: cwd.map(Into::into),
        owner: owner.map(Into::into),
        created_at: at,
        last_active_at: at,
        agent_seq: 0,
        posts: Vec::new(),
        comments: Vec::new(),
        trace: Vec::new(),
    })
}

pub fn session_new(s: &Store, args: &SessionNewArgs) -> Result<Written<Session>> {
    let file = create_session(s, Some(&args.agent), args.title.as_deref(), args.cwd.as_deref(), None)?;
    let session = session_of(&file);
    s.save(file)?;
    let ev = event("session-created", &session.id, Some(&session.id), None);
    Ok((session, vec![ev]))
}

/// Most recently active first.
pub fn session_list(s: &Store) -> Result<Vec<Session>> {
    let stage = s.stage();
    let mut entries: Vec<_> = stage.sessions.values().collect();
    entries.sort_by_key(|e| Reverse((e.data.last_active_at, e.ord)));
    Ok(entries.into_iter().map(|e| session_of(&e.data)).collect())
}

/// An empty title clears it. The file name keeps the title it was created with.
pub fn session_rename(s: &Store, args: &SessionRenameArgs) -> Result<Written<Session>> {
    let mut file = load(s, &args.id)?;
    file.title = trimmed(Some(&args.title), MAX_TITLE);
    let session = session_of(&file);
    s.save(file)?;
    Ok((
        session,
        vec![event("session-updated", &args.id, Some(&args.id), None)],
    ))
}

const TRASH_SECS: i64 = 30 * 86_400;

/// Moves the session to the trash (assets stay) so `session_restore` can bring it back;
/// `purge` deletes it for good instead, from the trash too. Deleting what is already in
/// the trash changes nothing (`already`); purging an id that is neither live nor
/// trashed is `not_found`, which is also what a second purge gets.
pub fn session_delete(s: &Store, args: &SessionDeleteArgs) -> Result<Written<SessionDeleteResult>> {
    let id = args.id.as_str();
    let trashed = s.trashed().into_iter().find(|t| t.2.id == id);
    let result = |trashed, purged, already| SessionDeleteResult { id: args.id.clone(), trashed, purged, already };
    if !args.purge {
        return if s.stage().sessions.contains_key(id) {
            s.trash_session(&args.id, now())?;
            Ok((result(true, false, false), vec![event("session-deleted", id, Some(id), None)]))
        } else if trashed.is_some() {
            Ok((result(true, false, true), Vec::new()))
        } else {
            Err(session_missing(id))
        };
    }
    let live = s.stage().sessions.contains_key(id);
    match (live, trashed) {
        (true, _) => s.remove_session(&args.id)?,
        (false, Some((path, ..))) => s.delete_trashed(&path)?,
        (false, None) => return Err(session_missing(id)),
    }
    delete_unreferenced_assets(s, id)?;
    let events = if live { vec![event("session-deleted", id, Some(id), None)] } else { Vec::new() };
    Ok((result(false, true, false), events))
}

/// Deletes the session's assets unless a live or trashed post references them
/// (assets are shared by content hash).
fn delete_unreferenced_assets(s: &Store, id: &str) -> Result<()> {
    let referenced = referenced_assets(s);
    for meta in asset_metas(s) {
        if meta.session.as_deref() == Some(id) && !referenced.contains(meta.id.as_str()) {
            s.delete_asset_file(&s.asset_path(&meta.id))?;
            s.delete_asset_file(&s.asset_path(&format!("{}.json", meta.id)))?;
        }
    }
    Ok(())
}

/// Brings a trashed session back with its posts, comments and assets. A live session
/// with that id is the `already` case; an id that is neither live nor trashed is `not_found`.
pub fn session_restore(s: &Store, id: &str) -> Result<Written<SessionRestoreResult>> {
    let result = |already| SessionRestoreResult { id: id.into(), restored: true, already };
    if s.stage().sessions.contains_key(id) {
        return Ok((result(true), Vec::new()));
    }
    let (path, _, data) = s
        .trashed()
        .into_iter()
        .find(|t| t.2.id == id)
        .ok_or_else(|| session_missing(id))?;
    s.untrash_session(&path, data)?;
    Ok((result(false), vec![event("session-restored", id, Some(id), None)]))
}

/// Purges sessions trashed more than 30 days before `at`, with their unreferenced assets.
pub fn trash_purge(s: &Store, at: i64) -> Result<()> {
    for (path, deleted_at, data) in s.trashed() {
        if deleted_at < at - TRASH_SECS {
            s.delete_trashed(&path)?;
            delete_unreferenced_assets(s, &data.id)?;
        }
    }
    Ok(())
}

/// Sessions matching `query` by title, agent or cwd (best), or by post titles,
/// part text and comments; most recently active first within a rank.
pub fn session_search(s: &Store, args: &SessionSearchArgs) -> Result<Vec<SessionHit>> {
    let query = args.query.trim().to_lowercase();
    if query.is_empty() {
        return Err(usage("session-search needs a query"));
    }
    let limit = args.limit.unwrap_or(50) as usize;
    let stage = s.stage();
    let mut hits: Vec<(u8, i64, u64, SessionHit)> = Vec::new();
    // ponytail: a linear scan over every loaded session; an index when this shows up in profiles.
    for e in stage.sessions.values() {
        let f = &e.data;
        let named = [Some(f.agent.as_str()), f.title.as_deref(), f.cwd.as_deref()]
            .into_iter()
            .flatten()
            .any(|t| t.to_lowercase().contains(&query));
        let found = if named { None } else { content_match(f, &query) };
        if named || found.is_some() {
            let (post, text) = found.unzip();
            let hit = SessionHit {
                session: session_of(f),
                post: post.flatten(),
                text,
            };
            hits.push((u8::from(!named), f.last_active_at, e.ord, hit));
        }
    }
    hits.sort_by_key(|(rank, active, ord, _)| (*rank, Reverse(*active), Reverse(*ord)));
    Ok(hits.into_iter().take(limit).map(|h| h.3).collect())
}

/// The first post title, text line or comment containing `query`.
fn content_match(f: &SessionFile, query: &str) -> Option<(Option<PostId>, String)> {
    let line_of = |text: &str| {
        text.lines()
            .find(|l| l.to_lowercase().contains(query))
            .map(|l| clip(l.trim(), 200).to_owned())
    };
    for p in &f.posts {
        if let Some(t) = line_of(&p.title) {
            return Some((Some(p.id.clone()), t));
        }
        for part in &p.parts {
            let text = match part {
                Part::Markdown { markdown, .. } => Some(markdown.as_str()),
                Part::Code { code, .. } => Some(code.as_str()),
                Part::Terminal { text, .. } => Some(text.as_str()),
                Part::Mermaid { mermaid, .. } => Some(mermaid.as_str()),
                Part::Diff { patch, .. } => patch.as_deref(),
                Part::Html { .. }
                | Part::Image { .. }
                | Part::Trace { .. }
                | Part::Json { .. } => None,
            };
            if let Some(t) = text.and_then(line_of) {
                return Some((Some(p.id.clone()), t));
            }
        }
    }
    f.comments
        .iter()
        .find_map(|c| line_of(&c.text).map(|t| (c.post.clone(), t)))
}

// ---- Posts ---------------------------------------------------------------------

fn kinds_of(parts: &[Part]) -> Vec<String> {
    parts.iter().map(|p| part_kind(p).to_owned()).collect()
}

fn summary(session: &SessionId, p: &PostRec) -> PostSummary {
    PostSummary {
        id: p.id.clone(),
        session: session.clone(),
        title: p.title.clone(),
        kinds: kinds_of(&p.parts),
        version: p.version,
        updated_at: p.updated_at,
    }
}

/// Give every part without an id (or with a duplicate one) a fresh id.
fn normalize_ids(s: &Store, parts: &mut [Part]) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    for part in parts.iter_mut() {
        let slot = part_id_mut(part);
        if slot
            .as_deref()
            .is_none_or(|id| id.is_empty() || !seen.insert(id.to_owned()))
        {
            let id = new_id(s)?;
            seen.insert(id.clone());
            *slot = Some(id);
        }
    }
    Ok(())
}

/// User comments in the session the agent hasn't seen, advancing the cursor
/// past everything read (agent comments too), like sideshow's collectFeedback.
fn collect_feedback(f: &mut SessionFile) -> Vec<Comment> {
    let fresh: Vec<Comment> = f
        .comments
        .iter()
        .filter(|c| c.seq > f.agent_seq)
        .cloned()
        .collect();
    if let Some(last) = fresh.iter().map(|c| c.seq).max() {
        f.agent_seq = f.agent_seq.max(last);
    }
    fresh.into_iter().filter(|c| c.author == "user").collect()
}

/// The newest session started by conversation `owner`, if any.
pub fn owned_session(s: &Store, owner: &str) -> Option<SessionId> {
    s.stage()
        .sessions
        .values()
        .filter(|e| e.data.owner.as_deref() == Some(owner))
        .max_by_key(|e| (e.data.last_active_at, e.ord))
        .map(|e| e.data.id.clone())
}

/// Publish a post. With no `session`, joins the caller's conversation's session
/// (`args.owner`), or without one the newest for (`agent`, `cwd`); else starts one.
pub fn post_new(
    s: &Store,
    args: &PostNewArgs,
    agent: Option<&str>,
    cwd: Option<&str>,
) -> Result<Written<PostWriteResult>> {
    validate_parts(s, &args.parts, None)?;
    let mut events = Vec::new();
    let mut file = match &args.session {
        Some(id) => load(s, id)?,
        None => {
            let agent = reserved_agent(agent);
            let owner = args.owner.as_deref();
            let found = match owner {
                Some(owner) => owned_session(s, owner).map(|id| load(s, &id)).transpose()?,
                None => s
                    .stage()
                    .sessions
                    .values()
                    .filter(|e| e.data.agent == agent && e.data.cwd.as_deref() == cwd)
                    .max_by_key(|e| (e.data.last_active_at, e.ord))
                    .map(|e| e.data.clone()),
            };
            match found {
                Some(f) => f,
                None => {
                    let f = create_session(s, Some(&agent), args.session_title.as_deref(), cwd, owner)?;
                    events.push(event("session-created", &f.id, Some(&f.id), None));
                    f
                }
            }
        }
    };
    let mut parts = args.parts.clone();
    normalize_ids(s, &mut parts)?;
    let at = now();
    let post = PostRec {
        id: new_id(s)?.into(),
        title: trimmed(args.title.as_deref(), MAX_TITLE).unwrap_or_else(|| "Untitled".into()),
        version: 1,
        created_at: at,
        updated_at: at,
        parts,
        history: Vec::new(),
    };
    let result = PostWriteResult {
        post: summary(&file.id, &post),
        feedback: Vec::new(),
    };
    events.push(event("post-created", &post.id, Some(&file.id), Some(1)));
    file.posts.push(Arc::new(post));
    file.last_active_at = at;
    let feedback = collect_feedback(&mut file);
    s.save(file)?;
    Ok((PostWriteResult { feedback, ..result }, events))
}

/// Archive the current version and write the next one. `parts: None` keeps the parts.
fn write_version(
    s: &Store,
    mut file: SessionFile,
    at: usize,
    title: Option<&str>,
    parts: Option<Vec<Part>>,
) -> Result<Written<PostWriteResult>> {
    let time = now();
    let p = Arc::make_mut(&mut file.posts[at]);
    p.history.push(VersionRec {
        version: p.version,
        title: p.title.clone(),
        at: p.updated_at,
        parts: p.parts.clone(),
    });
    if let Some(title) = trimmed(title, MAX_TITLE) {
        p.title = title;
    }
    if let Some(parts) = parts {
        p.parts = parts;
    }
    p.version += 1;
    p.updated_at = time;
    let post = summary(&file.id, p);
    file.last_active_at = time;
    let feedback = collect_feedback(&mut file);
    let ev = event("post-updated", &post.id, Some(&file.id), Some(post.version));
    s.save(file)?;
    Ok((PostWriteResult { post, feedback }, vec![ev]))
}

/// A new version with a new title and/or parts. Replacing the parts assigns
/// fresh ids to parts that don't carry one.
pub fn post_update(s: &Store, args: &PostUpdateArgs) -> Result<Written<PostWriteResult>> {
    if let Some(parts) = &args.parts {
        validate_parts(s, parts, None)?;
    }
    let (file, at) = load_post(s, &args.id)?;
    let parts = match &args.parts {
        Some(parts) => {
            let mut parts = parts.clone();
            normalize_ids(s, &mut parts)?;
            Some(parts)
        }
        None => None,
    };
    write_version(s, file, at, args.title.as_deref(), parts)
}

/// A part id, else a 0-based index below `len`.
fn find_part(parts: &[Part], target: &str, len: usize) -> Option<usize> {
    parts
        .iter()
        .position(|p| part_id(p) == Some(target))
        .or_else(|| target.parse().ok().filter(|i| *i < len))
}

/// Add, replace or remove one part (a new version). `set` keeps the old part's id.
pub fn post_part(s: &Store, args: &PostPartArgs) -> Result<Written<PostWriteResult>> {
    let (file, at) = load_post(s, &args.id)?;
    let mut parts = file.posts[at].parts.clone();
    let missing = |t: &str| not_found(format!("post \"{}\" has no part \"{t}\"", args.id));
    let need_part = || {
        args.part
            .clone()
            .ok_or_else(|| usage(format!("post-part {:?} needs a part", args.op)))
    };
    let need_target = || {
        args.target
            .as_deref()
            .ok_or_else(|| usage(format!("post-part {:?} needs a target", args.op)))
    };
    let changed = match args.op {
        PartOp::Add => {
            let i = match args.target.as_deref() {
                None => parts.len(),
                Some(t) => find_part(&parts, t, parts.len() + 1).ok_or_else(|| missing(t))?,
            };
            let mut part = need_part()?;
            if part_id(&part).is_some_and(|id| parts.iter().any(|p| part_id(p) == Some(id))) {
                *part_id_mut(&mut part) = None;
            }
            parts.insert(i, part);
            Some(i)
        }
        PartOp::Set => {
            let t = need_target()?;
            let i = find_part(&parts, t, parts.len()).ok_or_else(|| missing(t))?;
            let mut part = need_part()?;
            *part_id_mut(&mut part) = part_id(&parts[i]).map(Into::into);
            parts[i] = part;
            Some(i)
        }
        PartOp::Rm => {
            let t = need_target()?;
            let i = find_part(&parts, t, parts.len()).ok_or_else(|| missing(t))?;
            if parts.len() == 1 {
                return Err(usage("a post needs at least one part"));
            }
            parts.remove(i);
            None
        }
    };
    if let Some(i) = changed {
        validate_parts(s, &parts, Some(i))?;
    }
    normalize_ids(s, &mut parts)?;
    write_version(s, file, at, None, Some(parts))
}

/// Newest created first, optionally in one session. Updates never reorder posts:
/// a session reads as the order its cards were made.
pub fn post_list(s: &Store, args: &PostListArgs) -> Result<Vec<PostSummary>> {
    if let Some(id) = &args.session {
        read(s, id, |_| ())?;
    }
    let stage = s.stage();
    let mut posts: Vec<(i64, u64, PostSummary)> = stage
        .sessions
        .values()
        .filter(|e| args.session.as_ref().is_none_or(|id| *id == e.data.id))
        .flat_map(|e| {
            e.data
                .posts
                .iter()
                .map(|p| (p.created_at, stage.index.post_ord(&p.id), summary(&e.data.id, p)))
        })
        .collect();
    posts.sort_by_key(|(created, ord, _)| Reverse((*created, *ord)));
    let limit = args.limit.map_or(usize::MAX, |l| l as usize);
    Ok(posts.into_iter().take(limit).map(|p| p.2).collect())
}

/// The current version, or any older one.
pub fn post_show(s: &Store, args: &PostShowArgs) -> Result<Post> {
    read_post(s, &args.id, |file, at| show(file, at, args))?
}

fn show(file: &SessionFile, at: usize, args: &PostShowArgs) -> Result<Post> {
    let p = &file.posts[at];
    let (title, parts, version, updated_at) = match args.version {
        None => (&p.title, &p.parts, p.version, p.updated_at),
        Some(v) if v == p.version => (&p.title, &p.parts, v, p.updated_at),
        Some(v) => p
            .history
            .iter()
            .find(|h| h.version == v)
            .map(|h| (&h.title, &h.parts, v, h.at))
            .ok_or_else(|| {
                not_found(format!(
                    "post \"{}\" has no version {v} (current is {})",
                    args.id, p.version
                ))
            })?,
    };
    Ok(Post {
        id: p.id.clone(),
        session: file.id.clone(),
        title: title.clone(),
        parts: parts.clone(),
        version,
        created_at: p.created_at,
        updated_at,
    })
}

/// Deletes the post, its history and its comments.
pub fn post_delete(s: &Store, id: &str) -> Result<Vec<Event>> {
    let (mut file, at) = load_post(s, id)?;
    file.posts.remove(at);
    file.comments.retain(|c| c.post.as_deref() != Some(id));
    file.last_active_at = now();
    let ev = event("post-deleted", id, Some(&file.id), None);
    s.save(file)?;
    Ok(vec![ev])
}

// ---- Comments ------------------------------------------------------------------

fn unit(v: f64, name: &str) -> Result<f64> {
    if v.is_finite() && (0.0..=1.0).contains(&v) {
        Ok(v)
    } else {
        Err(usage(format!("anchor.{name} must be between 0 and 1")))
    }
}

/// Check an anchor against the post and pin it to the current version.
fn sanitize_anchor(
    anchor: &CommentAnchor,
    part_count: usize,
    version: u64,
) -> Result<CommentAnchor> {
    let check_part = |part: u32| {
        if (part as usize) < part_count {
            Ok(part)
        } else {
            Err(usage(format!(
                "anchor.part {part} is out of range (post has {part_count} parts)"
            )))
        }
    };
    Ok(match anchor {
        CommentAnchor::Point { part, x, y, .. } => CommentAnchor::Point {
            part: check_part(*part)?,
            post_version: version,
            x: unit(*x, "x")?,
            y: unit(*y, "y")?,
        },
        CommentAnchor::Rect {
            part, x, y, w, h, ..
        } => CommentAnchor::Rect {
            part: check_part(*part)?,
            post_version: version,
            x: unit(*x, "x")?,
            y: unit(*y, "y")?,
            w: unit(*w, "w")?,
            h: unit(*h, "h")?,
        },
        CommentAnchor::LineRange {
            part,
            start_line,
            end_line,
            file,
            ..
        } => {
            if *start_line < 1 {
                return Err(usage("anchor.start_line must be at least 1"));
            }
            CommentAnchor::LineRange {
                part: check_part(*part)?,
                post_version: version,
                start_line: *start_line,
                end_line: (*end_line).max(*start_line),
                file: file.as_deref().map(|f| clip(f, MAX_TITLE).to_owned()),
            }
        }
    })
}

/// Comment on a post (its session is implied) or a session. `author`
/// defaults to `args.author`, then the request's `agent`, then the session's
/// agent. The daemon should [`CommentSignal::notify`](crate::CommentSignal::notify) after this commits.
pub fn comment_new(
    s: &Store,
    args: &CommentNewArgs,
    agent: Option<&str>,
) -> Result<Written<Comment>> {
    let text = clip(args.text.trim(), MAX_COMMENT_TEXT).to_owned();
    if text.is_empty() {
        return Err(usage("comment text is empty"));
    }
    let (mut file, post, anchor) = match &args.post {
        Some(post_id) => {
            let (file, at) = load_post(s, post_id)?;
            if args.session.as_ref().is_some_and(|id| *id != file.id) {
                return Err(usage(format!(
                    "post \"{post_id}\" is not in session \"{}\"",
                    args.session.as_deref().unwrap_or("")
                )));
            }
            let p = &file.posts[at];
            let anchor = match &args.anchor {
                Some(a) => Some(sanitize_anchor(a, p.parts.len(), p.version)?),
                None => None,
            };
            (file, Some(post_id.clone()), anchor)
        }
        None => {
            let id = args
                .session
                .as_deref()
                .ok_or_else(|| usage("comment-new needs a post or a session"))?;
            if args.anchor.is_some() {
                return Err(usage("an anchored comment needs a post"));
            }
            (load(s, id)?, None, None)
        }
    };
    let author = trimmed(args.author.as_deref(), 100)
        .or_else(|| trimmed(agent, 100))
        .unwrap_or_else(|| file.agent.clone());
    let comment = Comment {
        id: new_id(s)?.into(),
        seq: s.stage().index.next_comment_seq(),
        session: file.id.clone(),
        post,
        author,
        text,
        created_at: now(),
        anchor,
    };
    file.comments.push(comment.clone());
    file.last_active_at = comment.created_at;
    let ev = event("comment-created", &comment.id, Some(&file.id), None);
    s.save(file)?;
    Ok((comment, vec![ev]))
}

/// Comments in seq order, filtered by session, post and/or `after_seq`.
pub fn comment_list(s: &Store, args: &CommentListArgs) -> Result<Vec<Comment>> {
    let stage = s.stage();
    let mut comments: Vec<Comment> = stage
        .sessions
        .values()
        .filter(|e| args.session.as_ref().is_none_or(|id| *id == e.data.id))
        .flat_map(|e| e.data.comments.iter())
        .filter(|c| args.post.is_none() || c.post == args.post)
        .filter(|c| c.seq > args.after_seq.unwrap_or(0))
        .cloned()
        .collect();
    comments.sort_by_key(|c| c.seq);
    Ok(comments)
}

pub fn comment_delete(s: &Store, id: &str) -> Result<Vec<Event>> {
    let session = s
        .stage()
        .index
        .comment_session(id)
        .cloned()
        .ok_or_else(|| not_found(format!("comment \"{id}\" not found")))?;
    let mut file = load(s, &session)?;
    file.comments.retain(|c| c.id != id);
    file.last_active_at = now();
    s.save(file)?;
    Ok(vec![event("comment-deleted", id, Some(&session), None)])
}

/// The first `user` comment after `after_seq` (default: what the agent has
/// already seen, the session's cursor) in the session, or on `post` when
/// given. Returning one advances the cursor past it, so it isn't re-delivered
/// as feedback. Pair with [`CommentSignal::wait_for`](crate::CommentSignal::wait_for) for `wait`.
pub fn next_user_comment(
    s: &Store,
    session: Option<&str>,
    post: Option<&str>,
    after_seq: Option<u64>,
) -> Result<Option<Comment>> {
    let mut file = match (post, session) {
        (Some(p), _) => load_post(s, p)?.0,
        (None, Some(id)) => load(s, id)?,
        (None, None) => return Err(usage("wait needs a session or a post")),
    };
    let after = after_seq.unwrap_or(file.agent_seq);
    let found = file
        .comments
        .iter()
        .filter(|c| c.author == "user" && c.seq > after)
        .filter(|c| post.is_none() || c.post.as_deref() == post)
        .min_by_key(|c| c.seq)
        .cloned();
    if let Some(c) = &found {
        if c.seq > file.agent_seq {
            file.agent_seq = c.seq;
            s.save(file)?;
        }
    }
    Ok(found)
}

// ---- Assets --------------------------------------------------------------------

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            write!(s, "{b:02x}").unwrap();
            s
        })
}

fn info_of(m: &AssetMeta) -> AssetInfo {
    AssetInfo {
        id: m.id.clone(),
        kind: m.kind,
        content_type: m.content_type.clone(),
        byte_length: m.byte_length,
        filename: m.filename.clone(),
        created_at: m.created_at,
    }
}

fn asset_meta(s: &Store, id: &str) -> Option<AssetMeta> {
    if !s.asset_exists(id) {
        return None;
    }
    let bytes = fs::read(s.asset_path(&format!("{id}.json"))).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn asset_metas(s: &Store) -> Vec<AssetMeta> {
    let Ok(entries) = fs::read_dir(s.root().join(crate::json_store::ASSETS)) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.strip_suffix(".json").map(str::to_owned))
        .filter_map(|id| asset_meta(s, &id))
        .collect()
}

/// Every asset id a post (current or past version, live or trashed session) still points at.
fn referenced_assets(s: &Store) -> std::collections::HashSet<String> {
    let stage = s.stage();
    let trashed = s.trashed();
    let mut ids = std::collections::HashSet::new();
    for data in stage.sessions.values().map(|e| &e.data).chain(trashed.iter().map(|t| &t.2)) {
        for p in &data.posts {
            let versions = std::iter::once(&p.parts).chain(p.history.iter().map(|h| &h.parts));
            for part in versions.flatten() {
                match part {
                    Part::Image { asset_id, .. } => {
                        ids.insert(asset_id.to_string());
                    }
                    Part::Trace { asset_id: Some(a), .. } => {
                        ids.insert(a.to_string());
                    }
                    _ => {}
                }
            }
        }
    }
    ids
}

/// Store bytes under their SHA-256. Re-uploading the same bytes is a no-op
/// that returns the original record (keeping its kind, filename and time).
pub fn asset_put(s: &Store, args: &AssetPutArgs) -> Result<AssetInfo> {
    // Reject oversize before decoding: base64 is 4 chars per 3 bytes.
    if args.data_base64.len() / 4 * 3 > MAX_ASSET_BYTES + 3 {
        return Err(usage(format!("asset exceeds {MAX_ASSET_BYTES} bytes")));
    }
    let data = STANDARD
        .decode(args.data_base64.trim())
        .map_err(|e| usage(format!("data_base64: {e}")))?;
    if data.is_empty() {
        return Err(usage("empty upload"));
    }
    if data.len() > MAX_ASSET_BYTES {
        return Err(usage(format!("asset exceeds {MAX_ASSET_BYTES} bytes")));
    }
    if let Some(id) = &args.session {
        read(s, id, |_| ())?;
    }
    let id = hex(&Sha256::digest(&data));
    if let Some(meta) = asset_meta(s, &id) {
        return Ok(info_of(&meta));
    }
    let content_type = match args.content_type.trim() {
        "" => "application/octet-stream",
        ct => clip(ct, MAX_TITLE),
    };
    let meta = AssetMeta {
        schema: SCHEMA,
        id: id.clone().into(),
        session: args.session.clone(),
        kind: args.kind,
        content_type: content_type.into(),
        byte_length: data.len() as u64,
        filename: args.filename.as_deref().map(|f| clip(f, MAX_TITLE).to_owned()),
        created_at: now(),
    };
    s.write_asset_file(&s.asset_path(&id), &data)?;
    let mut json = serde_json::to_vec_pretty(&meta).expect("asset meta serializes");
    json.push(b'\n');
    s.write_asset_file(&s.asset_path(&format!("{id}.json")), &json)?;
    Ok(info_of(&meta))
}

pub fn asset_get(s: &Store, id: &str) -> Result<AssetData> {
    let missing = || not_found(format!("asset \"{id}\" not found"));
    let meta = asset_meta(s, id).ok_or_else(missing)?;
    let data = fs::read(s.asset_path(id)).map_err(|_| missing())?;
    Ok(AssetData {
        info: info_of(&meta),
        data_base64: STANDARD.encode(data),
    })
}

// ---- Traces --------------------------------------------------------------------

/// Replace a session's trace. Fields are truncated and only the last
/// [`MAX_TRACE_STEPS`] steps are kept. Returns the stored step count.
pub fn trace_set(s: &Store, args: &TraceSetArgs) -> Result<Written<usize>> {
    let mut file = load(s, &args.session)?;
    let skip = args.steps.len().saturating_sub(MAX_TRACE_STEPS);
    file.trace = args.steps[skip..]
        .iter()
        .map(|step| TraceStep {
            kind: step.kind.as_deref().map(|k| clip(k, MAX_STEP_KIND).to_owned()),
            label: clip(&step.label, MAX_STEP_LABEL).to_owned(),
            detail: step.detail.as_deref().map(|d| clip(d, MAX_STEP_DETAIL).to_owned()),
            ts: step.ts.clone(),
        })
        .collect();
    file.last_active_at = now();
    let count = file.trace.len();
    s.save(file)?;
    Ok((
        count,
        vec![event("trace-updated", &args.session, Some(&args.session), None)],
    ))
}

pub fn trace_list(s: &Store, session: &str) -> Result<Vec<TraceStep>> {
    read(s, session, |f| f.trace.clone())
}

// ---- Settings ------------------------------------------------------------------

pub fn setting_get(s: &Store, key: &str) -> Result<Option<String>> {
    s.db.prepare_cached("SELECT value FROM settings WHERE key = ?1")
        .and_then(|mut st| st.query_row([key], |r| r.get(0)).optional())
        .db()
}

pub fn setting_set(s: &Store, args: &SettingSetArgs) -> Result<Vec<Event>> {
    if args.key.trim().is_empty() {
        return Err(usage("setting key is empty"));
    }
    s.db.prepare_cached("INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = excluded.value")
        .and_then(|mut st| st.execute(params![args.key, args.value]))
        .db()?;
    Ok(vec![event("setting-changed", &args.key, None, None)])
}
