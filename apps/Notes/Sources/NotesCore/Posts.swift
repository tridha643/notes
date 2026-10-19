// Sideshow's data model on the wire: sessions, posts made of typed parts, anchored
// comments, assets, traces, kits and the daemon's render output. Mirrors
// crates/proto/src/posts.rs (the source of truth). Tagged enums (`Part`,
// `CommentAnchor`, `PartRender`) carry a kebab-case `kind`; optional fields are
// omitted when nil and empty lists when empty, like serde's skip_serializing_if.

import Foundation

// MARK: Parts

public enum Part: Codable, Sendable, Equatable {
    case html(html: String, kits: [String] = [], id: String? = nil)
    case markdown(markdown: String, id: String? = nil)
    case mermaid(mermaid: String, id: String? = nil)
    /// `patch` wins over `files` when both are set.
    case diff(patch: String? = nil, files: [DiffFile] = [], layout: DiffLayout = .unified, id: String? = nil)
    case image(assetId: String, alt: String? = nil, caption: String? = nil, id: String? = nil)
    case trace(steps: [TraceStep] = [], assetId: String? = nil, title: String? = nil, id: String? = nil)
    case terminal(text: String, cols: UInt32? = nil, title: String? = nil, id: String? = nil)
    case json(data: JSONValue, id: String? = nil)
    case code(code: String, language: String? = nil, title: String? = nil, lineStart: UInt32? = nil, id: String? = nil)

    enum Key: CodingKey {
        case kind, id, html, kits, markdown, mermaid, patch, files, layout, assetId, alt, caption
        case steps, title, text, cols, data, code, language, lineStart
    }

    public var kind: String {
        switch self {
        case .html: "html"
        case .markdown: "markdown"
        case .mermaid: "mermaid"
        case .diff: "diff"
        case .image: "image"
        case .trace: "trace"
        case .terminal: "terminal"
        case .json: "json"
        case .code: "code"
        }
    }

    public var id: String? {
        switch self {
        case .html(_, _, let id), .markdown(_, let id), .mermaid(_, let id), .diff(_, _, _, let id),
             .image(_, _, _, let id), .trace(_, _, _, let id), .terminal(_, _, _, let id), .json(_, let id),
             .code(_, _, _, _, let id): id
        }
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Key.self)
        let id = try c.decodeIfPresent(String.self, forKey: .id)
        func str(_ key: Key) throws -> String? { try c.decodeIfPresent(String.self, forKey: key) }
        switch try c.decode(String.self, forKey: .kind) {
        case "html":
            self = .html(html: try c.decode(String.self, forKey: .html), kits: try c.decodeIfPresent([String].self, forKey: .kits) ?? [], id: id)
        case "markdown": self = .markdown(markdown: try c.decode(String.self, forKey: .markdown), id: id)
        case "mermaid": self = .mermaid(mermaid: try c.decode(String.self, forKey: .mermaid), id: id)
        case "diff":
            self = .diff(patch: try str(.patch), files: try c.decodeIfPresent([DiffFile].self, forKey: .files) ?? [],
                         layout: try c.decodeIfPresent(DiffLayout.self, forKey: .layout) ?? .unified, id: id)
        case "image":
            self = .image(assetId: try c.decode(String.self, forKey: .assetId), alt: try str(.alt), caption: try str(.caption), id: id)
        case "trace":
            self = .trace(steps: try c.decodeIfPresent([TraceStep].self, forKey: .steps) ?? [], assetId: try str(.assetId), title: try str(.title), id: id)
        case "terminal":
            self = .terminal(text: try c.decode(String.self, forKey: .text), cols: try c.decodeIfPresent(UInt32.self, forKey: .cols), title: try str(.title), id: id)
        case "json": self = .json(data: try c.decodeIfPresent(JSONValue.self, forKey: .data) ?? .null, id: id)
        case "code":
            self = .code(code: try c.decode(String.self, forKey: .code), language: try str(.language), title: try str(.title),
                         lineStart: try c.decodeIfPresent(UInt32.self, forKey: .lineStart), id: id)
        case let kind:
            throw DecodingError.dataCorruptedError(forKey: .kind, in: c, debugDescription: "unknown part kind \(kind)")
        }
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: Key.self)
        try c.encode(kind, forKey: .kind)
        switch self {
        case .html(let html, let kits, _):
            try c.encode(html, forKey: .html)
            if !kits.isEmpty { try c.encode(kits, forKey: .kits) }
        case .markdown(let markdown, _): try c.encode(markdown, forKey: .markdown)
        case .mermaid(let mermaid, _): try c.encode(mermaid, forKey: .mermaid)
        case .diff(let patch, let files, let layout, _):
            try c.encodeIfPresent(patch, forKey: .patch)
            if !files.isEmpty { try c.encode(files, forKey: .files) }
            try c.encode(layout, forKey: .layout)
        case .image(let assetId, let alt, let caption, _):
            try c.encode(assetId, forKey: .assetId)
            try c.encodeIfPresent(alt, forKey: .alt)
            try c.encodeIfPresent(caption, forKey: .caption)
        case .trace(let steps, let assetId, let title, _):
            if !steps.isEmpty { try c.encode(steps, forKey: .steps) }
            try c.encodeIfPresent(assetId, forKey: .assetId)
            try c.encodeIfPresent(title, forKey: .title)
        case .terminal(let text, let cols, let title, _):
            try c.encode(text, forKey: .text)
            try c.encodeIfPresent(cols, forKey: .cols)
            try c.encodeIfPresent(title, forKey: .title)
        case .json(let data, _): try c.encode(data, forKey: .data)
        case .code(let code, let language, let title, let lineStart, _):
            try c.encode(code, forKey: .code)
            try c.encodeIfPresent(language, forKey: .language)
            try c.encodeIfPresent(title, forKey: .title)
            try c.encodeIfPresent(lineStart, forKey: .lineStart)
        }
        try c.encodeIfPresent(id, forKey: .id)
    }
}

