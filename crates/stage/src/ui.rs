//! Drawing: header · sidebar · post title bar · cards · keybar. Pure over `App`.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Mode, Row};

pub const BORDER: Color = Color::Rgb(0x7f, 0x84, 0x9c);
pub const BLUE: Color = Color::Rgb(0x89, 0xb4, 0xfa);
pub const GREEN: Color = Color::Rgb(0xa6, 0xe3, 0xa1);
pub const RED: Color = Color::Rgb(0xf3, 0x8b, 0xa8);
pub const CYAN: Color = Color::Rgb(0x89, 0xdc, 0xeb);
pub const PEACH: Color = Color::Rgb(0xfa, 0xb3, 0x87);
pub const MAUVE: Color = Color::Rgb(0xcb, 0xa6, 0xf7);
pub const YELLOW: Color = Color::Rgb(0xf9, 0xe2, 0xaf);
pub const BAR: Color = Color::Rgb(0x31, 0x32, 0x44);
pub const SELECTED: Color = Color::Rgb(0x45, 0x47, 0x5a);
pub const ADDED_BG: Color = Color::Rgb(0x1f, 0x3a, 0x2a);
pub const REMOVED_BG: Color = Color::Rgb(0x45, 0x22, 0x2a);

pub fn dim() -> Style {
    Style::new().fg(BORDER)
}

pub fn border() -> Style {
    Style::new().fg(BORDER)
}

pub fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

const ANSI: [&str; 8] = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"];

fn ansi_color(name: &str) -> Option<Color> {
    if let Some(n) = name.strip_prefix("256-") {
        return n.parse().ok().map(Color::Indexed);
    }
    if let Some(hex) = name.strip_prefix("rgb-") {
        let v = u32::from_str_radix(hex, 16).ok()?;
        return Some(Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8));
    }
    let (bright, base) = match name.strip_prefix("bright-") {
        Some(b) => (8, b),
        None => (0, name),
    };
    ANSI.iter().position(|n| *n == base).map(|i| Color::Indexed((i + bright) as u8))
}

/// A span class from notesd's renderer (highlight capture names like
/// `keyword`, or ANSI classes like `fg-red bold`) → a cell style.
pub fn class_style(class: Option<&str>) -> Style {
    let Some(class) = class else { return Style::new() };
    let mut style = Style::new();
    for word in class.split_whitespace() {
        style = match word {
            "keyword" | "operator" => style.fg(MAUVE),
            "string" => style.fg(GREEN),
            "comment" => style.fg(BORDER).add_modifier(Modifier::ITALIC),
            "function" => style.fg(BLUE),
            "type" | "tag" | "attribute" => style.fg(YELLOW),
            "constant" | "number" => style.fg(PEACH),
            "property" | "parameter" | "variable" | "label" => style.fg(CYAN),
            "bold" => style.add_modifier(Modifier::BOLD),
            "dim" => style.add_modifier(Modifier::DIM),
            "italic" => style.add_modifier(Modifier::ITALIC),
            "underline" => style.add_modifier(Modifier::UNDERLINED),
            "inverse" => style.add_modifier(Modifier::REVERSED),
            "strike" => style.add_modifier(Modifier::CROSSED_OUT),
            w => match (w.strip_prefix("fg-"), w.strip_prefix("bg-")) {
                (Some(c), _) => ansi_color(c).map_or(style, |c| style.fg(c)),
                (_, Some(c)) => ansi_color(c).map_or(style, |c| style.bg(c)),
                _ => style,
            },
        };
    }
    style
}

pub struct Geometry {
    pub header: Rect,
    pub sidebar: Rect,
    /// Post title bar on its first row, laid-out cards below it.
    pub main: Rect,
    pub keybar: Rect,
}

pub fn geometry((w, h): (u16, u16)) -> Geometry {
    let body = h.saturating_sub(2);
    let side = (w / 4).clamp(20.min(w), 40);
    Geometry {
        header: Rect::new(0, 0, w, 1.min(h)),
        sidebar: Rect::new(0, 1, side, body),
        main: Rect::new(side + 1, 1, w.saturating_sub(side + 1), body),
        keybar: Rect::new(0, h.saturating_sub(1), w, 1.min(h)),
    }
}

