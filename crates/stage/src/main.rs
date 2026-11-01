//! `stage [--session S]`: terminal setup, the probe, the event loop.
//!
//! Three blocking sources feed one channel: crossterm's reader (keys, mouse,
//! SIGWINCH resizes), the notesd watch stream, and the request worker, which
//! owns the `StageClient` and answers each `Effect::Request` with a `Msg::Reply`,
//! so a slow render never holds a key or a scroll. The loop blocks on the
//! channel, drains queued input within a 4 ms work budget, then draws once.
//! Idle never wakes; output is flushed once per frame.

use std::{
    io::{self, Write},
    path::PathBuf,
    process::{Command, ExitCode},
    sync::mpsc,
    thread,
    time::Duration,
};

use ratatui::{
    backend::CrosstermBackend,
    crossterm::{
        cursor::{Hide, Show},
        event::{self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseEventKind},
        execute,
        terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    },
    Terminal,
};
use stage::{
    app::{App, Effect, Msg, StageRequest},
    client::{next_request, StageClient, Watch},
    gallery::Gallery,
    kitty,
    latency::Latency,
    probe,
    screen::Screen,
};

fn restore() {
    let mut out = io::stdout();
    // Delete any kitty placements we made, leave the alt screen, give the shell back.
    let _ = out.write_all(b"\x1b_Ga=d,d=a\x1b\\");
    let _ = execute!(out, DisableMouseCapture, DisableBracketedPaste, LeaveAlternateScreen, Show);
    let _ = disable_raw_mode();
}

/// The most one turn spends taking further messages before it paints, so a steady
/// stream of events can't hold a frame back.
const TURN: Duration = Duration::from_millis(4);
/// How long after the last scroll live pages start moving again.
const SCROLL_SETTLE: Duration = Duration::from_millis(150);

fn is_scroll(msg: &Msg) -> bool {
    match msg {
        Msg::Wheel(..) => true,
        Msg::Key(k) => match k.code {
            KeyCode::Char('d' | 'u') => k.modifiers.contains(KeyModifiers::CONTROL),
            code => matches!(code, KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown | KeyCode::Char(' ' | 'b' | 'j' | 'k')),
        },
        _ => false,
    }
}

fn usage() -> ExitCode {
    eprintln!("usage: stage [--session <id>]\n\nShows sideshow sessions from notesd (STAGE_LATENCY_LOG=<file> logs paint times).");
    ExitCode::from(2)
}

/// OSC 52: put `text` on the system clipboard through the terminal.
fn osc52(text: &str) -> Vec<u8> {
    use base64_lite::encode;
    format!("\x1b]52;c;{}\x07", encode(text.as_bytes())).into_bytes()
}

