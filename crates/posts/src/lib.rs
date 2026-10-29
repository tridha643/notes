//! Sideshow engine for `notesd`: sessions, versioned posts made of typed parts,
//! anchored comments, content-addressed assets, traces, kits, settings, and the
//! daemon-side renderer (code highlighting, diff hunks, ANSI terminals).
//!
//! Synchronous, over a [`Store`]: one JSON file per session in the session
//! folder (json_store.rs) plus the caller's `rusqlite::Connection` for settings
//! and the render cache. Inside a daemon unit, file writes are journaled so a
//! failed op or `batch --atomic` rolls the folder back ([`Store::begin`],
//! [`Store::rollback`]). The crate never writes an event log: write functions return the
//! [`Event`]s they caused with `seq: 0`, and the daemon assigns `seq` when it
//! appends them to its log. After a successful [`comment_new`] the daemon calls
//! [`CommentSignal::notify`] so `wait`ers wake.
//!
//! Errors are wire-ready [`ErrorBody`]s: `not_found` for missing records,
//! `usage` for validation failures (the same rules as sideshow's
//! `postSurfaces.ts`, plus its size limits), `other` for SQLite and file failures.
//!
//! # Highlighter
//!
//! Pierre's Highlights (`pierre/highlights.wasm`), a WebAssembly lexer for
//! 80 languages, run in the wasmi interpreter. It replaced tree-sitter: on a
//! 57 KB Rust file it lexes in ~7 ms warm where tree-sitter took ~21 ms, with
//! no per-language query compile (one ~4 ms module compile per process).
//! Unknown languages render as plain lines. Diff sides highlight in parallel
//! on scoped threads. Numbers: `crates/posts/BENCH.md`.
//!
//! # Render cache
//!
//! 1. **Key** = SHA-256 of [`RENDER_VERSION`] plus every part field the
//!    renderer reads (not ids or titles, so retitling doesn't re-render). The
//!    key covers the whole render input, so an entry never needs invalidating
//!    and there is no TTL: a changed part is a different key. The obligations
//!    are on the renderer: any change to its output (code here, or a grammar
//!    crate upgrade) must bump `RENDER_VERSION`, which orphans every old entry,
//!    and a field it starts reading must join `cache_key` (render.rs).
//! 2. **Bounded**: an in-memory LRU capped by bytes (64 MiB by default) sits in
//!    front of the `render_cache` table, which is trimmed to 256 MiB
//!    oldest-first. Both tiers hold the part's wire JSON, so a hit is a lookup
//!    and a copy; answer `post-render` with [`post_render_json`] and splice the
//!    string into the response line rather than round-tripping it through
//!    `serde_json::Value`.
//! 3. **Stats**: [`RenderCache::stats`] returns hits, misses, entries and bytes
//!    (of the in-memory tier) for `doctor`. Table hits count as hits.
//! 4. **Rollback switch**: `NOTES_RENDER_CACHE=off` bypasses both tiers; every
//!    render is then computed fresh and counted as a miss.

use std::{
    sync::{Condvar, Mutex},
    time::{Duration, Instant},
};

use notes_proto::{ErrorBody, ErrorCode, Event};
use rusqlite::Connection;

mod ansi;
mod diff;
mod highlight;
mod json_store;
mod migrate_json;
mod render;
mod sqlite_store;
mod stage_index;
mod store;
mod validate;

pub use json_store::{AssetMeta, PostRec, SessionFile, Store, VersionRec, SCHEMA};
pub use render::{
    post_render, post_render_json, render_part, CacheStats, RenderCache,
    RENDER_VERSION,
};
pub use store::*;
pub use validate::{
    part_bytes, part_kind, MAX_ASSET_BYTES, MAX_COMMENT_TEXT, MAX_PART_BYTES, MAX_POST_BYTES,
    MAX_TITLE, MAX_TRACE_STEPS,
};

pub type Result<T> = std::result::Result<T, ErrorBody>;

/// A write's value plus the events it caused (`seq` is 0 until the daemon logs them).
pub type Written<T> = (T, Vec<Event>);

pub(crate) fn error(code: ErrorCode, message: impl Into<String>) -> ErrorBody {
    ErrorBody {
        code,
        message: message.into(),
        hint: None,
        details: None,
    }
}

pub(crate) fn usage(message: impl Into<String>) -> ErrorBody {
    error(ErrorCode::Usage, message)
}

pub(crate) fn not_found(message: impl Into<String>) -> ErrorBody {
    error(ErrorCode::NotFound, message)
}

pub(crate) fn other(message: impl Into<String>) -> ErrorBody {
    error(ErrorCode::Other, message)
}

pub(crate) trait Db<T> {
    fn db(self) -> Result<T>;
}

impl<T> Db<T> for rusqlite::Result<T> {
    fn db(self) -> Result<T> {
        self.map_err(|e| error(ErrorCode::Other, format!("database: {e}")))
    }
}

/// Create the tables sideshow still keeps in SQLite (settings, render cache).
/// Idempotent. Session data lives in the folder ([`Store`]).
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS render_cache (
            key BLOB PRIMARY KEY, value BLOB NOT NULL, bytes INTEGER NOT NULL
        );",
    )
    .db()
}

/// Wakes `wait`ers when a comment lands. A generation counter under a Mutex +
/// Condvar: no polling, no timers. The daemon calls [`notify`](Self::notify)
/// after each committed [`comment_new`] and blocks in [`wait_for`](Self::wait_for).
#[derive(Debug, Default)]
pub struct CommentSignal {
    generation: Mutex<u64>,
    cond: Condvar,
}

impl CommentSignal {
    pub fn notify(&self) {
        *self.generation.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        self.cond.notify_all();
    }

    pub fn generation(&self) -> u64 {
        *self.generation.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Block until the generation moves past `seen` or `timeout` elapses
    /// (`None` waits forever). Returns the generation; `== seen` means timeout.
    pub fn wait_timeout(&self, seen: u64, timeout: Option<Duration>) -> u64 {
        let guard = self.generation.lock().unwrap_or_else(|e| e.into_inner());
        let guard = match timeout {
            None => self
                .cond
                .wait_while(guard, |g| *g == seen)
                .unwrap_or_else(|e| e.into_inner()),
            Some(t) => {
                self.cond
                    .wait_timeout_while(guard, t, |g| *g == seen)
                    .unwrap_or_else(|e| e.into_inner())
                    .0
            }
        };
        *guard
    }

    /// Run `check` (typically [`next_user_comment`] under the connection lock)
    /// until it yields a comment, sleeping on the condvar between attempts.
    /// Returns `Ok(None)` on timeout (`None` = no timeout).
    pub fn wait_for<T>(
        &self,
        timeout: Option<Duration>,
        mut check: impl FnMut() -> Result<Option<T>>,
    ) -> Result<Option<T>> {
        let deadline = timeout.map(|t| Instant::now() + t);
        loop {
            // Read the generation before checking, so a notify between the
            // check and the wait still wakes us.
            let seen = self.generation();
            if let Some(found) = check()? {
                return Ok(Some(found));
            }
            let left = match deadline {
                Some(d) => match d.checked_duration_since(Instant::now()) {
                    Some(left) if !left.is_zero() => Some(left),
                    _ => return Ok(None),
                },
                None => None,
            };
            self.wait_timeout(seen, left);
        }
    }
}
