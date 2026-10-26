//! Post parts → cards. A card is built once per (post, version) from the
//! version-pinned `post-show` + `post-render`; [`layout`] then wraps cards to a
//! width with borders and applies folds. Every string goes through
//! `sanitize` on its way in.

use std::collections::{HashMap, HashSet};

use notes_proto::posts::{
    AssetId, Comment, DiffLineKind, Part, PartRender, Post, PostRender, StyledLine, TraceStep,
};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

use crate::{
    sanitize::{self, cap, sanitize},
    ui,
};

pub struct Card {
    pub label: String,
    pub body: CardBody,
}

pub enum CardBody {
    Lines(Vec<Line<'static>>),
    Tree(FoldTree),
    Placeholder(String),
    /// Preformatted rows (terminal output, code): never re-flowed. A row wider
    /// than the card hard-wraps; continuations start with `gutter` (the line
    /// number column's blank twin) or a dim `↪`. `shade` gives it a terminal's
    /// own background.
    Pre { lines: Vec<Line<'static>>, gutter: Option<usize>, shade: bool },
    /// Pixels drawn through kitty graphics; `fallback` shows until they're
    /// ready, and for good if they can't be made.
    Visual { source: VisualSource, fallback: Vec<Line<'static>> },
}

/// What a visual card renders from.
#[derive(Debug, Clone, PartialEq)]
pub enum VisualSource {
    Mermaid(String),
    Asset(AssetId),
    Html(String),
}

/// A visual's state as layout sees it.
#[derive(Debug, Clone, PartialEq)]
pub enum Fit {
    /// Sized to this many cells.
    Ready { cols: u16, rows: u16 },
    /// Still rendering: hold `rows` rows, so the feed doesn't jump when it lands.
    Pending { rows: u16 },
    Failed(String),
}

/// JSON values and trace steps, flattened in preorder. `end` is the index
/// just past a node's subtree, so a closed node skips straight to it.
pub struct FoldTree {
    pub nodes: Vec<Node>,
    /// Nodes shallower than this start open.
    pub open_depth: usize,
}

pub struct Node {
    pub depth: usize,
    pub line: Line<'static>,
    pub end: usize,
}

/// Per card, the nodes the user toggled away from their default.
pub type Folds = HashMap<usize, HashSet<usize>>;

impl FoldTree {
    pub fn is_open(&self, i: usize, toggled: Option<&HashSet<usize>>) -> bool {
        (self.nodes[i].depth < self.open_depth) != toggled.is_some_and(|t| t.contains(&i))
    }

    /// Visible nodes with their fold marker.
    pub fn visible(&self, toggled: Option<&HashSet<usize>>) -> Vec<(usize, Line<'static>)> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.nodes.len() {
            let node = &self.nodes[i];
            let kids = node.end > i + 1;
            let open = self.is_open(i, toggled);
            let marker = match (kids, open) {
                (false, _) => "  ",
                (true, true) => "▾ ",
                (true, false) => "▸ ",
            };
            let mut spans = vec![Span::raw("  ".repeat(node.depth)), Span::styled(marker, ui::dim())];
            spans.extend(node.line.spans.iter().cloned());
            out.push((i, Line::from(spans)));
            i = if kids && !open { node.end } else { i + 1 };
        }
        out
    }
}

pub fn build_cards(post: &Post, render: &PostRender) -> Vec<Card> {
    post.parts
        .iter()
        .enumerate()
        .map(|(i, part)| card(part, render.parts.get(i)))
        .collect()
}

fn label(kind: &str, extra: Option<&str>) -> String {
    match extra.map(|e| sanitize(e).into_owned()) {
        Some(e) if !e.is_empty() => format!("{kind} · {e}"),
        _ => kind.to_owned(),
    }
}

fn card(part: &Part, render: Option<&PartRender>) -> Card {
    // Exhaustive on purpose: a new part kind is a compile error here.
    match (part, render) {
        (Part::Markdown { markdown, .. }, _) => Card {
            label: "markdown".into(),
            body: CardBody::Lines(capped(markdown_lines(markdown), markdown.len())),
        },
        (Part::Code { title, language, code, .. }, Some(PartRender::Code { line_start, lines, .. })) => Card {
            label: label("code", title.as_deref().or(language.as_deref())),
            body: CardBody::Pre {
                gutter: Some(gutter_width(*line_start, lines.len())),
                lines: capped(code_lines(*line_start, lines), code.len()),
                shade: false,
            },
        },
        (Part::Code { title, language, code, line_start, .. }, _) => Card {
            label: label("code", title.as_deref().or(language.as_deref())),
            body: CardBody::Pre {
                gutter: Some(gutter_width(line_start.unwrap_or(1), code.lines().count())),
                lines: capped(plain_code(code, line_start.unwrap_or(1)), code.len()),
                shade: false,
            },
        },
        (Part::Diff { patch, files, .. }, Some(PartRender::Diff { layout, files: rendered })) => {
            let mut label = label("diff", Some(&format!("{} files", rendered.len())));
            if matches!(layout, notes_proto::posts::DiffLayout::Split) {
                label.push_str(" · split");
            }
            let bytes = patch.as_ref().map_or(0, |p| p.len())
                + files.iter().map(|f| f.before.len() + f.after.len()).sum::<usize>();
            Card { label, body: CardBody::Lines(capped(diff_lines(rendered), bytes)) }
        }
        (Part::Diff { patch, .. }, _) => {
            let text = patch.as_deref().unwrap_or("");
            Card {
                label: "diff".into(),
                body: CardBody::Lines(capped(sanitize::lines(text).map(Line::from).collect(), text.len())),
            }
        }
        (Part::Terminal { title, text, .. }, Some(PartRender::Terminal { lines })) => Card {
            label: label("terminal", title.as_deref()),
            body: CardBody::Pre { lines: capped(terminal_lines(lines.iter().map(styled).collect()), text.len()), gutter: None, shade: true },
        },
        (Part::Terminal { title, text, .. }, _) => Card {
            label: label("terminal", title.as_deref()),
            body: CardBody::Pre {
                lines: capped(terminal_lines(sanitize::lines(text).map(Line::from).collect()), text.len()),
                gutter: None,
                shade: true,
            },
        },
        (Part::Json { data, .. }, _) => Card {
            label: "json".into(),
            body: CardBody::Tree(json_tree(data)),
        },
        (Part::Trace { steps, asset_id, title, .. }, _) => match (steps.is_empty(), asset_id) {
            (true, Some(asset)) => Card {
                label: label("trace", title.as_deref()),
                body: CardBody::Placeholder(format!("steps are in asset {} · :copy for the raw part", sanitize(asset))),
            },
            _ => Card {
                label: label("trace", Some(&format!("{} steps", steps.len()))),
                body: CardBody::Tree(trace_tree(steps)),
            },
        },
        (Part::Mermaid { mermaid, .. }, _) => Card {
            label: "mermaid".into(),
            body: CardBody::Visual {
                source: VisualSource::Mermaid(mermaid.clone()),
                fallback: capped(sanitize::lines(mermaid).map(|l| Line::styled(l, ui::dim())).collect(), mermaid.len()),
            },
        },
        (Part::Image { asset_id, alt, caption, .. }, _) => Card {
            label: label("image", caption.as_deref()),
            body: CardBody::Visual {
                source: VisualSource::Asset(asset_id.clone()),
                fallback: vec![Line::styled(alt.as_deref().map_or("image".into(), |a| sanitize(a).into_owned()), ui::dim())],
            },
        },
        (Part::Html { html, .. }, _) => Card {
            label: "html".into(),
            body: CardBody::Visual {
                source: VisualSource::Html(html.clone()),
                fallback: vec![Line::styled("o opens it in the browser", ui::dim())],
            },
        },
    }
}

fn capped(mut lines: Vec<Line<'static>>, source_bytes: usize) -> Vec<Line<'static>> {
    if source_bytes > sanitize::PART_BYTES {
        // Byte cap on the source: keep a proportional prefix of lines.
        let keep = (lines.len() * sanitize::PART_BYTES / source_bytes).max(1);
        cap(&mut lines, keep, usize::MAX);
    } else {
        cap(&mut lines, sanitize::PART_LINES, sanitize::PART_BYTES);
    }
    lines
}

fn styled(line: &StyledLine) -> Line<'static> {
    Line::from(
        line.iter()
            .map(|s| Span::styled(sanitize(&s.text).into_owned(), ui::class_style(s.class.as_deref())))
            .collect::<Vec<_>>(),
    )
}

/// Cells taken by a code card's line-number column: digits, space, `│`, space.
fn gutter_width(start: u32, lines: usize) -> usize {
    (start as usize + lines).to_string().len() + 3
}

/// Terminal output as a terminal shows it: tabs to 8-column stops, a `$`
/// prompt picked out, trailing blank lines dropped, and "(no output)" if empty.
fn terminal_lines(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = lines
        .into_iter()
        .map(|line| {
            let mut col = 0;
            let mut spans: Vec<Span<'static>> = line
                .spans
                .into_iter()
                .map(|span| {
                    let mut text = String::with_capacity(span.content.len());
                    for ch in span.content.chars() {
                        if ch == '\t' {
                            let n = 8 - col % 8;
                            text.extend(std::iter::repeat_n(' ', n));
                            col += n;
                        } else {
                            text.push(ch);
                            col += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                        }
                    }
                    Span::styled(text, span.style)
                })
                .collect();
            if let Some(first) = spans.first_mut() {
                if let Some(rest) = first.content.strip_prefix("$ ") {
                    let rest = rest.to_owned();
                    let style = first.style;
                    *first = Span::styled("$ ", Style::new().fg(ui::TEAL).add_modifier(Modifier::BOLD));
                    spans.insert(1, Span::styled(rest, style.add_modifier(Modifier::BOLD)));
                }
            }
            Line::from(spans)
        })
        .collect();
    while out.last().is_some_and(|l| l.width() == 0) {
        out.pop();
    }
    if out.is_empty() {
        out.push(Line::styled("(no output)", ui::dim()));
    }
    out
}

fn gutter(n: u32, width: usize) -> Vec<Span<'static>> {
    vec![Span::styled(format!("{n:>width$} "), ui::dim()), Span::styled("│ ", ui::border())]
}

fn code_lines(start: u32, lines: &[StyledLine]) -> Vec<Line<'static>> {
    let width = (start as usize + lines.len()).to_string().len();
    lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let mut line = styled(l);
            line.spans.splice(0..0, gutter(start + i as u32, width));
            line
        })
        .collect()
}