mod base64_lite {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    pub fn encode(b: &[u8]) -> String {
        let mut s = String::with_capacity(b.len().div_ceil(3) * 4);
        for c in b.chunks(3) {
            let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
            for i in 0..4 {
                s.push(if i <= c.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
            }
        }
        s
    }
}

fn cache_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/".into());
    home.join("Library/Caches/stage")
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut want = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--session" | "-s" => match args.next() {
                Some(s) => want = Some(s.into()),
                None => return usage(),
            },
            _ => return usage(),
        }
    }
    let sock = notes_proto::default_socket_path();
    let (tx, rx) = mpsc::channel::<Msg>();

    let mut out = io::stdout();
    if enable_raw_mode().is_err() || execute!(out, EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste, Hide).is_err() {
        restore();
        eprintln!("stage: needs a terminal");
        return ExitCode::from(1);
    }
    // crossterm enables all-motion (1003), but Stage only uses clicks, drags
    // and wheels. Button-motion avoids streaming idle pointer movement across
    // a remote/mobile connection while preserving selection and touch scrolling.
    let _ = out.write_all(b"\x1b[?1003l\x1b[?1002h");
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        hook(info);
    }));
    let Ok(mut term) = Terminal::new(CrosstermBackend::new(kitty::Out::default())) else {
        restore();
        return ExitCode::from(1);
    };
    let mut app = App::new(want);
    if let Ok(size) = term.size() {
        app.size = (size.width, size.height);
    }
    let mut latency = Latency::from_env();
    let mut gallery = Gallery::new(tx.clone(), StageClient::new(sock.clone()), cache_dir(), notes_proto::default_stage_path(), app.target());
    let mut screen = Screen::default();
    let mut draw = |term: &mut Terminal<CrosstermBackend<kitty::Out>>, app: &mut App, gallery: &mut Gallery, screen: &mut Screen| {
        let _ = term.backend_mut().write_all(b"\x1b[?2026h");
        if screen.draw(term, app).unwrap_or(false) {
            gallery.unplace_all(term.backend_mut());
        }
        let focused = app.focused_visual();
        gallery.view(app.zoom_view);
        gallery.retarget(app.target(), focused.as_ref());
        gallery.place(term.backend_mut(), &app.on_screen);
        let _ = term.backend_mut().write_all(b"\x1b[?2026l");
        let _ = term.backend_mut().flush();
        latency.painted(&app.take_fresh());
    };

    // Draw first, probe after: the queries go out, the first frame paints, and
    // only then do we wait for the replies. The wait ends at the DA1 reply every
    // terminal sends; a busy terminal can take well over 100 ms, and missing the
    // replies would leave stage without graphics for the whole run.
    let _ = probe::send(&mut io::stdout());
    draw(&mut term, &mut app, &mut gallery, &mut screen);
    gallery.warm();
    let p = probe::read(0, Duration::from_secs(2));
    app.update(Msg::Probed { cell_px: p.cell_px, kitty: p.kitty });

    // After the first frame (connecting can wait on launchd starting notesd): the watch
    // starts at the seq notesd had committed before bootstrap lists anything,
    // so nothing published in between is missed (duplicates are deduped by version).
    let doctor = StageClient::new(sock.clone()).doctor().ok();
    if let Some(path) = doctor.as_ref().and_then(|d| d.stage.as_ref())
        .and_then(|stage| stage.get("path")).and_then(|path| path.as_str()) {
        gallery.set_stage_path(PathBuf::from(path));
    }
    let start = doctor.map(|d| d.seq.unwrap_or(0));
    let first = Watch::open(&sock, start);
    {
        let (tx, sock) = (tx.clone(), sock.clone());
        thread::spawn(move || {
            let mut watch = first.ok();
            let mut since = start;
            loop {
                if let Some(w) = &mut watch {
                    while let Some(e) = w.next_event() {
                        since = Some(e.seq);
                        if tx.send(Msg::Event(e)).is_err() {
                            return;
                        }
                    }
                }
                let _ = tx.send(Msg::WatchLost);
                // Only while notesd is gone: retry, backing off to 2 s.
                let mut wait = Duration::from_millis(100);
                watch = loop {
                    thread::sleep(wait);
                    if let Ok(w) = Watch::open(&sock, since) {
                        break Some(w);
                    }
                    wait = (wait * 2).min(Duration::from_secs(2));
                };
                if tx.send(Msg::WatchBack).is_err() {
                    return;
                }
            }
        });
    }

    {
        let tx = tx.clone();
        thread::spawn(move || {
            while let Ok(ev) = event::read() {
                let msgs = match ev {
                    TermEvent::Key(k) => vec![Msg::Key(k)],
                    // Stamped here: wheel acceleration needs arrival times, not batch times.
                    TermEvent::Mouse(m) if matches!(m.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown) => vec![Msg::Wheel(m, std::time::Instant::now())],
                    TermEvent::Mouse(m) => vec![Msg::Mouse(m)],
                    // A pane can start at a placeholder size (Rex: 80x24, 8x16 px cells)
                    // and get its real one in the first resize, so re-measure cells too.
                    TermEvent::Resize(w, h) => [Msg::Resize(w, h)].into_iter().chain(cell_px().map(Msg::CellPx)).collect(),
                    TermEvent::Paste(s) => s
                        .chars()
                        .map(|c| {
                            let code = if c == '\n' || c == '\r' { KeyCode::Char(' ') } else { KeyCode::Char(c) };
                            Msg::Key(KeyEvent { code, modifiers: KeyModifiers::NONE, kind: KeyEventKind::Press, state: KeyEventState::NONE })
                        })
                        .collect(),
                    _ => vec![],
                };
                for m in msgs {
                    if tx.send(m).is_err() {
                        return;
                    }
                }
            }
        });
    }
    // A resize while probing/warming the helper can precede crossterm's
    // SIGWINCH reader. Never bootstrap from the placeholder size used for the
    // first frame (Rex initially reports 80x24). Read it again after starting
    // input, and report this size through the loop, not the stale first one.
    if let Ok(size) = term.size() {
        app.size = (size.width, size.height);
        let _ = tx.send(Msg::Resize(size.width, size.height));
    }
    if let Some(px) = cell_px() {
        app.cell_px = px;
        let _ = tx.send(Msg::CellPx(px));
    }
    // Requests run on their own thread, renders last (`next_request`); each answer is one more message.
    let (req_tx, req_rx) = mpsc::channel::<StageRequest>();
    {
        let tx = tx.clone();
        thread::spawn(move || {
            let mut client = StageClient::new(sock);
            let mut queue = Vec::new();
            loop {
                if queue.is_empty() {
                    let Ok(req) = req_rx.recv() else { return };
                    queue.push(req);
                }
                queue.extend(req_rx.try_iter());
                let req = next_request(&mut queue).expect("the queue holds a request");
                if tx.send(Msg::Reply(client.perform(&req))).is_err() {
                    return;
                }
            }
        });
    }
    exit_on_hangup();
    drop(tx);

    let mut scrolled_at: Option<std::time::Instant> = None;
    // `STAGE_LOOP_TRACE=<file>`: per loop turn, time spent updating and drawing.
    let mut loop_trace = std::env::var_os("STAGE_LOOP_TRACE").and_then(|p| std::fs::File::create(p).ok());
    let trace_start = std::time::Instant::now();
    let mut pending = app.start();
    loop {
        let mut redraw = false;
        let mut later = Vec::new();
        for effect in pending.drain(..) {
            match effect {
                Effect::Redraw => redraw = true,
                Effect::Quit => {
                    gallery.clear(term.backend_mut());
                    let _ = term.backend_mut().flush();
                    restore();
                    return ExitCode::SUCCESS;
                }
                Effect::PrefetchZoom { key, target } => {
                    if let Some(m) = gallery.prefetch_zoom(key, target) {
                        later.extend(app.update(m));
                    }
                }
                Effect::NeedVisual { key, source } => {
                    for m in gallery.need(term.backend_mut(), key, source) {
                        later.extend(app.update(m));
                    }
                }
                Effect::Fatal(m) => {
                    restore();
                    eprintln!("{m}");
                    return ExitCode::from(2);
                }
                Effect::Copy(text) => {
                    let _ = term.backend_mut().write_all(&osc52(&text));
                }
                Effect::OpenFile { name, contents } => {
                    let dir = cache_dir();
                    let path = dir.join(name.replace('/', "_"));
                    if std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, contents)).is_ok() {
                        let _ = Command::new("open").arg(&path).spawn();
                    }
                }
                Effect::Request(req) => {
                    let _ = req_tx.send(req);
                }
            }
        }
        if !later.is_empty() {
            // Effects of a visual's failure (a redraw): run them before blocking.
            pending = later;
            if redraw {
                pending.push(Effect::Redraw);
            }
            continue;
        }
        if redraw {
            let t = std::time::Instant::now();
            draw(&mut term, &mut app, &mut gallery, &mut screen);
            if let Some(f) = loop_trace.as_mut() {
                let _ = writeln!(f, "draw {:.2} ms", t.elapsed().as_secs_f64() * 1000.0);
            }
            // Pixels for the visuals this frame shows (then their neighbors).
            pending = app.after_draw();
            if !pending.is_empty() {
                continue;
            }
        }
        // While scrolling, live pages hold still; 150 ms after the last scroll
        // they resume (the only timer, and only while scrolling).
        let msg = match scrolled_at {
            Some(at) => match rx.recv_timeout(SCROLL_SETTLE.saturating_sub(at.elapsed())) {
                Ok(msg) => msg,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    scrolled_at = None;
                    gallery.hold_live(false);
                    pending.push(Effect::Redraw);
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    restore();
                    return ExitCode::SUCCESS;
                }
            },
            None => match rx.recv() {
                Ok(msg) => msg,
                Err(_) => {
                    restore();
                    return ExitCode::SUCCESS;
                }
            },
        };
        let t = std::time::Instant::now();
        let mut n = 0;
        let mut scroll_events = 0;
        let scroll_before = app.scroll;
        // Coalesce input already queued, but never wait before painting it.
        // macOS can coalesce an 8 ms condition-variable timeout into 50+ ms;
        // that made scrolling and even single navigation keys visibly lag.
        for msg in std::iter::once(msg).chain(std::iter::from_fn(|| {
            TURN.checked_sub(t.elapsed())?;
            rx.try_recv().ok()
        })) {
            n += 1;
            if is_scroll(&msg) {
                scroll_events += 1;
                scrolled_at = Some(std::time::Instant::now());
                gallery.hold_live(true);
            }
            if matches!(msg, Msg::Resize(..)) {
                screen.invalidate();
            }
            match msg {
                // Graphics plumbing goes to the gallery; only its results reach the app.
                Msg::Gfx(g) => {
                    for m in gallery.handle(term.backend_mut(), g) {
                        pending.extend(app.update(m));
                    }
                }
                msg => pending.extend(app.update(msg)),
            }
        }
        if let Some(f) = loop_trace.as_mut() {
            let _ = writeln!(f, "update {n} msgs {:.2} ms at {:.2} ms scroll-events {scroll_events} rows {scroll_before}->{}",
                t.elapsed().as_secs_f64() * 1000.0, trace_start.elapsed().as_secs_f64() * 1000.0, app.scroll);
        }
    }
}

