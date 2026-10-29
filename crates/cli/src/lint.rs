//! Checks a post against the design guide (`notes guide`) and the space the
//! user's stage card gives it (`StageSpace`), before it's sent. A post that
//! can't fit the card (a table wider than it, a page wider or taller than its
//! viewport) is refused; one that only reads worse (Title Case, a long
//! paragraph) still publishes, with `warning:` lines on stderr.

use notes_proto::posts::{Part, StageSpace};

/// Above this an html part is no longer a light artifact (fx0's limit).
const MAX_HTML: usize = 512 * 1024;
/// The space assumed before stage has reported one. Checks against it only warn.
pub const DEFAULT_SPACE: StageSpace = StageSpace { cols: 80, rows: 40, html_width: 560, html_height: 560 };
/// An html page's body padding on each side (crates/stage/src/page.rs).
const PAGE_PADDING: u32 = 12;
/// Past these a markdown part stops reading at a glance (see the guide's "Fit the card").
const MAX_TABLE_COLS: usize = 5;
const MAX_PARAGRAPH_WORDS: usize = 60;
const MAX_MARKDOWN_LINES: usize = 40;

/// What a post breaks: `overflow` can't fit the card, `style` only reads worse.
#[derive(Debug, Default)]
pub struct Findings {
    pub overflow: Vec<String>,
    pub style: Vec<String>,
}

/// Findings for a post's title and parts, in order, for cards with `space`.
pub fn check(title: Option<&str>, parts: &[Part], space: &StageSpace) -> Findings {
    let mut out = Findings::default();
    if let Some(title) = title {
        if has_emoji(title) {
            out.style.push("title has emoji; the guide says no emoji".into());
        }
        if is_title_case(title) {
            out.style.push(format!("title {title:?} is Title Case; use sentence case"));
        }
    }
    for part in parts {
        match part {
            Part::Markdown { markdown, .. } => {
                if has_emoji(markdown) {
                    out.style.push("markdown has emoji; the guide says no emoji".into());
                }
                fit_markdown(markdown, space.cols as usize, &mut out);
            }
            Part::Mermaid { mermaid, .. } => {
                let head = mermaid.trim_start().lines().next().unwrap_or("").to_ascii_lowercase();
                let wide = head.ends_with(" lr") || head.ends_with(" rl");
                if wide && mermaid.matches("-->").count() > 4 {
                    out.style.push("mermaid LR graph with many edges shrinks to fit the card; use flowchart TD".into());
                }
            }
            Part::Html { html, .. } => {
                if html.len() > MAX_HTML {
                    out.style.push(format!("html is {} KB; keep a page under 512 KB", html.len() / 1024));
                }
                if has_emoji(html) {
                    out.style.push("html has emoji; the guide says no emoji".into());
                }
                fit_html(html, space, &mut out);
            }
            _ => {}
        }
    }
    out
}

/// Size checks on markdown source: tables, paragraphs, fenced code and length.
fn fit_markdown(md: &str, cols: usize, out: &mut Findings) {
    let lines: Vec<&str> = md.lines().collect();
    if lines.len() > MAX_MARKDOWN_LINES {
        out.style.push(format!("markdown is {} lines; keep a part under {MAX_MARKDOWN_LINES} or split it into posts", lines.len()));
    }
    let mut fenced = false;
    let mut table: Vec<Vec<usize>> = Vec::new();
    let mut words = 0;
    // A trailing blank line flushes the last table and paragraph.
    for line in lines.iter().chain([&""]) {
        let t = line.trim();
        if t.starts_with("```") || t.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            if line.chars().count() > cols {
                out.style.push(format!("a code line is {} chars; the card is {cols} wide, so it wraps", line.chars().count()));
            }
            continue;
        }
        if t.starts_with('|') {
            // Cell widths, minus the delimiter row (`|---|:-:|`).
            if !t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) {
                table.push(t.trim_matches('|').split('|').map(|c| c.trim().chars().count()).collect());
            }
            continue;
        }
        if !table.is_empty() {
            let n = table.iter().map(Vec::len).max().unwrap_or(0);
            let wide: usize = (0..n).map(|c| table.iter().filter_map(|r| r.get(c)).max().copied().unwrap_or(0)).sum::<usize>() + 3 * n;
            if wide > cols {
                out.overflow.push(format!("a table is {wide} cells wide and the card is {cols}; shorten cells or cut columns"));
            } else if n > MAX_TABLE_COLS {
                out.style.push(format!("a table has {n} columns; keep it to {MAX_TABLE_COLS} or split it"));
            }
            table.clear();
        }
        let prose = !t.is_empty() && !t.starts_with('#') && !t.starts_with('>');
        if prose {
            words += t.split_whitespace().count();
        } else {
            if words > MAX_PARAGRAPH_WORDS {
                out.style.push(format!("a paragraph runs {words} words; keep paragraphs under {MAX_PARAGRAPH_WORDS}"));
            }
            words = 0;
        }
    }
}

