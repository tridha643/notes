//! The session folder: one JSON file per session plus `assets/`, the only home of
//! session data (docs/stage-plan.md). [`Store`] keeps every session in memory and
//! writes a session's whole file on each change: temp file, fsync, rename.
//!
//! Inside a daemon unit ([`Store::begin`]), every file write is journaled so
//! [`Store::rollback`] can restore the folder and reload memory; [`Store::commit`]
//! drops the journal. Outside a unit (tests, migration) writes aren't journaled.

use std::{
    cell::{Ref, RefCell, RefMut},
    collections::HashMap,
    fs::{self, File},
    io::{self, Write},
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::{Path, PathBuf},
};

use notes_proto::{posts::*, ErrorCode, Event};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::{error, stage_index::StageIndex, Result};

/// Version of the session file format; a change bumps it and adds a migration.
pub const SCHEMA: u32 = 1;
const TMP_SUFFIX: &str = ".notesd-tmp";
pub(crate) const ASSETS: &str = "assets";
/// Deleted sessions wait here as `<deleted_at>--<file name>`; the loader skips dot names.
const TRASH: &str = ".trash";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionFile {
    pub schema: u32,
    pub id: SessionId,
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The conversation that started it (see `PostNewArgs::owner`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    pub created_at: i64,
    pub last_active_at: i64,
    /// The agent's feedback cursor: comment seqs it has already seen.
    #[serde(default)]
    pub agent_seq: u64,
    #[serde(default)]
    pub posts: Vec<PostRec>,
    #[serde(default)]
    pub comments: Vec<Comment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trace: Vec<TraceStep>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostRec {
    pub id: PostId,
    pub title: String,
    pub version: u64,
    pub created_at: i64,
    pub updated_at: i64,
    pub parts: Vec<Part>,
    /// Every earlier version, oldest first. Never pruned.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<VersionRec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionRec {
    pub version: u64,
    pub title: String,
    pub at: i64,
    pub parts: Vec<Part>,
}

/// `assets/<sha256>.json`, next to the bytes in `assets/<sha256>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetMeta {
    pub schema: u32,
    pub id: AssetId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    pub kind: AssetKind,
    pub content_type: String,
    pub byte_length: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    pub created_at: i64,
}

/// (inode, size, mtime s, mtime ns): a file changed outside notesd when this does.
type Stamp = (u64, u64, i64, i64);

pub(crate) struct Entry {
    /// File name inside the folder; fixed at creation (a rename keeps it).
    pub file: String,
    stamp: Stamp,
    /// Creation order, the tie-break for equal timestamps.
    pub ord: u64,
    pub data: SessionFile,
}

#[derive(Default)]
pub(crate) struct Stage {
    pub sessions: HashMap<SessionId, Entry>,
    pub index: StageIndex,
    /// Files that didn't parse (or repeat a session id): `file: reason`. Never rewritten.
    pub skipped: Vec<String>,
    journal: Option<Vec<(PathBuf, Option<Vec<u8>>)>>,
    next_ord: u64,
}

/// SQLite (settings, render cache) plus the session folder.
pub struct Store {
    pub db: Connection,
    root: PathBuf,
    stage: RefCell<Stage>,
    pub(crate) migration: RefCell<String>,
    /// Why the folder couldn't be read, if it couldn't.
    problem: RefCell<Option<String>>,
}

fn fsync(file: &File) -> io::Result<()> {
    // ponytail: fsync, not F_FULLFSYNC, matching the vault's notes (crash-safe, not power-loss-safe).
    if unsafe { libc::fsync(file.as_raw_fd()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Temp file in the same directory, fsync, rename over the target, fsync the directory.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().expect("stage paths have a parent");
    let name = path.file_name().expect("stage paths have a name").to_string_lossy();
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

fn stamp(path: &Path) -> io::Result<Stamp> {
    let m = fs::metadata(path)?;
    Ok((m.ino(), m.len(), m.mtime(), m.mtime_nsec()))
}

fn io_error(what: &str, path: &Path, e: io::Error) -> crate::ErrorBody {
    let mut body = error(ErrorCode::Other, format!("{what} {}: {e}", path.display()));
    if e.kind() == io::ErrorKind::PermissionDenied {
        body.hint = Some(
            "macOS is blocking the session folder: allow notesd in System Settings → Privacy & Security → Files and Folders (Desktop)"
                .into(),
        );
    }
    body
}

/// `2026-10-25-fix-login-redirect-AbCdEfGhIjK.json`: local creation date, a slug of
/// the title (or agent), and the full id so names never collide on a case-insensitive disk.
pub(crate) fn file_name(created_at: i64, label: &str, id: &SessionId) -> String {
    let mut slug = String::new();
    for c in label.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
        if slug.len() >= 40 {
            break;
        }
    }
    let slug = match slug.trim_end_matches('-') {
        "" => "session",
        s => s,
    };
    format!("{}-{slug}-{id}.json", local_date(created_at))
}

fn local_date(secs: i64) -> String {
    let t = secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return "1970-01-01".into();
    }
    format!("{:04}-{:02}-{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday)
}

impl Stage {
    fn load(root: &Path) -> Result<Stage> {
        fs::create_dir_all(root.join(ASSETS)).map_err(|e| io_error("create", root, e))?;
        let mut stage = Stage::default();
        let mut found = Vec::new();
        let entries = fs::read_dir(root).map_err(|e| io_error("read", root, e))?;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                // A temp file left by a crashed write; the real file is intact.
                if name.ends_with(TMP_SUFFIX) {
                    let _ = fs::remove_file(entry.path());
                }
                continue;
            }
            if !name.ends_with(".json") || !entry.file_type().is_ok_and(|t| t.is_file()) {
                continue;
            }
            match Self::read_file(&entry.path()) {
                Ok((data, stamp)) => found.push((name, stamp, data)),
                Err(reason) => stage.skipped.push(format!("{name}: {reason}")),
            }
        }
        // Oldest first, so creation order (`ord`) survives restarts.
        found.sort_by(|a, b| (a.2.created_at, &a.0).cmp(&(b.2.created_at, &b.0)));
        for (file, stamp, data) in found {
            if stage.sessions.contains_key(&data.id) {
                stage.skipped.push(format!("{file}: repeats session {}", data.id));
                continue;
            }
            stage.insert(file, stamp, data);
        }
        Ok(stage)
    }

    fn read_file(path: &Path) -> std::result::Result<(SessionFile, Stamp), String> {
        let stamp = stamp(path).map_err(|e| e.to_string())?;
        let bytes = fs::read(path).map_err(|e| e.to_string())?;
        // Check the version first, so a newer file says so instead of failing on its fields.
        #[derive(Deserialize)]
        struct Head {
            schema: u32,
        }
        let head: Head = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if head.schema != SCHEMA {
            return Err(format!("unknown schema {} (this notesd reads {SCHEMA})", head.schema));
        }
        let data: SessionFile = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        Ok((data, stamp))
    }

    fn insert(&mut self, file: String, stamp: Stamp, data: SessionFile) {
        self.index.add_session(&data);
        self.next_ord += 1;
        let ord = self.next_ord;
        self.sessions.insert(data.id.clone(), Entry { file, stamp, ord, data });
    }

    /// Insert a new session or swap in a session's new contents, keeping its `ord`.
    fn replace(&mut self, file: String, stamp: Stamp, data: SessionFile) {
        match self.sessions.get_mut(&data.id) {
            Some(entry) => {
                self.index.remove_session(&entry.data);
                self.index.add_session(&data);
                entry.stamp = stamp;
                entry.data = data;
            }
            None => self.insert(file, stamp, data),
        }
    }

    fn remove(&mut self, id: &SessionId) -> Option<Entry> {
        let entry = self.sessions.remove(id)?;
        self.index.remove_session(&entry.data);
        Some(entry)
    }

    /// Write `bytes` at `path`, journaling the old contents when inside a unit.
    fn write(&mut self, path: &Path, bytes: &[u8]) -> Result<()> {
        self.remember(path)?;
        atomic_write(path, bytes).map_err(|e| io_error("write", path, e))
    }

    fn delete(&mut self, path: &Path) -> Result<()> {
        self.remember(path)?;
        match fs::remove_file(path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(io_error("delete", path, e)),
            _ => Ok(()),
        }
    }

    fn remember(&mut self, path: &Path) -> Result<()> {
        let Some(journal) = &mut self.journal else {
            return Ok(());
        };
        if journal.iter().any(|(p, _)| p == path) {
            return Ok(());
        }
        let old = match fs::read(path) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(io_error("read", path, e)),
        };
        journal.push((path.to_owned(), old));
        Ok(())
    }
}

