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
        try h.send(["op": "mermaid", "id": 1, "source": "flowchart TD\n  A[agent] --> B{notesd}", "max_width": 400, "font_size": 14])
        let r = try #require(h.next())
        #expect(r["ok"] as? Bool == true)
        let svg = try #require(r["svg"] as? String)
        #expect(svg.hasPrefix("<svg") && !svg.contains("var("))
        #expect(svg.contains("#89b4fa") && svg.contains("#cdd6f4"))
        #expect((r["width"] as? Double ?? 0) > 0 && (r["height"] as? Double ?? 0) > 0)
    }

    @Test func mermaidDoesNotWaitBehindWebKitStartup() throws {
        // A fresh helper spends its first ~100 ms building a spare web view on the main
        // thread; `warm` is answered there, so mermaid must not queue behind it.
        let h = try Helper()
        try h.send(["op": "mermaid", "id": 1, "source": "flowchart TD\n  A --> B", "max_width": 400, "font_size": 14])
        try h.send(["op": "warm", "id": 2])
        let first = try #require(h.next())
        #expect(first["id"] as? Int == 1 && first["ok"] as? Bool == true, "\(first)")
        #expect(try #require(h.next())["id"] as? Int == 2)
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

    @Test func recycledPagesHaveFreshDocumentsAndCorrectFirstPixels() throws {
        let h = try Helper()
        let path = try page("")
        defer { try? FileManager.default.removeItem(atPath: path) }
        for n in 0..<16 {
            let width = 160 + (n % 3) * 40
            let height = 60 + (n % 4) * 20
            try """
            <body style='margin:0'><div style='height:\(height)px'></div>
            <script>
            document.body.style.background = window.oldPage || sessionStorage.getItem('oldPage') ? 'red' : 'lime';
            window.oldPage = true; sessionStorage.setItem('oldPage', 'yes');
            </script></body>
            """.write(toFile: path, atomically: true, encoding: .utf8)
            let id = 100 + n
            try h.send(["op": "html-open", "id": id, "path": path, "width": width, "max_height": 300, "scale": 1, "fps": 60])
            let first = try #require(h.next())
            #expect(first["id"] as? Int == id && first["frame"] as? Int == 1, "\(first)")
            #expect(first["w"] as? Int == width && first["h"] as? Int == height, "a blank-navigation callback must not capture the wrong document: \(first)")
            let w = try #require(first["w"] as? Int), hgt = try #require(first["h"] as? Int)
            let px = try takeShm(try #require(first["shm"] as? String), w * hgt * 4)
            #expect(Array(px.prefix(4)) == [0, 255, 0, 255], "recycling must reset globals AND session storage and capture the new page")
            try h.send(["op": "html-close", "id": id])
        }
    }

    @Test func recyclingDoesNotClearALivePagesStorage() throws {
        let path = try page("""
        <body style='margin:0;background:lime'><div style='height:80px'></div><script>
        sessionStorage.setItem('live', 'yes');
        addEventListener('resize', () => document.body.style.background =
          sessionStorage.getItem('live') === 'yes' ? 'lime' : 'red');
        </script></body>
        """)
        defer { try? FileManager.default.removeItem(atPath: path) }
        let h = try Helper()
        for id in [1, 2] {
            try h.send(["op": "html-open", "id": id, "path": path, "width": 160, "max_height": 100, "scale": 1, "fps": 60])
            let first = try #require(h.next())
            #expect(first["id"] as? Int == id)
            shm_unlink(try #require(first["shm"] as? String))
        }
        try h.send(["op": "html-close", "id": 2])
        Thread.sleep(forTimeInterval: 0.15)
        try h.send(["op": "html-ack", "id": 1, "frame": 1])
        try h.send(["op": "html-resize", "id": 1, "width": 200, "max_height": 100, "scale": 1])
        let next = try #require(h.next())
        #expect(next["id"] as? Int == 1 && next["w"] as? Int == 200)
        let w = try #require(next["w"] as? Int), height = try #require(next["h"] as? Int)
        let px = try takeShm(try #require(next["shm"] as? String), w * height * 4)
        #expect(Array(px.prefix(4)) == [0, 255, 0, 255], "recycling a different view must preserve the live page's storage")
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

    @Test func anUpdateCanRecoverAfterNavigationFailure() throws {
        let h = try Helper()
        let missing = NSTemporaryDirectory() + "missing-stage-page-\(UUID().uuidString).html"
        try h.send(["op": "html-open", "id": 23, "path": missing, "width": 200, "max_height": 200, "scale": 1, "fps": 60])
        let failure = try #require(h.next())
        #expect(failure["ok"] as? Bool == false)
        let path = try page("<body style='margin:0;background:lime'><div style='height:80px'></div></body>")
        defer { try? FileManager.default.removeItem(atPath: path) }
        try h.send(["op": "html-update", "id": 23, "path": path])
        let patched = try #require(h.next())
        #expect(patched["patched"] as? Bool == false)
        let frame = try #require(h.next())
        #expect(frame["w"] as? Int == 200 && frame["h"] as? Int == 80)
        shm_unlink(try #require(frame["shm"] as? String))
        try h.send(["op": "html-close", "id": 23])
    }

    @Test func pauseResumePreservesUnackedFrameBackpressure() throws {
        let path = try page("<body style='margin:0;background:lime'><div style='height:80px'></div></body>")
        defer { try? FileManager.default.removeItem(atPath: path) }
        let h = try Helper()
        try h.send(["op": "html-open", "id": 21, "path": path, "width": 200, "max_height": 200, "scale": 1, "fps": 60])
        let first = try #require(h.next())
        shm_unlink(try #require(first["shm"] as? String))
        for _ in 0..<10 {
            try h.send(["op": "html-pause", "id": 21])
            try h.send(["op": "html-resume", "id": 21])
        }
        #expect(h.next(0.2) == nil, "resume must not bypass the outstanding frame's ack")
        try h.send(["op": "html-ack", "id": 21, "frame": try #require(first["frame"] as? Int)])
        let next = try #require(h.next())
        #expect(next["frame"] as? Int == 2)
        shm_unlink(try #require(next["shm"] as? String))
        try h.send(["op": "html-close", "id": 21])
    }

    @Test func burstUpdatesEndAtTheLatestDocumentAndPixels() throws {
        let h = try Helper()
        let html = { (n: Int) in
            let color = n == 20 ? "#12ab34" : "#89b4fa"
            return "<body style='margin:0'><div id='box' style='height:\(60 + n * 3)px;background:\(color)'></div><script>window.revision = \(n / 3)</script></body>"
        }
        var paths: [String] = []
        defer { for path in paths { try? FileManager.default.removeItem(atPath: path) } }
        paths.append(try page(html(0)))
        try h.send(["op": "html-open", "id": 22, "path": paths[0], "width": 200, "max_height": 300, "scale": 1, "fps": 60])
        let first = try #require(h.next())
        shm_unlink(try #require(first["shm"] as? String))
        try h.send(["op": "html-ack", "id": 22, "frame": try #require(first["frame"] as? Int)])
        for n in 1...20 {
            paths.append(try page(html(n)))
            try h.send(["op": "html-update", "id": 22, "path": paths.last!])
        }
        var replies = 0, gotLatest = false
        let deadline = Date(timeIntervalSinceNow: 5)
        while Date() < deadline, let reply = h.next(1) {
            if reply["patched"] != nil { replies += 1 }
            if let shm = reply["shm"] as? String {
                let w = try #require(reply["w"] as? Int), height = try #require(reply["h"] as? Int)
                let px = try takeShm(shm, w * height * 4)
                if replies == 20 {
                    #expect((w, height) == (200, 120))
                    #expect(Array(px.prefix(4)) == [0x12, 0xab, 0x34, 255], "the final revision must win across interleaved patches and reloads")
                    gotLatest = true
                    break
                }
                try h.send(["op": "html-ack", "id": 22, "frame": try #require(reply["frame"] as? Int)])
            }
        }
        #expect(replies == 20 && gotLatest)
        try h.send(["op": "html-close", "id": 22])
    }

    @Test func unknownOpsAndBadRequestsError() throws {
        let h = try Helper()
        try h.send(["op": "nope", "id": 3])
        let r = try #require(h.next())
        #expect(r["ok"] as? Bool == false && r["id"] as? Int == 3)
        try h.send(["op": "html-ack", "id": 99, "frame": 1])
        #expect(try #require(h.next())["ok"] as? Bool == false)
        // Strict: a missing field or an unknown one is refused, under the request's id.
        try h.send(["op": "mermaid", "id": 4, "source": "flowchart TD\n  A --> B"])
        let missing = try #require(h.next())
        #expect(missing["ok"] as? Bool == false && missing["id"] as? Int == 4, "\(missing)")
        try h.send(["op": "warm", "id": 5, "extra": 1])
        #expect(try #require(h.next())["ok"] as? Bool == false)
    }
}
