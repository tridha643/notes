//! Syntax highlighting to token-class spans with Pierre's Highlights
//! (`pierre/highlights.wasm`, @pierre/highlights 0.2.0, Apache-2.0), a lexer
//! written in WebAssembly text that covers 80 languages. It runs in wasmi;
//! the module compiles once per process and each thread keeps an instance.

use std::{cell::RefCell, sync::OnceLock};

use notes_proto::posts::{Span, StyledLine};
use wasmi::{Caller, Config, Engine, Instance, Linker, Memory, Module, Store, TypedFunc};

const WASM: &[u8] = include_bytes!("../pierre/highlights.wasm");
/// Every alias Highlights resolves, `name id` per line (its `languages.ts`).
const LANGUAGES: &str = include_str!("../pierre/languages.txt");
/// Input starts at the second wasm page; the header lives in the first.
const PAGE: usize = 65536;
/// A record whose token id is this marks a line break.
const LINE_BREAK: u32 = u32::MAX;

/// Span class per Highlights token id (its `token-types.ts`, in order). Plain
/// identifiers and punctuation get no class: they're the default text colour.
fn class_for(token: u32) -> Option<&'static str> {
    const TYPES: [&str; 73] = [
        "none", "attribute", "attribute.jsx", "boolean", "comment", "comment.doc", "constant",
        "constant.builtin", "constructor", "diff.minus", "diff.plus", "embedded", "emphasis",
        "emphasis.strong", "function", "function.definition", "function.method", "keyword",
        "keyword.control", "keyword.declaration", "keyword.import", "keyword.operator", "label",
        "namespace", "number", "operator", "predictive", "preproc", "primary", "property",
        "property.json_key", "punctuation", "punctuation.bracket", "punctuation.bracket.html",
        "punctuation.bracket.jsx", "punctuation.delimiter", "punctuation.delimiter.html",
        "punctuation.delimiter.jsx", "punctuation.list_marker", "punctuation.markup",
        "punctuation.special", "selector", "selector.class", "selector.id", "string",
        "string.escape", "string.regex", "string.special", "string.special.symbol", "tag",
        "tag.doctype", "tag.jsx", "text.jsx", "text.literal", "title", "type", "type.builtin",
        "type.class", "variable", "variable.parameter", "variable.special", "enum", "hint",
        "keyword.jsdoc", "link_text", "link_uri", "selector.pseudo", "tag.component.jsx",
        "type.jsdoc", "variable.jsdoc", "variant", "background", "foreground",
    ];
    let name = *TYPES.get(token as usize)?;
    Some(match name {
        "string.escape" => "escape",
        "variable.parameter" | "variable.jsdoc" => "parameter",
        "variable.special" => "variable",
        "boolean" | "variant" => "constant",
        "constructor" | "enum" => "type",
        "hint" => "comment",
        "namespace" => "module",
        "preproc" | "title" => "keyword",
        "text.literal" | "link_text" | "link_uri" => "string",
        _ => match name.split('.').next().unwrap_or(name) {
            group @ ("attribute" | "comment" | "constant" | "function" | "keyword" | "label"
            | "number" | "operator" | "property" | "string" | "tag" | "type") => group,
            "selector" => "tag",
            _ => return None,
        },
    })
}

/// A Highlights language (its wasm id) with a canonical display name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lang {
    id: u8,
    name: &'static str,
}

fn aliases() -> impl Iterator<Item = (&'static str, u8)> {
    LANGUAGES.lines().filter_map(|l| {
        let (name, id) = l.split_once(' ')?;
        Some((name, id.parse().ok()?))
    })
}

impl Lang {
    /// Resolve a language id or alias (`rs`, `ts`, `py`, `sh`, ...). Plain
    /// text resolves to `None`, so it's never sent through the lexer.
    pub fn from_name(name: &str) -> Option<Lang> {
        let name = name.trim().to_ascii_lowercase();
        let id = aliases().find(|(alias, _)| *alias == name)?.1;
        // The longest alias names the language (`rust` over `rs`).
        let canonical = aliases().filter(|a| a.1 == id).map(|a| a.0).max_by_key(|a| a.len())?;
        (id != 0).then_some(Lang { id, name: canonical })
    }

