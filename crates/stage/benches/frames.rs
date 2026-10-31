//! Frame budget harness: runs the real `stage` binary in a pty against a temp
//! notesd seeded with one post of each text kind, plays a fixed scenario, and
//! measures what the terminal would see. `cargo bench -p stage --bench frames`.
//!
//! Measured from outside the process: bytes written per input (by kind), input
//! → end-of-paint latency (p50/p95, includes notesd round trips), frames per
//! paste, resize → repaint, time to first draw, and idle wakeups (macOS
//! `proc_pid_rusage`). Exits non-zero when a budget is broken.

use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::{fd::FromRawFd, unix::net::UnixStream, unix::process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use notes_proto::{
    posts::{Part, PostNewArgs, PostWriteResult, SessionId},
    to_line, Request, Response, Verb, PROTOCOL_VERSION,
};

const TIOCSWINSZ: libc::c_ulong = 0x8008_7467;
const TIOCSCTTY: libc::c_ulong = 0x2000_7461;
const SYNC: &[u8] = b"\x1b[?2026h";

// Budgets (plan v8 targets; scroll has no margin-scroll yet, so it's report-only).
const RESIZE_MS: f64 = 50.0;
const PASTE_FRAMES: usize = 1;
const IDLE_WAKEUPS: u64 = 0;
const INPUT_P95_MS: f64 = 16.0;
const NATIVE_WHEEL_KIB: f64 = 1.0;
const RESIZE_BURST_FRAMES: usize = 121; // At most one frame per size plus a pending settle frame.

fn notesd_bin() -> Option<PathBuf> {
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
    ["release/notesd", "debug/notesd"].iter().map(|p| target.join(p)).find(|p| p.exists())
}

struct Proc(Child);
impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn agent(sock: &Path, verb: Verb) -> PostWriteResult {
    let mut s = UnixStream::connect(sock).unwrap();
    let req = Request { v: PROTOCOL_VERSION, id: 1, verb, cwd: Some("/bench".into()), agent: Some("bench".into()) };
    s.write_all(to_line(&req).as_bytes()).unwrap();
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).unwrap();
    match serde_json::from_str(&line).unwrap() {
        Response::Ok { result, .. } => serde_json::from_value(result).unwrap(),
        other => panic!("{other:?}"),
    }
}

fn seed(sock: &Path) -> SessionId {
    let md: String = (0..120).map(|i| format!("## Section {i}\n\nSome *prose* with `code` and a [link](http://x/{i}).\n\n")).collect();
    let code: String = (0..300).map(|i| format!("fn f{i}(x: u32) -> u32 {{ x + {i} }}\n")).collect();
    let patch = "--- a/x.rs\n+++ b/x.rs\n@@ -1,3 +1,3 @@\n fn a() {}\n-fn b() {}\n+fn b() { 1 }\n fn c() {}\n";
    let json = serde_json::json!({"users": (0..30).map(|i| serde_json::json!({"id": i, "tags": ["a", "b"]})).collect::<Vec<_>>()});
    let parts = [
        Part::Markdown { markdown: md, id: None },
        Part::Code { code, language: Some("rust".into()), title: None, line_start: None, id: None },
        Part::Diff { patch: Some(patch.into()), files: vec![], layout: Default::default(), id: None },
        Part::Terminal { text: "\x1b[32mok\x1b[0m done\n".repeat(50), cols: None, title: None, id: None },
        Part::Json { data: json, id: None },
    ];
    let mut session = None;
    for (i, part) in parts.into_iter().enumerate() {
        let r = agent(sock, Verb::PostNew(PostNewArgs { session: session.clone(), title: Some(format!("post {i}")), parts: vec![part], ..Default::default() }));
        session = Some(r.post.session);
    }
    session.unwrap()
}

struct Pty {
    master: fs::File,
    fd: i32,
}

impl Pty {
    fn read_burst(&mut self, quiet: Duration) -> (Vec<u8>, Option<Duration>) {
        let start = Instant::now();
        let mut out = Vec::new();
        let mut last = None;
        loop {
            let mut pfd = libc::pollfd { fd: self.fd, events: libc::POLLIN, revents: 0 };
            // SAFETY: one valid pollfd.
            if unsafe { libc::poll(&mut pfd, 1, quiet.as_millis() as i32) } <= 0 {
                return (out, last);
            }
            let mut buf = [0u8; 65536];
            match std::io::Read::read(&mut self.master, &mut buf) {
                Ok(0) | Err(_) => return (out, last),
                Ok(n) => {
                    out.extend_from_slice(&buf[..n]);
                    last = Some(start.elapsed());
                }
            }
        }
    }