/// First sidebar row on screen, so the selection stays visible.
pub fn sidebar_offset(app: &App, height: u16) -> usize {
    (app.sel + 1).saturating_sub(height as usize)
}

/// A full-width bar: text padded with the bar's own background.
fn bar(spans: Vec<Span<'static>>, width: u16, bg: Color) -> Paragraph<'static> {
    let used: usize = spans.iter().map(|s| s.content.width()).sum();
    let mut spans = spans;
    spans.push(Span::raw(" ".repeat((width as usize).saturating_sub(used))));
    Paragraph::new(Line::from(spans)).style(Style::new().bg(bg))
}

fn clip(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_owned();
    }
    let mut out = String::new();
    for c in s.chars() {
        if out.width() + 1 >= max {
            break;
        }
        out.push(c);
    }
    out.push('…');
    out
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let g = geometry(app.size);
    let open = app.open.as_ref().and_then(|id| app.sessions.iter().find(|s| &s.id == id));
    let mut head = vec![Span::styled(" stage ", bold().fg(BLUE))];
    if let Some(s) = open {
        head.push(Span::raw(clip(s.title.as_deref().unwrap_or(&s.id), 40)));
        head.push(Span::styled(format!(" · {}", s.agent), dim()));
        if let Some(cwd) = &s.cwd {
            head.push(Span::styled(format!(" · {}", clip(cwd, 40)), dim()));
        }
    }
    head.push(Span::styled(format!(" · {} sessions", app.sessions.len()), dim()));
    if let Some(b) = &app.banner {
        head.push(Span::styled(format!("  {b}"), Style::new().fg(PEACH).add_modifier(Modifier::BOLD)));
    }
    f.render_widget(bar(head, g.header.width, BAR), g.header);

    sidebar(f, app, g.sidebar);
    let sep = Rect::new(g.sidebar.x + g.sidebar.width, g.sidebar.y, 1, g.sidebar.height);
    f.render_widget(Paragraph::new(vec![Line::styled("│", border()); sep.height as usize]), sep);
    main(f, app, g.main);

    let keys = match &app.mode {
        Mode::Nav => match &app.toast {
            Some(t) => vec![Span::styled(format!(" {t}"), Style::new().fg(YELLOW))],
            None => vec![Span::styled(
                " j/k post  ↑↓ line  ⏎ fold  / search  c comment  v version  : command  q quit",
                dim(),
            )],
        },
        Mode::Search { query, .. } => vec![Span::styled(" / ", bold().fg(BLUE)), Span::raw(query.clone()), Span::styled("▏", dim())],
        Mode::Comment { input } => vec![Span::styled(" comment: ", bold().fg(BLUE)), Span::raw(input.clone()), Span::styled("▏", dim())],
        Mode::Command { input } => vec![Span::styled(" :", bold().fg(BLUE)), Span::raw(input.clone()), Span::styled("▏", dim())],
    };
    f.render_widget(bar(keys, g.keybar.width, BAR), g.keybar);
}

