import AppKit
import NotesCore

/// Code, diff and terminal parts as fixed-height lines of attributed text, built from
/// the daemon's `post-render` spans off the main thread. Every line is exactly
/// `CodeText.lineHeight` tall, so heights, gutters and line backgrounds are arithmetic
/// instead of text layout.
enum CodeText {
    static let fontSize: CGFloat = 12
    nonisolated(unsafe) static let font = NSFont.monospacedSystemFont(ofSize: fontSize, weight: .regular)
    nonisolated(unsafe) static let bold = NSFont.monospacedSystemFont(ofSize: fontSize, weight: .bold)
    static let lineHeight: CGFloat = 18
    /// One column's advance; monospaced, so a line's width is columns × this.
    static let advance: CGFloat = ("0" as NSString).size(withAttributes: [.font: font]).width
    nonisolated(unsafe) static let paragraph: NSParagraphStyle = {
        let p = NSMutableParagraphStyle()
        p.minimumLineHeight = lineHeight
        p.maximumLineHeight = lineHeight
        p.lineBreakMode = .byClipping
        return p
    }()
}

enum LineKind: Equatable, Sendable {
    case plain, added, removed, hunk, file, filler
}

/// One column of lines: the text view's content plus what the gutter and
/// backgrounds need, line for line.
struct CodePane {
    var text: NSAttributedString
    var kinds: [LineKind]
    /// Gutter label per line ("12", " 3  4 +", ...); all the same width.
    var gutter: [String]
    /// Source line and file per line for line-range anchors; nil for headers/fillers.
    var source: [(line: UInt32, file: String?)?]
    var columns: Int
    var lineCount: Int { kinds.count }
}

struct CodeModel {
    var panes: [CodePane]
    var title: String?
    var terminal: Bool
    var lineCount: Int { panes.map(\.lineCount).max() ?? 0 }
}

extension CodeModel {
    /// Maps one part's render output. A `native` or mismatched render falls back to
    /// the part's own text, unhighlighted.
    static func build(part: Part, render: PartRender?) -> CodeModel? {
        switch (part, render) {
        case (.code(let code, _, let title, let start, _), let r):
            let lines: [StyledLine]
            var first = start ?? 1
            if case .code(_, let s, let l)? = r { lines = l; first = s } else { lines = plainLines(code) }
            var b = PaneBuilder(terminal: false)
            let width = String(Int(first) + max(lines.count - 1, 0)).count
            for (i, line) in lines.enumerated() {
                let n = first + UInt32(i)
                b.add(line, kind: .plain, gutter: pad(String(n), width), source: (n, nil))
            }
            return CodeModel(panes: [b.pane], title: title, terminal: false)
        case (.terminal(let text, _, let title, _), let r):
            var b = PaneBuilder(terminal: true)
            let lines: [StyledLine]
            if case .terminal(let l)? = r { lines = l } else { lines = plainLines(text) }
            for line in lines { b.add(line, kind: .plain, gutter: "", source: nil) }
            return CodeModel(panes: [b.pane], title: title ?? "Terminal", terminal: true)
        case (.diff(_, _, let layout, _), .diff(let renderedLayout, let files)?):
            return (layout == .split || renderedLayout == .split) ? split(files) : unified(files)
        case (.diff(let patch, _, _, _), _):
            return CodeModel(panes: [{ var b = PaneBuilder(terminal: false); plainLines(patch ?? "").forEach { b.add($0, kind: .plain, gutter: "", source: nil) }; return b.pane }()], title: nil, terminal: false)
        default:
            return nil
        }
    }

    /// The line-range anchor for selected display lines `first...last` of one pane: source
    /// line numbers (new side, else old), skipping headers and fillers; nil if none.
    func lineAnchor(part: Int, version: UInt64, pane: Int, first: Int, last: Int) -> CommentAnchor? {
        guard panes.indices.contains(pane) else { return nil }
        let src = panes[pane].source
        let lo = max(0, first), hi = min(last, src.count - 1)
        guard lo <= hi else { return nil }
        let picked = (lo...hi).compactMap { src[$0] }
        guard let start = picked.map(\.line).min(), let end = picked.map(\.line).max() else { return nil }
        return .lineRange(part: UInt32(part), postVersion: version, startLine: start, endLine: end, file: picked.first?.file)
    }

    static func plainLines(_ text: String) -> [StyledLine] {
        var lines = text.split(separator: "\n", omittingEmptySubsequences: false).map { [Span(String($0))] }
        if lines.count > 1, lines.last?.first?.text.isEmpty == true { lines.removeLast() }
        return lines
    }

    static func pad(_ s: String, _ width: Int) -> String {
        String(repeating: " ", count: max(0, width - s.count)) + s
    }

    static func unified(_ files: [DiffFileRender]) -> CodeModel {
        let width = files.flatMap { $0.hunks.flatMap { $0.lines.flatMap { [$0.oldLine, $0.newLine] } } }.compactMap { $0 }.max().map { String($0).count } ?? 1
        var b = PaneBuilder(terminal: false)
        let blank = String(repeating: " ", count: width * 2 + 3)
        for file in files {
            b.add([Span(file.filename, "file"), Span("  +\(file.added) −\(file.removed)", "counts")], kind: .file, gutter: blank, source: nil)
            for hunk in file.hunks {
                b.add([Span(hunk.header, "hunk")], kind: .hunk, gutter: blank, source: nil)
                for line in hunk.lines {
                    let sign = line.kind == .added ? "+" : line.kind == .removed ? "−" : " "
                    let gutter = pad(line.oldLine.map(String.init) ?? "", width) + " " + pad(line.newLine.map(String.init) ?? "", width) + " " + sign
                    let n = line.newLine ?? line.oldLine
                    b.add(line.spans, kind: line.kind == .added ? .added : line.kind == .removed ? .removed : .plain,
                          gutter: gutter, source: n.map { ($0, file.filename) })
                }
            }
        }
        return CodeModel(panes: [b.pane], title: nil, terminal: false)
    }