public struct DiffFile: Codable, Sendable, Equatable {
    public var filename: String
    public var before: String
    public var after: String
    public var language: String?

    public init(filename: String, before: String, after: String, language: String? = nil) {
        self.filename = filename
        self.before = before
        self.after = after
        self.language = language
    }
}

public enum DiffLayout: String, Codable, Sendable {
    case unified, split
}

public struct TraceStep: Codable, Sendable, Equatable, Hashable {
    public var label: String
    public var kind: String?
    public var detail: String?
    /// ISO 8601.
    public var ts: String?

    public init(label: String, kind: String? = nil, detail: String? = nil, ts: String? = nil) {
        self.label = label
        self.kind = kind
        self.detail = detail
        self.ts = ts
    }
}

// MARK: Records

public struct Session: Codable, Sendable, Equatable {
    public var id: String
    public var agent: String
    public var title: String?
    public var cwd: String?
    /// Unix seconds.
    public var createdAt: Int64
    public var lastActiveAt: Int64
    public var postCount: UInt32

    public init(id: String, agent: String, title: String? = nil, cwd: String? = nil, createdAt: Int64, lastActiveAt: Int64, postCount: UInt32) {
        self.id = id
        self.agent = agent
        self.title = title
        self.cwd = cwd
        self.createdAt = createdAt
        self.lastActiveAt = lastActiveAt
        self.postCount = postCount
    }
}

public struct Post: Codable, Sendable, Equatable {
    public var id: String
    public var session: String
    public var title: String
    public var parts: [Part]
    public var version: UInt64
    public var createdAt: Int64
    public var updatedAt: Int64
}

public struct PostSummary: Codable, Sendable, Equatable {
    public var id: String
    public var session: String
    public var title: String
    /// Kind of each part, in order.
    public var kinds: [String]
    public var version: UInt64
    public var updatedAt: Int64
}

/// Result of every post write; `feedback` holds user comments the agent hasn't seen.
public struct PostWriteResult: Codable, Sendable, Equatable {
    public var post: PostSummary
    public var feedback: [Comment]

    enum Key: CodingKey { case post, feedback }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Key.self)
        post = try c.decode(PostSummary.self, forKey: .post)
        feedback = try c.decodeIfPresent([Comment].self, forKey: .feedback) ?? []
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: Key.self)
        try c.encode(post, forKey: .post)
        if !feedback.isEmpty { try c.encode(feedback, forKey: .feedback) }
    }
}

public struct Comment: Codable, Sendable, Equatable {
    public var id: String
    /// Monotonic per daemon.
    public var seq: UInt64
    public var session: String
    public var post: String?
    /// `user` for the person in the app, otherwise the agent name.
    public var author: String
    public var text: String
    public var createdAt: Int64
    public var anchor: CommentAnchor?

    public init(id: String, seq: UInt64, session: String, post: String? = nil, author: String, text: String, createdAt: Int64, anchor: CommentAnchor? = nil) {
        self.id = id
        self.seq = seq
        self.session = session
        self.post = post
        self.author = author
        self.text = text
        self.createdAt = createdAt
        self.anchor = anchor
    }
}

/// Where on a post a comment points. `part` is the 0-based part index; x/y/w/h are
/// fractions of the part's box.
public enum CommentAnchor: Codable, Sendable, Equatable {
    case point(part: UInt32, postVersion: UInt64, x: Double, y: Double)
    case rect(part: UInt32, postVersion: UInt64, x: Double, y: Double, w: Double, h: Double)
    case lineRange(part: UInt32, postVersion: UInt64, startLine: UInt32, endLine: UInt32, file: String? = nil)

