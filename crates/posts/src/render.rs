//! Daemon-side part rendering and its content-addressed cache (see crate docs).

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use notes_proto::posts::{Part, PartRender, Post, PostRender, PostShowArgs};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{ansi, diff, highlight, store::post_show, Result, Store};

/// Part of every cache key. Bump it whenever render output changes (renderer
/// code, class names, or a grammar crate upgrade) so old entries are never served.
pub const RENDER_VERSION: &str = "notes-posts-render-1";

const MEMORY_BUDGET: usize = 64 << 20;
const TABLE_BUDGET: i64 = 256 << 20;
/// Trim the table every this many inserts (a single windowed DELETE).
const TRIM_EVERY: u32 = 64;
const NATIVE_JSON: &str = r#"{"kind":"native"}"#;

/// Render one part, uncached. Code, diff and terminal parts are done here;
/// the rest render in the app and come back as `Native`.
///
/// Every field read here must also feed [`cache_key`].
pub fn render_part(part: &Part) -> PartRender {
    match part {
        Part::Code {
            code,
            language,
            line_start,
            ..
        } => {
            let lang = language.as_deref().and_then(highlight::Lang::from_name);
            PartRender::Code {
                language: lang
                    .map(|l| l.name().to_owned())
                    .or_else(|| language.clone()),
                line_start: line_start.unwrap_or(1),
                lines: highlight::highlight(lang, code),
            }
        }
        Part::Diff {
            patch,
            files,
            layout,
            ..
        } => PartRender::Diff {
            layout: *layout,
            files: match patch.as_deref().filter(|p| !p.is_empty()) {
                Some(patch) => diff::render_patch(patch),
                None => diff::render_files(files),
            },
        },
        Part::Terminal { text, .. } => PartRender::Terminal {
            lines: ansi::parse(text),
        },
        _ => PartRender::Native,
    }
}

/// SHA-256 over [`RENDER_VERSION`] and every field [`render_part`] reads,
/// length-prefixed; `None` for kinds rendered in the app. Fields the renderer
/// ignores (ids, titles, `cols`) are left out on purpose, so renaming a code
/// part's title doesn't re-render it. They're named below so that a renderer
/// change that starts reading one has to touch this match too.
fn cache_key(part: &Part) -> Option<[u8; 32]> {
    let mut h = Sha256::new();
    h.update(RENDER_VERSION.as_bytes());
    fn field(h: &mut Sha256, s: &str) {
        h.update((s.len() as u64).to_le_bytes());
        h.update(s.as_bytes());
    }
    fn opt(h: &mut Sha256, s: Option<&str>) {
        h.update([s.is_some() as u8]);
        field(h, s.unwrap_or(""));
    }
    match part {
        Part::Code {
            code,
            language,
            line_start,
            title: _,
            id: _,
        } => {
            h.update(b"code");
            field(&mut h, code);
            opt(&mut h, language.as_deref());
            h.update(line_start.unwrap_or(1).to_le_bytes());
        }
        Part::Diff {
            patch,
            files,
            layout,
            id: _,
        } => {
            h.update(b"diff");
            opt(&mut h, patch.as_deref());
            h.update((files.len() as u64).to_le_bytes());
            for f in files {
                field(&mut h, &f.filename);
                field(&mut h, &f.before);
                field(&mut h, &f.after);
                opt(&mut h, f.language.as_deref());
            }
            h.update([*layout as u8]);
        }
        Part::Terminal {
            text,
            cols: _,
            title: _,
            id: _,
        } => {
            h.update(b"terminal");
            field(&mut h, text);
        }
        _ => return None,
    }
    Some(h.finalize().into())
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    /// Entries and bytes held in memory (bytes = their JSON size).
    pub entries: u64,
    pub bytes: u64,
}

struct Entry {
    json: Arc<str>,
    used: u64,
}

