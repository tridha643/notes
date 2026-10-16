//! SQLite note storage shared by the CLI and the native app.

mod note_store;

pub use note_store::{NoteId, NoteRecord, NoteStore, NoteStoreError, NoteSummary};