impl Store {
    /// Open the folder at `root` (created if missing) next to `db`, which keeps
    /// settings and the render cache. Moves sessions out of a pre-JSON database once.
    pub fn open(db: Connection, root: PathBuf) -> Result<Store> {
        crate::migrate(&db)?;
        // An unreadable folder (macOS privacy, a file in the way) mustn't take the vault
        // down with it: serve no sessions, report it in `doctor`, and let writes fail.
        let (stage, problem) = match Stage::load(&root) {
            Ok(stage) => (stage, None),
            Err(e) => (Stage::default(), Some(e.message)),
        };
        let store = Store {
            db,
            root,
            stage: RefCell::new(stage),
            migration: RefCell::new("none needed".into()),
            problem: RefCell::new(problem),
        };
        if store.problem.borrow().is_none() {
            crate::migrate_json::run(&store);
        }
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn stage(&self) -> Ref<'_, Stage> {
        self.stage.borrow()
    }

    pub(crate) fn stage_mut(&self) -> RefMut<'_, Stage> {
        self.stage.borrow_mut()
    }

    /// Start journaling file writes for one daemon unit.
    pub fn begin(&self) {
        self.stage_mut().journal = Some(Vec::new());
    }

    /// The unit committed: keep the files.
    pub fn commit(&self) {
        self.stage_mut().journal = None;
    }

    /// The unit failed: put every journaled file back and reload the folder.
    pub fn rollback(&self) {
        let journal = self.stage_mut().journal.take().unwrap_or_default();
        for (path, old) in journal.into_iter().rev() {
            let result = match old {
                Some(bytes) => atomic_write(&path, &bytes),
                None => fs::remove_file(&path).or_else(|e| {
                    if e.kind() == io::ErrorKind::NotFound { Ok(()) } else { Err(e) }
                }),
            };
            if let Err(e) = result {
                eprintln!("notesd: rollback of {} failed: {e}", path.display());
            }
        }
        self.reload();
    }

    fn reload(&self) {
        match Stage::load(&self.root) {
            Ok(fresh) => {
                *self.stage_mut() = fresh;
                *self.problem.borrow_mut() = None;
            }
            Err(e) => {
                eprintln!("notesd: reloading {} failed: {}", self.root.display(), e.message);
                *self.problem.borrow_mut() = Some(e.message);
            }
        }
    }

    /// Write one session's file (creating it on first write) and remember its stamp.
    pub(crate) fn save(&self, data: SessionFile) -> Result<()> {
        let mut stage = self.stage_mut();
        let file = match stage.sessions.get(&data.id) {
            Some(e) => e.file.clone(),
            None => file_name(data.created_at, data.title.as_deref().unwrap_or(&data.agent), &data.id),
        };
        let path = self.root.join(&file);
        let mut bytes = serde_json::to_vec_pretty(&data).expect("session serializes");
        bytes.push(b'\n');
        stage.write(&path, &bytes)?;
        let stamp = stamp(&path).map_err(|e| io_error("stat", &path, e))?;
        stage.replace(file, stamp, data);
        Ok(())
    }

    /// Remove a session's file.
    pub(crate) fn remove_session(&self, id: &SessionId) -> Result<()> {
        let mut stage = self.stage_mut();
        let Some(file) = stage.sessions.get(id).map(|e| e.file.clone()) else {
            return Ok(());
        };
        stage.delete(&self.root.join(file))?;
        stage.remove(id);
        Ok(())
    }

    /// Move a session's file into the trash, stamped with the deletion time `at`.
    pub(crate) fn trash_session(&self, id: &SessionId, at: i64) -> Result<()> {
        let mut stage = self.stage_mut();
        let Some(e) = stage.sessions.get(id) else {
            return Ok(());
        };
        let from = self.root.join(&e.file);
        let to = self.root.join(TRASH).join(format!("{at}--{}", e.file));
        let mut bytes = serde_json::to_vec_pretty(&e.data).expect("session serializes");
        bytes.push(b'\n');
        fs::create_dir_all(self.root.join(TRASH)).map_err(|e| io_error("create", &to, e))?;
        stage.write(&to, &bytes)?;
        stage.delete(&from)?;
        stage.remove(id);
        Ok(())
    }

    /// Every trashed session as (path, deleted_at, session); files that don't parse are skipped.
    pub(crate) fn trashed(&self) -> Vec<(PathBuf, i64, SessionFile)> {
        let Ok(entries) = fs::read_dir(self.root.join(TRASH)) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let at = name.split_once("--")?.0.parse().ok()?;
                let (data, _) = Stage::read_file(&e.path()).ok()?;
                Some((e.path(), at, data))
            })
            .collect()
    }

    /// Put a trashed session back under its original file name.
    pub(crate) fn untrash_session(&self, trashed: &Path, data: SessionFile) -> Result<()> {
        let name = trashed.file_name().expect("trash paths have a name").to_string_lossy();
        let file = name.split_once("--").map_or(&*name, |(_, f)| f).to_owned();
        let mut bytes = serde_json::to_vec_pretty(&data).expect("session serializes");
        bytes.push(b'\n');
        let path = self.root.join(&file);
        let mut stage = self.stage_mut();
        stage.write(&path, &bytes)?;
        stage.delete(trashed)?;
        let stamp = stamp(&path).map_err(|e| io_error("stat", &path, e))?;
        stage.replace(file, stamp, data);
        Ok(())
    }

    /// Remove a trashed session's file for good.
    pub(crate) fn delete_trashed(&self, trashed: &Path) -> Result<()> {
        self.stage_mut().delete(trashed)
    }

    pub(crate) fn asset_path(&self, id: &str) -> PathBuf {
        self.root.join(ASSETS).join(id)
    }

    pub(crate) fn asset_exists(&self, id: &str) -> bool {
        // Ids are hex digests; anything else can't name a file of ours.
        id.bytes().all(|b| b.is_ascii_hexdigit()) && self.asset_path(id).is_file()
    }

    pub(crate) fn write_asset_file(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        self.stage_mut().write(path, bytes)
    }

    pub(crate) fn delete_asset_file(&self, path: &Path) -> Result<()> {
        self.stage_mut().delete(path)
    }

    /// Pick up files added, removed or replaced outside notesd (Finder, `cp`, `rm`).
    /// Our own writes already updated their stamps, so they produce nothing here.
    pub fn refresh(&self) -> Result<Vec<Event>> {
        if self.problem.borrow().is_some() {
            // The folder was unreadable at startup; try again now that it changed.
            self.reload();
            return Ok(Vec::new());
        }
        let mut on_disk: HashMap<String, Stamp> = HashMap::new();
        if let Ok(entries) = fs::read_dir(&self.root) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') || !name.ends_with(".json") {
                    continue;
                }
                if let Ok(s) = stamp(&entry.path()) {
                    on_disk.insert(name, s);
                }
            }
        }
        let mut events = Vec::new();
        let mut stage = self.stage_mut();
        // Rebuilt every pass, so a fixed or deleted file stops being reported.
        stage.skipped.clear();
        let known: Vec<(SessionId, String, Stamp)> = stage
            .sessions
            .values()
            .map(|e| (e.data.id.clone(), e.file.clone(), e.stamp))
            .collect();
        for (id, file, old) in &known {
            match on_disk.remove(file) {
                None => {
                    stage.remove(id);
                    events.push(crate::store::event("session-deleted", id, Some(id), None));
                }
                Some(now) if now != *old => match Stage::read_file(&self.root.join(file)) {
                    Ok((data, now)) if data.id == *id => {
                        stage.replace(file.clone(), now, data);
                        events.push(crate::store::event("session-updated", id, Some(id), None));
                    }
                    // Hand-edited into something we can't read: keep serving memory, and
                    // keep the old stamp so the next pass checks (and reports) it again.
                    Ok(_) | Err(_) => stage.skipped.push(format!(
                        "{file}: changed outside notesd and no longer reads as session {id}"
                    )),
                },
                Some(_) => {}
            }
        }
        for (file, _) in on_disk {
            match Stage::read_file(&self.root.join(&file)) {
                Ok((data, stamp)) if !stage.sessions.contains_key(&data.id) => {
                    let id = data.id.clone();
                    stage.insert(file, stamp, data);
                    events.push(crate::store::event("session-created", &id, Some(&id), None));
                }
                Ok((data, _)) => stage.skipped.push(format!("{file}: repeats session {}", data.id)),
                Err(reason) => stage.skipped.push(format!("{file}: {reason}")),
            }
        }
        Ok(events)
    }

    /// `{path, sessions, skipped, migration}` for `doctor`.
    pub fn doctor(&self) -> serde_json::Value {
        let stage = self.stage();
        serde_json::json!({
            "path": self.root,
            "sessions": stage.sessions.len(),
            "skipped": stage.skipped,
            "migration": *self.migration.borrow(),
            "error": *self.problem.borrow(),
        })
    }
}
