//! Drawing: header · sidebar · post title bar · cards · keybar. Pure over `App`.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Mode};

/// Dim text (versions, metadata, key descriptions).
pub const BORDER: Color = Color::Rgb(0x7f, 0x84, 0x9c);
/// Card frames and the sidebar rule, a step quieter than dim text.
pub const FRAME: Color = Color::Rgb(0x58, 0x5b, 0x70);
pub const TEXT: Color = Color::Rgb(0xcd, 0xd6, 0xf4);
pub const TEAL: Color = Color::Rgb(0x94, 0xe2, 0xd5);
/// Surface fills are the pane's own background, so Rex's glass shows through:
/// libghostty draws a cell with an explicit background as an opaque slab.
pub const SIDEBAR: Color = Color::Reset;
pub const CODE_BG: Color = Color::Reset;
pub const BLUE: Color = Color::Rgb(0x89, 0xb4, 0xfa);
pub const GREEN: Color = Color::Rgb(0xa6, 0xe3, 0xa1);
pub const RED: Color = Color::Rgb(0xf3, 0x8b, 0xa8);
pub const CYAN: Color = Color::Rgb(0x89, 0xdc, 0xeb);
pub const PEACH: Color = Color::Rgb(0xfa, 0xb3, 0x87);
pub const MAUVE: Color = Color::Rgb(0xcb, 0xa6, 0xf7);
pub const YELLOW: Color = Color::Rgb(0xf9, 0xe2, 0xaf);
pub const BAR: Color = Color::Reset;
pub const SELECTED: Color = Color::Rgb(0x31, 0x32, 0x44);
/// The cursor line: a soft band, quieter than a selection.
pub const CURSOR: Color = Color::Rgb(0x24, 0x25, 0x3a);
/// Text selected by a mouse drag.
pub const SELECTION: Color = Color::Rgb(0x45, 0x47, 0x5a);
pub const ADDED_BG: Color = Color::Rgb(0x1f, 0x3a, 0x2a);
pub const REMOVED_BG: Color = Color::Rgb(0x45, 0x22, 0x2a);

pub fn dim() -> Style {
    Style::new().fg(BORDER)
}

pub fn border() -> Style {
    Style::new().fg(FRAME)
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
    ANSI.iter().position(|n| *n == base).map(|i| PALETTE16[i + bright])
}

/// The 16 ANSI colors in stage's palette, so terminal output matches the cards
/// around it instead of whatever the host terminal's scheme is.
const PALETTE16: [Color; 16] = [
    Color::Rgb(0x45, 0x47, 0x5a), // black
    RED,
    GREEN,
    YELLOW,
    BLUE,
    Color::Rgb(0xf5, 0xc2, 0xe7), // magenta
    TEAL,
    Color::Rgb(0xba, 0xc2, 0xde), // white
    Color::Rgb(0x58, 0x5b, 0x70), // bright black
    RED,
    GREEN,
    YELLOW,
    BLUE,
    Color::Rgb(0xf5, 0xc2, 0xe7),
    TEAL,
    TEXT,
];

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

/// Below this width the sidebar hides, so the feed keeps a readable column.
pub const SIDEBAR_MIN_WIDTH: u16 = 60;

pub fn geometry((w, h): (u16, u16)) -> Geometry {
    let body = h.saturating_sub(2);
    let side = if w < SIDEBAR_MIN_WIDTH { 0 } else { (w / 4).clamp(20, 40) };
    Geometry {
        header: Rect::new(0, 0, w, 1.min(h)),
        sidebar: Rect::new(0, 1, side, body),
        main: if side == 0 { Rect::new(0, 1, w, body) } else { Rect::new(side + 1, 1, w.saturating_sub(side + 1), body) },
        keybar: Rect::new(0, h.saturating_sub(1), w, 1.min(h)),
    }
}

