// Sideshow wire shapes, written the way serde emits crates/proto/src/posts.rs:
// tag first, snake_case fields, None/empty-Vec fields omitted, `layout` and
// PartRender::Code's `language` always present.

import Foundation
import Testing
@testable import NotesCore

/// Decodes `json` as T, checks it re-encodes to the same JSON, returns the value.
@discardableResult
func roundTrip<T: Codable & Equatable>(_ type: T.Type, _ json: String) throws -> T {
    let value = try decode(type, json)
    #expect(try object(Wire.encoder.encode(value)) == object(json), "\(json)")
    #expect(try Wire.decoder.decode(type, from: Wire.encoder.encode(value)) == value)
    return value
}

@Test func everyPartKindRoundTrips() throws {
    #expect(try roundTrip(Part.self, #"{"kind":"html","html":"<b>x</b>","kits":["slides","issues"],"id":"h"}"#)
        == .html(html: "<b>x</b>", kits: ["slides", "issues"], id: "h"))
    #expect(try roundTrip(Part.self, #"{"kind":"html","html":"<i>"}"#) == .html(html: "<i>"))
    #expect(try roundTrip(Part.self, ##"{"kind":"markdown","markdown":"# Why"}"##) == .markdown(markdown: "# Why"))
    #expect(try roundTrip(Part.self, #"{"kind":"mermaid","mermaid":"flowchart LR\n A-->B","id":"m"}"#) == .mermaid(mermaid: "flowchart LR\n A-->B", id: "m"))
    #expect(try roundTrip(Part.self, #"{"kind":"diff","patch":"--- a\n+++ b\n","layout":"unified"}"#) == .diff(patch: "--- a\n+++ b\n"))
    #expect(try roundTrip(Part.self, #"{"kind":"diff","files":[{"filename":"a.rs","before":"x","after":"y","language":"rust"}],"layout":"split"}"#)
        == .diff(files: [DiffFile(filename: "a.rs", before: "x", after: "y", language: "rust")], layout: .split))
    // serde's #[serde(default)] layout: missing on input means unified.
    #expect(try decode(Part.self, #"{"kind":"diff","patch":"p"}"#) == .diff(patch: "p", layout: .unified))
    #expect(try roundTrip(Part.self, #"{"kind":"image","asset_id":"abc","alt":"A","caption":"C"}"#) == .image(assetId: "abc", alt: "A", caption: "C"))
    #expect(try roundTrip(Part.self, #"{"kind":"trace","steps":[{"label":"read","kind":"tool","detail":"d","ts":"2026-01-01T00:00:00Z"},{"label":"edit"}],"title":"T"}"#)
        == .trace(steps: [TraceStep(label: "read", kind: "tool", detail: "d", ts: "2026-01-01T00:00:00Z"), TraceStep(label: "edit")], title: "T"))
    #expect(try roundTrip(Part.self, #"{"kind":"trace","asset_id":"t1"}"#) == .trace(assetId: "t1"))
    #expect(try roundTrip(Part.self, #"{"kind":"terminal","text":"\u001b[31mred\u001b[0m","cols":80,"title":"cargo"}"#)
        == .terminal(text: "\u{1b}[31mred\u{1b}[0m", cols: 80, title: "cargo"))
    #expect(try roundTrip(Part.self, #"{"kind":"json","data":{"a":[1,null,2.5,"s",true,{"user_id":7}]}}"#)
        == .json(data: .object([.init("a", .array([.number("1"), .null, .number("2.5"), .string("s"), .bool(true), .object([.init("user_id", .number("7"))])]))])))
    #expect(try roundTrip(Part.self, #"{"kind":"code","code":"fn main() {}\n","language":"rust","title":"main.rs","line_start":3,"id":"c"}"#)
        == .code(code: "fn main() {}\n", language: "rust", title: "main.rs", lineStart: 3, id: "c"))
    #expect(throws: (any Error).self) { try decode(Part.self, #"{"kind":"video","src":"x"}"#) }
}

