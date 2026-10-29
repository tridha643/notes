// stage-helper end to end: spawn the built binary and speak its JSON-lines protocol.

import AppKit
import Foundation
import ImageIO
import StageShm
import Testing
import UniformTypeIdentifiers

private final class Marker {}

/// The helper process for the life of the value, with replies read on a background thread.
private final class Helper: @unchecked Sendable {
    let process = Process()
    let stdin = Pipe(), stdout = Pipe()
    private let lock = NSCondition()
    private var lines: [[String: Any]] = []

    init() throws {
        let products = Bundle(for: Marker.self).bundleURL.deletingLastPathComponent()
        process.executableURL = products.appendingPathComponent("stage-helper")
        process.standardInput = stdin
        process.standardOutput = stdout
        process.standardError = FileHandle.nullDevice
        try process.run()
        Thread.detachNewThread { [self] in
            var buffer = Data()
            while true {
                let chunk = stdout.fileHandleForReading.availableData
                if chunk.isEmpty { break }
                buffer.append(chunk)
                while let nl = buffer.firstIndex(of: 0x0A) {
                    let line = buffer[buffer.startIndex..<nl]
                    buffer = Data(buffer[(nl + 1)...])
                    if let obj = (try? JSONSerialization.jsonObject(with: line)) as? [String: Any] {
                        lock.lock(); lines.append(obj); lock.signal(); lock.unlock()
                    }
                }
            }
        }
    }

    deinit {
        try? stdin.fileHandleForWriting.close()
        process.waitUntilExit()
    }

    func send(_ obj: [String: Any]) throws {
        var data = try JSONSerialization.data(withJSONObject: obj)
        data.append(0x0A)
        stdin.fileHandleForWriting.write(data)
    }

    /// The next line, or nil after `timeout` seconds.
    func next(_ timeout: Double = 10) -> [String: Any]? {
        let deadline = Date(timeIntervalSinceNow: timeout)
        lock.lock()
        defer { lock.unlock() }
        while lines.isEmpty { if !lock.wait(until: deadline) { return nil } }
        return lines.removeFirst()
    }
}

/// Read a shm object's bytes and unlink it, as stage (or kitty) would.
private func takeShm(_ name: String, _ count: Int) throws -> [UInt8] {
    let fd = stage_shm_open(name, O_RDONLY, 0)
    #expect(fd >= 0)
    defer { close(fd); shm_unlink(name) }
    let map = try #require(mmap(nil, count, PROT_READ, MAP_SHARED, fd, 0).flatMap { $0 == MAP_FAILED ? nil : $0 })
    defer { munmap(map, count) }
    return [UInt8](UnsafeRawBufferPointer(start: map, count: count))
}

private func page(_ html: String) throws -> String {
    let path = NSTemporaryDirectory() + "stage-helper-\(UUID().uuidString).html"
    try html.write(toFile: path, atomically: true, encoding: .utf8)
    return path
}

@Suite(.serialized) struct StageHelperTests {
    @Test func mermaidResolvesThePalette() throws {
        let h = try Helper()
        try h.send(["op": "mermaid", "id": 1, "source": "flowchart TD\n  A[agent] --> B{notesd}"])
        let r = try #require(h.next())
        #expect(r["ok"] as? Bool == true)
        let svg = try #require(r["svg"] as? String)
        #expect(svg.hasPrefix("<svg") && !svg.contains("var("))
        #expect(svg.contains("#89b4fa") && svg.contains("#cdd6f4"))
        #expect((r["width"] as? Double ?? 0) > 0 && (r["height"] as? Double ?? 0) > 0)
    }

    @Test func imageRoundTripsThroughShm() throws {
        // 2×1: opaque red, half-transparent blue.
        let px: [UInt8] = [255, 0, 0, 255, 0, 0, 255, 128]
        let provider = CGDataProvider(data: Data(px) as CFData)!
        let image = CGImage(width: 2, height: 1, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: 8,
                            space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.last.rawValue),
                            provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent)!
        let path = NSTemporaryDirectory() + "stage-helper-\(UUID().uuidString).png"
        let dest = CGImageDestinationCreateWithURL(URL(fileURLWithPath: path) as CFURL, UTType.png.identifier as CFString, 1, nil)!
        CGImageDestinationAddImage(dest, image, nil)
        #expect(CGImageDestinationFinalize(dest))

        let h = try Helper()
        try h.send(["op": "image", "id": 2, "path": path])
        let r = try #require(h.next())
        #expect(r["ok"] as? Bool == true && r["w"] as? Int == 2 && r["h"] as? Int == 1)
        let got = try takeShm(try #require(r["shm"] as? String), 8)
        #expect(Array(got[0..<4]) == [255, 0, 0, 255])
        #expect(got[6] >= 253 && got[7] == 128, "straight alpha, not premultiplied: \(got)")
    }

