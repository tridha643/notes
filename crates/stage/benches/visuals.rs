//! Visual pipeline latency: request → pixels transmitted, per kind, through
//! the real gallery and stage-helper (no terminal). `cold` starts a fresh
//! helper each run; `warm` reuses one. Fixtures: benches/fixtures.
//!
//!   cargo build -p notesd && (cd apps/Notes && swift build -c release --product stage-helper)
//!   cargo bench -p stage --bench visuals

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

use stage::{
    app::{Msg, Target, VisualKey},
    cards::VisualSource,
    client::StageClient,
    gallery::Gallery,
};

struct Proc(Child);
impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * p).round() as usize]
}

/// One round: ask for all three kinds at once (as stage does on open), wait for each.
fn round(g: &mut Gallery, rx: &mpsc::Receiver<Msg>, n: u64, asset: &str, fx: &Path) -> HashMap<&'static str, f64> {
    let target = Target { width_px: 66 * 14, max_height_px: 18 * 30 };
    let kinds: [(&str, VisualSource); 3] = [
        ("mermaid", VisualSource::Mermaid(std::fs::read_to_string(fx.join("flow.mmd")).unwrap())),
        ("image", VisualSource::Asset(asset.into())),
        ("html", VisualSource::Html(std::fs::read_to_string(fx.join("anim.html")).unwrap())),
    ];
    let mut asked: HashMap<VisualKey, (&'static str, Instant)> = HashMap::new();
    let mut done_now: HashMap<&'static str, f64> = HashMap::new();
    for (i, (kind, source)) in kinds.into_iter().enumerate() {
        let key: VisualKey = (format!("bench-{kind}").into(), n, i);
        asked.insert(key.clone(), (kind, Instant::now()));
        let mut sink = Vec::new();
        for m in g.need(&mut sink, key.clone(), source, target) {
            match m {
                Msg::VisualReady { .. } => {
                    done_now.insert(kind, asked[&key].1.elapsed().as_secs_f64() * 1000.0);
                }
                _ => panic!("{kind} failed to start"),
            }
        }
    }
    let mut done = done_now;
    let mut sink = Vec::new();
    while done.len() < 3 {
        let msg = rx.recv_timeout(Duration::from_secs(20)).expect("a visual within 20 s");
        let Msg::Gfx(gfx) = msg else { continue };
        for m in g.handle(&mut sink, gfx) {
            match m {
                Msg::VisualReady { key, .. } => {
                    if let Some((kind, at)) = asked.get(&key) {
                        done.entry(*kind).or_insert_with(|| at.elapsed().as_secs_f64() * 1000.0);
                    }
                }
                Msg::VisualFailed { reason, .. } => panic!("visual failed: {reason}"),
                _ => {}
            }
        }
        sink.clear();
    }
    // Nothing is on screen in the bench: pages pause, as off-screen pages do in stage.
    g.place(&mut sink, &[]);
    done
}

fn main() {
    let target = root().join("target");
    let Some(notesd) = ["release/notesd", "debug/notesd"].iter().map(|p| target.join(p)).find(|p| p.exists()) else {
        eprintln!("SKIP: build notesd first");
        return;
    };
    let helper = root().join("apps/Notes/.build/out/Products/Release/stage-helper");
    if !helper.exists() {
        eprintln!("SKIP: build stage-helper first (swift build -c release --product stage-helper)");
        return;
    }
    // SAFETY: single-threaded here; the gallery reads it when it spawns the helper.
    unsafe { std::env::set_var("STAGE_HELPER", &helper) };
    let dir = PathBuf::from(format!("/tmp/stv-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("vault")).unwrap();
    let sock = dir.join("s.sock");
    let _notesd = Proc(
        Command::new(notesd)
            .env("NOTES_VAULT", dir.join("vault"))
            .env("NOTES_DB", dir.join("state/notes.db"))
            .env("NOTES_SOCK", &sock)
            .env("HOME", dir.join("home"))
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    while UnixStream::connect(&sock).is_err() {
        std::thread::sleep(Duration::from_millis(10));
    }
    let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("benches/fixtures");
    let png = std::fs::read(fx.join("hub.png")).unwrap();
    use base64::{engine::general_purpose::STANDARD, Engine};
    let mut s = UnixStream::connect(&sock).unwrap();
    let req = serde_json::json!({"v":1,"id":1,"verb":"asset-put","args":{"kind":"image","content_type":"image/png","data_base64":STANDARD.encode(&png)}});
    s.write_all(format!("{req}\n").as_bytes()).unwrap();
    let mut line = String::new();
    BufReader::new(&s).read_line(&mut line).unwrap();
    let asset = serde_json::from_str::<serde_json::Value>(&line).unwrap()["result"]["id"].as_str().unwrap().to_owned();

    let mut results: HashMap<(&str, &str), Vec<f64>> = HashMap::new();
    let cold_runs = 5;
    for n in 0..cold_runs {
        let (tx, rx) = mpsc::channel();
        // A cache folder of its own: no helper is listening there yet, so this is a true cold start.
        let mut g = Gallery::new(tx, StageClient::new(sock.clone()), dir.join(format!("cold-{n}")), dir.join("home/Desktop/Stage"), Target { width_px: 924, max_height_px: 540 });
        // As stage does after its first frame; the requests follow at once (worst case).
        g.warm();
        for (kind, ms) in round(&mut g, &rx, n, &asset, &fx) {
            results.entry(("cold", kind)).or_default().push(ms);
        }
    }
    let (tx, rx) = mpsc::channel();
    let mut g = Gallery::new(tx, StageClient::new(sock.clone()), dir.join("cache"), dir.join("home/Desktop/Stage"), Target { width_px: 924, max_height_px: 540 });
    round(&mut g, &rx, 100, &asset, &fx);
    for n in 0..10 {
        for (kind, ms) in round(&mut g, &rx, 200 + n, &asset, &fx) {
            results.entry(("warm", kind)).or_default().push(ms);
        }
    }
    // A stage that starts while the shared helper is already up (every start but the first).
    for n in 0..5 {
        let (tx, rx) = mpsc::channel();
        let mut g = Gallery::new(tx, StageClient::new(sock.clone()), dir.join("cache"), dir.join("home/Desktop/Stage"), Target { width_px: 924, max_height_px: 540 });
        g.warm();
        for (kind, ms) in round(&mut g, &rx, 300 + n, &asset, &fx) {
            results.entry(("restart", kind)).or_default().push(ms);
        }
    }
    println!("visual request → transmitted (ms)        p50      p95");
    for state in ["cold", "restart", "warm"] {
        for kind in ["mermaid", "image", "html"] {
            let v = results.get_mut(&(state, kind)).unwrap();
            println!("  {state:<7} {kind:<8}                    {:7.1}  {:7.1}", pct(v, 0.5), pct(v, 0.95));
        }
    }
    let _ = Command::new("pkill").arg("-f").arg(dir.to_string_lossy().as_ref()).status();
    let _ = std::fs::remove_dir_all(&dir);
}
