import AppKit
import CryptoKit
import NotesCore

/// A post with every part prepared for display: highlighted text, styled markdown,
/// decoded images, parsed traces, pele layouts. Built on `PostLoader.queue`, then
/// handed to the main thread read-only.
final class LoadedPost: @unchecked Sendable {
    let post: Post
    /// Newest version; `post.version` is older while the user looks at history.
    let latest: UInt64
    let parts: [PartModel]

    init(post: Post, latest: UInt64, parts: [PartModel]) {
        self.post = post
        self.latest = latest
        self.parts = parts
    }

    var id: String { post.id }
}

enum PartModel {
    case code(CodeModel)
    case markdown(NSAttributedString)
    case mermaid(MermaidModel)
    case html(HTMLModel)
    case image(ImageModel)
    case json(JSONNode)
    case trace(TraceModel)
}

struct MermaidModel {
    let source: String
    /// Layout at natural width; narrower panes re-render with `maxWidth`.
    let natural: PeleDiagram?
    let error: String?
}

struct HTMLModel {
    let html: String
    let kits: [Kit]
    /// sha256 of the markup and kits; keys reported heights and loaded web views.
    let key: String
}

struct ImageModel {
    let image: CGImage?
    let data: Data?
    let alt: String?
    let caption: String?
}

struct TraceModel {
    let title: String
    let steps: [TraceStep]
    let note: String?
}

/// Everything a session view shows.
struct SessionSnapshot: @unchecked Sendable {
    var posts: [LoadedPost]
    var comments: [Comment]
    var trace: [TraceStep]
}

func sha256(_ s: String) -> String {
    SHA256.hash(data: Data(s.utf8)).map { String(format: "%02x", $0) }.joined()
}

/// Loads and prepares posts on one serial queue, separate from the app's request
/// queue so a comment send never waits behind a session load. Its caches are only
/// touched on that queue.
enum PostLoader {
    static let queue = DispatchQueue(label: "dev.notes.app.posts", qos: .userInitiated)
    /// post-render output by "id@version"; versions are immutable, so entries never go stale.
    // ponytail: unbounded; an LRU if sessions grow to thousands of posts.
    nonisolated(unsafe) private static var renders: [String: PostRender] = [:]
    nonisolated(unsafe) private static var kitList: [Kit]?
    nonisolated(unsafe) private static let assets: NSCache<NSString, NSData> = {
        let c = NSCache<NSString, NSData>()
        c.totalCostLimit = 64 << 20
        return c
    }()

    static func run<T: Sendable>(_ work: @escaping @Sendable (DaemonClient) throws -> T,
                                 then: @escaping @MainActor @Sendable (Result<T, Error>) -> Void) {
        queue.async {
            let result = Result { try work(client) }
            onMain { then(result) }
        }
    }

    static func session(_ client: DaemonClient, _ id: String) throws -> SessionSnapshot {
        // post-list is newest update first (then newest row), so reversed it's creation order
        // for posts created in the same second; created_at (seconds) sorts the rest.
        let summaries = try client.postList(session: id)
        let posts = try summaries.reversed().map { try load(client, $0.id) }.enumerated()
            .sorted { ($0.element.post.createdAt, $0.offset) < ($1.element.post.createdAt, $1.offset) }.map(\.element)
        let comments = try client.commentList(CommentListArgs(session: id)).sorted { $0.seq < $1.seq }
        let trace = (try? client.traceList(session: id)) ?? []
        return SessionSnapshot(posts: posts, comments: comments, trace: trace)
    }

    /// One post (the latest version unless `version`), with `post-render` requested once per version.
    static func load(_ client: DaemonClient, _ id: String, version: UInt64? = nil) throws -> LoadedPost {
        let post = try client.postShow(id, version: version)
        let latest = version == nil ? post.version : try client.postShow(id).version
        var render: PostRender?
        if post.parts.contains(where: { ["code", "diff", "terminal"].contains($0.kind) }) {
            let key = "\(id)@\(post.version)"
            if let cached = renders[key] { render = cached } else {
                render = try? client.postRender(id, version: post.version)
                renders[key] = render
            }
        }
        let parts = post.parts.enumerated().map { i, part in
            prepare(part, render: render?.parts.indices.contains(i) == true ? render!.parts[i] : nil, client: client)
        }
        return LoadedPost(post: post, latest: latest, parts: parts)
    }

    static func prepare(_ part: Part, render: PartRender?, client: DaemonClient) -> PartModel {
        switch part {
        case .code, .diff, .terminal:
            return .code(CodeModel.build(part: part, render: render) ?? CodeModel(panes: [], title: nil, terminal: false))
        case .markdown(let markdown, _):
            return .markdown(styledMarkdown(markdown))
        case .mermaid(let source, _):
            do { return .mermaid(MermaidModel(source: source, natural: try Pele.shared.render(source), error: nil)) }
            catch { return .mermaid(MermaidModel(source: source, natural: nil, error: error.message)) }
        case .html(let html, let ids, _):
            let all = ids.isEmpty ? [] : kits(client)
            let kits = ids.compactMap { id in all.first { $0.id == id } }
            return .html(HTMLModel(html: html, kits: kits, key: sha256(html + "\u{0}" + ids.joined(separator: ","))))
        case .image(let assetId, let alt, let caption, _):
            let data = asset(client, assetId)
            let image = data.flatMap { CGImageSourceCreateWithData($0 as CFData, nil) }
                .flatMap { CGImageSourceCreateImageAtIndex($0, 0, [kCGImageSourceShouldCacheImmediately: true] as CFDictionary) }
            return .image(ImageModel(image: image, data: data, alt: alt, caption: caption))
        case .json(let data, _):
            return .json(JSONNode(value: data))
        case .trace(let steps, let assetId, let title, _):
            var all = steps, note: String?
            if steps.isEmpty, let assetId {
                if let data = asset(client, assetId) { all = parseTrace(String(decoding: data, as: UTF8.self)) }
                else { note = "Trace file unavailable — it may have been evicted." }
            }
            return .trace(TraceModel(title: title ?? "Agent trace", steps: all, note: note))
        }
    }

    /// The editor's markdown styling, applied once to a read-only copy.
    static func styledMarkdown(_ markdown: String) -> NSAttributedString {
        let text = markdown.hasSuffix("\n") ? String(markdown.dropLast()) : markdown
        let storage = NSTextStorage(string: text, attributes: Style.base)
        Style.restyle(storage, edited: NSRange(location: 0, length: storage.length))
        return NSAttributedString(attributedString: storage)
    }

    /// Bytes of an asset, fetched once.
    static func asset(_ client: DaemonClient, _ id: String) -> Data? {
        if let cached = assets.object(forKey: id as NSString) { return cached as Data }
        guard let data = try? client.assetGet(id).data else { return nil }
        assets.setObject(data as NSData, forKey: id as NSString, cost: data.count)
        return data
    }

    /// The `kits` verb, asked once per launch.
    static func kits(_ client: DaemonClient) -> [Kit] {
        if let kitList { return kitList }
        guard let list = try? client.kits() else { return [] }
        kitList = list
        return list
    }
}