#[derive(Default)]
struct Lru {
    map: HashMap<[u8; 32], Entry>,
    bytes: usize,
    tick: u64,
    hits: u64,
    misses: u64,
    inserts: u32,
}

impl Lru {
    fn get(&mut self, key: &[u8; 32]) -> Option<Arc<str>> {
        self.tick += 1;
        let entry = self.map.get_mut(key)?;
        entry.used = self.tick;
        Some(entry.json.clone())
    }

    fn insert(&mut self, key: [u8; 32], json: Arc<str>, budget: usize) {
        self.tick += 1;
        self.bytes += json.len();
        if let Some(old) = self.map.insert(
            key,
            Entry {
                json,
                used: self.tick,
            },
        ) {
            self.bytes -= old.json.len();
        }
        // ponytail: O(n) scan for the oldest entry; n is a few thousand at most
        // under a 64 MiB budget. Swap for an intrusive list if eviction shows up.
        while self.bytes > budget && self.map.len() > 1 {
            let oldest = *self
                .map
                .iter()
                .min_by_key(|(_, e)| e.used)
                .map(|(k, _)| k)
                .expect("non-empty");
            let gone = self.map.remove(&oldest).expect("present");
            self.bytes -= gone.json.len();
        }
    }
}

/// The render cache, holding each part's render as wire JSON in both tiers so
/// a hit is a lookup plus a copy. One per daemon; share it by reference.
pub struct RenderCache {
    enabled: bool,
    budget: usize,
    lru: Mutex<Lru>,
}

impl Default for RenderCache {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderCache {
    /// 64 MiB in memory; `NOTES_RENDER_CACHE=off` disables caching.
    pub fn new() -> Self {
        let off = std::env::var("NOTES_RENDER_CACHE").is_ok_and(|v| v.eq_ignore_ascii_case("off"));
        Self::with_config(!off, MEMORY_BUDGET)
    }

    pub fn with_config(enabled: bool, memory_budget: usize) -> Self {
        RenderCache {
            enabled,
            budget: memory_budget,
            lru: Mutex::new(Lru::default()),
        }
    }

    fn lru(&self) -> std::sync::MutexGuard<'_, Lru> {
        self.lru.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn stats(&self) -> CacheStats {
        let lru = self.lru();
        CacheStats {
            hits: lru.hits,
            misses: lru.misses,
            entries: lru.map.len() as u64,
            bytes: lru.bytes as u64,
        }
    }

    /// One part's `PartRender` as JSON: memory, then the `render_cache` table,
    /// then a fresh render stored in both. Native kinds never touch the cache.
    /// Cache I/O errors are ignored (the render is still returned).
    pub fn render_part_json(&self, conn: &Connection, part: &Part) -> Arc<str> {
        let Some(key) = cache_key(part) else {
            return NATIVE_JSON.into();
        };
        let fresh = || -> Arc<str> {
            serde_json::to_string(&render_part(part))
                .expect("render serializes")
                .into()
        };
        if !self.enabled {
            self.lru().misses += 1;
            return fresh();
        }
        {
            let mut lru = self.lru();
            if let Some(hit) = lru.get(&key) {
                lru.hits += 1;
                return hit;
            }
        }
        // Not holding the lock while reading the table or rendering.
        let stored: Option<String> = conn
            .prepare_cached("SELECT value FROM render_cache WHERE key = ?1")
            .and_then(|mut s| {
                s.query_row([&key[..]], |r| r.get::<_, Vec<u8>>(0))
                    .optional()
            })
            .ok()
            .flatten()
            .and_then(|bytes| String::from_utf8(bytes).ok());
        if let Some(json) = stored {
            let json: Arc<str> = json.into();
            let mut lru = self.lru();
            lru.hits += 1;
            lru.insert(key, json.clone(), self.budget);
            return json;
        }
        let json = fresh();
        let _ = conn
            .prepare_cached(
                "INSERT OR REPLACE INTO render_cache (key, value, bytes) VALUES (?1, ?2, ?3)",
            )
            .and_then(|mut s| s.execute(params![&key[..], json.as_bytes(), json.len() as i64]));
        let trim = {
            let mut lru = self.lru();
            lru.misses += 1;
            lru.insert(key, json.clone(), self.budget);
            lru.inserts += 1;
            lru.inserts.is_multiple_of(TRIM_EVERY)
        };
        if trim {
            // Keep the newest rows whose running size fits the table budget.
            let _ = conn.execute(
                "DELETE FROM render_cache WHERE rowid IN (
                    SELECT rowid FROM (SELECT rowid, sum(bytes) OVER (ORDER BY rowid DESC) AS run FROM render_cache)
                    WHERE run > ?1)",
                [TABLE_BUDGET],
            );
        }
        json
    }

