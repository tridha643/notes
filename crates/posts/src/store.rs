//! Every sideshow verb over SQLite. Mirrors docs/ref/sideshow/server/sqlStore.ts
//! and the flow functions in app.ts.

use std::time::{SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::STANDARD, Engine};
use notes_proto::{posts::*, Event};
use rusqlite::{params, Connection, OptionalExtension, Row};
use sha2::{Digest, Sha256};

use crate::{
    not_found, usage,
    validate::{
        clip, part_id, part_id_mut, part_kind, validate_parts, MAX_ASSET_BYTES, MAX_COMMENT_TEXT,
        MAX_STEP_DETAIL, MAX_STEP_KIND, MAX_STEP_LABEL, MAX_TITLE, MAX_TRACE_STEPS,
    },
    Db, Result, Written,
};

/// Past versions kept per post (sideshow's HISTORY_LIMIT); older ones are pruned.
pub const HISTORY_LIMIT: u64 = 20;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// 8 random bytes as 11 url-safe base64 chars, never starting with `-`/`_`
/// (they'd read as CLI flags). Sideshow's newId.
fn new_id(conn: &Connection) -> Result<String> {
    let bytes: Vec<u8> = conn
        .prepare_cached("SELECT randomblob(8)")
        .and_then(|mut s| s.query_row([], |r| r.get(0)))
        .db()?;
    let mut id = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    if id.starts_with(['-', '_']) {
        id.replace_range(..1, "0");
    }
    Ok(id)
}

fn event(kind: &str, id: &str, session: Option<&str>, version: Option<u64>) -> Event {
    Event {
        seq: 0,
        kind: kind.into(),
        path: None,
        version,
        id: Some(id.into()),
        session: session.map(Into::into),
    }
}

