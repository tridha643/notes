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

/// A new revision of the page, applied in place when only stylesheets, inline styles of
/// uniquely identified elements, or text of identified leaf elements changed, so scripts,
/// animations and state keep running. Anything else (scripts, structure, other
/// attributes) answers why, and the page reloads. Adapted from fx0's preview runtime
/// (vercel-labs/fx0 lib/preview-update.ts, Apache-2.0).
private let patchScript = """
const parse = s => new DOMParser().parseFromString(s, 'text/html');
const textTags = new Set(['h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'p', 'span', 'li', 'button', 'a', 'label']);
const changes = [];
const fail = why => { throw new Error(why) };
const attrs = el => [...el.attributes].filter(a => a.name !== 'style').map(a => a.name + '=' + a.value).sort().join('\\n');
const walk = (x, y) => {
  if (x.nodeType !== y.nodeType) fail('tree-shape-changed');
  if (x.nodeType === 3) return x.data === y.data || fail('text-outside-leaf');
  if (x.nodeType !== 1) return;
  if (x.localName !== y.localName || x.id !== y.id) fail('element-identity-changed');
  if (attrs(x) !== attrs(y)) fail('attributes-changed');
  if (x.localName === 'script') return x.textContent === y.textContent || fail('script-changed');
  if (x.localName === 'style') return x.textContent === y.textContent || changes.push(['css', x, y]);
  if ((x.getAttribute('style') ?? '') !== (y.getAttribute('style') ?? '')) changes.push(['style', x, y]);
  const xs = [...x.childNodes], ys = [...y.childNodes];
  const leaf = xs.concat(ys).every(n => n.nodeType === 3);
  if (leaf && xs.length + ys.length > 0) {
    if (x.textContent === y.textContent) return;
    return textTags.has(x.localName) ? changes.push(['text', x, y]) : fail('text-outside-leaf');
  }
  if (xs.length !== ys.length) fail('tree-shape-changed');
  xs.forEach((n, i) => walk(n, ys[i]));
};
try {
  const a = parse(before), b = parse(after);
  walk(a.documentElement, b.documentElement);
  // Resolve every live target first, so a patch applies whole or not at all.
  const authored = [...a.querySelectorAll('style')], live = [...document.querySelectorAll('style')];
  const ops = changes.map(([kind, x, y]) => {
    if (kind === 'css') {
      if (authored.length !== live.length) fail('stylesheets-changed-at-runtime');
      return () => { live[authored.indexOf(x)].textContent = y.textContent };
    }
    const found = x.id ? document.querySelectorAll('#' + CSS.escape(x.id)) : [];
    if (found.length !== 1) fail('unidentified-target');
    const el = found[0];
    if (kind === 'text') return () => { el.textContent = y.textContent };
    const style = y.getAttribute('style');
    return () => style === null ? el.removeAttribute('style') : el.setAttribute('style', style);
  });
  ops.forEach(op => op());
  return 'ok';
} catch (e) {
  return e.message || String(e);
}
"""

/// `STAGE_HELPER_TRACE=1` logs each frame's snapshot and conversion time to stderr.
private let trace = ProcessInfo.processInfo.environment["STAGE_HELPER_TRACE"] != nil

/// Milliseconds since `t0`, for `STAGE_HELPER_TRACE` phase lines.
func traceMark(_ what: String, since t0: UInt64) {
    guard trace else { return }
    let ms = Double(DispatchTime.now().uptimeNanoseconds - t0) / 1e6
    FileHandle.standardError.write("\(what) \(String(format: "%.1f", ms)) ms\n".data(using: .utf8)!)
}

/// WKUserContentController retains its handlers; this breaks the cycle with Page.
@MainActor
private final class Weak: NSObject, WKScriptMessageHandler {
    weak var page: Page?
    init(_ page: Page) { self.page = page }
    func userContentController(_ c: WKUserContentController, didReceive message: WKScriptMessage) {
        page?.message(message.body as? String ?? "")
    }
}

/// One web view built ahead of time, in its window, with a blank page loaded: WebKit's
/// web process is already up when a page opens, instead of starting on that open.
@MainActor
enum Spare {
    // Reuse closed pages as well as the proactively warmed spare. Keep this
    // bounded: a burst must not leave an unbounded pool of WebKit views/windows.
    private static var ready: [(WKWebView, NSWindow)] = []
    private static var resetting = 0
    private static let capacity = 32

