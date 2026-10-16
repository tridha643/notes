use std::{fmt, fs, path::Path, str::FromStr, time::Duration};

use rusqlite::{params, Connection, OptionalExtension};
use thiserror::Error;

/// A note ID is a database-local SQLite integer primary key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NoteId(pub i64);

impl fmt::Display for NoteId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for NoteId {
    type Err = std::num::ParseIntError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value.parse().map(Self)
    }
}

/// A full note body with creation and update times in UTC Unix seconds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NoteRecord {
    pub id: NoteId,
    pub title: String,
    pub body: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Note list metadata excludes bodies so sidebar reads stay small.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NoteSummary {
    pub id: NoteId,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Store failures distinguish missing notes from filesystem and SQLite errors.
#[derive(Debug, Error)]
pub enum NoteStoreError {
    #[error("Note not found: {0}")]
    NoteNotFound(NoteId),
    #[error("Note database directory error: {0}")]
    Directory(#[from] std::io::Error),
    #[error("Note database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("Note database path must not be empty")]
    EmptyPath,
    #[error("Note database requires WAL journal mode; SQLite selected {0}")]
    WalUnavailable(String),
}

/// A synchronous SQLite connection; callers must keep disk work off the UI thread.
pub struct NoteStore {
    connection: Connection,
}

impl NoteStore {
    /// Opens the note database, creating parent directories and initializing its schema.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, NoteStoreError> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            return Err(NoteStoreError::EmptyPath);
        }
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        let journal_mode: String =
            connection.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
        if !journal_mode.eq_ignore_ascii_case("wal") {
            return Err(NoteStoreError::WalUnavailable(journal_mode));
        }
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS notes (
                id INTEGER PRIMARY KEY,
                title TEXT NOT NULL,
                body TEXT NOT NULL DEFAULT '',
                created_at INTEGER NOT NULL DEFAULT (unixepoch()),
                updated_at INTEGER NOT NULL DEFAULT (unixepoch())
            );
            CREATE INDEX IF NOT EXISTS notes_updated_at_idx ON notes(updated_at DESC, id DESC);",
        )?;
        Ok(Self { connection })
    }

    /// Creates a note without altering its title or body, including empty strings.
    pub fn create_note(&self, title: &str, body: &str) -> Result<NoteRecord, NoteStoreError> {
        Ok(self.connection.query_row(
            "INSERT INTO notes (title, body) VALUES (?1, ?2)
             RETURNING id, title, body, created_at, updated_at",
            params![title, body],
            read_note_record,
        )?)
    }

    /// Lists note metadata by most recently updated, breaking timestamp ties by newest ID.
    pub fn list_notes(&self) -> Result<Vec<NoteSummary>, NoteStoreError> {
        let mut statement = self.connection.prepare(
            "SELECT id, title, created_at, updated_at FROM notes
             ORDER BY updated_at DESC, id DESC",
        )?;
        let notes = statement.query_map([], |row| {
            Ok(NoteSummary {
                id: NoteId(row.get(0)?),
                title: row.get(1)?,
                created_at: row.get(2)?,
                updated_at: row.get(3)?,
            })
        })?;
        Ok(notes.collect::<Result<Vec<_>, _>>()?)
    }

    /// Reads a complete note, returning an explicit error for an unknown ID.
    pub fn get_note(&self, id: NoteId) -> Result<NoteRecord, NoteStoreError> {
        self.connection
            .query_row(
                "SELECT id, title, body, created_at, updated_at FROM notes WHERE id = ?1",
                [id.0],
                read_note_record,
            )
            .optional()?
            .ok_or(NoteStoreError::NoteNotFound(id))
    }

    /// Atomically appends literal text with no separator, avoiding lost concurrent appends.
    pub fn append_note(&self, id: NoteId, text: &str) -> Result<NoteRecord, NoteStoreError> {
        self.connection
            .query_row(
                "UPDATE notes SET body = body || ?1, updated_at = max(updated_at, unixepoch())
                 WHERE id = ?2 RETURNING id, title, body, created_at, updated_at",
                params![text, id.0],
                read_note_record,
            )
            .optional()?
            .ok_or(NoteStoreError::NoteNotFound(id))
    }
}

