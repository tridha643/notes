// Pure pieces behind the post view: the json tree, trace parsing and folding,
// pele in JavaScriptCore, and span → attributed text for code, diff and terminal.

import AppKit
import Foundation
import Testing
@testable import NotesCore
@testable import NotesUI

@Test func jsonTreeBuildsLazilyAndPagesLargeArrays() throws {
    let root = JSONNode(value: try JSONValue.parse(Data(#"{"name":"x","tags":[],"nested":{"a":1},"n":null}"#.utf8)))
    #expect(root.summary == "{4 keys}")
    #expect(root.children.map(\.key) == ["name", "tags", "nested", "n"])
    #expect(root.children.map(\.summary) == ["\"x\"", "[]", "{1 key}", "null"])
    #expect(root.children.map(\.isExpandable) == [false, false, true, false])

    let big = JSONNode(value: .array((0..<250).map { .number(String($0)) }))
    #expect(big.summary == "[250 items]")
    #expect(big.children.map(\.key) == ["[0…99]", "[100…199]", "[200…249]"])
    #expect(big.children[2].children.count == 50 && big.children[2].children.first?.key == "200")
    #expect(big.children[2].children.last?.summary == "249")

    // A million items: 100 groups of 10,000, each 100 groups of 100: nothing more is built.
    let huge = JSONNode(value: .array(Array(repeating: .null, count: 1_000_000)))
    #expect(huge.children.count == 100 && huge.children[0].key == "[0…9999]")
    #expect(huge.children[0].children.count == 100 && huge.children[0].children[1].key == "[100…199]")
    #expect(huge.children[0].children[1].children.first?.key == "100")
    #expect(JSONNode(value: .number("3")).isExpandable == false)
}

@Test func traceParsesArraysObjectsAndJSONL() {
    #expect(parseTrace(#"[{"label":"a","kind":"tool","ts":"t"},"b",{"nolabel":1}]"#) == [TraceStep(label: "a", kind: "tool", ts: "t"), TraceStep(label: "b")])
    #expect(parseTrace(#"{"label":"one","detail":"d"}"#) == [TraceStep(label: "one", detail: "d")])
    #expect(parseTrace("{\"label\":\"x\"}\nnot json\n\n\"s\"\n{\"kind\":\"k\"}") == [TraceStep(label: "x"), TraceStep(label: "not json"), TraceStep(label: "s")])
    #expect(parseTrace("  ").isEmpty)
    #expect(traceTime("2026-01-01T00:00:10Z") == 1_767_225_610)
    #expect(traceTime("2026-01-01T00:00:10.500Z") == 1_767_225_610.5)
}

@Test func sessionTraceFoldsBetweenPosts() {
    let t0 = 1_767_225_600.0
    func ts(_ s: Double) -> String { Date(timeIntervalSince1970: t0 + s).ISO8601Format() }
    let steps = [
        TraceStep(label: "do it", kind: "prompt", ts: ts(0)), TraceStep(label: "on it", kind: "say", ts: ts(1)),
        TraceStep(label: "ls", kind: "bash", ts: ts(2)), TraceStep(label: "thinking", kind: "say", ts: ts(3)),
        TraceStep(label: "cat", kind: "bash", ts: ts(4)), TraceStep(label: "done", kind: "say", ts: ts(5)),
        TraceStep(label: "after", kind: "bash", ts: ts(100)), TraceStep(label: "no time"),
    ]
    let closed = timelineGaps(steps: steps, posts: [t0 + 10], open: [])
    #expect(closed.count == 2)
    #expect(closed[0] == [.step(index: 0, style: .prompt), .step(index: 1, style: .response), .fold(first: 2, count: 3, open: false), .step(index: 5, style: .response)])
    #expect(closed[1] == [.fold(first: 6, count: 2, open: false)])
    let open = timelineGaps(steps: steps, posts: [t0 + 10], open: [2])
    #expect(open[0] == [.step(index: 0, style: .prompt), .step(index: 1, style: .response), .fold(first: 2, count: 3, open: true),
                        .step(index: 2, style: .command), .step(index: 3, style: .response), .step(index: 4, style: .command), .step(index: 5, style: .response)])
}

@Test func peleRendersDiagramsWithThemeColors() throws {
    let theme = Theme.pele(dark: true)
    let sources = [
        "flowchart LR\n  A[Agent] -->|post-new| B(notesd)\n  B --> C{App}",
        "sequenceDiagram\n  Agent->>notesd: post-new\n  notesd-->>App: post-created",
        "erDiagram\n  CUSTOMER ||--o{ ORDER : places\n  ORDER ||--|{ LINE-ITEM : contains",
    ]
    let clock = ContinuousClock()
    var first: Duration?
    for (i, source) in sources.enumerated() {
        let started = clock.now
        let d = try Pele.shared.render(source)
        if i == 0 { first = clock.now - started }
        #expect(d.type == ["flowchart", "sequence", "er"][i])
        #expect(d.width > 0 && d.height > 0 && d.svg.hasPrefix("<svg"))
        #expect(d.svg.contains("var(--"))
        let svg = resolvePeleSVG(d.svg, theme: theme)
        #expect(!svg.contains("var("), "unresolved CSS variable in \(d.type)")
        #expect(!svg.contains("color-mix("))
        #expect(svg.contains(theme["--pele-fg"]!) && svg.contains(theme["--pele-surface"]!))
        let image = try #require(NSImage(data: Data(svg.utf8)), "NSImage can't read the resolved \(d.type) svg")
        #expect(abs(image.size.width - d.width) < 1)
        #expect(MermaidRaster.rasterize(svg, size: CGSize(width: d.width, height: d.height), scale: 2) != nil)
    }
    // Warm renders reuse the one context: well under a frame.
    let started = clock.now
    for _ in 0..<20 { _ = try Pele.shared.render(sources[0]) }
    let warm = (clock.now - started) / 20
    print("pele first render \(first!), warm \(warm)")
    #expect(warm < .milliseconds(4))
    // A narrow pane gets a vertical layout of the wide flowchart.
    let wide = try Pele.shared.render("flowchart LR\n A[one] --> B[two] --> C[three] --> D[four] --> E[five] --> F[six]")
    let narrow = try Pele.shared.render("flowchart LR\n A[one] --> B[two] --> C[three] --> D[four] --> E[five] --> F[six]", maxWidth: 240)
    #expect(narrow.width < wide.width)
    #expect(throws: (any Error).self) { try Pele.shared.render("flowchart LR\n A -->") }
}

@Test func resolverHandlesFallbacksMixAndRx() {
    let svg = ##"<svg style="--_s:var(--pele-surface,#f3f3f3);--_bg:var(--pele-bg,#fff);color:var(--_fg,#111)"><rect style="rx:var(--pele-radius,4px)" fill="color-mix(in srgb,var(--_s) 50%,var(--_bg))"/><g fill="var(--pele-series-2,#f58518)"/></svg>"##
    let out = resolvePeleSVG(svg, theme: ["--pele-surface": "#000000", "--pele-bg": "#ffffff"])
    #expect(out == ##"<svg style="color:#111"><rect rx="4" fill="#808080"/><g fill="#f58518"/></svg>"##)
}

@Test func codeSpansMapToAttributedLines() async throws {
    let render = PartRender.code(language: "rust", lineStart: 41, lines: [[Span("fn", "keyword"), Span(" main() {")], [Span("\t\"hi\"", "string")], [Span("}")]])
    let model = try #require(CodeModel.build(part: .code(code: "", title: "main.rs"), render: render))
    let pane = model.panes[0]
    #expect(model.title == "main.rs" && model.lineCount == 3)
    #expect(pane.text.string == "fn main() {\n    \"hi\"\n}")
    #expect(pane.gutter == ["41", "42", "43"])
    #expect(pane.columns == 11)
    #expect(pane.source.map { $0?.line } == [41, 42, 43])
    let kw = pane.text.attribute(.foregroundColor, at: 0, effectiveRange: nil) as? NSColor
    #expect(kw === Theme.tokens["keyword"])
    #expect((pane.text.attribute(.foregroundColor, at: 13, effectiveRange: nil) as? NSColor) === Theme.tokens["string"])
    #expect((pane.text.attribute(.font, at: 0, effectiveRange: nil) as? NSFont) == CodeText.font)
    let p = try #require(pane.text.attribute(.paragraphStyle, at: 0, effectiveRange: nil) as? NSParagraphStyle)
    #expect(p.minimumLineHeight == CodeText.lineHeight && p.maximumLineHeight == CodeText.lineHeight)

    // TextKit 2 lays every line on the gutter's fixed grid (a baselineOffset breaks this).
    let fragments = await MainActor.run { () -> [CGRect] in
        let view = NSTextView(usingTextLayoutManager: true)
        view.textStorage?.setAttributedString(pane.text)
        let lm = view.textLayoutManager!
        var frames: [CGRect] = []
        lm.enumerateTextLayoutFragments(from: lm.documentRange.location, options: [.ensuresLayout]) { frames.append($0.layoutFragmentFrame); return true }
        return frames
    }
    #expect(fragments.map(\.minY) == [0, 1, 2].map { CGFloat($0) * CodeText.lineHeight })
    #expect(fragments.allSatisfy { $0.height == CodeText.lineHeight })

    // No render (or a native one): the source, unhighlighted, numbered from line_start.
    let plain = try #require(CodeModel.build(part: .code(code: "a\nb\n", lineStart: 9), render: nil))
    #expect(plain.panes[0].text.string == "a\nb" && plain.panes[0].gutter == [" 9", "10"])
}

@Test func diffSpansMapToUnifiedAndSplit() throws {
    let file = DiffFileRender(filename: "src/lib.rs", added: 2, removed: 1, hunks: [DiffHunk(header: "@@ -1,3 +1,4 @@", lines: [
        DiffLine(kind: .context, oldLine: 1, newLine: 1, spans: [Span("use x;")]),
        DiffLine(kind: .removed, oldLine: 2, spans: [Span("old")]),
        DiffLine(kind: .added, newLine: 2, spans: [Span("new", "string")]),
        DiffLine(kind: .added, newLine: 3, spans: [Span("more")]),
        DiffLine(kind: .context, oldLine: 3, newLine: 4, spans: [Span("end")]),
    ])])
    let unified = try #require(CodeModel.build(part: .diff(patch: "p"), render: .diff(layout: .unified, files: [file])))
    let u = unified.panes[0]
    #expect(unified.panes.count == 1)
    #expect(u.kinds == [.file, .hunk, .plain, .removed, .added, .added, .plain])
    #expect(u.text.string == "src/lib.rs  +2 −1\n@@ -1,3 +1,4 @@\nuse x;\nold\nnew\nmore\nend")
    #expect(u.gutter[2] == "1 1  " && u.gutter[3] == "2   −" && u.gutter[4] == "  2 +")
    #expect(u.source.map { $0?.line } == [nil, nil, 1, 2, 2, 3, 4])
    #expect(u.source[3]?.file == "src/lib.rs")

    let split = try #require(CodeModel.build(part: .diff(layout: .split), render: .diff(layout: .split, files: [file])))
    let (l, r) = (split.panes[0], split.panes[1])
    #expect(l.lineCount == r.lineCount)
    #expect(l.kinds == [.file, .hunk, .plain, .removed, .filler, .plain])
    #expect(r.kinds == [.file, .hunk, .plain, .added, .added, .plain])
    #expect(l.text.string == "src/lib.rs\n@@ -1,3 +1,4 @@\nuse x;\nold\n\nend")
    #expect(r.text.string == "+2 −1\n@@ -1,3 +1,4 @@\nuse x;\nnew\nmore\nend")
    #expect(l.source.map { $0?.line } == [nil, nil, 1, 2, nil, 3])
    #expect(r.source.map { $0?.line } == [nil, nil, 1, 2, 3, 4])
}

@Test func terminalClassesMapToSGRColors() throws {
    let model = try #require(CodeModel.build(part: .terminal(text: ""), render: .terminal(lines: [[Span("ok", "fg-green bold"), Span(" plain")], [Span("x", "fg-bright-red underline")]])))
    let t = model.panes[0].text
    #expect(model.terminal && model.title == "Terminal")
    #expect((t.attribute(.foregroundColor, at: 0, effectiveRange: nil) as? NSColor) == Theme.sgr["green"])
    #expect((t.attribute(.font, at: 0, effectiveRange: nil) as? NSFont) == CodeText.bold)
    #expect((t.attribute(.foregroundColor, at: 3, effectiveRange: nil) as? NSColor) == Theme.terminalText)
    #expect((t.attribute(.foregroundColor, at: 9, effectiveRange: nil) as? NSColor) == Theme.sgr["bright-red"])
    #expect(t.attribute(.underlineStyle, at: 9, effectiveRange: nil) as? Int == NSUnderlineStyle.single.rawValue)
}
