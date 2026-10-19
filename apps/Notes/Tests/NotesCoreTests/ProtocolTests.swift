// Wire shapes from crates/proto/src/lib.rs tests and docs/cli-spec.md.

import Foundation
import Testing
@testable import NotesCore

func object(_ data: Data) throws -> NSObject {
    try JSONSerialization.jsonObject(with: data) as! NSObject
}

func object(_ json: String) throws -> NSObject {
    try object(Data(json.utf8))
}

func decode<T: Decodable>(_ type: T.Type, _ json: String) throws -> T {
    try Wire.decoder.decode(type, from: Data(json.utf8))
}

@Test func requestLinesMatchTheWire() throws {
    let write = Request(id: 7, verb: "write", args: WriteArgs(note: "work/plan", body: "hi", ifVersion: 42))
    let line = try Wire.line(write)
    #expect(line.last == 0x0A && !line.dropLast().contains(0x0A))
    #expect(try object(line) == object(#"{"v":1,"id":7,"verb":"write","args":{"note":"work/plan","body":"hi","if_version":42,"force":false}}"#))

    // Optional args are omitted, like serde's skip_serializing_if.
    let list = Request(id: 2, verb: "list", args: ListArgs(recursive: true))
    #expect(try object(Wire.line(list)) == object(#"{"v":1,"id":2,"verb":"list","args":{"recursive":true}}"#))
    let watch = Request(id: 3, verb: "watch", args: WatchArgs())
    #expect(try object(Wire.line(watch)) == object(#"{"v":1,"id":3,"verb":"watch","args":{}}"#))
    #expect(try object(Wire.line(Request(id: 4, verb: "read", args: NoteArgs(note: "@last")))) == object(#"{"v":1,"id":4,"verb":"read","args":{"note":"@last"}}"#))
    #expect(try object(Wire.line(Request(id: 5, verb: "new", args: NewArgs(path: "work/idea")))) == object(#"{"v":1,"id":5,"verb":"new","args":{"path":"work/idea","body":""}}"#))
    #expect(try object(Wire.line(Request(id: 6, verb: "delete", args: DeleteArgs(note: "a")))) == object(#"{"v":1,"id":6,"verb":"delete","args":{"note":"a","purge":false}}"#))

    let back = try Wire.decoder.decode(Request<WriteArgs>.self, from: line)
    #expect(back.args == write.args && back.id == 7 && back.v == 1 && back.verb == "write")
}

@Test func responsesDecodeAndRoundTrip() throws {
    let ok = try decode(Response<WriteResult>.self, #"{"id":7,"ok":true,"result":{"path":"work/plan.md","version":43}}"#)
    #expect(ok == .ok(id: 7, result: WriteResult(path: "work/plan.md", version: 43)))

    let err = try decode(Response<WriteResult>.self, #"{"id":7,"ok":false,"error":{"code":"ambiguous","message":"\"foo\" matches 3 times (lines 4, 9, 30)","hint":"add surrounding text or pass --all"}}"#)
    #expect(err == .err(id: 7, error: ErrorBody(code: .ambiguous, message: "\"foo\" matches 3 times (lines 4, 9, 30)", hint: "add surrounding text or pass --all")))

    let event = try decode(Response<WriteResult>.self, #"{"id":7,"event":{"seq":12,"kind":"note-updated","path":"work/plan.md","version":44}}"#)
    #expect(event == .event(id: 7, event: Event(seq: 12, kind: "note-updated", path: "work/plan.md", version: 44)))

    for response in [ok, err, event] {
        #expect(try Wire.decoder.decode(Response<WriteResult>.self, from: Wire.line(response)) == response)
    }
    // Events never need a typed result.
    #expect(try decode(Response<Ignored>.self, #"{"id":1,"event":{"seq":1,"kind":"note-created"}}"#).id == 1)
}

@Test func versionConflictCarriesCurrentVersion() throws {
    let r = try decode(Response<WriteResult>.self, #"{"id":7,"ok":false,"error":{"code":"version_conflict","message":"x","details":{"current_version":9}}}"#)
    guard case .err(_, let error) = r else { Issue.record("expected error"); return }
    #expect(error.code == .versionConflict)
    #expect(error.currentVersion == 9)
    #expect(try object(Wire.encoder.encode(error)) == object(#"{"code":"version_conflict","message":"x","details":{"current_version":9}}"#))
    // A code this client doesn't know still decodes.
    #expect(try decode(ErrorBody.self, #"{"code":"brand_new","message":"m"}"#).code == .other)
}

@Test func listEntriesAndNotes() throws {
    let entries = try decode([ListEntry].self, #"[{"folder":"work"},{"path":"work/plan.md","title":"plan","version":3,"updated_at":1760000000}]"#)
    #expect(entries == [.folder("work"), .note(NoteSummary(path: "work/plan.md", title: "plan", version: 3, updatedAt: 1760000000))])
    #expect(try object(Wire.encoder.encode(entries)) == object(#"[{"folder":"work"},{"path":"work/plan.md","title":"plan","version":3,"updated_at":1760000000}]"#))

    let note = try decode(Note.self, ##"{"path":"a.md","title":"a","version":2,"body":"# a\n","line_count":1,"created_at":1,"updated_at":2}"##)
    #expect(note.lineCount == 1 && note.body == "# a\n" && note.updatedAt == 2)
    #expect(try decode(Note.self, String(decoding: Wire.encoder.encode(note), as: UTF8.self)) == note)
}

@Test func socketPathHonoursEnvironment() {
    #expect(defaultSocketPath(environment: ["NOTES_SOCK": "/tmp/x.sock"]) == "/tmp/x.sock")
    #expect(defaultSocketPath(environment: [:]).hasSuffix("/Library/Application Support/Notes/notesd.sock"))
}
