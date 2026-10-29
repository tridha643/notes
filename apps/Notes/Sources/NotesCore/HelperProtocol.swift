// The stage ↔ stage-helper wire, mirroring crates/proto/src/helper.rs. JSON lines; every
// request has an `id` and its fields are exactly the ones named here: a missing field,
// an unknown field or an unknown op is an error, like `deny_unknown_fields` on the Rust
// side. Fixtures in crates/proto/tests/fixtures/helper/ pin both ends.

import Foundation

/// One request line from stage.
public enum HelperRequest: Sendable, Equatable, Codable {
    case warm(id: Int)
    case mermaid(id: Int, source: String, maxWidth: Double, fontSize: Double)
    case image(id: Int, path: String)
    case htmlOpen(id: Int, path: String, width: Double, maxHeight: Double, scale: Double, fps: Double)
    case htmlUpdate(id: Int, path: String)
    case htmlAck(id: Int, frame: Int)
    case htmlPause(id: Int)
    case htmlResume(id: Int)
    case htmlClose(id: Int)
    case htmlResize(id: Int, width: Double, maxHeight: Double, scale: Double)

    enum Key: String, CodingKey, CaseIterable {
        case op, id, source, path, width, scale, fps, frame
        case maxWidth = "max_width", fontSize = "font_size", maxHeight = "max_height"
    }

    /// The wire name and the fields (besides `op`) it carries.
    private static let ops: [String: Set<Key>] = [
        "warm": [.id], "mermaid": [.id, .source, .maxWidth, .fontSize], "image": [.id, .path],
        "html-open": [.id, .path, .width, .maxHeight, .scale, .fps], "html-update": [.id, .path],
        "html-ack": [.id, .frame], "html-pause": [.id], "html-resume": [.id], "html-close": [.id],
        "html-resize": [.id, .width, .maxHeight, .scale],
    ]

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Key.self)
        let op = try c.decode(String.self, forKey: .op)
        guard let fields = Self.ops[op] else {
            throw DecodingError.dataCorruptedError(forKey: .op, in: c, debugDescription: "unknown op \"\(op)\"")
        }
        let present = try decoder.container(keyedBy: AnyKey.self).allKeys.map(\.stringValue)
        if let extra = present.first(where: { $0 != "op" && Key(rawValue: $0).map(fields.contains) != true }) {
            throw DecodingError.dataCorruptedError(forKey: .op, in: c, debugDescription: "\(op) has no field \"\(extra)\"")
        }
        let id = try c.decode(Int.self, forKey: .id)
        switch op {
        case "warm": self = .warm(id: id)
        case "mermaid":
            self = .mermaid(id: id, source: try c.decode(String.self, forKey: .source),
                            maxWidth: try c.decode(Double.self, forKey: .maxWidth), fontSize: try c.decode(Double.self, forKey: .fontSize))
        case "image": self = .image(id: id, path: try c.decode(String.self, forKey: .path))
        case "html-open":
            self = .htmlOpen(id: id, path: try c.decode(String.self, forKey: .path), width: try c.decode(Double.self, forKey: .width),
                             maxHeight: try c.decode(Double.self, forKey: .maxHeight), scale: try c.decode(Double.self, forKey: .scale),
                             fps: try c.decode(Double.self, forKey: .fps))
        case "html-update": self = .htmlUpdate(id: id, path: try c.decode(String.self, forKey: .path))
        case "html-ack": self = .htmlAck(id: id, frame: try c.decode(Int.self, forKey: .frame))
        case "html-pause": self = .htmlPause(id: id)
        case "html-resume": self = .htmlResume(id: id)
        case "html-close": self = .htmlClose(id: id)
        default:
            self = .htmlResize(id: id, width: try c.decode(Double.self, forKey: .width), maxHeight: try c.decode(Double.self, forKey: .maxHeight),
                               scale: try c.decode(Double.self, forKey: .scale))
        }
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: Key.self)
        switch self {
        case .warm(let id): try c.encode("warm", forKey: .op); try c.encode(id, forKey: .id)
        case .mermaid(let id, let source, let maxWidth, let fontSize):
            try c.encode("mermaid", forKey: .op); try c.encode(id, forKey: .id); try c.encode(source, forKey: .source)
            try c.encode(maxWidth, forKey: .maxWidth); try c.encode(fontSize, forKey: .fontSize)
        case .image(let id, let path): try c.encode("image", forKey: .op); try c.encode(id, forKey: .id); try c.encode(path, forKey: .path)
        case .htmlOpen(let id, let path, let width, let maxHeight, let scale, let fps):
            try c.encode("html-open", forKey: .op); try c.encode(id, forKey: .id); try c.encode(path, forKey: .path)
            try c.encode(width, forKey: .width); try c.encode(maxHeight, forKey: .maxHeight)
            try c.encode(scale, forKey: .scale); try c.encode(fps, forKey: .fps)
        case .htmlUpdate(let id, let path): try c.encode("html-update", forKey: .op); try c.encode(id, forKey: .id); try c.encode(path, forKey: .path)
        case .htmlAck(let id, let frame): try c.encode("html-ack", forKey: .op); try c.encode(id, forKey: .id); try c.encode(frame, forKey: .frame)
        case .htmlPause(let id): try c.encode("html-pause", forKey: .op); try c.encode(id, forKey: .id)
        case .htmlResume(let id): try c.encode("html-resume", forKey: .op); try c.encode(id, forKey: .id)
        case .htmlClose(let id): try c.encode("html-close", forKey: .op); try c.encode(id, forKey: .id)
        case .htmlResize(let id, let width, let maxHeight, let scale):
            try c.encode("html-resize", forKey: .op); try c.encode(id, forKey: .id)
            try c.encode(width, forKey: .width); try c.encode(maxHeight, forKey: .maxHeight); try c.encode(scale, forKey: .scale)
        }
    }

    /// The `id` of the request line `data` is, if it has a readable one: what a failure reply echoes.
    public static func id(in data: Data) -> Int? {
        ((try? JSONSerialization.jsonObject(with: data)) as? [String: Any])?["id"] as? Int
    }
}