/// Run `f` inside a SAVEPOINT: all of it lands or none does, and it nests
/// inside a transaction the caller already opened.
fn atomic<T>(conn: &Connection, f: impl FnOnce() -> Result<T>) -> Result<T> {
    conn.execute_batch("SAVEPOINT notes_posts").db()?;
    match f() {
        Ok(v) => {
            conn.execute_batch("RELEASE notes_posts").db()?;
            Ok(v)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK TO notes_posts; RELEASE notes_posts");
            Err(e)
        }
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

fn touch(conn: &Connection, session: &str) -> Result<()> {
    conn.prepare_cached("UPDATE sessions SET last_active_at = ?1 WHERE id = ?2")
        .and_then(|mut s| s.execute(params![now(), session]))
        .db()?;
    Ok(())
}

// ---- Sessions ------------------------------------------------------------------

const SESSION_SELECT: &str = "SELECT s.id, s.agent, s.title, s.cwd, s.created_at, s.last_active_at,
    (SELECT count(*) FROM posts p WHERE p.session = s.id) FROM sessions s";

fn session_row(r: &Row) -> rusqlite::Result<Session> {
    Ok(Session {
        id: r.get(0)?,
        agent: r.get(1)?,
        title: r.get(2)?,
        cwd: r.get(3)?,
        created_at: r.get(4)?,
        last_active_at: r.get(5)?,
        post_count: r.get(6)?,
    })
}

pub fn session_get(conn: &Connection, id: &str) -> Result<Session> {
    conn.prepare_cached(&format!("{SESSION_SELECT} WHERE s.id = ?1"))
        .and_then(|mut s| s.query_row([id], session_row).optional())
        .db()?
        .ok_or_else(|| not_found(format!("session \"{id}\" not found")))
}

fn create_session(
    conn: &Connection,
    agent: Option<&str>,
    title: Option<&str>,
    cwd: Option<&str>,
) -> Result<Session> {
    let at = now();
    let session = Session {
        id: new_id(conn)?,
        agent: reserved_agent(agent),
        title: trimmed(title, MAX_TITLE),
        cwd: cwd.map(Into::into),
        created_at: at,
        last_active_at: at,
        post_count: 0,
    };
    conn.prepare_cached(
        "INSERT INTO sessions (id, agent, title, cwd, created_at, last_active_at) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
    )
    .and_then(|mut s| s.execute(params![session.id, session.agent, session.title, session.cwd, at]))
    .db()?;
    Ok(session)
}

pub fn session_new(conn: &Connection, args: &SessionNewArgs) -> Result<Written<Session>> {
    let session = create_session(
        conn,
        Some(&args.agent),
        args.title.as_deref(),
        args.cwd.as_deref(),
    )?;
    let ev = event("session-created", &session.id, Some(&session.id), None);
    Ok((session, vec![ev]))
}

/// Most recently active first.
pub fn session_list(conn: &Connection) -> Result<Vec<Session>> {
    let mut stmt = conn
        .prepare_cached(&format!(
            "{SESSION_SELECT} ORDER BY s.last_active_at DESC, s.rowid DESC"
        ))
        .db()?;
    let rows = stmt.query_map([], session_row).db()?;
    rows.collect::<rusqlite::Result<_>>().db()
}

/// An empty title clears it.
pub fn session_rename(conn: &Connection, args: &SessionRenameArgs) -> Result<Written<Session>> {
    let title = trimmed(Some(&args.title), MAX_TITLE);
    let n = conn
        .execute(
            "UPDATE sessions SET title = ?1 WHERE id = ?2",
            params![title, args.id],
        )
        .db()?;
    if n == 0 {
        return Err(not_found(format!("session \"{}\" not found", args.id)));
    }
    let session = session_get(conn, &args.id)?;
    Ok((
        session,
        vec![event("session-updated", &args.id, Some(&args.id), None)],
    ))
}

/// Deletes the session's posts, versions, comments and trace, and its assets
/// unless a surviving post references them (assets are shared by content hash).
pub fn session_delete(conn: &Connection, id: &str) -> Result<Vec<Event>> {
    atomic(conn, || {
        session_get(conn, id)?;
        for sql in [
            "DELETE FROM comments WHERE session = ?1",
            "DELETE FROM post_versions WHERE post IN (SELECT id FROM posts WHERE session = ?1)",
            "DELETE FROM asset_refs WHERE post IN (SELECT id FROM posts WHERE session = ?1)",
            "DELETE FROM posts WHERE session = ?1",
            "DELETE FROM trace_steps WHERE session = ?1",
            "DELETE FROM assets WHERE session = ?1 AND id NOT IN (SELECT asset FROM asset_refs)",
            "DELETE FROM sessions WHERE id = ?1",
        ] {
            conn.prepare_cached(sql)
                .and_then(|mut s| s.execute([id]))
                .db()?;
        }
        Ok(vec![event("session-deleted", id, Some(id), None)])
    })
}

// ---- Posts ---------------------------------------------------------------------

struct PostRow {
    session: String,
    title: String,
    parts: String,
    version: u64,
    created_at: i64,
    updated_at: i64,
}

fn post_row(conn: &Connection, id: &str) -> Result<PostRow> {
    conn.prepare_cached(
        "SELECT session, title, parts, version, created_at, updated_at FROM posts WHERE id = ?1",
    )
    .and_then(|mut s| {
        s.query_row([id], |r| {
            Ok(PostRow {
                session: r.get(0)?,
                title: r.get(1)?,
                parts: r.get(2)?,
                version: r.get(3)?,
                created_at: r.get(4)?,
                updated_at: r.get(5)?,
            })
        })
        .optional()
    })
    .db()?
    .ok_or_else(|| not_found(format!("post \"{id}\" not found")))
}

fn parse_parts(json: &str) -> Result<Vec<Part>> {
    serde_json::from_str(json)
        .map_err(|e| crate::error(notes_proto::ErrorCode::Other, format!("stored parts: {e}")))
}

fn kinds_of(parts: &[Part]) -> String {
    parts.iter().map(part_kind).collect::<Vec<_>>().join(",")
}

fn summary(
    id: &str,
    session: &str,
    title: &str,
    kinds: &str,
    version: u64,
    updated_at: i64,
) -> PostSummary {
    PostSummary {
        id: id.into(),
        session: session.into(),
        title: title.into(),
        kinds: kinds
            .split(',')
            .filter(|k| !k.is_empty())
            .map(Into::into)
            .collect(),
        version,
        updated_at,
    }
}

/// Give every part without an id (or with a duplicate one) a fresh id.
fn normalize_ids(conn: &Connection, parts: &mut [Part]) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    for part in parts.iter_mut() {
        let slot = part_id_mut(part);
        if slot
            .as_deref()
            .is_none_or(|id| id.is_empty() || !seen.insert(id.to_owned()))
        {
            let id = new_id(conn)?;
            seen.insert(id.clone());
            *slot = Some(id);
        }
    }
    Ok(())
}

fn add_asset_refs(conn: &Connection, post: &str, parts: &[Part]) -> Result<()> {
    let mut stmt = conn
        .prepare_cached("INSERT OR IGNORE INTO asset_refs (asset, post) VALUES (?1, ?2)")
        .db()?;
    for part in parts {
        let asset = match part {
            Part::Image { asset_id, .. } => Some(asset_id),
            Part::Trace { asset_id, .. } => asset_id.as_ref(),
            _ => None,
        };
        if let Some(asset) = asset {
            stmt.execute(params![asset, post]).db()?;
        }
    }
    Ok(())
}

fn comment_row(r: &Row) -> rusqlite::Result<Comment> {
    let anchor: Option<String> = r.get(7)?;
    Ok(Comment {
        seq: r.get(0)?,
        id: r.get(1)?,
        session: r.get(2)?,
        post: r.get(3)?,
        author: r.get(4)?,
        text: r.get(5)?,
        created_at: r.get(6)?,
        anchor: anchor.and_then(|a| serde_json::from_str(&a).ok()),
    })
}

const COMMENT_COLS: &str = "seq, id, session, post, author, text, created_at, anchor";

/// User comments in the session the agent hasn't seen, advancing the cursor
/// past everything read (agent comments too), like sideshow's collectFeedback.
fn collect_feedback(conn: &Connection, session: &str) -> Result<Vec<Comment>> {
    let fresh: Vec<Comment> = conn
        .prepare_cached(&format!(
            "SELECT {COMMENT_COLS} FROM comments
             WHERE session = ?1 AND seq > (SELECT agent_seq FROM sessions WHERE id = ?1) ORDER BY seq"
        ))
        .and_then(|mut s| s.query_map([session], comment_row)?.collect())
        .db()?;
    if let Some(last) = fresh.last() {
        mark_seen(conn, session, last.seq)?;
    }
    Ok(fresh.into_iter().filter(|c| c.author == "user").collect())
}

fn mark_seen(conn: &Connection, session: &str, seq: u64) -> Result<()> {
    conn.prepare_cached("UPDATE sessions SET agent_seq = ?1 WHERE id = ?2 AND agent_seq < ?1")
        .and_then(|mut s| s.execute(params![seq, session]))
        .db()?;
    Ok(())
}

/// Publish a post. With no `session`, reuses the newest session for
/// (`agent`, `cwd`) — the request's caller — or creates one.
pub fn post_new(
    conn: &Connection,
    args: &PostNewArgs,
    agent: Option<&str>,
    cwd: Option<&str>,
) -> Result<Written<PostWriteResult>> {
    validate_parts(conn, &args.parts, None)?;
    atomic(conn, || {
        let mut events = Vec::new();
        let session = match &args.session {
            Some(id) => session_get(conn, id)?.id,
            None => {
                let agent = reserved_agent(agent);
                let found: Option<String> = conn
                    .prepare_cached(
                        "SELECT id FROM sessions WHERE agent = ?1 AND cwd IS ?2
                         ORDER BY last_active_at DESC, rowid DESC LIMIT 1",
                    )
                    .and_then(|mut s| s.query_row(params![agent, cwd], |r| r.get(0)).optional())
                    .db()?;
                match found {
                    Some(id) => id,
                    None => {
                        let s = create_session(conn, Some(&agent), None, cwd)?;
                        events.push(event("session-created", &s.id, Some(&s.id), None));
                        s.id
                    }
                }
            }
        };
        let mut parts = args.parts.clone();
        normalize_ids(conn, &mut parts)?;
        let id = new_id(conn)?;
        let title = trimmed(args.title.as_deref(), MAX_TITLE).unwrap_or_else(|| "Untitled".into());
        let kinds = kinds_of(&parts);
        let at = now();
        let json = serde_json::to_string(&parts).expect("parts serialize");
        conn.prepare_cached(
            "INSERT INTO posts (id, session, title, parts, kinds, version, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?6)",
        )
        .and_then(|mut s| s.execute(params![id, session, title, json, kinds, at]))
        .db()?;
        add_asset_refs(conn, &id, &parts)?;
        touch(conn, &session)?;
        let feedback = collect_feedback(conn, &session)?;
        events.push(event("post-created", &id, Some(&session), Some(1)));
        Ok((
            PostWriteResult {
                post: summary(&id, &session, &title, &kinds, 1, at),
                feedback,
            },
            events,
        ))
    })
}

/// Archive the current version and write the next one. `parts: None` keeps
/// the stored parts without re-serializing them.
fn write_version(
    conn: &Connection,
    id: &str,
    current: PostRow,
    title: Option<&str>,
    parts: Option<&[Part]>,
) -> Result<Written<PostWriteResult>> {
    let title = trimmed(title, MAX_TITLE).unwrap_or(current.title);
    let version = current.version + 1;
    let at = now();
    conn.prepare_cached(
        "INSERT OR REPLACE INTO post_versions (post, version, title, parts, at)
         SELECT id, version, title, parts, updated_at FROM posts WHERE id = ?1",
    )
    .and_then(|mut s| s.execute([id]))
    .db()?;
    conn.prepare_cached("DELETE FROM post_versions WHERE post = ?1 AND version <= ?2")
        .and_then(|mut s| s.execute(params![id, version.saturating_sub(HISTORY_LIMIT + 1)]))
        .db()?;
    let kinds = match parts {
        Some(parts) => {
            let json = serde_json::to_string(parts).expect("parts serialize");
            let kinds = kinds_of(parts);
            conn.prepare_cached("UPDATE posts SET title = ?1, parts = ?2, kinds = ?3, version = ?4, updated_at = ?5 WHERE id = ?6")
                .and_then(|mut s| s.execute(params![title, json, kinds, version, at, id]))
                .db()?;
            add_asset_refs(conn, id, parts)?;
            kinds
        }
        None => {
            conn.prepare_cached(
                "UPDATE posts SET title = ?1, version = ?2, updated_at = ?3 WHERE id = ?4",
            )
            .and_then(|mut s| s.execute(params![title, version, at, id]))
            .db()?;
            conn.prepare_cached("SELECT kinds FROM posts WHERE id = ?1")
                .and_then(|mut s| s.query_row([id], |r| r.get(0)))
                .db()?
        }
    };
    touch(conn, &current.session)?;
    let feedback = collect_feedback(conn, &current.session)?;
    let ev = event("post-updated", id, Some(&current.session), Some(version));
    Ok((
        PostWriteResult {
            post: summary(id, &current.session, &title, &kinds, version, at),
            feedback,
        },
        vec![ev],
    ))
}

/// A new version with a new title and/or parts. Replacing the parts assigns
/// fresh ids to parts that don't carry one.
pub fn post_update(conn: &Connection, args: &PostUpdateArgs) -> Result<Written<PostWriteResult>> {
    if let Some(parts) = &args.parts {
        validate_parts(conn, parts, None)?;
    }
    atomic(conn, || {
        let current = post_row(conn, &args.id)?;
        let parts = match &args.parts {
            Some(parts) => {
                let mut parts = parts.clone();
                normalize_ids(conn, &mut parts)?;
                Some(parts)
            }
            None => None,
        };
        write_version(
            conn,
            &args.id,
            current,
            args.title.as_deref(),
            parts.as_deref(),
        )
    })
}

/// A part id, else a 0-based index below `len`.
fn find_part(parts: &[Part], target: &str, len: usize) -> Option<usize> {
    parts
        .iter()
        .position(|p| part_id(p) == Some(target))
        .or_else(|| target.parse().ok().filter(|i| *i < len))
}

/// Add, replace or remove one part (a new version). `set` keeps the old part's id.
pub fn post_part(conn: &Connection, args: &PostPartArgs) -> Result<Written<PostWriteResult>> {
    atomic(conn, || {
        let current = post_row(conn, &args.id)?;
        let mut parts = parse_parts(&current.parts)?;
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
                let at = match args.target.as_deref() {
                    None => parts.len(),
                    Some(t) => find_part(&parts, t, parts.len() + 1).ok_or_else(|| missing(t))?,
                };
                let mut part = need_part()?;
                if part_id(&part).is_some_and(|id| parts.iter().any(|p| part_id(p) == Some(id))) {
                    *part_id_mut(&mut part) = None;
                }
                parts.insert(at, part);
                Some(at)
            }
            PartOp::Set => {
                let t = need_target()?;
                let at = find_part(&parts, t, parts.len()).ok_or_else(|| missing(t))?;
                let mut part = need_part()?;
                *part_id_mut(&mut part) = part_id(&parts[at]).map(Into::into);
                parts[at] = part;
                Some(at)
            }
            PartOp::Rm => {
                let t = need_target()?;
                let at = find_part(&parts, t, parts.len()).ok_or_else(|| missing(t))?;
                if parts.len() == 1 {
                    return Err(usage("a post needs at least one part"));
                }
                parts.remove(at);
                None
            }
        };
        if let Some(i) = changed {
            validate_parts(conn, &parts, Some(i))?;
        }
        normalize_ids(conn, &mut parts)?;
        write_version(conn, &args.id, current, None, Some(&parts))
    })
}

