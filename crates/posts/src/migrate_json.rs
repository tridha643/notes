//! One-time move of sessions from the pre-JSON SQLite tables into the folder.
//! Export, re-read every written file and compare, and only then drop the old
//! tables. Any failure leaves the tables in place (the next start retries) and
//! is reported by `doctor`.

use std::fs;

use crate::{
    json_store::{atomic_write, file_name, SessionFile, Store},
    sqlite_store::{export_assets, export_sessions, has_legacy_tables, LEGACY_TABLES},
    Db, Result,
};

pub(crate) fn run(store: &Store) {
    let outcome = match has_legacy_tables(&store.db) {
        Ok(false) => return,
        Ok(true) => migrate(store),
        Err(e) => Err(e),
    };
    let status = match outcome {
        Ok(n) => format!("moved {n} sessions from SQLite"),
        Err(e) => {
            eprintln!("notesd: session migration failed: {}", e.message);
            format!("failed, SQLite tables kept: {}", e.message)
        }
    };
    *store.migration.borrow_mut() = status;
}

fn migrate(store: &Store) -> Result<usize> {
    let sessions = export_sessions(&store.db)?;
    let assets = export_assets(&store.db)?;
    for (meta, bytes) in &assets {
        let path = store.asset_path(&meta.id);
        if !path.exists() {
            write(&path, bytes)?;
        }
        let meta_path = store.asset_path(&format!("{}.json", meta.id));
        if !meta_path.exists() {
            write(&meta_path, &serde_json::to_vec_pretty(meta).expect("meta serializes"))?;
        }
    }
    let mut written = Vec::new();
    for session in &sessions {
        // A file from an earlier, interrupted migration is kept as is.
        if store.stage().sessions.contains_key(&session.id) {
            continue;
        }
        let label = session.title.as_deref().unwrap_or(&session.agent);
        let path = store.root().join(file_name(session.created_at, label, &session.id));
        let mut bytes = serde_json::to_vec_pretty(session).expect("session serializes");
        bytes.push(b'\n');
        write(&path, &bytes)?;
        written.push((path, session));
    }
    // Verify: every file reads back to exactly what was exported.
    for (path, session) in &written {
        let bytes = fs::read(path).map_err(|e| crate::other(format!("re-read {}: {e}", path.display())))?;
        let back: SessionFile = serde_json::from_slice(&bytes)
            .map_err(|e| crate::other(format!("re-parse {}: {e}", path.display())))?;
        if back != **session {
            return Err(crate::other(format!("{} doesn't match its SQLite rows", path.display())));
        }
    }
    store.refresh()?;
    let missing: Vec<_> = sessions
        .iter()
        .filter(|s| !store.stage().sessions.contains_key(&s.id))
        .map(|s| s.id.to_string())
        .collect();
    if !missing.is_empty() {
        return Err(crate::other(format!("sessions missing after export: {}", missing.join(", "))));
    }
    let drops: String = LEGACY_TABLES.iter().map(|t| format!("DROP TABLE IF EXISTS {t};")).collect();
    store.db.execute_batch(&format!("BEGIN; {drops} COMMIT;")).db()?;
    Ok(sessions.len())
}

fn write(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    atomic_write(path, bytes).map_err(|e| crate::other(format!("write {}: {e}", path.display())))
}
