//! Syntax highlighting to token-class spans with tree-sitter-highlight.
//!
//! Each language's query set compiles once, on first use, into a `'static`
//! `OnceLock`; the parser and cursors are reused per thread.

use std::{cell::RefCell, sync::OnceLock};

use notes_proto::posts::{Span, StyledLine};
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

/// Capture names we ask tree-sitter for; it picks the longest configured
/// prefix of each query capture (`function.method.call` → `function.method`).
const CAPTURES: &[&str] = &[
    "attribute",
    "boolean",
    "comment",
    "constant",
    "constant.builtin",
    "constructor",
    "escape",
    "function",
    "function.builtin",
    "function.macro",
    "function.method",
    "keyword",
    "label",
    "module",
    "number",
    "operator",
    "property",
    "punctuation",
    "string",
    "string.special",
    "tag",
    "type",
    "type.builtin",
    "variable",
    "variable.builtin",
    "variable.parameter",
];

/// Span class per capture index. Plain identifiers and punctuation get no
/// class: they're the default text colour and would only bloat the output.
fn class_for(capture: usize) -> Option<&'static str> {
    Some(match CAPTURES[capture] {
        "variable" | "punctuation" => return None,
        "boolean" | "constant.builtin" => "constant",
        "constructor" | "type.builtin" => "type",
        "string.special" => "string",
        "function.builtin" | "function.macro" | "function.method" => "function",
        "variable.builtin" => "variable",
        "variable.parameter" => "parameter",
        name => name,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Bash,
    C,
    Cpp,
    Css,
    Go,
    Html,
    Java,
    Javascript,
    Json,
    Python,
    Ruby,
    Rust,
    Swift,
    Toml,
    Tsx,
    Typescript,
    Yaml,
}

pub const ALL_LANGS: &[Lang] = &[
    Lang::Bash,
    Lang::C,
    Lang::Cpp,
    Lang::Css,
    Lang::Go,
    Lang::Html,
    Lang::Java,
    Lang::Javascript,
    Lang::Json,
    Lang::Python,
    Lang::Ruby,
    Lang::Rust,
    Lang::Swift,
    Lang::Toml,
    Lang::Tsx,
    Lang::Typescript,
    Lang::Yaml,
];

impl Lang {
    /// Resolve a language id or common alias (`rs`, `ts`, `py`, `sh`, ...).
    pub fn from_name(name: &str) -> Option<Lang> {
        Some(match name.trim().to_ascii_lowercase().as_str() {
            "bash" | "sh" | "shell" | "zsh" | "shellscript" | "console" => Lang::Bash,
            "c" | "h" => Lang::C,
            "cpp" | "c++" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => Lang::Cpp,
            "css" | "scss" | "less" => Lang::Css,
            "go" | "golang" => Lang::Go,
            "html" | "htm" | "xhtml" | "vue" | "svelte" | "xml" | "svg" => Lang::Html,
            "java" => Lang::Java,
            "javascript" | "js" | "jsx" | "mjs" | "cjs" | "node" => Lang::Javascript,
            "json" | "jsonc" | "json5" | "jsonl" | "ndjson" => Lang::Json,
            "python" | "py" | "pyi" | "python3" => Lang::Python,
            "ruby" | "rb" | "rake" | "gemspec" => Lang::Ruby,
            "rust" | "rs" => Lang::Rust,
            "swift" => Lang::Swift,
            "toml" => Lang::Toml,
            "tsx" => Lang::Tsx,
            "typescript" | "ts" | "mts" | "cts" => Lang::Typescript,
            "yaml" | "yml" => Lang::Yaml,
            _ => return None,
        })
    }

    /// Infer from a file path's name or extension.
    pub fn from_filename(path: &str) -> Option<Lang> {
        let name = path.rsplit('/').next().unwrap_or(path);
        match name {
            "Gemfile" | "Rakefile" | "Podfile" | "Fastfile" => return Some(Lang::Ruby),
            "Cargo.lock" | "Pipfile" => return Some(Lang::Toml),
            ".bashrc" | ".zshrc" | ".profile" | ".bash_profile" | ".envrc" => {
                return Some(Lang::Bash)
            }
            "Package.swift" => return Some(Lang::Swift),
            _ => {}
        }
        let (_, ext) = name.rsplit_once('.')?;
        Lang::from_name(ext)
    }

    pub fn name(self) -> &'static str {
        match self {
            Lang::Bash => "bash",
            Lang::C => "c",
            Lang::Cpp => "cpp",
            Lang::Css => "css",
            Lang::Go => "go",
            Lang::Html => "html",
            Lang::Java => "java",
            Lang::Javascript => "javascript",
            Lang::Json => "json",
            Lang::Python => "python",
            Lang::Ruby => "ruby",
            Lang::Rust => "rust",
            Lang::Swift => "swift",
            Lang::Toml => "toml",
            Lang::Tsx => "tsx",
            Lang::Typescript => "typescript",
            Lang::Yaml => "yaml",
        }
    }

    /// The compiled query set, built on first use (a few ms per language).
    pub fn config(self) -> &'static HighlightConfiguration {
        static CONFIGS: [OnceLock<HighlightConfiguration>; 17] = [const { OnceLock::new() }; 17];
        CONFIGS[ALL_LANGS.iter().position(|l| *l == self).expect("listed")]
            .get_or_init(|| build(self))
    }
}