/// Size checks on html source against the page's viewport: what would be cut
/// off, or widen the page so stage zooms the whole part out.
fn fit_html(html: &str, space: &StageSpace, out: &mut Findings) {
    let (w, h) = (space.html_width, space.html_height);
    let flat = html.to_ascii_lowercase().replace([' ', '\n', '\t'], "");
    if flat.contains("position:fixed") {
        out.overflow.push("html uses position: fixed; the card sizes to normal flow, so it gets cut off".into());
    }
    if !css_numbers(&flat, "vh").is_empty() {
        out.overflow.push("html uses vh units; the card is measured to its content, so there's no viewport height to fill".into());
    }
    // `width:` and `min-width:`, not `max-width:` (a cap is fine) or `border-width:`.
    let sets_width = |before: &str| {
        before.ends_with("min-width:") || before.strip_suffix("width:").is_some_and(|b| !b.ends_with('-'))
    };
    let widest = css_numbers(&flat, "px").into_iter().filter(|(at, _)| sets_width(&flat[..*at])).map(|(_, px)| px);
    if let Some(px) = widest.max_by(f64::total_cmp).filter(|px| *px > w as f64) {
        out.overflow.push(format!("html sets a width of {px} px and the card is {w} px wide; use % or max-width"));
    }
    for svg in tags(html, "svg") {
        let (width, view) = (attr(svg, "width"), attr(svg, "viewbox"));
        let px = width.and_then(|v| v.parse::<f64>().ok());
        if view.is_none() && px.is_some_and(|px| px > w as f64) {
            out.overflow.push(format!("an svg is {} px wide with no viewBox and the card is {w} px; add a viewBox so it scales", px.unwrap()));
        }
        // A viewBox drawing at the full card width (no width, or a % one) is as tall as its aspect makes it.
        let full = width.is_none_or(|v| v.ends_with('%'));
        let aspect = view.and_then(|v| {
            let n: Vec<f64> = v.split([' ', ',']).filter_map(|x| x.parse().ok()).collect();
            (n.len() == 4 && n[2] > 0.0).then(|| n[3] / n[2])
        });
        if let Some(aspect) = aspect.filter(|_| full) {
            let tall = (aspect * (w - 2 * PAGE_PADDING) as f64).round();
            if tall > (h - 2 * PAGE_PADDING) as f64 {
                out.overflow.push(format!("an svg is {tall} px tall at the card's {w} px width and {h} px fit; split it or make it wider than tall"));
            }
        }
    }
}

/// (byte offset, value) of each number directly followed by `unit` in lowercase, spaceless CSS.
fn css_numbers(flat: &str, unit: &str) -> Vec<(usize, f64)> {
    flat.match_indices(unit)
        .filter_map(|(end, _)| {
            let start = flat[..end].rfind(|c: char| !(c.is_ascii_digit() || c == '.')).map_or(0, |i| i + 1);
            let next = flat[end + unit.len()..].chars().next();
            let whole = !next.is_some_and(|c| c.is_ascii_alphanumeric());
            flat[start..end].parse().ok().filter(|_| whole && start < end).map(|n| (start, n))
        })
        .collect()
}

/// The attribute text of each `<name …>` start tag.
fn tags<'a>(html: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("<{name}");
    html.match_indices(&open)
        .filter_map(|(at, _)| {
            let rest = &html[at + open.len()..];
            rest.starts_with(|c: char| c.is_whitespace() || c == '>').then(|| &rest[..rest.find('>').unwrap_or(rest.len())])
        })
        .collect()
}

/// An attribute's value from a start tag's text, matched case-insensitively.
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(name).map(|i| i + from) {
        from = i + name.len();
        let before = lower[..i].chars().last();
        if !before.is_some_and(|c| c.is_whitespace()) {
            continue;
        }
        let rest = tag[from..].trim_start().strip_prefix('=')?.trim_start();
        let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'');
        return Some(match quote {
            Some(q) => &rest[1..rest[1..].find(q).map_or(rest.len(), |e| e + 1)],
            None => &rest[..rest.find(|c: char| c.is_whitespace() || c == '/').unwrap_or(rest.len())],
        });
    }
    None
}

fn has_emoji(s: &str) -> bool {
    s.chars().any(|c| matches!(c as u32, 0x1F300..=0x1FAFF | 0x2600..=0x27BF | 0x1F000..=0x1F2FF))
}