    enum Key: CodingKey { case kind, part, postVersion, x, y, w, h, startLine, endLine, file }

    public var part: UInt32 {
        switch self {
        case .point(let part, _, _, _), .rect(let part, _, _, _, _, _), .lineRange(let part, _, _, _, _): part
        }
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Key.self)
        let part = try c.decode(UInt32.self, forKey: .part)
        let version = try c.decode(UInt64.self, forKey: .postVersion)
        func num(_ key: Key) throws -> Double { try c.decode(Double.self, forKey: key) }
        switch try c.decode(String.self, forKey: .kind) {
        case "point": self = .point(part: part, postVersion: version, x: try num(.x), y: try num(.y))
        case "rect": self = .rect(part: part, postVersion: version, x: try num(.x), y: try num(.y), w: try num(.w), h: try num(.h))
        case "line-range":
            self = .lineRange(part: part, postVersion: version, startLine: try c.decode(UInt32.self, forKey: .startLine),
                              endLine: try c.decode(UInt32.self, forKey: .endLine), file: try c.decodeIfPresent(String.self, forKey: .file))
        case let kind:
            throw DecodingError.dataCorruptedError(forKey: .kind, in: c, debugDescription: "unknown anchor kind \(kind)")
        }
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: Key.self)
        switch self {
        case .point(let part, let version, let x, let y):
            try c.encode("point", forKey: .kind)
            try c.encode(part, forKey: .part)
            try c.encode(version, forKey: .postVersion)
            try c.encode(x, forKey: .x)
            try c.encode(y, forKey: .y)
        case .rect(let part, let version, let x, let y, let w, let h):
            try c.encode("rect", forKey: .kind)
            try c.encode(part, forKey: .part)
            try c.encode(version, forKey: .postVersion)
            try c.encode(x, forKey: .x)
            try c.encode(y, forKey: .y)
            try c.encode(w, forKey: .w)
            try c.encode(h, forKey: .h)
        case .lineRange(let part, let version, let start, let end, let file):
            try c.encode("line-range", forKey: .kind)
            try c.encode(part, forKey: .part)
            try c.encode(version, forKey: .postVersion)
            try c.encode(start, forKey: .startLine)
            try c.encode(end, forKey: .endLine)
            try c.encodeIfPresent(file, forKey: .file)
        }
    }

    /// "part 2 · lines 4–9" / "part 1 · v3", like sideshow's anchor label.
    public var summary: String {
        switch self {
        case .lineRange(let part, _, let start, let end, let file):
            let lines = start == end ? "line \(start)" : "lines \(start)–\(end)"
            return "part \(part + 1) · " + (file.map { "\($0) " } ?? "") + lines
        case .point(let part, let version, _, _), .rect(let part, let version, _, _, _, _):
            return "part \(part + 1) · v\(version)"
        }
    }
}

public enum AssetKind: String, Codable, Sendable {
    case image, trace, file
}

/// `id` is the SHA-256 of the bytes.
public struct AssetInfo: Codable, Sendable, Equatable {
    public var id: String
    public var kind: AssetKind
    public var contentType: String
    public var byteLength: UInt64
    public var filename: String?
    public var createdAt: Int64
}

public struct AssetData: Codable, Sendable, Equatable {
    public var info: AssetInfo
    public var dataBase64: String

    public var data: Data? { Data(base64Encoded: dataBase64) }
}

public struct Kit: Codable, Sendable, Equatable {
    public var id: String
    public var label: String
    public var summary: String
    public var css: String
    public var js: String?
}

// MARK: Render output (post-render)

public struct Span: Codable, Sendable, Equatable {
    public var text: String
    /// Token class (`keyword`, `string`, ...) or for terminals SGR styles like `fg-red bold`.
    public var cls: String?

    enum CodingKeys: String, CodingKey { case text, cls = "class" }

    public init(_ text: String, _ cls: String? = nil) {
        self.text = text
        self.cls = cls
    }
}

public typealias StyledLine = [Span]

public enum DiffLineKind: String, Codable, Sendable {
    case context, added, removed
}

public struct DiffLine: Codable, Sendable, Equatable {
    public var kind: DiffLineKind
    public var oldLine: UInt32?
    public var newLine: UInt32?
    public var spans: StyledLine

    public init(kind: DiffLineKind, oldLine: UInt32? = nil, newLine: UInt32? = nil, spans: StyledLine) {
        self.kind = kind
        self.oldLine = oldLine
        self.newLine = newLine
        self.spans = spans
    }
}

public struct DiffHunk: Codable, Sendable, Equatable {
    public var header: String
    public var lines: [DiffLine]

    public init(header: String, lines: [DiffLine]) {
        self.header = header
        self.lines = lines
    }
}

public struct DiffFileRender: Codable, Sendable, Equatable {
    public var filename: String
    public var language: String?
    public var added: UInt32
    public var removed: UInt32
    public var hunks: [DiffHunk]

