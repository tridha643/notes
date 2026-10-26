//! `cargo run --release -p notes-posts --example bench` — numbers for BENCH.md.

use std::time::{Duration, Instant};

use notes_posts::*;
use notes_proto::posts::*;
use rusqlite::Connection;

fn time<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let t = Instant::now();
    let v = f();
    (v, t.elapsed())
}

/// Median and p99 of `n` runs.
fn stats(n: usize, mut f: impl FnMut()) -> (Duration, Duration) {
    let mut runs: Vec<Duration> = (0..n).map(|_| time(&mut f).1).collect();
    runs.sort();
    (runs[n / 2], runs[(n * 99 / 100).min(n - 1)])
}

fn ms(d: Duration) -> String {
    format!("{:.3} ms", d.as_secs_f64() * 1e3)
}

fn row(name: &str, (median, p99): (Duration, Duration)) {
    println!("| {name} | {} | {} |", ms(median), ms(p99));
}

fn db() -> Store {
    let dir = std::env::temp_dir().join(format!("notes-posts-bench-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    Store::open(Connection::open_in_memory().unwrap(), dir).unwrap()
}

fn code(text: String, lang: &str) -> Part {
    Part::Code {
        code: text,
        language: Some(lang.into()),
        title: None,
        line_start: None,
        id: None,
    }
}

const RUST_SRC: &str = include_str!("../src/store.rs");

fn rust_500() -> String {
    RUST_SRC
        .lines()
        .cycle()
        .take(500)
        .map(|l| format!("{l}\n"))
        .collect()
}

/// A git patch over 10 Rust files, 2,000 diff lines in all.
fn patch_2000() -> String {
    let src: Vec<&str> = RUST_SRC.lines().collect();
    let mut patch = String::new();
    let mut lines = 0;
    for f in 0..10 {
        patch += &format!("diff --git a/src/f{f}.rs b/src/f{f}.rs\nindex 1..2 100644\n--- a/src/f{f}.rs\n+++ b/src/f{f}.rs\n");
        for h in 0..10 {
            // 20 lines per hunk: 14 context, 3 removed, 3 added.
            let start = (f * 37 + h * 41) % (src.len() - 20);
            patch += &format!("@@ -{0},17 +{0},17 @@\n", start + 1);
            for (i, l) in src[start..start + 17].iter().enumerate() {
                match i {
                    7..=9 => {
                        patch += &format!("-{l}\n");
                    }
                    _ => patch += &format!(" {l}\n"),
                }
                if i == 9 {
                    for l in &src[start + 7..start + 10] {
                        patch += &format!("+{}\n", l.replace("conn", "db"));
                    }
                }
            }
            lines += 20;
        }
    }
    assert_eq!(lines, 2000);
    patch
}

/// ~1 MB of colourful test-runner output with progress-bar overprints.
fn ansi_1mb() -> String {
    let mut s = String::new();
    let mut i = 0;
    while s.len() < 1 << 20 {
        s += &match i % 4 {
            0 => format!("\x1b[1;32m PASS \x1b[0m src/test_{i}.rs \x1b[2m({i} ms)\x1b[22m\n"),
            1 => format!("\x1b[33mwarning\x1b[39m: unused variable `x{i}` \x1b[38;5;244m--> src/lib.rs:{i}:9\x1b[0m\n"),
            2 => format!("[{:>3}%]\r[{:>3}%]\r\x1b[2K\x1b[38;2;80;160;255mdownloading\x1b[0m crate-{i}\n", i % 100, (i + 1) % 100),
            _ => format!("test tests::case_{i} ... \x1b[32mok\x1b[0m\n"),
        };
        i += 1;
    }
    s
}

fn main() {
    println!("## Language load (one-time, per language)\n");
    let (_, rust_load) = time(|| render_part(&code("fn a() {}\n".into(), "rust")));
    println!("- rust: first render incl. query compile {}", ms(rust_load));
    let (_, rust_again) = time(|| render_part(&code("fn a() {}\n".into(), "rust")));
    println!("- rust: second tiny render {}", ms(rust_again));
    let langs = [
        "bash",
        "c",
        "cpp",
        "css",
        "go",
        "html",
        "java",
        "javascript",
        "json",
        "python",
        "ruby",
        "swift",
        "toml",
        "tsx",
        "typescript",
        "yaml",
    ];
    let mut total = rust_load;
    for lang in langs {
        let (_, d) = time(|| render_part(&code("x\n".into(), lang)));
        println!("- {lang}: {}", ms(d));
        total += d;
    }
    println!("- all 17 languages: {}\n", ms(total));

    let rust = code(rust_500(), "rust");
    let diff = Part::Diff {
        patch: Some(patch_2000()),
        files: vec![],
        layout: DiffLayout::Unified,
        id: None,
    };
    let before = rust_500();
    let after = before.replace("conn", "db");
    let pair = Part::Diff {
        patch: None,
        files: vec![DiffFile {
            filename: "src/store.rs".into(),
            before,
            after,
            language: None,
        }],
        layout: DiffLayout::Split,
        id: None,
    };
    let term = Part::Terminal {
        text: ansi_1mb(),
        cols: None,
        title: None,
        id: None,
    };

    println!("## Render (median / p99)\n");
    println!("| case | median | p99 |\n|---|---|---|");
    for (name, part, n) in [
        ("500-line Rust code", &rust, 50),
        ("2,000-line 10-file git patch", &diff, 20),
        ("500-line before/after pair", &pair, 20),
        ("1 MB ANSI terminal log", &term, 10),
    ] {
        row(
            &format!("{name}: cold render (no cache)"),
            stats(n, || drop(render_part(part))),
        );
        // Cache miss: render + serialize + table insert, each run on a fresh cache and db.
        let mut runs = Vec::new();
        for _ in 0..n {
            let (conn, cache) = (db(), RenderCache::with_config(true, 64 << 20));
            runs.push(time(|| drop(cache.render_part_json(&conn.db, part))).1);
        }
        runs.sort();
        row(
            &format!("{name}: cache miss (render + store)"),
            (runs[n / 2], runs[(n * 99 / 100).min(n - 1)]),
        );
        let conn = db();
        let cache = RenderCache::with_config(true, 64 << 20);
        cache.render_part_json(&conn.db, part);
        row(
            &format!("{name}: memory hit (key hash + Arc)"),
            stats(200, || drop(cache.render_part_json(&conn.db, part))),
        );
        let post = Post {
            id: "p".into(),
            session: "s".into(),
            title: "t".into(),
            parts: vec![part.clone()],
            version: 1,
            created_at: 0,
            updated_at: 0,
        };
        row(
            &format!("{name}: memory hit, render_post_json (wire JSON)"),
            stats(200, || drop(cache.render_post_json(&conn.db, &post))),
        );
        row(
            &format!("{name}: memory hit, render_post (typed, parses JSON)"),
            stats(50, || drop(cache.render_post(&conn.db, &post))),
        );
        row(
            &format!("{name}: table hit (fresh process, warm db)"),
            stats(n, || {
                drop(RenderCache::with_config(true, 64 << 20).render_part_json(&conn.db, part))
            }),
        );
        let s = cache.stats();
        println!("<!-- stats {name}: {s:?} -->");
    }
    let off = RenderCache::with_config(false, 0);
    let conn = db();
    row(
        "500-line Rust code: NOTES_RENDER_CACHE=off",
        stats(50, || drop(off.render_part_json(&conn.db, &rust))),
    );

    println!("\n## Store on a temp session folder (median / p99 of 2,000)\n");
    println!("| verb | median | p99 |\n|---|---|---|");
    let conn = db();
    let parts = vec![
        Part::Markdown {
            markdown: "# Plan\n\nSome prose about the change.".into(),
            id: None,
        },
        code(
            RUST_SRC.lines().take(60).collect::<Vec<_>>().join("\n"),
            "rust",
        ),
    ];
    let args = PostNewArgs {
        session: None,
        title: Some("Bench".into()),
        parts,
        ..Default::default()
    };
    let mut ids = Vec::new();
    row(
        "post_new (2 parts, implicit session)",
        stats(2000, || {
            ids.push(
                post_new(&conn, &args, Some("claude"), Some("/repo"))
                    .unwrap()
                    .0
                    .post
                    .id,
            )
        }),
    );
    let mut i = 0;
    row(
        "post_show",
        stats(2000, || {
            post_show(
                &conn,
                &PostShowArgs {
                    id: ids[i % ids.len()].clone(),
                    version: None,
                },
            )
            .unwrap();
            i += 1;
        }),
    );
    let mut i = 0;
    row(
        "comment_new",
        stats(2000, || {
            let a = CommentNewArgs {
                post: Some(ids[i % ids.len()].clone()),
                session: None,
                text: "looks good".into(),
                anchor: None,
                author: Some("user".into()),
            };
            comment_new(&conn, &a, Some("claude")).unwrap();
            i += 1;
        }),
    );
    let mut i = 0;
    row(
        "post_update (title only, 1 unseen comment as feedback)",
        stats(2000, || {
            let a = PostUpdateArgs {
                id: ids[i % ids.len()].clone(),
                title: Some(format!("v{i}")),
                parts: None,
            };
            post_update(&conn, &a).unwrap();
            i += 1;
        }),
    );
    let mut i = 0;
    row(
        "post_part set (1 part, revalidate + rewrite)",
        stats(2000, || {
            let a = PostPartArgs {
                id: ids[i % ids.len()].clone(),
                op: PartOp::Set,
                target: Some("0".into()),
                part: Some(Part::Markdown {
                    markdown: format!("# v{i}"),
                    id: None,
                }),
            };
            post_part(&conn, &a).unwrap();
            i += 1;
        }),
    );
}
