// The stage ↔ stage-helper wire, crates/proto/tests/fixtures/helper: every request the
// helper reads must decode here (and not once a field is added or dropped), and every
// reply it writes must encode to the fixture's JSON.

import Foundation
import Testing
@testable import NotesCore

private let fixtures = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent()  // NotesCoreTests
    .deletingLastPathComponent()  // Tests
    .deletingLastPathComponent()  // Notes
    .deletingLastPathComponent()  // apps
    .deletingLastPathComponent()  // repo root
    .appendingPathComponent("crates/proto/tests/fixtures/helper")

private func helperGolden(_ file: String) throws -> Data {
    try Data(contentsOf: fixtures.appendingPathComponent(file))
}

private func helperJSON(_ data: Data) throws -> NSDictionary {
    try #require(try JSONSerialization.jsonObject(with: data) as? NSDictionary)
}

private let requests: [String: HelperRequest] = [
    "warm": .warm(id: 1),
    "mermaid": .mermaid(id: 2, source: "flowchart TD\n  a --> b", maxWidth: 400, fontSize: 14),
    "image": .image(id: 3, path: "/tmp/photo.heic"),
    "html-open": .htmlOpen(id: 4, path: "/tmp/page.html", width: 330, maxHeight: 180, scale: 2, fps: 60),
    "html-update": .htmlUpdate(id: 4, path: "/tmp/page.html"),
    "html-ack": .htmlAck(id: 4, frame: 7),
    "html-pause": .htmlPause(id: 4),
    "html-resume": .htmlResume(id: 4),
    "html-close": .htmlClose(id: 4),
    "html-resize": .htmlResize(id: 4, width: 400, maxHeight: 200, scale: 2),
]

private let replies: [String: HelperReply] = [
    "done": .done(id: 1),
    "mermaid": .mermaid(id: 2, svg: "<svg xmlns=\"http://www.w3.org/2000/svg\"/>", width: 212.5, height: 96),
    "image": .image(id: 3, shm: "/stg-123-1", w: 640, h: 480),
    "failed": .failed(id: 3, error: "can't decode /tmp/photo.heic"),
    "failed-no-id": .failed(id: nil, error: "bad request: not a JSON object"),
    "frame": .frame(id: 4, frame: 7, shm: "/stg-123-2", w: 660, h: 360),
    "patched": .patched(id: 4, patched: true),
]

private func fixtureNames(_ suffix: String) throws -> Set<String> {
    Set(try FileManager.default.contentsOfDirectory(atPath: fixtures.path).filter { $0.hasSuffix(suffix) }
        .map { String($0.dropLast(suffix.count)) })
}

@Test func helperRequestsDecodeAndReencodeToTheFixtures() throws {
    #expect(try fixtureNames(".request.json") == Set(requests.keys), "a fixture without a sample here (or the reverse)")
    for (name, expected) in requests {
        let line = try helperGolden("\(name).request.json")
        #expect(try HelperRequest.parse(line) == expected, "\(name)")
        #expect(try helperJSON(JSONEncoder().encode(expected)) == helperJSON(line), "\(name)")
    }
}

@Test func helperRepliesEncodeToTheFixtures() throws {
    #expect(try fixtureNames(".reply.json") == Set(replies.keys), "a fixture without a sample here (or the reverse)")
    for (name, reply) in replies {
        #expect(try helperJSON(JSONEncoder().encode(reply)) == helperJSON(helperGolden("\(name).reply.json")), "\(name)")
    }
}

@Test func helperRequestsAreStrict() throws {
    for name in requests.keys {
        var line = try #require(try helperJSON(helperGolden("\(name).request.json")).mutableCopy() as? NSMutableDictionary)
        line["bogus"] = 1
        let extra = try JSONSerialization.data(withJSONObject: line)
        #expect(throws: HelperRequestError.self, "\(name) accepted an unknown field") { try HelperRequest.parse(extra) }
        for key in line.allKeys as! [String] where key != "op" && key != "bogus" {
            let dropped = try #require(line.mutableCopy() as? NSMutableDictionary)
            dropped.removeObject(forKey: key)
            dropped.removeObject(forKey: "bogus")
            #expect(throws: HelperRequestError.self, "\(name) accepted no \(key)") {
                try HelperRequest.parse(JSONSerialization.data(withJSONObject: dropped))
            }
        }
    }
    #expect(throws: HelperRequestError.self) { try HelperRequest.parse(Data(#"{"op":"nope","id":1}"#.utf8)) }
    #expect(throws: HelperRequestError.self) { try HelperRequest.parse(Data("[1]".utf8)) }
}

@Test func aFailedRequestIsAnsweredUnderItsId() {
    do {
        _ = try HelperRequest.parse(Data(#"{"op":"html-ack","id":9}"#.utf8))
        Issue.record("accepted an ack without a frame")
    } catch let error as HelperRequestError {
        #expect(error.id == 9 && error.message == "missing \"frame\"", "\(error)")
    } catch {
        Issue.record("\(error)")
    }
}
