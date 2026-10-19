// Wire types for notesd: one JSON object per line. Mirrors crates/proto/src/lib.rs,
// which is the source of truth for field names. Keys are snake_case on the wire;
// `Wire.encoder`/`Wire.decoder` convert, so Swift names stay camelCase.

import Foundation

public let protocolVersion: UInt32 = 1

public enum Wire {
    public static let encoder: JSONEncoder = {
        let e = JSONEncoder()
        e.keyEncodingStrategy = .convertToSnakeCase
        e.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        return e
    }()

    public static let decoder: JSONDecoder = {
        let d = JSONDecoder()
        d.keyDecodingStrategy = .convertFromSnakeCase
        return d
    }()

    /// One protocol line: JSON + `\n`.
    public static func line(_ value: some Encodable) throws -> Data {
        var data = try encoder.encode(value)
        data.append(0x0A)
        return data
    }
}

/// `{"v":1,"id":N,"verb":"…","args":{…}}`
public struct Request<Args: Codable & Sendable>: Codable, Sendable {
    public var v: UInt32 = protocolVersion
    public var id: UInt64
    public var verb: String
    public var args: Args

    public init(id: UInt64, verb: String, args: Args) {
        self.id = id
        self.verb = verb
        self.args = args
    }
}

// Args for the verbs the app uses. Optional fields are omitted when nil, like serde's
// `skip_serializing_if = "Option::is_none"`.

public struct ListArgs: Codable, Sendable, Equatable {
    public var folder: String?
    public var recursive = false
    /// `updated` (daemon default), `created` or `title`.
    public var sort: String?
    public init(folder: String? = nil, recursive: Bool = false, sort: String? = nil) {
        self.folder = folder
        self.recursive = recursive
        self.sort = sort
    }
}

public struct NoteArgs: Codable, Sendable, Equatable {
    public var note: String
}

public struct NewArgs: Codable, Sendable, Equatable {
    public var path: String
    public var body = ""
}

public struct WriteArgs: Codable, Sendable, Equatable {
    public var note: String
    public var body: String
    public var ifVersion: UInt64?
    public var force = false
}

public struct DeleteArgs: Codable, Sendable, Equatable {
    public var note: String
    public var purge = false
}

public struct WatchArgs: Codable, Sendable, Equatable {
    public var since: UInt64?
    public var kinds: [String]?
}

/// One response line: `{"id","ok":true,"result"}`, `{"id","ok":false,"error"}` or `{"id","event"}`.
public enum Response<Result: Codable & Sendable>: Codable, Sendable {
    case ok(id: UInt64, result: Result)
    case err(id: UInt64, error: ErrorBody)
    case event(id: UInt64, event: Event)

    enum Key: CodingKey { case id, ok, result, error, event }

    public var id: UInt64 {
        switch self {
        case .ok(let id, _), .err(let id, _), .event(let id, _): id
        }
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Key.self)
        let id = try c.decode(UInt64.self, forKey: .id)
        if let event = try c.decodeIfPresent(Event.self, forKey: .event) {
            self = .event(id: id, event: event)
        } else if try c.decode(Bool.self, forKey: .ok) {
            self = .ok(id: id, result: try c.decode(Result.self, forKey: .result))
        } else {
            self = .err(id: id, error: try c.decode(ErrorBody.self, forKey: .error))
        }
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: Key.self)
        try c.encode(id, forKey: .id)
        switch self {
        case .ok(_, let result):
            try c.encode(true, forKey: .ok)
            try c.encode(result, forKey: .result)
        case .err(_, let error):
            try c.encode(false, forKey: .ok)
            try c.encode(error, forKey: .error)
        case .event(_, let event):
            try c.encode(event, forKey: .event)
        }
    }
}

extension Response: Equatable where Result: Equatable {}

/// Decodes any `result` without looking at it (e.g. a `watch` ack).
public struct Ignored: Codable, Sendable {
    public init() {}
    public init(from decoder: Decoder) throws {}
    public func encode(to encoder: Encoder) throws {}
}

public struct ErrorBody: Codable, Sendable, Equatable, Error {
    public var code: ErrorCode
    public var message: String
    public var hint: String?
    // ponytail: only the field the app reads; add a JSON value type if other details matter.
    public var details: Details?

    public struct Details: Codable, Sendable, Equatable {
        public var currentVersion: UInt64?
    }

    public init(code: ErrorCode, message: String, hint: String? = nil, details: Details? = nil) {
        self.code = code
        self.message = message
        self.hint = hint
        self.details = details
    }

    /// `current_version` from a `version_conflict` error's details.
    public var currentVersion: UInt64? { details?.currentVersion }
}

public enum ErrorCode: String, Codable, Sendable {
    case usage, notFound = "not_found", ambiguous, versionConflict = "version_conflict"
    /// Set by clients when the socket can't be reached; the daemon never sends it.
    case daemonUnreachable = "daemon_unreachable"
    case other

    public init(from decoder: Decoder) throws {
        self = ErrorCode(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .other
    }
}

public struct Event: Codable, Sendable, Equatable {
    public var seq: UInt64
    public var kind: String
    public var path: String?
    public var version: UInt64?

    public init(seq: UInt64, kind: String, path: String? = nil, version: UInt64? = nil) {
        self.seq = seq
        self.kind = kind
        self.path = path
        self.version = version
    }
}

public struct NoteSummary: Codable, Sendable, Equatable {
    public var path: String
    public var title: String
    public var version: UInt64
    /// Unix seconds.
    public var updatedAt: Int64
}

/// One `list` result element: `{"folder":"work"}` or a note summary. Folders come first.
public enum ListEntry: Codable, Sendable, Equatable {
    case folder(String)
    case note(NoteSummary)

    enum Key: CodingKey { case folder }

    public init(from decoder: Decoder) throws {
        if let folder = try decoder.container(keyedBy: Key.self).decodeIfPresent(String.self, forKey: .folder) {
            self = .folder(folder)
        } else {
            self = .note(try NoteSummary(from: decoder))
        }
    }

    public func encode(to encoder: Encoder) throws {
        switch self {
        case .folder(let folder):
            var c = encoder.container(keyedBy: Key.self)
            try c.encode(folder, forKey: .folder)
        case .note(let note):
            try note.encode(to: encoder)
        }
    }
}

public struct Note: Codable, Sendable, Equatable {
    public var path: String
    public var title: String
    public var version: UInt64
    public var body: String
    public var lineCount: UInt32
    public var createdAt: Int64
    public var updatedAt: Int64
}

public struct WriteResult: Codable, Sendable, Equatable {
    public var path: String
    public var version: UInt64
}