fn plain_code(code: &str, start: u32) -> Vec<Line<'static>> {
    let lines: Vec<_> = sanitize::lines(code).collect();
    let width = (start as usize + lines.len()).to_string().len();
    lines
        .into_iter()
        .enumerate()
        .map(|(i, l)| {
            let mut spans = gutter(start + i as u32, width);
            spans.push(Span::raw(l));
            Line::from(spans)
        })
        .collect()
}

fn diff_lines(files: &[notes_proto::posts::DiffFileRender]) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for file in files {
        out.push(Line::from(vec![
            Span::styled(sanitize(&file.filename).into_owned(), Style::new().add_modifier(Modifier::BOLD)),
            Span::styled(format!("  +{}", file.added), Style::new().fg(ui::GREEN)),
            Span::styled(format!(" −{}", file.removed), Style::new().fg(ui::RED)),
        ]));
        for hunk in &file.hunks {
            out.push(Line::styled(sanitize(&hunk.header).into_owned(), Style::new().fg(ui::CYAN)));
            for l in &hunk.lines {
                let (sign, bg) = match l.kind {
                    DiffLineKind::Added => ("+", Some(ui::ADDED_BG)),
                    DiffLineKind::Removed => ("-", Some(ui::REMOVED_BG)),
                    DiffLineKind::Context => (" ", None),
                };
                let num = |n: Option<u32>| n.map_or("    ".into(), |n| format!("{n:>4}"));
                let mut line = styled(&l.spans);
                line.spans.insert(
                    0,
                    Span::styled(format!("{} {} {sign} ", num(l.old_line), num(l.new_line)), ui::dim()),
                );
                if let Some(bg) = bg {
                    line = line.style(Style::new().bg(bg));
                }
                out.push(line);
            }
        }
    }
    out
}

