//! Terminal probes, sent before the first frame and read after it, before
//! crossterm's event reader owns stdin (crossterm misparses APC replies).
//! A DA1 query goes last: every terminal answers it, so its reply means the
//! others are in (or never coming) and we stop early.

use std::{
    io::{self, Write},
    time::{Duration, Instant},
};

/// Kitty graphics query (a 1x1 RGB image, query only), cell size in px, DA1.
pub const QUERIES: &[u8] = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[16t\x1b[c";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Probe {
    pub cell_px: Option<(u16, u16)>,
    pub kitty: bool,
    /// The DA1 reply arrived, so nothing more is coming.
    pub done: bool,
}

pub fn send(out: &mut impl Write) -> io::Result<()> {
    out.write_all(QUERIES)?;
    out.flush()
}

/// Pull probe replies out of `bytes`; anything else (keys typed early) is dropped.
pub fn parse(bytes: &[u8]) -> Probe {
    let text = String::from_utf8_lossy(bytes);
    let mut probe = Probe { kitty: text.contains("\x1b_Gi=31;OK"), ..Probe::default() };
    for reply in text.split('\x1b').skip(1) {
        if let Some(body) = reply.strip_prefix("[6;").and_then(|r| r.split('t').next()) {
            let mut it = body.split(';').filter_map(|n| n.parse::<u16>().ok());
            if let (Some(h), Some(w)) = (it.next(), it.next()) {
                probe.cell_px = Some((w, h));
            }
        }
        // DA1: CSI ? digits/semicolons c
        if let Some(rest) = reply.strip_prefix("[?") {
            if rest.trim_start_matches(|c: char| c.is_ascii_digit() || c == ';').starts_with('c') {
                probe.done = true;
            }
        }
    }
    probe
}

/// Read replies from the tty for up to `timeout`, stopping at the DA1 reply.
pub fn read(fd: i32, timeout: Duration) -> Probe {
    let deadline = Instant::now() + timeout;
    let mut bytes = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        // SAFETY: one valid pollfd for the duration of the call.
        let n = unsafe { libc::poll(&mut pfd, 1, left.as_millis().max(1) as i32) };
        if n <= 0 {
            break;
        }
        let mut buf = [0u8; 512];
        // SAFETY: reads into a stack buffer of the given length.
        let got = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if got <= 0 {
            break;
        }
        bytes.extend_from_slice(&buf[..got as usize]);
        if parse(&bytes).done {
            break;
        }
    }
    parse(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_replies_and_drops_keys() {
        let p = parse(b"jk\x1b_Gi=31;OK\x1b\\\x1b[6;30;14t\x1b[?62;22;52cq");
        assert_eq!(p, Probe { cell_px: Some((14, 30)), kitty: true, done: true });
        let p = parse(b"\x1b[?1;2c");
        assert_eq!(p, Probe { cell_px: None, kitty: false, done: true });
        assert!(!parse(b"\x1b[6;30;14t").done);
    }
}