/// Most recently updated first, optionally in one session.
pub fn post_list(conn: &Connection, args: &PostListArgs) -> Result<Vec<PostSummary>> {
    if let Some(s) = &args.session {
        session_get(conn, s)?;
    }
    let limit = args.limit.map_or(-1, i64::from);
    let mut stmt = conn
        .prepare_cached(
            "SELECT id, session, title, kinds, version, updated_at FROM posts
             WHERE ?1 IS NULL OR session = ?1 ORDER BY updated_at DESC, rowid DESC LIMIT ?2",
        )
        .db()?;
    let rows = stmt
        .query_map(params![args.session, limit], |r| {
            Ok(summary(
                &r.get::<_, String>(0)?,
                &r.get::<_, String>(1)?,
                &r.get::<_, String>(2)?,
                &r.get::<_, String>(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })
        .db()?;
    rows.collect::<rusqlite::Result<_>>().db()
}

/// The current version, or an older one still in history.
pub fn post_show(conn: &Connection, args: &PostShowArgs) -> Result<Post> {
    let current = post_row(conn, &args.id)?;
    let (title, parts, version, updated_at) = match args.version {
        None => (current.title, current.parts, current.version, current.updated_at),
        Some(v) if v == current.version => (current.title, current.parts, v, current.updated_at),
        Some(v) => conn
            .prepare_cached("SELECT title, parts, at FROM post_versions WHERE post = ?1 AND version = ?2")
            .and_then(|mut s| s.query_row(params![args.id, v], |r| Ok((r.get(0)?, r.get(1)?, v, r.get(2)?))).optional())
            .db()?
            .ok_or_else(|| {
                not_found(format!(
                    "post \"{}\" has no version {v} (current is {}; the last {HISTORY_LIMIT} are kept)",
                    args.id, current.version
                ))
            })?,
    };
    Ok(Post {
        id: args.id.clone(),
        session: current.session,
        title,
        parts: parse_parts(&parts)?,
        version,
        created_at: current.created_at,
        updated_at,
    })
}

/// Deletes the post, its history and its comments.
pub fn post_delete(conn: &Connection, id: &str) -> Result<Vec<Event>> {
    atomic(conn, || {
        let current = post_row(conn, id)?;
        for sql in [
            "DELETE FROM comments WHERE post = ?1",
            "DELETE FROM post_versions WHERE post = ?1",
            "DELETE FROM asset_refs WHERE post = ?1",
            "DELETE FROM posts WHERE id = ?1",
        ] {
            conn.prepare_cached(sql)
                .and_then(|mut s| s.execute([id]))
                .db()?;
        }
        touch(conn, &current.session)?;
        Ok(vec![event(
            "post-deleted",
            id,
            Some(&current.session),
            None,
        )])
    })
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
    conn: &Connection,
    args: &CommentNewArgs,
    agent: Option<&str>,
) -> Result<Written<Comment>> {
    let text = clip(args.text.trim(), MAX_COMMENT_TEXT).to_owned();
    if text.is_empty() {
        return Err(usage("comment text is empty"));
    }
    atomic(conn, || {
        let (session, post, anchor) = match &args.post {
            Some(post_id) => {
                let post = post_row(conn, post_id)?;
                if args.session.as_ref().is_some_and(|s| *s != post.session) {
                    return Err(usage(format!(
                        "post \"{post_id}\" is not in session \"{}\"",
                        args.session.as_deref().unwrap_or("")
                    )));
                }
                let anchor = match &args.anchor {
                    Some(a) => Some(sanitize_anchor(
                        a,
                        parse_parts(&post.parts)?.len(),
                        post.version,
                    )?),
                    None => None,
                };
                (post.session, Some(post_id.clone()), anchor)
            }
            None => {
                let id = args
                    .session
                    .as_deref()
                    .ok_or_else(|| usage("comment-new needs a post or a session"))?;
                if args.anchor.is_some() {
                    return Err(usage("an anchored comment needs a post"));
                }
                (session_get(conn, id)?.id, None, None)
            }
        };
        let author = match trimmed(args.author.as_deref(), 100).or_else(|| trimmed(agent, 100)) {
            Some(a) => a,
            None => session_get(conn, &session)?.agent,
        };
        let id = new_id(conn)?;
        let created_at = now();
        let anchor_json = anchor
            .as_ref()
            .map(|a| serde_json::to_string(a).expect("anchor serializes"));
        conn.prepare_cached(
            "INSERT INTO comments (id, session, post, author, text, created_at, anchor) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .and_then(|mut s| s.execute(params![id, session, post, author, text, created_at, anchor_json]))
        .db()?;
        let seq = conn.last_insert_rowid() as u64;
        touch(conn, &session)?;
        let ev = event("comment-created", &id, Some(&session), None);
        Ok((
            Comment {
                id,
                seq,
                session,
                post,
                author,
                text,
                created_at,
                anchor,
            },
            vec![ev],
        ))
    })
}

/// Comments in seq order, filtered by session, post and/or `after_seq`.
pub fn comment_list(conn: &Connection, args: &CommentListArgs) -> Result<Vec<Comment>> {
    let mut stmt = conn
        .prepare_cached(&format!(
            "SELECT {COMMENT_COLS} FROM comments
             WHERE (?1 IS NULL OR session = ?1) AND (?2 IS NULL OR post = ?2) AND seq > ?3 ORDER BY seq"
        ))
        .db()?;
    let rows = stmt
        .query_map(
            params![args.session, args.post, args.after_seq.unwrap_or(0)],
            comment_row,
        )
        .db()?;
    rows.collect::<rusqlite::Result<_>>().db()
}

pub fn comment_delete(conn: &Connection, id: &str) -> Result<Vec<Event>> {
    atomic(conn, || {
        let session: String = conn
            .prepare_cached("DELETE FROM comments WHERE id = ?1 RETURNING session")
            .and_then(|mut s| s.query_row([id], |r| r.get(0)).optional())
            .db()?
            .ok_or_else(|| not_found(format!("comment \"{id}\" not found")))?;
        touch(conn, &session)?;
        Ok(vec![event("comment-deleted", id, Some(&session), None)])
    })
}

/// The first `user` comment after `after_seq` (default: what the agent has
/// already seen, the session's cursor) in the session, or on `post` when
/// given. Returning one advances the cursor past it, so it isn't re-delivered
/// as feedback. Pair with [`CommentSignal::wait_for`](crate::CommentSignal::wait_for) for `wait`.
pub fn next_user_comment(
    conn: &Connection,
    session: Option<&str>,
    post: Option<&str>,
    after_seq: Option<u64>,
) -> Result<Option<Comment>> {
    let session = match (post, session) {
        (Some(p), _) => post_row(conn, p)?.session,
        (None, Some(s)) => s.to_owned(),
        (None, None) => return Err(usage("wait needs a session or a post")),
    };
    let cursor: u64 = conn
        .prepare_cached("SELECT agent_seq FROM sessions WHERE id = ?1")
        .and_then(|mut s| s.query_row([&session], |r| r.get(0)).optional())
        .db()?
        .ok_or_else(|| not_found(format!("session \"{session}\" not found")))?;
    let found = conn
        .prepare_cached(&format!(
            "SELECT {COMMENT_COLS} FROM comments
             WHERE session = ?1 AND (?2 IS NULL OR post = ?2) AND author = 'user' AND seq > ?3 ORDER BY seq LIMIT 1"
        ))
        .and_then(|mut s| s.query_row(params![session, post, after_seq.unwrap_or(cursor)], comment_row).optional())
        .db()?;
    if let Some(c) = &found {
        mark_seen(conn, &session, c.seq)?;
    }
    Ok(found)
}

// ---- Assets --------------------------------------------------------------------

fn kind_name(kind: AssetKind) -> &'static str {
    match kind {
        AssetKind::Image => "image",
        AssetKind::Trace => "trace",
        AssetKind::File => "file",
    }
}

fn asset_info(r: &Row) -> rusqlite::Result<AssetInfo> {
    let kind: String = r.get(1)?;
    Ok(AssetInfo {
        id: r.get(0)?,
        kind: match kind.as_str() {
            "image" => AssetKind::Image,
            "trace" => AssetKind::Trace,
            _ => AssetKind::File,
        },
        content_type: r.get(2)?,
        byte_length: r.get(3)?,
        filename: r.get(4)?,
        created_at: r.get(5)?,
    })
}

const ASSET_COLS: &str = "id, kind, content_type, byte_length, filename, created_at";

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            write!(s, "{b:02x}").unwrap();
            s
        })
}

