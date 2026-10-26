// stage-helper: the pixels stage (the Rust TUI) can't make itself. JSON lines on
// stdin, one reply or frame per line on stdout, logs on stderr.
//
//   mermaid    source → pele SVG with stage's palette resolved (no var() left, so resvg draws it)
//   image      a file → RGBA8 in a fresh POSIX shm object
//   html-*     a page in an offscreen WKWebView → frames in shm, only while something changes
//
// Every pixel buffer is a new shm object that we never unlink: the consumer (kitty via
// `t=s`, or stage when it drops a frame) does. Requests run on the main thread except
// pele, which has its own queue.

import Accelerate
import AppKit
import ImageIO
import NotesCore
import StageShm

/// stage's palette (crates/stage/src/ui.rs). Fonts are ones resvg's fontdb can load too.
let peleTheme: PeleTheme = [
    "--pele-bg": "#1e1e2e", "--pele-fg": "#cdd6f4", "--pele-muted": "#a6adc8", "--pele-line": "#7f849c",
    "--pele-surface": "#313244", "--pele-surface-alt": "#45475a", "--pele-border": "#89b4fa",
    "--pele-accent": "#f5c2e7", "--pele-font": "Helvetica Neue", "--pele-font-mono": "Menlo",
]

@MainActor
func send(_ message: [String: Any]) {
    guard var data = try? JSONSerialization.data(withJSONObject: message, options: [.withoutEscapingSlashes]) else { return }
    data.append(0x0A)
    FileHandle.standardOutput.write(data)
}

@MainActor
func fail(_ id: Any, _ error: String) {
    send(["id": id, "ok": false, "error": error])
}

// MARK: shared memory

private var shmCounter = 0

/// Copy `pixels` (RGBA8, rows top-down) into a fresh shm object and return its name.
@MainActor
func writeShm(_ pixels: [UInt8]) throws -> String {
    shmCounter += 1
    let name = "/stg-\(getpid())-\(shmCounter)"  // macOS caps shm names at 31 chars
    let fd = stage_shm_open(name, O_CREAT | O_EXCL | O_RDWR, 0o600)
    guard fd >= 0 else { throw POSIXError(.init(rawValue: errno) ?? .EIO) }
    defer { close(fd) }
    guard ftruncate(fd, off_t(pixels.count)) == 0,
          let map = mmap(nil, pixels.count, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0), map != MAP_FAILED
    else {
        shm_unlink(name)
        throw POSIXError(.init(rawValue: errno) ?? .EIO)
    }
    pixels.withUnsafeBytes { map.copyMemory(from: $0.baseAddress!, byteCount: pixels.count) }
    munmap(map, pixels.count)
    return name
}

/// Draw `image` scaled to `w`×`h` and return straight-alpha sRGB RGBA8 rows, top-down.
func rgba(_ image: CGImage, _ w: Int, _ h: Int) -> [UInt8] {
    var px = [UInt8](repeating: 0, count: w * h * 4)
    px.withUnsafeMutableBytes { buf in
        let ctx = CGContext(data: buf.baseAddress, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4,
                            space: CGColorSpace(name: CGColorSpace.sRGB)!,
                            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
        ctx.interpolationQuality = .high
        ctx.draw(image, in: CGRect(x: 0, y: 0, width: w, height: h))
    }
    // CoreGraphics only draws premultiplied; kitty's f=32 wants straight alpha.
    px.withUnsafeMutableBytes { buf in
        var v = vImage_Buffer(data: buf.baseAddress, height: vImagePixelCount(h), width: vImagePixelCount(w), rowBytes: w * 4)
        _ = vImageUnpremultiplyData_RGBA8888(&v, &v, vImage_Flags(kvImageNoFlags))
    }
    return px
}

// MARK: ops

/// Renders still running on pele's queue; EOF waits for them before exiting.
let renders = DispatchGroup()

@MainActor
func mermaid(_ req: [String: Any], id: Any) {
    guard let source = req["source"] as? String else { return fail(id, "mermaid needs a source") }
    let maxWidth = (req["max_width"] as? NSNumber)?.doubleValue
    let fontSize = (req["font_size"] as? NSNumber)?.doubleValue ?? 14
    nonisolated(unsafe) let id = id
    let theme = peleTheme
    // pele blocks on its own queue, so render off the main thread.
    renders.enter()
    DispatchQueue.global(qos: .userInitiated).async {
        defer { renders.leave() }
        do {
            let d = try Pele.shared.render(source, maxWidth: maxWidth, fontSize: fontSize)
            let svg = resolvePeleSVG(d.svg, theme: theme).replacingOccurrences(of: Pele.fontFamily, with: "Helvetica Neue")
            DispatchQueue.main.async { MainActor.assumeIsolated { send(["id": id, "ok": true, "svg": svg, "width": d.width, "height": d.height]) } }
        } catch {
            let message = "\(error)"
            DispatchQueue.main.async { MainActor.assumeIsolated { fail(id, message) } }
        }
    }
}

@MainActor
func image(_ req: [String: Any], id: Any) {
    guard let path = req["path"] as? String else { return fail(id, "image needs a path") }
    guard let source = CGImageSourceCreateWithURL(URL(fileURLWithPath: path) as CFURL, nil),
          let image = CGImageSourceCreateImageAtIndex(source, 0, nil)
    else { return fail(id, "can't decode \(path)") }
    do {
        let shm = try writeShm(rgba(image, image.width, image.height))
        send(["id": id, "ok": true, "shm": shm, "w": image.width, "h": image.height])
    } catch {
        fail(id, "shm: \(error)")
    }
}

var pages: [Int: Page] = [:]

@MainActor
func handle(_ line: String) {
    guard let data = line.data(using: .utf8),
          let req = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
    else { return fail(NSNull(), "bad request: not a JSON object") }
    let id: Any = req["id"] ?? NSNull()
    let op = req["op"] as? String ?? ""
    let pageId = (req["id"] as? NSNumber)?.intValue ?? -1
    let num = { (key: String) in (req[key] as? NSNumber)?.doubleValue }
    switch op {
    case "mermaid": mermaid(req, id: id)
    case "image": image(req, id: id)
    case "html-open":
        guard let path = req["path"] as? String, let width = num("width"), width > 0 else {
            return fail(id, "html-open needs a path and a width")
        }
        pages[pageId]?.close()
        pages[pageId] = Page(id: pageId, path: path, width: width, maxHeight: num("max_height") ?? 2000,
                             scale: num("scale") ?? 2, fps: num("fps") ?? 60)
    case "html-ack", "html-pause", "html-resume", "html-resize", "html-close":
        guard let page = pages[pageId] else { return fail(id, "no page \(pageId)") }
        switch op {
        case "html-ack": page.ack((req["frame"] as? NSNumber)?.intValue ?? -1)
        case "html-pause": page.pause()
        case "html-resume": page.resume()
        case "html-resize": page.resize(width: num("width"), maxHeight: num("max_height"), scale: num("scale"))
        default:
            page.close()
            pages[pageId] = nil
        }
    default: fail(id, "unknown op \"\(op)\"")
    }
}

// No Dock icon, no menu bar, never takes focus.
let app = NSApplication.shared
app.setActivationPolicy(.prohibited)
Thread.detachNewThread {
    while let line = readLine() {
        if line.isEmpty { continue }
        DispatchQueue.main.async { MainActor.assumeIsolated { handle(line) } }
    }
    // The main queue is FIFO: once this runs, every queued request has started (and
    // entered `renders`); exit after their replies.
    DispatchQueue.main.async { renders.notify(queue: .main) { exit(0) } }
}
app.run()
