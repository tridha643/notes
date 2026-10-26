// The Rust wire contract, crates/proto/tests/fixtures: every request the app sends
// must decode here and re-encode without a key Rust lacks (notesd rejects unknown
// fields), and every sideshow result the app reads must decode.

import Foundation
import Testing
@testable import NotesCore

private let fixtures = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent()  // NotesCoreTests
    .deletingLastPathComponent()  // Tests
    .deletingLastPathComponent()  // Notes
    .deletingLastPathComponent()  // apps
    .deletingLastPathComponent()  // repo root
    .appendingPathComponent("crates/proto/tests/fixtures")

/// Paths of keys in `swift` that `rust` doesn't have.
private func extraKeys(_ swift: Any, _ rust: Any, at path: String) -> [String] {
    switch (swift, rust) {
    case let (s as [String: Any], r as [String: Any]):
        return s.flatMap { key, value in
            r[key].map { extraKeys(value, $0, at: "\(path).\(key)") } ?? ["\(path).\(key)"]
        }
    case let (s as [Any], r as [Any]):
        return zip(s, r).enumerated().flatMap { i, pair in extraKeys(pair.0, pair.1, at: "\(path)[\(i)]") }
    default:
        return []
    }
}

private func checkRequest<Args: Codable & Sendable>(_ verb: String, _: Args.Type) throws {
    let golden = try Data(contentsOf: fixtures.appendingPathComponent("\(verb).request.json"))
    let request = try Wire.decoder.decode(Request<Args>.self, from: golden)
    #expect(request.verb == verb)
    let again = try Wire.encoder.encode(request)
    let extra = extraKeys(
        try JSONSerialization.jsonObject(with: again), try JSONSerialization.jsonObject(with: golden), at: verb)
    #expect(extra.isEmpty, "Swift sends keys notesd would reject: \(extra)")
}

private func checkResponse<Result: Codable & Sendable>(_ verb: String, _: Result.Type) throws {
    let golden = try Data(contentsOf: fixtures.appendingPathComponent("\(verb).response.json"))
    let response = try Wire.decoder.decode(Response<Result>.self, from: golden)
    guard case .ok(id: 7, result: _) = response else {
        Issue.record("\(verb): not an ok response")
        return
    }
}

@Test func requestsTheAppSendsMatchTheRustContract() throws {
    try checkRequest("list", ListArgs.self)
    try checkRequest("read", NoteArgs.self)
    try checkRequest("new", NewArgs.self)
    try checkRequest("write", WriteArgs.self)
    try checkRequest("delete", DeleteArgs.self)
    try checkRequest("restore", NoteArgs.self)
    try checkRequest("watch", WatchArgs.self)
    try checkRequest("session-new", SessionNewArgs.self)
    try checkRequest("session-list", NoArgs.self)
    try checkRequest("session-rename", SessionRenameArgs.self)
    try checkRequest("session-delete", IdArgs.self)
    try checkRequest("post-new", PostNewArgs.self)
    try checkRequest("post-update", PostUpdateArgs.self)
    try checkRequest("post-part", PostPartArgs.self)
    try checkRequest("post-list", PostListArgs.self)
    try checkRequest("post-show", PostShowArgs.self)
    try checkRequest("post-render", PostShowArgs.self)
    try checkRequest("post-delete", IdArgs.self)
    try checkRequest("comment-new", CommentNewArgs.self)
    try checkRequest("comment-list", CommentListArgs.self)
    try checkRequest("comment-delete", IdArgs.self)
    try checkRequest("wait", WaitArgs.self)
    try checkRequest("asset-put", AssetPutArgs.self)
    try checkRequest("asset-get", IdArgs.self)
    try checkRequest("trace-set", TraceSetArgs.self)
    try checkRequest("trace-list", IdArgs.self)
    try checkRequest("kits", NoArgs.self)
    try checkRequest("setting-get", SettingGetArgs.self)
    try checkRequest("setting-set", SettingSetArgs.self)
}

@Test func sideshowResultsDecodeFromTheRustContract() throws {
    try checkResponse("session-new", Session.self)
    try checkResponse("session-list", [Session].self)
    try checkResponse("post-new", PostWriteResult.self)
    try checkResponse("post-list", [PostSummary].self)
    try checkResponse("post-show", Post.self)
    try checkResponse("post-render", PostRender.self)
    try checkResponse("comment-new", NotesCore.Comment.self)
    try checkResponse("comment-list", [NotesCore.Comment].self)
    try checkResponse("asset-put", AssetInfo.self)
    try checkResponse("trace-list", [TraceStep].self)
}
