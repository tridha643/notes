// Line-at-a-time markdown spans for the editor. Only code fences carry state across
// lines, so the editor restyles the edited paragraphs and keeps going only while
// the fence state after a line changed.

import Foundation

public enum MarkdownStyle: Equatable, Sendable {
    case heading(Int)
    /// Syntax characters (`#`, `**`, backticks) drawn dimmed.
    case marker
    case bold
    case italic
    case code
    /// A fence line or a line inside a fenced code block.
    case codeBlock
}

public struct MarkdownSpan: Equatable, Sendable {
    public var range: NSRange
    public var style: MarkdownStyle
    public init(_ range: NSRange, _ style: MarkdownStyle) {
        self.range = range
        self.style = style
    }
}

private let inlineRules: [(NSRegularExpression, MarkdownStyle, Int)] = [
    // (pattern, style, marker length). Code first so bold/italic inside code are skipped.
    (try! NSRegularExpression(pattern: "`[^`\\n]+`"), .code, 1),
    (try! NSRegularExpression(pattern: "\\*\\*[^*\\n]+\\*\\*|__[^_\\n]+__"), .bold, 2),
    (try! NSRegularExpression(pattern: "(?<![*\\w])\\*[^*\\s][^*\\n]*\\*(?!\\*)|(?<![_\\w])_[^_\\s][^_\\n]*_(?![_\\w])"), .italic, 1),
]

/// Spans for one line (no trailing newline). `inCode` is whether the line starts
/// inside a fenced block; the returned flag is the state after it.
public func markdownSpans(line: String, inCode: Bool) -> (spans: [MarkdownSpan], inCode: Bool) {
    let ns = line as NSString
    let whole = NSRange(location: 0, length: ns.length)
    if line.hasPrefix("```") || line.hasPrefix("~~~") {
        return ([MarkdownSpan(whole, .codeBlock)], !inCode)
    }
    if inCode { return ([MarkdownSpan(whole, .codeBlock)], true) }

    var spans: [MarkdownSpan] = []
    var hashes = 0
    while hashes < ns.length, hashes < 7, ns.character(at: hashes) == 0x23 { hashes += 1 }
    if (1...6).contains(hashes), hashes == ns.length || ns.character(at: hashes) == 0x20 {
        spans.append(MarkdownSpan(whole, .heading(hashes)))
        spans.append(MarkdownSpan(NSRange(location: 0, length: hashes), .marker))
    }
    var taken: [NSRange] = []
    for (regex, style, marker) in inlineRules {
        for match in regex.matches(in: line, range: whole) {
            let r = match.range
            if taken.contains(where: { NSIntersectionRange($0, r).length > 0 }) { continue }
            if style == .code { taken.append(r) }
            spans.append(MarkdownSpan(r, style))
            spans.append(MarkdownSpan(NSRange(location: r.location, length: marker), .marker))
            spans.append(MarkdownSpan(NSRange(location: NSMaxRange(r) - marker, length: marker), .marker))
        }
    }
    return (spans, false)
}
