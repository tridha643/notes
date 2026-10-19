// DaemonClient against a fake notesd on a real unix socket.

import Foundation
import Testing
@testable import NotesCore

/// Answers list/read/write/new/delete/watch from an in-memory vault, one thread per connection.
final class FakeDaemon: @unchecked Sendable {
    let path: String
    private let listener: Int32
    private let lock = NSLock()
    private var notes: [String: (body: String, version: Int)]
    private(set) var requestLines: [String] = []

    init(notes: [String: String]) throws {
        path = "/tmp/notes-app-test-\(getpid())-\(UUID().uuidString.prefix(8)).sock"
        self.notes = notes.mapValues { ($0, 1) }
        listener = socket(AF_UNIX, SOCK_STREAM, 0)
        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        withUnsafeMutableBytes(of: &addr.sun_path) { $0.copyBytes(from: Array(path.utf8)) }
        let rc = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(listener, $0, socklen_t(MemoryLayout<sockaddr_un>.size)) }
        }
        guard rc == 0, listen(listener, 8) == 0 else { throw DaemonError.unreachable("bind \(path)") }
        Thread.detachNewThread { [self] in
            while case let fd = accept(listener, nil, nil), fd >= 0 {
                Thread.detachNewThread { self.serve(Socket(fd: fd)) }
            }
        }
    }

    /// Writes as another client would, bumping the version.
    func externalWrite(_ path: String, _ body: String) {
        lock.withLock { notes[path] = (body, notes[path]!.version + 1) }
    }

    func requests(_ verb: String) -> [String] {
        lock.withLock { requestLines.filter { $0.contains(#""verb":"\#(verb)""#) } }
    }

    /// Unlinks the socket. The listener stays open: closing it under the blocked
    /// accept() would let another test's socket reuse the fd number.
    func stop() { unlink(path) }

    private func serve(_ socket: Socket) {
        while let line = try? socket.readLine() {
            let request = try! JSONSerialization.jsonObject(with: line) as! [String: Any]
            lock.withLock { requestLines.append(String(decoding: line, as: UTF8.self)) }
            let id = request["id"] as! Int
            let args = request["args"] as? [String: Any] ?? [:]
            for reply in lock.withLock({ answer(request["verb"] as! String, args) }) {
                var body = reply
                body["id"] = id
                try? socket.write(JSONSerialization.data(withJSONObject: body) + Data([0x0A]))
            }
            if request["verb"] as? String == "watch" { return } // closes after one event
        }
    }

    private func resolve(_ ref: String) -> String? {
        if ref == "@last" { return notes.keys.sorted().last }
        return [ref, ref + ".md"].first { notes[$0] != nil }
    }

    private func answer(_ verb: String, _ args: [String: Any]) -> [[String: Any]] {
        func ok(_ result: Any) -> [[String: Any]] { [["ok": true, "result": result]] }
        func err(_ code: String, _ message: String, _ details: [String: Any]? = nil) -> [[String: Any]] {
            var error: [String: Any] = ["code": code, "message": message]
            error["details"] = details
            return [["ok": false, "error": error]]
        }
        switch verb {
        case "list":
            let folders = Set(notes.keys.compactMap { $0.contains("/") ? String($0.split(separator: "/")[0]) : nil })
            return ok(folders.sorted().map { ["folder": $0] } + notes.keys.sorted().map { path in
                ["path": path, "title": String(path.split(separator: "/").last!.dropLast(3)), "version": notes[path]!.version, "updated_at": 0]
            })
        case "read":
            guard let path = resolve(args["note"] as! String), let note = notes[path] else { return err("not_found", "no such note") }
            return ok(["path": path, "title": "t", "version": note.version, "body": note.body,
                       "line_count": note.body.split(separator: "\n").count, "created_at": 0, "updated_at": 0])
        case "write":
            guard let path = resolve(args["note"] as! String), let note = notes[path] else { return err("not_found", "no such note") }
            if let expected = args["if_version"] as? Int, expected != note.version {
                return err("version_conflict", "\(path) is at version \(note.version), not \(expected)", ["current_version": note.version])
            }
            notes[path] = (args["body"] as! String, note.version + 1)
            return ok(["path": path, "version": note.version + 1])
        case "new":
            let path = (args["path"] as! String) + ".md"
            notes[path] = (args["body"] as? String ?? "", 1)
            return ok(["path": path, "version": 1])
        case "watch":
            return [["ok": true, "result": NSNull()], ["event": ["seq": 5, "kind": "note-updated", "path": "a.md", "version": 2]]]
        default:
            return err("usage", "unknown verb \(verb)")
        }
    }
}

@Suite struct DaemonClientTests {
    let fake: FakeDaemon
    let client: DaemonClient

    init() throws {
        fake = try FakeDaemon(notes: ["a.md": "# A\n", "work/plan.md": "plan"])
        client = DaemonClient(socketPath: fake.path)
    }

    @Test func listsFoldersAndNotes() throws {
        defer { fake.stop() }
        let entries = try client.list()
        #expect(entries.first == .folder("work"))
        #expect(entries.contains(.note(NoteSummary(path: "work/plan.md", title: "plan", version: 1, updatedAt: 0))))
        #expect(try object(Data(fake.requestLines[0].utf8)) == object(#"{"v":1,"id":1,"verb":"list","args":{"recursive":true,"sort":"title"}}"#))
    }

    @Test func readsAndReportsNotFound() throws {
        defer { fake.stop() }
        let note = try client.read("@last")
        #expect(note.path == "work/plan.md" && note.body == "plan" && note.version == 1)
        #expect(throws: DaemonError.daemon(ErrorBody(code: .notFound, message: "no such note"))) { try client.read("nope") }
    }

    @Test func writeChecksVersionAndConflicts() throws {
        defer { fake.stop() }
        let written = try client.write("a", body: "# A\nmore", ifVersion: 1)
        #expect(written == WriteResult(path: "a.md", version: 2))
        #expect(try client.read("a").body == "# A\nmore")
        do {
            _ = try client.write("a", body: "stale", ifVersion: 1)
            Issue.record("expected a version conflict")
        } catch let error as DaemonError {
            #expect(error.errorBody?.code == .versionConflict)
            #expect(error.errorBody?.currentVersion == 2)
        }
        #expect(try client.read("a").body == "# A\nmore")
        #expect(fake.requestLines.contains { $0.contains(#""if_version":1"#) && $0.contains(#""verb":"write""#) })
    }

    @Test func newNote() throws {
        defer { fake.stop() }
        #expect(try client.new("work/idea").path == "work/idea.md")
        #expect(try client.read("work/idea").version == 1)
    }

    @Test func watchStreamsEventsThenEnds() throws {
        defer { fake.stop() }
        let watch = try client.watch(since: 4)
        #expect(try watch.next() == Event(seq: 5, kind: "note-updated", path: "a.md", version: 2))
        #expect(try watch.next() == nil)
        #expect(fake.requestLines.last?.contains(#""since":4"#) == true)
    }

    @Test func cancelUnblocksAWatch() throws {
        defer { fake.stop() }
        // A watch on a socket that never answers: cancel must wake the blocked read.
        let silent = "/tmp/notes-app-silent-\(getpid()).sock"
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        defer { close(fd); unlink(silent) }
        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        withUnsafeMutableBytes(of: &addr.sun_path) { $0.copyBytes(from: Array(silent.utf8)) }
        unlink(silent)
        _ = withUnsafePointer(to: &addr) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size)) } }
        listen(fd, 1)
        let watch = try DaemonClient(socketPath: silent).watch()
        DispatchQueue.global().asyncAfter(deadline: .now() + 0.1) { watch.cancel() }
        #expect(try watch.next() == nil)
    }

    @Test func unreachableSocket() {
        defer { fake.stop() }
        let missing = DaemonClient(socketPath: "/tmp/notes-app-missing-\(getpid()).sock")
        #expect {
            try missing.list()
        } throws: { error in
            if case .unreachable = error as? DaemonError { true } else { false }
        }
    }
}
