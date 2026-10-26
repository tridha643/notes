//! Read-only view of the pre-JSON SQLite tables, used once by migrate_json.rs to
//! move every session into the folder. Nothing writes these tables any more.

use notes_proto::posts::*;
use rusqlite::{params, Connection};

use crate::{
    json_store::{AssetMeta, PostRec, SessionFile, VersionRec, SCHEMA},
    Db, Result,
};

/// Tables the JSON folder replaced, dropped after a verified export.
pub(crate) const LEGACY_TABLES: [&str; 7] = [
    "sessions",
    "posts",
    "post_versions",
    "asset_refs",
    "comments",
    "assets",
    "trace_steps",
];

pub(crate) fn has_legacy_tables(db: &Connection) -> Result<bool> {
    db.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'sessions'",
        [],
        |r| r.get::<_, i64>(0),
    )
    .db()
    .map(|n| n > 0)
}

fn parts(json: &str) -> Result<Vec<Part>> {
    serde_json::from_str(json).map_err(|e| crate::other(format!("stored parts: {e}")))
}

/// Every legacy session as the file it becomes.
pub(crate) fn export_sessions(db: &Connection) -> Result<Vec<SessionFile>> {
    let mut sessions = Vec::new();
    let mut stmt = db
        .prepare("SELECT id, agent, title, cwd, created_at, last_active_at, agent_seq FROM sessions ORDER BY rowid")
        .db()?;
    let rows = stmt
        .query_map([], |r| {
            Ok(SessionFile {
                schema: SCHEMA,
                id: SessionId(r.get(0)?),
                agent: r.get(1)?,
                title: r.get(2)?,
                cwd: r.get(3)?,
                created_at: r.get(4)?,
                last_active_at: r.get(5)?,
                agent_seq: r.get(6)?,
                posts: Vec::new(),
                comments: Vec::new(),
                trace: Vec::new(),
            })
        })
        .db()?;
    for row in rows {
        let mut s = row.db()?;
        s.posts = export_posts(db, &s.id)?;
        s.comments = export_comments(db, &s.id)?;
        s.trace = export_trace(db, &s.id)?;
        sessions.push(s);
    }
    Ok(sessions)
}

fn export_posts(db: &Connection, session: &str) -> Result<Vec<PostRec>> {
    let mut stmt = db
        .prepare("SELECT id, title, parts, version, created_at, updated_at FROM posts WHERE session = ?1 ORDER BY rowid")
        .db()?;
    let rows: Vec<(String, String, String, u64, i64, i64)> = stmt
        .query_map([session], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))
        .db()?
        .collect::<rusqlite::Result<_>>()
        .db()?;
    let mut versions = db
        .prepare("SELECT version, title, parts, at FROM post_versions WHERE post = ?1 ORDER BY version")
        .db()?;
    let mut posts = Vec::new();
    for (id, title, json, version, created_at, updated_at) in rows {
        let history = versions
            .query_map(params![id], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, String>(2)?, r.get(3)?)))
            .db()?
            .map(|row| {
                let (version, title, json, at) = row.db()?;
                Ok(VersionRec { version, title, at, parts: parts(&json)? })
            })
            .collect::<Result<_>>()?;
        posts.push(PostRec {
            id: PostId(id),
            title,
            version,
            created_at,
            updated_at,
            parts: parts(&json)?,
            history,
        });
    }
    Ok(posts)
}

fn export_comments(db: &Connection, session: &str) -> Result<Vec<Comment>> {
    let mut stmt = db
        .prepare("SELECT seq, id, post, author, text, created_at, anchor FROM comments WHERE session = ?1 ORDER BY seq")
        .db()?;
    let rows = stmt
        .query_map([session], |r| {
            let anchor: Option<String> = r.get(6)?;
            Ok(Comment {
                seq: r.get(0)?,
                id: CommentId(r.get(1)?),
                session: SessionId(session.into()),
                post: r.get::<_, Option<String>>(2)?.map(PostId),
                author: r.get(3)?,
                text: r.get(4)?,
                created_at: r.get(5)?,
                anchor: anchor.and_then(|a| serde_json::from_str(&a).ok()),
            })
        })
        .db()?;
    rows.collect::<rusqlite::Result<_>>().db()
}

fn export_trace(db: &Connection, session: &str) -> Result<Vec<TraceStep>> {
    let mut stmt = db
        .prepare("SELECT kind, label, detail, ts FROM trace_steps WHERE session = ?1 ORDER BY seq")
        .db()?;
    let rows = stmt
        .query_map([session], |r| {
            Ok(TraceStep { kind: r.get(0)?, label: r.get(1)?, detail: r.get(2)?, ts: r.get(3)? })
        })
        .db()?;
    rows.collect::<rusqlite::Result<_>>().db()
}

/// Every legacy asset with its bytes.
pub(crate) fn export_assets(db: &Connection) -> Result<Vec<(AssetMeta, Vec<u8>)>> {
    let mut stmt = db
        .prepare("SELECT id, session, kind, content_type, byte_length, filename, created_at, data FROM assets ORDER BY rowid")
        .db()?;
    let rows = stmt
        .query_map([], |r| {
            let kind: String = r.get(2)?;
            Ok((
                AssetMeta {
                    schema: SCHEMA,
                    id: AssetId(r.get(0)?),
                    session: r.get::<_, Option<String>>(1)?.map(SessionId),
                    kind: match kind.as_str() {
                        "image" => AssetKind::Image,
                        "trace" => AssetKind::Trace,
                        _ => AssetKind::File,
                    },
                    content_type: r.get(3)?,
                    byte_length: r.get(4)?,
                    filename: r.get(5)?,
                    created_at: r.get(6)?,
                },
                r.get(7)?,
            ))
        })
        .db()?;
    rows.collect::<rusqlite::Result<_>>().db()
}
