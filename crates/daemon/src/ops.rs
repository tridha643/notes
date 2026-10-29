//! One handler per verb. Each runs inside a [`Tx`], so a failure undoes its file and SQLite changes.

use std::{fs, path::Path};

use notes_proto::{
    DiffResult, DoctorResult, ErrorBody, ErrorCode, HistoryEntry, ListArgs, ListEntry, ListSort,
    Note, NoteSummary, PROTOCOL_VERSION, Verb, WriteResult,
};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

use crate::{
    Fail, R, fail,
    store::{Daemon, Row, Tx, rel_string, title_of, walk_notes},
    text,
};

fn value(v: impl serde::Serialize) -> R<Value> {
    Ok(serde_json::to_value(v).expect("results serialize"))
}

fn written(row: &Row) -> R<Value> {
    value(WriteResult {
        path: row.path.clone().unwrap_or_default(),
        version: row.version,
    })
}

fn ambiguous(what: &str, candidates: &[String]) -> Fail {
    let mut e = text::error(
        ErrorCode::Ambiguous,
        format!(
            "{what:?} matches {} notes: {}",
            candidates.len(),
            candidates.join(", ")
        ),
        Some("use the folder path, e.g. work/plan"),
    );
    e.details = Some(json!({ "candidates": candidates }));
    Fail(e)
}

fn check_version(row: &Row, expected: Option<u64>) -> R<()> {
    match expected {
        Some(v) if v != row.version => Err(Fail(ErrorBody::version_conflict(
            row.path.as_deref().unwrap_or(""),
            v,
            row.version,
        ))),
        _ => Ok(()),
    }
}