// ---- JSON and trace trees ----------------------------------------------------

fn count(n: usize, noun: &str) -> String {
    if n == 1 { format!("1 {noun}") } else { format!("{n} {noun}s") }
}

fn json_tree(data: &Value) -> FoldTree {
    fn push(v: &Value, key: Option<String>, depth: usize, nodes: &mut Vec<Node>) {
        let i = nodes.len();
        let mut spans = Vec::new();
        if let Some(k) = key {
            spans.push(Span::styled(format!("{k:?}"), Style::new().fg(ui::BLUE)));
            spans.push(Span::styled(": ", ui::dim()));
        }
        let leaf = |s: String, color| Span::styled(s, Style::new().fg(color));
        match v {
            Value::Object(m) => spans.push(Span::styled(format!("{{… {}}}", count(m.len(), "key")), ui::dim())),
            Value::Array(a) => spans.push(Span::styled(format!("[… {}]", count(a.len(), "item")), ui::dim())),
            Value::String(s) => spans.push(leaf(format!("{:?}", sanitize(s)), ui::GREEN)),
            Value::Number(n) => spans.push(leaf(n.to_string(), ui::PEACH)),
            Value::Bool(b) => spans.push(leaf(b.to_string(), ui::PEACH)),
            Value::Null => spans.push(leaf("null".into(), ui::PEACH)),
        }
        nodes.push(Node { depth, line: Line::from(spans), end: 0 });
        match v {
            Value::Object(m) => {
                for (k, v) in m {
                    push(v, Some(sanitize(k).into_owned()), depth + 1, nodes);
                }
            }
            Value::Array(a) => {
                for (k, v) in a.iter().enumerate() {
                    push(v, Some(k.to_string()), depth + 1, nodes);
                }
            }
            _ => {}
        }
        nodes[i].end = nodes.len();
    }
    let mut nodes = Vec::new();
    push(data, None, 0, &mut nodes);
    FoldTree { nodes, open_depth: 2 }
}

fn trace_tree(steps: &[TraceStep]) -> FoldTree {
    let mut nodes = Vec::new();
    for step in steps {
        let i = nodes.len();
        let mut spans = vec![Span::styled("● ", Style::new().fg(ui::BLUE))];
        if let Some(kind) = &step.kind {
            spans.push(Span::styled(format!("{} ", sanitize(kind)), ui::dim()));
        }
        spans.push(Span::raw(sanitize(&step.label).into_owned()));
        if let Some(ts) = &step.ts {
            spans.push(Span::styled(format!("  {}", sanitize(ts)), ui::dim()));
        }
        nodes.push(Node { depth: 0, line: Line::from(spans), end: 0 });
        for l in step.detail.as_deref().map(sanitize::lines).into_iter().flatten() {
            nodes.push(Node { depth: 1, line: Line::from(l), end: nodes.len() + 1 });
        }
        nodes[i].end = nodes.len();
    }
    FoldTree { nodes, open_depth: 0 }
}

// ---- Markdown ------------------------------------------------------------------

