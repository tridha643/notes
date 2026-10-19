// DaemonClient's sideshow verbs against scripts/fake_notesd.py, the same stand-in
// daemon the app is run against by hand.

import Foundation
import Testing
@testable import NotesCore

/// Runs fake_notesd.py on a fresh socket for the life of the value.
final class PythonFake {
    let path = "/tmp/notes-app-py-\(getpid())-\(UUID().uuidString.prefix(8)).sock"
    let process = Process()
    let log = Pipe()

    init() throws {
        let script = URL(fileURLWithPath: #filePath).deletingLastPathComponent().appendingPathComponent("../../scripts/fake_notesd.py").standardized
        process.executableURL = URL(fileURLWithPath: "/usr/bin/python3")
        process.arguments = [script.path, path]
        process.standardOutput = log
        try process.run()
        for _ in 0..<200 where !FileManager.default.fileExists(atPath: path) { usleep(20_000) }
    }

    deinit {
        process.terminate()
        process.waitUntilExit()
        unlink(path)
    }
}

@Suite(.serialized) struct SideshowClientTests {
    let fake: PythonFake
    let client: DaemonClient

    init() throws {
        fake = try PythonFake()
        client = DaemonClient(socketPath: fake.path)
    }

    @Test func sessionsPostsAndRender() throws {
        let sessions = try client.sessionList()
        #expect(sessions.map(\.id) == ["s1", "s2"])
        #expect(sessions[0].agent == "claude" && sessions[0].postCount == 9)

        let posts = try client.postList(session: "s1")
        #expect(posts.count == 9)
        #expect(Set(posts.flatMap(\.kinds)) == ["markdown", "code", "diff", "terminal", "mermaid", "image", "json", "trace", "html"])

        let diff = try client.postShow("p3")
        #expect(diff.parts.map(\.kind) == ["diff", "diff"])
        let render = try client.postRender("p3", version: 1)
        #expect(render.version == 1 && render.parts.count == 2)
        guard case .diff(.unified, let files) = render.parts[0], case .diff(.split, let split) = render.parts[1] else { Issue.record("expected diffs"); return }
        #expect(files[0].filename == "src/lib.rs" && files[0].added == 2 && files[0].removed == 1)
        #expect(files[0].hunks[0].lines.map(\.kind) == [.context, .removed, .added, .added, .context, .context])
        #expect(split[0].filename == "notes.txt")

        guard case .code(_, let start, let lines) = try client.postRender("p2").parts[0] else { Issue.record("expected code"); return }
        #expect(start == 120 && lines[0].first == Span("pub", "keyword"))
        guard case .terminal(let term) = try client.postRender("p4").parts[0] else { Issue.record("expected terminal"); return }
        #expect(term[0][0] == Span("   Compiling", "fg-green"))
        #expect(try client.postRender("p1").parts == [.native])
    }

    @Test func jsonPartsKeepTheirKeyOrder() throws {
        let post = try client.postShow("p7")
        guard case .json(.object(let members), _) = post.parts[0] else { Issue.record("expected json object"); return }
        #expect(members.map(\.key) == ["zeta", "alpha", "nested"])
        #expect(members[1].value == .array([.number("1"), .number("2.5"), .null, .bool(true), .string("x")]))
        #expect(members[2].value["user_id"] == .number("7"))
        if case .array(let big)? = members[2].value["big"] { #expect(big.count == 250) } else { Issue.record("expected big array") }
    }

    @Test func commentsAssetsKitsAndTrace() throws {
        let posted = try client.commentNew(CommentNewArgs(post: "p2", text: "ship it", anchor: .lineRange(part: 0, postVersion: 1, startLine: 120, endLine: 121), author: "user"))
        #expect(posted.author == "user" && posted.session == "s1" && posted.post == "p2")
        let all = try client.commentList(CommentListArgs(session: "s1"))
        #expect(all.last == posted)
        #expect(try client.commentList(CommentListArgs(session: "s1", afterSeq: posted.seq)).isEmpty)
        try client.commentDelete(posted.id)
        #expect(try client.commentList(CommentListArgs(session: "s1")).contains(posted) == false)

        guard case .image(let assetId, _, let caption, _) = try client.postShow("p6").parts[0] else { Issue.record("expected image"); return }
        let asset = try client.assetGet(assetId)
        #expect(caption == "Figure 1: a gradient")
        #expect(asset.info.contentType == "image/png" && asset.data?.prefix(4) == Data([0x89, 0x50, 0x4E, 0x47]))
        #expect(asset.info.byteLength == UInt64(asset.data?.count ?? 0))
        #expect(throws: DaemonError.self) { try client.assetGet("missing") }

        let kits = try client.kits()
        #expect(kits.map(\.id) == ["issues", "slides"] && kits[1].js != nil)
        #expect(try client.traceList(session: "s1").first == TraceStep(label: "Build the posts view", kind: "prompt", ts: try client.traceList(session: "s1")[0].ts))

        try client.sessionRename("s2", title: "Renamed")
        #expect(try client.sessionList().first { $0.id == "s2" }?.title == "Renamed")
        try client.postDelete("p10")
        try client.sessionDelete("s2")
        #expect(try client.sessionList().map(\.id) == ["s1"])
    }

    @Test func watchDeliversSideshowEvents() throws {
        let watch = try client.watch()
        _ = try client.commentNew(CommentNewArgs(post: "p1", text: "hello", author: "user"))
        var kinds: [String] = []
        while kinds.count < 2, let e = try watch.next() { kinds.append(e.kind); #expect(e.session == "s1") }
        watch.cancel()
        #expect(kinds == ["comment-created", "session-updated"])
    }
}
