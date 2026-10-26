//! Unified/git patch parsing and before/after diffing into highlighted hunks.

use std::time::Duration;

use notes_proto::posts::{DiffFile, DiffFileRender, DiffHunk, DiffLine, DiffLineKind, StyledLine};

use crate::highlight::{highlight_many, Lang};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct PatchFile<'a> {
    /// `None` for `/dev/null` (a new file) or when the patch never names it.
    pub old_path: Option<&'a str>,
    pub new_path: Option<&'a str>,
    pub hunks: Vec<PatchHunk<'a>>,
    /// Saw a `---` header (so the next one starts another file).
    seen_minus: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct PatchHunk<'a> {
    pub header: &'a str,
    pub lines: Vec<(DiffLineKind, Option<u32>, Option<u32>, &'a str)>,
}

impl PatchFile<'_> {
    /// The new path, or the old one for a deletion.
    pub fn filename(&self) -> &str {
        self.new_path.or(self.old_path).unwrap_or("")
    }
}

fn header_path(raw: &str) -> Option<&str> {
    // `--- a/x.rs\t2024-01-01 ...` (diff -u adds a tab and a timestamp).
    let path = raw.split('\t').next().unwrap_or(raw).trim_end();
    let path = path
        .strip_prefix('"')
        .and_then(|p| p.strip_suffix('"'))
        .unwrap_or(path);
    if path == "/dev/null" {
        return None;
    }
    Some(
        path.strip_prefix("a/")
            .or_else(|| path.strip_prefix("b/"))
            .unwrap_or(path),
    )
}

/// `@@ -a[,b] +c[,d] @@` → (a, b, c, d).
fn hunk_range(line: &str) -> Option<(u32, u32, u32, u32)> {
    let mut parts = line.strip_prefix("@@ ")?.split(' ');
    let range = |s: Option<&str>, sign: char| -> Option<(u32, u32)> {
        let s = s?.strip_prefix(sign)?;
        Some(match s.split_once(',') {
            Some((start, len)) => (start.parse().ok()?, len.parse().ok()?),
            None => (s.parse().ok()?, 1),
        })
    };
    let (a, b) = range(parts.next(), '-')?;
    let (c, d) = range(parts.next(), '+')?;
    Some((a, b, c, d))
}

/// Parse a (possibly multi-file) unified or git patch. Preamble text such as a
/// `git format-patch` commit message is ignored.
pub fn parse_patch(patch: &str) -> Vec<PatchFile<'_>> {
    let mut files: Vec<PatchFile> = Vec::new();
    // Lines still expected in the open hunk (old side, new side).
    let (mut old_left, mut new_left, mut old_no, mut new_no) = (0u32, 0u32, 0u32, 0u32);
    for line in patch.lines() {
        if old_left > 0 || new_left > 0 {
            let kind = match line.as_bytes().first() {
                // Some tools strip the space off empty context lines.
                Some(b' ') | None => Some(DiffLineKind::Context),
                Some(b'+') => Some(DiffLineKind::Added),
                Some(b'-') => Some(DiffLineKind::Removed),
                Some(b'\\') => continue,
                // A short hunk: this line belongs to the headers below.
                _ => None,
            };
            if let Some(kind) = kind {
                let text = line.get(1..).unwrap_or("");
                let (old, new) = match kind {
                    DiffLineKind::Context => {
                        (old_left, new_left) =
                            (old_left.saturating_sub(1), new_left.saturating_sub(1));
                        (old_no, new_no) = (old_no + 1, new_no + 1);
                        (Some(old_no - 1), Some(new_no - 1))
                    }
                    DiffLineKind::Added => {
                        new_left = new_left.saturating_sub(1);
                        new_no += 1;
                        (None, Some(new_no - 1))
                    }
                    DiffLineKind::Removed => {
                        old_left = old_left.saturating_sub(1);
                        old_no += 1;
                        (Some(old_no - 1), None)
                    }
                };
                let hunk = files
                    .last_mut()
                    .and_then(|f| f.hunks.last_mut())
                    .expect("open hunk");
                hunk.lines.push((kind, old, new, text));
                continue;
            }
            (old_left, new_left) = (0, 0);
        }
        if let Some(rest) = line.strip_prefix("diff --git ") {
            // Best guess from `a/x b/y`; ---/+++ and rename lines refine it.
            let (old, new) = rest
                .split_once(" b/")
                .map_or((None, None), |(a, b)| (header_path(a), Some(b)));
            files.push(PatchFile {
                old_path: old,
                new_path: new,
                ..Default::default()
            });
        } else if let Some(rest) = line.strip_prefix("--- ") {
            match files.last_mut() {
                Some(f) if f.hunks.is_empty() && !f.seen_minus => {
                    f.old_path = header_path(rest);
                    f.seen_minus = true;
                }
                _ => files.push(PatchFile {
                    old_path: header_path(rest),
                    seen_minus: true,
                    ..Default::default()
                }),
            }
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            if let Some(f) = files.last_mut().filter(|f| f.hunks.is_empty()) {
                f.new_path = header_path(rest);
            }
        } else if let Some(rest) = line
            .strip_prefix("rename from ")
            .or_else(|| line.strip_prefix("copy from "))
        {
            if let Some(f) = files.last_mut() {
                f.old_path = Some(rest);
            }
        } else if let Some(rest) = line
            .strip_prefix("rename to ")
            .or_else(|| line.strip_prefix("copy to "))
        {
            if let Some(f) = files.last_mut() {
                f.new_path = Some(rest);
            }
        } else if line.starts_with("new file mode") {
            if let Some(f) = files.last_mut() {
                f.old_path = None;
            }
        } else if line.starts_with("deleted file mode") {
            if let Some(f) = files.last_mut() {
                f.new_path = None;
            }
        } else if let Some((a, b, c, d)) = hunk_range(line) {
            if files.is_empty() {
                files.push(PatchFile::default());
            }
            let file = files.last_mut().expect("file");
            file.hunks.push(PatchHunk {
                header: line,
                lines: Vec::new(),
            });
            (old_left, new_left, old_no, new_no) = (b, d, a, c);
        }
    }
    files
}