@Test func anchorsRoundTrip() throws {
    #expect(try roundTrip(CommentAnchor.self, #"{"kind":"point","part":1,"post_version":2,"x":0.25,"y":0.5}"#)
        == .point(part: 1, postVersion: 2, x: 0.25, y: 0.5))
    #expect(try roundTrip(CommentAnchor.self, #"{"kind":"rect","part":0,"post_version":3,"x":0.1,"y":0.2,"w":0.3,"h":0.4}"#)
        == .rect(part: 0, postVersion: 3, x: 0.1, y: 0.2, w: 0.3, h: 0.4))
    let lines = try roundTrip(CommentAnchor.self, #"{"kind":"line-range","part":1,"post_version":2,"start_line":4,"end_line":9}"#)
    #expect(lines == .lineRange(part: 1, postVersion: 2, startLine: 4, endLine: 9))
    #expect(lines.summary == "part 2 · lines 4–9")
    try roundTrip(CommentAnchor.self, #"{"kind":"line-range","part":0,"post_version":1,"start_line":2,"end_line":2,"file":"src/lib.rs"}"#)
}

@Test func renderTypesRoundTrip() throws {
    #expect(try roundTrip(PartRender.self, #"{"kind":"code","language":"rust","line_start":1,"lines":[[{"text":"fn","class":"keyword"},{"text":" main"}]]}"#)
        == .code(language: "rust", lineStart: 1, lines: [[Span("fn", "keyword"), Span(" main")]]))
    // No skip_serializing_if on Code.language: serde writes null.
    try roundTrip(PartRender.self, #"{"kind":"code","language":null,"line_start":7,"lines":[]}"#)
    let diff = try roundTrip(PartRender.self, #"""
        {"kind":"diff","layout":"split","files":[{"filename":"a.rs","language":"rust","added":1,"removed":1,"hunks":[{"header":"@@ -1,2 +1,2 @@",
        "lines":[{"kind":"context","old_line":1,"new_line":1,"spans":[{"text":"use x;"}]},{"kind":"removed","old_line":2,"spans":[{"text":"a"}]},
        {"kind":"added","new_line":2,"spans":[{"text":"b","class":"string"}]}]}]}]}
        """#)
    guard case .diff(.split, let files) = diff else { Issue.record("expected diff"); return }
    #expect(files[0].hunks[0].lines[1] == DiffLine(kind: .removed, oldLine: 2, spans: [Span("a")]))
    #expect(try roundTrip(PartRender.self, #"{"kind":"terminal","lines":[[{"text":"ok","class":"fg-green bold"}]]}"#) == .terminal(lines: [[Span("ok", "fg-green bold")]]))
    #expect(try roundTrip(PartRender.self, #"{"kind":"native"}"#) == .native)
    try roundTrip(PostRender.self, #"{"id":"p1","version":3,"parts":[{"kind":"native"},{"kind":"terminal","lines":[]}]}"#)
}

@Test func recordsRoundTrip() throws {
    try roundTrip(Session.self, #"{"id":"s1","agent":"claude","title":"Fix","cwd":"/r","created_at":1,"last_active_at":2,"post_count":3}"#)
    try roundTrip(Session.self, #"{"id":"s2","agent":"codex","created_at":0,"last_active_at":0,"post_count":0}"#)
    let post = try roundTrip(Post.self, #"{"id":"p1","session":"s1","title":"Plan","parts":[{"kind":"markdown","markdown":"x","id":"intro"},{"kind":"code","code":"y"}],"version":2,"created_at":0,"updated_at":5}"#)
    #expect(post.parts.map(\.kind) == ["markdown", "code"] && post.parts[0].id == "intro")
    try roundTrip(PostSummary.self, #"{"id":"p1","session":"s1","title":"Plan","kinds":["markdown","code"],"version":2,"updated_at":0}"#)
    try roundTrip(PostWriteResult.self, #"{"post":{"id":"p1","session":"s1","title":"Plan","kinds":["markdown"],"version":1,"updated_at":0}}"#)
    let written = try roundTrip(PostWriteResult.self, #"{"post":{"id":"p1","session":"s1","title":"Plan","kinds":["markdown"],"version":1,"updated_at":0},"feedback":[{"id":"c7","seq":7,"session":"s1","post":"p1","author":"user","text":"looks\n  good","created_at":0}]}"#)
    #expect(written.feedback.first?.text == "looks\n  good")
    try roundTrip(NotesCore.Comment.self, #"{"id":"c8","seq":8,"session":"s1","post":"p1","author":"tester","text":"fix this","created_at":0,"anchor":{"kind":"line-range","part":0,"post_version":3,"start_line":4,"end_line":9}}"#)
    try roundTrip(NotesCore.Comment.self, #"{"id":"c9","seq":9,"session":"s1","author":"user","text":"hi","created_at":0}"#)
    try roundTrip(AssetInfo.self, #"{"id":"sha","kind":"image","content_type":"image/png","byte_length":4,"filename":"a.png","created_at":0}"#)
    let asset = try roundTrip(AssetData.self, #"{"info":{"id":"sha","kind":"trace","content_type":"text/plain","byte_length":2,"created_at":0},"data_base64":"aGk="}"#)
    #expect(asset.data == Data("hi".utf8))
    try roundTrip(Kit.self, #"{"id":"slides","label":"Slides","summary":"Deck","css":".deck{}","js":"go()"}"#)
    try roundTrip(Kit.self, #"{"id":"issues","label":"Issues","summary":"Cards","css":""}"#)
    try roundTrip([TraceStep].self, #"[{"label":"a"},{"label":"b","kind":"bash","detail":"d","ts":"t"}]"#)
}

@Test func sideshowRequestLinesMatchTheWire() throws {
    func line<A: Codable & Sendable>(_ verb: String, _ args: A) throws -> NSObject { try object(Wire.line(Request(id: 1, verb: verb, args: args))) }
    // Unit verbs have no `args`, like serde's unit variants.
    #expect(try line("session-list", NoArgs()) == object(#"{"v":1,"id":1,"verb":"session-list"}"#))
    #expect(try line("kits", NoArgs()) == object(#"{"v":1,"id":1,"verb":"kits"}"#))
    #expect(try Wire.decoder.decode(Request<NoArgs>.self, from: Data(#"{"v":1,"id":5,"verb":"kits"}"#.utf8)).verb == "kits")
    #expect(try line("post-show", PostShowArgs(id: "p1", version: 2)) == object(#"{"v":1,"id":1,"verb":"post-show","args":{"id":"p1","version":2}}"#))
    #expect(try line("post-render", PostShowArgs(id: "p1")) == object(#"{"v":1,"id":1,"verb":"post-render","args":{"id":"p1"}}"#))
    #expect(try line("post-list", PostListArgs(session: "s1")) == object(#"{"v":1,"id":1,"verb":"post-list","args":{"session":"s1"}}"#))
    #expect(try line("comment-new", CommentNewArgs(post: "p1", text: "fix this", anchor: .lineRange(part: 0, postVersion: 3, startLine: 4, endLine: 9), author: "user"))
        == object(#"{"v":1,"id":1,"verb":"comment-new","args":{"post":"p1","text":"fix this","anchor":{"kind":"line-range","part":0,"post_version":3,"start_line":4,"end_line":9},"author":"user"}}"#))
    #expect(try line("comment-list", CommentListArgs(session: "s1", afterSeq: 3)) == object(#"{"v":1,"id":1,"verb":"comment-list","args":{"session":"s1","after_seq":3}}"#))
    #expect(try line("wait", WaitArgs(post: "p1", timeoutSecs: 5)) == object(#"{"v":1,"id":1,"verb":"wait","args":{"post":"p1","timeout_secs":5}}"#))
    #expect(try line("asset-get", IdArgs(id: "sha")) == object(#"{"v":1,"id":1,"verb":"asset-get","args":{"id":"sha"}}"#))
    #expect(try line("asset-put", AssetPutArgs(session: "s1", kind: .image, contentType: "image/png", filename: "a.png", dataBase64: "iVBORw=="))
        == object(#"{"v":1,"id":1,"verb":"asset-put","args":{"session":"s1","kind":"image","content_type":"image/png","filename":"a.png","data_base64":"iVBORw=="}}"#))
    #expect(try line("post-part", PostPartArgs(id: "p1", op: .set, target: "0", part: .terminal(text: "x", cols: 80, title: "cargo")))
        == object(#"{"v":1,"id":1,"verb":"post-part","args":{"id":"p1","op":"set","target":"0","part":{"kind":"terminal","text":"x","cols":80,"title":"cargo"}}}"#))
    #expect(try line("post-new", PostNewArgs(title: "Data", parts: [.json(data: .object([.init("a", .array([.number("1"), .null]))]))]))
        == object(#"{"v":1,"id":1,"verb":"post-new","args":{"title":"Data","parts":[{"kind":"json","data":{"a":[1,null]}}]}}"#))
    #expect(try line("post-update", PostUpdateArgs(id: "p1", parts: [.markdown(markdown: "x")])) == object(#"{"v":1,"id":1,"verb":"post-update","args":{"id":"p1","parts":[{"kind":"markdown","markdown":"x"}]}}"#))
    #expect(try line("session-new", SessionNewArgs(agent: "claude", title: "Fix")) == object(#"{"v":1,"id":1,"verb":"session-new","args":{"agent":"claude","title":"Fix"}}"#))
    #expect(try line("session-rename", SessionRenameArgs(id: "s1", title: "New")) == object(#"{"v":1,"id":1,"verb":"session-rename","args":{"id":"s1","title":"New"}}"#))
    #expect(try line("trace-set", TraceSetArgs(session: "s1", steps: [TraceStep(label: "read"), TraceStep(label: "edit", kind: "tool")]))
        == object(#"{"v":1,"id":1,"verb":"trace-set","args":{"session":"s1","steps":[{"label":"read"},{"label":"edit","kind":"tool"}]}}"#))
    #expect(try line("setting-set", SettingSetArgs(key: "theme", value: "github")) == object(#"{"v":1,"id":1,"verb":"setting-set","args":{"key":"theme","value":"github"}}"#))
    #expect(try line("setting-get", SettingGetArgs(key: "theme")) == object(#"{"v":1,"id":1,"verb":"setting-get","args":{"key":"theme"}}"#))
}

@Test func sideshowEventsCarryIdAndSession() throws {
    let r = try decode(Response<Ignored>.self, #"{"id":0,"event":{"seq":12,"kind":"comment-created","id":"c3","session":"s1"}}"#)
    guard case .event(_, let e) = r else { Issue.record("expected event"); return }
    #expect(e == Event(seq: 12, kind: "comment-created", id: "c3", session: "s1"))
    #expect(try object(Wire.encoder.encode(e)) == object(#"{"seq":12,"kind":"comment-created","id":"c3","session":"s1"}"#))
}

@Test func jsonParseKeepsKeyOrderAndLiterals() throws {
    let v = try JSONValue.parse(Data(#" {"zeta":1,"alpha":[1.50,-2e3,"é\n😀"],"m_id":{"b":true,"a":null}} "#.utf8))
    guard case .object(let members) = v else { Issue.record("expected object"); return }
    #expect(members.map(\.key) == ["zeta", "alpha", "m_id"])
    #expect(v["alpha"] == .array([.number("1.50"), .number("-2e3"), .string("é\n😀")]))
    #expect(v["m_id"]?["a"] == .null)
    #expect(throws: (any Error).self) { try JSONValue.parse(Data(#"{"a":1,}"#.utf8)) }
    #expect(throws: (any Error).self) { try JSONValue.parse(Data(#"[1] 2"#.utf8)) }
}
