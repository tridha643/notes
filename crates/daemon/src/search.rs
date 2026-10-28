//! fff-search 0.11.0 with its own watcher off (`watch: false`, no debounce timer):
//! notesd tells it about every change through `update`. Config matches docs/research/fff-check.md's
//! retest, except searches never stop early on a time budget.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::mpsc::{self, Sender},
    thread,
    time::{Duration, Instant},
};

use fff_search::{
    Casing, Constraint, FFFMode, FilePicker, FilePickerOptions, FrecencyTracker,
    FuzzySearchOptions, GrepConfig, GrepMode, GrepSearchOptions, PaginationArgs, QueryParser,
    SharedFilePicker, SharedFrecency,
};
use notes_proto::{ErrorCode, FindArgs, FindHit, SearchArgs, SearchHit};

use crate::{R, fail};

const DEFAULT_LIMIT: u32 = 50;
const STARTUP_WAIT: Duration = Duration::from_secs(120);
/// Repeat reads of one note inside this window count once toward frecency.
const FRECENCY_DEDUPE: Duration = Duration::from_secs(60);

pub struct Search {
    picker: SharedFilePicker,
    frecency: SharedFrecency,
    /// Paths read, for the frecency thread.
    opened: Sender<PathBuf>,
}

fn fff_err(e: impl std::fmt::Display) -> crate::Fail {
    fail(ErrorCode::Other, format!("fff: {e}"), None)
}

impl Search {
    /// Index the vault and return once scan + content index are done. fff's wait helpers
    /// poll every 10 ms, but only here at startup; nothing polls once we serve.
    pub fn open(vault: &Path, state: &Path) -> R<Search> {
        std::fs::create_dir_all(state)?;
        let (picker, frecency) = (SharedFilePicker::default(), SharedFrecency::default());
        frecency
            .init(FrecencyTracker::open(state.join("frecency")).map_err(fff_err)?)
            .map_err(fff_err)?;
        let options = FilePickerOptions {
            base_path: vault.to_string_lossy().into_owned(),
            enable_mmap_cache: true,
            enable_content_indexing: true,
            mode: FFFMode::Ai,
            watch: false,
            ..Default::default()
        };
        FilePicker::new_with_shared_state(picker.clone(), frecency.clone(), options)
            .map_err(fff_err)?;
        if !picker.wait_for_scan(STARTUP_WAIT) || !picker.wait_for_indexing_complete(STARTUP_WAIT) {
            return Err(fail(ErrorCode::Other, "fff: initial index timed out", None));
        }
        // Each fff frecency record is a synced LMDB commit (F_FULLFSYNC on macOS, ~4 ms), so one
        // thread blocked on a channel does them, off every request's path.
        let (opened, reads) = mpsc::channel::<PathBuf>();
        let (p, f) = (picker.clone(), frecency.clone());
        thread::Builder::new()
            .name("notesd-frecency".into())
            .spawn(move || {
                let mut last = HashMap::new();
                for path in reads {
                    let now = Instant::now();
                    if last
                        .get(&path)
                        .is_none_or(|&t: &Instant| now - t >= FRECENCY_DEDUPE)
                    {
                        track(&p, &f, &path);
                        last.insert(path, now);
                    }
                }
            })?;
        Ok(Search {
            picker,
            frecency,
            opened,
        })
    }

    /// Re-read these paths (created, modified or gone) after notesd changed them or saw them change.
    pub fn update(&self, paths: &[PathBuf]) {
        if paths.is_empty() {
            return;
        }
        let mut overflow = false;
        if let Ok(mut guard) = self.picker.write()
            && let Some(picker) = guard.as_mut()
        {
            for path in paths {
                if path.is_file() {
                    // None: fff's 1,024-slot overflow for post-scan files is full.
                    overflow |= picker.handle_create_or_modify(path).is_none();
                } else {
                    picker.remove_file_by_path(path);
                }
            }
        }
        if overflow {
            // ponytail: one-shot background rescan; writes landing mid-rescan are picked up on their next touch.
            let _ = self.picker.trigger_full_rescan_async(&self.frecency);
        }
    }

    /// Count a read toward frecency ranking (queued; never blocks the caller).
    pub fn opened(&self, path: PathBuf) {
        let _ = self.opened.send(path);
    }

    pub fn find(&self, args: &FindArgs) -> R<Vec<FindHit>> {
        let guard = self.picker.read().map_err(fff_err)?;
        let picker = guard.as_ref().ok_or_else(|| fff_err("index not ready"))?;
        let query = QueryParser::default().parse(&args.query);
        let limit = args.limit.unwrap_or(DEFAULT_LIMIT) as usize;
        let options = FuzzySearchOptions {
            pagination: PaginationArgs { offset: 0, limit },
            ..Default::default()
        };
        let found = picker.fuzzy_search(&query, None, options);
        Ok(found
            .items
            .iter()
            .zip(&found.scores)
            .map(|(file, score)| FindHit {
                path: file.relative_path(picker),
                score: score.total as f64,
            })
            .filter(|hit| is_note(&hit.path))
            .collect())
    }

    pub fn search(&self, args: &SearchArgs, folder: Option<&str>) -> R<Vec<SearchHit>> {
        let guard = self.picker.read().map_err(fff_err)?;
        let picker = guard.as_ref().ok_or_else(|| fff_err("index not ready"))?;
        let mut query = QueryParser::new(GrepConfig).parse(&args.query);
        let glob = folder.map(|f| format!("{f}/**"));
        if let Some(glob) = &glob {
            query.constraints.push(Constraint::Glob(glob));
        }
        let options = GrepSearchOptions {
            page_limit: args.limit.unwrap_or(DEFAULT_LIMIT) as usize,
            casing: Some(if args.case_sensitive {
                Casing::Sensitive
            } else {
                Casing::Smart
            }),
            mode: if args.fuzzy {
                GrepMode::Fuzzy
            } else if fff_search::has_regex_metacharacters(&args.query) {
                GrepMode::Regex
            } else {
                GrepMode::PlainText
            },
            // Run to completion: no time budget, so a miss is a real miss (docs/cli-spec.md).
            time_budget_ms: 0,
            enforce_time_budget: false,
            before_context: args.context as usize,
            after_context: args.context as usize,
            ..Default::default()
        };
        let found = picker.grep(&query, &options);
        let prefix = folder.map(|f| format!("{f}/"));
        Ok(found
            .matches
            .into_iter()
            .map(|m| SearchHit {
                path: found.files[m.file_index].relative_path(picker),
                line: m.line_number as u32,
                text: m.line_content,
                before: m.context_before,
                after: m.context_after,
            })
            // fff's literal fallback can drop the folder constraint; keep the scope exact.
            .filter(|hit| {
                is_note(&hit.path) && prefix.as_ref().is_none_or(|p| hit.path.starts_with(p))
            })
            .collect())
    }
}

fn track(picker: &SharedFilePicker, frecency: &SharedFrecency, path: &Path) {
    if let Ok(frecency) = frecency.read()
        && let Some(tracker) = frecency.as_ref()
        && tracker.track_access(path).is_ok()
        && let Ok(mut guard) = picker.write()
        && let Some(picker) = guard.as_mut()
    {
        let _ = picker.update_single_file_frecency(path, tracker);
    }
}

fn is_note(rel: &str) -> bool {
    rel.ends_with(".md") && !rel.split('/').any(|part| part.starts_with('.'))
}
