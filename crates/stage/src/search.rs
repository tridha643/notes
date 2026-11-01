//! Matches of a search query in feed lines: case-insensitive substring over a
//! line's plain text, reported as cell ranges so the draw can light them.

use ratatui::text::Line;
use unicode_width::UnicodeWidthChar;

/// A query as the chars it matches against (lowercased).
pub fn needle(query: &str) -> Vec<char> {
    query.chars().map(fold).collect()
}

fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// The cell ranges `from..to` where `needle` occurs in `line`.
fn ranges(line: &Line<'_>, needle: &[char]) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return vec![];
    }
    let mut col = 0;
    let cells: Vec<(char, usize, usize)> = line
        .spans
        .iter()
        .flat_map(|s| s.content.chars())
        .map(|c| {
            let from = col;
            col += c.width().unwrap_or(0);
            (fold(c), from, col)
        })
        .collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + needle.len() <= cells.len() {
        if cells[i..i + needle.len()].iter().map(|c| c.0).eq(needle.iter().copied()) {
            out.push((cells[i].1, cells[i + needle.len() - 1].2));
            i += needle.len();
        } else {
            i += 1;
        }
    }
    out
}

/// Every match of `needle` in `lines` as (line, from cell, to cell), in order.
pub fn matches(lines: &[Line<'_>], needle: &[char]) -> Vec<(usize, usize, usize)> {
    lines.iter().enumerate().flat_map(|(i, line)| ranges(line, needle).into_iter().map(move |(from, to)| (i, from, to))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::text::Span;

    #[test]
    fn matches_ignore_case_across_spans_and_report_cells() {
        let line = Line::from(vec![Span::raw("a界 Fo"), Span::raw("o fOO")]);
        assert_eq!(ranges(&line, &needle("foo")), [(4, 7), (8, 11)]);
        assert!(ranges(&line, &needle("")).is_empty());
    }
}
