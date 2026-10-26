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
    }
    let mut latency = Latency::from_env();
    let mut draw = |term: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App| {
        let _ = term.backend_mut().write_all(b"\x1b[?2026h");
        let _ = term.draw(|f| ui::draw(f, app));
        let _ = term.backend_mut().write_all(b"\x1b[?2026l");
        let _ = term.backend_mut().flush();
        latency.painted(&app.take_fresh());
    };

    // Draw first, probe after: the queries go out, the first frame paints, and
    // only then do we wait (≤100 ms) for the replies.
    let _ = probe::send(&mut io::stdout());
    draw(&mut term, &mut app);
    let p = probe::read(0, Duration::from_millis(100));
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
                    TermEvent::Resize(w, h) => vec![Msg::Resize(w, h)],
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
    let start = app.start();
    let mut pending = perform(&mut app, &mut client, start);
    loop {
        let mut redraw = false;
        for effect in pending.drain(..) {
            match effect {
                Effect::Redraw => redraw = true,
                Effect::Quit => {
                    restore();
                    return ExitCode::SUCCESS;
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
        if redraw {
            draw(&mut term, &mut app);
        }
        let Ok(msg) = rx.recv() else {
            restore();
            return ExitCode::SUCCESS;
        };
        pending.extend(pump(&mut app, &mut client, msg));
        while let Ok(msg) = rx.try_recv() {
            pending.extend(pump(&mut app, &mut client, msg));
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