/// Focus gives the selected card the whole pane instead of the session sidebar.
pub fn geometry_for(app: &App) -> Geometry {
    let mut g = geometry(app.size);
    if app.focused_card.is_some() || app.sidebar_hidden {
        g.sidebar.width = 0;
        g.main.x = 0;
        g.main.width = app.size.0;
    }
    g
}

/// A picker replaces (rather than squeezes) the feed on a phone. Search must
/// also remain visible when the desktop sidebar was explicitly hidden.
pub fn session_area(app: &App) -> Option<Rect> {
    let g = geometry_for(app);
    (app.session_picker || (matches!(app.mode, Mode::Search { .. }) && g.sidebar.width == 0))
        .then(|| Rect::new(0, g.main.y, app.size.0, g.main.height))
}

pub fn search_offset(selected: usize, height: u16) -> usize {
    (selected + 1).saturating_sub(height.saturating_sub(1) as usize)
}

/// First sidebar row on screen, so the selection stays visible.
pub fn sidebar_offset(app: &App, height: u16) -> usize {
    (app.sel + 1).saturating_sub(height as usize)
}

/// The part of `s` between cell columns `from` and `to`.
pub fn cells(s: &str, from: usize, to: usize) -> String {
    let mut col = 0;
    let mut out = String::new();
    for ch in s.chars() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if col >= from && col < to {
            out.push(ch);
        }
        col += w;
    }
    out
}

/// Give cells `from..to` of a line a background, splitting spans as needed.
fn highlight(line: Line<'static>, from: usize, to: usize, bg: Color) -> Line<'static> {
    let mut spans = Vec::new();
    let mut col = 0;
    for span in line.spans {
        let (mut plain, mut lit) = (String::new(), String::new());
        let flush = |plain: &mut String, lit: &mut String, spans: &mut Vec<Span<'static>>| {
            if !plain.is_empty() {
                spans.push(Span::styled(std::mem::take(plain), span.style));
            }
            if !lit.is_empty() {
                spans.push(Span::styled(std::mem::take(lit), span.style.bg(bg)));
            }
        };
        for ch in span.content.chars() {
            let inside = col >= from && col < to;
            if inside && !plain.is_empty() || !inside && !lit.is_empty() {
                flush(&mut plain, &mut lit, &mut spans);
            }
            if inside { lit.push(ch) } else { plain.push(ch) }
            col += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        }
        flush(&mut plain, &mut lit, &mut spans);
    }
    // A selection past the line's end still shows, as far as it reaches.
    if to > col && to != usize::MAX && from <= col.max(from) {
        spans.push(Span::styled(" ".repeat(to - col.max(from)), Style::new().bg(bg)));
    }
    Line::from(spans).style(line.style)
}

