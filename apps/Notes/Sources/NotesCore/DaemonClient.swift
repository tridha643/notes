// Blocking unix-socket client for notesd. Call it off the main thread; each request
// opens its own short connection (a local round trip is microseconds), and `Watch`
// holds one long-lived connection for the event stream.

import Foundation
import Synchronization

public enum DaemonError: Error, Sendable, Equatable {
    /// The socket couldn't be reached: notesd isn't running.
    case unreachable(String)
    /// notesd answered `ok:false`.
    case daemon(ErrorBody)
    /// The connection broke or a line didn't parse.
    case protocolError(String)

    public var errorBody: ErrorBody? {
        if case .daemon(let body) = self { body } else { nil }
    }
}

/// `$NOTES_SOCK`, else `~/Library/Application Support/Notes/notesd.sock`.
public func defaultSocketPath(environment: [String: String] = ProcessInfo.processInfo.environment) -> String {
    if let sock = environment["NOTES_SOCK"], !sock.isEmpty { return sock }
    return NSHomeDirectory() + "/Library/Application Support/Notes/notesd.sock"
}

public final class DaemonClient: Sendable {
    public let socketPath: String
    private let nextID = Atomic<UInt64>(1)

    public init(socketPath: String = defaultSocketPath()) {
        self.socketPath = socketPath
    }

    public func list(_ args: ListArgs = ListArgs(recursive: true, sort: "title")) throws -> [ListEntry] {
        try call("list", args)
    }

    /// `note` is a ref: a path with or without `.md`, a unique bare title, or `@last`.
    public func read(_ note: String) throws -> Note {
        try call("read", NoteArgs(note: note))
    }

    public func write(_ note: String, body: String, ifVersion: UInt64?) throws -> WriteResult {
        try call("write", WriteArgs(note: note, body: body, ifVersion: ifVersion))
    }

    public func new(_ path: String, body: String = "") throws -> WriteResult {
        try call("new", NewArgs(path: path, body: body))
    }

    public func delete(_ note: String) throws -> WriteResult {
        try call("delete", DeleteArgs(note: note))
    }

    /// One request, one response. Event lines for other ids are skipped.
    public func call<Args: Codable & Sendable, Result: Codable & Sendable>(_ verb: String, _ args: Args) throws -> Result {
        try callLine(verb, args).0
    }

    /// `call`, also returning the raw response line.
    func callLine<Args: Codable & Sendable, Result: Codable & Sendable>(_ verb: String, _ args: Args) throws -> (Result, Data) {
        let id = nextID.wrappingAdd(1, ordering: .relaxed).oldValue
        let socket = try Socket(path: socketPath)
        try socket.send(Request(id: id, verb: verb, args: args))
        while let line = try socket.readLine() {
            let response: Response<Result>
            do {
                response = try Wire.decoder.decode(Response<Result>.self, from: line)
            } catch {
                throw DaemonError.protocolError("bad response to \(verb): \(error)")
            }
            switch response {
            case .ok(id, let result): return (result, line)
            case .err(id, let error): throw DaemonError.daemon(error)
            default: continue
            }
        }
        throw DaemonError.protocolError("notesd closed the connection before answering \(verb)")
    }

    // MARK: Sideshow

    public func sessionList() throws -> [Session] { try call("session-list", NoArgs()) }

    public func sessionRename(_ id: String, title: String) throws {
        let _: Ignored = try call("session-rename", SessionRenameArgs(id: id, title: title))
    }

    public func sessionDelete(_ id: String) throws {
        let _: Ignored = try call("session-delete", IdArgs(id: id))
    }

    public func postList(session: String?, limit: UInt32? = nil) throws -> [PostSummary] {
        try call("post-list", PostListArgs(session: session, limit: limit))
    }

    /// A post (optionally an older version), with `json` parts in the agent's key order.
    public func postShow(_ id: String, version: UInt64? = nil) throws -> Post {
        var (post, line): (Post, Data) = try callLine("post-show", PostShowArgs(id: id, version: version))
        if post.parts.contains(where: { $0.kind == "json" }), let raw = try? JSONValue.parse(line) {
            for (i, part) in post.parts.enumerated() {
                if case .json(_, let pid) = part, let data = raw["result"]?["parts"]?[i]?["data"] { post.parts[i] = .json(data: data, id: pid) }
            }
        }
        return post
    }

