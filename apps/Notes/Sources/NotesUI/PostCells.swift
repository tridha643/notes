import AppKit
import NotesCore
import WebKit

/// Card geometry shared by heights and cell layout: a centered column of cards.
enum Layout {
    static let maxCard: CGFloat = 820
    static let margin: CGFloat = 24
    static let pad: CGFloat = 16
    static let radius: CGFloat = 12

    static func card(_ width: CGFloat) -> (x: CGFloat, w: CGFloat) {
        let w = min(maxCard, max(200, width - margin * 2))
        return ((width - w) / 2, w)
    }

    static func content(_ width: CGFloat) -> (x: CGFloat, w: CGFloat) {
        let c = card(width)
        return (c.x + pad, c.w - pad * 2)
    }
}

/// Where a row sits in its post's card.
enum CardEdge { case none, top, middle, bottom }

/// Draws the card segment behind a row with one layer: no drawRect, no selection.
final class CardRowView: NSTableRowView {
    private let fill = CALayer()
    var edge = CardEdge.none { didSet { if edge != oldValue { needsLayout = true } } }
    static let footerGap: CGFloat = 20

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layer?.masksToBounds = true
        fill.cornerCurve = .continuous
        layer?.addSublayer(fill)
    }

    required init?(coder: NSCoder) { fatalError() }

    override func drawSelection(in dirtyRect: NSRect) {}
    override func drawBackground(in dirtyRect: NSRect) {}

    // NSTableRowView is already flipped; overriding isFlipped in Swift costs a thunk on
    // every coordinate conversion (13% of scroll time when it was here).

    override func layout() {
        super.layout()
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        let c = Layout.card(bounds.width)
        let r = Layout.radius
        fill.isHidden = edge == .none
        fill.cornerRadius = edge == .middle ? 0 : r
        // A fully rounded layer pushed past the row edge that joins the next segment, and
        // clipped by the row: square on that side without relying on maskedCorners.
        let h = bounds.height - (edge == .bottom ? Self.footerGap : 0)
        switch edge {
        case .top: fill.frame = NSRect(x: c.x, y: 0, width: c.w, height: h + r)
        case .bottom: fill.frame = NSRect(x: c.x, y: -r, width: c.w, height: h + r)
        default: fill.frame = NSRect(x: c.x, y: 0, width: c.w, height: h)
        }
        effectiveAppearance.performAsCurrentDrawingAppearance { fill.backgroundColor = Theme.card.cgColor }
        CATransaction.commit()
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        needsLayout = true
    }
}

/// Base of every cell: flipped manual layout, and option-click point anchors
/// (hit-testing claims option-clicks before subviews such as web views see them).
class PostCell: NSView {
    weak var owner: PostsView?
    var row: Row?
    var anchorable: Bool { false }
    private var pinLayers: [CALayer] = []

    override var isFlipped: Bool { true }

    init(_ id: String) {
        super.init(frame: .zero)
        identifier = NSUserInterfaceItemIdentifier(id)
        wantsLayer = true
    }

    required init?(coder: NSCoder) { fatalError() }

    /// Space below the box; parts leave `PostsView.gap` before the next row.
    var bottomGap: CGFloat { 0 }

    /// The part's box inside the card.
    var box: NSRect {
        let c = Layout.content(bounds.width)
        return NSRect(x: c.x, y: 0, width: c.w, height: max(0, bounds.height - bottomGap))
    }

    override func hitTest(_ point: NSPoint) -> NSView? {
        if anchorable, let e = NSApp.currentEvent, e.type == .leftMouseDown, e.modifierFlags.contains(.option),
           box.contains(convert(point, from: superview)) {
            return self
        }
        return super.hitTest(point)
    }

    override func mouseDown(with event: NSEvent) {
        guard anchorable, event.modifierFlags.contains(.option), let row else { return super.mouseDown(with: event) }
        let p = convert(event.locationInWindow, from: nil), b = box
        owner?.pointAnchor(row, x: min(max((p.x - b.minX) / b.width, 0), 1), y: min(max((p.y - b.minY) / b.height, 0), 1))
    }

