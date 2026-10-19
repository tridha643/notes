import AppKit
import NotesCore

/// Light/dark colors for posts. Hex values follow sideshow's default (GitHub) theme so
/// html parts, mermaid and the native parts agree. Colors are dynamic NSColors, so
/// attributed strings built once off the main thread follow appearance changes.
enum Theme {
    struct Palette {
        let bg, panel, surface, text, muted, faint, border, border2: String
        let infoBg, infoText, infoBorder, okBg, okText, okBorder, warnBg, warnText, warnBorder, badBg, badText, badBorder: String
    }

    static let light = Palette(
        bg: "#f6f8fa", panel: "#eaeef2", surface: "#ffffff", text: "#1f2328", muted: "#59636e", faint: "#818b98",
        border: "#d1d9e0", border2: "#afb8c1",
        infoBg: "#ddf4ff", infoText: "#0969da", infoBorder: "#54aeff", okBg: "#dafbe1", okText: "#1a7f37", okBorder: "#4ac26b",
        warnBg: "#fff8c5", warnText: "#9a6700", warnBorder: "#d4a72c", badBg: "#ffebe9", badText: "#cf222e", badBorder: "#ff8182")
    static let dark = Palette(
        bg: "#0d1117", panel: "#161b22", surface: "#1c2128", text: "#e6edf3", muted: "#9198a1", faint: "#6e7681",
        border: "#30363d", border2: "#444c56",
        infoBg: "#132339", infoText: "#4493f8", infoBorder: "#54aeff", okBg: "#15291d", okText: "#3fb950", okBorder: "#4ac26b",
        warnBg: "#2a2214", warnText: "#d29922", warnBorder: "#d4a72c", badBg: "#301a1c", badText: "#ff7b72", badBorder: "#ff8182")

    static func isDark(_ appearance: NSAppearance) -> Bool {
        appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
    }

    static func hex(_ s: String, alpha: CGFloat = 1) -> NSColor {
        let v = UInt32(s.dropFirst(), radix: 16) ?? 0
        return NSColor(srgbRed: CGFloat(v >> 16 & 255) / 255, green: CGFloat(v >> 8 & 255) / 255, blue: CGFloat(v & 255) / 255, alpha: alpha)
    }

    static func dynamic(_ light: String, _ dark: String, alpha: CGFloat = 1) -> NSColor {
        let l = hex(light, alpha: alpha), d = hex(dark, alpha: alpha)
        return NSColor(name: nil) { isDark($0) ? d : l }
    }

    static func dynamic(_ pick: (Palette) -> String, alpha: CGFloat = 1) -> NSColor {
        dynamic(pick(light), pick(dark), alpha: alpha)
    }

    // Chrome
    static let pane = dynamic(\.bg)
    static let card = dynamic(\.surface)
    static let cardLine = dynamic(\.border)
    static let codeBackground = dynamic(\.bg)
    static let text = dynamic(\.text)
    static let muted = dynamic(\.muted)
    static let faint = dynamic(\.faint)
    static let accent = dynamic(\.infoText)
    static let chip = dynamic(\.panel)
    static let added = dynamic("#e6ffec", "#12261e")
    static let removed = dynamic("#ffebe9", "#2d1517")
    static let addedGutter = dynamic("#ccffd8", "#1b4721")
    static let removedGutter = dynamic("#ffd7d5", "#5a1e1e")
    static let hunk = dynamic(\.infoBg)
    static let fileHeader = dynamic(\.panel)

    // Code tokens (GitHub light/dark). A class like `keyword.control` uses its first word.
    static let tokens: [String: NSColor] = [
        "keyword": dynamic("#cf222e", "#ff7b72"), "storage": dynamic("#cf222e", "#ff7b72"),
        "string": dynamic("#0a3069", "#a5d6ff"), "regexp": dynamic("#0a3069", "#a5d6ff"),
        "comment": dynamic("#6e7781", "#8b949e"),
        "number": dynamic("#0550ae", "#79c0ff"), "constant": dynamic("#0550ae", "#79c0ff"),
        "boolean": dynamic("#0550ae", "#79c0ff"), "attribute": dynamic("#0550ae", "#79c0ff"),
        "property": dynamic("#0550ae", "#79c0ff"), "variable": dynamic("#953800", "#ffa657"),
        "function": dynamic("#8250df", "#d2a8ff"), "method": dynamic("#8250df", "#d2a8ff"),
        "type": dynamic("#953800", "#ffa657"), "class": dynamic("#953800", "#ffa657"),
        "tag": dynamic("#116329", "#7ee787"), "operator": dynamic("#cf222e", "#ff7b72"),
        "punctuation": dynamic("#1f2328", "#e6edf3"), "builtin": dynamic("#0550ae", "#79c0ff"),
    ]

    static func token(_ cls: String?) -> NSColor? {
        guard let cls, let word = cls.split(whereSeparator: { $0 == "." || $0 == " " }).first else { return nil }
        return tokens[String(word)]
    }

    // Terminals stay dark in both appearances, like sideshow's term vars.
    static let terminalBackground = hex("#0d1117")
    static let terminalBar = hex("#161b22")
    static let terminalText = hex("#e6edf3")
    static let terminalTitle = hex("#9198a1")
    static let sgr: [String: NSColor] = [
        "black": hex("#484f58"), "red": hex("#ff7b72"), "green": hex("#3fb950"), "yellow": hex("#d29922"),
        "blue": hex("#58a6ff"), "magenta": hex("#bc8cff"), "cyan": hex("#39c5cf"), "white": hex("#b1bac4"),
        "bright-black": hex("#6e7681"), "bright-red": hex("#ffa198"), "bright-green": hex("#56d364"), "bright-yellow": hex("#e3b341"),
        "bright-blue": hex("#79c0ff"), "bright-magenta": hex("#d2a8ff"), "bright-cyan": hex("#56d4dd"), "bright-white": hex("#ffffff"),
    ]

    /// pele's variables for the current appearance.
    static func pele(dark: Bool) -> PeleTheme {
        let p = dark ? Self.dark : Self.light
        return ["--pele-bg": p.surface, "--pele-fg": p.text, "--pele-muted": p.muted, "--pele-line": p.muted,
                "--pele-surface": p.panel, "--pele-surface-alt": p.bg, "--pele-border": p.border2, "--pele-accent": p.infoText,
                "--pele-font": Pele.fontFamily, "--pele-font-mono": Pele.monoFamily]
    }

    /// sideshow's html-part tokens (surfacePage tokenVars), pinned to one mode.
    static func htmlTokens(dark: Bool) -> String {
        let p = dark ? Self.dark : Self.light
        let vars: [(String, String)] = [
            ("color-background-primary", p.surface), ("color-background-secondary", p.panel), ("color-background-tertiary", p.bg),
            ("color-background-info", p.infoBg), ("color-background-success", p.okBg), ("color-background-warning", p.warnBg),
            ("color-background-danger", p.badBg), ("color-text-primary", p.text), ("color-text-secondary", p.muted),
            ("color-text-tertiary", p.faint), ("color-text-info", p.infoText), ("color-text-success", p.okText),
            ("color-text-warning", p.warnText), ("color-text-danger", p.badText), ("color-border-primary", p.border2),
            ("color-border-secondary", p.border), ("color-border-tertiary", p.border), ("color-border-info", p.infoBorder),
            ("color-border-success", p.okBorder), ("color-border-warning", p.warnBorder), ("color-border-danger", p.badBorder),
        ]
        return ":root{" + vars.map { "--\($0.0): \($0.1);" }.joined() + "}"
    }
}