fn build(lang: Lang) -> HighlightConfiguration {
    use tree_sitter_javascript as js;
    use tree_sitter_typescript as ts;
    let (language, highlights, injections, locals): (tree_sitter::Language, String, &str, String) =
        match lang {
            Lang::Bash => (
                tree_sitter_bash::LANGUAGE.into(),
                tree_sitter_bash::HIGHLIGHT_QUERY.into(),
                "",
                String::new(),
            ),
            Lang::C => (
                tree_sitter_c::LANGUAGE.into(),
                tree_sitter_c::HIGHLIGHT_QUERY.into(),
                "",
                String::new(),
            ),
            Lang::Cpp => (
                tree_sitter_cpp::LANGUAGE.into(),
                [
                    tree_sitter_cpp::HIGHLIGHT_QUERY,
                    tree_sitter_c::HIGHLIGHT_QUERY,
                ]
                .concat(),
                "",
                String::new(),
            ),
            Lang::Css => (
                tree_sitter_css::LANGUAGE.into(),
                tree_sitter_css::HIGHLIGHTS_QUERY.into(),
                "",
                String::new(),
            ),
            Lang::Go => (
                tree_sitter_go::LANGUAGE.into(),
                tree_sitter_go::HIGHLIGHTS_QUERY.into(),
                "",
                String::new(),
            ),
            Lang::Html => (
                tree_sitter_html::LANGUAGE.into(),
                tree_sitter_html::HIGHLIGHTS_QUERY.into(),
                tree_sitter_html::INJECTIONS_QUERY,
                String::new(),
            ),
            Lang::Java => (
                tree_sitter_java::LANGUAGE.into(),
                tree_sitter_java::HIGHLIGHTS_QUERY.into(),
                "",
                String::new(),
            ),
            Lang::Javascript => (
                js::LANGUAGE.into(),
                [js::JSX_HIGHLIGHT_QUERY, js::HIGHLIGHT_QUERY].concat(),
                js::INJECTIONS_QUERY,
                js::LOCALS_QUERY.into(),
            ),
            Lang::Json => (
                tree_sitter_json::LANGUAGE.into(),
                tree_sitter_json::HIGHLIGHTS_QUERY.into(),
                "",
                String::new(),
            ),
            Lang::Python => (
                tree_sitter_python::LANGUAGE.into(),
                tree_sitter_python::HIGHLIGHTS_QUERY.into(),
                "",
                String::new(),
            ),
            Lang::Ruby => (
                tree_sitter_ruby::LANGUAGE.into(),
                tree_sitter_ruby::HIGHLIGHTS_QUERY.into(),
                "",
                tree_sitter_ruby::LOCALS_QUERY.into(),
            ),
            Lang::Rust => (
                tree_sitter_rust::LANGUAGE.into(),
                tree_sitter_rust::HIGHLIGHTS_QUERY.into(),
                tree_sitter_rust::INJECTIONS_QUERY,
                String::new(),
            ),
            Lang::Swift => (
                tree_sitter_swift::LANGUAGE.into(),
                tree_sitter_swift::HIGHLIGHTS_QUERY.into(),
                tree_sitter_swift::INJECTIONS_QUERY,
                tree_sitter_swift::LOCALS_QUERY.into(),
            ),
            Lang::Toml => (
                tree_sitter_toml_ng::LANGUAGE.into(),
                tree_sitter_toml_ng::HIGHLIGHTS_QUERY.into(),
                "",
                String::new(),
            ),
            Lang::Tsx => (
                ts::LANGUAGE_TSX.into(),
                [
                    ts::HIGHLIGHTS_QUERY,
                    js::JSX_HIGHLIGHT_QUERY,
                    js::HIGHLIGHT_QUERY,
                ]
                .concat(),
                js::INJECTIONS_QUERY,
                [ts::LOCALS_QUERY, js::LOCALS_QUERY].concat(),
            ),
            Lang::Typescript => (
                ts::LANGUAGE_TYPESCRIPT.into(),
                [ts::HIGHLIGHTS_QUERY, js::HIGHLIGHT_QUERY].concat(),
                js::INJECTIONS_QUERY,
                [ts::LOCALS_QUERY, js::LOCALS_QUERY].concat(),
            ),
            Lang::Yaml => (
                tree_sitter_yaml::LANGUAGE.into(),
                tree_sitter_yaml::HIGHLIGHTS_QUERY.into(),
                "",
                String::new(),
            ),
        };
    let mut config =
        HighlightConfiguration::new(language, lang.name(), &highlights, injections, &locals)
            .unwrap_or_else(|e| panic!("{} highlight queries compile: {e}", lang.name()));
    config.configure(CAPTURES);
    config
}