/// Whether a patch names at least one file or hunk (sideshow rejects ones that don't).
pub fn patch_has_content(patch: &str) -> bool {
    parse_patch(patch)
        .iter()
        .any(|f| f.old_path.is_some() || f.new_path.is_some() || !f.hunks.is_empty())
}

fn lang_for(filename: &str, language: Option<&str>) -> (Option<Lang>, Option<String>) {
    let lang = language
        .and_then(Lang::from_name)
        .or_else(|| Lang::from_filename(filename));
    (
        lang,
        lang.map(|l| l.name().to_owned())
            .or_else(|| language.map(str::to_owned)),
    )
}

fn line_at(lines: &[StyledLine], i: usize) -> StyledLine {
    lines.get(i).cloned().unwrap_or_default()
}

/// Render every file of a patch. Each side of a file (context+removed,
/// context+added) is highlighted as one text, so multi-line constructs
/// inside a hunk stay right; all sides highlight in parallel.
pub fn render_patch(patch: &str) -> Vec<DiffFileRender> {
    let files = parse_patch(patch);
    let langs: Vec<_> = files.iter().map(|f| lang_for(f.filename(), None)).collect();
    let sides: Vec<(String, String)> = files
        .iter()
        .map(|file| {
            let (mut old_text, mut new_text) = (String::new(), String::new());
            for (kind, _, _, text) in file.hunks.iter().flat_map(|h| &h.lines) {
                if *kind != DiffLineKind::Added {
                    old_text.push_str(text);
                    old_text.push('\n');
                }
                if *kind != DiffLineKind::Removed {
                    new_text.push_str(text);
                    new_text.push('\n');
                }
            }
            (old_text, new_text)
        })
        .collect();
    let jobs: Vec<_> = langs
        .iter()
        .zip(&sides)
        .flat_map(|((lang, _), (old, new))| [(*lang, old.as_str()), (*lang, new.as_str())])
        .collect();
    let mut highlighted = highlight_many(&jobs).into_iter();
    files
        .iter()
        .zip(langs)
        .map(|(file, (_, language))| {
            let old_hl = highlighted.next().expect("old side");
            let new_hl = highlighted.next().expect("new side");
            let (mut oi, mut ni, mut added, mut removed) = (0, 0, 0, 0);
            let hunks = file
                .hunks
                .iter()
                .map(|h| DiffHunk {
                    header: h.header.to_owned(),
                    lines: h
                        .lines
                        .iter()
                        .map(|&(kind, old_line, new_line, _)| {
                            let spans = match kind {
                                DiffLineKind::Context => {
                                    (oi, ni) = (oi + 1, ni + 1);
                                    line_at(&new_hl, ni - 1)
                                }
                                DiffLineKind::Added => {
                                    (ni, added) = (ni + 1, added + 1);
                                    line_at(&new_hl, ni - 1)
                                }
                                DiffLineKind::Removed => {
                                    (oi, removed) = (oi + 1, removed + 1);
                                    line_at(&old_hl, oi - 1)
                                }
                            };
                            DiffLine {
                                kind,
                                old_line,
                                new_line,
                                spans,
                            }
                        })
                        .collect(),
                })
                .collect();
            DiffFileRender {
                filename: file.filename().to_owned(),
                language,
                added,
                removed,
                hunks,
            }
        })
        .collect()
}

