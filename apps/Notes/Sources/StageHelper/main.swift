// stage-helper: the pixels stage (the Rust TUI) can't make itself. JSON lines on
// stdin, one reply or frame per line on stdout, logs on stderr.
//
//   mermaid    source → pele SVG with stage's palette resolved (no var() left, so resvg draws it)
//   image      a file → RGBA8 in a fresh POSIX shm object
//   html-*     a page in an offscreen WKWebView → frames in shm, only while something changes;
//              html-update patches a new revision in place (see Page.update)
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

/// One stage talking to us: stdout in pipe mode, a socket connection with `--serve`.
/// Its pages live here, so a stage that quits takes its pages with it.
final class Client: @unchecked Sendable {
    private let out: Int32
    /// Guards writes so a reply sent off the main thread never interleaves with a frame.
    private let lock = NSLock()
    /// Main thread only.
    var pages: [Int: Page] = [:]

    init(out: Int32) { self.out = out }

    /// One reply line. Callable from any thread: replies that don't touch AppKit
    /// (mermaid) go out from their own queue instead of waiting on the main thread.
    func send(_ message: [String: Any]) {
        guard var data = try? JSONSerialization.data(withJSONObject: message, options: [.withoutEscapingSlashes]) else { return }
        data.append(0x0A)
        lock.lock()
        defer { lock.unlock() }
        data.withUnsafeBytes { buf in
            var sent = 0
            while sent < buf.count {
                let n = write(out, buf.baseAddress! + sent, buf.count - sent)
                if n <= 0 { return }  // the client went away; its reader notices
                sent += n
            }
        }
    }

    func fail(_ id: Any, _ error: String) {
        send(["id": id, "ok": false, "error": error])
    }
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
func mermaid(_ req: [String: Any], id: Any, client: Client) {
    guard let source = req["source"] as? String else { return client.fail(id, "mermaid needs a source") }
    let maxWidth = (req["max_width"] as? NSNumber)?.doubleValue
    let fontSize = (req["font_size"] as? NSNumber)?.doubleValue ?? 14
    nonisolated(unsafe) let id = id
    let theme = peleTheme
    // pele blocks on its own queue, so render off the main thread.
    renders.enter()
    DispatchQueue.global(qos: .userInitiated).async {
        defer { renders.leave() }
        do {
            let t0 = DispatchTime.now().uptimeNanoseconds
            let d = try Pele.shared.render(source, maxWidth: maxWidth, fontSize: fontSize)
            traceMark("pele render", since: t0)
            let svg = resolvePeleSVG(d.svg, theme: theme).replacingOccurrences(of: Pele.fontFamily, with: "Helvetica Neue")
            client.send(["id": id, "ok": true, "svg": svg, "width": d.width, "height": d.height])
        } catch {
            client.fail(id, "\(error)")
        }
    }
}

@MainActor
func image(_ req: [String: Any], id: Any, client: Client) {
    guard let path = req["path"] as? String else { return client.fail(id, "image needs a path") }
    guard let source = CGImageSourceCreateWithURL(URL(fileURLWithPath: path) as CFURL, nil),
          let image = CGImageSourceCreateImageAtIndex(source, 0, nil)
    else { return client.fail(id, "can't decode \(path)") }
    do {
        let shm = try writeShm(rgba(image, image.width, image.height))
        client.send(["id": id, "ok": true, "shm": shm, "w": image.width, "h": image.height])
    } catch {
        client.fail(id, "shm: \(error)")
    }
}

@MainActor
func handle(_ line: String, from client: Client) {
    guard let data = line.data(using: .utf8),
          let req = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
    else { return client.fail(NSNull(), "bad request: not a JSON object") }
    let id: Any = req["id"] ?? NSNull()
    let op = req["op"] as? String ?? ""
    let pageId = (req["id"] as? NSNumber)?.intValue ?? -1
    let num = { (key: String) in (req[key] as? NSNumber)?.doubleValue }
    switch op {
    // Sent by stage at startup to launch the helper early; launching is the warm-up.
    case "warm": client.send(["id": id, "ok": true])
    case "mermaid": mermaid(req, id: id, client: client)
    case "image": image(req, id: id, client: client)
    case "html-open":
        guard let path = req["path"] as? String, let width = num("width"), width > 0 else {
            return client.fail(id, "html-open needs a path and a width")
        }
        client.pages[pageId]?.close()
        client.pages[pageId] = Page(id: pageId, client: client, path: path, width: width, maxHeight: num("max_height") ?? 2000,
                                    scale: num("scale") ?? 2, fps: num("fps") ?? 60)
    case "html-update":
        guard let path = req["path"] as? String else { return client.fail(id, "html-update needs a path") }
        guard let page = client.pages[pageId] else { return client.fail(id, "no page \(pageId)") }
        page.update(path: path)
    case "html-ack", "html-pause", "html-resume", "html-resize", "html-close":
        guard let page = client.pages[pageId] else { return client.fail(id, "no page \(pageId)") }
        switch op {
        case "html-ack": page.ack((req["frame"] as? NSNumber)?.intValue ?? -1)
        case "html-pause": page.pause()
        case "html-resume": page.resume()
        case "html-resize": page.resize(width: num("width"), maxHeight: num("max_height"), scale: num("scale"))
        default:
            page.close()
            client.pages[pageId] = nil
        }
    default: client.fail(id, "unknown op \"\(op)\"")
    }
}

/// Lines from `fd` until EOF, each handled on the main thread for `client`.
func readLines(_ fd: Int32, for client: Client) {
    guard let file = fdopen(fd, "r") else { return }
    var line: UnsafeMutablePointer<CChar>?
    var cap = 0
    while getline(&line, &cap, file) > 0 {
        let text = String(cString: line!).trimmingCharacters(in: .newlines)
        if !text.isEmpty {
            DispatchQueue.main.async { MainActor.assumeIsolated { handle(text, from: client) } }
        }
    }
    free(line)
    fclose(file)
}

// MARK: serve mode

/// Clients connected to a `--serve` helper; when the last leaves, exit after 10 idle minutes.
@MainActor enum Served {
    static var clients = 0
    static var idleExit: DispatchWorkItem?
}

/// `--serve <socket>`: one helper per user, shared by every stage, so a stage
/// starting up finds pele loaded and WebKit's web process running already.
func serve(_ path: String) {
    let fd = socket(AF_UNIX, SOCK_STREAM, 0)
    var addr = sockaddr_un()
    addr.sun_family = sa_family_t(AF_UNIX)
    withUnsafeMutableBytes(of: &addr.sun_path) { buf in path.utf8CString.withUnsafeBytes { buf.copyMemory(from: UnsafeRawBufferPointer(rebasing: $0.prefix(buf.count - 1))) } }
    // A helper already answering there wins; a stale socket file doesn't.
    if connectTo(path) { exit(0) }
    unlink(path)
    let len = socklen_t(MemoryLayout<sockaddr_un>.size)
    guard withUnsafePointer(to: &addr, { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(fd, $0, len) } }) == 0,
          listen(fd, 8) == 0
    else { exit(0) }  // another helper won the race to bind
    chmod(path, 0o600)
    DispatchQueue.main.async { MainActor.assumeIsolated { scheduleIdleExit() } }
    Thread.detachNewThread {
        while true {
            let conn = accept(fd, nil, nil)
            if conn < 0 { continue }
            let client = Client(out: conn)
            DispatchQueue.main.async { MainActor.assumeIsolated { Served.clients += 1; Served.idleExit?.cancel(); Served.idleExit = nil } }
            Thread.detachNewThread {
                readLines(dup(conn), for: client)
                DispatchQueue.main.async {
                    MainActor.assumeIsolated {
                        for page in client.pages.values { page.close() }
                        client.pages.removeAll()
                        close(conn)
                        Served.clients -= 1
                        if Served.clients == 0 { scheduleIdleExit() }
                    }
                }
            }
        }
    }
}

