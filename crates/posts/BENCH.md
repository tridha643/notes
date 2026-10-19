# notes-posts benchmarks

Measured 2026-10-18 on an Apple M5 (10 cores, 24 GB), release build, with
`cargo run --release -p notes-posts --example bench`. Medians and p99s over
10–2,000 runs per row (see `examples/bench.rs`); single machine, nothing else
pinned, so treat the third digit as noise. Rerun after any renderer change.

## Highlighter choice

On the same 500 lines of Rust (scratch comparison, not kept in the repo):

| | load | first highlight | warm highlight |
|---|---|---|---|
| syntect 5.3 `default-fancy` | 1.4 ms (lazy) | 60 ms | 12 ms |
| tree-sitter-highlight 0.25 | 9.5 ms (query compile) | 1.4 ms | 1.3 ms |

tree-sitter is ~10x faster per render, which matters more than its slightly
larger one-time load.

## Language load (one-time per language, per process)

First render of a one-line part, which compiles that language's highlight
queries into a `static OnceLock`. Later renders never pay it again.

| language | load | language | load |
|---|---|---|---|
| rust | 12–28 ms (varies by run) | javascript | 8 ms |
| swift | 170 ms | typescript | 17 ms |
| cpp | 19 ms | tsx | 18 ms |
| ruby | 17 ms | bash | 11 ms |
| c, java, python, yaml | 2–3 ms | go, css, toml, html, json | ≤1 ms |

All 17 together take ~285 ms. `warm_languages()` does them all up front if
the daemon would rather pay that at startup on a background thread than on
the first Swift part.

## Render

Rendering runs at a typical 2–9 ms cold and a 5–46 µs cache hit. The one
exception is the 1 MB log: its hit costs 0.3 ms, almost all of it hashing
1 MB for the key.

| case | median | p99 |
|---|---|---|
| 500-line Rust code: cold render (no cache) | 2.400 ms | 2.795 ms |
| 500-line Rust code: cache miss (render + store) | 2.473 ms | 2.782 ms |
| 500-line Rust code: memory hit | 0.005 ms | 0.005 ms |
| 500-line Rust code: memory hit, `render_post_json` | 0.005 ms | 0.006 ms |
| 500-line Rust code: memory hit, `render_post` (typed) | 0.270 ms | 0.397 ms |
| 500-line Rust code: table hit (new process, warm db) | 0.010 ms | 0.014 ms |
| 500-line Rust code: `NOTES_RENDER_CACHE=off` | 2.460 ms | 2.711 ms |
| 2,000-line 10-file git patch: cold render | 8.420 ms | 10.168 ms |
| 2,000-line 10-file git patch: cache miss | 8.700 ms | 9.327 ms |
| 2,000-line 10-file git patch: memory hit | 0.022 ms | 0.022 ms |
| 2,000-line 10-file git patch: memory hit, `render_post_json` | 0.027 ms | 0.033 ms |
| 2,000-line 10-file git patch: memory hit, `render_post` (typed) | 1.273 ms | 1.474 ms |
| 2,000-line 10-file git patch: table hit | 0.046 ms | 0.060 ms |
| 500-line before/after file pair: cold render | 3.158 ms | 3.361 ms |
| 500-line before/after file pair: memory hit | 0.010 ms | 0.010 ms |
| 500-line before/after file pair: table hit | 0.016 ms | 0.026 ms |
| 1 MB ANSI terminal log: cold render | 4.241 ms | 4.528 ms |
| 1 MB ANSI terminal log: cache miss | 5.629 ms | 5.818 ms |
| 1 MB ANSI terminal log: memory hit | 0.295 ms | 0.314 ms |
| 1 MB ANSI terminal log: memory hit, `render_post_json` | 0.316 ms | 0.334 ms |
| 1 MB ANSI terminal log: memory hit, `render_post` (typed) | 5.081 ms | 6.396 ms |
| 1 MB ANSI terminal log: table hit | 0.432 ms | 0.583 ms |

Notes:

- **Hit cost is the key hash.** A hit is a SHA-256 over the part's render
  inputs plus an `Arc` clone, so it scales with part size. sha2's `asm`
  feature turns on the ARMv8 SHA instructions; without it the 1 MB hit was
  1.7 ms.
- **Use the JSON path for the wire.** `render_post` (typed) parses the cached
  JSON back into structs, which costs 0.3–5 ms for big parts. The daemon
  should answer `post-render` with `post_render_json`.
- **Diff sides highlight in parallel.** Each file's old and new side runs on
  scoped threads once a diff passes 16 KiB. Serial, the 10-file patch took
  39 ms, because hunks are code fragments and tree-sitter's error recovery
  makes them slower per line than whole files.
- **The 1 MB terminal miss** (5.6 ms) includes serializing ~1.7 MB of render
  JSON and writing it to the `render_cache` table.

## Store (in-memory SQLite, 2,000 runs each)

| verb | median | p99 |
|---|---|---|
| post_new (2 parts, implicit session) | 0.007 ms | 0.014 ms |
| post_show | 0.002 ms | 0.002 ms |
| comment_new | 0.004 ms | 0.017 ms |
| post_update (title only, piggybacks 1 comment) | 0.006 ms | 0.014 ms |
| post_part set (revalidate + rewrite) | 0.009 ms | 0.034 ms |

These are CPU-only numbers. On the daemon's file-backed WAL database, each
write also pays a commit (and fsync, depending on `synchronous`), which isn't
measured here.