/// A full-width bar: text padded with the bar's own background.
fn bar(spans: Vec<Span<'static>>, width: u16, bg: Color) -> Paragraph<'static> {
    let used: usize = spans.iter().map(|s| s.content.width()).sum();
    let mut spans = spans;
    spans.push(Span::raw(" ".repeat((width as usize).saturating_sub(used))));
    Paragraph::new(Line::from(spans)).style(Style::new().bg(bg))
}

pub fn clip(s: &str, max: usize) -> String {
    if max == 0 { return String::new(); }
    if s.width() <= max { return s.to_owned(); }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let width = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + width > max - 1 { break; }
        out.push(c);
        used += width;
    }
    out.push('…');
    out
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let g = geometry_for(app);
    let open = app.open.as_ref().and_then(|id| app.sessions.iter().find(|s| &s.id == id));
    let mut head = vec![Span::styled(" ≡ ", bold().fg(BLUE)), Span::styled("stage", bold().fg(TEAL))];
    if let Some(s) = open {
        let name = crate::sanitize::sanitize(s.title.as_deref().unwrap_or(&s.agent)).into_owned();
        head.push(Span::styled("  session ", dim()));
        head.push(Span::styled(clip(&name, 40), Style::new().fg(TEXT)));
        head.push(Span::styled(format!(" · {} posts", app.posts.len()), dim()));
    }
    if app.sessions.len() > 1 {
        head.push(Span::styled(format!(" · {} sessions", app.sessions.len()), dim()));
    }
    match &app.banner {
        Some(b) => head.push(Span::styled(format!(" · {b}"), Style::new().fg(PEACH).add_modifier(Modifier::BOLD))),
        None => head.push(Span::styled(" · live", dim())),
    }
    if app.size.0 < SIDEBAR_MIN_WIDTH {
        let name = if app.session_picker { "Sessions".to_owned() }
            else if matches!(app.mode, Mode::Search { .. }) { "Search sessions".to_owned() }
            else { open.map(|s| crate::sanitize::sanitize(s.title.as_deref().unwrap_or(&s.agent)).into_owned()).unwrap_or_else(|| "stage".into()) };
        let status = app.banner.as_ref().map(|s| format!(" · {}", clip(s, app.size.0 as usize / 2))).unwrap_or_default();
        head = vec![
            Span::styled(" ≡ ", bold().fg(BLUE)),
            Span::styled(clip(&name, (app.size.0 as usize).saturating_sub(3 + status.width())), bold().fg(TEAL)),
            Span::styled(status, Style::new().fg(PEACH)),
        ];
    }
    f.render_widget(bar(head, g.header.width, BAR), g.header);

    if let Some(area) = session_area(app) {
        app.on_screen.clear();
        sidebar(f, app, area);
    } else {
        if g.sidebar.width > 0 {
            sidebar(f, app, g.sidebar);
            let sep = Rect::new(g.sidebar.x + g.sidebar.width, g.sidebar.y, 1, g.sidebar.height);
            f.render_widget(Paragraph::new(vec![Line::styled("│", border()); sep.height as usize]), sep);
        }
        main(f, app, g.main);
    }

    let keys = match &app.mode {
        Mode::Nav => match &app.toast {
            Some(t) => vec![Span::styled(format!(" {t}"), Style::new().fg(YELLOW))],
            None if app.size.0 < SIDEBAR_MIN_WIDTH => chips(compact_keys(app)),
            None => chips(&[
                ("j/k", "post"),
                ("J/K", "session"),
                ("↑↓", "scroll"),
                ("enter", "fold"),
                ("/", "search"),
                ("c", "comment"),
                ("z", if app.focused_card.is_some() { "back" } else { "focus card" }),
                ("s", "sidebar"),
                ("r", "rename"),
                ("d", "delete"),
                (":", "command"),
                ("q", "quit"),
            ]),
        },
        Mode::Search { query, .. } => prompt("search", query, &[("enter", "open"), ("esc", "cancel")], g.keybar.width),
        Mode::Comment { input } => {
            let on = app.shown_post().map(|p| crate::sanitize::sanitize(&p.title).into_owned()).unwrap_or_default();
            let label = if app.size.0 < SIDEBAR_MIN_WIDTH { "reply".into() } else { format!("comment on “{}”", clip(&on, 30)) };
            prompt(&label, input, &[("enter", "send"), ("esc", "cancel")], g.keybar.width)
        }
        Mode::Command { input } => prompt(":", input, &[("enter", "run"), ("esc", "cancel")], g.keybar.width),
        Mode::Rename { input, .. } => prompt("rename", input, &[("enter", "save"), ("esc", "cancel")], g.keybar.width),
        Mode::Delete { what, .. } => {
            let mut spans = vec![Span::styled(format!(" delete {}? ", clip(what, (g.keybar.width as usize).saturating_sub(37))), bold().fg(RED))];
            spans.extend(chips(&[("y", "delete"), ("any key", "cancel")]).into_iter().skip(1));
            spans
        }
    };
    f.render_widget(bar(keys, g.keybar.width, BAR), g.keybar);
}

