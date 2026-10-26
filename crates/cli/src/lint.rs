//! Checks a post against the design guide (`notes guide`) before it's sent.
//! They only warn: a post that breaks a rule still publishes, and the agent
//! sees why on stderr (`warning:` lines) so it can fix the next one.

use notes_proto::posts::Part;

/// Above this an html part is no longer a light artifact (fx0's limit).
const MAX_HTML: usize = 512 * 1024;

/// Warnings for a post's title and parts, in order.
pub fn check(title: Option<&str>, parts: &[Part]) -> Vec<String> {
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
            Part::Markdown { markdown, .. } if has_emoji(markdown) => {
                out.push("markdown has emoji; the guide says no emoji".into());
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
        let w = check(Some("Ship it 🚀"), &[html("<div style='position: fixed'>x</div>")]);
        assert_eq!(w.len(), 2, "{w:?}");
        let lr = Part::Mermaid { mermaid: "flowchart LR\nA-->B\nB-->C\nC-->D\nD-->E\nE-->F".into(), id: None };
        assert_eq!(check(None, &[lr]).len(), 1);
        assert!(check(Some("Cache layout"), &[html("<p id=t>ok</p>")]).is_empty());
    }
}
