//! The only path from post data to terminal cells. Every untrusted string is
//! sanitized here before it's styled, and every part and post is capped here.
//!
//! Sanitizing drops ESC, every C0 control except `\t` (newlines are split on
//! before this runs), DEL and C1 (U+0080–U+009F), so a post can never move the
//! cursor, set the clipboard (OSC 52), open a link (OSC 8), or start an APC/DCS
//! string: the bytes after a dropped ESC are shown as plain text.

use std::borrow::Cow;

use ratatui::text::{Line, Span};

/// Per-part cap: lines, then bytes of source.
pub const PART_LINES: usize = 400;
pub const PART_BYTES: usize = 64 << 10;
/// Per-post cap over all cards.
pub const POST_LINES: usize = 1500;
pub const POST_BYTES: usize = 256 << 10;

fn bad(c: char) -> bool {
    (c.is_control() && c != '\t') || ('\u{80}'..='\u{9f}').contains(&c)
}

/// Drop control characters; tabs become spaces (a raw tab has no cell width).
pub fn sanitize(s: &str) -> Cow<'_, str> {
    if !s.chars().any(|c| bad(c) || c == '\t') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\t' => out.push_str("    "),
            c if bad(c) => {}
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// Sanitize every line of a multi-line string.
pub fn lines(s: &str) -> impl Iterator<Item = String> + '_ {
    s.split('\n').map(|l| sanitize(l.trim_end_matches('\r')).into_owned())
}

/// Truncate `out` to `max_lines` and `max_bytes` of text, appending a
/// "… N more lines" row. Returns how many lines were dropped.
pub fn cap(out: &mut Vec<Line<'static>>, max_lines: usize, max_bytes: usize) -> usize {
    let mut bytes = 0;
    let mut keep = out.len().min(max_lines);
    for (i, line) in out.iter().enumerate().take(keep) {
        bytes += line.spans.iter().map(|s| s.content.len()).sum::<usize>();
        if bytes > max_bytes {
            keep = i;
            break;
        }
    }
    let dropped = out.len() - keep;
    if dropped > 0 {
        out.truncate(keep);
        out.push(Line::from(Span::styled(
            format!("… {dropped} more lines · :copy for the raw part"),
            crate::ui::dim(),
        )));
    }
    dropped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(s: &str) -> bool {
        !s.chars().any(|c| c == '\x1b' || bad(c))
    }

    #[test]
    fn no_escape_survives() {
        for evil in [
            "\x1b[31mred\x1b[0m",
            "\x1b]8;;https://evil\x07link\x1b]8;;\x07",
            "\x1b]52;c;aGk=\x07",
            "\x1b_Ga=T;AAAA\x1b\\",
            "\x1bPq#0;2;0;0;0\x1b\\",
            "c1 \u{9b}31m csi and \u{9d} osc",
            "bell\x07 bs\x08 del\x7f nul\0",
        ] {
            let out = sanitize(evil);
            assert!(clean(&out), "{evil:?} → {out:?}");
        }
        assert_eq!(sanitize("\x1b[31mred"), "[31mred");
        assert_eq!(sanitize("a\tb"), "a    b");
        assert!(matches!(sanitize("plain é ✓"), Cow::Borrowed(_)));
        assert!(lines("a\r\n\x1bb").all(|l| clean(&l)));
    }

    #[test]
    fn caps_lines_and_bytes() {
        let mut v: Vec<Line> = (0..500).map(|i| Line::from(format!("{i}"))).collect();
        assert_eq!(cap(&mut v, PART_LINES, PART_BYTES), 100);
        assert_eq!(v.len(), PART_LINES + 1);
        assert!(v.last().unwrap().to_string().contains("100 more lines"));

        let mut v: Vec<Line> = (0..10).map(|_| Line::from("x".repeat(1000))).collect();
        assert_eq!(cap(&mut v, PART_LINES, 2500), 8);
        assert_eq!(v.len(), 3);

        let mut v: Vec<Line> = vec![Line::from("ok")];
        assert_eq!(cap(&mut v, PART_LINES, PART_BYTES), 0);
        assert_eq!(v.len(), 1);
    }
}