fn markdown_lines(src: &str) -> Vec<Line<'static>> {
    use comrak::{nodes::NodeValue, Arena, Options};
    let mut options = Options::default();
    options.extension.table = true;
    options.extension.strikethrough = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    let arena = Arena::new();
    let root = comrak::parse_document(&arena, src, &options);
    let mut out = Vec::new();
    for node in root.children() {
        block(node, "", &mut out);
    }
    while out.last().is_some_and(|l: &Line| l.width() == 0) {
        out.pop();
    }
    return out;

    fn inline<'a>(node: comrak::Node<'a>, style: Style, line: &mut Vec<Span<'static>>, lines: &mut Vec<Vec<Span<'static>>>) {
        let value = node.data().value.clone();
        match value {
            NodeValue::Text(t) => line.push(Span::styled(sanitize(&t).into_owned(), style)),
            NodeValue::Code(c) => line.push(Span::styled(sanitize(&c.literal).into_owned(), Style::new().fg(ui::GREEN))),
            NodeValue::SoftBreak => line.push(Span::styled(" ", style)),
            NodeValue::LineBreak => lines.push(std::mem::take(line)),
            NodeValue::HtmlInline(h) => line.push(Span::styled(sanitize(&h).into_owned(), ui::dim())),
            NodeValue::Image(link) => {
                line.push(Span::styled(format!("[image: {}]", sanitize(&link.url)), ui::dim()));
            }
            NodeValue::Link(link) => {
                for c in node.children() {
                    inline(c, style.fg(ui::BLUE).add_modifier(Modifier::UNDERLINED), line, lines);
                }
                line.push(Span::styled(format!(" ({})", sanitize(&link.url)), ui::dim()));
            }
            other => {
                let style = match other {
                    NodeValue::Emph => style.add_modifier(Modifier::ITALIC),
                    NodeValue::Strong => style.add_modifier(Modifier::BOLD),
                    NodeValue::Strikethrough => style.add_modifier(Modifier::CROSSED_OUT),
                    _ => style,
                };
                for c in node.children() {
                    inline(c, style, line, lines);
                }
            }
        }
    }

    /// A paragraph's lines: the first starts with `marker` (a list bullet), the
    /// rest are indented to match.
    fn para<'a>(node: comrak::Node<'a>, style: Style, prefix: &str, marker: &str, out: &mut Vec<Line<'static>>) {
        let mut line = Vec::new();
        let mut lines = Vec::new();
        for c in node.children() {
            inline(c, style, &mut line, &mut lines);
        }
        lines.push(line);
        for (i, spans) in lines.into_iter().enumerate() {
            let mut all = vec![Span::styled(prefix.to_owned(), ui::dim())];
            if i == 0 {
                all.push(Span::styled(marker.to_owned(), Style::new().fg(ui::MAUVE)));
            } else {
                all.push(Span::raw(" ".repeat(marker.width())));
            }
            all.extend(spans);
            out.push(Line::from(all));
        }
    }

    fn block<'a>(node: comrak::Node<'a>, prefix: &str, out: &mut Vec<Line<'static>>) {
        let value = node.data().value.clone();
        match value {
            NodeValue::Paragraph => {
                para(node, Style::new().fg(ui::TEXT), prefix, "", out);
                out.push(Line::from(prefix.to_owned()));
            }
            NodeValue::Heading(h) => {
                let style = match h.level {
                    1 | 2 => Style::new().fg(ui::BLUE).add_modifier(Modifier::BOLD),
                    _ => Style::new().fg(ui::TEXT).add_modifier(Modifier::BOLD),
                };
                para(node, style, prefix, "", out);
                out.push(Line::from(prefix.to_owned()));
            }
            NodeValue::CodeBlock(cb) => {
                // A shaded block, like the code cards; boxed() fills the row's background.
                let shade = Style::new().bg(ui::CODE_BG);
                for l in sanitize::lines(cb.literal.trim_end_matches('\n')) {
                    out.push(
                        Line::from(vec![Span::raw(format!("{prefix} ")), Span::styled(l, Style::new().fg(ui::TEXT))])
                            .style(shade),
                    );
                }
                out.push(Line::from(prefix.to_owned()));
            }
            NodeValue::List(list) => {
                for (n, item) in (list.start..).zip(node.children()) {
                    let bullet = match list.list_type {
                        comrak::nodes::ListType::Bullet => "• ".to_owned(),
                        comrak::nodes::ListType::Ordered => format!("{n}. "),
                    };
                    let task = match &item.data().value {
                        NodeValue::TaskItem(t) => Some(if t.symbol.is_some() { "☑ " } else { "☐ " }),
                        _ => None,
                    };
                    let inner = format!("{prefix}  {}", " ".repeat(UnicodeWidthStr::width(bullet.as_str())));
                    for (k, child) in item.children().enumerate() {
                        if k == 0 && matches!(child.data().value, NodeValue::Paragraph) {
                            let marker = format!("  {bullet}{}", task.unwrap_or(""));
                            para(child, Style::new().fg(ui::TEXT), prefix, &marker, out);
                        } else {
                            block(child, &inner, out);
                        }
                    }
                }
                out.push(Line::from(prefix.to_owned()));
            }
            NodeValue::BlockQuote | NodeValue::MultilineBlockQuote(_) => {
                for child in node.children() {
                    block(child, &format!("{prefix}│ "), out);
                }
            }
            NodeValue::ThematicBreak => {
                out.push(Line::styled(format!("{prefix}────────"), ui::dim()));
                out.push(Line::from(prefix.to_owned()));
            }
            NodeValue::Table(_) => {
                table(node, prefix, out);
                out.push(Line::from(prefix.to_owned()));
            }
            NodeValue::HtmlBlock(h) => {
                for l in sanitize::lines(h.literal.trim_end_matches('\n')) {
                    out.push(Line::styled(format!("{prefix}{l}"), ui::dim()));
                }
                out.push(Line::from(prefix.to_owned()));
            }
            _ => {
                for child in node.children() {
                    block(child, prefix, out);
                }
            }
        }
    }

    fn table<'a>(node: comrak::Node<'a>, prefix: &str, out: &mut Vec<Line<'static>>) {
        let rows: Vec<(bool, Vec<String>)> = node
            .children()
            .map(|row| {
                let header = matches!(row.data().value, NodeValue::TableRow(true));
                let cells = row
                    .children()
                    .map(|cell| {
                        let (mut line, mut lines) = (Vec::new(), Vec::new());
                        for c in cell.children() {
                            inline(c, Style::new(), &mut line, &mut lines);
                        }
                        line.iter().map(|s| s.content.as_ref()).collect::<String>()
                    })
                    .collect();
                (header, cells)
            })
            .collect();
        let cols = rows.iter().map(|r| r.1.len()).max().unwrap_or(0);
        let widths: Vec<usize> = (0..cols)
            .map(|c| rows.iter().filter_map(|r| r.1.get(c)).map(|s| s.width()).max().unwrap_or(0))
            .collect();
        let indent = format!("{prefix}  ");
        for (header, cells) in &rows {
            let mut spans = vec![Span::raw(indent.clone())];
            for (c, w) in widths.iter().enumerate() {
                let text = cells.get(c).map_or("", String::as_str);
                let style = if *header { Style::new().fg(ui::TEXT).add_modifier(Modifier::BOLD) } else { Style::new().fg(ui::TEXT) };
                spans.push(Span::styled(format!("{text}{}   ", " ".repeat(w - text.width())), style));
            }
            out.push(Line::from(spans));
            if *header {
                let rule: Vec<String> = widths.iter().map(|w| "─".repeat(*w)).collect();
                out.push(Line::styled(format!("{indent}{}", rule.join("   ")), ui::border()));
            }
        }
    }
}

