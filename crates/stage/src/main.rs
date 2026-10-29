//! `stage [--session S]`: terminal setup, the probe, the event loop.
//!
//! Three blocking sources feed one channel: crossterm's reader (keys, mouse,
//! SIGWINCH resizes), the notesd watch stream, and nothing else. The loop
//! blocks on the channel, drains everything pending, then draws once, so an
//! idle stage never wakes and a 240-key paste is one frame.

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
        event::{self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers},
        execute,
        terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    },
    Terminal,
};
use stage::{
    app::{App, Effect, Msg},
    client::{StageClient, Watch},
    gallery::Gallery,
    latency::Latency,
    perform, probe, pump, ui,
};

fn restore() {
    let mut out = io::stdout();
    // Delete any kitty placements we made, leave the alt screen, give the shell back.
    let _ = out.write_all(b"\x1b_Ga=d,d=a\x1b\\");
    let _ = execute!(out, DisableMouseCapture, DisableBracketedPaste, LeaveAlternateScreen, Show);
    let _ = disable_raw_mode();
}

/// How long after the last scroll live pages start moving again.
const SCROLL_SETTLE: Duration = Duration::from_millis(150);

fn is_scroll(msg: &Msg) -> bool {
    use ratatui::crossterm::event::MouseEventKind;
    match msg {
        Msg::Mouse(m) => matches!(m.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown),
        Msg::Key(k) => matches!(k.code, KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown | KeyCode::Char(' ' | 'b' | 'j' | 'k')),
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
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        hook(info);
    }));
    let Ok(mut term) = Terminal::new(CrosstermBackend::new(io::stdout())) else {
        restore();
        return ExitCode::from(1);
    };
    let mut app = App::new(want);
    if let Ok(size) = term.size() {
        app.size = (size.width, size.height);
        // Through the loop too, so the starting card width reaches notesd.
        let _ = tx.send(Msg::Resize(size.width, size.height));
    }
    let mut latency = Latency::from_env();
    let mut gallery = Gallery::new(tx.clone(), StageClient::new(sock.clone()), cache_dir(), notes_proto::default_stage_path(), app.target());
    let mut drawn_size = app.size;
    let mut draw = |term: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App, gallery: &mut Gallery| {
        let _ = term.backend_mut().write_all(b"\x1b[?2026h");
        let _ = term.draw(|f| ui::draw(f, app));
        // A resize makes ratatui clear the screen (inside `draw`), and Rex frees the
        // images with it: send them again after the clear, before placing them.
        let focused = app.focused_visual();
        if app.size != drawn_size {
            gallery.replace_all(term.backend_mut(), app.target(), focused.as_ref());
            drawn_size = app.size;
        } else {
            gallery.retarget(app.target(), focused.as_ref());
        }
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
    draw(&mut term, &mut app, &mut gallery);
    gallery.warm();
    let p = probe::read(0, Duration::from_secs(2));
    app.update(Msg::Probed { cell_px: p.cell_px, kitty: p.kitty });

    // After the first frame (connecting can wait on launchd starting notesd): the watch
    // starts at the seq notesd had committed before bootstrap lists anything,
    // so nothing published in between is missed (duplicates are deduped by version).
    let start = StageClient::new(sock.clone()).latest_seq().ok();
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
    drop(tx);

    let mut client = StageClient::new(sock);
    let mut scrolled_at: Option<std::time::Instant> = None;
    // `STAGE_LOOP_TRACE=<file>`: per loop turn, time spent updating and drawing.
    let mut loop_trace = std::env::var_os("STAGE_LOOP_TRACE").and_then(|p| std::fs::File::create(p).ok());
    let start = app.start();
    let t = std::time::Instant::now();
    let mut pending = perform(&mut app, &mut client, start);
    if let Some(f) = loop_trace.as_mut() {
        let _ = writeln!(f, "bootstrap {:.2} ms", t.elapsed().as_secs_f64() * 1000.0);
    }
    loop {
        let mut redraw = false;
        let mut later = Vec::new();
        for effect in pending.drain(..) {
            match effect {
                Effect::Redraw => redraw = true,
                Effect::Quit => {
                    gallery.clear(term.backend_mut());
                    restore();
                    return ExitCode::SUCCESS;
                }
                Effect::PrefetchZoom { key, target } => {
                    if let Some(m) = gallery.prefetch_zoom(key, target) {
                        later.extend(pump(&mut app, &mut client, m));
                    }
                }
                Effect::NeedVisual { key, source } => {
                    for m in gallery.need(term.backend_mut(), key, source) {
                        later.extend(pump(&mut app, &mut client, m));
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
                Effect::Request(_) => unreachable!("pump performs requests"),
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
            draw(&mut term, &mut app, &mut gallery);
            if let Some(f) = loop_trace.as_mut() {
                let _ = writeln!(f, "draw {:.2} ms", t.elapsed().as_secs_f64() * 1000.0);
            }
            // Pixels for the visuals this frame shows (then their neighbors).
            let fx = app.after_draw();
            pending = perform(&mut app, &mut client, fx);
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
        for msg in std::iter::once(msg).chain(std::iter::from_fn(|| rx.try_recv().ok())) {
            n += 1;
            if is_scroll(&msg) {
                scrolled_at = Some(std::time::Instant::now());
                gallery.hold_live(true);
            }
            match msg {
                // Graphics plumbing goes to the gallery; only its results reach the app.
                Msg::Gfx(g) => {
                    for m in gallery.handle(term.backend_mut(), g) {
                        pending.extend(pump(&mut app, &mut client, m));
                    }
                }
                msg => pending.extend(pump(&mut app, &mut client, msg)),
            }
        }
        if let Some(f) = loop_trace.as_mut() {
            let _ = writeln!(f, "update {n} msgs {:.2} ms", t.elapsed().as_secs_f64() * 1000.0);
        }
    }
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

/// Cell size in px from the tty's pixel size, when the terminal reports one.
fn cell_px() -> Option<(u16, u16)> {
    let s = crossterm::terminal::window_size().ok()?;
    (s.width > 0 && s.height > 0 && s.columns > 0 && s.rows > 0).then(|| (s.width / s.columns, s.height / s.rows))
}
