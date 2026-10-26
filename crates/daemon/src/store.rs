//! Vault files + SQLite, mutated only inside a [`Tx`]: one SQLite transaction plus a
//! journal of file changes, so a failed op (or atomic batch) rolls back both.

use std::{
    fs::{self, File},
    io::{self, Write},
    os::fd::AsRawFd,
    path::{Component, Path, PathBuf},
    sync::{Condvar, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use notes_proto::{ErrorCode, Event};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{Fail, R, fail, search::Search};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS notes(
  id INTEGER PRIMARY KEY,
  path TEXT UNIQUE,             -- NULL while the note is in the trash
  title TEXT NOT NULL,
  version INTEGER NOT NULL,
  hash TEXT NOT NULL,           -- blake3 of the current body
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS notes_title ON notes(title COLLATE NOCASE);
CREATE TABLE IF NOT EXISTS history(
  note_id INTEGER NOT NULL,
  version INTEGER NOT NULL,
  at INTEGER NOT NULL,
  author TEXT NOT NULL,         -- agent name, 'human' or 'external'
  body TEXT NOT NULL,
  PRIMARY KEY(note_id, version));
CREATE TABLE IF NOT EXISTS trash(
  id INTEGER PRIMARY KEY,
  note_id INTEGER NOT NULL,
  path TEXT NOT NULL,           -- original path
  deleted_at INTEGER NOT NULL,
  body TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS events(
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  kind TEXT NOT NULL,
  path TEXT,
  version INTEGER,
  at INTEGER NOT NULL,
  ext_id TEXT,
  session TEXT);
";

pub const TRASH_DAYS: i64 = 30;
const TMP_SUFFIX: &str = ".notesd-tmp";

pub struct Daemon {
    pub vault: PathBuf,
    pub socket: PathBuf,
    pub db_path: PathBuf,
    /// SQLite plus the sideshow session folder; holding it is also the write lock for the
    /// vault and the folder.
    /// ponytail: one lock for every note (per-note serialization falls out of it); per-note
    /// locks if writes to different notes ever need to run in parallel.
    db: Mutex<notes_posts::Store>,
    pub search: Search,
    /// Latest committed event seq; watchers wait on the condvar.
    bus: (Mutex<u64>, Condvar),
    /// Sideshow render output, keyed by content hash (see notes_posts::RenderCache).
    pub render: notes_posts::RenderCache,
    /// Wakes `wait` requests when a comment commits.
    pub comments: notes_posts::CommentSignal,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub id: i64,
    pub path: Option<String>,
    pub title: String,
    pub version: u64,
    pub hash: String,
    pub created_at: i64,
    pub updated_at: i64,
}

enum Undo {
    /// File at this path had these bytes (`None`: didn't exist).
    File(PathBuf, Option<Vec<u8>>),
    DirCreated(PathBuf),
    DirRemoved(PathBuf),
    Renamed(PathBuf, PathBuf),
}

pub struct Tx<'a> {
    pub d: &'a Daemon,
    pub db: &'a Connection,
    /// Sideshow verbs go through this; `db` is its connection.
    pub store: &'a notes_posts::Store,
    pub author: &'a str,
    pub now: i64,
    undo: Vec<Undo>,
    /// Absolute paths fff must re-read once the unit ends.
    touched: Vec<PathBuf>,
    events: bool,
    /// Caller's working directory; picks the implicit sideshow session.
    pub cwd: Option<String>,
    /// A comment committed in this unit, so `wait`ers must re-check.
    pub commented: bool,
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

pub fn hash(body: &str) -> String {
    blake3::hash(body.as_bytes()).to_hex().to_string()
}

pub fn title_of(rel: &str) -> String {
    Path::new(rel)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn fsync(file: &File) -> io::Result<()> {
    // ponytail: fsync, not F_FULLFSYNC (what std's sync_all does on macOS): same durability
    // SQLite gives by default here. Survives process crashes; use sync_all() for power loss.
    if unsafe { libc::fsync(file.as_raw_fd()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Temp file in the same directory, fsync, rename over the target, fsync the directory.
fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().expect("note paths have a parent");
    let name = path
        .file_name()
        .expect("note paths have a name")
        .to_string_lossy();
    let tmp = dir.join(format!(".{name}{TMP_SUFFIX}"));
    let result = (|| {
        let mut file = File::create(&tmp)?;
        file.write_all(bytes)?;
        fsync(&file)?;
        fs::rename(&tmp, path)?;
        fsync(&File::open(dir)?)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn row_from(r: &rusqlite::Row) -> rusqlite::Result<Row> {
    Ok(Row {
        id: r.get(0)?,
        path: r.get(1)?,
        title: r.get(2)?,
        version: r.get::<_, i64>(3)? as u64,
        hash: r.get(4)?,
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
    })
}
const ROW: &str = "SELECT id, path, title, version, hash, created_at, updated_at FROM notes";

impl Daemon {
    pub fn open(vault: PathBuf, db_path: PathBuf, socket: PathBuf, stage: PathBuf) -> R<Daemon> {
        let state = db_path.parent().unwrap_or(Path::new(".")).to_path_buf();
        fs::create_dir_all(&state)?;
        let db = Connection::open(&db_path)?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        db.execute_batch(SCHEMA)?;
        // Databases from before sideshow events carried ids.
        if !db.prepare("SELECT ext_id FROM events LIMIT 0").is_ok() {
            db.execute_batch("ALTER TABLE events ADD COLUMN ext_id TEXT; ALTER TABLE events ADD COLUMN session TEXT;")?;
        }
        let seq: Option<i64> = db.query_row("SELECT max(seq) FROM events", [], |r| r.get(0))?;
        let db = notes_posts::Store::open(db, stage).map_err(Fail)?;
        let search = Search::open(&vault, &state.join("fff"))?;
        Ok(Daemon {
            vault,
            socket,
            db_path,
            db: Mutex::new(db),
            search,
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
            undo: Vec::new(),
            touched: Vec::new(),
            events: false,
            cwd: None,
            commented: false,
        };
        let result = f(&mut tx).and_then(|v| Ok(db.execute_batch("COMMIT").map(|()| v)?));
        if result.is_err() {
            let _ = db.execute_batch("ROLLBACK");
            tx.undo_files();
            store.rollback();
        } else {
            store.commit();
        }
        self.search.update(&tx.touched);
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

    /// Record every note whose file differs from SQLite (outside edits, creates, deletes).
    pub fn sync_paths(&self, rels: &[String]) {
        let result = self.unit("external", |tx| {
            for rel in rels {
                if let Err(Fail(e)) = tx.sync(rel) {
                    eprintln!("notesd: skipping {rel}: {}", e.message);
                }
            }
            Ok(())
        });
        if let Err(Fail(e)) = result {
            eprintln!("notesd: sync failed: {}", e.message);
        }
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

    /// Startup: bring SQLite in line with the vault, drop stale temp files, purge old trash.
    pub fn reconcile(&self) -> R<()> {
        let mut rels = Vec::new();
        walk_notes(&self.vault, &self.vault, &mut rels, true);
        {
            let store = self.db.lock().unwrap_or_else(|e| e.into_inner());
            let mut stmt = store.db.prepare("SELECT path FROM notes WHERE path IS NOT NULL")?;
            let known = stmt.query_map([], |r| r.get::<_, String>(0))?;
            for rel in known {
                let rel = rel?;
                if !rels.contains(&rel) {
                    rels.push(rel);
                }
            }
        }
        self.sync_paths(&rels);
        self.unit("external", |tx| tx.purge_trash())
    }

    pub fn note_count(db: &Connection) -> R<u64> {
        Ok(db.query_row(
            "SELECT count(*) FROM notes WHERE path IS NOT NULL",
            [],
            |r| r.get::<_, i64>(0),
        )? as u64)
    }
}

/// Vault-relative `.md` paths under `dir` (hidden names and symlinks skipped).
/// With `clean_tmp`, also removes temp files left by a crashed write.
pub fn walk_notes(vault: &Path, dir: &Path, out: &mut Vec<String>, clean_tmp: bool) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if name.starts_with('.') {
            if clean_tmp && kind.is_file() && name.ends_with(TMP_SUFFIX) {
                let _ = fs::remove_file(entry.path());
            }
        } else if kind.is_dir() {
            walk_notes(vault, &entry.path(), out, clean_tmp);
        } else if kind.is_file() && name.ends_with(".md") {
            out.push(rel_string(vault, &entry.path()));
        }
    }
}

pub fn rel_string(vault: &Path, abs: &Path) -> String {
    abs.strip_prefix(vault)
        .unwrap_or(abs)
        .to_string_lossy()
        .into_owned()
}

impl Tx<'_> {
    pub fn abs(&self, rel: &str) -> PathBuf {
        self.d.vault.join(rel)
    }

    pub fn row(&self, rel: &str) -> R<Option<Row>> {
        Ok(self
            .db
            .prepare_cached(&format!("{ROW} WHERE path = ?1"))?
            .query_row([rel], row_from)
            .optional()?)
    }

    pub fn row_by_id(&self, id: i64) -> R<Row> {
        Ok(self
            .db
            .prepare_cached(&format!("{ROW} WHERE id = ?1"))?
            .query_row([id], row_from)?)
    }

    /// Paths of live notes with this title (case-insensitive).
    pub fn paths_titled(&self, title: &str) -> R<Vec<String>> {
        let mut stmt = self.db.prepare_cached("SELECT path FROM notes WHERE path IS NOT NULL AND title = ?1 COLLATE NOCASE ORDER BY path")?;
        let rows = stmt.query_map([title], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

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

    pub fn event(&mut self, kind: &str, path: &str, version: Option<u64>) -> R<()> {
        self.db
            .prepare_cached("INSERT INTO events(kind, path, version, at) VALUES (?1, ?2, ?3, ?4)")?
            .execute(params![kind, path, version.map(|v| v as i64), self.now])?;
        self.events = true;
        Ok(())
    }

    /// Bring one note's SQLite state in line with its file and return both.
    /// Outside creates/edits become versions authored `external`; an outside delete goes to the trash.
    pub fn sync(&mut self, rel: &str) -> R<Option<(Row, String)>> {
        let body =
            match fs::read(self.abs(rel)) {
                Ok(bytes) => Some(String::from_utf8(bytes).map_err(|_| {
                    fail(ErrorCode::Other, format!("{rel} is not UTF-8 text"), None)
                })?),
                Err(e) if e.kind() == io::ErrorKind::NotFound => None,
                Err(e) => return Err(e.into()),
            };
        let row = self.row(rel)?;
        match (row, body) {
            (None, None) => Ok(None),
            (Some(row), None) => {
                let last: String = self.db.query_row(
                    "SELECT body FROM history WHERE note_id = ?1 AND version = ?2",
                    params![row.id, row.version as i64],
                    |r| r.get(0),
                )?;
                self.touched.push(self.abs(rel));
                self.trash_note(&row, rel, &last)?;
                Ok(None)
            }
            (row, Some(body)) => {
                if row.as_ref().is_some_and(|r| r.hash == hash(&body)) {
                    return Ok(Some((row.unwrap(), body)));
                }
                self.touched.push(self.abs(rel));
                let kind = if row.is_some() {
                    "note-updated"
                } else {
                    "note-created"
                };
                let saved = self.record(row.as_ref(), rel, &body, "external", kind)?;
                Ok(Some((saved, body)))
            }
        }
    }

    /// Write `body` as the note's next version (or version 1 for a new note).
    /// Unchanged content is a no-op that returns the current row.
    pub fn save(&mut self, prev: Option<&Row>, rel: &str, body: &str, kind: &str) -> R<Row> {
        if let Some(p) = prev
            && p.path.as_deref() == Some(rel)
            && p.hash == hash(body)
        {
            return Ok(p.clone());
        }
        self.put_file(&self.abs(rel), body.as_bytes())?;
        let author = self.author;
        self.record(prev, rel, body, author, kind)
    }

    /// SQLite side of a new version: notes row, history snapshot, event.
    fn record(
        &mut self,
        prev: Option<&Row>,
        rel: &str,
        body: &str,
        author: &str,
        kind: &str,
    ) -> R<Row> {
        let (title, hash) = (title_of(rel), hash(body));
        let row = match prev {
            Some(p) => {
                let version = p.version + 1;
                self.db
                    .prepare_cached("UPDATE notes SET path = ?1, title = ?2, version = ?3, hash = ?4, updated_at = ?5 WHERE id = ?6")?
                    .execute(params![rel, title, version as i64, hash, self.now, p.id])?;
                Row {
                    path: Some(rel.into()),
                    title,
                    version,
                    hash,
                    updated_at: self.now,
                    ..p.clone()
                }
            }
            None => {
                self.db
                    .prepare_cached("INSERT INTO notes(path, title, version, hash, created_at, updated_at) VALUES (?1, ?2, 1, ?3, ?4, ?4)")?
                    .execute(params![rel, title, hash, self.now])?;
                let id = self.db.last_insert_rowid();
                Row {
                    id,
                    path: Some(rel.into()),
                    title,
                    version: 1,
                    hash,
                    created_at: self.now,
                    updated_at: self.now,
                }
            }
        };
        self.db
            .prepare_cached("INSERT INTO history(note_id, version, at, author, body) VALUES (?1, ?2, ?3, ?4, ?5)")?
            .execute(params![row.id, row.version as i64, self.now, author, body])?;
        self.event(kind, rel, Some(row.version))?;
        Ok(row)
    }

    pub fn trash_note(&mut self, row: &Row, rel: &str, body: &str) -> R<()> {
        self.db
            .prepare_cached(
                "INSERT INTO trash(note_id, path, deleted_at, body) VALUES (?1, ?2, ?3, ?4)",
            )?
            .execute(params![row.id, rel, self.now, body])?;
        self.db
            .execute("UPDATE notes SET path = NULL WHERE id = ?1", [row.id])?;
        self.event("note-deleted", rel, Some(row.version))
    }

    /// Opportunistic: runs at startup and after deletes, never on a timer.
    pub fn purge_trash(&mut self) -> R<()> {
        let cutoff = self.now - TRASH_DAYS * 86_400;
        self.db.execute("DELETE FROM history WHERE note_id IN (SELECT note_id FROM trash WHERE deleted_at < ?1)", [cutoff])?;
        self.db.execute("DELETE FROM notes WHERE path IS NULL AND id IN (SELECT note_id FROM trash WHERE deleted_at < ?1)", [cutoff])?;
        self.db
            .execute("DELETE FROM trash WHERE deleted_at < ?1", [cutoff])?;
        Ok(())
    }

    // File mutations: each is journaled first so a failed unit can undo it.

    pub fn put_file(&mut self, abs: &Path, bytes: &[u8]) -> R<()> {
        let old = match fs::read(abs) {
            Ok(old) => Some(old),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        self.undo.push(Undo::File(abs.into(), old));
        self.touched.push(abs.into());
        Ok(atomic_write(abs, bytes)?)
    }

    pub fn remove_file(&mut self, abs: &Path) -> R<()> {
        let old = fs::read(abs)?;
        self.undo.push(Undo::File(abs.into(), Some(old)));
        self.touched.push(abs.into());
        Ok(fs::remove_file(abs)?)
    }

    pub fn rename(&mut self, from: &Path, to: &Path) -> R<()> {
        fs::rename(from, to)?;
        self.undo.push(Undo::Renamed(from.into(), to.into()));
        self.touched.extend([from.into(), to.into()]);
        Ok(())
    }

    /// Create `rel` and any missing parents inside the vault, with a `folder-created` event each.
    pub fn mkdirs(&mut self, rel: &str) -> R<()> {
        let mut missing = Vec::new();
        let mut dir = Path::new(rel);
        while !dir.as_os_str().is_empty() && !self.abs(&dir.to_string_lossy()).exists() {
            missing.push(dir.to_path_buf());
            dir = dir.parent().unwrap_or(Path::new(""));
        }
        for dir in missing.iter().rev() {
            let abs = self.d.vault.join(dir);
            fs::create_dir(&abs)?;
            self.undo.push(Undo::DirCreated(abs));
            self.event("folder-created", &dir.to_string_lossy(), None)?;
        }
        Ok(())
    }

    pub fn rmdir(&mut self, abs: &Path) -> R<()> {
        fs::remove_dir(abs)?;
        self.undo.push(Undo::DirRemoved(abs.into()));
        Ok(())
    }

    fn undo_files(&mut self) {
        for undo in self.undo.drain(..).rev() {
            let result = match &undo {
                Undo::File(path, Some(bytes)) => atomic_write(path, bytes),
                Undo::File(path, None) => fs::remove_file(path),
                Undo::DirCreated(path) => fs::remove_dir(path),
                Undo::DirRemoved(path) => fs::create_dir(path),
                Undo::Renamed(from, to) => fs::rename(to, from),
            };
            if let Err(e) = result {
                eprintln!("notesd: rollback step failed: {e}");
            }
        }
    }

    /// Vault-relative path from user input. Rejects absolute paths, `..`, hidden names and
    /// anything reached through a symlink; returns the on-disk spelling (APFS ignores case).
    pub fn clean(&self, input: &str) -> R<String> {
        let input = input.trim().trim_end_matches('/');
        let usage = |m: String| {
            fail(
                ErrorCode::Usage,
                m,
                Some("use a path inside the vault, e.g. work/plan"),
            )
        };
        if input.is_empty() {
            return Err(usage("empty path".into()));
        }
        if Path::new(input).is_absolute() {
            return Err(usage(format!("{input}: absolute paths aren't allowed")));
        }
        let mut parts = Vec::new();
        for c in Path::new(input).components() {
            match c {
                Component::Normal(s) if !s.to_string_lossy().starts_with('.') => {
                    parts.push(s.to_string_lossy().into_owned())
                }
                Component::CurDir => {}
                _ => {
                    return Err(usage(format!(
                        "{input}: `..` and hidden names aren't allowed"
                    )));
                }
            }
        }
        if parts.is_empty() {
            return Err(usage("empty path".into()));
        }
        // Canonicalize the deepest existing ancestor: it must still be the same path inside the vault.
        let full = self.d.vault.join(parts.join("/"));
        let mut existing = full.as_path();
        while fs::symlink_metadata(existing).is_err() {
            existing = existing.parent().unwrap_or(&self.d.vault);
        }
        let canon = existing
            .canonicalize()
            .map_err(|_| usage(format!("{input} goes through a broken symlink")))?;
        let same =
            canon.to_string_lossy().to_lowercase() == existing.to_string_lossy().to_lowercase();
        if !same || !canon.starts_with(&self.d.vault) {
            return Err(usage(format!("{input} goes through a symlink")));
        }
        let rest = full.strip_prefix(existing).unwrap_or(Path::new(""));
        Ok(rel_string(&self.d.vault, &canon.join(rest)))
    }
}
