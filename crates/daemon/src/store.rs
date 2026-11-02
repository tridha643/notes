//! SQLite and the session folder, mutated only inside a [`Tx`]: a failed op (or atomic
//! batch) rolls back both.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Condvar, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use notes_proto::Event;
use rusqlite::{Connection, params};

use crate::{Fail, R};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS events(
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  kind TEXT NOT NULL,
  path TEXT,
  version INTEGER,
  at INTEGER NOT NULL,
  ext_id TEXT,
  session TEXT);
";

pub struct Daemon {
    pub socket: PathBuf,
    pub db_path: PathBuf,
    /// SQLite plus the session folder; holding it is also the write lock for the folder.
    db: Mutex<notes_posts::Store>,
    /// Latest committed event seq; watchers wait on the condvar.
    bus: (Mutex<u64>, Condvar),
    /// Sideshow render output, keyed by content hash (see notes_posts::RenderCache).
    pub render: notes_posts::RenderCache,
    /// Wakes `wait` requests when a comment commits.
    pub comments: notes_posts::CommentSignal,
}

pub struct Tx<'a> {
    pub d: &'a Daemon,
    pub db: &'a Connection,
    /// Session verbs go through this; `db` is its connection.
    pub store: &'a notes_posts::Store,
    pub author: &'a str,
    pub now: i64,
    events: bool,
    /// Caller's working directory; picks the implicit session.
    pub cwd: Option<String>,
    /// A comment committed in this unit, so `wait`ers must re-check.
    pub commented: bool,
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

impl Daemon {
    pub fn open(db_path: PathBuf, socket: PathBuf, stage: PathBuf) -> R<Daemon> {
        let state = db_path.parent().unwrap_or(Path::new(".")).to_path_buf();
        fs::create_dir_all(&state)?;
        let db = Connection::open(&db_path)?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        db.execute_batch(SCHEMA)?;
        // Databases from before events carried ids.
        if !db.prepare("SELECT ext_id FROM events LIMIT 0").is_ok() {
            db.execute_batch("ALTER TABLE events ADD COLUMN ext_id TEXT; ALTER TABLE events ADD COLUMN session TEXT;")?;
        }
        let seq: Option<i64> = db.query_row("SELECT max(seq) FROM events", [], |r| r.get(0))?;
        let db = notes_posts::Store::open(db, stage).map_err(Fail)?;
        Ok(Daemon {
            socket,
            db_path,
            db: Mutex::new(db),
            bus: (Mutex::new(seq.unwrap_or(0) as u64), Condvar::new()),
            render: notes_posts::RenderCache::new(),
            comments: notes_posts::CommentSignal::default(),
        })
    }

    /// Run `f` as one unit: commit both SQLite and files, or roll back both.
    pub fn unit<T>(&self, author: &str, f: impl FnOnce(&mut Tx) -> R<T>) -> R<T> {
        let store = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let db = &store.db;
        if !db.is_autocommit() {
            let _ = db.execute_batch("ROLLBACK"); // left open by a thread that panicked mid-unit
        }
        db.execute_batch("BEGIN IMMEDIATE")?;
        store.begin();
        let mut tx = Tx {
            d: self,
            db,
            store: &store,
            author,
            now: now(),
            events: false,
            cwd: None,
            commented: false,
        };
        let result = f(&mut tx).and_then(|v| Ok(db.execute_batch("COMMIT").map(|()| v)?));
        if result.is_err() {
            let _ = db.execute_batch("ROLLBACK");
            store.rollback();
        } else {
            store.commit();
        }
        if tx.events && result.is_ok() {
            let seq: i64 = db.query_row("SELECT max(seq) FROM events", [], |r| r.get(0))?;
            *self.bus.0.lock().unwrap_or_else(|e| e.into_inner()) = seq as u64;
            self.bus.1.notify_all();
        }
        if tx.commented && result.is_ok() {
            self.comments.notify();
        }
        result
    }

    /// Committed events after `since`, oldest first.
    pub fn events_after(&self, since: u64, limit: u32) -> R<Vec<Event>> {
        let store = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = store.db.prepare_cached(
"SELECT seq, kind, path, version, ext_id, session FROM events WHERE seq > ?1 ORDER BY seq LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![since as i64, limit], |r| {
            Ok(Event {
                seq: r.get::<_, i64>(0)? as u64,
                kind: r.get(1)?,
                path: r.get(2)?,
                version: r.get::<_, Option<i64>>(3)?.map(|v| v as u64),
                id: r.get(4)?,
                session: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn latest_seq(&self) -> u64 {
        *self.bus.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Block until an event after `seq` commits or `stop()` turns true. No timeout.
    pub fn wait_for_event(&self, seq: u64, stop: impl Fn() -> bool) {
        let mut latest = self.bus.0.lock().unwrap_or_else(|e| e.into_inner());
        while *latest <= seq && !stop() {
            latest = self.bus.1.wait(latest).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Wake every waiter so it can re-check its stop condition.
    pub fn wake_waiters(&self) {
        let _guard = self.bus.0.lock().unwrap_or_else(|e| e.into_inner());
        self.bus.1.notify_all();
    }

    /// Pick up session files added, removed or replaced outside notesd.
    pub fn stage_refresh(&self) {
        let result = self.unit("external", |tx| {
            let events = tx.store.refresh().map_err(Fail)?;
            tx.post_events(events)
        });
        if let Err(Fail(e)) = result {
            eprintln!("notesd: session folder refresh failed: {}", e.message);
        }
    }

    pub fn stage_path(&self) -> PathBuf {
        self.db.lock().unwrap_or_else(|e| e.into_inner()).root().to_path_buf()
    }

    /// Startup: purge session trash older than its retention.
    pub fn purge_trash(&self) -> R<()> {
        self.unit("external", |tx| notes_posts::trash_purge(tx.store, tx.now).map_err(Fail))
    }
}

impl Tx<'_> {
    /// Append events returned by notes_posts (their `seq` is assigned here).
    pub fn post_events(&mut self, events: Vec<Event>) -> R<()> {
        for e in events {
            self.db
                .prepare_cached("INSERT INTO events(kind, path, version, at, ext_id, session) VALUES (?1, ?2, ?3, ?4, ?5, ?6)")?
                .execute(params![e.kind, e.path, e.version.map(|v| v as i64), self.now, e.id, e.session])?;
            self.events = true;
        }
        Ok(())
    }
}
