// One html part rendered live: an offscreen WKWebView whose frames go to stage as
// shm buffers. Capture is driven by the page, never by a polling timer:
//   - the first frame once the page has loaded and painted;
//   - one frame per DOM mutation burst (debounced ~16 ms);
//   - a frame timer at `fps` only while the page animates (CSS/Web Animations running,
//     or requestAnimationFrame being called), stopped as soon as it goes quiet.
// At most one frame is unacked; while one is in flight a due capture is deferred to
// the ack, so stage always gets the newest pixels and never a backlog.

import AppKit
import WebKit

/// Reports mutations and the first rAF after idle, and answers "still animating?".
private let pageScript = """
(() => {
  const post = m => { try { webkit.messageHandlers.stage.postMessage(m) } catch (e) {} };
  const raf = window.requestAnimationFrame.bind(window);
  window.__stageRAF = raf;
  let calls = 0, armed = true;
  window.requestAnimationFrame = cb => { calls++; if (armed) { armed = false; post('wake') } return raf(cb) };
  window.__stageAnimating = () => {
    const busy = document.getAnimations().some(a => a.playState === 'running') || calls > 0;
    calls = 0;
    if (!busy) armed = true;
    return busy;
  };
  const watch = () => new MutationObserver(() => post('mut')).observe(document.documentElement,
    { subtree: true, childList: true, attributes: true, characterData: true });
  if (document.documentElement) watch(); else document.addEventListener('DOMContentLoaded', watch);
})();
"""

/// `STAGE_HELPER_TRACE=1` logs each frame's snapshot and conversion time to stderr.
private let trace = ProcessInfo.processInfo.environment["STAGE_HELPER_TRACE"] != nil

/// WKUserContentController retains its handlers; this breaks the cycle with Page.
@MainActor
private final class Weak: NSObject, WKScriptMessageHandler {
    weak var page: Page?
    init(_ page: Page) { self.page = page }
    func userContentController(_ c: WKUserContentController, didReceive message: WKScriptMessage) {
        page?.message(message.body as? String ?? "")
    }
}

@MainActor
final class Page: NSObject, WKNavigationDelegate {
    let id: Int
    private let fps: Double
    private var width: Double, maxHeight: Double, scale: Double
    private var height: Double
    private let window: NSWindow
    private let web: WKWebView

    private var loaded = false, paused = false, snapping = false
    private var frame = 0
    private var inFlight: Int?
    /// A capture came due while a frame was unacked or a snapshot was running.
    private var due = false
    private var lastHash: Int?
    private var timer: DispatchSourceTimer?
    private var framesSinceCheck = 0
    private var mutation: DispatchWorkItem?

