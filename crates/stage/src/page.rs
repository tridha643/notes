//! The document stage loads an html part into: stage's palette as sideshow's
//! theme tokens, sideshow's base styles and SVG classes, the part's kits, then
//! the part itself. It mirrors Notes.app's shell (apps/Notes/Sources/NotesUI/
//! WebParts.swift), so a post written to sideshow's guide renders the same in
//! both. The defaults sit in a cascade layer, so any rule the agent writes wins
//! over them; only media is held to the card's width.

use notes_proto::kits::kits;

/// A full page for an html part (a body fragment or a whole document).
pub fn document(html: &str, kit_ids: &[String]) -> String {
    let chosen: Vec<_> = kits().into_iter().filter(|k| kit_ids.contains(&k.id)).collect();
    let css: String = chosen.iter().map(|k| k.css.as_str()).collect();
    let js: String = chosen.iter().filter_map(|k| k.js.as_deref()).map(|js| format!("<script>{js}</script>")).collect();
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\">\
         <style>@layer stage{{{TOKENS}{BASE}}}{FIT}{css}</style></head><body>{SVG_DEFS}\n{html}\n{js}</body></html>"
    )
}

/// stage's palette (crates/stage/src/ui.rs) under sideshow's token names.
const TOKENS: &str = ":root{color-scheme:dark;\
--color-background-primary:#1e1e2e;--color-background-secondary:#313244;--color-background-tertiary:#181825;\
--color-background-info:rgba(137,180,250,.16);--color-background-success:rgba(166,227,161,.16);\
--color-background-warning:rgba(249,226,175,.16);--color-background-danger:rgba(243,139,168,.16);\
--color-text-primary:#cdd6f4;--color-text-secondary:#a6adc8;--color-text-tertiary:#7f849c;\
--color-text-info:#89b4fa;--color-text-success:#a6e3a1;--color-text-warning:#f9e2af;--color-text-danger:#f38ba8;\
--color-border-primary:#7f849c;--color-border-secondary:#45475a;--color-border-tertiary:#45475a;\
--color-border-info:#89b4fa;--color-border-success:#a6e3a1;--color-border-warning:#f9e2af;--color-border-danger:#f38ba8;\
--c-teal-bg:rgba(148,226,213,.16);--c-teal-line:#94e2d5;--c-teal-text:#94e2d5;\
--c-coral-bg:rgba(250,179,135,.16);--c-coral-line:#fab387;--c-coral-text:#fab387;\
--font-sans:-apple-system,BlinkMacSystemFont,\"Helvetica Neue\",sans-serif;--font-serif:\"Iowan Old Style\",Georgia,serif;\
--font-mono:ui-monospace,SFMono-Regular,Menlo,monospace;\
--border-radius-md:8px;--border-radius-lg:12px;--border-radius-xl:16px}";

/// From sideshow's server/surfacePage.ts, as Notes.app has it, at a card's text size.
const BASE: &str = "html{box-sizing:border-box}*,*::before,*::after{box-sizing:inherit}\
body{margin:0;padding:12px;background:var(--color-background-primary);color:var(--color-text-primary);font:14px/1.5 var(--font-sans)}\
code,pre,kbd{font-family:var(--font-mono)}\
button{font:500 14px/1.4 var(--font-sans);color:var(--color-text-primary);background:none;border:0.5px solid var(--color-border-secondary);border-radius:var(--border-radius-md);padding:6px 14px}\
input:not([type=checkbox]):not([type=radio]):not([type=range]),select,textarea{font:14px/1.4 var(--font-sans);color:var(--color-text-primary);background:var(--color-background-primary);border:0.5px solid var(--color-border-secondary);border-radius:var(--border-radius-md);padding:6px 10px}\
input[type=checkbox],input[type=radio],input[type=range],progress{accent-color:var(--color-border-info)}\
svg{font-family:var(--font-sans);fill:var(--color-text-primary)}\
.t{font-size:14px}.ts{font-size:12px;fill:var(--color-text-secondary)}.th{font-size:14px;font-weight:500}\
.box{fill:var(--color-background-secondary);stroke:var(--color-border-tertiary);rx:8px}\
.arr{stroke:var(--color-text-secondary);stroke-width:1.2;fill:none}\
.leader{stroke:var(--color-border-secondary);stroke-width:1;stroke-dasharray:3 4;fill:none}\
.c-blue,.c-blue .box{fill:var(--color-background-info);stroke:var(--color-border-info)}.c-blue text,text.c-blue{fill:var(--color-text-info);stroke:none}\
.c-teal,.c-teal .box{fill:var(--c-teal-bg);stroke:var(--c-teal-line)}.c-teal text,text.c-teal{fill:var(--c-teal-text);stroke:none}\
.c-amber,.c-amber .box{fill:var(--color-background-warning);stroke:var(--color-border-warning)}.c-amber text,text.c-amber{fill:var(--color-text-warning);stroke:none}\
.c-coral,.c-coral .box{fill:var(--c-coral-bg);stroke:var(--c-coral-line)}.c-coral text,text.c-coral{fill:var(--c-coral-text);stroke:none}\
.c-green,.c-green .box{fill:var(--color-background-success);stroke:var(--color-border-success)}.c-green text,text.c-green{fill:var(--color-text-success);stroke:none}\
.c-red,.c-red .box{fill:var(--color-background-danger);stroke:var(--color-border-danger)}.c-red text,text.c-red{fill:var(--color-text-danger);stroke:none}\
.c-gray,.c-gray .box{fill:var(--color-background-secondary);stroke:var(--color-border-secondary)}.c-gray text,text.c-gray{fill:var(--color-text-secondary);stroke:none}";

/// Outside the layer: pictures and scalable SVG shrink to the card instead of
/// widening the page, which would zoom the whole part out.
const FIT: &str = "img,video,canvas,svg[viewBox]{max-width:100%;height:auto}";

const SVG_DEFS: &str = r#"<svg width="0" height="0" style="position:absolute" aria-hidden="true"><defs><marker id="arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="6.5" markerHeight="6.5" orient="auto-start-reverse"><path d="M0 0L10 5L0 10z" fill="context-stroke"/></marker></defs></svg>"#;

#[cfg(test)]
mod tests {
    use super::document;

    #[test]
    fn wraps_the_part_with_tokens_and_only_the_kits_it_asks_for() {
        let plain = document("<p id=t>hi</p>", &[]);
        assert!(plain.starts_with("<!doctype html>") && plain.contains("<p id=t>hi</p>"));
        assert!(plain.contains("--color-text-primary:#cdd6f4") && plain.contains("marker id=\"arrow\""));
        assert!(!plain.contains(".tree{"));
        let kit = document("<ul class=tree></ul>", &["issues".into()]);
        assert!(kit.contains(".tree{") && !kit.contains(".deck{"));
        // The agent's own styles come after the layered defaults, so they win.
        assert!(kit.find("@layer stage").unwrap() < kit.find("<ul class=tree>").unwrap());
    }
}
