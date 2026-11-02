//! One handler per verb. Each runs inside a [`Tx`], so a failure undoes its session-file and SQLite changes.

use notes_proto::{DoctorResult, ErrorCode, PROTOCOL_VERSION, Verb};
use serde_json::{Value, json};

use crate::{Fail, R, fail, store::Tx};

fn value(v: impl serde::Serialize) -> R<Value> {
    Ok(serde_json::to_value(v).expect("results serialize"))
}

impl Tx<'_> {
    pub fn op(&mut self, verb: &Verb) -> R<Value> {
        match verb {
            Verb::Doctor => value(DoctorResult {
                protocol: PROTOCOL_VERSION,
                daemon_version: env!("CARGO_PKG_VERSION").into(),
                socket: self.d.socket.clone(),
                database: self.d.db_path.clone(),
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
            session => self.sessions(session),
        }
    }

    /// Session verbs: sessions, posts, comments, assets, traces, kits, settings.
    fn sessions(&mut self, verb: &Verb) -> R<Value> {
        use notes_posts as p;
        let db = self.store;
        let agent = Some(self.author).filter(|a| *a != "human");
        match verb {
            Verb::SessionNew(a) => self.written_posts(p::session_new(db, a)),
            Verb::SessionList => value(p::session_list(db).map_err(Fail)?),
            Verb::SessionSearch(a) => value(p::session_search(db, a).map_err(Fail)?),
            Verb::SessionRename(a) => self.written_posts(p::session_rename(db, a)),
            Verb::SessionDelete(a) => {
                // Trash only grows here, so expired sessions leave here too, not only at startup.
                p::trash_purge(db, self.now).map_err(Fail)?;
                self.written_posts(p::session_delete(db, a))
            }
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
            Verb::Kits => value(notes_proto::kits::kits()),
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
}

pub fn manifest() -> Value {
    json!({
        "protocol": PROTOCOL_VERSION,
        "daemon_version": env!("CARGO_PKG_VERSION"),
        "transport": "unix socket; one JSON object per line each way",
        "request": { "v": "u32 (1)", "id": "u64", "verb": "string", "args": "object", "cwd": "path?", "agent": "string? (author of posts and comments, default human)" },
        "responses": ["{id, ok:true, result}", "{id, ok:false, error:{code, message, hint?, details?}}", "{id, event:{seq, kind, id?, session?}} (watch)"],
        "error_codes": { "other": 1, "usage": 2, "not_found": 3, "daemon_unreachable": 6 },
        "event_kinds": ["session-created", "session-updated", "session-deleted", "post-created", "post-updated", "post-deleted", "comment-created", "comment-deleted", "trace-updated", "setting-changed"],
        "verbs": [
            { "verb": "watch", "args": { "since": "u64? (exclusive; default now)", "kinds": "[string] exact or prefix before '-', e.g. post" }, "result": "event lines until the client disconnects" },
            { "verb": "batch", "args": { "ops": "[{verb, args}]", "atomic": "bool" }, "result": "[{ok:true, result} | {ok:false, error}]" },
            { "verb": "manifest", "result": "this object" },
            { "verb": "session-delete", "args": { "id": "string", "purge": "bool" }, "result": "{id, trashed, purged, already}; trashed 30 days unless purge" },
            { "verb": "session-restore", "args": { "id": "string" }, "result": "{id, restored, already}" },
            { "verb": "doctor", "result": "{protocol, daemon_version, socket, database, render_cache?, seq?, stage?}" }
        ]
    })
}
