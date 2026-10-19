// Arbitrary JSON for `json` parts, and the lazily expanded tree the app shows it as.
// Codable decoding can't see object key order (keyed containers are unordered), so
// `JSONValue.parse` reads raw bytes in order; `DaemonClient.postShow` uses it to put
// json parts back in the order the agent wrote them (serde_json preserve_order).

import Foundation

public indirect enum JSONValue: Codable, Sendable, Equatable {
    case null
    case bool(Bool)
    /// The number's literal text, so `1`, `1.0` and big integers show as written.
    case number(String)
    case string(String)
    case array([JSONValue])
    case object([Member])

    public struct Member: Sendable, Equatable {
        public var key: String
        public var value: JSONValue
        public init(_ key: String, _ value: JSONValue) {
            self.key = key
            self.value = value
        }
    }

    public subscript(key: String) -> JSONValue? {
        if case .object(let members) = self { members.first { $0.key == key }?.value } else { nil }
    }

    public subscript(index: Int) -> JSONValue? {
        if case .array(let items) = self, items.indices.contains(index) { items[index] } else { nil }
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() { self = .null }
        else if let b = try? c.decode(Bool.self) { self = .bool(b) }
        else if let i = try? c.decode(Int64.self) { self = .number(String(i)) }
        else if let d = try? c.decode(Double.self) { self = .number(String(d)) }
        else if let s = try? c.decode(String.self) { self = .string(s) }
        else if let a = try? c.decode([JSONValue].self) { self = .array(a) }
        else {
            // Unordered here; postShow restores the wire order with `parse`.
            self = .object(try c.decode([String: JSONValue].self).sorted { $0.key < $1.key }.map { Member($0.key, $0.value) })
        }
    }

    public func encode(to encoder: Encoder) throws {
        switch self {
        case .null:
            var c = encoder.singleValueContainer()
            try c.encodeNil()
        case .bool(let b):
            var c = encoder.singleValueContainer()
            try c.encode(b)
        case .number(let n):
            var c = encoder.singleValueContainer()
            if let i = Int64(n) { try c.encode(i) } else { try c.encode(Double(n) ?? 0) }
        case .string(let s):
            var c = encoder.singleValueContainer()
            try c.encode(s)
        case .array(let items):
            var c = encoder.unkeyedContainer()
            for item in items { try c.encode(item) }
        case .object(let members):
            // A dictionary, not a keyed container: Wire's snake_case strategy rewrites
            // keyed-container keys but leaves dictionary keys alone.
            var c = encoder.singleValueContainer()
            try c.encode(Dictionary(members.map { ($0.key, $0.value) }) { _, last in last })
        }
    }

    /// Parses JSON keeping object key order. Throws on malformed input.
    public static func parse(_ data: Data) throws -> JSONValue {
        try data.withUnsafeBytes { raw in
            var p = Parser(bytes: raw.bindMemory(to: UInt8.self))
            let value = try p.value()
            p.skipSpace()
            guard p.i == p.bytes.count else { throw p.fail("trailing characters") }
            return value
        }
    }
}

private struct Parser {
    let bytes: UnsafeBufferPointer<UInt8>
    var i = 0

    func fail(_ why: String) -> DaemonError { .protocolError("json at \(i): \(why)") }

    mutating func skipSpace() {
        while i < bytes.count, [0x20, 0x09, 0x0A, 0x0D].contains(bytes[i]) { i += 1 }
    }

    mutating func value() throws -> JSONValue {
        skipSpace()
        guard i < bytes.count else { throw fail("unexpected end") }
        switch bytes[i] {
        case UInt8(ascii: "{"):
            i += 1
            var members: [JSONValue.Member] = []
            skipSpace()
            if i < bytes.count, bytes[i] == UInt8(ascii: "}") { i += 1; return .object(members) }
            while true {
                skipSpace()
                let key = try string()
                skipSpace()
                try expect(":")
                members.append(.init(key, try value()))
                skipSpace()
                if try separator("}") { return .object(members) }
            }
        case UInt8(ascii: "["):
            i += 1
            var items: [JSONValue] = []
            skipSpace()
            if i < bytes.count, bytes[i] == UInt8(ascii: "]") { i += 1; return .array(items) }
            while true {
                items.append(try value())
                skipSpace()
                if try separator("]") { return .array(items) }
            }
        case UInt8(ascii: "\""): return .string(try string())
        case UInt8(ascii: "t"): try literal("true"); return .bool(true)
        case UInt8(ascii: "f"): try literal("false"); return .bool(false)
        case UInt8(ascii: "n"): try literal("null"); return .null
        default:
            let start = i
            while i < bytes.count, "+-0123456789.eE".utf8.contains(bytes[i]) { i += 1 }
            guard i > start else { throw fail("unexpected character") }
            return .number(String(decoding: UnsafeBufferPointer(rebasing: bytes[start..<i]), as: UTF8.self))
        }
    }

