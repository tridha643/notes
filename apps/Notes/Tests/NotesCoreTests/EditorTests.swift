// Drives the real Editor (TextKit 2 text view, idle save, conflict banner, incremental
// restyle) against the fake daemon. No window: edits go through insertText like typing.

import AppKit
import Foundation
import Testing
@testable import NotesCore
@testable import NotesUI

@MainActor
@Suite(.serialized) struct EditorTests {
    /// `NotesUI.client` reads NOTES_SOCK once, so every editor test shares one fake.
    static let fake: FakeDaemon = {
        let fake = try! FakeDaemon(notes: ["a.md": "# A\n", "b.md": "intro\n"])
        setenv("NOTES_SOCK", fake.path, 1)
        return fake
    }()

    init() { _ = Self.fake }

    func settle(_ seconds: Double = 0.6) async throws {
        try await Task.sleep(for: .seconds(seconds))
    }

    func type(_ editor: Editor, _ text: String, at location: Int? = nil) {
        let at = location ?? (editor.textView.string as NSString).length
        editor.textView.setSelectedRange(NSRange(location: at, length: 0))
        editor.textView.insertText(text, replacementRange: NSRange(location: at, length: 0))
    }

    @Test func savesAfterIdleThenResolvesConflict() async throws {
        let fake = Self.fake
        #expect(NotesUI.client.socketPath == fake.path)
        let editor = Editor()
        editor.load(try NotesUI.client.read("a"))
        #expect(editor.textView.textLayoutManager != nil) // TextKit 2
        #expect(editor.version == 1 && !editor.isDirty)

        type(editor, "more")
        #expect(editor.isDirty)
        #expect(fake.requests("write").isEmpty) // waits for the idle pause
        try await settle()
        #expect(fake.requests("write").count == 1)
        #expect(fake.requests("write")[0].contains(#""if_version":1"#))
        #expect(editor.version == 2 && !editor.isDirty)

        // Someone else writes v3; our next save (if_version 2) conflicts.
        fake.externalWrite("a.md", "theirs\n")
        type(editor, " mine")
        try await settle()
        #expect(editor.banner.isHidden == false)
        #expect(editor.bannerLabel.stringValue.contains("v3"))
        #expect(editor.textView.string == "# A\nmore mine") // user's text kept on screen
        #expect(editor.isDirty)

        // Typing during the conflict doesn't autosave.
        let writes = fake.requests("write").count
        type(editor, "!")
        try await settle()
        #expect(fake.requests("write").count == writes)

        // Keep Mine writes over v3.
        editor.bannerPrimaryClicked()
        try await settle(0.2)
        #expect(fake.requests("write").last?.contains(#""if_version":3"#) == true)
        #expect(try NotesUI.client.read("a").body == "# A\nmore mine!")
        #expect(editor.banner.isHidden && !editor.isDirty && editor.version == 4)
    }

    @Test func refreshKeepsCleanNotesCurrent() throws {
        let editor = Editor()
        editor.load(try NotesUI.client.read("b"))
        Self.fake.externalWrite("b.md", "intro\nnew line\n")
        editor.refresh(try NotesUI.client.read("b"))
        #expect(editor.textView.string == "intro\nnew line\n")
    }

    @Test func sidebarTreeNestsByPath() {
        func note(_ path: String) -> ListEntry { .note(NoteSummary(path: path, title: path, version: 1, updatedAt: 0)) }
        // Folders first, as `list` sends them; "work/deep" was never listed itself.
        let roots = Node.tree([.folder("work"), note("top.md"), note("work/plan.md"), note("work/deep/x.md")])
        #expect(roots.map(\.path) == ["work", "top.md"])
        #expect(roots[0].children.map(\.path) == ["work/plan.md", "work/deep"])
        #expect(roots[0].children[1].title == "deep" && roots[0].children[1].children.map(\.path) == ["work/deep/x.md"])
    }

    @Test func restyleFollowsFenceState() throws {
        let editor = Editor()
        editor.load(try NotesUI.client.read("b"))
        editor.textView.string = "# Title\nplain **bold**\nlast"
        let storage = editor.textView.textStorage!
        func font(at i: Int) -> NSFont { storage.attribute(.font, at: i, effectiveRange: nil) as! NSFont }
        #expect(font(at: 2).pointSize == 26)
        #expect(font(at: 16).fontDescriptor.symbolicTraits.contains(.bold))
        #expect(!font(at: 25).isFixedPitch)

        // Opening a fence on line 1 turns every later line into code; closing it restores them.
        type(editor, "```\n", at: 0)
        #expect(font(at: 4 + 2).isFixedPitch && font(at: 4 + 25).isFixedPitch)
        editor.textView.setSelectedRange(NSRange(location: 0, length: 4))
        editor.textView.insertText("", replacementRange: NSRange(location: 0, length: 4))
        #expect(font(at: 2).pointSize == 26 && !font(at: 25).isFixedPitch)
    }
}