    /// A post's [`PostRender`] as JSON, spliced from cached part JSON with no
    /// re-serialization: the fast path for answering `post-render`.
    pub fn render_post_json(&self, conn: &Connection, post: &Post) -> String {
        let parts: Vec<Arc<str>> = post
            .parts
            .iter()
            .map(|p| self.render_part_json(conn, p))
            .collect();
        let mut out = String::with_capacity(64 + parts.iter().map(|p| p.len() + 1).sum::<usize>());
        out.push_str(r#"{"id":"#);
        out.push_str(&serde_json::to_string(&post.id).expect("string serializes"));
        out.push_str(&format!(r#","version":{},"parts":["#, post.version));
        for (i, part) in parts.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(part);
        }
        out.push_str("]}");
        out
    }

    /// A post's render as a typed value (parses the cached JSON).
    pub fn render_post(&self, conn: &Connection, post: &Post) -> PostRender {
        PostRender {
            id: post.id.clone(),
            version: post.version,
            parts: post
                .parts
                .iter()
                .map(|p| {
                    serde_json::from_str(&self.render_part_json(conn, p))
                        .expect("cached render parses")
                })
                .collect(),
        }
    }
}

/// The `post-render` verb as JSON (see [`RenderCache::render_post_json`]).
pub fn post_render_json(s: &Store, cache: &RenderCache, args: &PostShowArgs) -> Result<String> {
    Ok(cache.render_post_json(&s.db, &post_show(s, args)?))
}

/// The `post-render` verb as a typed value.
pub fn post_render(s: &Store, cache: &RenderCache, args: &PostShowArgs) -> Result<PostRender> {
    Ok(cache.render_post(&s.db, &post_show(s, args)?))
}

/// Compile every language's highlight queries now (~350 ms total, swift alone
/// ~190 ms) instead of on each language's first render. Optional: call it
/// from a background thread at daemon start to keep first renders smooth.
pub fn warm_languages() {
    for lang in highlight::ALL_LANGS {
        lang.config();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrate;

    fn code(text: &str) -> Part {
        Part::Code {
            code: text.into(),
            language: Some("rust".into()),
            title: None,
            line_start: None,
            id: None,
        }
    }

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        conn
    }

    #[test]
    fn hits_misses_and_table_tier_are_counted() {
        let conn = db();
        let cache = RenderCache::with_config(true, 1 << 20);
        let a = cache.render_part_json(&conn, &code("fn a() {}"));
        assert_eq!(
            serde_json::from_str::<PartRender>(&a).unwrap(),
            render_part(&code("fn a() {}"))
        );
        assert_eq!(
            cache.stats(),
            CacheStats {
                hits: 0,
                misses: 1,
                entries: 1,
                bytes: a.len() as u64
            }
        );
        let again = cache.render_part_json(&conn, &code("fn a() {}"));
        assert!(
            Arc::ptr_eq(&a, &again),
            "memory hit returns the cached value"
        );
        cache.render_part_json(&conn, &code("fn b() {}"));
        let s = cache.stats();
        assert_eq!((s.hits, s.misses, s.entries), (1, 2, 2));

        // A fresh process (empty memory) finds the render in the table.
        let cold = RenderCache::with_config(true, 1 << 20);
        assert_eq!(cold.render_part_json(&conn, &code("fn a() {}")), a);
        assert_eq!((cold.stats().hits, cold.stats().misses), (1, 0));
        let rows: i64 = conn
            .query_row("SELECT count(*) FROM render_cache", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 2);
    }

    #[test]
    fn key_covers_render_inputs_only() {
        let part = |language: &str,
                    title: Option<&str>,
                    line_start: Option<u32>,
                    id: Option<&str>| Part::Code {
            code: "fn a() {}".into(),
            language: Some(language.into()),
            title: title.map(Into::into),
            line_start,
            id: id.map(Into::into),
        };
        let titled = part("rust", Some("x.rs"), None, Some("p1"));
        assert_eq!(
            cache_key(&titled),
            cache_key(&code("fn a() {}")),
            "title and id don't change the render"
        );
        assert_ne!(
            cache_key(&part("rust", None, Some(10), None)),
            cache_key(&titled)
        );
        assert_ne!(cache_key(&part("go", None, None, None)), cache_key(&titled));
        let term = |text: &str, cols| Part::Terminal {
            text: text.into(),
            cols,
            title: None,
            id: None,
        };
        assert_eq!(cache_key(&term("x", Some(80))), cache_key(&term("x", None)));
        assert_ne!(cache_key(&term("x", None)), cache_key(&term("y", None)));
        assert_eq!(
            cache_key(&Part::Markdown {
                markdown: "x".into(),
                id: None
            }),
            None
        );
    }

    #[test]
    fn memory_tier_is_bounded_by_bytes() {
        let conn = db();
        let cache = RenderCache::with_config(true, 1);
        cache.render_part_json(&conn, &code("fn a() {}"));
        cache.render_part_json(&conn, &code("fn b() {}"));
        assert_eq!(
            cache.stats().entries,
            1,
            "over budget keeps only the newest"
        );
    }

    #[test]
    fn off_switch_bypasses_both_tiers() {
        let conn = db();
        let cache = RenderCache::with_config(false, 1 << 20);
        let a = cache.render_part_json(&conn, &code("fn a() {}"));
        let b = cache.render_part_json(&conn, &code("fn a() {}"));
        assert_eq!(a, b);
        assert_eq!(
            cache.stats(),
            CacheStats {
                hits: 0,
                misses: 2,
                entries: 0,
                bytes: 0
            }
        );
        let rows: i64 = conn
            .query_row("SELECT count(*) FROM render_cache", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[test]
    fn native_kinds_skip_the_cache_and_json_matches_typed() {
        let conn = db();
        let cache = RenderCache::with_config(true, 1 << 20);
        let post = Post {
            id: "p\"1".into(),
            session: "s".into(),
            title: "t".into(),
            parts: vec![
                Part::Markdown {
                    markdown: "# hi".into(),
                    id: None,
                },
                code("let x = 1;\nlet y = 2;\n"),
            ],
            version: 3,
            created_at: 0,
            updated_at: 0,
        };
        let r = cache.render_post(&conn, &post);
        assert_eq!((r.id.as_str(), r.version), ("p\"1", 3));
        assert_eq!(r.parts[0], PartRender::Native);
        let PartRender::Code {
            language,
            line_start,
            lines,
        } = &r.parts[1]
        else {
            panic!("code")
        };
        assert_eq!(
            (language.as_deref(), *line_start, lines.len()),
            (Some("rust"), 1, 2)
        );
        assert_eq!(
            serde_json::from_str::<PostRender>(&cache.render_post_json(&conn, &post)).unwrap(),
            r
        );
        assert_eq!((cache.stats().misses, cache.stats().hits), (1, 1));
    }
}