    /// Old on the left, new on the right; a removed run pairs with the added run
    /// after it, and the shorter side is padded with filler lines.
    static func split(_ files: [DiffFileRender]) -> CodeModel {
        let width = files.flatMap { $0.hunks.flatMap { $0.lines.flatMap { [$0.oldLine, $0.newLine] } } }.compactMap { $0 }.max().map { String($0).count } ?? 1
        var left = PaneBuilder(terminal: false), right = PaneBuilder(terminal: false)
        let blank = String(repeating: " ", count: width + 2)
        func side(_ pane: inout PaneBuilder, _ line: DiffLine?, _ n: UInt32?, _ file: String) {
            guard let line else { return pane.add([], kind: .filler, gutter: blank, source: nil) }
            let sign = line.kind == .added ? "+" : line.kind == .removed ? "−" : " "
            pane.add(line.spans, kind: line.kind == .added ? .added : line.kind == .removed ? .removed : .plain,
                     gutter: pad(n.map(String.init) ?? "", width) + " " + sign, source: n.map { ($0, file) })
        }
        func row(_ l: DiffLine?, _ r: DiffLine?, file: String) {
            side(&left, l, l?.oldLine, file)
            side(&right, r, r?.newLine, file)
        }
        for file in files {
            left.add([Span(file.filename, "file")], kind: .file, gutter: blank, source: nil)
            right.add([Span("+\(file.added) −\(file.removed)", "counts")], kind: .file, gutter: blank, source: nil)
            for hunk in file.hunks {
                left.add([Span(hunk.header, "hunk")], kind: .hunk, gutter: blank, source: nil)
                right.add([Span(hunk.header, "hunk")], kind: .hunk, gutter: blank, source: nil)
                var removed: [DiffLine] = [], added: [DiffLine] = []
                func flush() {
                    for i in 0..<max(removed.count, added.count) {
                        row(i < removed.count ? removed[i] : nil, i < added.count ? added[i] : nil, file: file.filename)
                    }
                    removed = []
                    added = []
                }
                for line in hunk.lines {
                    switch line.kind {
                    case .removed:
                        if !added.isEmpty { flush() }
                        removed.append(line)
                    case .added: added.append(line)
                    case .context:
                        flush()
                        row(line, line, file: file.filename)
                    }
                }
                flush()
            }
        }
        return CodeModel(panes: [left.pane, right.pane], title: nil, terminal: false)
    }
}

/// Accumulates lines into one attributed string.
struct PaneBuilder {
    let terminal: Bool
    private let text = NSMutableAttributedString()
    private var kinds: [LineKind] = []
    private var gutter: [String] = []
    private var source: [(line: UInt32, file: String?)?] = []
    private var columns = 0

    init(terminal: Bool) { self.terminal = terminal }

    var pane: CodePane { CodePane(text: text, kinds: kinds, gutter: gutter, source: source, columns: columns) }

    mutating func add(_ spans: StyledLine, kind: LineKind, gutter label: String, source line: (line: UInt32, file: String?)?) {
        if !kinds.isEmpty { text.append(NSAttributedString(string: "\n", attributes: CodeText.base(terminal))) }
        var count = 0
        for span in spans {
            let s = span.text.replacingOccurrences(of: "\t", with: "    ")
            count += s.count
            text.append(NSAttributedString(string: s, attributes: CodeText.attributes(span.cls, terminal: terminal)))
        }
        columns = max(columns, count)
        kinds.append(kind)
        gutter.append(label)
        source.append(line)
    }
}

extension CodeText {
    static func base(_ terminal: Bool) -> [NSAttributedString.Key: Any] {
        [.font: font, .foregroundColor: terminal ? Theme.terminalText : Theme.text, .paragraphStyle: paragraph]
    }

    /// Attributes for one span: token classes for code and diffs, `fg-red bold`
    /// style lists for terminals, plus the synthetic `file`/`hunk`/`counts` classes.
    static func attributes(_ cls: String?, terminal: Bool) -> [NSAttributedString.Key: Any] {
        var a = base(terminal)
        guard let cls else { return a }
        if terminal {
            for word in cls.split(separator: " ") {
                switch word {
                case "bold": a[.font] = bold
                case "dim": a[.foregroundColor] = (a[.foregroundColor] as? NSColor)?.withAlphaComponent(0.6)
                case "italic": a[.obliqueness] = 0.15
                case "underline": a[.underlineStyle] = NSUnderlineStyle.single.rawValue
                default:
                    if word.hasPrefix("fg-"), let c = Theme.sgr[String(word.dropFirst(3))] { a[.foregroundColor] = c }
                    if word.hasPrefix("bg-"), let c = Theme.sgr[String(word.dropFirst(3))] { a[.backgroundColor] = c.withAlphaComponent(0.35) }
                }
            }
            return a
        }
        switch cls {
        case "file": a[.font] = bold
        case "hunk": a[.foregroundColor] = Theme.accent
        case "counts": a[.foregroundColor] = Theme.muted
        default: if let c = Theme.token(cls) { a[.foregroundColor] = c }
        }
        return a
    }
}
