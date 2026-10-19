import Foundation
import Testing
@testable import NotesCore

@Test func headingsInlineStylesAndFences() {
    let (heading, _) = markdownSpans(line: "## Plan", inCode: false)
    #expect(heading.contains(MarkdownSpan(NSRange(location: 0, length: 7), .heading(2))))
    #expect(heading.contains(MarkdownSpan(NSRange(location: 0, length: 2), .marker)))
    #expect(markdownSpans(line: "#hashtag", inCode: false).spans.isEmpty)

    let (inline, after) = markdownSpans(line: "a **b** *c* `d *e*`", inCode: false)
    #expect(!after)
    #expect(inline.contains(MarkdownSpan(NSRange(location: 2, length: 5), .bold)))
    #expect(inline.contains(MarkdownSpan(NSRange(location: 8, length: 3), .italic)))
    #expect(inline.contains(MarkdownSpan(NSRange(location: 12, length: 7), .code)))
    // Emphasis inside inline code stays code.
    #expect(!inline.contains { $0.style == .italic && $0.range.location > 12 })

    let (fence, open) = markdownSpans(line: "```swift", inCode: false)
    #expect(open && fence == [MarkdownSpan(NSRange(location: 0, length: 8), .codeBlock)])
    let (code, stillOpen) = markdownSpans(line: "# not a heading", inCode: true)
    #expect(stillOpen && code == [MarkdownSpan(NSRange(location: 0, length: 15), .codeBlock)])
    #expect(markdownSpans(line: "```", inCode: true).inCode == false)
}