    @Test func staticPageSendsOneFrameThenNothing() throws {
        let path = try page("<body style='margin:0;background:#1e1e2e'><div style='height:300px'>still</div></body>")
        let h = try Helper()
        try h.send(["op": "html-open", "id": 7, "path": path, "width": 400, "max_height": 1000, "scale": 2, "fps": 60])
        let first = try #require(h.next())
        #expect(first["frame"] as? Int == 1 && first["w"] as? Int == 800 && first["h"] as? Int == 600, "\(first)")
        _ = try takeShm(try #require(first["shm"] as? String), 800 * 600 * 4)
        try h.send(["op": "html-ack", "id": 7, "frame": 1])
        #expect(h.next(0.5) == nil)
    }

    @Test func animatingPageStreamsFramesWhileAcked() throws {
        let path = try page("""
        <style>body{margin:0;background:#1e1e2e}.b{width:40px;height:40px;background:#a6e3a1;animation:m .5s linear infinite alternate}
        @keyframes m{from{transform:translateX(0)}to{transform:translateX(300px)}}</style><div style='height:100px'><div class=b></div></div>
        """)
        let h = try Helper()
        try h.send(["op": "html-open", "id": 8, "path": path, "width": 400, "max_height": 100, "scale": 1, "fps": 60])
        var frames = 0
        let start = Date()
        while Date().timeIntervalSince(start) < 1.5, let r = h.next(1) {
            guard let k = r["frame"] as? Int, let shm = r["shm"] as? String else { continue }
            _ = try takeShm(shm, 400 * 100 * 4)
            try h.send(["op": "html-ack", "id": 8, "frame": k])
            frames += 1
        }
        let fps = Double(frames) / Date().timeIntervalSince(start)
        #expect(fps >= 20, "\(fps) fps")
        try h.send(["op": "html-close", "id": 8])
    }

    @Test func animatedPageResizeNeverEmitsTheMeasurementViewport() throws {
        let path = try page("""
        <style>body{margin:0}.b{height:100px;background:#89b4fa;animation:p .2s linear infinite alternate}
        @keyframes p{to{opacity:.5}}</style><div class=b></div>
        """)
        let h = try Helper()
        try h.send(["op": "html-open", "id": 11, "path": path, "width": 400, "max_height": 200, "scale": 1, "fps": 60])
        let first = try #require(h.next())
        shm_unlink(try #require(first["shm"] as? String))
        try h.send(["op": "html-ack", "id": 11, "frame": try #require(first["frame"] as? Int)])
        for n in 0..<12 {
            let width = n.isMultiple(of: 2) ? 600 : 400
            try h.send(["op": "html-resize", "id": 11, "width": width, "max_height": 200, "scale": 1])
            var gotResizedFrame = false
            let deadline = Date(timeIntervalSinceNow: 2)
            while Date() < deadline, let frame = h.next(0.5) {
                guard let shm = frame["shm"] as? String, let number = frame["frame"] as? Int else { continue }
                shm_unlink(shm)
                #expect((frame["h"] as? Int ?? 0) >= 100, "measurement viewport escaped: \(frame)")
                try h.send(["op": "html-ack", "id": 11, "frame": number])
                if frame["w"] as? Int == width { gotResizedFrame = true; break }
            }
            #expect(gotResizedFrame, "resize to \(width) never produced a frame")
        }
        try h.send(["op": "html-close", "id": 11])
    }

    @Test func widePagesAreZoomedOutToFitTheCard() throws {
        let path = try page("<body style='margin:0'><div style='width:1000px;height:500px;background:#89b4fa'></div></body>")
        let h = try Helper()
        try h.send(["op": "html-open", "id": 10, "path": path, "width": 400, "max_height": 1000, "scale": 1, "fps": 60])
        let first = try #require(h.next())
        shm_unlink(try #require(first["shm"] as? String))
        // 1000 px wide into 400: zoom 0.4, so 500 px tall shows as 200.
        #expect(first["w"] as? Int == 400 && first["h"] as? Int == 200, "\(first)")
        try h.send(["op": "html-close", "id": 10])
    }

    @Test func updatesPatchStylesAndTextInPlaceElseReload() throws {
        let v1 = "<style>p{color:#fff}</style><body style='margin:0'><p id=t>one</p><script>let x = 1</script></body>"
        let path = try page(v1)
        let h = try Helper()
        try h.send(["op": "html-open", "id": 9, "path": path, "width": 200, "max_height": 100, "scale": 1, "fps": 60])
        let first = try #require(h.next())
        shm_unlink(try #require(first["shm"] as? String))
        try h.send(["op": "html-ack", "id": 9, "frame": 1])
        func update(_ html: String) throws -> Bool? {
            try h.send(["op": "html-update", "id": 9, "path": try page(html)])
            while let r = h.next(2) {
                if let shm = r["shm"] as? String { shm_unlink(shm); continue }
                if let patched = r["patched"] as? Bool { return patched }
            }
            return nil
        }
        #expect(try update(v1.replacingOccurrences(of: "one", with: "two").replacingOccurrences(of: "#fff", with: "#f00")) == true)
        #expect(try update(v1.replacingOccurrences(of: "let x = 1", with: "let x = 2")) == false, "a script change reloads")
        try h.send(["op": "html-close", "id": 9])
    }

    @Test func unknownOpsAndBadRequestsError() throws {
        let h = try Helper()
        try h.send(["op": "nope", "id": 3])
        let r = try #require(h.next())
        #expect(r["ok"] as? Bool == false && r["id"] as? Int == 3)
        try h.send(["op": "html-ack", "id": 99, "frame": 1])
        #expect(try #require(h.next())["ok"] as? Bool == false)
    }
}