    public func postRender(_ id: String, version: UInt64? = nil) throws -> PostRender {
        try call("post-render", PostShowArgs(id: id, version: version))
    }

    public func postDelete(_ id: String) throws {
        let _: Ignored = try call("post-delete", IdArgs(id: id))
    }

    public func commentNew(_ args: CommentNewArgs) throws -> Comment { try call("comment-new", args) }

    public func commentList(_ args: CommentListArgs) throws -> [Comment] { try call("comment-list", args) }

    public func commentDelete(_ id: String) throws {
        let _: Ignored = try call("comment-delete", IdArgs(id: id))
    }

    public func assetGet(_ id: String) throws -> AssetData { try call("asset-get", IdArgs(id: id)) }

    /// A session's trace steps.
    public func traceList(session: String) throws -> [TraceStep] { try call("trace-list", IdArgs(id: session)) }

    public func kits() throws -> [Kit] { try call("kits", NoArgs()) }

    public func watch(since: UInt64? = nil) throws -> Watch {
        let socket = try Socket(path: socketPath)
        try socket.send(Request(id: nextID.wrappingAdd(1, ordering: .relaxed).oldValue, verb: "watch", args: WatchArgs(since: since)))
        return Watch(socket: socket)
    }
}

/// A `watch` stream. Call `next()` from one thread; `cancel()` from any thread unblocks it.
public final class Watch: @unchecked Sendable {
    private let socket: Socket

    init(socket: Socket) { self.socket = socket }

    /// Blocks until the next event; nil when the stream ends (daemon gone or cancelled).
    public func next() throws -> Event? {
        while let line = try socket.readLine() {
            switch try Wire.decoder.decode(Response<Ignored>.self, from: line) {
            case .event(_, let event): return event
            case .err(_, let error): throw DaemonError.daemon(error)
            case .ok: continue
            }
        }
        return nil
    }

    public func cancel() { socket.shutdown() }
}

/// A connected unix stream socket with a line reader. Closes on deinit.
final class Socket {
    let fd: Int32
    private var buffer = [UInt8]()
    private var start = 0
    private var chunk = [UInt8](repeating: 0, count: 64 * 1024)

    init(path: String) throws {
        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let bytes = Array(path.utf8)
        guard bytes.count < MemoryLayout.size(ofValue: addr.sun_path) else {
            throw DaemonError.unreachable("socket path too long: \(path)")
        }
        withUnsafeMutableBytes(of: &addr.sun_path) { $0.copyBytes(from: bytes) }
        fd = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { throw DaemonError.unreachable(String(cString: strerror(errno))) }
        var one: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &one, socklen_t(MemoryLayout<Int32>.size))
        let rc = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard rc == 0 else {
            // No close here: every stored property is set, so deinit runs and closes fd.
            throw DaemonError.unreachable("\(path): \(String(cString: strerror(errno)))")
        }
    }

    /// Wraps an accepted fd (used by the test server).
    init(fd: Int32) { self.fd = fd }

    deinit { Darwin.close(fd) }

    func send(_ message: some Encodable) throws {
        try write(Wire.line(message))
    }

    func write(_ data: Data) throws {
        try data.withUnsafeBytes { raw in
            var offset = 0
            while offset < raw.count {
                let n = Darwin.write(fd, raw.baseAddress! + offset, raw.count - offset)
                if n < 0 && errno == EINTR { continue }
                guard n > 0 else { throw DaemonError.protocolError("write: \(String(cString: strerror(errno)))") }
                offset += n
            }
        }
    }

    /// The next line without its `\n`, or nil at EOF.
    func readLine() throws -> Data? {
        var scanFrom = start
        while true {
            if let nl = buffer[scanFrom...].firstIndex(of: 0x0A) {
                let line = Data(buffer[start..<nl])
                start = nl + 1
                if start == buffer.count { buffer.removeAll(keepingCapacity: true); start = 0 }
                return line
            }
            if start > 0 { buffer.removeFirst(start); start = 0 }
            scanFrom = buffer.count
            let n = chunk.withUnsafeMutableBytes { Darwin.read(fd, $0.baseAddress!, $0.count) }
            if n < 0 && errno == EINTR { continue }
            if n < 0 { throw DaemonError.protocolError("read: \(String(cString: strerror(errno)))") }
            if n == 0 { return nil }
            buffer.append(contentsOf: chunk[..<n])
        }
    }

    func shutdown() { Darwin.shutdown(fd, SHUT_RDWR) }
}