/// Three or more words and every word longer than a short connector capitalized.
fn is_title_case(s: &str) -> bool {
    const SMALL: &[&str] = &["a", "an", "and", "as", "at", "by", "for", "in", "of", "on", "or", "the", "to", "vs", "with"];
    let words: Vec<&str> = s.split_whitespace().filter(|w| w.chars().next().is_some_and(char::is_alphabetic)).collect();
    let rest: Vec<&&str> = words.iter().skip(1).filter(|w| !SMALL.contains(&w.to_lowercase().as_str())).collect();
    // All-caps words (API, TUI) say nothing about the case style.
    let rest: Vec<&&str> = rest.into_iter().filter(|w| w.chars().any(char::is_lowercase)).collect();
    words.len() >= 3 && rest.len() >= 2 && rest.iter().all(|w| w.starts_with(char::is_uppercase))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn html(s: &str) -> Part {
        Part::Html { html: s.into(), kits: vec![], id: None }
    }

    #[test]
    fn flags_what_the_guide_forbids() {
        assert!(is_title_case("Rear Hub Teardown Notes"));
        assert!(!is_title_case("Rear hub teardown"));
        assert!(!is_title_case("How a post reaches the TUI pane"));
        assert!(!is_title_case("Fix API"));
        let f = check(Some("Ship it 🚀"), &[html("<div style='position: fixed'>x</div>")], &DEFAULT_SPACE);
        assert_eq!((f.style.len(), f.overflow.len()), (1, 1), "{f:?}");
        let lr = Part::Mermaid { mermaid: "flowchart LR\nA-->B\nB-->C\nC-->D\nD-->E\nE-->F".into(), id: None };
        assert_eq!(check(None, &[lr], &DEFAULT_SPACE).style.len(), 1);
        let ok = check(Some("Cache layout"), &[html("<p id=t>ok</p>")], &DEFAULT_SPACE);
        assert!(ok.style.is_empty() && ok.overflow.is_empty());
    }

    #[test]
    fn refuses_markdown_wider_than_the_card() {
        let md = |s: String| Part::Markdown { markdown: s, id: None };
        let ok = check(None, &[md("# Plan\n\nShort lead.\n\n| a | b |\n|---|---|\n| 1 | 2 |\n".into())], &DEFAULT_SPACE);
        assert!(ok.style.is_empty() && ok.overflow.is_empty());
        let wide = check(None, &[md(format!("| a | b |\n|---|---|\n| {} | x |\n", "y".repeat(90)))], &DEFAULT_SPACE);
        assert_eq!(wide.overflow.len(), 1, "{wide:?}");
        for style in ["| a | b | c | d | e | f |\n|-|-|-|-|-|-|\n".into(), "word ".repeat(70), "line\n\n".repeat(30)] {
            let f = check(None, &[md(style.clone())], &DEFAULT_SPACE);
            assert_eq!((f.style.len(), f.overflow.len()), (1, 0), "{style}");
        }
        // A narrow pane refuses a table that fits the default width.
        let mid = md(format!("| a | b |\n|---|---|\n| {} | x |\n", "y".repeat(40)));
        assert!(check(None, &[mid.clone()], &DEFAULT_SPACE).overflow.is_empty());
        assert_eq!(check(None, &[mid], &StageSpace { cols: 40, ..DEFAULT_SPACE }).overflow.len(), 1);
    }

    #[test]
    fn refuses_html_that_overflows_its_viewport() {
        let space = StageSpace { cols: 70, rows: 40, html_width: 500, html_height: 400 };
        let overflow = |s: &str| check(None, &[html(s)], &space).overflow;
        // Drawings that scale to the card, fixed sizes inside it, and max-width caps all fit.
        for fits in [
            r#"<svg width="100%" viewBox="0 0 760 330"><rect/></svg>"#,
            r#"<svg viewBox="0 0 8 8" width="8" height="8"></svg><div style="width: 320px">x</div>"#,
            r#"<div style="max-width:1000px;min-height:20vw">x</div>"#,
            r#"<svg width="900" height="200" viewBox="0 0 900 200"></svg>"#,
        ] {
            assert!(overflow(fits).is_empty(), "{fits}: {:?}", overflow(fits));
        }
        for breaks in [
            r#"<div style="width: 760px">wide</div>"#,
            "<style>.page{min-width:640px}</style>",
            r#"<svg width="900" height="200"><rect/></svg>"#,
            r#"<svg width="100%" viewBox="0 0 760 760"></svg>"#,
            "<style>body{min-height:100vh}</style>",
        ] {
            assert_eq!(overflow(breaks).len(), 1, "{breaks}: {:?}", overflow(breaks));
        }
    }
}