private struct AnyKey: CodingKey {
    let stringValue: String
    var intValue: Int? { nil }
    init?(stringValue: String) { self.stringValue = stringValue }
    init?(intValue: Int) { nil }
}

/// One line from the helper: a reply or a frame. `id` is nil in a failure only when the
/// request line couldn't be read at all.
public enum HelperReply: Sendable, Equatable, Encodable {
    case frame(id: Int, frame: Int, shm: String, w: Int, h: Int)
    case failed(id: Int?, error: String)
    case mermaid(id: Int, svg: String, width: Double, height: Double)
    case patched(id: Int, patched: Bool)
    case image(id: Int, shm: String, w: Int, h: Int)
    case done(id: Int)

    enum Key: String, CodingKey { case id, ok, error, frame, shm, w, h, svg, width, height, patched }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: Key.self)
        switch self {
        case .frame(let id, let frame, let shm, let w, let h):
            try c.encode(id, forKey: .id); try c.encode(frame, forKey: .frame)
            try c.encode(shm, forKey: .shm); try c.encode(w, forKey: .w); try c.encode(h, forKey: .h)
        case .failed(let id, let error):
            if let id { try c.encode(id, forKey: .id) } else { try c.encodeNil(forKey: .id) }
            try c.encode(false, forKey: .ok); try c.encode(error, forKey: .error)
        case .mermaid(let id, let svg, let width, let height):
            try c.encode(id, forKey: .id); try c.encode(true, forKey: .ok)
            try c.encode(svg, forKey: .svg); try c.encode(width, forKey: .width); try c.encode(height, forKey: .height)
        case .patched(let id, let patched):
            try c.encode(id, forKey: .id); try c.encode(true, forKey: .ok); try c.encode(patched, forKey: .patched)
        case .image(let id, let shm, let w, let h):
            try c.encode(id, forKey: .id); try c.encode(true, forKey: .ok)
            try c.encode(shm, forKey: .shm); try c.encode(w, forKey: .w); try c.encode(h, forKey: .h)
        case .done(let id):
            try c.encode(id, forKey: .id); try c.encode(true, forKey: .ok)
        }
    }
}

/// A request line that didn't decode, with the id to answer it under.
public struct HelperRequestError: Error, Sendable {
    public let id: Int?
    public let message: String
}

extension HelperRequest {
    /// Decode one line, or say why not.
    public static func parse(_ line: Data) throws -> HelperRequest {
        do {
            return try JSONDecoder().decode(HelperRequest.self, from: line)
        } catch let DecodingError.keyNotFound(key, _) {
            throw HelperRequestError(id: id(in: line), message: "missing \"\(key.stringValue)\"")
        } catch let DecodingError.typeMismatch(_, context) {
            throw HelperRequestError(id: id(in: line), message: "wrong type for \"\(context.codingPath.last?.stringValue ?? "?")\"")
        } catch let DecodingError.dataCorrupted(context) {
            throw HelperRequestError(id: id(in: line), message: context.debugDescription)
        } catch {
            throw HelperRequestError(id: id(in: line), message: "\(error)")
        }
    }
}
