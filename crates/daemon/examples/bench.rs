//! Socket latency per verb on a generated vault, then indexed-idle CPU and wakeups.
//!
//!     cargo build --release -p notesd --examples
//!     target/release/examples/bench [NOTES] [SAMPLES]
//!
//! Spawns target/release/notesd on /tmp/nd-bench; every sample is one fresh connection,
//! one request line and one response line, like a `notes` CLI call minus process startup.

use std::{
    fmt::Write as _,
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

const TOPICS: [&str; 10] = [
    "database", "meeting", "release", "research", "travel", "recipe", "garden", "invoice",
    "design", "review",
];
const WORDS: [&str; 24] = [
    "migration",
    "agenda",
    "checklist",
    "findings",
    "itinerary",
    "sourdough",
    "seedling",
    "audit",
    "latency",
    "budget",
    "contract",
    "backup",
    "launch",
    "retrospective",
    "observation",
    "borrow",
    "checker",
    "window",
    "kernel",
    "socket",
    "index",
    "vault",
    "folder",
    "draft",
];

fn generate(vault: &Path, notes: usize) -> u64 {
    let mut seed = 249u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut bytes = 0;
    for i in 0..notes {
        let topic = TOPICS[i % TOPICS.len()];
        let dir = vault.join(topic).join(format!("{topic}-{}", i % 5));
        fs::create_dir_all(&dir).unwrap();
        let mut body =
            format!("# {topic} note {i}\n\nnoteidentity{i:05}\n\n## Todo\n- item\n\n## Log\n");
        let target = 1024 + (next() % 7168) as usize;
        while body.len() < target {
            let line: Vec<&str> = (0..12)
                .map(|_| WORDS[(next() % WORDS.len() as u64) as usize])
                .collect();
            let _ = writeln!(
                body,
                "{} [[{topic}-{:04}]]",
                line.join(" "),
                next() % notes as u64
            );
        }
        bytes += body.len() as u64;
        fs::write(dir.join(format!("{topic}-{i:04}.md")), body).unwrap();
    }
    bytes
}

/// A verb and its args.
type Req = (&'static str, Value);
type Case = (&'static str, Box<dyn FnMut() -> Req>);

fn call(sock: &Path, verb: &str, args: Value) -> Value {
    let mut stream = UnixStream::connect(sock).unwrap();
    let mut req = json!({ "v": 1, "id": 1, "verb": verb, "agent": "bench" });
    if !args.is_null() {
        req["args"] = args;
    }
    stream.write_all(format!("{req}\n").as_bytes()).unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    let r: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(r["ok"], true, "{verb}: {r}");
    r["result"].clone()
}

fn pct(samples: &mut [f64], p: f64) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[((samples.len() as f64 * p).ceil() as usize).max(1) - 1]
}

struct Usage {
    at: Instant,
    cpu_ns: f64,
    interrupt_wakeups: u64,
    pkg_idle_wakeups: u64,
    resident: u64,
    footprint: u64,
    diskio: u64,
}

#[allow(deprecated)] // libc's mach_timebase_info; mach2 would be a new dependency
fn usage(pid: i32) -> Usage {
    let mut info: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V2, (&raw mut info).cast()) };
    assert_eq!(rc, 0, "proc_pid_rusage");
    let mut tb = libc::mach_timebase_info { numer: 0, denom: 0 };
    #[allow(deprecated)]
    unsafe {
        libc::mach_timebase_info(&mut tb)
    };
    let ns_per_tick = tb.numer as f64 / tb.denom as f64;
    Usage {
        at: Instant::now(),
        cpu_ns: (info.ri_user_time + info.ri_system_time) as f64 * ns_per_tick,
        interrupt_wakeups: info.ri_interrupt_wkups,
        pkg_idle_wakeups: info.ri_pkg_idle_wkups,
        resident: info.ri_resident_size,
        footprint: info.ri_phys_footprint,
        diskio: info.ri_diskio_bytesread + info.ri_diskio_byteswritten,
    }
}

fn idle_row(label: &str, pid: i32, seconds: u64) -> String {
    let before = usage(pid);
    thread::sleep(Duration::from_secs(seconds));
    let after = usage(pid);
    format!(
        "| {label} | {:.3} | {:.3} | {} | {} | {} | {} | {} |",
        after.at.duration_since(before.at).as_secs_f64(),
        (after.cpu_ns - before.cpu_ns) / 1e6,
        after.interrupt_wakeups - before.interrupt_wakeups,
        after.pkg_idle_wakeups - before.pkg_idle_wakeups,
        after.diskio - before.diskio,
        after.resident,
        after.footprint,
    )
}