    /// True at the closing bracket, false after a comma.
    mutating func separator(_ close: Character) throws -> Bool {
        guard i < bytes.count else { throw fail("unexpected end") }
        if bytes[i] == UInt8(ascii: ",") { i += 1; return false }
        if bytes[i] == close.asciiValue! { i += 1; return true }
        throw fail("expected , or \(close)")
    }

    mutating func expect(_ c: Character) throws {
        guard i < bytes.count, bytes[i] == c.asciiValue! else { throw fail("expected \(c)") }
        i += 1
    }

    mutating func literal(_ word: String) throws {
        for b in word.utf8 {
            guard i < bytes.count, bytes[i] == b else { throw fail("expected \(word)") }
            i += 1
        }
    }

    mutating func string() throws -> String {
        try expect("\"")
        var out = [UInt8]()
        while i < bytes.count {
            let b = bytes[i]
            i += 1
            if b == UInt8(ascii: "\"") { return String(decoding: out, as: UTF8.self) }
            guard b == UInt8(ascii: "\\") else { out.append(b); continue }
            guard i < bytes.count else { break }
            let e = bytes[i]
            i += 1
            switch e {
            case UInt8(ascii: "n"): out.append(0x0A)
            case UInt8(ascii: "t"): out.append(0x09)
            case UInt8(ascii: "r"): out.append(0x0D)
            case UInt8(ascii: "b"): out.append(0x08)
            case UInt8(ascii: "f"): out.append(0x0C)
            case UInt8(ascii: "u"):
                var scalar = try hex4()
                if (0xD800..<0xDC00).contains(scalar), i + 1 < bytes.count, bytes[i] == UInt8(ascii: "\\"), bytes[i + 1] == UInt8(ascii: "u") {
                    i += 2
                    let low = try hex4()
                    scalar = 0x10000 + ((scalar - 0xD800) << 10) + (low - 0xDC00)
                }
                out.append(contentsOf: String(Character(Unicode.Scalar(scalar) ?? "\u{FFFD}")).utf8)
            default: out.append(e) // \" \\ \/
            }
        }
        throw fail("unterminated string")
    }

    mutating func hex4() throws -> UInt32 {
        guard i + 4 <= bytes.count, let v = UInt32(String(decoding: UnsafeBufferPointer(rebasing: bytes[i..<i + 4]), as: UTF8.self), radix: 16) else {
            throw fail("bad \\u escape")
        }
        i += 4
        return v
    }
}

// MARK: Tree

/// One row of the json tree. Children are built on first expand, and containers with
/// more than `JSONNode.page` entries are split into index ranges ([0…99], [100…199]),
/// nesting deeper for huge arrays, so expanding a million-item array builds 100 rows.
public final class JSONNode {
    public static let page = 100

    /// Object key, array index, or a range label like `[100…199]`.
    public let key: String?
    public let value: JSONValue
    /// Entry range a range node covers in `value`; nil for a value node.
    let range: Range<Int>?
    private var built: [JSONNode]?

    public init(key: String? = nil, value: JSONValue) {
        self.key = key
        self.value = value
        range = nil
    }

    private init(range: Range<Int>, of value: JSONValue) {
        key = "[\(range.lowerBound)…\(range.upperBound - 1)]"
        self.value = value
        self.range = range
    }

    public var isRange: Bool { range != nil }

    private var entryCount: Int {
        switch value {
        case .array(let a): a.count
        case .object(let m): m.count
        default: 0
        }
    }

    public var isExpandable: Bool { isRange || entryCount > 0 }

    /// `{3 keys}`, `[1 item]`, or the primitive's display text.
    public var summary: String {
        if let range { return "\(range.count) items" }
        switch value {
        case .array(let a): return a.isEmpty ? "[]" : "[\(a.count) item\(a.count == 1 ? "" : "s")]"
        case .object(let m): return m.isEmpty ? "{}" : "{\(m.count) key\(m.count == 1 ? "" : "s")}"
        default: return JSONNode.display(value)
        }
    }

    public static func display(_ value: JSONValue) -> String {
        switch value {
        case .null: "null"
        case .bool(let b): b ? "true" : "false"
        case .number(let n): n
        case .string(let s): "\"\(s)\""
        case .array, .object: ""
        }
    }

    public var children: [JSONNode] {
        if let built { return built }
        let all = range ?? 0..<entryCount
        var out: [JSONNode] = []
        if all.count > Self.page {
            var step = Self.page
            while all.count > step * Self.page { step *= Self.page }
            for lo in stride(from: all.lowerBound, to: all.upperBound, by: step) {
                out.append(JSONNode(range: lo..<min(lo + step, all.upperBound), of: value))
            }
        } else {
            switch value {
            case .array(let a): out = all.map { JSONNode(key: String($0), value: a[$0]) }
            case .object(let m): out = all.map { JSONNode(key: m[$0].key, value: m[$0].value) }
            default: break
            }
        }
        built = out
        return out
    }
}
