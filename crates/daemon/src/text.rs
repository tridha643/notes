//! Pure text operations behind the write verbs: no I/O, no locks.

use notes_proto::{ErrorBody, ErrorCode, Replacement};
use serde_json::json;

pub fn error(code: ErrorCode, message: impl Into<String>, hint: Option<&str>) -> ErrorBody {
    ErrorBody {
        code,
        message: message.into(),
        hint: hint.map(Into::into),
        details: None,
    }
}

/// Apply every replacement in order to the evolving text, or fail without a result.
pub fn apply_edits(path: &str, body: &str, edits: &[Replacement]) -> Result<String, ErrorBody> {
    if edits.is_empty() {
        return Err(error(
            ErrorCode::Usage,
            "edit needs at least one replacement",
            None,
        ));
    }
    let mut out = body.to_string();
    for edit in edits {
        if edit.old.is_empty() {
            return Err(error(
                ErrorCode::Usage,
                "edit `old` text is empty",
                Some("use prepend or append to add text"),
            ));
        }
        let starts: Vec<usize> = out.match_indices(&edit.old).map(|(i, _)| i).collect();
        match starts.len() {
            0 => {
                return Err(error(
                    ErrorCode::NotFound,
                    format!("{:?} not found in {path}", edit.old),
                    Some("re-read the note; `old` must match exactly, including whitespace"),
                ));
            }
            1 => {}
            n if !edit.all => {
                let mut lines: Vec<usize> = starts
                    .iter()
                    .map(|&i| out[..i].matches('\n').count() + 1)
                    .collect();
                lines.dedup();
                let list = lines
                    .iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut e = error(
                    ErrorCode::Ambiguous,
                    format!("{:?} matches {n} times (lines {list})", edit.old),
                    Some("add surrounding text or pass --all"),
                );
                e.details = Some(json!({ "matches": n, "lines": lines }));
                return Err(e);
            }
            _ => {}
        }
        out = if edit.all {
            out.replace(&edit.old, &edit.new)
        } else {
            out.replacen(&edit.old, &edit.new, 1)
        };
    }
    Ok(out)
}

fn with_newline(text: &str) -> String {
    let mut block = text.to_string();
    if !block.ends_with('\n') {
        block.push('\n');
    }
    block
}

/// ATX heading level and text, e.g. `## Todo ##` → (2, "Todo").
fn heading(line: &str) -> Option<(usize, &str)> {
    let line = line.trim_end_matches(['\n', '\r']);
    let trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let level = trimmed.bytes().take_while(|&b| b == b'#').count();
    let rest = &trimmed[level..];
    if !(1..=6).contains(&level) || !(rest.is_empty() || rest.starts_with([' ', '\t'])) {
        return None;
    }
    Some((level, rest.trim().trim_end_matches('#').trim_end()))
}

fn is_fence(line: &str) -> bool {
    let t = line.trim_start_matches(' ');
    t.starts_with("```") || t.starts_with("~~~")
}

/// Add `text` at the end of the note, or at the end of the section under `under`
/// (before the next heading of the same or higher level, after its last non-blank line).
pub fn append(body: &str, text: &str, under: Option<&str>) -> Result<String, ErrorBody> {
    let block = with_newline(text);
    let Some(under) = under else {
        let mut out = body.to_string();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        return Ok(out + &block);
    };
    let want = under.trim().trim_start_matches('#').trim();
    let (mut offset, mut fenced) = (0, false);
    let (mut section, mut insert_at) = (None, 0);
    for line in body.split_inclusive('\n') {
        let end = offset + line.len();
        offset = end;
        let fence = is_fence(line);
        let found = if fenced || fence { None } else { heading(line) };
        fenced ^= fence;
        match (section, found) {
            (Some(level), Some((l, _))) if l <= level => break,
            (Some(_), _) if !line.trim().is_empty() => insert_at = end,
            (None, Some((l, t))) if t == want => (section, insert_at) = (Some(l), end),
            _ => {}
        }
    }
    if section.is_none() {
        return Err(error(
            ErrorCode::NotFound,
            format!("no heading {want:?} in the note"),
            Some("check the heading text, or append without --under"),
        ));
    }
    let mut out = body[..insert_at].to_string();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    Ok(out + &block + &body[insert_at..])
}