    /// Infer from a file path's name or extension.
    pub fn from_filename(path: &str) -> Option<Lang> {
        let name = path.rsplit('/').next().unwrap_or(path);
        let special = match name {
            "Gemfile" | "Rakefile" | "Podfile" | "Fastfile" => Some("ruby"),
            "Cargo.lock" | "Pipfile" => Some("toml"),
            ".bashrc" | ".zshrc" | ".profile" | ".bash_profile" | ".envrc" => Some("bash"),
            "Dockerfile" | "Containerfile" => Some("dockerfile"),
            "Makefile" | "makefile" | "GNUmakefile" => Some("make"),
            _ => None,
        };
        special.map_or_else(|| Lang::from_name(name.rsplit_once('.')?.1), Lang::from_name)
    }

    pub fn name(self) -> &'static str {
        self.name
    }
}

/// One wasm instance and the views into it, per thread.
struct Lexer {
    store: Store<()>,
    memory: Memory,
    run: TypedFunc<(), ()>,
}

impl Lexer {
    fn new() -> Option<Lexer> {
        static MODULE: OnceLock<Option<(Engine, Module)>> = OnceLock::new();
        let (engine, module) = MODULE
            .get_or_init(|| {
                let mut config = Config::default();
                config.wasm_simd(true);
                let engine = Engine::new(&config);
                let module = Module::new(&engine, WASM).ok()?;
                Some((engine, module))
            })
            .as_ref()?;
        let mut store = Store::new(engine, ());
        let mut linker = <Linker<()>>::new(engine);
        linker.func_wrap("env", "is_id_start", |c: Caller<'_, ()>, p: u32, n: u32| ident(c, p, n, true)).ok()?;
        linker.func_wrap("env", "is_id_continue", |c: Caller<'_, ()>, p: u32, n: u32| ident(c, p, n, false)).ok()?;
        let instance: Instance = linker.instantiate_and_start(&mut store, module).ok()?;
        let memory = instance.get_memory(&store, "memory")?;
        let run = instance.get_typed_func(&store, "highlight").ok()?;
        Some(Lexer { store, memory, run })
    }

    /// Lex `text` and return its records: `(end, token)` pairs with UTF-16
    /// end offsets, a `LINE_BREAK` token marking where the next line starts.
    fn records(&mut self, lang: Lang, text: &str) -> Option<Vec<(u32, u32)>> {
        let len = text.len();
        let pages = 1 + (len + 96).div_ceil(PAGE);
        let have = self.memory.size(&self.store) as usize;
        if pages > have {
            self.memory.grow(&mut self.store, (pages - have) as u64).ok()?;
        }
        let mem = self.memory.data_mut(&mut self.store);
        mem[0] = lang.id;
        mem[1] = 3; // output mode: UTF-16 line records
        mem[2..6].copy_from_slice(&u32::try_from(len).ok()?.to_le_bytes());
        mem[PAGE..PAGE + len].copy_from_slice(text.as_bytes());
        mem[PAGE + len] = 0;
        self.run.call(&mut self.store, ()).ok()?;
        let mem = self.memory.data(&self.store);
        let word = |at: usize| u32::from_le_bytes(mem[at..at + 4].try_into().expect("4 bytes"));
        let (at, n) = (word(6) as usize, word(10) as usize);
        let out = mem.get(at..at + n)?;
        let (records, _) = out.as_chunks::<4>();
        let words: Vec<u32> = records.iter().map(|w| u32::from_le_bytes(*w)).collect();
        Some(words.as_chunks::<2>().0.iter().map(|&[end, token]| (end, token)).collect())
    }
}

/// The wasm asks about non-ASCII identifier characters (`len` UTF-8 bytes at `ptr`).
fn ident(caller: Caller<'_, ()>, ptr: u32, len: u32, start: bool) -> u32 {
    let Some(memory) = caller.get_export("memory").and_then(|e| e.into_memory()) else {
        return 0;
    };
    let bytes = memory.data(&caller).get(ptr as usize..(ptr + len) as usize).unwrap_or_default();
    let ch = std::str::from_utf8(bytes).ok().and_then(|s| s.chars().next());
    ch.is_some_and(|c| if start { unicode_ident::is_xid_start(c) } else { unicode_ident::is_xid_continue(c) }) as u32
}

/// Split `text` into lines of class-tagged spans. `None` (or a lexer
/// failure) yields plain unstyled lines. A trailing newline doesn't add a line.
pub fn highlight(lang: Option<Lang>, text: &str) -> Vec<StyledLine> {
    thread_local!(static LEXER: RefCell<Option<Lexer>> = RefCell::new(Lexer::new()));
    let records = lang.and_then(|lang| {
        LEXER.with_borrow_mut(|lexer| lexer.as_mut()?.records(lang, text))
    });
    let mut out = LineBuilder::default();
    let Some(records) = records else {
        out.push(text, None);
        return out.finish();
    };
    // Walk the text once, turning UTF-16 offsets into byte offsets.
    let (mut chars, mut units, mut at) = (text.char_indices().peekable(), 0u32, 0usize);
    for (end, token) in records {
        let from = at;
        while units < end {
            let Some((_, c)) = chars.next() else { break };
            units += c.len_utf16() as u32;
            at = chars.peek().map_or(text.len(), |(i, _)| *i);
        }
        let piece = &text[from..at];
        let class = if token == LINE_BREAK { None } else { class_for(token) };
        // Highlights folds whitespace into the neighbouring token; keep it
        // unclassed so plain runs merge and spans stay tight.
        let core = piece.trim();
        if class.is_none() || core.is_empty() {
            out.push(piece, None);
        } else {
            let lead = piece.len() - piece.trim_start().len();
            out.push(&piece[..lead], None);
            out.push(core, class);
            out.push(&piece[lead + core.len()..], None);
        }
    }
    out.push(&text[at..], None);
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

    fn lang(name: &str) -> Option<Lang> {
        Lang::from_name(name)
    }

    #[test]
    fn rust_tokens_get_classes() {
        let lines = highlight(lang("rust"), "fn main() {\n    // hi\n    let s = \"x\";\n}\n");
        assert_eq!(lines.len(), 4);
        let spans = classes(&lines);
        assert!(spans.contains(&("fn".into(), Some("keyword".into()))), "{spans:?}");
        assert!(spans.contains(&("main".into(), Some("function".into()))), "{spans:?}");
        assert!(spans.contains(&("// hi".into(), Some("comment".into()))), "{spans:?}");
        assert!(spans.contains(&("\"x\"".into(), Some("string".into()))), "{spans:?}");
        // Text round-trips exactly.
        let text: Vec<String> = lines
            .iter()
            .map(|l| l.iter().map(|s| s.text.as_str()).collect())
            .collect();
        assert_eq!(text, ["fn main() {", "    // hi", "    let s = \"x\";", "}"]);
    }

    #[test]
    fn non_ascii_text_round_trips() {
        let src = "let café = \"naïve 🚀\"; // ok ✓\nlet ä = 1;\n";
        let lines = highlight(lang("rust"), src);
        let text: String = lines
            .iter()
            .map(|l| l.iter().map(|s| s.text.as_str()).collect::<String>() + "\n")
            .collect();
        assert_eq!(text, src);
        assert!(classes(&lines).contains(&("\"naïve 🚀\"".into(), Some("string".into()))));
    }

    #[test]
    fn html_highlights_embedded_javascript() {
        let lines = highlight(lang("html"), "<script>let x = 1;</script>\n");
        assert!(classes(&lines).contains(&("let".into(), Some("keyword".into()))), "{:?}", classes(&lines));
    }

    #[test]
    fn unknown_language_is_plain_and_crlf_is_trimmed() {
        assert_eq!(Lang::from_name("brainfuck"), None);
        assert_eq!(Lang::from_name("text"), None);
        let lines = highlight(None, "a\r\n\r\nb");
        assert_eq!(
            lines,
            vec![
                vec![Span { text: "a".into(), class: None }],
                vec![],
                vec![Span { text: "b".into(), class: None }]
            ]
        );
    }

    #[test]
    fn parallel_batches_match_serial_in_order() {
        let big = "fn f() { let x = \"s\"; }\n".repeat(2000);
        let jobs = [(lang("rust"), big.as_str()), (None, "plain\n"), (lang("python"), "def g(): pass\n")];
        let serial: Vec<_> = jobs.iter().map(|(l, t)| highlight(*l, t)).collect();
        assert_eq!(highlight_many(&jobs), serial);
    }

    #[test]
    fn languages_resolve_from_names_and_filenames() {
        assert_eq!(Lang::from_name("TS").map(Lang::name), Some("typescript"));
        assert_eq!(Lang::from_name("rs").map(Lang::name), Some("rust"));
        assert_eq!(Lang::from_filename("src/app.tsx").map(Lang::name), Some("tsx"));
        assert_eq!(Lang::from_filename("a/b/Gemfile").map(Lang::name), Some("ruby"));
        assert_eq!(Lang::from_filename("notes.md").map(Lang::name), Some("markdown"));
        assert_eq!(Lang::from_filename("LICENSE"), None);
    }
}
