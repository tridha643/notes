// The post view's moving parts without a window: the loader against the Python fake,
// and the two ways the composer gets an anchor (text selection, option-click).

import AppKit
import Foundation
import Testing
@testable import NotesCore
@testable import NotesUI

@Suite(.serialized) struct PostsViewTests {
    @Test func loaderPreparesEveryPartKind() throws {
        let fake = try PythonFake()
        let client = DaemonClient(socketPath: fake.path)
        let snapshot = try PostLoader.session(client, "s1")
        #expect(snapshot.posts.map(\.id) == (1...9).map { "p\($0)" })
        #expect(snapshot.comments.count == 2 && snapshot.trace.count == 4)
        var seen = Set<String>()
        for loaded in snapshot.posts {
            for part in loaded.parts {
                switch part {
                case .code(let m):
                    seen.insert(m.terminal ? "terminal" : m.panes.count == 2 ? "split" : "code")
                    #expect(m.lineCount > 0)
                case .markdown(let s):
                    seen.insert("markdown")
                    #expect(s.string.hasPrefix("# Posts in the app"))
                case .mermaid(let m):
                    seen.insert("mermaid")
                    #expect(m.natural != nil && m.error == nil)
                case .html(let m):
                    seen.insert("html")
                    #expect(m.kits.map(\.id) == ["issues"] && m.key.count == 64)
                    let page = WebParts.page(m, dark: true)
                    #expect(page.contains("Content-Security-Policy") && page.contains(".badge.ok") && page.contains("window.sendPrompt"))
                    #expect(page.contains("--color-text-primary: \(Theme.dark.text)"))
                case .image(let m):
                    seen.insert("image")
                    #expect(m.image?.width == 320 && m.image?.height == 180 && m.caption == "Figure 1: a gradient")
                case .json(let node):
                    seen.insert("json")
                    #expect(node.children.map(\.key) == ["zeta", "alpha", "nested"])
                case .trace(let t):
                    seen.insert("trace")
                    #expect(t.steps.count == (t.title == "Inline steps" ? 2 : 3) && t.note == nil)
                }
            }
        }
        #expect(seen == ["markdown", "code", "split", "terminal", "mermaid", "html", "image", "json", "trace"])

        // History: an older version comes back with the newest version number alongside.
        let _: PostWriteResult = try client.call("post-update", PostUpdateArgs(id: "p2", parts: [.code(code: "fn v2() {}\n", language: "rust")]))
        let old = try PostLoader.load(client, "p2", version: 1)
        #expect(old.post.version == 1 && old.latest == 2)
        guard case .code(let m) = old.parts[0] else { Issue.record("expected code"); return }
        #expect(m.panes[0].gutter.first == "120")
        #expect(try PostLoader.load(client, "p2").parts.count == 1)
    }

    @MainActor @Test func selectingCodeLinesAnchorsTheComposer() throws {
        let file = DiffFileRender(filename: "src/lib.rs", added: 1, removed: 1, hunks: [DiffHunk(header: "@@ -4,2 +4,2 @@", lines: [
            DiffLine(kind: .context, oldLine: 4, newLine: 4, spans: [Span("a")]),
            DiffLine(kind: .removed, oldLine: 5, spans: [Span("b")]),
            DiffLine(kind: .added, newLine: 5, spans: [Span("c")]),
        ])])
        let model = try #require(CodeModel.build(part: .diff(patch: "p"), render: .diff(layout: .unified, files: [file])))
        let cell = CodeCell()
        cell.frame = NSRect(x: 0, y: 0, width: 600, height: 200)
        var picked: (Int, Int, Int)?
        cell.configure(model) { picked = ($0, $1, $2) }
        // Display lines: 0 file header, 1 hunk, 2 "a", 3 "b", 4 "c". Select from "a" into "b".
        cell.panes[0].text.setSelectedRange(NSRange(location: (model.panes[0].text.string as NSString).range(of: "a\n").location, length: 3))
        let (pane, first, last) = try #require(picked)
        #expect(pane == 0 && first == 2 && last == 3)
        #expect(model.lineAnchor(part: 1, version: 3, pane: pane, first: first, last: last)
            == .lineRange(part: 1, postVersion: 3, startLine: 4, endLine: 5, file: "src/lib.rs"))
        // Headers alone carry no source lines; out-of-range selections don't trap.
        #expect(model.lineAnchor(part: 0, version: 1, pane: 0, first: 0, last: 1) == nil)
        #expect(model.lineAnchor(part: 0, version: 1, pane: 0, first: 9, last: 12) == nil)
    }

    @MainActor @Test func optionClickPlacesAPointAnchor() throws {
        let posts = PostsView()
        let cell = MarkdownCell()
        cell.owner = posts
        cell.row = .part("p1", 2, 0)
        cell.frame = NSRect(x: 0, y: 0, width: 400, height: 110)
        let box = cell.box // content column, minus the bottom gap
        let window = NSWindow(contentRect: cell.frame, styleMask: [], backing: .buffered, defer: true)
        window.contentView = cell
        // A click at the middle of the box, in window coordinates (the cell is flipped).
        let mid = cell.convert(NSPoint(x: box.midX, y: box.midY), to: nil)
        let click = try #require(NSEvent.mouseEvent(with: .leftMouseDown, location: mid, modifierFlags: .option, timestamp: 0,
                                                    windowNumber: window.windowNumber, context: nil, eventNumber: 0, clickCount: 1, pressure: 1))
        cell.mouseDown(with: click)
        guard case .point(let part, let version, let x, let y)? = posts.composer.anchor else { Issue.record("no point anchor"); return }
        #expect(part == 0 && version == 2 && posts.composer.post == "p1")
        #expect(abs(x - 0.5) < 0.01 && abs(y - 0.5) < 0.01)
        #expect(posts.composer.context.stringValue.contains("part 1 · v2"))
        posts.composer.dropAnchor()
        #expect(posts.composer.anchor == nil)
    }
}