/// Keybar entries, as in the demo: the key bold blue, its action dim, `·` between.
fn chips(keys: &[(&str, &str)]) -> Vec<Span<'static>> {
    let mut spans = vec![Span::raw(" ")];
    for (i, (key, what)) in keys.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", dim()));
        }
        spans.push(Span::styled(key.to_string(), bold().fg(BLUE)));
        spans.push(Span::styled(format!(" {what}"), dim()));
    }
    spans
}

/// Compact controls are both keyboard hints and tappable targets. Use the same
/// fitted list for drawing and hit-testing, so no invisible action is clickable.
fn compact_keys(app: &App) -> &'static [(&'static str, &'static str)] {
    let keys: &[(&str, &str)] = if app.session_picker {
        &[("↑", "up"), ("↓", "down"), ("⏎", "open"), ("/", "find"), ("s", "back")]
    } else {
        &[("↑", "up"), ("↓", "down"), ("/", "find"), ("c", "reply"), ("z", "focus")]
    };
    let mut used = 1;
    let count = keys.iter().enumerate().take_while(|(i, (key, label))| {
        used += key.width() + 1 + label.width() + if *i == 0 { 0 } else { 3 };
        used <= app.size.0 as usize
    }).count();
    &keys[..count]
}

pub fn compact_action(app: &App, col: u16) -> Option<crossterm::event::KeyCode> {
    use crossterm::event::KeyCode;
    if app.size.0 >= SIDEBAR_MIN_WIDTH || app.toast.is_some() { return None; }
    let mut start = 1;
    for (key, label) in compact_keys(app) {
        let end = start + key.width() + 1 + label.width();
        if (start..end).contains(&(col as usize)) {
            return Some(match *key {
                "↑" if !app.session_picker => KeyCode::PageUp,
                "↓" if !app.session_picker => KeyCode::PageDown,
                "↑" => KeyCode::Up,
                "↓" => KeyCode::Down,
                "⏎" => KeyCode::Enter,
                _ => KeyCode::Char(key.chars().next()?),
            });
        }
        start = end + 3;
    }
    None
}

/// Keep the insertion point visible by showing the tail of long input. Hints
/// yield to input on narrow screens instead of pushing the cursor off-screen.
fn prompt(label: &str, input: &str, keys: &[(&str, &str)], width: u16) -> Vec<Span<'static>> {
    let width = width as usize;
    let label = clip(label, width.saturating_sub(4).min(width / 2));
    let room = width.saturating_sub(label.width() + 3);
    let mut tail = String::new();
    let mut used = 0;
    for c in input.chars().rev() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + cw > room { break; }
        tail.push(c);
        used += cw;
    }
    let mut spans = vec![
        Span::styled(format!(" {label} "), bold().fg(BLUE)),
        Span::styled(tail.chars().rev().collect::<String>(), Style::new().fg(TEXT)),
        Span::styled("▏", Style::new().fg(BLUE)),
    ];
    let hints = chips(keys);
    let hints_width: usize = hints.iter().map(|s| s.content.width()).sum();
    if label.width() + used + 3 + hints_width <= width { spans.extend(hints); }
    spans
}

/// `15:50` for today, else `Oct 18`, in local time.
pub fn when(secs: i64) -> String {
    let tm = local(secs as libc::time_t);
    if (tm.tm_year, tm.tm_yday) == today() {
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
    } else {
        const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
        format!("{} {}", MONTHS[tm.tm_mon.clamp(0, 11) as usize], tm.tm_mday)
    }
}

/// Today's local (year, day of year): `when` writes times for today, dates before.
pub fn today() -> (i32, i32) {
    let tm = local(unsafe { libc::time(std::ptr::null_mut()) });
    (tm.tm_year, tm.tm_yday)
}

fn local(t: libc::time_t) -> libc::tm {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    tm
}

