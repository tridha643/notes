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
// mermaid, which never touches AppKit and goes straight to pele's own queue.

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
    /// Used under `lock`.
    private let encoder: JSONEncoder = {
        let e = JSONEncoder()
        e.outputFormatting = .withoutEscapingSlashes
        return e
    }()
    /// Main thread only.
    var pages: [Int: Page] = [:]

    init(out: Int32) { self.out = out }

    /// One reply line. Callable from any thread: replies that don't touch AppKit
    /// (mermaid) go out from their own queue instead of waiting on the main thread.
    func send(_ reply: HelperReply) {
        lock.lock()
        defer { lock.unlock() }
        guard var data = try? encoder.encode(reply) else { return }
        data.append(0x0A)
        data.withUnsafeBytes { buf in
            var sent = 0
            while sent < buf.count {
                let n = write(out, buf.baseAddress! + sent, buf.count - sent)
                if n <= 0 { return }  // the client went away; its reader notices
                sent += n
            }
        }
    }

    func fail(_ id: Int?, _ error: String) {
        send(.failed(id: id, error: error))
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

/// Called from the reader thread, so a diagram never waits behind WebKit work on the main thread.
func mermaid(id: Int, source: String, maxWidth: Double, fontSize: Double, client: Client) {
    let theme = peleTheme
    // pele blocks on its own queue, so render off the reader thread.
    renders.enter()
    DispatchQueue.global(qos: .userInitiated).async {
        defer { renders.leave() }
        do {
            let t0 = DispatchTime.now().uptimeNanoseconds
            let d = try Pele.shared.render(source, maxWidth: maxWidth, fontSize: fontSize)
            traceMark("pele render", since: t0)
            let svg = resolvePeleSVG(d.svg, theme: theme).replacingOccurrences(of: Pele.fontFamily, with: "Helvetica Neue")
            client.send(.mermaid(id: id, svg: svg, width: d.width, height: d.height))
        } catch {
            client.fail(id, "\(error)")
        }
    }
}

@MainActor
func image(id: Int, path: String, client: Client) {
    guard let source = CGImageSourceCreateWithURL(URL(fileURLWithPath: path) as CFURL, nil),
          let image = CGImageSourceCreateImageAtIndex(source, 0, nil)
    else { return client.fail(id, "can't decode \(path)") }
    do {
        let shm = try writeShm(rgba(image, image.width, image.height))
        client.send(.image(id: id, shm: shm, w: image.width, h: image.height))
    } catch {
        client.fail(id, "shm: \(error)")
    }
}

@MainActor
func handle(_ req: HelperRequest, from client: Client) {
    switch req {
    // Sent by stage at startup to launch the helper early; launching is the warm-up.
    case .warm(let id): client.send(.done(id: id))
    // Handled on the reader thread, see `readLines`.
    case .mermaid(let id, let source, let maxWidth, let fontSize): mermaid(id: id, source: source, maxWidth: maxWidth, fontSize: fontSize, client: client)
    case .image(let id, let path): image(id: id, path: path, client: client)
    case .htmlOpen(let id, let path, let width, let maxHeight, let scale, let fps):
        guard width > 0 else { return client.fail(id, "html-open needs a width") }
        client.pages[id]?.close()
        client.pages[id] = Page(id: id, client: client, path: path, width: width, maxHeight: maxHeight, scale: scale, fps: fps)
    case .htmlUpdate(let id, let path):
        guard let page = client.pages[id] else { return client.fail(id, "no page \(id)") }
        page.update(path: path)
    case .htmlAck(let id, _), .htmlPause(let id), .htmlResume(let id), .htmlResize(let id, _, _, _), .htmlClose(let id):
        guard let page = client.pages[id] else { return client.fail(id, "no page \(id)") }
        switch req {
        case .htmlAck(_, let frame): page.ack(frame)
        case .htmlPause: page.pause()
        case .htmlResume: page.resume()
        case .htmlResize(_, let width, let maxHeight, let scale): page.resize(width: width, maxHeight: maxHeight, scale: scale)
        default:
            page.close()
            client.pages[id] = nil
        }
    }
}

/// Lines from `fd` until EOF for `client`: mermaid starts here, the rest is handled on the main thread.
func readLines(_ fd: Int32, for client: Client) {
    guard let file = fdopen(fd, "r") else { return }
    var line: UnsafeMutablePointer<CChar>?
    var cap = 0
    while getline(&line, &cap, file) > 0 {
        let text = String(cString: line!).trimmingCharacters(in: .newlines)
        if text.isEmpty { continue }
        let req: HelperRequest
        do { req = try HelperRequest.parse(Data(text.utf8)) } catch let e as HelperRequestError {
            client.fail(e.id, "bad request: \(e.message)")
            continue
        } catch { continue }
        if case .mermaid(let id, let source, let maxWidth, let fontSize) = req {
            mermaid(id: id, source: source, maxWidth: maxWidth, fontSize: fontSize, client: client)
            continue
        }
        DispatchQueue.main.async { MainActor.assumeIsolated { handle(req, from: client) } }
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

// Warm up before the first request: pele's JavaScript on its queue first (it doesn't
// need AppKit, which takes ~40 ms to start cold), then a spare web view (and with it
// WebKit's web process) on the main thread.
renders.enter()
DispatchQueue.global(qos: .userInitiated).async {
    _ = try? Pele.shared.render("flowchart TD\n  a --> b", maxWidth: nil, fontSize: 14)
    renders.leave()
}
// No Dock icon, no menu bar, never takes focus.
let app = NSApplication.shared
app.setActivationPolicy(.prohibited)
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