/// Split `text` into lines of class-tagged spans. `None` (or a highlighter
/// error) yields plain unstyled lines. A trailing newline doesn't add a line.
pub fn highlight(lang: Option<Lang>, text: &str) -> Vec<StyledLine> {
    let mut out = LineBuilder::default();
    let highlighted = lang.is_some_and(|lang| {
        thread_local!(static HL: RefCell<Highlighter> = RefCell::new(Highlighter::new()));
        HL.with_borrow_mut(|hl| {
            let Ok(events) = hl.highlight(lang.config(), text.as_bytes(), None, |name| {
                Lang::from_name(name).map(Lang::config)
            }) else {
                return false;
            };
            let mut stack: Vec<Option<&'static str>> = Vec::new();
            for event in events {
                match event {
                    Ok(HighlightEvent::HighlightStart(h)) => stack.push(class_for(h.0)),
                    Ok(HighlightEvent::HighlightEnd) => {
                        stack.pop();
                    }
                    Ok(HighlightEvent::Source { start, end }) => {
                        // Innermost classed capture wins; unclassed ones inherit.
                        let class = stack.iter().rev().find_map(|c| *c);
                        out.push(&text[start..end], class);
                    }
                    Err(_) => return false,
                }
            }
            true
        })
    });
    if !highlighted {
        out = LineBuilder::default();
        out.push(text, None);
    }
    out.finish()
}

