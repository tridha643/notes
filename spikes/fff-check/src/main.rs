//! Throwaway resident fff-search probe; the Python driver owns corpus and mutations.
use fff_search::{
    Casing, Constraint, FFFMode, FilePicker, FilePickerOptions, FrecencyTracker,
    FuzzySearchOptions, GrepConfig, GrepMode, GrepSearchOptions, PaginationArgs, QueryParser,
    QueryTracker, SharedFilePicker, SharedFrecency,
};
mod vault_watcher;

use serde_json::{Value, json};
use std::error::Error;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

type ProbeResult<T> = Result<T, Box<dyn Error>>;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);

fn emit_json(value: &Value) -> ProbeResult<()> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, value)?;
    writeln!(stdout)?;
    stdout.flush()?;
    Ok(())
}

fn percentile_ms(samples: &[f64], percentile: f64) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[(sorted.len() as f64 * percentile).ceil() as usize - 1]
}

// Includes parser + shared lock + search + copying paths/lines into a JSON value.
// JSON encoding and pipe I/O happen outside the timed boundary.
fn execute_query(
    shared: &SharedFilePicker,
    tracker: &QueryTracker,
    request: &Value,
) -> ProbeResult<Value> {
    let text = request["query"]
        .as_str()
        .ok_or("fff-check: missing query")?;
    let guard = shared.read()?;
    let picker = guard.as_ref().ok_or("fff-check: missing picker")?;
    if request["kind"] == "find" {
        let parser = QueryParser::default();
        let query = parser.parse(text);
        let found = picker.fuzzy_search(
            &query,
            Some(tracker),
            FuzzySearchOptions {
                pagination: PaginationArgs {
                    offset: 0,
                    limit: 50,
                },
                ..Default::default()
            },
        );
        let paths: Vec<_> = found
            .items
            .iter()
            .map(|f| f.relative_path(picker))
            .collect();
        return Ok(json!({"paths": paths, "total_matched": found.total_matched}));
    }
    let parser = QueryParser::new(GrepConfig);
    let mut query = parser.parse(text);
    if let Some(scope) = request["scope"].as_str() {
        query.constraints.push(Constraint::FilePath(scope));
    }
    let options = GrepSearchOptions {
        page_limit: 50,
        casing: Some(Casing::Smart),
        mode: if request["fuzzy"] == true {
            GrepMode::Fuzzy
        } else if fff_search::has_regex_metacharacters(text) {
            GrepMode::Regex
        } else {
            GrepMode::PlainText
        },
        // Default budget semantics and enforced deadlines are measured separately.
        time_budget_ms: 8,
        enforce_time_budget: request["enforce_budget"] == true,
        ..Default::default()
    };
    let found = picker.grep(&query, &options);
    let matches: Vec<_> = found
        .matches
        .iter()
        .map(|m| {
            json!({
                "path": found.files[m.file_index].relative_path(picker),
                "line": m.line_number,
                "text": m.line_content,
            })
        })
        .collect();
    Ok(json!({
        "matches": matches, "explicit_fuzzy": request["fuzzy"] == true,
        "files_searched": found.total_files_searched,
        "next_file_offset": found.next_file_offset,
        "regex_error": found.regex_fallback_error,
    }))
}