// ---- Layout --------------------------------------------------------------------

/// What a laid-out row points at, for clicks, folds and `:copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    None,
    Card(usize),
    Node(usize, usize),
}

#[derive(Default)]
pub struct Laid {
    pub lines: Vec<Line<'static>>,
    pub hits: Vec<Hit>,
    pub slots: Vec<Slot>,
}

/// Rows reserved for a card's pixels: from `line`, `rows` tall, `cols` wide,
/// `col` cells in from the card's left edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    pub card: usize,
    pub line: usize,
    pub rows: u16,
    pub cols: u16,
    pub col: u16,
}

/// Split a preformatted line every `width` cells, keeping every space. Each
/// continuation starts with the gutter's blank twin (`   │ `) or a dim `↪ `.
pub fn hard_wrap(line: &Line<'static>, width: usize, gutter: Option<usize>) -> Vec<Line<'static>> {
    let width = width.max(4);
    let lead: Vec<Span<'static>> = match gutter {
        Some(g) if g + 4 <= width => vec![Span::raw(" ".repeat(g - 2)), Span::styled("│ ", ui::border())],
        _ => vec![Span::styled("↪ ", ui::dim())],
    };
    let lead_w: usize = lead.iter().map(|s| s.content.width()).sum();
    let mut rows: Vec<Vec<Span<'static>>> = vec![Vec::new()];
    let mut used = 0;
    for span in &line.spans {
        let mut chunk = String::new();
        for ch in span.content.chars() {
            let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if used + cw > width {
                rows.last_mut().unwrap().push(Span::styled(std::mem::take(&mut chunk), span.style));
                rows.push(lead.clone());
                used = lead_w;
            }
            chunk.push(ch);
            used += cw;
        }
        rows.last_mut().unwrap().push(Span::styled(chunk, span.style));
    }
    rows.into_iter().map(|spans| Line::from(spans).style(line.style)).collect()
}

/// Wrap a styled line to `width` cells, breaking at spaces where it can.
pub fn wrap(line: &Line<'static>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let base = line.style;
    // Continuation rows hang under the text, past any indent and list marker.
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    let body = text.trim_start();
    let marker = ["• ", "☐ ", "☑ ", "│ "].iter().find(|m| body.starts_with(**m)).map_or(0, |m| m.width());
    let hang = (text.width() - body.width() + marker).min(width / 2);
    let mut out: Vec<Vec<Span<'static>>> = vec![Vec::new()];
    let mut used = 0;
    // Words keep their trailing spaces; a word wider than the line splits by char.
    let words = line.spans.iter().flat_map(|span| {
        let style = span.style;
        span.content
            .split_inclusive(' ')
            .map(move |w| (w.to_owned(), style))
            .collect::<Vec<_>>()
    });
    for (word, style) in words {
        let w = word.trim_end().width();
        if used > 0 && used + w > width {
            out.push(vec![Span::raw(" ".repeat(hang))]);
            used = hang;
        }
        if w <= width {
            used += word.width();
            out.last_mut().unwrap().push(Span::styled(word, style));
            continue;
        }
        let mut chunk = String::new();
        for ch in word.chars() {
            let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if used + cw > width {
                out.last_mut().unwrap().push(Span::styled(std::mem::take(&mut chunk), style));
                out.push(vec![Span::raw(" ".repeat(hang))]);
                used = hang;
            }
            chunk.push(ch);
            used += cw;
        }
        out.last_mut().unwrap().push(Span::styled(chunk, style));
    }
    // A row's trailing space is counted above but must not widen the row past `width`.
    for row in &mut out {
        if let Some(last) = row.last_mut() {
            let trimmed = last.content.trim_end().to_owned();
            last.content = trimmed.into();
        }
    }
    out.into_iter().map(|spans| Line::from(spans).style(base)).collect()
}