/// Store bytes under their SHA-256. Re-uploading the same bytes is a no-op
/// that returns the original record (keeping its kind, filename and time).
pub fn asset_put(conn: &Connection, args: &AssetPutArgs) -> Result<AssetInfo> {
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
    if let Some(s) = &args.session {
        session_get(conn, s)?;
    }
    let id = hex(&Sha256::digest(&data));
    let at = now();
    let content_type = match args.content_type.trim() {
        "" => "application/octet-stream",
        ct => clip(ct, MAX_TITLE),
    };
    let inserted = conn
        .prepare_cached(
            "INSERT OR IGNORE INTO assets (id, session, kind, content_type, byte_length, filename, data, created_at, last_accessed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
        )
        .and_then(|mut s| {
            s.execute(params![
                id,
                args.session,
                kind_name(args.kind),
                content_type,
                data.len(),
                args.filename.as_deref().map(|f| clip(f, MAX_TITLE)),
                data,
                at
            ])
        })
        .db()?;
    if inserted == 0 {
        conn.prepare_cached("UPDATE assets SET last_accessed = ?1 WHERE id = ?2")
            .and_then(|mut s| s.execute(params![at, id]))
            .db()?;
    }
    conn.prepare_cached(&format!("SELECT {ASSET_COLS} FROM assets WHERE id = ?1"))
        .and_then(|mut s| s.query_row([&id], asset_info))
        .db()
}