/// [`highlight`] each job, spread over scoped threads once there's enough
/// text to beat the spawn cost (~20 µs a thread). Results keep job order.
pub fn highlight_many(jobs: &[(Option<Lang>, &str)]) -> Vec<Vec<StyledLine>> {
    let total: usize = jobs.iter().map(|(_, t)| t.len()).sum();
    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(jobs.len());
    if workers < 2 || total < 16 << 10 {
        return jobs
            .iter()
            .map(|(lang, text)| highlight(*lang, text))
            .collect();
    }
    // Longest jobs first, each to the least-loaded worker.
    let mut order: Vec<usize> = (0..jobs.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(jobs[i].1.len()));
    let mut queues: Vec<(usize, Vec<usize>)> = vec![(0, Vec::new()); workers];
    for i in order {
        let q = queues.iter_mut().min_by_key(|q| q.0).expect("workers > 0");
        q.0 += jobs[i].1.len();
        q.1.push(i);
    }
    let mut out = vec![Vec::new(); jobs.len()];
    std::thread::scope(|s| {
        let handles: Vec<_> = queues
            .iter()
            .map(|(_, q)| {
                s.spawn(move || {
                    q.iter()
                        .map(|&i| (i, highlight(jobs[i].0, jobs[i].1)))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        for handle in handles {
            for (i, lines) in handle.join().expect("highlight thread") {
                out[i] = lines;
            }
        }
    });
    out
}

/// Accumulates spans into lines, merging neighbours with the same class.
#[derive(Default)]
pub struct LineBuilder {
    lines: Vec<StyledLine>,
    line: StyledLine,
}

impl LineBuilder {
    pub fn push(&mut self, text: &str, class: Option<&str>) {
        let mut rest = text;
        loop {
            let (piece, more) = match rest.find('\n') {
                Some(i) => (&rest[..i], Some(&rest[i + 1..])),
                None => (rest, None),
            };
            let piece = if more.is_some() {
                piece.strip_suffix('\r').unwrap_or(piece)
            } else {
                piece
            };
            if !piece.is_empty() {
                match self.line.last_mut() {
                    Some(last) if last.class.as_deref() == class => last.text.push_str(piece),
                    _ => self.line.push(Span {
                        text: piece.to_owned(),
                        class: class.map(str::to_owned),
                    }),
                }
            }
            match more {
                Some(next) => {
                    self.lines.push(std::mem::take(&mut self.line));
                    rest = next;
                }
                None => break,
            }
        }
    }

    /// Break the current line (used by callers that feed text without `\n`).
    pub fn newline(&mut self) {
        self.lines.push(std::mem::take(&mut self.line));
    }

    pub fn finish(mut self) -> Vec<StyledLine> {
        if !self.line.is_empty() {
            self.lines.push(self.line);
        }
        self.lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classes(lines: &[StyledLine]) -> Vec<(String, Option<String>)> {
        lines
            .iter()
            .flatten()
            .map(|s| (s.text.clone(), s.class.clone()))
            .collect()
    }

    #[test]
    fn every_language_compiles_and_highlights() {
        for lang in ALL_LANGS {
            let lines = highlight(Some(*lang), "x = 1\n");
            assert_eq!(lines.len(), 1, "{}", lang.name());
        }
    }

    #[test]
    fn rust_tokens_get_classes() {
        let lines = highlight(
            Some(Lang::Rust),
            "fn main() {\n    // hi\n    let s = \"x\";\n}\n",
        );
        assert_eq!(lines.len(), 4);
        let spans = classes(&lines);
        assert!(spans.contains(&("fn".into(), Some("keyword".into()))));
        assert!(spans.contains(&("main".into(), Some("function".into()))));
        assert!(spans.contains(&("// hi".into(), Some("comment".into()))));
        assert!(spans.contains(&("\"x\"".into(), Some("string".into()))));
        // Text round-trips exactly.
        let text: Vec<String> = lines
            .iter()
            .map(|l| l.iter().map(|s| s.text.as_str()).collect())
            .collect();
        assert_eq!(
            text,
            ["fn main() {", "    // hi", "    let s = \"x\";", "}"]
        );
    }

    #[test]
    fn html_injects_javascript() {
        let lines = highlight(Some(Lang::Html), "<script>let x = 1;</script>\n");
        assert!(classes(&lines).contains(&("let".into(), Some("keyword".into()))));
    }

    #[test]
    fn unknown_language_is_plain_and_crlf_is_trimmed() {
        assert_eq!(Lang::from_name("brainfuck"), None);
        let lines = highlight(None, "a\r\n\r\nb");
        assert_eq!(
            lines,
            vec![
                vec![Span {
                    text: "a".into(),
                    class: None
                }],
                vec![],
                vec![Span {
                    text: "b".into(),
                    class: None
                }]
            ]
        );
    }

    #[test]
    fn parallel_batches_match_serial_in_order() {
        let big = "fn f() { let x = \"s\"; }\n".repeat(2000);
        let jobs = [
            (Some(Lang::Rust), big.as_str()),
            (None, "plain\n"),
            (Some(Lang::Python), "def g(): pass\n"),
        ];
        let serial: Vec<_> = jobs.iter().map(|(l, t)| highlight(*l, t)).collect();
        assert_eq!(highlight_many(&jobs), serial);
    }

    #[test]
    fn languages_resolve_from_names_and_filenames() {
        assert_eq!(Lang::from_name("TS"), Some(Lang::Typescript));
        assert_eq!(Lang::from_filename("src/app.tsx"), Some(Lang::Tsx));
        assert_eq!(Lang::from_filename("a/b/Gemfile"), Some(Lang::Ruby));
        assert_eq!(Lang::from_filename("Makefile"), None);
        assert_eq!(Lang::from_filename("notes.md"), None);
    }
}
