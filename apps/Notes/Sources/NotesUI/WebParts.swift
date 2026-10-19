import AppKit
import NotesCore
import WebKit

/// html parts: sandboxed WKWebViews, created only while a part is on screen and
/// pooled. A non-persistent data store keeps nothing on disk; the page's CSP
/// (sideshow's html-surface policy) allows only inline code plus the CDN allowlist;
/// navigation never leaves the page. The bridge mirrors sideshow's
/// (sendPrompt, openLink, copyToClipboard) and reports content height.
@MainActor
final class WebParts: NSObject, WKScriptMessageHandler, WKNavigationDelegate, WKUIDelegate {
    static let shared = WebParts()
    /// Idle web views kept for reuse; more are released.
    static let poolSize = 3

    weak var owner: PostsView?
    private let store = WKWebsiteDataStore.nonPersistent()
    private var idle: [WKWebView] = []
    /// What each web view has loaded, and which part it's showing.
    private var loaded: [ObjectIdentifier: String] = [:]
    private var bound: [ObjectIdentifier: (post: String, part: Int, key: String)] = [:]
    private(set) var created = 0

    /// A web view for `key`: an idle one that already shows it, else any idle one, else new.
    func take(key: String) -> WKWebView {
        if let i = idle.firstIndex(where: { loaded[ObjectIdentifier($0)] == key }) ?? idle.indices.last {
            return idle.remove(at: i)
        }
        let config = WKWebViewConfiguration()
        config.websiteDataStore = store
        config.userContentController.add(self, name: "sideshow")
        config.preferences.javaScriptCanOpenWindowsAutomatically = false
        let web = WKWebView(frame: .zero, configuration: config)
        web.navigationDelegate = self
        web.uiDelegate = self
        web.underPageBackgroundColor = Theme.card
        created += 1
        return web
    }

    func give(_ web: WKWebView) {
        web.removeFromSuperview()
        bound[ObjectIdentifier(web)] = nil
        if idle.count < Self.poolSize {
            idle.append(web)
        } else {
            loaded[ObjectIdentifier(web)] = nil
            web.configuration.userContentController.removeScriptMessageHandler(forName: "sideshow")
        }
    }

    /// Shows a part; reloads only when the content or appearance changed.
    func show(_ model: HTMLModel, in web: WKWebView, post: String, part: Int, dark: Bool) {
        let key = model.key + (dark ? ":dark" : ":light")
        bound[ObjectIdentifier(web)] = (post, part, model.key)
        guard loaded[ObjectIdentifier(web)] != key else { return }
        loaded[ObjectIdentifier(web)] = key
        web.loadHTMLString(Self.page(model, dark: dark), baseURL: nil)
    }

    // MARK: Bridge

    func userContentController(_ controller: WKUserContentController, didReceive message: WKScriptMessage) {
        guard let web = message.webView, let body = message.body as? [String: Any], let type = body["type"] as? String,
              let binding = bound[ObjectIdentifier(web)] else { return }
        switch type {
        case "resize":
            if let h = body["height"] as? Double { owner?.htmlResized(key: binding.key, height: CGFloat(h)) }
        case "send-prompt":
            if let text = body["text"] as? String { owner?.sendPrompt(post: binding.post, text: text) }
        case "open-link":
            if let s = body["url"] as? String { Self.openExternal(s) }
        case "copy":
            if let text = body["text"] as? String {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(text, forType: .string)
            }
        default: break
        }
    }

    static func openExternal(_ s: String) {
        guard let url = URL(string: s), ["http", "https"].contains(url.scheme?.lowercased()) else { return }
        NSWorkspace.shared.open(url)
    }