pub fn prepend(body: &str, text: &str) -> String {
    with_newline(text) + body
}

pub fn line_count(body: &str) -> u32 {
    body.lines().count() as u32
}

/// 1-based inclusive line slice; an end past the last line is clamped.
pub fn slice_lines(body: &str, start: u32, end: u32) -> Result<String, ErrorBody> {
    if start == 0 || end < start {
        return Err(error(
            ErrorCode::Usage,
            format!("bad line range {start}:{end}"),
            Some("lines are 1-based, e.g. --lines 1:20"),
        ));
    }
    Ok(body
        .split_inclusive('\n')
        .skip(start as usize - 1)
        .take((end - start + 1) as usize)
        .collect())
}

/// Point `[[old]]`, `[[old|alias]]` and `[[old#heading]]` links at `new`; `None` when nothing changed.
pub fn rewrite_links(body: &str, olds: &[&str], new: &str) -> Option<String> {
    let (mut out, mut rest, mut changed) = (String::new(), body, false);
    while let Some(i) = rest.find("[[") {
        out.push_str(&rest[..i + 2]);
        rest = &rest[i + 2..];
        for old in olds {
            if let Some(after) = rest.strip_prefix(old)
                && (after.starts_with("]]") || after.starts_with(['|', '#']))
            {
                out.push_str(new);
                rest = after;
                changed = true;
                break;
            }
        }
    }
    out.push_str(rest);
    changed.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rep(old: &str, new: &str, all: bool) -> Replacement {
        Replacement {
            old: old.into(),
            new: new.into(),
            all,
        }
    }

    #[test]
    fn edits_are_exact_ordered_and_all_or_nothing() {
        let body = "a foo\nb\nfoo c\nfoo\n";
        let e = apply_edits(
            "p.md",
            body,
            &[rep("b", "B", false), rep("foo", "x", false)],
        )
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::Ambiguous);
        assert_eq!(e.message, "\"foo\" matches 3 times (lines 1, 3, 4)");
        assert_eq!(e.details.unwrap()["lines"], json!([1, 3, 4]));
        assert_eq!(
            apply_edits(
                "p.md",
                body,
                &[rep("foo", "x", true), rep("b\n", "", false)]
            )
            .unwrap(),
            "a x\nx c\nx\n"
        );
        assert_eq!(
            apply_edits("p.md", body, &[rep("nope", "x", false)])
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
        assert_eq!(
            apply_edits("p.md", body, &[rep("", "x", false)])
                .unwrap_err()
                .code,
            ErrorCode::Usage
        );
    }

    #[test]
    fn append_under_heading_lands_at_section_end() {
        let body = "# Plan\nintro\n## Todo\n- a\n\n### Sub\n- s\n## Done\n- d";
        assert_eq!(
            append(body, "- b", Some("Todo")).unwrap(),
            "# Plan\nintro\n## Todo\n- a\n\n### Sub\n- s\n- b\n## Done\n- d"
        );
        assert_eq!(
            append(body, "- e\n", Some("## Done")).unwrap(),
            format!("{body}\n- e\n")
        );
        assert_eq!(
            append("## T\n\n## U\n", "x", Some("T")).unwrap(),
            "## T\nx\n\n## U\n"
        );
        assert_eq!(
            append("```\n## Fake\n```\n", "x", Some("Fake"))
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
        assert_eq!(append("no newline", "x", None).unwrap(), "no newline\nx\n");
        assert_eq!(prepend("body\n", "top"), "top\nbody\n");
    }

    #[test]
    fn line_slices_and_links() {
        assert_eq!(slice_lines("1\n2\n3\n", 2, 9).unwrap(), "2\n3\n");
        assert_eq!(
            slice_lines("1\n2\n", 0, 1).unwrap_err().code,
            ErrorCode::Usage
        );
        assert_eq!(line_count("a\nb"), 2);
        let body = "see [[Old]], [[Old|o]], [[Old#h]], [[work/Old]], [[Older]]";
        assert_eq!(
            rewrite_links(body, &["Old", "work/Old"], "New").unwrap(),
            "see [[New]], [[New|o]], [[New#h]], [[New]], [[Older]]"
        );
        assert_eq!(rewrite_links("[[Other]]", &["Old"], "New"), None);
    }
}
