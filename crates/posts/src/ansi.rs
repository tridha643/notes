//! ANSI terminal output → styled lines. SGR (`ESC[…m`) becomes span classes
//! like `fg-red bold`; every other escape sequence is dropped. Carriage
//! returns resolve like sideshow's: a line keeps only what follows its last `\r`.

use notes_proto::posts::StyledLine;

use crate::highlight::LineBuilder;

const NAMES: [&str; 8] = [
    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Color {
    #[default]
    Default,
    /// 0..=15: the 8 named colours, then their bright variants.
    Named(u8),
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Style {
    fg: Color,
    bg: Color,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    inverse: bool,
    strike: bool,
}

impl Style {
    fn class(&self) -> Option<String> {
        use std::fmt::Write;
        let mut s = String::new();
        for (prefix, color) in [("fg", self.fg), ("bg", self.bg)] {
            match color {
                Color::Default => {}
                Color::Named(n) if n < 8 => write!(s, " {prefix}-{}", NAMES[n as usize]).unwrap(),
                Color::Named(n) => write!(s, " {prefix}-bright-{}", NAMES[n as usize - 8]).unwrap(),
                Color::Indexed(n) => write!(s, " {prefix}-256-{n}").unwrap(),
                Color::Rgb(r, g, b) => write!(s, " {prefix}-rgb-{r:02x}{g:02x}{b:02x}").unwrap(),
            }
        }
        for (on, name) in [
            (self.bold, "bold"),
            (self.dim, "dim"),
            (self.italic, "italic"),
            (self.underline, "underline"),
            (self.inverse, "inverse"),
            (self.strike, "strike"),
        ] {
            if on {
                s.push(' ');
                s.push_str(name);
            }
        }
        (!s.is_empty()).then(|| s[1..].to_owned())
    }

    fn apply(&mut self, params: &[u16]) {
        if params.is_empty() {
            *self = Style::default();
            return;
        }
        let mut i = 0;
        while i < params.len() {
            let p = params[i];
            match p {
                0 => *self = Style::default(),
                1 => self.bold = true,
                2 => self.dim = true,
                3 => self.italic = true,
                4 => self.underline = true,
                7 => self.inverse = true,
                9 => self.strike = true,
                21 => self.underline = true,
                22 => (self.bold, self.dim) = (false, false),
                23 => self.italic = false,
                24 => self.underline = false,
                27 => self.inverse = false,
                29 => self.strike = false,
                30..=37 => self.fg = Color::Named((p - 30) as u8),
                39 => self.fg = Color::Default,
                40..=47 => self.bg = Color::Named((p - 40) as u8),
                49 => self.bg = Color::Default,
                90..=97 => self.fg = Color::Named((p - 90 + 8) as u8),
                100..=107 => self.bg = Color::Named((p - 100 + 8) as u8),
                38 | 48 => {
                    let color = match params.get(i + 1) {
                        Some(5) => {
                            let n = params.get(i + 2).copied().unwrap_or(0).min(255) as u8;
                            i += 2;
                            if n < 16 {
                                Color::Named(n)
                            } else {
                                Color::Indexed(n)
                            }
                        }
                        Some(2) => {
                            let c =
                                |k: usize| params.get(i + k).copied().unwrap_or(0).min(255) as u8;
                            let rgb = Color::Rgb(c(2), c(3), c(4));
                            i += 4;
                            rgb
                        }
                        _ => Color::Default,
                    };
                    if p == 38 {
                        self.fg = color;
                    } else {
                        self.bg = color;
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
}

/// Parse terminal output into lines. A trailing newline doesn't add a line.
pub fn parse(text: &str) -> Vec<StyledLine> {
    let mut out = LineBuilder::default();
    let mut style = Style::default();
    let mut class = None;
    let mut params: Vec<u16> = Vec::new();
    for raw_line in text.split_inclusive('\n') {
        let line = raw_line.strip_suffix('\n').unwrap_or(raw_line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        // Overprinted progress output: keep what follows the last CR.
        let line = line.rfind('\r').map_or(line, |i| &line[i + 1..]);
        let bytes = line.as_bytes();
        let (mut i, mut start) = (0, 0);
        while i < bytes.len() {
            let b = bytes[i];
            if b != 0x1b && (b >= 0x20 || b == b'\t') {
                i += 1;
                continue;
            }
            out.push(&line[start..i], class.as_deref());
            i += 1;
            if b == 0x1b && i < bytes.len() {
                match bytes[i] {
                    b'[' => {
                        // CSI: parameter/intermediate bytes, then a final byte 0x40..=0x7e.
                        let body = i + 1;
                        let mut j = body;
                        while j < bytes.len() && !(0x40..=0x7e).contains(&bytes[j]) {
                            j += 1;
                        }
                        if j < bytes.len() && bytes[j] == b'm' {
                            params.clear();
                            params.extend(
                                line[body..j]
                                    .split([';', ':'])
                                    .map(|p| p.parse::<u16>().unwrap_or(0)),
                            );
                            if line[body..j].is_empty() {
                                params.clear();
                            }
                            let before = style;
                            style.apply(&params);
                            if style != before {
                                class = style.class();
                            }
                        }
                        i = (j + 1).min(bytes.len());
                    }
                    b']' | b'P' | b'_' | b'^' | b'X' => {
                        // OSC/DCS/APC/PM/SOS: up to BEL or ST (ESC \).
                        let mut j = i + 1;
                        while j < bytes.len()
                            && bytes[j] != 0x07
                            && !(bytes[j] == 0x1b && bytes.get(j + 1) == Some(&b'\\'))
                        {
                            j += 1;
                        }
                        i = if j < bytes.len() && bytes[j] == 0x1b {
                            j + 2
                        } else {
                            j + 1
                        }
                        .min(bytes.len());
                    }
                    // Two- and three-byte escapes (charset selects take one more byte).
                    b'(' | b')' | b'*' | b'+' | b'#' | b'%' => i = (i + 2).min(bytes.len()),
                    _ => i += 1,
                }
            }
            // A malformed escape may have skipped into a multi-byte char.
            while !line.is_char_boundary(i) {
                i += 1;
            }
            start = i;
        }
        out.push(&line[start..], class.as_deref());
        if raw_line.ends_with('\n') {
            out.newline();
        }
    }
    out.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use notes_proto::posts::Span;

    fn span(text: &str, class: Option<&str>) -> Span {
        Span {
            text: text.into(),
            class: class.map(Into::into),
        }
    }

    #[test]
    fn sgr_colors_and_attributes_become_classes() {
        let lines = parse("\x1b[1;31merror\x1b[0m: bad\n\x1b[92mok\x1b[39m done\n");
        assert_eq!(
            lines,
            vec![
                vec![span("error", Some("fg-red bold")), span(": bad", None)],
                vec![span("ok", Some("fg-bright-green")), span(" done", None)],
            ]
        );
    }

    #[test]
    fn style_carries_across_lines_and_extended_colors_parse() {
        let lines = parse("\x1b[4ma\nb\x1b[24m\n\x1b[38;5;196mx\x1b[48;2;255;136;0my\x1b[38;5;3mz");
        assert_eq!(lines[0], vec![span("a", Some("underline"))]);
        assert_eq!(lines[1], vec![span("b", Some("underline"))]);
        assert_eq!(
            lines[2],
            vec![
                span("x", Some("fg-256-196")),
                span("y", Some("fg-256-196 bg-rgb-ff8800")),
                span("z", Some("fg-yellow bg-rgb-ff8800"))
            ]
        );
    }

    #[test]
    fn non_sgr_escapes_and_controls_are_stripped() {
        let text = "\x1b]0;title\x07\x1b[2K\x1b[1Ghi\x1b]8;;https://x.y\x1b\\link\x1b]8;;\x1b\\\x1b(B\x08!\tend";
        assert_eq!(parse(text), vec![vec![span("hilink!\tend", None)]]);
    }

    #[test]
    fn carriage_returns_keep_the_last_overprint() {
        assert_eq!(
            parse("10%\r50%\r100%\r\nnext\n"),
            vec![vec![span("100%", None)], vec![span("next", None)]]
        );
        assert_eq!(
            parse("a\n\nb\n"),
            vec![vec![span("a", None)], vec![], vec![span("b", None)]]
        );
    }

    #[test]
    fn bare_reset_and_truncated_sequences_are_safe() {
        assert_eq!(
            parse("\x1b[31mred\x1b[mplain\x1b["),
            vec![vec![span("red", Some("fg-red")), span("plain", None)]]
        );
        assert_eq!(parse("\x1b"), Vec::<StyledLine>::new());
        assert_eq!(parse("\x1b(é!\x1bé?"), vec![vec![span("!?", None)]]);
    }
}