pub fn asset_get(conn: &Connection, id: &str) -> Result<AssetData> {
    let (info, data) = conn
        .prepare_cached(&format!(
            "SELECT {ASSET_COLS}, data FROM assets WHERE id = ?1"
        ))
        .and_then(|mut s| {
            s.query_row([id], |r| Ok((asset_info(r)?, r.get::<_, Vec<u8>>(6)?)))
                .optional()
        })
        .db()?
        .ok_or_else(|| not_found(format!("asset \"{id}\" not found")))?;
    conn.prepare_cached("UPDATE assets SET last_accessed = ?1 WHERE id = ?2")
        .and_then(|mut s| s.execute(params![now(), id]))
        .db()?;
    Ok(AssetData {
        info,
        data_base64: STANDARD.encode(data),
    })
}

// ---- Traces --------------------------------------------------------------------

/// Replace a session's trace. Fields are truncated and only the last
/// [`MAX_TRACE_STEPS`] steps are kept. Returns the stored step count.
pub fn trace_set(conn: &Connection, args: &TraceSetArgs) -> Result<Written<usize>> {
    atomic(conn, || {
        session_get(conn, &args.session)?;
        conn.prepare_cached("DELETE FROM trace_steps WHERE session = ?1")
            .and_then(|mut s| s.execute([&args.session]))
            .db()?;
        let skip = args.steps.len().saturating_sub(MAX_TRACE_STEPS);
        let mut stmt = conn
            .prepare_cached("INSERT INTO trace_steps (session, seq, kind, label, detail, ts) VALUES (?1, ?2, ?3, ?4, ?5, ?6)")
            .db()?;
        for (seq, step) in args.steps[skip..].iter().enumerate() {
            stmt.execute(params![
                args.session,
                seq,
                step.kind.as_deref().map(|k| clip(k, MAX_STEP_KIND)),
                clip(&step.label, MAX_STEP_LABEL),
                step.detail.as_deref().map(|d| clip(d, MAX_STEP_DETAIL)),
                step.ts,
            ])
            .db()?;
        }
        touch(conn, &args.session)?;
        let count = args.steps.len() - skip;
        Ok((
            count,
            vec![event(
                "trace-updated",
                &args.session,
                Some(&args.session),
                None,
            )],
        ))
    })
}