fn bordered(label: &str, width: usize) -> Line<'static> {
    let label = format!(" {label} ");
    let fill = width.saturating_sub(3 + label.width());
    Line::from(vec![
        Span::styled("╭─", ui::border()),
        Span::styled(label, ui::dim()),
        Span::styled(format!("{}╮", "─".repeat(fill)), ui::border()),
    ])
}

/// Pad a content row to the card's inner width and add the side borders. A
/// row with a background (diff +/-) shades the whole inner width.
fn boxed(content: Line<'static>, inner: usize) -> Line<'static> {
    let pad = inner.saturating_sub(content.width());
    let fill = content.style;
    let mut spans = vec![Span::styled("│ ", ui::border())];
    spans.extend(content.spans.into_iter().map(|s| Span::styled(s.content, fill.patch(s.style))));
    spans.push(Span::styled(" ".repeat(pad), fill));
    spans.push(Span::styled(" │", ui::border()));
    Line::from(spans)
}

/// Every card, wrapped to `width`, with folds applied and comments below.
/// `fit` sizes each visual card (by card index) for the inner width.
pub fn layout(cards: &[Card], comments: &[Comment], width: u16, folds: &Folds, fit: &dyn Fn(usize) -> Fit) -> Laid {
    let width = (width as usize).max(8);
    let inner = width - 4;
    let mut laid = Laid::default();
    let push = |laid: &mut Laid, line, hit| {
        laid.lines.push(line);
        laid.hits.push(hit);
    };
    for (c, card) in cards.iter().enumerate() {
        push(&mut laid, bordered(&card.label, width), Hit::Card(c));
        let rows: Vec<(Line<'static>, Hit)> = match &card.body {
            CardBody::Lines(lines) => lines.iter().map(|l| (l.clone(), Hit::Card(c))).collect(),
            CardBody::Pre { lines, gutter, shade } => {
                let shade = if *shade { Style::new().bg(ui::CODE_BG) } else { Style::new() };
                for line in lines {
                    for row in hard_wrap(line, inner, *gutter) {
                        push(&mut laid, boxed(row.patch_style(shade), inner), Hit::Card(c));
                    }
                }
                push(&mut laid, Line::styled(format!("╰{}╯", "─".repeat(width - 2)), ui::border()), Hit::Card(c));
                push(&mut laid, Line::default(), Hit::None);
                continue;
            }
            CardBody::Placeholder(text) => vec![(Line::styled(text.clone(), ui::dim()), Hit::Card(c))],
            CardBody::Tree(tree) => tree
                .visible(folds.get(&c))
                .into_iter()
                .map(|(n, l)| (l, Hit::Node(c, n)))
                .collect(),
            CardBody::Visual { fallback, .. } => match fit(c) {
                Fit::Ready { cols, rows } => {
                    let cols = cols.min(inner as u16);
                    let col = 2 + (inner as u16 - cols) / 2;
                    laid.slots.push(Slot { card: c, line: laid.lines.len(), rows, cols, col });
                    vec![(Line::default(), Hit::Card(c)); rows as usize]
                }
                Fit::Pending { rows } => {
                    let mut held = vec![(Line::styled("rendering…", ui::dim()), Hit::Card(c))];
                    held.resize(rows.max(1) as usize, (Line::default(), Hit::Card(c)));
                    held
                }
                Fit::Failed(why) => {
                    let mut rows: Vec<_> = fallback.iter().map(|l| (l.clone(), Hit::Card(c))).collect();
                    rows.push((Line::styled(sanitize(&why).into_owned(), ui::dim()), Hit::Card(c)));
                    rows
                }
            },
        };
        for (line, hit) in rows {
            for row in wrap(&line, inner) {
                push(&mut laid, boxed(row, inner), hit);
            }
        }
        push(
            &mut laid,
            Line::styled(format!("╰{}╯", "─".repeat(width - 2)), ui::border()),
            Hit::Card(c),
        );
        push(&mut laid, Line::default(), Hit::None);
    }
    if !comments.is_empty() {
        for comment in comments {
            let who = if comment.author == "user" { "you".into() } else { sanitize(&comment.author).into_owned() };
            let line = Line::from(vec![
                Span::styled("↳ ", ui::dim()),
                Span::styled(who, Style::new().fg(ui::BLUE)),
                Span::styled(" · ", ui::dim()),
                Span::styled(sanitize::lines(&comment.text).collect::<Vec<_>>().join(" "), Style::new().fg(ui::TEXT)),
            ]);
            for row in wrap(&line, width) {
                push(&mut laid, row, Hit::None);
            }
        }
    }
    let dropped = cap(&mut laid.lines, sanitize::POST_LINES, sanitize::POST_BYTES);
    if dropped > 0 {
        laid.hits.truncate(laid.lines.len() - 1);
        laid.hits.push(Hit::None);
    }
    laid
}

/// The raw text behind a part, for `:copy`.
pub fn raw_part(part: &Part) -> String {
    match part {
        Part::Markdown { markdown, .. } => markdown.clone(),
        Part::Code { code, .. } => code.clone(),
        Part::Terminal { text, .. } => text.clone(),
        Part::Mermaid { mermaid, .. } => mermaid.clone(),
        Part::Html { html, .. } => html.clone(),
        Part::Json { data, .. } => serde_json::to_string_pretty(data).unwrap_or_default(),
        Part::Diff { patch: Some(p), .. } => p.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notes_proto::posts::{DiffFileRender, DiffHunk, DiffLayout, DiffLine, Span as WireSpan};
    use ratatui::{backend::TestBackend, widgets::Paragraph, Terminal};

    fn post(parts: Vec<Part>) -> Post {
        Post {
            id: "p1".into(),
            session: "s1".into(),
            title: "T".into(),
            parts,
            version: 1,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn span(text: &str, class: Option<&str>) -> WireSpan {
        WireSpan { text: text.into(), class: class.map(Into::into) }
    }

    /// Lay one part out at width 40 and draw it into a TestBackend; the golden
    /// is the buffer's text, row by row (styles are covered by the line tests).
    fn golden(part: Part, render: PartRender) -> String {
        let p = post(vec![part]);
        let r = PostRender { id: "p1".into(), version: 1, parts: vec![render] };
        let laid = layout(&build_cards(&p, &r), &[], 40, &Folds::new(), &|_| Fit::Pending { rows: 1 });
        let height = laid.lines.len() as u16;
        let mut term = Terminal::new(TestBackend::new(40, height)).unwrap();
        term.draw(|f| f.render_widget(Paragraph::new(laid.lines.clone()), f.area())).unwrap();
        let buf = term.backend().buffer();
        (0..height)
            .map(|y| (0..40).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_owned())
            .collect::<Vec<_>>()
            .join("\n")
            .trim_end()
            .to_owned()
    }

    #[test]
    fn golden_markdown() {
        let md = "# Title\n\nSome *em* and `code`, a [link](http://x).\n\n- one\n- two\n\n| a | bb |\n|---|---|\n| 1 | 2 |\n";
        assert_eq!(
            golden(Part::Markdown { markdown: md.into(), id: None }, PartRender::Native),
            "╭─ markdown ───────────────────────────╮
│ Title                                │
│                                      │
│ Some em and code, a link (http://x). │
│                                      │
│   • one                              │
│   • two                              │
│                                      │
│   a   bb                             │
│   ─   ──                             │
│   1   2                              │
╰──────────────────────────────────────╯"
        );
    }

    #[test]
    fn golden_code() {
        let part = Part::Code { code: "fn a() {}\nlet b;".into(), language: Some("rust".into()), title: None, line_start: Some(9), id: None };
        let render = PartRender::Code {
            language: Some("rust".into()),
            line_start: 9,
            lines: vec![vec![span("fn", Some("keyword")), span(" a() {}", None)], vec![span("let b;", None)]],
        };
        assert_eq!(
            golden(part, render),
            "╭─ code · rust ────────────────────────╮
│  9 │ fn a() {}                       │
│ 10 │ let b;                          │
╰──────────────────────────────────────╯"
        );
    }

    #[test]
    fn golden_diff() {
        let part = Part::Diff { patch: Some("x".into()), files: vec![], layout: DiffLayout::Unified, id: None };
        let render = PartRender::Diff {
            layout: DiffLayout::Unified,
            files: vec![DiffFileRender {
                filename: "a.rs".into(),
                language: None,
                added: 1,
                removed: 1,
                hunks: vec![DiffHunk {
                    header: "@@ -1 +1 @@".into(),
                    lines: vec![
                        DiffLine { kind: DiffLineKind::Removed, old_line: Some(1), new_line: None, spans: vec![span("old", None)] },
                        DiffLine { kind: DiffLineKind::Added, old_line: None, new_line: Some(1), spans: vec![span("new", None)] },
                    ],
                }],
            }],
        };
        assert_eq!(
            golden(part, render),
            "╭─ diff · 1 files ─────────────────────╮
│ a.rs  +1 −1                          │
│ @@ -1 +1 @@                          │
│    1      - old                      │
│         1 + new                      │
╰──────────────────────────────────────╯"
        );
    }

    #[test]
    fn diff_rows_shade_the_full_inner_width() {
        let part = Part::Diff { patch: None, files: vec![], layout: DiffLayout::Unified, id: None };
        let render = PartRender::Diff {
            layout: DiffLayout::Unified,
            files: vec![DiffFileRender {
                filename: "f".into(),
                language: None,
                added: 1,
                removed: 0,
                hunks: vec![DiffHunk {
                    header: "@@".into(),
                    lines: vec![DiffLine { kind: DiffLineKind::Added, old_line: None, new_line: Some(1), spans: vec![span("x", None)] }],
                }],
            }],
        };
        let laid = layout(&build_cards(&post(vec![part]), &PostRender { id: "p1".into(), version: 1, parts: vec![render] }), &[], 40, &Folds::new(), &|_| Fit::Pending { rows: 1 });
        let row = &laid.lines[3];
        let pad = &row.spans[row.spans.len() - 2];
        assert_eq!(pad.style.bg, Some(ui::ADDED_BG));
        assert_eq!(row.width(), 40);
    }

    #[test]
    fn golden_terminal_strips_escapes() {
        let part = Part::Terminal { text: "x".into(), cols: None, title: Some("cargo".into()), id: None };
        let render = PartRender::Terminal { lines: vec![vec![span("ok \x1b]52;c;aGk=\x07", Some("fg-green bold"))]] };
        assert_eq!(
            golden(part, render),
            "╭─ terminal · cargo ───────────────────╮
│ ok ]52;c;aGk=                        │
╰──────────────────────────────────────╯"
        );
    }

    #[test]
    fn golden_json_and_trace() {
        let data = serde_json::json!({"a": 1, "b": {"c": [true]}});
        assert_eq!(
            golden(Part::Json { data, id: None }, PartRender::Native),
            "╭─ json ───────────────────────────────╮
│ ▾ {… 2 keys}                         │
│     \"a\": 1                           │
│   ▾ \"b\": {… 1 key}                   │
│     ▸ \"c\": [… 1 item]                │
╰──────────────────────────────────────╯"
        );
        let steps = vec![
            TraceStep { label: "read".into(), kind: Some("tool".into()), detail: Some("file.rs".into()), ts: None },
            TraceStep { label: "done".into(), kind: None, detail: None, ts: None },
        ];
        assert_eq!(
            golden(Part::Trace { steps, asset_id: None, title: None, id: None }, PartRender::Native),
            "╭─ trace · 2 steps ────────────────────╮
│ ▸ ● tool read                        │
│   ● done                             │
╰──────────────────────────────────────╯"
        );
    }

    #[test]
    fn visuals_reserve_rows_or_fall_back() {
        let cards = build_cards(
            &post(vec![Part::Mermaid { mermaid: "graph TD\nA-->B".into(), id: None }]),
            &PostRender { id: "p1".into(), version: 1, parts: vec![PartRender::Native] },
        );
        let lay = |fit: Fit| layout(&cards, &[], 40, &Folds::new(), &move |_| fit.clone());
        let pending = lay(Fit::Pending { rows: 4 });
        assert!(pending.lines[1].to_string().contains("rendering…"));
        assert!(pending.slots.is_empty());
        // The held rows are the size it's expected to land at, so nothing below jumps.
        assert_eq!(pending.lines.len(), 1 + 4 + 1 + 1);

        let failed = lay(Fit::Failed("no stage-helper".into()));
        let text: String = failed.lines.iter().map(|l| l.to_string()).collect();
        assert!(text.contains("A-->B") && text.contains("no stage-helper"), "{text}");

        // 36 inner cells, a 20×3 image: centered (col 2 + 8), three blank rows after the border.
        let ready = lay(Fit::Ready { cols: 20, rows: 3 });
        assert_eq!(ready.slots, vec![Slot { card: 0, line: 1, rows: 3, cols: 20, col: 10 }]);
        assert_eq!(ready.lines.len(), 1 + 3 + 1 + 1);
    }

    #[test]
    fn toggling_a_node_changes_only_its_subtree() {
        let data = serde_json::json!({"a": {"x": 1, "y": 2}, "b": [1, 2, 3], "c": "z"});
        let tree = json_tree(&data);
        let before: Vec<String> = tree.visible(None).into_iter().map(|(_, l)| l.to_string()).collect();
        // node 1 is "a" (open by default at depth 1); close it.
        let toggled: HashSet<usize> = [1].into();
        let after: Vec<String> = tree.visible(Some(&toggled)).into_iter().map(|(_, l)| l.to_string()).collect();
        let subtree = tree.nodes[1].end - 2; // a's children
        assert_eq!(after.len(), before.len() - subtree);
        assert_eq!(&after[..1], &before[..1]);
        assert_eq!(after[1].replace('▸', "▾"), before[1]);
        assert_eq!(&after[2..], &before[2 + subtree..]);
    }

    #[test]
    fn caps_a_huge_part() {
        let code = (0..1000).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let card = card(&Part::Code { code, language: None, title: None, line_start: None, id: None }, None);
        let CardBody::Pre { lines, .. } = card.body else { panic!() };
        assert_eq!(lines.len(), sanitize::PART_LINES + 1);
        assert!(lines.last().unwrap().to_string().contains("600 more lines"));
    }

    #[test]
    fn wrap_breaks_at_spaces_and_splits_long_words() {
        let rows = wrap(&Line::from("aaa bbb ccc"), 7);
        assert_eq!(rows.iter().map(|l| l.to_string()).collect::<Vec<_>>(), ["aaa bbb", "ccc"]);
        let rows = wrap(&Line::from("abcdefghij"), 4);
        assert_eq!(rows.iter().map(|l| l.to_string()).collect::<Vec<_>>(), ["abcd", "efgh", "ij"]);
    }
}

#[cfg(test)]
mod pre_tests {
    use super::*;

    #[test]
    fn terminal_output_keeps_columns_and_marks_continuations() {
        let lines = terminal_lines(vec![Line::from("$ cargo test"), Line::from("a\tb"), Line::from(""), Line::from("")]);
        let text: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        assert_eq!(text, ["$ cargo test", "a       b"], "tab to column 8, trailing blanks dropped");
        assert_eq!(lines[0].spans[0].style.fg, Some(ui::TEAL));
        assert_eq!(terminal_lines(vec![])[0].to_string(), "(no output)");

        let rows: Vec<String> = hard_wrap(&Line::from("0123456789abcdef"), 10, None).iter().map(|l| l.to_string()).collect();
        assert_eq!(rows, ["0123456789", "↪ abcdef"]);
        // Code continuations sit under the gutter rule, not under the number.
        let rows: Vec<String> = hard_wrap(&Line::from(" 9 │ 0123456789abcdef"), 14, Some(5)).iter().map(|l| l.to_string()).collect();
        assert_eq!(rows, [" 9 │ 012345678", "   │ 9abcdef"]);
    }
}