    /// Only the initial about:blank load runs in the view; clicked http(s) links open in the browser.
    func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction, decisionHandler: @escaping @MainActor (WKNavigationActionPolicy) -> Void) {
        let url = action.request.url
        if action.navigationType == .other, url?.absoluteString == "about:blank" || url?.scheme == "about" {
            return decisionHandler(.allow)
        }
        if action.targetFrame?.isMainFrame == false, action.navigationType == .other { return decisionHandler(.allow) } // iframes the page embeds
        if let url { Self.openExternal(url.absoluteString) }
        decisionHandler(.cancel)
    }

    /// window.open: hand http(s) to the browser, never open a web view.
    func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration, for action: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
        if let url = action.request.url { Self.openExternal(url.absoluteString) }
        return nil
    }

    // MARK: Page

    nonisolated static let cdns = ["https://cdnjs.cloudflare.com", "https://esm.sh", "https://cdn.jsdelivr.net", "https://unpkg.com",
                       "https://fonts.googleapis.com", "https://fonts.gstatic.com"].joined(separator: " ")

    nonisolated static let csp = [
        "default-src 'none'", "script-src 'unsafe-inline' \(cdns)", "style-src 'unsafe-inline' \(cdns)", "font-src \(cdns) data:",
        "img-src https: data: blob:", "connect-src \(cdns)", "media-src https: data: blob:",
    ].joined(separator: "; ")

    nonisolated static func page(_ model: HTMLModel, dark: Bool) -> String {
        let accents = dark
            ? "--c-teal-bg:rgba(31,169,150,.18);--c-teal-line:#1fa996;--c-teal-text:#6fd0c2;--c-coral-bg:rgba(232,131,94,.18);--c-coral-line:#e8835e;--c-coral-text:#f0a987;"
            : "--c-teal-bg:#e1f4f1;--c-teal-line:#1fa996;--c-teal-text:#0c6e62;--c-coral-bg:#fdece5;--c-coral-line:#e8835e;--c-coral-text:#a44f28;"
        let kitCSS = model.kits.map(\.css).joined(separator: "\n")
        let kitJS = model.kits.compactMap(\.js).joined(separator: "\n")
        return """
            <!doctype html><html lang="en"><head><meta charset="utf-8">
            <meta name="viewport" content="width=device-width, initial-scale=1">
            <meta http-equiv="Content-Security-Policy" content="\(csp)">
            <style>\(Theme.htmlTokens(dark: dark))\(tokensCSS)\(kitBaseCSS):root{\(accents)}\(kitCSS):root{color-scheme:\(dark ? "dark" : "light")}</style>
            </head><body>
            \(svgDefs)
            \(model.html)
            <script>\(bridgeJS)</script>
            \(kitJS.isEmpty ? "" : "<script>\(kitJS)</script>")
            </body></html>
            """
    }

    // From sideshow's server/surfacePage.ts (TOKENS_CSS, KIT_CSS, SVG_DEFS, BRIDGE_JS),
    // with the bridge posting to WebKit instead of a parent frame.
    nonisolated static let tokensCSS = """
        :root{--font-sans:-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,Helvetica,Arial,sans-serif;
        --font-serif:"Iowan Old Style","Palatino Linotype",Palatino,Georgia,serif;--font-mono:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;
        --border-radius-md:8px;--border-radius-lg:12px;--border-radius-xl:16px}
        html{box-sizing:border-box;scrollbar-width:none}html::-webkit-scrollbar,body::-webkit-scrollbar{display:none}
        *,*::before,*::after{box-sizing:inherit}
        body{margin:0;padding:16px;background:var(--color-background-primary);color:var(--color-text-primary);font:16px/1.6 var(--font-sans)}
        """

    nonisolated static let kitBaseCSS = """
        button{font:500 14px/1.4 var(--font-sans);color:var(--color-text-primary);background:none;border:0.5px solid var(--color-border-secondary);border-radius:var(--border-radius-md);padding:6px 14px;cursor:pointer}
        button:hover{background:var(--color-background-secondary)}
        input:not([type=checkbox]):not([type=radio]):not([type=range]),select,textarea{font:14px/1.4 var(--font-sans);color:var(--color-text-primary);background:var(--color-background-primary);border:0.5px solid var(--color-border-secondary);border-radius:var(--border-radius-md);padding:6px 10px;outline:none}
        input:focus,select:focus,textarea:focus{border-color:var(--color-border-info)}
        input::placeholder,textarea::placeholder{color:var(--color-text-tertiary)}
        textarea{resize:vertical}
        input[type=checkbox],input[type=radio],input[type=range],progress{accent-color:var(--color-border-info)}
        svg{font-family:var(--font-sans);fill:var(--color-text-primary)}
        .t{font-size:14px}.ts{font-size:12px;fill:var(--color-text-secondary)}.th{font-size:14px;font-weight:500}
        .box{fill:var(--color-background-secondary);stroke:var(--color-border-tertiary);rx:8px}
        .arr{stroke:var(--color-text-secondary);stroke-width:1.2;fill:none}
        .leader{stroke:var(--color-border-secondary);stroke-width:1;stroke-dasharray:3 4;fill:none}
        .node{cursor:pointer}.node:hover{opacity:.75}
        .c-blue,.c-blue .box{fill:var(--color-background-info);stroke:var(--color-border-info)}.c-blue text,text.c-blue{fill:var(--color-text-info);stroke:none}
        .c-teal,.c-teal .box{fill:var(--c-teal-bg);stroke:var(--c-teal-line)}.c-teal text,text.c-teal{fill:var(--c-teal-text);stroke:none}
        .c-amber,.c-amber .box{fill:var(--color-background-warning);stroke:var(--color-border-warning)}.c-amber text,text.c-amber{fill:var(--color-text-warning);stroke:none}
        .c-coral,.c-coral .box{fill:var(--c-coral-bg);stroke:var(--c-coral-line)}.c-coral text,text.c-coral{fill:var(--c-coral-text);stroke:none}
        .c-green,.c-green .box{fill:var(--color-background-success);stroke:var(--color-border-success)}.c-green text,text.c-green{fill:var(--color-text-success);stroke:none}
        .c-red,.c-red .box{fill:var(--color-background-danger);stroke:var(--color-border-danger)}.c-red text,text.c-red{fill:var(--color-text-danger);stroke:none}
        .c-gray,.c-gray .box{fill:var(--color-background-secondary);stroke:var(--color-border-secondary)}.c-gray text,text.c-gray{fill:var(--color-text-secondary);stroke:none}
        """

    nonisolated static let svgDefs = #"<svg width="0" height="0" style="position:absolute" aria-hidden="true"><defs><marker id="arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="6.5" markerHeight="6.5" orient="auto-start-reverse"><path d="M0 0L10 5L0 10z" fill="context-stroke"/></marker></defs></svg>"#

    /// Height reports come from a ResizeObserver; a value that flips back within
    /// 250 ms (a scrollbar/100vh feedback loop) is dropped, as in sideshow. No timers.
    nonisolated static let bridgeJS = """
        (function(){
        var post=function(m){m.__sideshow=true;window.webkit.messageHandlers.sideshow.postMessage(m);};
        window.sendPrompt=function(t){post({type:'send-prompt',text:String(t)});};
        window.openLink=function(u){post({type:'open-link',url:String(u)});};
        window.copyToClipboard=function(t){post({type:'copy',text:String(t)});};
        document.addEventListener('click',function(e){var a=e.target&&e.target.closest?e.target.closest('a[href]'):null;
        if(a&&/^https?:/.test(a.href)){e.preventDefault();window.openLink(a.href);}});
        var last=0,prev=0,lastT=0;
        function report(){var h=Math.ceil(document.body?document.body.scrollHeight:document.documentElement.scrollHeight);
        if(h<=0||h===last)return;var t=performance.now();if(h===prev&&t-lastT<250)return;prev=last;last=h;lastT=t;post({type:'resize',height:h});}
        if(document.readyState==='complete')report();else window.addEventListener('load',report);
        if(document.fonts&&document.fonts.ready)document.fonts.ready.then(report);
        if(window.ResizeObserver){var ro=new ResizeObserver(report);ro.observe(document.documentElement);if(document.body)ro.observe(document.body);}
        report();
        })();
        """
}