fn read_note_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<NoteRecord> {
    Ok(NoteRecord {
        id: NoteId(row.get(0)?),
        title: row.get(1)?,
        body: row.get(2)?,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initializes_nested_database_with_wal_and_normal_sync() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested/notes.db");
        let store = NoteStore::open(&path).unwrap();
        let journal: String = store
            .connection
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        let synchronous: i64 = store
            .connection
            .pragma_query_value(None, "synchronous", |row| row.get(0))
            .unwrap();
        assert_eq!(journal, "wal");
        assert_eq!(synchronous, 1);
        assert!(store.list_notes().unwrap().is_empty());
        drop(store);
        assert!(NoteStore::open(path)
            .unwrap()
            .list_notes()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn round_trips_unicode_quotes_and_newlines_after_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("notes.db");
        let store = NoteStore::open(&path).unwrap();
        let note = store
            .create_note("Today's 🦀 notes", "你好\n'); DROP TABLE notes; --")
            .unwrap();
        assert!(note.id.0 > 0);
        assert!(note.created_at > 0);
        assert_eq!(note.created_at, note.updated_at);
        drop(store);
        let store = NoteStore::open(path).unwrap();
        assert_eq!(store.get_note(note.id).unwrap(), note);
        assert_eq!(store.create_note("", "").unwrap().body, "");
    }

    #[test]
    fn append_preserves_content_and_creation_time_and_updates_recency() {
        let directory = tempfile::tempdir().unwrap();
        let store = NoteStore::open(directory.path().join("notes.db")).unwrap();
        let first = store.create_note("First", "Start").unwrap();
        let second = store.create_note("Second", "").unwrap();
        store
            .connection
            .execute("UPDATE notes SET updated_at = 1", [])
            .unwrap();
        assert_eq!(store.list_notes().unwrap()[0].id, second.id);
        let appended = store.append_note(first.id, "\n追加 🦀").unwrap();
        assert_eq!(appended.body, "Start\n追加 🦀");
        assert_eq!(appended.title, first.title);
        assert_eq!(appended.created_at, first.created_at);
        assert!(appended.updated_at > 1);
        assert_eq!(store.list_notes().unwrap()[0].id, first.id);
    }

    #[test]
    fn unknown_ids_fail_without_creating_notes() {
        let directory = tempfile::tempdir().unwrap();
        let store = NoteStore::open(directory.path().join("notes.db")).unwrap();
        for result in [
            store.get_note(NoteId(42)),
            store.append_note(NoteId(42), "text"),
        ] {
            assert!(matches!(
                result,
                Err(NoteStoreError::NoteNotFound(NoteId(42)))
            ));
        }
        assert!(store.list_notes().unwrap().is_empty());
    }

    #[test]
    fn concurrent_connections_do_not_lose_appends() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("notes.db");
        let store = NoteStore::open(&path).unwrap();
        let id = store.create_note("Concurrent", "").unwrap().id;
        let writers: Vec<_> = (0..4)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let store = NoteStore::open(path).unwrap();
                    for _ in 0..20 {
                        store.append_note(id, "🦀").unwrap();
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
        assert_eq!(store.get_note(id).unwrap().body, "🦀".repeat(80));
    }

    #[test]
    fn rejects_empty_paths_and_reports_filesystem_errors() {
        assert!(matches!(
            NoteStore::open(""),
            Err(NoteStoreError::EmptyPath)
        ));
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("file");
        fs::write(&file, "not a directory").unwrap();
        assert!(matches!(
            NoteStore::open(file.join("notes.db")),
            Err(NoteStoreError::Directory(_))
        ));
    }

    #[test]
    fn rejects_databases_that_cannot_enable_wal() {
        assert!(matches!(
            NoteStore::open(":memory:"),
            Err(NoteStoreError::WalUnavailable(mode)) if mode == "memory"
        ));
    }

    #[test]
    fn simultaneous_first_opens_initialize_a_single_database() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested/notes.db");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let writers: Vec<_> = (0..4)
            .map(|_| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    let store = NoteStore::open(path).unwrap();
                    store.create_note("Concurrent first open", "").unwrap();
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
        assert_eq!(
            NoteStore::open(path).unwrap().list_notes().unwrap().len(),
            4
        );
    }
}
