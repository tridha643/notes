// Mermaid → SVG with pele 1.6.0 in one JavaScriptCore context. The context is made
// on the first diagram, evaluates pele.global.js once and stays warm; JSContext isn't
// thread-safe, so every call runs on one serial queue. Text widths come from Core
// Text, measured with the same fonts CoreSVG draws (".AppleSystemUIFont", Menlo), so
// labels fit their boxes exactly. pele colors everything through CSS variables;
// `resolvePeleSVG` substitutes the theme's values so NSImage's SVG renderer, which
// doesn't do custom properties, draws the right colors.

import CoreText
import Foundation
import JavaScriptCore

public struct PeleDiagram: Sendable, Equatable {
    /// SVG with pele's CSS variables still in place; resolve per theme.
    public var svg: String
    public var width: Double
    public var height: Double
    public var type: String
}

public final class Pele: @unchecked Sendable {
    public static let shared = Pele()

    public static let fontFamily = ".AppleSystemUIFont"
    public static let monoFamily = "Menlo"

    private let queue = DispatchQueue(label: "dev.notes.pele", qos: .userInitiated)
    private var context: JSContext?
    private var renderFunction: JSValue?
    private var fonts: [Int: CTFont] = [:]

    /// Renders on pele's queue and waits; call it off the main thread.
    /// `maxWidth` lets pele turn wide flowcharts vertical in narrow panes.
    public func render(_ source: String, maxWidth: Double? = nil, fontSize: Double = 14) throws -> PeleDiagram {
        try queue.sync {
            let (context, render) = try warm()
            var options: [String: Any] = ["fontFamily": Self.fontFamily, "fontFamilyMono": Self.monoFamily, "fontSize": fontSize,
                                          "measurer": context.objectForKeyedSubscript("__measurer") as Any]
            if let maxWidth { options["maxWidth"] = maxWidth }
            let result = render.call(withArguments: [source, options])
            if let exception = context.exception {
                context.exception = nil
                throw DaemonError.protocolError("mermaid: \(exception.toString() ?? "render failed")")
            }
            guard let result, let svg = result.forProperty("svg").toString() else {
                throw DaemonError.protocolError("mermaid: no svg")
            }
            return PeleDiagram(svg: svg, width: result.forProperty("width").toDouble(),
                               height: result.forProperty("height").toDouble(),
                               type: result.forProperty("type").toString() ?? "")
        }
    }

    private func warm() throws -> (JSContext, JSValue) {
        if let context, let renderFunction { return (context, renderFunction) }
        guard let url = Self.scriptURL, let script = try? String(contentsOf: url, encoding: .utf8) else {
            throw DaemonError.protocolError("pele.global.js missing from the app bundle")
        }
        let context = JSContext()!
        let width: @convention(block) (String, Double, Int) -> Double = { [unowned self] text, size, style in
            measure(text, size: size, style: style)
        }
        let measurer = JSValue(newObjectIn: context)!
        measurer.setObject(width, forKeyedSubscript: "width" as NSString)
        context.setObject(measurer, forKeyedSubscript: "__measurer" as NSString)
        context.evaluateScript(script, withSourceURL: url)
        guard context.exception == nil, let render = context.objectForKeyedSubscript("pele")?.forProperty("render"), render.isObject else {
            throw DaemonError.protocolError("pele failed to load: \(context.exception?.toString() ?? "no pele.render")")
        }
        self.context = context
        renderFunction = render
        return (context, render)
    }

    /// The app ships it in Contents/Resources (build-app.sh); `swift test` uses the package bundle.
    static var scriptURL: URL? {
        Bundle.main.url(forResource: "pele.global", withExtension: "js") ?? Bundle.module.url(forResource: "pele.global", withExtension: "js")
    }

    /// pele's TextMeasurer: style bits are 1 bold, 2 italic, 4 mono. Runs on `queue`.
    private func measure(_ text: String, size: Double, style: Int) -> Double {
        let key = Int(size * 100) << 3 | (style & 7)
        let font: CTFont
        if let cached = fonts[key] {
            font = cached
        } else {
            var base = style & 4 != 0
                ? CTFontCreateWithName(Self.monoFamily as CFString, size, nil)
                : CTFontCreateUIFontForLanguage(.system, size, nil)!
            var traits: CTFontSymbolicTraits = []
            if style & 1 != 0 { traits.insert(.traitBold) }
            if style & 2 != 0 { traits.insert(.traitItalic) }
            if !traits.isEmpty, let styled = CTFontCreateCopyWithSymbolicTraits(base, size, nil, traits, traits) { base = styled }
            fonts[key] = base
            font = base
        }
        let line = CTLineCreateWithAttributedString(NSAttributedString(string: text, attributes: [.init(kCTFontAttributeName as String): font]))
        return CTLineGetTypographicBounds(line, nil, nil, nil)
    }
}

/// Theme values for pele's variables (`--pele-bg`, `--pele-fg`, ...), as hex colors.
public typealias PeleTheme = [String: String]

/// Replaces every `var(--…)` with the theme's value (or pele's fallback), evaluates
/// `color-mix(in srgb, …)`, and turns `style="rx:4px"` into an `rx` attribute: the
/// three CSS features CoreSVG doesn't implement but pele's output uses.
public func resolvePeleSVG(_ svg: String, theme: PeleTheme) -> String {
    var s = svg
    // Root aliases: --_bg:var(--pele-bg,#fff) → the theme's --pele-bg.
    var alias: [String: String] = [:]
    for m in svg.matches(of: #/(--_[a-z0-9]+):var\((--pele-[a-z0-9-]+),([^;)"]*)\)/#) {
        alias[String(m.1)] = theme[String(m.2)] ?? String(m.3)
    }
    s = s.replacing(#/--_[a-z0-9]+:[^;"]*;?/#, with: "")
    // Innermost var() first; fallbacks may hold another var().
    while s.contains("var(") {
        let next = s.replacing(#/var\((--[a-z0-9_-]+)(?:,([^()]*))?\)/#) { m in
            alias[String(m.1)] ?? theme[String(m.1)] ?? m.2.map(String.init) ?? ""
        }
        if next == s { break }
        s = next
    }
    s = s.replacing(#/color-mix\(in srgb,\s*(#[0-9a-fA-F]{6})\s+([0-9.]+)%,\s*(#[0-9a-fA-F]{6})\)/#) { m in
        mixHex(String(m.1), String(m.3), Double(m.2)! / 100)
    }
    s = s.replacing(#/style="rx:([0-9.]+)px"/#) { m in #"rx="\#(m.1)""# }
    return s
}

/// `a` weighted `t`, `b` weighted 1-t, like color-mix(in srgb, a t%, b).
func mixHex(_ a: String, _ b: String, _ t: Double) -> String {
    func rgb(_ h: String) -> [Double] {
        let v = UInt32(h.dropFirst(), radix: 16) ?? 0
        return [Double(v >> 16 & 255), Double(v >> 8 & 255), Double(v & 255)]
    }
    let (x, y) = (rgb(a), rgb(b))
    return "#" + (0..<3).map { String(format: "%02x", Int((x[$0] * t + y[$0] * (1 - t)).rounded())) }.joined()
}