pub fn trace_list(conn: &Connection, session: &str) -> Result<Vec<TraceStep>> {
    session_get(conn, session)?;
    let mut stmt = conn
        .prepare_cached(
            "SELECT kind, label, detail, ts FROM trace_steps WHERE session = ?1 ORDER BY seq",
        )
        .db()?;
    let rows = stmt
        .query_map([session], |r| {
            Ok(TraceStep {
                kind: r.get(0)?,
                label: r.get(1)?,
                detail: r.get(2)?,
                ts: r.get(3)?,
            })
        })
        .db()?;
    rows.collect::<rusqlite::Result<_>>().db()
}

// ---- Settings ------------------------------------------------------------------

pub fn setting_get(conn: &Connection, key: &str) -> Result<Option<String>> {
    conn.prepare_cached("SELECT value FROM settings WHERE key = ?1")
        .and_then(|mut s| s.query_row([key], |r| r.get(0)).optional())
        .db()
}

pub fn setting_set(conn: &Connection, args: &SettingSetArgs) -> Result<Vec<Event>> {
    if args.key.trim().is_empty() {
        return Err(usage("setting key is empty"));
    }
    conn.prepare_cached("INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = excluded.value")
        .and_then(|mut s| s.execute(params![args.key, args.value]))
        .db()?;
    Ok(vec![event("setting-changed", &args.key, None, None)])
}
