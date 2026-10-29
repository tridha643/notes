//! Part validation, mirroring sideshow's strict mode (postSurfaces.ts) plus
//! its edge limits (app.ts). serde already enforces each kind's required
//! fields and types; this checks what serde can't.

use notes_proto::{kits::KIT_IDS, posts::Part};

use crate::{diff::patch_has_content, usage, Result, Store};

pub const MAX_PART_BYTES: usize = 2 << 20;
pub const MAX_POST_BYTES: usize = 16 << 20;
pub const MAX_ASSET_BYTES: usize = 25 << 20;
/// Titles, in chars; longer ones are truncated (like sideshow).
pub const MAX_TITLE: usize = 500;
pub const MAX_COMMENT_TEXT: usize = 8000;
/// A session's trace keeps its most recent steps.
pub const MAX_TRACE_STEPS: usize = 2000;
pub(crate) const MAX_STEP_LABEL: usize = 500;
pub(crate) const MAX_STEP_DETAIL: usize = 4000;
pub(crate) const MAX_STEP_KIND: usize = 40;

/// The kind name, as on the wire.
pub fn part_kind(part: &Part) -> &'static str {
    match part {
        Part::Html { .. } => "html",
        Part::Markdown { .. } => "markdown",
        Part::Mermaid { .. } => "mermaid",
        Part::Diff { .. } => "diff",
        Part::Image { .. } => "image",
        Part::Trace { .. } => "trace",
        Part::Terminal { .. } => "terminal",
        Part::Json { .. } => "json",
        Part::Code { .. } => "code",
    }
}

pub(crate) fn part_id(part: &Part) -> Option<&str> {
    part_id_slot(part).as_deref()
}

fn part_id_slot(part: &Part) -> &Option<String> {
    match part {
        Part::Html { id, .. }
        | Part::Markdown { id, .. }
        | Part::Mermaid { id, .. }
        | Part::Diff { id, .. }
        | Part::Image { id, .. }
        | Part::Trace { id, .. }
        | Part::Terminal { id, .. }
        | Part::Json { id, .. }
        | Part::Code { id, .. } => id,
    }
}

pub(crate) fn part_id_mut(part: &mut Part) -> &mut Option<String> {
    match part {
        Part::Html { id, .. }
        | Part::Markdown { id, .. }
        | Part::Mermaid { id, .. }
        | Part::Diff { id, .. }
        | Part::Image { id, .. }
        | Part::Trace { id, .. }
        | Part::Terminal { id, .. }
        | Part::Json { id, .. }
        | Part::Code { id, .. } => id,
    }
}

/// Content bytes a part carries (sideshow's surfacesByteLength). Asset bytes
/// are bounded separately by [`MAX_ASSET_BYTES`].
pub fn part_bytes(part: &Part) -> usize {
    let opt = |s: &Option<String>| s.as_ref().map_or(0, String::len);
    match part {
        Part::Html { html, kits, .. } => html.len() + kits.iter().map(String::len).sum::<usize>(),
        Part::Markdown { markdown, .. } => markdown.len(),
        Part::Mermaid { mermaid, .. } => mermaid.len(),
        Part::Diff { patch, files, .. } => {
            opt(patch)
                + files
                    .iter()
                    .map(|f| f.filename.len() + f.before.len() + f.after.len())
                    .sum::<usize>()
        }
        Part::Image {
            asset_id,
            alt,
            caption,
            ..
        } => asset_id.len() + opt(alt) + opt(caption),
        Part::Trace {
            steps,
            asset_id,
            title,
            ..
        } => {
            asset_id.as_ref().map_or(0, |a| a.len())
                + opt(title)
                + steps
                    .iter()
                    .map(|s| s.label.len() + opt(&s.kind) + opt(&s.detail) + opt(&s.ts))
                    .sum::<usize>()
        }
        Part::Terminal { text, title, .. } => text.len() + opt(title),
        Part::Json { data, .. } => {
            struct Count(usize);
            impl std::io::Write for Count {
                fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                    self.0 += buf.len();
                    Ok(buf.len())
                }
                fn flush(&mut self) -> std::io::Result<()> {
                    Ok(())
                }
            }
            let mut n = Count(0);
            serde_json::to_writer(&mut n, data).expect("json value serializes");
            n.0
        }
        Part::Code {
            code,
            language,
            title,
            ..
        } => code.len() + opt(language) + opt(title),
    }
}