    fn input(&mut self, bytes: &[u8]) -> (usize, f64, usize) {
        self.master.write_all(bytes).unwrap();
        let (out, last) = self.read_burst(Duration::from_millis(100));
        let frames = out.windows(SYNC.len()).filter(|w| *w == SYNC).count();
        (out.len(), last.map_or(0.0, |d| d.as_secs_f64() * 1000.0), frames)
    }

    fn resize(&self, cols: u16, rows: u16) {
        let ws = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
        // SAFETY: TIOCSWINSZ with a valid winsize; the kernel sends SIGWINCH to the pty's group.
        unsafe { libc::ioctl(self.fd, TIOCSWINSZ, &ws) };
    }
}

fn wakeups(pid: i32) -> u64 {
    let mut info: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
    // SAFETY: rusage_info_v2 buffer for RUSAGE_INFO_V2.
    let ok = unsafe { libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V2, (&mut info as *mut libc::rusage_info_v2).cast()) };
    assert_eq!(ok, 0, "proc_pid_rusage");
    info.ri_pkg_idle_wkups + info.ri_interrupt_wkups
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v.get(((v.len() as f64 - 1.0) * p).round() as usize).copied().unwrap_or(0.0)
}

fn main() {
    let Some(bin) = notesd_bin() else {
        eprintln!("SKIP: build notesd first (`cargo build -p notesd`)");
        std::process::exit(0);
    };
    let dir = PathBuf::from(format!("/tmp/stb-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("vault")).unwrap();
    fs::create_dir_all(dir.join("home")).unwrap();
    let sock = dir.join("s.sock");
    let env = [
        ("NOTES_VAULT", dir.join("vault")),
        ("NOTES_DB", dir.join("state/notes.db")),
        ("NOTES_SOCK", sock.clone()),
        ("HOME", dir.join("home")),
        ("NOTES_STAGE_DIR", dir.join("sessions")),
    ];
    let _notesd = Proc(Command::new(bin).envs(env.clone()).stderr(Stdio::null()).spawn().unwrap());
    let start = Instant::now();
    while UnixStream::connect(&sock).is_err() {
        assert!(start.elapsed() < Duration::from_secs(30));
        thread::sleep(Duration::from_millis(10));
    }
    let session = seed(&sock);

    let (mut master, mut slave) = (0, 0);
    let ws = libc::winsize { ws_row: 40, ws_col: 120, ws_xpixel: 0, ws_ypixel: 0 };
    // SAFETY: openpty fills two fds.
    assert_eq!(unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), &ws as *const _ as *mut _) }, 0);
    let stage_bin = std::env::var_os("STAGE_BENCH_BIN").unwrap_or_else(|| env!("CARGO_BIN_EXE_stage").into());
    let mut cmd = Command::new(stage_bin);
    cmd.args(["--session", session.as_str()]).envs(env).env("STAGE_HELPER", "/nonexistent");
    // SAFETY: the slave fd is valid; each Stdio takes its own dup.
    unsafe {
        cmd.stdin(Stdio::from_raw_fd(libc::dup(slave)))
            .stdout(Stdio::from_raw_fd(libc::dup(slave)))
            .stderr(Stdio::from_raw_fd(libc::dup(slave)))
            .pre_exec(|| {
                libc::setsid();
                libc::ioctl(0, TIOCSCTTY, 0);
                Ok(())
            });
    }
    let spawned = Instant::now();
    let stage = Proc(cmd.spawn().unwrap());
    // SAFETY: we own the master fd from here on.
    let mut pty = Pty { master: unsafe { fs::File::from_raw_fd(master) }, fd: master };
    unsafe { libc::close(slave) };

    // Time to first draw: the first synchronized frame.
    let mut seen = Vec::new();
    let first_draw = loop {
        let (out, _) = pty.read_burst(Duration::from_millis(5));
        seen.extend(out);
        if seen.windows(SYNC.len()).any(|w| w == SYNC) {
            break spawned.elapsed().as_secs_f64() * 1000.0;
        }
        assert!(spawned.elapsed() < Duration::from_secs(10), "stage never drew");
    };
    // Act like a terminal: end capability probing with DA1. Otherwise the
    // two-second probe swallows the scenario's first keys as terminal replies.
    pty.master.write_all(b"\x1b[?1;2c").unwrap();
    pty.read_burst(Duration::from_millis(300)); // bootstrap settles

    // Matched scroll cohort: start at the first markdown post, with the sidebar
    // visible. Every tick moves three rows; no post switching or edge no-ops.
    pty.input(b"g");
    let mut wheel_ms = Vec::new();
    let mut wheel_bytes = 0;
    for _ in 0..30 {
        let (bytes, ms, _) = pty.input(b"\x1b[<65;80;20M");
        assert!(bytes > 0, "scroll produced no output within 100 ms (stall or fixture edge)");
        wheel_bytes += bytes;
        wheel_ms.push(ms);
    }
    let wheel_p95 = pct(&mut wheel_ms, 0.95);
    pty.input(b"g");
    let (wheel_burst_bytes, _, wheel_burst_frames) = pty.input(&b"\x1b[<65;80;20M".repeat(120));
    pty.input(b"G");

    let mut rows: Vec<(&str, usize, f64)> = Vec::new();
    let run = |pty: &mut Pty, kind: &'static str, bytes: &[u8], n: usize, rows: &mut Vec<(&str, usize, f64)>| {
        for _ in 0..n {
            let (b, ms, _) = pty.input(bytes);
            rows.push((kind, b, ms));
        }
    };
    // Shown first: the json post (the latest). Cursor onto its root, then fold.
    run(&mut pty, "json move", b"\x1b[B", 1, &mut rows);
    run(&mut pty, "json fold", b"\r", 10, &mut rows);
    run(&mut pty, "post switch", b"k", 4, &mut rows); // to the markdown post
    run(&mut pty, "scroll tick", b"\x1b[<65;80;20M", 30, &mut rows);
    run(&mut pty, "line move", b"\x1b[B", 30, &mut rows);
    for _ in 0..10 {
        run(&mut pty, "post switch", b"j", 1, &mut rows);
        run(&mut pty, "post switch", b"k", 1, &mut rows);
    }
    run(&mut pty, "search key", b"/", 1, &mut rows);
    run(&mut pty, "search key", b"post", 1, &mut rows);
    run(&mut pty, "search key", b"\x1b", 1, &mut rows);

    // A 240-char paste into the comment box is one frame.
    pty.input(b"c");
    let paste = [b"\x1b[200~".as_slice(), &[b'x'; 240], b"\x1b[201~"].concat();
    let (_, _, paste_frames) = pty.input(&paste);
    pty.input(b"\x1b");

    // Resize → repaint.
    pty.read_burst(Duration::from_millis(100));
    pty.resize(100, 30);
    let (_, last) = pty.read_burst(Duration::from_millis(100));
    let resize_ms = last.map_or(f64::INFINITY, |d| d.as_secs_f64() * 1000.0);

    // Isolated resizes should remain immediate, rather than paying a debounce
    // delay on every event. Measure input to the last output byte (not GPU paint).
    let mut isolated_resize_ms = Vec::new();
    for width in (0..20).map(|i| 100 + (i + 1) % 2) {
        let start = Instant::now();
        pty.resize(width, 30);
        let read_start = Instant::now();
        let (_, last) = pty.read_burst(Duration::from_millis(30));
        let last = last.expect("every isolated resize must produce a frame");
        isolated_resize_ms.push((read_start + last - start).as_secs_f64() * 1000.0);
    }

    // Open-loop drag: keep issuing sizes while the reader drains Stage output.
    // Report generator lateness so a slow sender cannot masquerade as batching.
    let fd = pty.fd;
    let sender = thread::spawn(move || {
        let start = Instant::now();
        let mut last_resize = start;
        let mut lateness = Duration::ZERO;
        for i in 0..120 {
            let scheduled = start + Duration::from_secs_f64(i as f64 / 240.0);
            thread::sleep(scheduled.saturating_duration_since(Instant::now()));
            last_resize = Instant::now();
            lateness = lateness.max(last_resize.saturating_duration_since(scheduled));
            let (cols, rows) = if i == 119 { (100, 30) } else { (80 + i % 40, 40) };
            let ws = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
            // SAFETY: the main thread owns this fd until this sender joins.
            assert_eq!(unsafe { libc::ioctl(fd, TIOCSWINSZ, &ws) }, 0);
        }
        (last_resize, lateness)
    });
    let read_start = Instant::now();
    let (burst_out, last) = pty.read_burst(Duration::from_millis(100));
    let (last_resize, lateness) = sender.join().unwrap();
    let burst_frames = burst_out.windows(SYNC.len()).filter(|w| *w == SYNC).count();
    let burst_tail_ms = last.map_or(f64::INFINITY, |last|
        (read_start + last).saturating_duration_since(last_resize).as_secs_f64() * 1000.0);
    let isolated_p95 = pct(&mut isolated_resize_ms, 0.95);

    // Idle: nothing happens for 3 s.
    pty.read_burst(Duration::from_millis(200));
    let pid = stage.0.id() as i32;
    let before = wakeups(pid);
    let (idle_out, _) = pty.read_burst(Duration::from_secs(3));
    let idle = wakeups(pid) - before;

    println!("stage frame bench ({} inputs)", rows.len() + 3);
    println!("  matched sidebar wheel      {:.2} KiB/tick / {wheel_p95:.2} ms output p95 (30 ticks)", wheel_bytes as f64 / 30.0 / 1024.0);
    println!("  queued 120-tick burst      {wheel_burst_frames:7} frames / {:.1} KiB", wheel_burst_bytes as f64 / 1024.0);
    println!("  time to first draw        {first_draw:7.1} ms");
    let mut kinds: Vec<&str> = rows.iter().map(|r| r.0).collect();
    kinds.dedup();
    kinds.sort();
    kinds.dedup();
    for kind in &kinds {
        let r: Vec<_> = rows.iter().filter(|r| r.0 == *kind).collect();
        let kib = r.iter().map(|r| r.1).sum::<usize>() as f64 / r.len() as f64 / 1024.0;
        let mut kind_ms: Vec<f64> = r.iter().map(|r| r.2).collect();
        println!("  KiB per {kind:<17} {kib:7.2}   output p95 {:6.2} ms", pct(&mut kind_ms, 0.95));
    }
    let mut ms: Vec<f64> = rows.iter().map(|r| r.2).collect();
    let (p50, p95) = (pct(&mut ms, 0.5), pct(&mut ms, 0.95));
    println!("  input → paint p50 / p95   {p50:7.1} / {p95:.1} ms");
    println!("  frames per 240-key paste  {paste_frames:7}");
    println!("  resize → repaint          {resize_ms:7.1} ms");
    println!("  isolated resize p95        {isolated_p95:7.2} ms ({} samples)", isolated_resize_ms.len());
    println!("  240 Hz resize burst        {burst_frames:7} frames / {:.1} KiB", burst_out.len() as f64 / 1024.0);
    println!("  burst final resize → output {burst_tail_ms:6.2} ms (sender max late {:.2} ms)", lateness.as_secs_f64() * 1000.0);
    println!("  idle wakeups (3 s)        {idle:7}   (bytes written {})", idle_out.len());

    let mut failed = Vec::new();
    if idle > IDLE_WAKEUPS || !idle_out.is_empty() {
        failed.push(format!("idle wakeups {idle} > {IDLE_WAKEUPS}"));
    }
    if paste_frames > PASTE_FRAMES {
        failed.push(format!("paste took {paste_frames} frames"));
    }
    if burst_frames == 0 || burst_frames > RESIZE_BURST_FRAMES {
        failed.push(format!("resize burst took {burst_frames} frames (budget 1..={RESIZE_BURST_FRAMES})"));
    }
    if burst_tail_ms > RESIZE_MS || isolated_p95 > INPUT_P95_MS {
        failed.push(format!("resize latency: isolated p95 {isolated_p95:.2} ms, burst tail {burst_tail_ms:.2} ms"));
    }
    if resize_ms > RESIZE_MS {
        failed.push(format!("resize repaint {resize_ms:.1} ms > {RESIZE_MS}"));
    }
    let rex_native_scroll = std::env::var("TERM_PROGRAM").is_ok_and(|name| name == "rex")
        && std::env::var_os("STAGE_NO_SCROLL_REGION").is_none();
    let wheel_kib = wheel_bytes as f64 / 30.0 / 1024.0;
    if rex_native_scroll && wheel_kib > NATIVE_WHEEL_KIB {
        failed.push(format!("native wheel output {wheel_kib:.2} KiB/tick > {NATIVE_WHEEL_KIB}"));
    }
    if wheel_burst_frames == 0 || wheel_burst_frames > 8 {
        failed.push(format!("queued wheel burst took {wheel_burst_frames} frames (budget 1..=8)"));
    }
    if wheel_p95 > INPUT_P95_MS {
        failed.push(format!("wheel p95 {wheel_p95:.1} ms > {INPUT_P95_MS}"));
    }
    if p95 > INPUT_P95_MS {
        failed.push(format!("input p95 {p95:.1} ms > {INPUT_P95_MS}"));
    }
    drop(stage);
    drop(_notesd);
    // Only this benchmark's isolated shared helper; do not leave it for its idle timeout.
    let _ = Command::new("pkill").arg("-f").arg(dir.to_string_lossy().as_ref()).status();
    let _ = fs::remove_dir_all(&dir);
    if !failed.is_empty() {
        eprintln!("FAILED: {}", failed.join("; "));
        std::process::exit(1);
    }
}