    public init(filename: String, language: String? = nil, added: UInt32, removed: UInt32, hunks: [DiffHunk]) {
        self.filename = filename
        self.language = language
        self.added = added
        self.removed = removed
        self.hunks = hunks
    }
}

/// Render output for one part, in post order; parts the app renders itself are `native`.
public enum PartRender: Codable, Sendable, Equatable {
    case code(language: String?, lineStart: UInt32, lines: [StyledLine])
    case diff(layout: DiffLayout, files: [DiffFileRender])
    case terminal(lines: [StyledLine])
    case native

    enum Key: CodingKey { case kind, language, lineStart, lines, layout, files }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Key.self)
        switch try c.decode(String.self, forKey: .kind) {
        case "code":
            self = .code(language: try c.decodeIfPresent(String.self, forKey: .language), lineStart: try c.decode(UInt32.self, forKey: .lineStart),
                         lines: try c.decode([StyledLine].self, forKey: .lines))
        case "diff": self = .diff(layout: try c.decode(DiffLayout.self, forKey: .layout), files: try c.decode([DiffFileRender].self, forKey: .files))
        case "terminal": self = .terminal(lines: try c.decode([StyledLine].self, forKey: .lines))
        default: self = .native
        }
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: Key.self)
        switch self {
        case .code(let language, let lineStart, let lines):
            try c.encode("code", forKey: .kind)
            try c.encode(language, forKey: .language) // serde writes `null` here: no skip_serializing_if
            try c.encode(lineStart, forKey: .lineStart)
            try c.encode(lines, forKey: .lines)
        case .diff(let layout, let files):
            try c.encode("diff", forKey: .kind)
            try c.encode(layout, forKey: .layout)
            try c.encode(files, forKey: .files)
        case .terminal(let lines):
            try c.encode("terminal", forKey: .kind)
            try c.encode(lines, forKey: .lines)
        case .native:
            try c.encode("native", forKey: .kind)
        }
    }
}

public struct PostRender: Codable, Sendable, Equatable {
    public var id: String
    public var version: UInt64
    public var parts: [PartRender]
}

// MARK: Verb arguments

public struct SessionNewArgs: Codable, Sendable, Equatable {
    public var agent: String
    public var title: String?
    public var cwd: String?
}

public struct SessionRenameArgs: Codable, Sendable, Equatable {
    public var id: String
    public var title: String
}

public struct IdArgs: Codable, Sendable, Equatable {
    public var id: String
}

public struct PostNewArgs: Codable, Sendable, Equatable {
    public var session: String?
    public var title: String?
    public var parts: [Part]
}

public struct PostUpdateArgs: Codable, Sendable, Equatable {
    public var id: String
    public var title: String?
    public var parts: [Part]?
}

public enum PartOp: String, Codable, Sendable {
    case add, set, rm
}

/// `target` is a part id or 0-based index; for `add`, the insert position.
public struct PostPartArgs: Codable, Sendable, Equatable {
    public var id: String
    public var op: PartOp
    public var target: String?
    public var part: Part?
}

public struct PostListArgs: Codable, Sendable, Equatable {
    public var session: String?
    public var limit: UInt32?
}

public struct PostShowArgs: Codable, Sendable, Equatable {
    public var id: String
    public var version: UInt64?
}

public struct CommentNewArgs: Codable, Sendable, Equatable {
    public var post: String?
    public var session: String?
    public var text: String
    public var anchor: CommentAnchor?
    public var author: String?

    public init(post: String? = nil, session: String? = nil, text: String, anchor: CommentAnchor? = nil, author: String? = nil) {
        self.post = post
        self.session = session
        self.text = text
        self.anchor = anchor
        self.author = author
    }
}

public struct CommentListArgs: Codable, Sendable, Equatable {
    public var session: String?
    public var post: String?
    public var afterSeq: UInt64?

    public init(session: String? = nil, post: String? = nil, afterSeq: UInt64? = nil) {
        self.session = session
        self.post = post
        self.afterSeq = afterSeq
    }
}

/// `timeout_secs` 0 = no timeout; always sent, like serde's `#[serde(default)]` field.
public struct WaitArgs: Codable, Sendable, Equatable {
    public var session: String?
    public var post: String?
    public var timeoutSecs: UInt64 = 0
}

public struct AssetPutArgs: Codable, Sendable, Equatable {
    public var session: String?
    public var kind: AssetKind
    public var contentType: String
    public var filename: String?
    public var dataBase64: String
}

public struct TraceSetArgs: Codable, Sendable, Equatable {
    public var session: String
    public var steps: [TraceStep]
}

public struct SettingGetArgs: Codable, Sendable, Equatable {
    public var key: String
}

public struct SettingSetArgs: Codable, Sendable, Equatable {
    public var key: String
    public var value: String
}