fn sidebar(f: &mut Frame, app: &App, area: Rect) {
    let w = area.width as usize;
    let mut lines = Vec::new();
    if let Mode::Search { hits, sel, query } = &app.mode {
        lines.push(Line::styled(format!(" {} matches", hits.len()), bold().fg(TEXT)));
        let offset = search_offset(*sel, area.height);
        for (i, hit) in hits.iter().enumerate().skip(offset).take(area.height.saturating_sub(1) as usize) {
            let title = crate::sanitize::sanitize(hit.session.title.as_deref().unwrap_or(&hit.session.id));
            let text = hit.text.as_ref().map(|t| format!(" · {}", crate::sanitize::sanitize(t))).unwrap_or_default();
            let style = if i == *sel { Style::new().bg(SELECTED) } else { Style::new() };
            lines.push(Line::styled(format!(" {}", clip(&format!("{title}{text}"), w.saturating_sub(1))), style.add_modifier(Modifier::BOLD)));
        }
        if hits.is_empty() && !query.is_empty() {
            lines.push(Line::styled(" no sessions match", dim()));
        }
        f.render_widget(Paragraph::new(lines).style(Style::new().bg(SIDEBAR)), area);
        return;
    }
    let offset = sidebar_offset(app, area.height);
    for (i, s) in app.sessions.iter().enumerate().skip(offset).take(area.height as usize) {
        let selected = i == app.sel;
        let mark = if selected { Span::styled("▌", Style::new().fg(BLUE)) } else { Span::raw(" ") };
        let row_bg = if selected { Style::new().bg(SELECTED) } else { Style::new() };
        let open = app.open.as_ref() == Some(&s.id);
        let title = crate::sanitize::sanitize(s.title.as_deref().unwrap_or(&s.agent)).into_owned();
        let dot = if app.unread.contains(&s.id) { " ●" } else { "" };
        let room = w.saturating_sub(2 + dot.width());
        let style = if open { bold().fg(TEXT) } else { dim() };
        let line = Line::from(vec![
            mark,
            Span::styled(format!("{:<room$}", clip(&title, room)), style.patch(row_bg)),
            Span::styled(format!("{dot} "), Style::new().fg(MAUVE).patch(row_bg)),
        ]);
        lines.push(line);
    }
    if app.sessions.is_empty() {
        lines.push(Line::styled(" no sessions yet", dim()));
    }
    f.render_widget(Paragraph::new(lines).style(Style::new().bg(SIDEBAR)), area);
}

