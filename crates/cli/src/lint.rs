//! Checks a post against the design guide (`notes guide`) before it's sent.
//! They only warn: a post that breaks a rule still publishes, and the agent
//! sees why on stderr (`warning:` lines) so it can fix the next one.

use notes_proto::posts::Part;

/// Above this an html part is no longer a light artifact (fx0's limit).
const MAX_HTML: usize = 512 * 1024;
/// A stage card's text width when stage hasn't reported one; wider rows wrap.
pub const CARD_COLS: usize = 80;
/// Past these a markdown part stops reading at a glance (see the guide's "Fit the card").
const MAX_TABLE_COLS: usize = 5;
const MAX_PARAGRAPH_WORDS: usize = 60;
const MAX_MARKDOWN_LINES: usize = 40;

/// Warnings for a post's title and parts, in order, for cards `cols` cells wide.
pub fn check(title: Option<&str>, parts: &[Part], cols: usize) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(title) = title {
        if has_emoji(title) {
            out.push("title has emoji; the guide says no emoji".into());
        }
        if is_title_case(title) {
            out.push(format!("title {title:?} is Title Case; use sentence case"));
        }
    }
    for part in parts {
        match part {
            Part::Markdown { markdown, .. } => {
                if has_emoji(markdown) {
                    out.push("markdown has emoji; the guide says no emoji".into());
                }
                out.extend(fit_markdown(markdown, cols));
            }
            Part::Mermaid { mermaid, .. } => {
                let head = mermaid.trim_start().lines().next().unwrap_or("").to_ascii_lowercase();
                let wide = head.ends_with(" lr") || head.ends_with(" rl");
                if wide && mermaid.matches("-->").count() > 4 {
                    out.push("mermaid LR graph with many edges shrinks to fit the card; use flowchart TD".into());
                }
            }
            Part::Html { html, .. } => {
                if html.len() > MAX_HTML {
                    out.push(format!("html is {} KB; keep a page under 512 KB", html.len() / 1024));
                }
                let flat = html.to_ascii_lowercase().replace(' ', "");
                if flat.contains("position:fixed") {
                    out.push("html uses position: fixed; the card sizes to normal flow, so it gets clipped".into());
                }
                if has_emoji(html) {
                    out.push("html has emoji; the guide says no emoji".into());
                }
            }
            _ => {}
        }
    }
    out
}

/// Size checks on markdown source: tables, paragraphs, fenced code and length.
fn fit_markdown(md: &str, cols: usize) -> Vec<String> {
    let mut out = Vec::new();
    let lines: Vec<&str> = md.lines().collect();
    if lines.len() > MAX_MARKDOWN_LINES {
        out.push(format!("markdown is {} lines; keep a part under {MAX_MARKDOWN_LINES} or split it into posts", lines.len()));
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
                out.push(format!("a code line is {} chars; the card is {cols} wide, so it wraps", line.chars().count()));
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
            if n > MAX_TABLE_COLS {
                out.push(format!("a table has {n} columns; keep it to {MAX_TABLE_COLS} or split it"));
            } else if wide > cols {
                out.push(format!("a table is {wide} cells wide; the card is {cols}, so cells wrap. Shorten cells or cut columns"));
            }
            table.clear();
        }
        let prose = !t.is_empty() && !t.starts_with('#') && !t.starts_with('>');
        if prose {
            words += t.split_whitespace().count();
        } else {
            if words > MAX_PARAGRAPH_WORDS {
                out.push(format!("a paragraph runs {words} words; keep paragraphs under {MAX_PARAGRAPH_WORDS}"));
            }
            words = 0;
        }
    }
    out
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
        let w = check(Some("Ship it 🚀"), &[html("<div style='position: fixed'>x</div>")], CARD_COLS);
        assert_eq!(w.len(), 2, "{w:?}");
        let lr = Part::Mermaid { mermaid: "flowchart LR\nA-->B\nB-->C\nC-->D\nD-->E\nE-->F".into(), id: None };
        assert_eq!(check(None, &[lr], CARD_COLS).len(), 1);
        assert!(check(Some("Cache layout"), &[html("<p id=t>ok</p>")], CARD_COLS).is_empty());
    }

    #[test]
    fn flags_markdown_that_overflows_the_card() {
        let md = |s: String| Part::Markdown { markdown: s, id: None };
        let ok = "# Plan\n\nShort lead.\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";
        assert!(check(None, &[md(ok.into())], CARD_COLS).is_empty());
        let wide = format!("| a | b |\n|---|---|\n| {} | x |\n", "y".repeat(90));
        let many = "| a | b | c | d | e | f |\n|-|-|-|-|-|-|\n";
        let long = "word ".repeat(70);
        let tall = "line\n\n".repeat(30);
        for bad in [wide, many.into(), long, tall] {
            assert_eq!(check(None, &[md(bad.clone())], CARD_COLS).len(), 1, "{bad}");
        }
        // A narrow pane flags a table that fits the default width.
        let mid = md(format!("| a | b |\n|---|---|\n| {} | x |\n", "y".repeat(40)));
        assert!(check(None, &[mid.clone()], CARD_COLS).is_empty());
        assert_eq!(check(None, &[mid], 40).len(), 1);
    }
}