impl Tx<'_> {
    pub fn op(&mut self, verb: &Verb) -> R<Value> {
        match verb {
            Verb::List(a) => value(self.list(a)?),
            Verb::Find(a) => value(self.d.search.find(a)?),
            Verb::Search(a) => {
                let folder = a
                    .folder
                    .as_deref()
                    .filter(|f| !f.trim().is_empty())
                    .map(|f| self.clean(f))
                    .transpose()?;
                value(self.d.search.search(a, folder.as_deref())?)
            }
            Verb::Read(a) => {
                let (row, body) = self.resolve(&a.note)?;
                let rel = row.path.clone().unwrap_or_default();
                self.d.search.opened(self.abs(&rel));
                let line_count = text::line_count(&body);
                let body = match a.lines {
                    Some(l) => text::slice_lines(&body, l.start, l.end)?,
                    None => body,
                };
                let (title, version, created_at, updated_at) =
                    (row.title, row.version, row.created_at, row.updated_at);
                value(Note {
                    path: rel,
                    title,
                    version,
                    body,
                    line_count,
                    created_at,
                    updated_at,
                })
            }
            Verb::New(a) => {
                let rel = self.note_path(&a.path)?;
                if self.sync(&rel)?.is_some() || self.abs(&rel).exists() {
                    return Err(fail(
                        ErrorCode::Usage,
                        format!("{rel} already exists"),
                        Some("use write, edit or append"),
                    ));
                }
                self.mkdir_parent(&rel)?;
                written(&self.save(None, &rel, &a.body, "note-created")?)
            }
            Verb::Edit(a) => {
                let (row, body) = self.resolve(&a.note)?;
                check_version(&row, a.if_version)?;
                let rel = row.path.clone().unwrap_or_default();
                let new = text::apply_edits(&rel, &body, &a.edits)?;
                written(&self.save(Some(&row), &rel, &new, "note-updated")?)
            }
            Verb::Append(a) => {
                let (row, body) = self.resolve(&a.note)?;
                check_version(&row, a.if_version)?;
                let new = text::append(&body, &a.text, a.under.as_deref())?;
                written(&self.save(
                    Some(&row),
                    row.path.as_deref().unwrap_or(""),
                    &new,
                    "note-updated",
                )?)
            }
            Verb::Prepend(a) => {
                let (row, body) = self.resolve(&a.note)?;
                check_version(&row, a.if_version)?;
                written(&self.save(
                    Some(&row),
                    row.path.as_deref().unwrap_or(""),
                    &text::prepend(&body, &a.text),
                    "note-updated",
                )?)
            }
            Verb::Write(a) => {
                if a.if_version.is_none() && !a.force {
                    return Err(fail(
                        ErrorCode::Usage,
                        "write replaces the whole note",
                        Some("pass --if-version N (from `notes read`) or --force"),
                    ));
                }
                let (row, _) = self.resolve(&a.note)?;
                check_version(&row, a.if_version)?;
                written(&self.save(
                    Some(&row),
                    row.path.as_deref().unwrap_or(""),
                    &a.body,
                    "note-updated",
                )?)
            }
            Verb::Move(a) => self.move_note(&a.note, &a.to),
            Verb::Mkdir(a) => {
                let rel = self.clean(&a.folder)?;
                let abs = self.abs(&rel);
                if abs.exists() && !abs.is_dir() {
                    return Err(fail(
                        ErrorCode::Usage,
                        format!("{rel} exists and isn't a folder"),
                        None,
                    ));
                }
                self.mkdirs(&rel)?;
                Ok(json!(format!("{rel}/")))
            }
            Verb::Rmdir(a) => {
                let rel = self.clean(&a.folder)?;
                let abs = self.abs(&rel);
                if !abs.is_dir() {
                    return Err(fail(
                        ErrorCode::NotFound,
                        format!("no folder {rel}"),
                        Some("run `notes list`"),
                    ));
                }
                let left = fs::read_dir(&abs)?
                    .flatten()
                    .filter(|e| e.file_name() != ".DS_Store")
                    .count();
                if left > 0 {
                    return Err(fail(
                        ErrorCode::Usage,
                        format!("{rel} isn't empty ({left} entries)"),
                        Some("move or delete what's inside first"),
                    ));
                }
                let _ = fs::remove_file(abs.join(".DS_Store"));
                self.rmdir(&abs)?;
                self.event("folder-removed", &rel, None)?;
                Ok(json!(format!("{rel}/")))
            }
            Verb::Delete(a) => {
                let (row, body) = self.resolve(&a.note)?;
                let rel = row.path.clone().unwrap_or_default();
                self.remove_file(&self.abs(&rel))?;
                if a.purge {
                    self.db
                        .execute("DELETE FROM history WHERE note_id = ?1", [row.id])?;
                    self.db
                        .execute("DELETE FROM notes WHERE id = ?1", [row.id])?;
                    self.event("note-deleted", &rel, Some(row.version))?;
                } else {
                    self.trash_note(&row, &rel, &body)?;
                }
                self.purge_trash()?;
                value(WriteResult {
                    path: rel,
                    version: row.version,
                })
            }
            Verb::Restore(a) => self.restore(&a.note),
            Verb::History(a) => {
                let (row, _) = self.resolve(&a.note)?;
                let limit = a.limit.map_or(-1, i64::from);
                let mut stmt = self.db.prepare_cached("SELECT version, at, author FROM history WHERE note_id = ?1 ORDER BY version DESC LIMIT ?2")?;
                let rows = stmt.query_map(params![row.id, limit], |r| {
                    Ok(HistoryEntry {
                        version: r.get::<_, i64>(0)? as u64,
                        at: r.get(1)?,
                        author: r.get(2)?,
                    })
                })?;
                value(rows.collect::<Result<Vec<_>, _>>()?)
            }
            Verb::Diff(a) => {
                let (row, _) = self.resolve(&a.note)?;
                let rel = row.path.clone().unwrap_or_default();
                let to = a.to.unwrap_or(row.version);
                let from = a.from.unwrap_or(to.saturating_sub(1));
                let (old, new) = (self.body_at(&row, from)?, self.body_at(&row, to)?);
                let unified = similar::TextDiff::from_lines(&old, &new)
                    .unified_diff()
                    .header(&format!("{rel}@v{from}"), &format!("{rel}@v{to}"))
                    .to_string();
                value(DiffResult {
                    path: rel,
                    from,
                    to,
                    unified,
                })
            }
            Verb::Revert(a) => {
                let (row, _) = self.resolve(&a.note)?;
                if a.to == 0 {
                    return Err(fail(
                        ErrorCode::Usage,
                        "versions start at 1",
                        Some("see `notes history`"),
                    ));
                }
                let body = self.body_at(&row, a.to)?;
                written(&self.save(
                    Some(&row),
                    row.path.as_deref().unwrap_or(""),
                    &body,
                    "note-updated",
                )?)
            }
            Verb::Doctor => value(DoctorResult {
                protocol: PROTOCOL_VERSION,
                daemon_version: env!("CARGO_PKG_VERSION").into(),
                socket: self.d.socket.clone(),
                vault: self.d.vault.clone(),
                database: self.d.db_path.clone(),
                notes: Daemon::note_count(self.db)?,
                render_cache: Some(
                    serde_json::to_value(self.d.render.stats()).expect("stats serialize"),
                ),
                seq: Some(self.d.latest_seq()),
                stage: Some(self.store.doctor()),
            }),
            Verb::Manifest => Ok(manifest()),
            Verb::Watch(_) | Verb::Wait(_) => Err(fail(
                ErrorCode::Usage,
                "watch can't run inside a batch",
                Some("open its own connection"),
            )),
            Verb::Batch(_) => Err(fail(ErrorCode::Usage, "batches can't nest", None)),
            sideshow => self.sideshow(sideshow),
        }
    }

    /// Clean a note path and add `.md` before canonicalizing, so case matches the file on disk.
    /// Sideshow verbs: sessions, posts, comments, assets, traces, kits, settings.
    fn sideshow(&mut self, verb: &Verb) -> R<Value> {
        use notes_posts as p;
        let db = self.store;
        let agent = Some(self.author).filter(|a| *a != "human");
        match verb {
            Verb::SessionNew(a) => self.written_posts(p::session_new(db, a)),
            Verb::SessionList => value(p::session_list(db).map_err(Fail)?),
            Verb::SessionSearch(a) => value(p::session_search(db, a).map_err(Fail)?),
            Verb::SessionRename(a) => self.written_posts(p::session_rename(db, a)),
            Verb::SessionDelete(a) => self.written_posts(p::session_delete(db, a)),
            Verb::SessionRestore(a) => self.written_posts(p::session_restore(db, &a.id)),
            Verb::PostNew(a) => {
                let cwd = self.cwd.clone();
                self.written_posts(p::post_new(db, a, agent, cwd.as_deref()))
            }
            Verb::PostUpdate(a) => self.written_posts(p::post_update(db, a)),
            Verb::PostPart(a) => self.written_posts(p::post_part(db, a)),
            Verb::PostList(a) => value(p::post_list(db, a).map_err(Fail)?),
            Verb::PostShow(a) => value(p::post_show(db, a).map_err(Fail)?),
            // ponytail: re-parses the cached JSON into a Value; splice the raw string into the
            // response line if big renders show up in profiles.
            Verb::PostRender(a) => Ok(serde_json::from_str(
                &p::post_render_json(db, &self.d.render, a).map_err(Fail)?,
            )
            .expect("render JSON parses")),
            Verb::PostDelete(a) => self.posts_events(p::post_delete(db, &a.id)),
            Verb::CommentNew(a) => {
                let r = self.written_posts(p::comment_new(db, a, agent))?;
                self.commented = true;
                Ok(r)
            }
            Verb::CommentList(a) => value(p::comment_list(db, a).map_err(Fail)?),
            Verb::CommentDelete(a) => self.posts_events(p::comment_delete(db, &a.id)),
            Verb::AssetPut(a) => value(p::asset_put(db, a).map_err(Fail)?),
            Verb::AssetGet(a) => value(p::asset_get(db, &a.id).map_err(Fail)?),
            Verb::TraceSet(a) => self.written_posts(p::trace_set(db, a)),
            Verb::TraceList(a) => value(p::trace_list(db, &a.id).map_err(Fail)?),
            Verb::Kits => value(p::kits()),
            Verb::SettingGet(a) => value(p::setting_get(db, &a.key).map_err(Fail)?),
            Verb::SettingSet(a) => self.posts_events(p::setting_set(db, a)),
            other => Err(fail(
                ErrorCode::Usage,
                format!("{other:?} isn't handled here"),
                None,
            )),
        }
    }

    fn written_posts<T: serde::Serialize>(
        &mut self,
        r: notes_posts::Result<notes_posts::Written<T>>,
    ) -> R<Value> {
        let (v, events) = r.map_err(Fail)?;
        self.post_events(events)?;
        value(v)
    }

    fn posts_events(&mut self, r: notes_posts::Result<Vec<notes_proto::Event>>) -> R<Value> {
        self.post_events(r.map_err(Fail)?)?;
        Ok(Value::Null)
    }

    fn note_path(&self, input: &str) -> R<String> {
        let input = input.trim().trim_end_matches('/');
        let rel = self.clean(&if input.ends_with(".md") {
            input.to_string()
        } else {
            format!("{input}.md")
        })?;
        if title_of(&rel).trim().is_empty() {
            return Err(fail(ErrorCode::Usage, format!("{rel}: empty title"), None));
        }
        Ok(rel)
    }

    /// Note ref → current row and body: a path (with or without `.md`), a unique bare title, or `@last`.
    fn resolve(&mut self, note: &str) -> R<(Row, String)> {
        let rel = if note == "@last" {
            let last: Option<String> = self
                .db
                .query_row(
                    "SELECT n.path FROM history h JOIN notes n ON n.id = h.note_id WHERE n.path IS NOT NULL ORDER BY h.rowid DESC LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            last.ok_or_else(|| {
                fail(
                    ErrorCode::NotFound,
                    "no notes yet",
                    Some("create one with `notes new`"),
                )
            })?
        } else if note.contains('/') || note.ends_with(".md") {
            self.note_path(note)?
        } else {
            let mut paths = self.paths_titled(note.trim())?;
            match paths.len() {
                0 => self.note_path(note)?,
                1 => paths.remove(0),
                _ => return Err(ambiguous(note, &paths)),
            }
        };
        self.sync(&rel)?.ok_or_else(|| {
            fail(
                ErrorCode::NotFound,
                format!("no note {rel}"),
                Some("run `notes list` or `notes find`"),
            )
        })
    }

    fn body_at(&self, row: &Row, version: u64) -> R<String> {
        if version == 0 {
            return Ok(String::new());
        }
        let body: Option<String> = self
            .db
            .query_row(
                "SELECT body FROM history WHERE note_id = ?1 AND version = ?2",
                params![row.id, version as i64],
                |r| r.get(0),
            )
            .optional()?;
        let path = row.path.as_deref().unwrap_or("");
        body.ok_or_else(|| {
            fail(
                ErrorCode::NotFound,
                format!("{path} has no version {version}"),
                Some("see `notes history`"),
            )
        })
    }

    fn mkdir_parent(&mut self, rel: &str) -> R<()> {
        match Path::new(rel).parent() {
            Some(p) if !p.as_os_str().is_empty() => self.mkdirs(&p.to_string_lossy()),
            _ => Ok(()),
        }
    }

    fn list(&mut self, a: &ListArgs) -> R<Vec<ListEntry>> {
        let base = match a.folder.as_deref().map(str::trim) {
            Some(f) if !matches!(f, "" | "." | "/") => self.clean(f)?,
            _ => String::new(),
        };
        let dir = self.abs(&base);
        if !dir.is_dir() {
            return Err(fail(
                ErrorCode::NotFound,
                format!("no folder {base}"),
                Some("run `notes list`"),
            ));
        }
        let (mut folders, mut rels) = (Vec::new(), Vec::new());
        let mut stack = vec![dir];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir)?.flatten() {
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue;
                }
                if kind.is_dir() {
                    folders.push(rel_string(&self.d.vault, &entry.path()));
                    if a.recursive {
                        stack.push(entry.path());
                    }
                } else if kind.is_file() && name.ends_with(".md") {
                    rels.push(rel_string(&self.d.vault, &entry.path()));
                }
            }
        }
        let mut notes = Vec::new();
        for rel in rels {
            // Trust SQLite for known notes (the watcher keeps it current); sync only unseen files.
            let row = match self.row(&rel)? {
                Some(row) => Some(row),
                None => self.sync(&rel).ok().flatten().map(|(row, _)| row),
            };
            notes.extend(row);
        }
        match a.sort {
            ListSort::Updated => notes.sort_by(|x, y| {
                y.updated_at
                    .cmp(&x.updated_at)
                    .then_with(|| x.path.cmp(&y.path))
            }),
            ListSort::Created => notes.sort_by(|x, y| {
                y.created_at
                    .cmp(&x.created_at)
                    .then_with(|| x.path.cmp(&y.path))
            }),
            ListSort::Title => notes.sort_by(|x, y| {
                x.title
                    .to_lowercase()
                    .cmp(&y.title.to_lowercase())
                    .then_with(|| x.path.cmp(&y.path))
            }),
        }
        if let Some(limit) = a.limit {
            notes.truncate(limit as usize);
        }
        folders.sort();
        let folders = folders
            .into_iter()
            .map(|folder| ListEntry::Folder { folder });
        let notes = notes.into_iter().map(|r| {
            ListEntry::Note(NoteSummary {
                path: r.path.unwrap_or_default(),
                title: r.title,
                version: r.version,
                updated_at: r.updated_at,
            })
        });
        Ok(folders.chain(notes).collect())
    }

    /// Rename or move into a folder, then point `[[Old]]` / `[[old/path]]` links at the new spot.
    fn move_note(&mut self, note: &str, to: &str) -> R<Value> {
        let (row, _) = self.resolve(note)?;
        let src = row.path.clone().unwrap_or_default();
        let target = self.clean(to)?;
        let dest = if self.abs(&target).is_dir() {
            self.clean(&format!(
                "{target}/{}",
                Path::new(&src)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            ))?
        } else {
            self.note_path(to)?
        };
        if dest == src {
            return written(&row);
        }
        if self.abs(&dest).exists() {
            return Err(fail(
                ErrorCode::Usage,
                format!("{dest} already exists"),
                Some("pick another name, or delete it first"),
            ));
        }
        let old_title_unique = self.paths_titled(&row.title)?.len() == 1;
        self.mkdir_parent(&dest)?;
        self.rename(&self.abs(&src), &self.abs(&dest))?;
        let new_title = title_of(&dest);
        self.db.execute(
            "UPDATE notes SET path = ?1, title = ?2 WHERE id = ?3",
            params![dest, new_title, row.id],
        )?;
        self.event("note-moved", &dest, Some(row.version))?;

        let new_target = if self.paths_titled(&new_title)?.len() == 1 {
            new_title
        } else {
            dest.trim_end_matches(".md").to_string()
        };
        let mut olds = vec![src.trim_end_matches(".md").to_string()];
        if old_title_unique {
            olds.push(row.title.clone());
        }
        let olds: Vec<&str> = olds.iter().map(String::as_str).collect();
        let mut all = Vec::new();
        walk_notes(&self.d.vault, &self.d.vault, &mut all, false);
        let needles: Vec<String> = olds.iter().map(|old| format!("[[{old}")).collect();
        for rel in all {
            // ponytail: reads every note (~21 ms at 1k notes); ask fff grep for `[[old` if vaults grow.
            // Only notes that may link here get synced and hashed.
            let Ok(raw) = fs::read_to_string(self.abs(&rel)) else {
                continue; // unreadable (e.g. not UTF-8): its links stay, the move still lands
            };
            if !needles.iter().any(|n| raw.contains(n.as_str())) {
                continue;
            }
            if let Ok(Some((r, body))) = self.sync(&rel)
                && let Some(new_body) = text::rewrite_links(&body, &olds, &new_target)
            {
                self.save(Some(&r), &rel, &new_body, "note-updated")?;
            }
        }
        written(&self.row_by_id(row.id)?)
    }

    fn restore(&mut self, note: &str) -> R<Value> {
        let by_path = note.contains('/') || note.ends_with(".md");
        let key = if by_path {
            self.note_path(note)?
        } else {
            note.trim().to_string()
        };
        let sql = if by_path {
            "SELECT t.id, t.note_id, t.path, t.body FROM trash t WHERE t.path = ?1 ORDER BY t.deleted_at DESC, t.id DESC"
        } else {
            "SELECT t.id, t.note_id, t.path, t.body FROM trash t JOIN notes n ON n.id = t.note_id
             WHERE n.title = ?1 COLLATE NOCASE ORDER BY t.deleted_at DESC, t.id DESC"
        };
        let mut stmt = self.db.prepare_cached(sql)?;
        let found: Vec<(i64, i64, String, String)> = stmt
            .query_map([&key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<Result<_, _>>()?;
        drop(stmt);
        let mut paths: Vec<String> = found.iter().map(|f| f.2.clone()).collect();
        paths.sort();
        paths.dedup();
        if paths.len() > 1 {
            return Err(ambiguous(note, &paths));
        }
        let Some((trash_id, note_id, rel, body)) = found.into_iter().next() else {
            return Err(fail(
                ErrorCode::NotFound,
                format!("{note} isn't in the trash"),
                Some("trash keeps notes for 30 days"),
            ));
        };
        if self.sync(&rel)?.is_some() || self.abs(&rel).exists() {
            return Err(fail(
                ErrorCode::Usage,
                format!("{rel} exists again"),
                Some("move that note away, then restore"),
            ));
        }
        self.mkdir_parent(&rel)?;
        let prev = self.row_by_id(note_id)?;
        let row = self.save(Some(&prev), &rel, &body, "note-restored")?;
        self.db
            .execute("DELETE FROM trash WHERE id = ?1", [trash_id])?;
        written(&row)
    }
}

pub fn manifest() -> Value {
    let note = "note ref: path with or without .md, unique bare title, or @last";
    let write = json!({ "path": "string", "version": "u64" });
    json!({
        "protocol": PROTOCOL_VERSION,
        "daemon_version": env!("CARGO_PKG_VERSION"),
        "transport": "unix socket; one JSON object per line each way",
        "request": { "v": "u32 (1)", "id": "u64", "verb": "string", "args": "object", "cwd": "path?", "agent": "string? (history author, default human)" },
        "responses": ["{id, ok:true, result}", "{id, ok:false, error:{code, message, hint?, details?}}", "{id, event:{seq, kind, path?, version?}} (watch)"],
        "error_codes": { "other": 1, "usage": 2, "not_found": 3, "ambiguous": 4, "version_conflict": 5, "daemon_unreachable": 6 },
        "event_kinds": ["note-created", "note-updated", "note-deleted", "note-restored", "note-moved", "folder-created", "folder-removed"],
        "verbs": [
            { "verb": "list", "args": { "folder": "string?", "recursive": "bool", "sort": "updated|created|title", "limit": "u32? (notes only)" }, "result": "[{folder} | {path, title, version, updated_at}], folders first" },
            { "verb": "find", "args": { "query": "string", "limit": "u32? (50)" }, "result": "[{path, score}]" },
            { "verb": "search", "args": { "query": "string", "folder": "string?", "context": "u32", "case_sensitive": "bool", "fuzzy": "bool", "limit": "u32? (50)" }, "result": "[{path, line, text, before?, after?}]" },
            { "verb": "read", "args": { "note": note, "lines": "{start, end}? 1-based inclusive" }, "result": "{path, title, version, body, line_count, created_at, updated_at}" },
            { "verb": "new", "args": { "path": "string", "body": "string" }, "result": write },
            { "verb": "edit", "args": { "note": note, "edits": "[{old, new, all}] applied in order, all-or-nothing", "if_version": "u64?" }, "result": write },
            { "verb": "append", "args": { "note": note, "text": "string", "under": "heading?", "if_version": "u64?" }, "result": write },
            { "verb": "prepend", "args": { "note": note, "text": "string", "if_version": "u64?" }, "result": write },
            { "verb": "write", "args": { "note": note, "body": "string", "if_version": "u64? (required unless force)", "force": "bool" }, "result": write },
            { "verb": "move", "args": { "note": note, "to": "path or existing folder" }, "result": write },
            { "verb": "mkdir", "args": { "folder": "string" }, "result": "\"folder/\"" },
            { "verb": "rmdir", "args": { "folder": "string (must be empty)" }, "result": "\"folder/\"" },
            { "verb": "delete", "args": { "note": note, "purge": "bool" }, "result": write },
            { "verb": "restore", "args": { "note": "original path or title in the trash" }, "result": write },
            { "verb": "history", "args": { "note": note, "limit": "u32?" }, "result": "[{version, at, author}] newest first" },
            { "verb": "diff", "args": { "note": note, "from": "u64? (to - 1)", "to": "u64? (current)" }, "result": "{path, from, to, unified}" },
            { "verb": "revert", "args": { "note": note, "to": "u64" }, "result": write },
            { "verb": "watch", "args": { "since": "u64? (exclusive; default now)", "kinds": "[string] exact or prefix before '-', e.g. note" }, "result": "event lines until the client disconnects" },
            { "verb": "batch", "args": { "ops": "[{verb, args}]", "atomic": "bool" }, "result": "[{ok:true, result} | {ok:false, error}]" },
            { "verb": "manifest", "result": "this object" },
            { "verb": "session-delete", "args": { "id": "string", "purge": "bool" }, "result": "{id, trashed, purged, already}; trashed 30 days unless purge" },
            { "verb": "session-restore", "args": { "id": "string" }, "result": "{id, restored, already}" },
            { "verb": "doctor", "result": "{protocol, daemon_version, socket, vault, database, notes}" }
        ]
    })
}