    /// WebKit only paints (and runs animations) for a view in a window: a borderless,
    /// fully transparent one that ignores the mouse.
    static func make(width: Double, height: Double) -> (WKWebView, NSWindow) {
        let config = WKWebViewConfiguration()
        // Each reusable view owns its data: clearing a closed page must not erase
        // session storage/cookies belonging to another live page.
        config.websiteDataStore = .nonPersistent()
        // Offscreen pages would otherwise be throttled like a background tab.
        config.preferences.inactiveSchedulingPolicy = .none
        let web = WKWebView(frame: NSRect(x: 0, y: 0, width: width, height: height), configuration: config)
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: width, height: height),
                              styleMask: .borderless, backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.alphaValue = 0
        window.ignoresMouseEvents = true
        window.hasShadow = false
        window.contentView = web
        window.orderFrontRegardless()
        return (web, window)
    }

    static func prepare() {
        guard ready.isEmpty && resetting == 0 else { return }
        let started = DispatchTime.now().uptimeNanoseconds
        let spare = make(width: 400, height: 300)
        spare.0.loadHTMLString("", baseURL: nil)
        ready.append(spare)
        traceMark("spare prepared", since: started)
    }

    /// The prepared view (or a fresh one); the next spare is built once this turn ends.
    static func take(width: Double, height: Double) -> (WKWebView, NSWindow) {
        let spare = ready.popLast() ?? make(width: width, height: height)
        DispatchQueue.main.async { prepare() }
        return spare
    }

    static func recycle(_ web: WKWebView, _ window: NSWindow) {
        web.navigationDelegate = nil
        web.stopLoading()
        web.configuration.userContentController.removeAllUserScripts()
        guard ready.count + resetting < capacity else {
            window.orderOut(nil)
            window.close()
            return
        }
        web.pageZoom = 1
        // A navigation destroys the old document, including user timers and globals.
        // Merely pausing captures would let scripts keep running in the pool.
        web.loadHTMLString("", baseURL: nil)
        resetting += 1
        // Blank navigation resets globals, but NOT sessionStorage. Do not lend
        // the view out until its private store has finished clearing all origins.
        web.configuration.websiteDataStore.removeData(
            ofTypes: WKWebsiteDataStore.allWebsiteDataTypes(), modifiedSince: .distantPast
        ) {
            resetting -= 1
            ready.append((web, window))
        }
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

    private var loaded = false, paused = false, snapping = false, closed = false
    /// A recycled view may still deliver callbacks from its blank navigation.
    private var navigation: WKNavigation?
    /// No snapshots while layout briefly uses a 1pt measurement viewport.
    private var measuring = false
    /// Bumped by each layout, so callbacks and snapshots from an older one are dropped.
    private var layoutVersion = 0
    private var frame = 0
    private var inFlight: Int?
    /// A capture came due while a frame was unacked or a snapshot was running.
    private var due = false
    private var lastHash: Int?
    private var timer: DispatchSourceTimer?
    private var framesSinceCheck = 0
    private var mutation: DispatchWorkItem?

    private let opened = DispatchTime.now().uptimeNanoseconds

    private let client: Client
    /// The authored html of the shown revision (what the next update diffs against).
    private var source: String
    /// Only one patch/navigation transition runs at once; diffs always use the
    /// last committed authored source, never a revision still being applied.
    private var updating = false
    private var pendingUpdates: [(path: String, source: String)] = []

    init(id: Int, client: Client, path: String, width: Double, maxHeight: Double, scale: Double, fps: Double) {
        self.id = id
        self.client = client
        self.width = width
        self.maxHeight = maxHeight
        self.height = maxHeight
        self.scale = scale
        self.fps = max(1, min(fps, 120))
        source = (try? String(contentsOfFile: path, encoding: .utf8)) ?? ""
        (web, window) = Spare.take(width: width, height: maxHeight)
        super.init()
        traceMark("html took spare", since: opened)
        // Scripts added now apply from the next navigation on: this page's load.
        let content = web.configuration.userContentController
        content.addUserScript(WKUserScript(source: pageScript, injectionTime: .atDocumentStart, forMainFrameOnly: true))
        content.add(Weak(self), name: "stage")
        web.navigationDelegate = self
        setSize(width, maxHeight)
        traceMark("html configured", since: opened)
        let url = URL(fileURLWithPath: path)
        navigation = web.loadFileURL(url, allowingReadAccessTo: url.deletingLastPathComponent())
        traceMark("html navigation queued", since: opened)
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        guard navigation != nil, navigation === self.navigation else { return }
        traceMark("html loaded", since: opened)
        loaded = true
        updating = false
        if pendingUpdates.isEmpty { layout() } else { applyNextUpdate() }
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        guard navigation != nil, navigation === self.navigation else { return }
        navigationFailed(error)
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        guard navigation != nil, navigation === self.navigation else { return }
        navigationFailed(error)
    }

    private func navigationFailed(_ error: Error) {
        navigation = nil
        loaded = false
        updating = false
        client.fail(id, "html: \(error.localizedDescription)")
        applyNextUpdate()
    }

    /// Size the viewport to the content (capped at maxHeight), wait for a paint, capture.
    private func layout() {
        // Measure in a 1pt-tall viewport without sending that intermediate size
        // to stage. A newer resize also supersedes these asynchronous callbacks.
        layoutVersion += 1
        let version = layoutVersion
        measuring = true
        setSize(width, 1)
        web.pageZoom = 1
        let measure = "[document.documentElement.scrollWidth, document.documentElement.scrollHeight]"
        web.evaluateJavaScript(measure) { [weak self] size, _ in
            guard let self, self.layoutVersion == version else { return }
            let size = (size as? [NSNumber])?.map(\.doubleValue) ?? []
            // A page wider than the card (a fixed-width teardown, a wide table) is zoomed
            // out to fit it, never in, and laid out again at that zoom.
            let zoom = size.count == 2 ? max(0.3, min(1, self.width / max(size[0], 1))) : 1
            guard zoom < 0.999 else { return self.show(height: size.count == 2 ? size[1] : self.maxHeight) }
            self.web.pageZoom = zoom
            self.web.evaluateJavaScript("document.documentElement.scrollHeight") { [weak self] h, _ in
                guard let self, self.layoutVersion == version else { return }
                self.show(height: ((h as? NSNumber)?.doubleValue ?? 0) * zoom)
            }
        }
    }

    /// Size the viewport to `height` points of content (capped) and capture.
    private func show(height content: Double) {
        setSize(width, max(1, min(content, maxHeight)))
        measuring = false
        traceMark("html measured", since: opened)
        // takeSnapshot waits for the next committed paint itself, so no rAF round trip first.
        lastHash = nil
        capture()
        checkAnimating()
    }

    private func setSize(_ w: Double, _ h: Double) {
        height = h
        window.setContentSize(NSSize(width: w, height: h))
        web.frame = NSRect(x: 0, y: 0, width: w, height: h)
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
        if updating || measuring || inFlight != nil || snapping {
            due = true
            return
        }
        due = false
        snapping = true
        let version = layoutVersion
        let (w, h) = (Int((width * scale).rounded()), Int((height * scale).rounded()))
        let t0 = DispatchTime.now().uptimeNanoseconds
        web.takeSnapshot(with: nil) { [weak self] image, _ in
            guard let self else { return }
            self.snapping = false
            if version != self.layoutVersion {
                if !self.measuring { self.capture() }
                return
            }
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
                    self.client.send(.frame(id: self.id, frame: self.frame, shm: shm, w: w, h: h))
                    if self.frame == 1 { traceMark("html first frame", since: self.opened) }
                } catch {
                    self.client.fail(self.id, "shm: \(error)")
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
        guard timer == nil, loaded, !paused, !updating else { return }
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
        // Keep ownership of an outstanding frame: its ack releases backpressure.
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

    /// Show the html at `path`: patched in place when it can be, else reloaded. The last
    /// frame stays on screen meanwhile, so the card never blanks.
    func update(path: String) {
        let after = (try? String(contentsOfFile: path, encoding: .utf8)) ?? ""
        pendingUpdates.append((path, after))
        applyNextUpdate()
    }

    private func applyNextUpdate() {
        guard !closed, !updating, !pendingUpdates.isEmpty else { return }
        // Wait for an active navigation, but allow a new revision to recover a
        // failed load instead of leaving its queue stuck forever.
        guard loaded || navigation == nil else { return }
        let change = pendingUpdates.removeFirst()
        updating = true
        // A snapshot or layout from the preceding revision must never be emitted
        // under the new revision, even while a fallback navigation is loading.
        layoutVersion += 1
        mutation?.cancel()
        stopTimer()
        guard loaded else {
            client.send(.patched(id: id, patched: false))
            return reload(change)
        }
        let t0 = DispatchTime.now().uptimeNanoseconds
        web.callAsyncJavaScript(patchScript, arguments: ["before": source, "after": change.source], in: nil, in: .page) { [weak self] result in
            guard let self, !self.closed else { return }
            let outcome = (try? result.get()) as? String ?? "patch-failed"
            traceMark("html update \(outcome)", since: t0)
            self.client.send(.patched(id: self.id, patched: outcome == "ok"))
            self.source = change.source
            if outcome == "ok" {
                self.updating = false
                if self.pendingUpdates.isEmpty { self.layout() } else { self.applyNextUpdate() }
            } else {
                self.reload(change)
                // didFinish commits this navigation before processing the next patch.
            }
        }
    }

    private func reload(_ change: (path: String, source: String)) {
        loaded = false
        source = change.source
        let url = URL(fileURLWithPath: change.path)
        navigation = web.loadFileURL(url, allowingReadAccessTo: url.deletingLastPathComponent())
    }

    func close() {
        guard !closed else { return }
        closed = true
        navigation = nil
        loaded = false
        paused = true
        layoutVersion += 1
        pendingUpdates.removeAll()
        stopTimer()
        mutation?.cancel()
        web.configuration.userContentController.removeScriptMessageHandler(forName: "stage")
        Spare.recycle(web, window)
    }
}