func connectTo(_ path: String) -> Bool {
    let fd = socket(AF_UNIX, SOCK_STREAM, 0)
    defer { close(fd) }
    var addr = sockaddr_un()
    addr.sun_family = sa_family_t(AF_UNIX)
    withUnsafeMutableBytes(of: &addr.sun_path) { buf in path.utf8CString.withUnsafeBytes { buf.copyMemory(from: UnsafeRawBufferPointer(rebasing: $0.prefix(buf.count - 1))) } }
    return withUnsafePointer(to: &addr) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size)) } } == 0
}

@MainActor
func scheduleIdleExit() {
    let work = DispatchWorkItem { exit(0) }
    Served.idleExit = work
    DispatchQueue.main.asyncAfter(deadline: .now() + 600, execute: work)
}

// No Dock icon, no menu bar, never takes focus.
let app = NSApplication.shared
app.setActivationPolicy(.prohibited)
// Warm up before the first request: pele's JavaScript on its queue, a spare web
// view (and with it WebKit's web process) on the main thread.
renders.enter()
DispatchQueue.global(qos: .userInitiated).async {
    _ = try? Pele.shared.render("flowchart TD\n  a --> b", maxWidth: nil, fontSize: 14)
    renders.leave()
}
DispatchQueue.main.async { MainActor.assumeIsolated { Spare.prepare() } }
let args = CommandLine.arguments
if args.count == 3, args[1] == "--serve" {
    serve(args[2])
} else {
    // Pipe mode: one client on stdin/stdout; exit at EOF once replies are out.
    Thread.detachNewThread {
        readLines(dup(0), for: Client(out: 1))
        // The main queue is FIFO: once this runs, every queued request has started (and
        // entered `renders`); exit after their replies.
        DispatchQueue.main.async { renders.notify(queue: .main) { exit(0) } }
    }
}
app.run()