/// Exit once the terminal hangs up (its pane closed). crossterm's reader spins on a
/// hung-up tty instead of failing, and a closed pane doesn't always send SIGHUP, so
/// an orphaned stage would burn a core forever. Blocks in `kevent`, waking only for
/// the user's keystrokes.
fn exit_on_hangup() {
    thread::spawn(|| {
        // SAFETY: plain kqueue calls on stdin with zeroed, fully initialized kevents.
        unsafe {
            let kq = libc::kqueue();
            let mut change: libc::kevent = std::mem::zeroed();
            change.ident = libc::STDIN_FILENO as usize;
            change.filter = libc::EVFILT_READ;
            change.flags = libc::EV_ADD | libc::EV_CLEAR;
            if kq < 0 || libc::kevent(kq, &change, 1, std::ptr::null_mut(), 0, std::ptr::null()) < 0 {
                return;
            }
            let mut event: libc::kevent = std::mem::zeroed();
            loop {
                match libc::kevent(kq, std::ptr::null(), 0, &mut event, 1, std::ptr::null()) {
                    1 if event.flags & libc::EV_EOF != 0 => std::process::exit(0),
                    n if n < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted => return,
                    _ => {}
                }
            }
        }
    });
}

/// Cell size in px from the tty's pixel size, when the terminal reports one.
fn cell_px() -> Option<(u16, u16)> {
    let s = crossterm::terminal::window_size().ok()?;
    (s.width > 0 && s.height > 0 && s.columns > 0 && s.rows > 0).then(|| (s.width / s.columns, s.height / s.rows))
}

#[cfg(test)]
mod tests {
    #[test]
    fn osc52_base64() {
        assert_eq!(super::base64_lite::encode(b"hi!"), "aGkh");
        assert_eq!(super::base64_lite::encode(b"hi"), "aGk=");
        assert_eq!(super::base64_lite::encode(b"h"), "aA==");
    }
}