    /// Pins for comments anchored to a point or rect on this part (fractions of the box).
    func setPins(_ points: [CGPoint]) {
        while pinLayers.count > points.count { pinLayers.removeLast().removeFromSuperlayer() }
        while pinLayers.count < points.count {
            let pin = CALayer()
            pin.cornerRadius = 6
            pin.borderWidth = 2
            pinLayers.append(pin)
            layer?.addSublayer(pin)
        }
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        let b = box
        for (pin, p) in zip(pinLayers, points) {
            pin.frame = NSRect(x: b.minX + p.x * b.width - 6, y: b.minY + p.y * b.height - 6, width: 12, height: 12)
            pin.zPosition = 10
            effectiveAppearance.performAsCurrentDrawingAppearance {
                pin.backgroundColor = Theme.accent.cgColor
                pin.borderColor = Theme.card.cgColor
            }
        }
        CATransaction.commit()
    }
    var pins: [CGPoint] = [] { didSet { needsLayout = true } }

    override func layout() {
        super.layout()
        setPins(pins)
    }
}

@MainActor func label(_ text: String = "", size: CGFloat = 13, weight: NSFont.Weight = .regular, color: NSColor = Theme.text) -> NSTextField {
    let l = NSTextField(labelWithString: text)
    l.font = .systemFont(ofSize: size, weight: weight)
    l.textColor = color
    l.lineBreakMode = .byTruncatingTail
    l.cell?.usesSingleLineMode = true
    return l
}

/// "3 min. ago"; "just now" within a minute, which also absorbs daemon/app clock skew.
func relativeTime(_ secs: Int64) -> String {
    let date = Date(timeIntervalSince1970: TimeInterval(secs))
    if abs(date.timeIntervalSinceNow) < 60 { return "just now" }
    let f = RelativeDateTimeFormatter()
    f.unitsStyle = .abbreviated
    return f.localizedString(for: date, relativeTo: Date())
}

// MARK: Header

final class HeaderCell: PostCell {
    let title = label(size: 15, weight: .semibold)
    let meta = label(size: 11, color: Theme.faint)
    let versions = NSPopUpButton(frame: .zero, pullsDown: false)
    let comment = NSButton()
    let delete = NSButton()
    var postID = ""