    init(id: Int, path: String, width: Double, maxHeight: Double, scale: Double, fps: Double) {
        self.id = id
        self.width = width
        self.maxHeight = maxHeight
        self.height = maxHeight
        self.scale = scale
        self.fps = max(1, min(fps, 120))
        let config = WKWebViewConfiguration()
        // Offscreen pages would otherwise be throttled like a background tab.
        config.preferences.inactiveSchedulingPolicy = .none
        web = WKWebView(frame: NSRect(x: 0, y: 0, width: width, height: maxHeight), configuration: config)
        // A borderless, fully transparent window that ignores the mouse: WebKit only
        // paints (and runs animations) for a view in a window.
        window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: width, height: maxHeight),
                          styleMask: .borderless, backing: .buffered, defer: false)
        super.init()
        config.userContentController.addUserScript(WKUserScript(source: pageScript, injectionTime: .atDocumentStart, forMainFrameOnly: true))
        config.userContentController.add(Weak(self), name: "stage")
        web.navigationDelegate = self
        window.isReleasedWhenClosed = false
        window.alphaValue = 0
        window.ignoresMouseEvents = true
        window.hasShadow = false
        window.contentView = web
        window.orderFrontRegardless()
        let url = URL(fileURLWithPath: path)
        web.loadFileURL(url, allowingReadAccessTo: url.deletingLastPathComponent())
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        loaded = true
        layout()
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        fail(id, "html: \(error.localizedDescription)")
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        fail(id, "html: \(error.localizedDescription)")
    }

    /// Size the viewport to the content (capped at maxHeight), wait for a paint, capture.
    private func layout() {
        // Measure in a 1px-tall viewport: scrollHeight is never less than the viewport, and in
        // quirks mode the root box fills it, so only a tiny one shows the content's own height.
        setSize(width, 1)
        web.evaluateJavaScript("document.documentElement.scrollHeight") { [weak self] h, _ in
            guard let self else { return }
            let content = (h as? NSNumber)?.doubleValue ?? self.maxHeight
            self.setSize(self.width, max(1, min(content, self.maxHeight)))
            self.afterPaint { self.lastHash = nil; self.capture(); self.checkAnimating() }
        }
    }

    private func setSize(_ w: Double, _ h: Double) {
        height = h
        window.setContentSize(NSSize(width: w, height: h))
        web.frame = NSRect(x: 0, y: 0, width: w, height: h)
    }

    /// Two real rAFs (not the wrapped one, which would read as animation).
    private func afterPaint(_ then: @escaping () -> Void) {
        web.callAsyncJavaScript("await new Promise(r => __stageRAF(() => __stageRAF(r)))", arguments: [:], in: nil, in: .page) { _ in then() }
    }

    func message(_ kind: String) {
        guard loaded, !paused else { return }
        switch kind {
        case "mut":
            mutation?.cancel()
            let work = DispatchWorkItem { [weak self] in self?.capture(); self?.checkAnimating() }
            mutation = work
            DispatchQueue.main.asyncAfter(deadline: .now() + .milliseconds(16), execute: work)
        case "wake": startTimer()
        default: break
        }
    }

    private func capture() {
        guard loaded, !paused else { return }
        if inFlight != nil || snapping {
            due = true
            return
        }
        due = false
        snapping = true
        let (w, h) = (Int((width * scale).rounded()), Int((height * scale).rounded()))
        let t0 = DispatchTime.now().uptimeNanoseconds
        web.takeSnapshot(with: nil) { [weak self] image, _ in
            guard let self else { return }
            self.snapping = false
            let t1 = DispatchTime.now().uptimeNanoseconds
            guard !self.paused, let cg = image?.cgImage(forProposedRect: nil, context: nil, hints: nil) else { return }
            let px = rgba(cg, w, h)
            if trace {
                FileHandle.standardError.write("snap \((t1 - t0) / 1000) us, rgba \((DispatchTime.now().uptimeNanoseconds - t1) / 1000) us, \(cg.width)x\(cg.height)\n".data(using: .utf8)!)
            }
            var hasher = Hasher()
            px.withUnsafeBytes { hasher.combine(bytes: $0) }
            let hash = hasher.finalize()
            if hash != self.lastHash {
                self.lastHash = hash
                do {
                    let shm = try writeShm(px)
                    self.frame += 1
                    self.inFlight = self.frame
                    send(["id": self.id, "frame": self.frame, "shm": shm, "w": w, "h": h])
                } catch {
                    fail(self.id, "shm: \(error)")
                }
            }
            if self.due { self.capture() }
        }
    }

    func ack(_ k: Int) {
        guard k == inFlight else { return }
        inFlight = nil
        if due { capture() }
    }

    /// Ask the page whether it's still moving; run the frame timer only while it is.
    private func checkAnimating() {
        web.evaluateJavaScript("__stageAnimating()") { [weak self] busy, _ in
            guard let self else { return }
            if (busy as? Bool) == true { self.startTimer() } else { self.stopTimer() }
        }
    }

    private func startTimer() {
        guard timer == nil, loaded, !paused else { return }
        let t = DispatchSource.makeTimerSource(queue: .main)
        t.schedule(deadline: .now(), repeating: 1 / fps, leeway: .milliseconds(2))
        t.setEventHandler { [weak self] in
            guard let self else { return }
            self.capture()
            self.framesSinceCheck += 1
            if self.framesSinceCheck >= Int(self.fps / 4) {
                self.framesSinceCheck = 0
                self.checkAnimating()
            }
        }
        timer = t
        t.resume()
    }

    private func stopTimer() {
        timer?.cancel()
        timer = nil
        framesSinceCheck = 0
    }

    func pause() {
        paused = true
        stopTimer()
        mutation?.cancel()
    }

    func resume() {
        paused = false
        lastHash = nil  // a fresh frame even if nothing changed
        inFlight = nil
        capture()
        checkAnimating()
    }

    func resize(width: Double?, maxHeight: Double?, scale: Double?) {
        if let width, width > 0 { self.width = width }
        if let maxHeight, maxHeight > 0 { self.maxHeight = maxHeight }
        if let scale, scale > 0 { self.scale = scale }
        guard loaded else { return }
        layout()
    }

    func close() {
        stopTimer()
        mutation?.cancel()
        web.configuration.userContentController.removeScriptMessageHandler(forName: "stage")
        window.orderOut(nil)
        window.close()
    }
}