fn main() {
    let mut args = std::env::args().skip(1);
    let notes: usize = args.next().map_or(1000, |s| s.parse().unwrap());
    let samples: usize = args.next().map_or(200, |s| s.parse().unwrap());
    let idle_secs = 30;
    let root = PathBuf::from("/tmp/nd-bench");
    let _ = fs::remove_dir_all(&root);
    let vault = root.join("vault");
    let bytes = generate(&vault, notes);
    let sock = root.join("s.sock");
    let exe = std::env::current_exe().unwrap();
    let notesd = exe.parent().unwrap().parent().unwrap().join("notesd");

    let started = Instant::now();
    let mut child = Command::new(&notesd)
        .env("NOTES_VAULT", &vault)
        .env("NOTES_DB", root.join("state/notes.db"))
        .env("NOTES_SOCK", &sock)
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    while UnixStream::connect(&sock).is_err() {
        thread::sleep(Duration::from_millis(1));
    }
    let doctor = call(&sock, "doctor", Value::Null);
    let ready_ms = started.elapsed().as_secs_f64() * 1e3;
    assert_eq!(doctor["notes"], notes as u64);

    let note = "database/database-0/database-0000.md";
    let mut n = 0u64;
    let mut cases: Vec<Case> = vec![
        ("doctor", Box::new(|| ("doctor", Value::Null))),
        ("manifest", Box::new(|| ("manifest", Value::Null))),
        ("list (root)", Box::new(|| ("list", json!({})))),
        (
            "list -r (1,000 notes)",
            Box::new(|| ("list", json!({ "recursive": true }))),
        ),
        (
            "find `release-plan`",
            Box::new(|| ("find", json!({ "query": "release-plan" }))),
        ),
        (
            "find typo `recipe-sourdugh`",
            Box::new(|| ("find", json!({ "query": "recipe-sourdugh" }))),
        ),
        (
            "search common `latency budget`",
            Box::new(|| ("search", json!({ "query": "latency budget" }))),
        ),
        (
            "search unique `noteidentity00777`",
            Box::new(|| ("search", json!({ "query": "noteidentity00777" }))),
        ),
        (
            "search miss `zzzzunfindable999`",
            Box::new(|| ("search", json!({ "query": "zzzzunfindable999" }))),
        ),
        (
            "search --fuzzy typo `sourdugh`",
            Box::new(|| {
                (
                    "search",
                    json!({ "query": "sourdugh", "fuzzy": true, "limit": 10 }),
                )
            }),
        ),
        ("read", Box::new(move || ("read", json!({ "note": note })))),
        (
            "read --lines 1:5",
            Box::new(move || {
                (
                    "read",
                    json!({ "note": note, "lines": { "start": 1, "end": 5 } }),
                )
            }),
        ),
        (
            "read bare title",
            Box::new(|| ("read", json!({ "note": "travel-0004" }))),
        ),
        (
            "history",
            Box::new(move || ("history", json!({ "note": note, "limit": 10 }))),
        ),
        (
            "new",
            Box::new(move || {
                n += 1;
                (
                    "new",
                    json!({ "path": format!("bench/new-{n}"), "body": "hello\n" }),
                )
            }),
        ),
        (
            "append",
            Box::new(move || ("append", json!({ "note": note, "text": "- appended" }))),
        ),
        (
            "append --under Todo",
            Box::new(move || {
                (
                    "append",
                    json!({ "note": note, "text": "- todo", "under": "Todo" }),
                )
            }),
        ),
        (
            "prepend",
            Box::new(move || ("prepend", json!({ "note": note, "text": "top" }))),
        ),
        (
            "edit (unique old)",
            Box::new(move || {
                n += 1;
                (
                    "edit",
                    json!({ "note": note, "edits": [{ "old": "noteidentity", "new": format!("noteidentity{n}x") }] }),
                )
            }),
        ),
        (
            "write --force",
            Box::new(move || {
                n += 1;
                (
                    "write",
                    json!({ "note": "bench/new-1", "body": format!("body {n}\n"), "force": true }),
                )
            }),
        ),
        (
            "diff (prev → current)",
            Box::new(move || ("diff", json!({ "note": note }))),
        ),
        (
            "revert --to 1",
            Box::new(move || ("revert", json!({ "note": "bench/new-1", "to": 1 }))),
        ),
        (
            "mkdir",
            Box::new(move || {
                n += 1;
                ("mkdir", json!({ "folder": format!("bench/dir-{n}") }))
            }),
        ),
        (
            "batch (3 reads, atomic)",
            Box::new(move || {
                let read = json!({ "verb": "read", "args": { "note": note } });
                (
                    "batch",
                    json!({ "atomic": true, "ops": [read, read, read] }),
                )
            }),
        ),
    ];
    let mut rows = Vec::new();
    let mut time = |label: &str, samples: usize, f: &mut dyn FnMut() -> (&'static str, Value)| {
        for _ in 0..5 {
            let (verb, args) = f();
            call(&sock, verb, args);
        }
        let mut ms: Vec<f64> = (0..samples)
            .map(|_| {
                let (verb, args) = f();
                let t = Instant::now();
                std::hint::black_box(call(&sock, verb, args));
                t.elapsed().as_secs_f64() * 1e3
            })
            .collect();
        rows.push(format!(
            "| {label} | {samples} | {:.3} | {:.3} | {:.3} |",
            pct(&mut ms, 0.5),
            pct(&mut ms, 0.95),
            pct(&mut ms, 1.0)
        ));
    };
    for (label, f) in &mut cases {
        time(label, samples, f.as_mut());
    }
    // Paired verbs: each sample is one call, alternating directions so state stays put.
    let mut flip = false;
    time(
        "move (rename + link rewrite scan)",
        samples.min(50),
        &mut || {
            flip = !flip;
            let (from, to) = if flip {
                ("bench/new-2", "bench/moved")
            } else {
                ("bench/moved", "bench/new-2")
            };
            ("move", json!({ "note": from, "to": to }))
        },
    );
    let mut flip = false;
    time("delete / restore (alternating)", samples, &mut || {
        flip = !flip;
        if flip {
            ("delete", json!({ "note": "bench/new-3" }))
        } else {
            ("restore", json!({ "note": "bench/new-3" }))
        }
    });
    let mut k = 0;
    time("rmdir", samples, &mut || {
        k += 1;
        ("rmdir", json!({ "folder": format!("bench/dir-{k}") }))
    });

    // Event delivery: append on one connection, time until a watch stream sees it.
    let mut watch = UnixStream::connect(&sock).unwrap();
    watch
        .write_all(
            b"{\"v\":1,\"id\":9,\"verb\":\"watch\",\"args\":{\"kinds\":[\"note-updated\"]}}\n",
        )
        .unwrap();
    let mut events = BufReader::new(watch.try_clone().unwrap());
    thread::sleep(Duration::from_millis(100));
    let mut ms: Vec<f64> = (0..samples)
        .map(|_| {
            let t = Instant::now();
            call(&sock, "append", json!({ "note": note, "text": "w" }));
            let mut line = String::new();
            events.read_line(&mut line).unwrap();
            t.elapsed().as_secs_f64() * 1e3
        })
        .collect();
    rows.push(format!(
        "| watch: append → event on an open stream | {samples} | {:.3} | {:.3} | {:.3} |",
        pct(&mut ms, 0.5),
        pct(&mut ms, 0.95),
        pct(&mut ms, 1.0)
    ));

    println!(
        "vault: {notes} notes, {bytes} bytes; ready (spawn → first doctor reply) {ready_ms:.1} ms\n"
    );
    println!("| Verb | Samples | p50 ms | p95 ms | max ms |\n|---|---:|---:|---:|---:|");
    for row in &rows {
        println!("{row}");
    }

    // Idle: let the watcher settle after our own writes, then measure with nobody talking.
    let pid = child.id() as i32;
    thread::sleep(Duration::from_secs(3));
    println!(
        "\n| Idle window | Seconds | CPU ms | Interrupt wakeups | Pkg-idle wakeups | Disk bytes | RSS at end | Footprint at end |"
    );
    println!("|---|---:|---:|---:|---:|---:|---:|---:|");
    println!(
        "{}",
        idle_row("watch stream open, no traffic", pid, idle_secs)
    );
    drop(events);
    drop(watch);
    thread::sleep(Duration::from_secs(1));
    println!("{}", idle_row("no clients", pid, idle_secs));
    let threads = Command::new("ps")
        .args(["-M", "-p", &pid.to_string()])
        .output()
        .unwrap();
    println!(
        "\nps -M (threads at end):\n{}",
        String::from_utf8_lossy(&threads.stdout)
    );

    child.kill().unwrap();
    child.wait().unwrap();
    let _ = fs::remove_dir_all(&root);
}