fn main() -> ProbeResult<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("fff-check: usage: fff-check VAULT STATE_DIRECTORY".into());
    }
    let vault = PathBuf::from(&args[0]).canonicalize()?;
    let state = PathBuf::from(&args[1]);
    std::fs::create_dir_all(&state)?;
    let started = Instant::now();
    let shared = SharedFilePicker::default();
    let frecency = SharedFrecency::default();
    frecency.init(FrecencyTracker::open(state.join("frecency"))?)?;
    // Direct tracker ownership avoids introducing a second GC thread just for reads.
    let tracker = QueryTracker::open(state.join("queries"))?;
    let init_started = Instant::now();
    FilePicker::new_with_shared_state(
        shared.clone(),
        frecency.clone(),
        FilePickerOptions {
            base_path: vault.to_string_lossy().into_owned(),
            enable_mmap_cache: true,
            enable_content_indexing: true,
            mode: FFFMode::Ai,
            watch: false,
            ..Default::default()
        },
    )?;
    if !shared.wait_for_scan(STARTUP_TIMEOUT) {
        return Err("fff-check: initial scan timed out".into());
    }
    let scan_ms = init_started.elapsed().as_secs_f64() * 1000.0;
    if !shared.wait_for_indexing_complete(STARTUP_TIMEOUT) {
        return Err("fff-check: content indexing timed out".into());
    }
    let index_ms = init_started.elapsed().as_secs_f64() * 1000.0;
    let mut watcher = Some(vault_watcher::VaultWatcher::start(&vault, shared.clone())?);
    let file_count = shared.read()?.as_ref().unwrap().live_file_count();
    emit_json(&json!({
        "event": "ready", "pid": std::process::id(), "files": file_count,
        "watch": false, "grep_budget_ms": 8, "automatic_fuzzy_retry": false,
        "scan_ms": scan_ms, "index_complete_ms": index_ms,
        "ready_ms": started.elapsed().as_secs_f64() * 1000.0,
    }))?;

    // Blocks on stdin between requests: the harness adds no idle timer or poll loop.
    for line in io::stdin().lock().lines() {
        let request: Value = serde_json::from_str(&line?)?;
        match request["op"].as_str() {
            Some("events") => {
                let events = watcher
                    .as_ref()
                    .ok_or("fff-check: watcher stopped")?
                    .drain_events();
                emit_json(&json!({"events": events}))?;
            }
            Some("stop_watcher") => {
                if let Some(watcher) = watcher.take() {
                    watcher.stop()?;
                }
                emit_json(&json!({"watcher_stopped": true}))?;
            }
            Some("update") => {
                let path = vault.join(
                    request["path"]
                        .as_str()
                        .ok_or("fff-check: missing update path")?,
                );
                let action = vault_watcher::apply_vault_update(&shared, &path)?;
                emit_json(&json!({"manual_update": action, "path": path}))?;
            }
            Some("query") => emit_json(&execute_query(&shared, &tracker, &request)?)?,
            Some("bench") => {
                let rounds = request["rounds"]
                    .as_u64()
                    .ok_or("fff-check: missing rounds")?;
                if rounds == 0 {
                    return Err("fff-check: rounds must be positive".into());
                }
                let queries = request["queries"]
                    .as_array()
                    .ok_or("fff-check: missing queries")?;
                let mut samples = vec![Vec::new(); queries.len()];
                let mut examples = Vec::new();
                let mut outcomes = vec![Vec::new(); queries.len()];
                for query in queries {
                    // Two excluded warmups per query, including first lazy engine work.
                    execute_query(&shared, &tracker, query)?;
                    examples.push(execute_query(&shared, &tracker, query)?);
                }
                // Rotate order to spread temporal drift across the query population.
                for round in 0..rounds as usize {
                    for offset in 0..queries.len() {
                        let i = (offset + round) % queries.len();
                        let start = Instant::now();
                        let result = execute_query(&shared, &tracker, &queries[i])?;
                        std::hint::black_box(&result);
                        samples[i].push(start.elapsed().as_secs_f64() * 1000.0);
                        outcomes[i].push(
                            json!({"matches": result["matches"].as_array().map(Vec::len),
                            "next_file_offset": result["next_file_offset"],
                            "files_searched": result["files_searched"]}),
                        );
                    }
                }
                let rows: Vec<_> = queries
                    .iter()
                    .enumerate()
                    .map(|(i, query)| {
                        json!({
                            "request": query, "p50_ms": percentile_ms(&samples[i], 0.50),
                            "p95_ms": percentile_ms(&samples[i], 0.95),
                            "max_ms": percentile_ms(&samples[i], 1.0), "samples_ms": samples[i],
                            "example": examples[i], "outcomes": outcomes[i],
                        })
                    })
                    .collect();
                emit_json(&json!({"event": "bench", "rounds": rounds, "queries": rows}))?;
            }
            Some("quit") => break,
            _ => return Err("fff-check: unknown operation".into()),
        }
    }
    // Take ownership out of the guard before stopping watcher threads that need that lock.
    if let Some(watcher) = watcher.take() {
        watcher.stop()?;
    }
    let picker = shared.write()?.take();
    if let Some(mut picker) = picker {
        picker.stop_background_monitor();
    }
    Ok(())
}