    init() {
        super.init("header")
        versions.controlSize = .small
        versions.font = .systemFont(ofSize: 11)
        versions.isBordered = false
        versions.target = self
        versions.action = #selector(pickVersion)
        for (b, symbol, tip, action) in [(comment, "text.bubble", "Comment on this post", #selector(reply)),
                                         (delete, "trash", "Delete post", #selector(remove))] {
            b.image = NSImage(systemSymbolName: symbol, accessibilityDescription: tip)
            b.isBordered = false
            b.toolTip = tip
            b.target = self
            b.action = action
            b.contentTintColor = Theme.muted
        }
        [title, meta, versions, comment, delete].forEach(addSubview)
    }

    required init?(coder: NSCoder) { fatalError() }

    func configure(_ post: LoadedPost) {
        postID = post.id
        title.stringValue = post.post.title
        versions.removeAllItems()
        versions.addItems(withTitles: (1...max(post.latest, 1)).reversed().map { "v\($0)" })
        versions.selectItem(withTitle: "v\(post.post.version)")
        versions.isEnabled = post.latest > 1
        meta.stringValue = (post.post.version < post.latest ? "older version · " : "") + relativeTime(post.post.updatedAt)
        needsLayout = true
    }

    override func layout() {
        super.layout()
        let b = box
        let y: CGFloat = 16
        delete.frame = NSRect(x: b.maxX - 22, y: y - 1, width: 22, height: 22)
        comment.frame = NSRect(x: b.maxX - 50, y: y - 1, width: 22, height: 22)
        meta.sizeToFit()
        meta.frame = NSRect(x: comment.frame.minX - 8 - meta.frame.width, y: y + 3, width: meta.frame.width, height: 16)
        let vw: CGFloat = versions.numberOfItems > 9 ? 58 : 48
        versions.frame = NSRect(x: meta.frame.minX - 4 - vw, y: y, width: vw, height: 20)
        title.frame = NSRect(x: b.minX, y: y, width: max(0, versions.frame.minX - 8 - b.minX), height: 20)
    }

    @objc func pickVersion() {
        guard let v = UInt64(versions.titleOfSelectedItem?.dropFirst() ?? "") else { return }
        owner?.showVersion(postID, v)
    }

    @objc func reply() { owner?.composer.target(post: postID) }

    @objc func remove() { owner?.deletePost(postID) }
}

// MARK: Code, diff, terminal

/// A horizontal-only scroll view that hands vertical scrolling to the timeline.
final class HScrollView: NSScrollView {
    /// Always overlay: a legacy scroller ("Show scroll bars: Always", or a mouse attached)
    /// would take 16 pt from the fixed-height pane and clip its last line. AppKit resets
    /// scrollerStyle when that preference changes, so pin it here.
    override var scrollerStyle: NSScroller.Style {
        get { .overlay }
        set { super.scrollerStyle = .overlay }
    }

    override func scrollWheel(with event: NSEvent) {
        let fits = (documentView?.frame.width ?? 0) <= contentView.bounds.width
        if fits || abs(event.scrollingDeltaY) > abs(event.scrollingDeltaX) { nextResponder?.scrollWheel(with: event) }
        else { super.scrollWheel(with: event) }
    }
}

/// Full-width line backgrounds under the text view: a layer background plus one
/// sublayer per run of same-kind colored lines, so nothing is drawn on the CPU.
final class CodeDocView: NSView {
    var terminal = false
    private var runs: [(kind: LineKind, range: Range<Int>)] = []
    private var runLayers: [CALayer] = []
    override var isFlipped: Bool { true }

    init() {
        super.init(frame: .zero)
        wantsLayer = true
    }

    required init?(coder: NSCoder) { fatalError() }

    var kinds: [LineKind] = [] {
        didSet {
            runs = []
            for (i, k) in kinds.enumerated() where k != .plain {
                if let last = runs.last, last.kind == k, last.range.upperBound == i { runs[runs.count - 1].range = last.range.lowerBound..<(i + 1) }
                else { runs.append((k, i..<(i + 1))) }
            }
            while runLayers.count > runs.count { runLayers.removeLast().removeFromSuperlayer() }
            while runLayers.count < runs.count { let l = CALayer(); runLayers.append(l); layer?.addSublayer(l) }
            needsLayout = true
        }
    }

    static func color(_ kind: LineKind) -> NSColor {
        switch kind {
        case .added: Theme.added
        case .removed: Theme.removed
        case .hunk: Theme.hunk
        case .file: Theme.fileHeader
        case .filler: Theme.chip.withAlphaComponent(0.5)
        case .plain: .clear
        }
    }

    override func layout() {
        super.layout()
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        effectiveAppearance.performAsCurrentDrawingAppearance {
            layer?.backgroundColor = (terminal ? Theme.terminalBackground : Theme.codeBackground).cgColor
            for (l, run) in zip(runLayers, runs) {
                l.backgroundColor = Self.color(run.kind).cgColor
                l.frame = NSRect(x: 0, y: CodePaneView.inset + CGFloat(run.range.lowerBound) * CodeText.lineHeight,
                                 width: bounds.width, height: CGFloat(run.range.count) * CodeText.lineHeight)
            }
        }
        CATransaction.commit()
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        needsLayout = true
    }
}

/// Line numbers, drawn only for the dirty lines.
final class GutterView: NSView {
    var pane: CodePane?
    var terminal = false
    override var isFlipped: Bool { true }
    static let attributes: [NSAttributedString.Key: Any] = [
        .font: CodeText.font, .foregroundColor: Theme.faint, .paragraphStyle: CodeText.paragraph,
    ]

    override func draw(_ dirty: NSRect) {
        (terminal ? Theme.terminalBackground : Theme.codeBackground).setFill()
        dirty.fill()
        guard let pane else { return }
        let lh = CodeText.lineHeight
        let first = max(0, Int((dirty.minY - CodePaneView.inset) / lh)), last = min(pane.lineCount, Int((dirty.maxY - CodePaneView.inset) / lh) + 1)
        guard first < last else { return }
        for i in first..<last {
            let y = CodePaneView.inset + CGFloat(i) * lh
            switch pane.kinds[i] {
            case .added: Theme.addedGutter.setFill(); NSRect(x: 0, y: y, width: bounds.width, height: lh).fill()
            case .removed: Theme.removedGutter.setFill(); NSRect(x: 0, y: y, width: bounds.width, height: lh).fill()
            case .hunk: Theme.hunk.setFill(); NSRect(x: 0, y: y, width: bounds.width, height: lh).fill()
            case .file: Theme.fileHeader.setFill(); NSRect(x: 0, y: y, width: bounds.width, height: lh).fill()
            default: break
            }
            (pane.gutter[i] as NSString).draw(in: NSRect(x: 8, y: y, width: bounds.width - 12, height: lh), withAttributes: Self.attributes)
        }
    }
}

/// One column of code: gutter, then a horizontally scrolling TextKit 2 view over line backgrounds.
final class CodePaneView: NSView, NSTextViewDelegate {
    static let inset: CGFloat = 8
    let gutter = GutterView()
    let scroll = HScrollView()
    let doc = CodeDocView()
    let text = NSTextView(usingTextLayoutManager: true)
    var pane: CodePane?
    var onSelect: ((Int, Int) -> Void)?
    override var isFlipped: Bool { true }

    init() {
        super.init(frame: .zero)
        text.isEditable = false
        text.isSelectable = true
        text.drawsBackground = false
        text.isRichText = true
        text.textContainerInset = NSSize(width: 0, height: Self.inset)
        text.textContainer?.lineFragmentPadding = 10
        text.textContainer?.widthTracksTextView = false
        text.isHorizontallyResizable = false
        text.isVerticallyResizable = false
        text.delegate = self
        doc.addSubview(text)
        scroll.documentView = doc
        scroll.drawsBackground = false
        scroll.hasHorizontalScroller = true
        scroll.hasVerticalScroller = false
        scroll.autohidesScrollers = true
        scroll.scrollerStyle = .overlay
        scroll.verticalScrollElasticity = .none
        addSubview(gutter)
        addSubview(scroll)
    }

    required init?(coder: NSCoder) { fatalError() }

    func set(_ pane: CodePane, terminal: Bool) {
        self.pane = pane
        gutter.pane = pane
        gutter.terminal = terminal
        doc.terminal = terminal
        doc.kinds = pane.kinds
        text.textStorage?.setAttributedString(pane.text)
        text.setSelectedRange(NSRange(location: 0, length: 0))
        gutter.needsDisplay = true
        needsLayout = true
    }

    var gutterWidth: CGFloat {
        guard let w = pane?.gutter.first?.count, w > 0 else { return 0 }
        return CGFloat(w) * CodeText.advance + 16
    }

    override func layout() {
        super.layout()
        let g = gutterWidth
        gutter.frame = NSRect(x: 0, y: 0, width: g, height: bounds.height)
        scroll.frame = NSRect(x: g, y: 0, width: bounds.width - g, height: bounds.height)
        // Write only on change: re-setting the container or document size every pass
        // re-tiles the scroll view and re-flashes its scroller, which dirties layout
        // again and keeps a scroll animation (and the CPU) running forever.
        let contentW = max(CGFloat(pane?.columns ?? 0) * CodeText.advance + 32, scroll.contentSize.width)
        let docFrame = NSRect(x: 0, y: 0, width: contentW, height: bounds.height)
        if doc.frame != docFrame { doc.frame = docFrame }
        if text.frame != doc.bounds { text.frame = doc.bounds }
        let size = NSSize(width: contentW, height: bounds.height)
        if text.textContainer?.size != size { text.textContainer?.size = size }
    }

    func textViewDidChangeSelection(_ notification: Notification) {
        let r = text.selectedRange()
        guard r.length > 0 else { return }
        let s = text.string as NSString
        func line(_ at: Int) -> Int {
            var n = 0, i = 0
            while i < at, case let found = s.range(of: "\n", options: .literal, range: NSRange(location: i, length: at - i)), found.location != NSNotFound {
                n += 1
                i = found.location + 1
            }
            return n
        }
        let first = line(r.location)
        onSelect?(first, first + line(NSMaxRange(r) - 1) - line(r.location))
    }
}

final class CodeCell: PostCell {
    static let titleHeight: CGFloat = 28
    let titleLabel = label(size: 12, weight: .medium)
    let bar = NSView()
    var panes: [CodePaneView] = []
    var model: CodeModel?
    override var anchorable: Bool { true }
    override var bottomGap: CGFloat { PostsView.gap }

    init() {
        super.init("code")
        bar.wantsLayer = true
        bar.addSubview(titleLabel)
        addSubview(bar)
    }

    required init?(coder: NSCoder) { fatalError() }

    func configure(_ model: CodeModel, onSelect: @escaping (Int, Int, Int) -> Void) {
        self.model = model
        while panes.count < model.panes.count { let p = CodePaneView(); panes.append(p); addSubview(p) }
        for (i, p) in panes.enumerated() {
            p.isHidden = i >= model.panes.count
            if i < model.panes.count {
                p.set(model.panes[i], terminal: model.terminal)
                p.onSelect = { onSelect(i, $0, $1) }
            }
        }
        bar.isHidden = model.title == nil
        titleLabel.stringValue = model.title ?? ""
        titleLabel.textColor = model.terminal ? Theme.terminalTitle : Theme.muted
        needsLayout = true
    }

    override func layout() {
        super.layout()
        let b = box
        var y = b.minY
        if !bar.isHidden {
            bar.frame = NSRect(x: b.minX, y: y, width: b.width, height: Self.titleHeight)
            effectiveAppearance.performAsCurrentDrawingAppearance {
                bar.layer?.backgroundColor = (model?.terminal == true ? Theme.terminalBar : Theme.chip).cgColor
            }
            bar.layer?.cornerRadius = 8
            bar.layer?.maskedCorners = [.layerMinXMaxYCorner, .layerMaxXMaxYCorner]
            titleLabel.frame = NSRect(x: 10, y: 6, width: b.width - 20, height: 16)
            y += Self.titleHeight
        }
        let visible = panes.filter { !$0.isHidden }
        guard !visible.isEmpty else { return }
        let w = (b.width - CGFloat(visible.count - 1)) / CGFloat(visible.count)
        for (i, p) in visible.enumerated() {
            p.frame = NSRect(x: b.minX + CGFloat(i) * (w + 1), y: y, width: w, height: b.maxY - y)
        }
    }
}

// MARK: Markdown

final class MarkdownCell: PostCell {
    let text = NSTextView(usingTextLayoutManager: true)
    override var anchorable: Bool { true }
    override var bottomGap: CGFloat { PostsView.gap }

    init() {
        super.init("markdown")
        text.isEditable = false
        text.isSelectable = true
        text.drawsBackground = false
        text.textContainerInset = .zero
        text.textContainer?.lineFragmentPadding = 0
        text.isVerticallyResizable = false
        addSubview(text)
    }

    required init?(coder: NSCoder) { fatalError() }

    func configure(_ s: NSAttributedString) {
        text.textStorage?.setAttributedString(s)
        needsLayout = true
    }

    override func layout() {
        super.layout()
        if text.frame != box { text.frame = box }
        let size = NSSize(width: box.width, height: .greatestFiniteMagnitude)
        if text.textContainer?.size != size { text.textContainer?.size = size }
    }
}

// MARK: Mermaid and images

/// A layer showing a CGImage, aspect-fit, plus an optional caption or error text.
final class ImageCell: PostCell {
    let picture = CALayer()
    let caption = NSTextField(wrappingLabelWithString: "")
    var imageSize = CGSize.zero
    var onOpen: (() -> Void)?
    override var anchorable: Bool { true }
    override var bottomGap: CGFloat { PostsView.gap }

    override init(_ id: String) {
        super.init(id)
        picture.contentsGravity = .resizeAspect
        layer?.addSublayer(picture)
        caption.font = .systemFont(ofSize: 12)
        caption.textColor = Theme.muted
        caption.isSelectable = true
        addSubview(caption)
    }

    required init?(coder: NSCoder) { fatalError() }

    func configure(image: CGImage?, size: CGSize, caption text: String?, alt: String?) {
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        picture.contents = image
        CATransaction.commit()
        imageSize = size
        caption.stringValue = text ?? ""
        caption.isHidden = text?.isEmpty ?? true
        setAccessibilityLabel(alt ?? text)
        toolTip = alt
        needsLayout = true
    }

    override func layout() {
        super.layout()
        let b = box
        let capH: CGFloat = caption.isHidden ? 0 : ImageCell.captionHeight
        let fit = imageSize.width > 0 ? min(1, b.width / imageSize.width) : 1
        let w = imageSize.width * fit, h = max(0, min(imageSize.height * fit, b.height - capH - 8))
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        picture.frame = NSRect(x: b.minX + (b.width - w) / 2, y: b.minY + 4, width: w, height: h)
        CATransaction.commit()
        caption.frame = NSRect(x: b.minX, y: b.minY + 8 + h, width: b.width, height: capH)
    }

    static let captionHeight: CGFloat = 20

    override func mouseDown(with event: NSEvent) {
        if event.clickCount == 2, !event.modifierFlags.contains(.option) { onOpen?() } else { super.mouseDown(with: event) }
    }
}

// MARK: JSON

final class JSONCell: PostCell, NSOutlineViewDataSource, NSOutlineViewDelegate {
    static let rowHeight: CGFloat = 20
    let outline = NSOutlineView()
    let scroll = PassScrollView()
    var root: JSONNode?
    var onResize: ((Int) -> Void)?
    var expanded: Set<ObjectIdentifier> = []
    var onExpandedChange: ((Set<ObjectIdentifier>) -> Void)?
    private var restoring = false
    override var anchorable: Bool { true }
    override var bottomGap: CGFloat { PostsView.gap }

    init() {
        super.init("json")
        let column = NSTableColumn(identifier: .init("v"))
        outline.addTableColumn(column)
        outline.outlineTableColumn = column
        outline.headerView = nil
        outline.rowHeight = Self.rowHeight
        outline.intercellSpacing = .zero
        outline.backgroundColor = .clear
        outline.indentationPerLevel = 14
        outline.style = .plain
        outline.selectionHighlightStyle = .none
        outline.dataSource = self
        outline.delegate = self
        outline.columnAutoresizingStyle = .uniformColumnAutoresizingStyle
        scroll.documentView = outline
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = false
        addSubview(scroll)
    }

    required init?(coder: NSCoder) { fatalError() }

    /// Top-level rows: a container's entries, or the primitive itself.
    var top: [JSONNode] { root.map { $0.isExpandable ? $0.children : [$0] } ?? [] }

    func configure(_ node: JSONNode, expanded: Set<ObjectIdentifier>) {
        root = node
        self.expanded = expanded
        restoring = true
        outline.reloadData()
        func reopen(_ nodes: [JSONNode]) {
            for n in nodes where expanded.contains(ObjectIdentifier(n)) {
                outline.expandItem(n)
                reopen(n.children)
            }
        }
        reopen(top)
        restoring = false
        needsLayout = true
    }

    override func layout() {
        super.layout()
        scroll.frame = box.insetBy(dx: 0, dy: 6)
        outline.tableColumns.first?.width = box.width
    }

    func outlineView(_ o: NSOutlineView, numberOfChildrenOfItem item: Any?) -> Int {
        (item as? JSONNode)?.children.count ?? top.count
    }

    func outlineView(_ o: NSOutlineView, child index: Int, ofItem item: Any?) -> Any {
        (item as? JSONNode)?.children[index] ?? top[index]
    }

    func outlineView(_ o: NSOutlineView, isItemExpandable item: Any) -> Bool { (item as? JSONNode)?.isExpandable ?? false }

    func outlineView(_ o: NSOutlineView, viewFor tableColumn: NSTableColumn?, item: Any) -> NSView? {
        let cell = o.makeView(withIdentifier: .init("jv"), owner: nil) as? NSTableCellView ?? {
            let c = NSTableCellView()
            c.identifier = .init("jv")
            let l = label(size: 12)
            l.translatesAutoresizingMaskIntoConstraints = false
            c.addSubview(l)
            c.textField = l
            NSLayoutConstraint.activate([l.leadingAnchor.constraint(equalTo: c.leadingAnchor), l.trailingAnchor.constraint(equalTo: c.trailingAnchor),
                                         l.centerYAnchor.constraint(equalTo: c.centerYAnchor)])
            return c
        }()
        guard let node = item as? JSONNode else { return cell }
        cell.textField?.attributedStringValue = Self.text(node)
        return cell
    }

    static func text(_ node: JSONNode) -> NSAttributedString {
        let s = NSMutableAttributedString()
        let mono = NSFont.monospacedSystemFont(ofSize: 12, weight: .regular)
        if let key = node.key {
            s.append(NSAttributedString(string: node.isRange ? key : (Int(key) != nil ? key : "\"\(key)\""), attributes: [.font: mono, .foregroundColor: Theme.tokens["property"]!]))
            if !node.isRange { s.append(NSAttributedString(string: ": ", attributes: [.font: mono, .foregroundColor: Theme.muted])) }
            else { s.append(NSAttributedString(string: " ", attributes: [.font: mono])) }
        }
        let color: NSColor = switch node.value {
        case _ where node.isRange || node.isExpandable: Theme.faint
        case .string: Theme.tokens["string"]!
        case .number: Theme.tokens["number"]!
        case .bool, .null: Theme.tokens["keyword"]!
        default: Theme.faint
        }
        s.append(NSAttributedString(string: node.summary, attributes: [.font: mono, .foregroundColor: color]))
        return s
    }

    func outlineViewItemDidExpand(_ n: Notification) { changed(n, open: true) }
    func outlineViewItemDidCollapse(_ n: Notification) { changed(n, open: false) }

    private func changed(_ n: Notification, open: Bool) {
        guard !restoring, let node = n.userInfo?["NSObject"] as? JSONNode else { return }
        if open { expanded.insert(ObjectIdentifier(node)) } else { expanded.remove(ObjectIdentifier(node)) }
        onExpandedChange?(expanded)
        onResize?(outline.numberOfRows)
    }
}

/// A scroll view that never scrolls itself, so the timeline keeps the wheel.
final class PassScrollView: NSScrollView {
    override func scrollWheel(with event: NSEvent) { nextResponder?.scrollWheel(with: event) }
}

// MARK: Trace steps, timeline entries, labels

/// A trace step (kind chip, label, ts, expandable detail), a session-timeline
/// entry, or a fold. Clicking toggles.
final class StepCell: PostCell {
    static let height: CGFloat = 26
    let chip = label(size: 10, weight: .medium, color: Theme.muted)
    let title = label(size: 13)
    let ts = label(size: 11, color: Theme.faint)
    let detail = NSTextField(wrappingLabelWithString: "")
    let marker = CALayer()
    var onToggle: (() -> Void)?

    init() {
        super.init("step")
        chip.wantsLayer = true
        chip.alignment = .center
        chip.layer?.cornerRadius = 4
        detail.font = .monospacedSystemFont(ofSize: 11, weight: .regular)
        detail.textColor = Theme.muted
        detail.isSelectable = true
        marker.cornerRadius = 4
        layer?.addSublayer(marker)
        [chip, title, ts, detail].forEach(addSubview)
    }

    required init?(coder: NSCoder) { fatalError() }

    func configure(step: TraceStep?, style: TimelineEntry.Style?, fold: (count: Int, open: Bool)?, open: Bool) {
        chip.stringValue = step?.kind ?? ""
        chip.isHidden = step?.kind == nil || style == .prompt || style == .response
        ts.stringValue = step?.ts.map(Self.shortTime) ?? ""
        detail.stringValue = open ? step?.detail ?? "" : ""
        detail.isHidden = !open || step?.detail == nil
        marker.isHidden = style != .prompt
        if let fold {
            title.stringValue = fold.open ? "··· hide \(fold.count) steps ···" : "··· \(fold.count) step\(fold.count == 1 ? "" : "s") ···"
            title.font = .systemFont(ofSize: 12)
            title.textColor = Theme.faint
            title.alignment = .center
        } else {
            title.stringValue = step?.label ?? ""
            title.font = style == .command ? .monospacedSystemFont(ofSize: 12, weight: .regular) : .systemFont(ofSize: 13, weight: style == .prompt ? .semibold : .regular)
            title.textColor = style == .command ? Theme.muted : Theme.text
            title.alignment = .left
        }
        toolTip = step?.detail == nil ? nil : "Click to \(open ? "hide" : "show") detail"
        needsLayout = true
    }

    static func shortTime(_ ts: String) -> String {
        guard let t = traceTime(ts) else { return ts }
        return Date(timeIntervalSince1970: t).formatted(date: .omitted, time: .shortened)
    }

    override func layout() {
        super.layout()
        let b = box
        var x = b.minX
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        marker.frame = NSRect(x: x, y: 9, width: 8, height: 8)
        effectiveAppearance.performAsCurrentDrawingAppearance {
            marker.backgroundColor = Theme.accent.cgColor
            chip.layer?.backgroundColor = Theme.chip.cgColor
        }
        CATransaction.commit()
        if !marker.isHidden { x += 16 }
        if !chip.isHidden {
            chip.sizeToFit()
            chip.frame = NSRect(x: x, y: 5, width: chip.frame.width + 10, height: 16)
            x += chip.frame.width + 8
        }
        ts.sizeToFit()
        ts.frame = NSRect(x: b.maxX - ts.frame.width, y: 5, width: ts.frame.width, height: 16)
        title.frame = NSRect(x: x, y: 4, width: max(0, ts.frame.minX - 8 - x), height: 18)
        if title.alignment == .center { title.frame = NSRect(x: b.minX, y: 4, width: b.width, height: 18) }
        detail.frame = NSRect(x: b.minX + 8, y: Self.height, width: b.width - 16, height: bounds.height - Self.height - 6)
    }

    override func mouseDown(with event: NSEvent) {
        if event.modifierFlags.contains(.option) { return super.mouseDown(with: event) }
        onToggle?()
    }
}

/// A label row: a trace part's title, a mermaid error, or a comment.
final class TextCell: PostCell {
    let meta = label(size: 11, color: Theme.faint)
    let body = NSTextField(wrappingLabelWithString: "")
    var partAnchorable = false
    override var anchorable: Bool { partAnchorable }

    init() {
        super.init("text")
        body.isSelectable = true
        [meta, body].forEach(addSubview)
    }

    required init?(coder: NSCoder) { fatalError() }

    func configure(meta m: String?, body text: NSAttributedString, anchorable: Bool) {
        meta.stringValue = m ?? ""
        meta.isHidden = m == nil
        body.attributedStringValue = text
        partAnchorable = anchorable
        needsLayout = true
    }

    override func layout() {
        super.layout()
        let c = Layout.content(bounds.width)
        let b = NSRect(x: c.x, y: 0, width: c.w, height: bounds.height)
        var y: CGFloat = 6
        if !meta.isHidden {
            meta.frame = NSRect(x: b.minX, y: y, width: b.width, height: 15)
            y += 17
        }
        body.frame = NSRect(x: b.minX, y: y, width: b.width, height: bounds.height - y - 4)
    }

    /// Height for the same content at `width`, using an offscreen field laid out the same way.
    @MainActor static func height(meta: Bool, body: NSAttributedString, width: CGFloat) -> CGFloat {
        sizer.attributedStringValue = body
        let w = Layout.content(width).w
        sizer.preferredMaxLayoutWidth = w
        let h = sizer.cell?.cellSize(forBounds: NSRect(x: 0, y: 0, width: w, height: .greatestFiniteMagnitude)).height ?? 16
        return ceil(h) + (meta ? 23 : 6) + 6
    }

    @MainActor private static let sizer = NSTextField(wrappingLabelWithString: "")
}

/// Hosts a pooled web view while on screen.
final class HTMLCell: PostCell {
    var web: WKWebView?
    override var anchorable: Bool { true }
    override var bottomGap: CGFloat { PostsView.gap }

    init() { super.init("html") }

    required init?(coder: NSCoder) { fatalError() }

    func attach(_ web: WKWebView) {
        self.web = web
        addSubview(web)
        needsLayout = true
    }

    func detach() -> WKWebView? {
        defer { web = nil }
        return web
    }

    override func layout() {
        super.layout()
        web?.frame = box
    }
}