fn main(f: &mut Frame, app: &mut App, area: Rect) {
    app.on_screen.clear();
    if area.height == 0 {
        return;
    }
    let title = Rect::new(area.x, area.y, area.width, 1);
    let body = Rect::new(area.x, area.y + 1, area.width, area.height - 1);
    // The title bar names the post under the cursor.
    let (name, version) = match app.shown_post() {
        Some(p) => (crate::sanitize::sanitize(&p.title).into_owned(), Some(p.version)),
        None => (if app.posts.is_empty() { "no posts yet".into() } else { "loading…".into() }, None),
    };
    let latest = app.shown.as_ref().and_then(|id| app.posts.iter().find(|p| &p.id == id)).map(|p| p.version);
    let total = app.feed().lines.len();
    let (scroll, cursor) = (app.scroll, app.cursor);
    // Sticky: the bar names the post only once its own header has scrolled off.
    let header_visible = app.shown.as_ref().and_then(|id| app.posts.iter().position(|p| &p.id == id)).is_some_and(|i| {
        let start = app.feed().starts.get(i).copied().unwrap_or(0);
        start >= app.scroll
    });
    let name = if header_visible && version.is_some() { String::new() } else { name };
    let version = version.filter(|_| !header_visible);
    let mut spans = vec![Span::styled(format!(" {}", clip(&name, area.width as usize / 2)), bold().fg(TEXT))];
    if let Some(v) = version {
        let pinned = if Some(v) != latest { format!(" (of v{})", latest.unwrap_or(v)) } else { String::new() };
        let agent = app.open.as_ref().and_then(|id| app.sessions.iter().find(|s| &s.id == id)).map(|s| s.agent.as_str());
        let at = app.shown_post().map(|p| when(p.updated_at)).unwrap_or_default();
        let by = agent.map(|a| format!(" · {}", crate::sanitize::sanitize(a))).unwrap_or_default();
        spans.push(Span::styled(format!("  v{v}{pinned} · {at}{by}"), dim()));
    }
    if let Some((_, card)) = &app.focused_card {
        let label = app.shown_cards().and_then(|cards| cards.get(*card)).map(|card| card.label.as_str()).unwrap_or("card");
        spans.push(Span::styled(format!("  · {} · focus (z/esc to close)", clip(label, 24)), Style::new().fg(TEAL)));
    }
    if total > body.height as usize {
        let last = (scroll + body.height as usize).min(total);
        spans.push(Span::styled(format!("  ↕ {}–{last} / {total}", scroll + 1), dim()));
    }
    if area.width < SIDEBAR_MIN_WIDTH {
        let position = if total > body.height as usize {
            format!(" ↕ {}–{}/{total}", scroll + 1, (scroll + body.height as usize).min(total))
        } else { String::new() };
        let label = if app.focused_card.is_some() { format!("focus · {name}") } else { name };
        spans = vec![
            Span::styled(format!(" {}", clip(&label, (area.width as usize).saturating_sub(1 + position.width()))), bold().fg(TEXT)),
            Span::styled(position, dim()),
        ];
    }
    f.render_widget(bar(spans, title.width, BAR), title);
    let content = Rect::new(body.x + 1, body.y, body.width.saturating_sub(1), body.height);
    let drag = app.drag;
    let feed = app.feed();
    let mut lines: Vec<Line> = feed.lines.iter().skip(scroll).take(body.height as usize).cloned().collect();
    // The cursor line: a soft band across the card (a fold node gets the stronger one).
    let on_node = matches!(feed.hits.get(cursor), Some((_, crate::cards::Hit::Node(..))));
    if let Some(line) = lines.get_mut(cursor.wrapping_sub(scroll)) {
        let w = content.width as usize;
        *line = highlight(line.clone(), 1, w.saturating_sub(1), if on_node { SELECTED } else { CURSOR });
    }
    // A mouse selection, cell-exact on its first and last lines.
    if let Some((a, b)) = drag.filter(|(a, b)| a != b) {
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        for (i, line) in lines.iter_mut().enumerate() {
            let at = scroll + i;
            if at < start.0 || at > end.0 {
                continue;
            }
            let from = if at == start.0 { start.1 as usize } else { 0 };
            let to = if at == end.0 { end.1 as usize + 1 } else { content.width as usize };
            *line = highlight(line.clone(), from, to, SELECTION);
        }
    }
    // Visuals whose reserved rows are on screen, cropped at the top and bottom edges.
    let mut on_screen: Vec<crate::app::OnScreen> = feed
        .slots
        .iter()
        .filter_map(|(key, slot)| {
            let end = slot.line + slot.rows as usize;
            let bottom = scroll + body.height as usize;
            if end <= scroll || slot.line >= bottom {
                return None;
            }
            let top = scroll.saturating_sub(slot.line) as u16;
            let first = slot.line.max(scroll);
            Some(crate::app::OnScreen {
                key: key.clone(),
                col: content.x + slot.col,
                row: body.y + (first - scroll) as u16,
                cols: slot.cols,
                top,
                rows: (end.min(bottom) - first) as u16,
                total: slot.rows,
                px: (0, 0),
            })
        })
        .collect();
    // Screen presents this buffer with native row moves when safe, including
    // a rectangular text region beside the sidebar on Rex.
    f.render_widget(Paragraph::new(lines), content);
    // The pixel size each card was laid out for, so the gallery knows when new pixels fit.
    for o in &mut on_screen {
        if let Some(crate::app::VisualState::Ready { w, h, .. }) = app.visuals.get(&o.key) {
            o.px = (*w, *h);
        }
    }
    app.on_screen = on_screen;
}