/// Diff before/after pairs with 3 lines of context, highlighting every whole
/// file (in parallel) so each line is coloured in its real context.
pub fn render_files(files: &[DiffFile]) -> Vec<DiffFileRender> {
    let langs: Vec<_> = files
        .iter()
        .map(|f| lang_for(&f.filename, f.language.as_deref()))
        .collect();
    let jobs: Vec<_> = files
        .iter()
        .zip(&langs)
        .flat_map(|(f, (lang, _))| [(*lang, f.before.as_str()), (*lang, f.after.as_str())])
        .collect();
    let mut highlighted = highlight_many(&jobs).into_iter();
    files
        .iter()
        .zip(langs)
        .map(|(file, (_, language))| {
            let before = highlighted.next().expect("before");
            let after = highlighted.next().expect("after");
            let diff = similar::TextDiff::configure()
                .timeout(Duration::from_millis(500))
                .diff_lines(&file.before, &file.after);
            let (mut added, mut removed) = (0, 0);
            let hunks = diff
                .unified_diff()
                .context_radius(3)
                .iter_hunks()
                .map(|hunk| DiffHunk {
                    header: hunk.header().to_string(),
                    lines: hunk
                        .iter_changes()
                        .map(|change| {
                            let (old, new) = (change.old_index(), change.new_index());
                            let (kind, spans) = match change.tag() {
                                similar::ChangeTag::Equal => {
                                    (DiffLineKind::Context, line_at(&after, new.unwrap_or(0)))
                                }
                                similar::ChangeTag::Insert => {
                                    added += 1;
                                    (DiffLineKind::Added, line_at(&after, new.unwrap_or(0)))
                                }
                                similar::ChangeTag::Delete => {
                                    removed += 1;
                                    (DiffLineKind::Removed, line_at(&before, old.unwrap_or(0)))
                                }
                            };
                            let num = |i: Option<usize>| i.map(|i| i as u32 + 1);
                            DiffLine {
                                kind,
                                old_line: num(old),
                                new_line: num(new),
                                spans,
                            }
                        })
                        .collect(),
                })
                .collect();
            DiffFileRender {
                filename: file.filename.clone(),
                language,
                added,
                removed,
                hunks,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIT_PATCH: &str = "From abc Mon Sep 17 00:00:00 2001
Subject: [PATCH] stuff

diff --git a/src/old.rs b/src/new.rs
similarity index 90%
rename from src/old.rs
rename to src/new.rs
index 111..222 100644
--- a/src/old.rs
+++ b/src/new.rs
@@ -1,4 +1,4 @@ fn top()
 fn a() {}
--- not a header, a removed line
+-- an added line
 fn c() {}

diff --git a/docs/added.md b/docs/added.md
new file mode 100644
index 0000000..333
--- /dev/null
+++ b/docs/added.md
@@ -0,0 +1,2 @@
+# Title
+body
\\ No newline at end of file
diff --git a/gone.py b/gone.py
deleted file mode 100644
--- a/gone.py
+++ /dev/null
@@ -1 +0,0 @@
-print(1)
diff --git a/x.toml b/x.toml
--- a/x.toml
+++ b/x.toml
@@ -1,2 +1,2 @@
-a = 1
\\ No newline at end of file
+a = 2
\\ No newline at end of file
@@ -10,3 +10,3 @@
 k = 1

 m = 3
";

    #[test]
    fn multi_file_git_patch_parses_renames_new_deleted_and_no_newline() {
        let files = parse_patch(GIT_PATCH);
        assert_eq!(files.len(), 4);
        assert_eq!(
            (files[0].old_path, files[0].new_path),
            (Some("src/old.rs"), Some("src/new.rs"))
        );
        assert_eq!(files[0].hunks[0].header, "@@ -1,4 +1,4 @@ fn top()");
        let kinds: Vec<_> = files[0].hunks[0]
            .lines
            .iter()
            .map(|l| (l.0, l.1, l.2))
            .collect();
        use DiffLineKind::*;
        assert_eq!(
            kinds,
            [
                (Context, Some(1), Some(1)),
                (Removed, Some(2), None),
                (Added, None, Some(2)),
                (Context, Some(3), Some(3)),
                (Context, Some(4), Some(4))
            ]
        );
        assert_eq!(
            files[0].hunks[0].lines[1].3,
            "-- not a header, a removed line"
        );
        assert_eq!(
            (files[1].old_path, files[1].new_path),
            (None, Some("docs/added.md"))
        );
        assert_eq!(files[1].hunks[0].lines.len(), 2);
        assert_eq!((files[2].filename(), files[2].new_path), ("gone.py", None));
        assert_eq!(files[3].hunks.len(), 2);
        assert_eq!(
            files[3].hunks[0].lines.len(),
            2,
            "no-newline markers are not lines"
        );
        // The blank line in hunk 2 is a context line whose space was stripped.
        assert_eq!(files[3].hunks[1].lines.len(), 3);
        assert_eq!(files[3].hunks[1].lines[2].1, Some(12));
    }

    #[test]
    fn rendered_patch_counts_and_highlights_by_filename() {
        let files = render_patch(GIT_PATCH);
        let names: Vec<_> = files.iter().map(|f| f.filename.as_str()).collect();
        assert_eq!(names, ["src/new.rs", "docs/added.md", "gone.py", "x.toml"]);
        assert_eq!((files[0].added, files[0].removed), (1, 1));
        assert_eq!(files[0].language.as_deref(), Some("rust"));
        assert_eq!(files[1].language.as_deref(), Some("markdown"));
        assert_eq!(
            (
                files[2].added,
                files[2].removed,
                files[2].language.as_deref()
            ),
            (0, 1, Some("python"))
        );
        let first = &files[0].hunks[0].lines[0].spans;
        assert_eq!(first[0].text, "fn");
        assert_eq!(first[0].class.as_deref(), Some("keyword"));
        assert_eq!(
            files[2].hunks[0].lines[0]
                .spans
                .iter()
                .map(|s| s.text.as_str())
                .collect::<String>(),
            "print(1)"
        );
    }

    #[test]
    fn plain_unified_multi_file_and_bare_hunks() {
        let patch = "--- a.js\t2024-01-01\n+++ a.js\t2024-01-02\n@@ -1 +1 @@\n-var a\n+let a\n--- b.js\n+++ b.js\n@@ -3,0 +4 @@\n+x\n";
        let files = parse_patch(patch);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].new_path, Some("a.js"));
        assert_eq!(
            files[1].hunks[0].lines[0],
            (DiffLineKind::Added, None, Some(4), "x")
        );
        assert!(patch_has_content("@@ -1 +1 @@\n-a\n+b\n"));
        assert!(!patch_has_content("just some text\n"));
    }

    #[test]
    fn before_after_pair_diffs_with_line_numbers() {
        let before: String = (1..=20).map(|i| format!("let v{i} = {i};\n")).collect();
        let after = before.replace("let v10 = 10;\n", "let v10 = 100;\nlet extra = 0;\n");
        let file = DiffFile {
            filename: "m.js".into(),
            before,
            after,
            language: None,
        };
        let r = render_files(std::slice::from_ref(&file)).remove(0);
        assert_eq!(
            (r.added, r.removed, r.language.as_deref()),
            (2, 1, Some("javascript"))
        );
        assert_eq!(r.hunks.len(), 1);
        assert_eq!(r.hunks[0].header, "@@ -7,7 +7,8 @@");
        let removed = r.hunks[0]
            .lines
            .iter()
            .find(|l| l.kind == DiffLineKind::Removed)
            .unwrap();
        assert_eq!(removed.old_line, Some(10));
        assert_eq!(removed.spans[0].class.as_deref(), Some("keyword"));
        let explicit = render_files(&[DiffFile {
            language: Some("rs".into()),
            ..file
        }])
        .remove(0);
        assert_eq!(explicit.language.as_deref(), Some("rust"));
    }
}