/// The first non-blank, non-comment line's first word (sideshow's mermaidDiagramType).
fn mermaid_has_type(src: &str) -> bool {
    src.lines()
        .map(str::trim)
        .any(|l| !l.is_empty() && !l.starts_with("%%") && !l.starts_with('#'))
}

/// Check one part's content. `at` is its request path, e.g. `parts[2]`.
pub(crate) fn validate_part(s: &Store, part: &Part, at: &str) -> Result<()> {
    let bytes = part_bytes(part);
    if bytes > MAX_PART_BYTES {
        return Err(usage(format!(
            "{at}: part is {bytes} bytes, over the {MAX_PART_BYTES}-byte limit"
        )));
    }
    let fail = |field: &str, message: String| Err(usage(format!("{at}.{field}: {message}")));
    match part {
        Part::Html { kits, .. } => {
            if let Some(bad) = kits.iter().find(|k| !KIT_IDS.contains(&k.as_str())) {
                return fail(
                    "kits",
                    format!("unknown kit \"{bad}\" — known: {}", KIT_IDS.join(", ")),
                );
            }
        }
        Part::Mermaid { mermaid, .. } if !mermaid_has_type(mermaid) => {
            return fail(
                "mermaid",
                "has no diagram type (first line should be e.g. \"flowchart TD\")".into(),
            );
        }
        Part::Diff { patch, files, .. } => {
            match patch.as_deref().filter(|p| !p.is_empty()) {
                Some(p) if !patch_has_content(p) => {
                    return fail(
                        "patch",
                        "did not parse to any file — expected a unified/git patch with --- /+++ headers and @@ hunks".into(),
                    );
                }
                Some(_) => {}
                None if files.is_empty() => {
                    return Err(usage(format!(
                        "{at}: diff part requires string \"patch\" or non-empty \"files\""
                    )));
                }
                None => {}
            }
            if let Some(i) = files.iter().position(|f| f.filename.trim().is_empty()) {
                return fail(&format!("files[{i}].filename"), "must not be empty".into());
            }
        }
        Part::Image { asset_id, .. } => {
            if !s.asset_exists(asset_id) {
                return fail(
                    "asset_id",
                    format!("no asset \"{asset_id}\" — upload it first (asset-put)"),
                );
            }
        }
        Part::Trace {
            steps, asset_id, ..
        } => match asset_id {
            Some(id) if !s.asset_exists(id) => {
                return fail(
                    "asset_id",
                    format!("no asset \"{id}\" — upload it first (asset-put)"),
                );
            }
            None if steps.is_empty() => {
                return Err(usage(format!(
                    "{at}: trace part requires \"asset_id\" or non-empty \"steps\""
                )));
            }
            _ => {}
        },
        Part::Code {
            line_start: Some(0),
            ..
        } => return fail("line_start", "must be at least 1".into()),
        _ => {}
    }
    Ok(())
}

/// Validate a post's parts: at least one, per-post size, and each part's
/// content. `only` limits content checks to one index (a single-part edit).
pub(crate) fn validate_parts(s: &Store, parts: &[Part], only: Option<usize>) -> Result<()> {
    if parts.is_empty() {
        return Err(usage("a post needs at least one part"));
    }
    let total: usize = parts.iter().map(part_bytes).sum();
    if total > MAX_POST_BYTES {
        return Err(usage(format!(
            "post is {total} bytes, over the {MAX_POST_BYTES}-byte limit"
        )));
    }
    for (i, part) in parts.iter().enumerate() {
        if only.is_none_or(|o| o == i) {
            validate_part(s, part, &format!("parts[{i}]"))?;
        }
    }
    Ok(())
}

/// Truncate to `max` chars without reallocating when it already fits.
pub(crate) fn clip(s: &str, max: usize) -> &str {
    s.char_indices().nth(max).map_or(s, |(i, _)| &s[..i])
}