fn sidebar(f: &mut Frame, app: &App, area: Rect) {
    let w = area.width as usize;
    let mut lines = Vec::new();
    if let Mode::Search { hits, sel, query } = &app.mode {
        lines.push(Line::styled(format!(" {} matches", hits.len()), dim()));
        for (i, hit) in hits.iter().enumerate() {
            let title = hit.session.title.as_deref().unwrap_or(&hit.session.id);
            let style = if i == *sel { Style::new().bg(SELECTED) } else { Style::new() };
            lines.push(Line::styled(format!(" {:<w$}", clip(title, w - 2), w = w - 1), style.add_modifier(Modifier::BOLD)));
            if let Some(text) = &hit.text {
                lines.push(Line::styled(format!("   {:<w$}", clip(&crate::sanitize::sanitize(text), w - 4), w = w - 3), style.fg(BORDER)));
            }
        }
        if hits.is_empty() && !query.is_empty() {
            lines.push(Line::styled(" no sessions match", dim()));
        }
        f.render_widget(Paragraph::new(lines), area);
        return;
    }
    let offset = sidebar_offset(app, area.height);
    for (i, row) in app.rows().into_iter().enumerate().skip(offset).take(area.height as usize) {
        let selected = i == app.sel;
        let line = match row {
            Row::Session(s) => {
                let s = &app.sessions[s];
                let open = app.open.as_ref() == Some(&s.id);
                let dot = if app.unread.contains(&s.id) { "●" } else { " " };
                let title = crate::sanitize::sanitize(s.title.as_deref().unwrap_or(&s.agent)).into_owned();
                let text = format!("{dot}{} {}", if open { "▾" } else { "▸" }, clip(&title, w.saturating_sub(4)));
                Line::styled(format!("{text:<w$}"), if open { bold() } else { Style::new() })
            }
            Row::Post(p) => {
                let p = &app.posts[p];
                let shown = app.shown.as_ref() == Some(&p.id);
                let comments = app.comments.iter().filter(|c| c.post.as_ref() == Some(&p.id)).count();
                let dot = if app.unseen.contains(&p.id) { "•" } else { " " };
                let tail = if comments > 0 { format!(" v{} {comments}c", p.version) } else { format!(" v{}", p.version) };
                let title = crate::sanitize::sanitize(&p.title).into_owned();
                let room = w.saturating_sub(4 + tail.width());
                let text = format!("  {dot}{:<room$}{tail}", clip(&title, room));
                let style = if shown { Style::new().fg(BLUE) } else { Style::new() };
                Line::styled(format!("{text:<w$}"), style)
            }
        };
        lines.push(if selected { line.style(Style::new().bg(SELECTED)) } else { line });
    }
    if app.sessions.is_empty() {
        lines.push(Line::styled(" no sessions yet", dim()));
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn main(f: &mut Frame, app: &mut App, area: Rect) {
    if area.height == 0 {
        return;
    }
    let title = Rect::new(area.x, area.y, area.width, 1);
    let body = Rect::new(area.x, area.y + 1, area.width, area.height - 1);
    let (name, version) = match app.shown_post() {
        Some(p) => (crate::sanitize::sanitize(&p.title).into_owned(), Some(p.version)),
        None => (if app.shown.is_some() { "loading…".into() } else { "no post".into() }, None),
    };
    let latest = app.shown.as_ref().and_then(|id| app.posts.iter().find(|p| &p.id == id)).map(|p| p.version);
    let (scroll, cursor) = app.shown.as_ref().and_then(|id| app.views.get(id)).map_or((0, 0), |v| (v.scroll, v.cursor));
    let total = app.laid().map_or(0, |l| l.lines.len());
    let mut spans = vec![Span::styled(format!(" {}", clip(&name, area.width as usize / 2)), bold())];
    if let Some(v) = version {
        let pinned = if Some(v) != latest { format!(" (of v{})", latest.unwrap_or(v)) } else { String::new() };
        spans.push(Span::styled(format!("  v{v}{pinned}"), dim()));
    }
    if total > body.height as usize {
        let last = (scroll + body.height as usize).min(total);
        spans.push(Span::styled(format!("  ↕ {}–{last} / {total}", scroll + 1), dim()));
    }
    f.render_widget(bar(spans, title.width, BAR), title);
    let Some(laid) = app.laid() else { return };
    let mut lines: Vec<Line> = laid.lines.iter().skip(scroll).take(body.height as usize).cloned().collect();
    if let Some(line) = lines.get_mut(cursor.wrapping_sub(scroll)) {
        *line = line.clone().patch_style(Style::new().bg(SELECTED));
    }
    // ponytail: ratatui's buffer diff redraws only changed cells; a scroll still
    // rewrites the column (~one screen of bytes). DECSTBM+DECSLRM margin scrolling
    // needs a custom backend write path; add it if wheel scrolling lags in Rex.
    f.render_widget(Paragraph::new(lines), Rect::new(body.x + 1, body.y, body.width.saturating_sub(1), body.height));
}
