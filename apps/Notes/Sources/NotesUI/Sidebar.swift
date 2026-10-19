import AppKit
import NotesCore

/// A folder or note in the sidebar tree. Folders are keyed by vault-relative path.
final class Node {
    let path: String
    let title: String
    let isFolder: Bool
    var children: [Node] = []

    init(path: String, title: String, isFolder: Bool) {
        self.path = path
        self.title = title
        self.isFolder = isFolder
    }

    /// Builds the tree from a recursive `list` (folders first, notes by path).
    static func tree(_ entries: [ListEntry]) -> [Node] {
        let root = Node(path: "", title: "", isFolder: true)
        var folders = ["": root]
        func folder(_ path: String) -> Node {
            if let node = folders[path] { return node }
            let slash = path.lastIndex(of: "/")
            let parent = folder(slash.map { String(path[..<$0]) } ?? "")
            let node = Node(path: path, title: slash.map { String(path[path.index(after: $0)...]) } ?? path, isFolder: true)
            parent.children.append(node)
            folders[path] = node
            return node
        }
        for entry in entries {
            switch entry {
            case .folder(let path): _ = folder(path)
            case .note(let note):
                let dir = note.path.lastIndex(of: "/").map { String(note.path[..<$0]) } ?? ""
                folder(dir).children.append(Node(path: note.path, title: note.title, isFolder: false))
            }
        }
        return root.children
    }
}

@MainActor
final class Sidebar: NSObject, NSOutlineViewDataSource, NSOutlineViewDelegate, NSTextFieldDelegate {
    let view: NSView
    let outline = NSOutlineView()
    private let nameField = NSTextField()
    private var roots: [Node] = []
    private var entries: [ListEntry]?
    private var expanded = Set<String>()
    private var selecting = false

    var onOpen: (String) -> Void = { _ in }
    var onCreate: (String) -> Void = { _ in }

    override init() {
        let glass = NSGlassEffectView()
        glass.cornerRadius = 0
        view = glass
        super.init()

        let column = NSTableColumn(identifier: .init("title"))
        outline.addTableColumn(column)
        outline.outlineTableColumn = column
        outline.headerView = nil
        outline.style = .sourceList
        outline.selectionHighlightStyle = .regular // drawn by RowView, not the system
        outline.backgroundColor = .clear
        outline.rowHeight = 28
        outline.intercellSpacing = .zero
        outline.indentationPerLevel = 12
        outline.dataSource = self
        outline.delegate = self

        let scroll = NSScrollView()
        scroll.documentView = outline
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.automaticallyAdjustsContentInsets = false

        let caption = NSTextField(labelWithString: "Notes")
        caption.font = .systemFont(ofSize: 11, weight: .semibold)
        caption.textColor = .tertiaryLabelColor

        nameField.placeholderString = "Note name"
        nameField.font = .systemFont(ofSize: 13)
        nameField.bezelStyle = .roundedBezel
        nameField.focusRingType = .none
        nameField.delegate = self
        nameField.isHidden = true

        let content = NSView()
        // The name field takes space only while visible (detachesHiddenViews).
        let stack = NSStackView(views: [caption, nameField, scroll])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 6
        stack.detachesHiddenViews = true
        stack.edgeInsets = NSEdgeInsets(top: 46, left: 0, bottom: 0, right: 0)
        stack.translatesAutoresizingMaskIntoConstraints = false
        content.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            stack.topAnchor.constraint(equalTo: content.topAnchor),
            stack.bottomAnchor.constraint(equalTo: content.bottomAnchor),
            caption.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 18),
            nameField.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 10),
            nameField.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -10),
            scroll.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: content.trailingAnchor),
        ])
        glass.contentView = content
    }

    // MARK: State

    /// Replaces the tree; a no-op when the list didn't change.
    func update(_ new: [ListEntry]) {
        guard new != entries else { return }
        entries = new
        let selected = selectedNode?.path
        roots = Node.tree(new)
        outline.reloadData()
        restoreExpanded(roots)
        if let selected { select(path: selected) }
    }

    private func restoreExpanded(_ nodes: [Node]) {
        for node in nodes where node.isFolder && expanded.contains(node.path) {
            outline.expandItem(node)
            restoreExpanded(node.children)
        }
    }

    var selectedNode: Node? { outline.item(atRow: outline.selectedRow) as? Node }

    /// Folder new notes go into: the selected folder, or the selected note's folder.
    var currentFolder: String? {
        guard let node = selectedNode else { return nil }
        if node.isFolder { return node.path }
        return node.path.lastIndex(of: "/").map { String(node.path[..<$0]) }
    }

    /// Selects a note without opening it again, expanding its folders.
    func select(path: String?) {
        selecting = true
        defer { selecting = false }
        guard let path, let node = find(path, in: roots) else {
            outline.deselectAll(nil)
            return
        }
        var prefix = ""
        for part in path.split(separator: "/").dropLast() {
            prefix += prefix.isEmpty ? String(part) : "/\(part)"
            if let folder = find(prefix, in: roots) { outline.expandItem(folder) }
        }
        let row = outline.row(forItem: node)
        if row >= 0 {
            outline.selectRowIndexes([row], byExtendingSelection: false)
            outline.scrollRowToVisible(row)
        }
    }

    private func find(_ path: String, in nodes: [Node]) -> Node? {
        for node in nodes {
            if node.path == path { return node }
            if node.isFolder, path.hasPrefix(node.path + "/"), let hit = find(path, in: node.children) { return hit }
        }
        return nil
    }

    func askForName() {
        nameField.stringValue = ""
        nameField.isHidden = false
        view.window?.makeFirstResponder(nameField)
    }

    private func hideNameField() {
        nameField.isHidden = true
        view.window?.makeFirstResponder(outline)
    }

    // MARK: Name field

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        switch selector {
        case #selector(NSResponder.insertNewline(_:)):
            let name = nameField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
            hideNameField()
            if !name.isEmpty { onCreate(name) }
            return true
        case #selector(NSResponder.cancelOperation(_:)):
            hideNameField()
            return true
        default:
            return false
        }
    }

    // MARK: Outline

    func outlineView(_ outlineView: NSOutlineView, numberOfChildrenOfItem item: Any?) -> Int {
        (item as? Node)?.children.count ?? roots.count
    }

    func outlineView(_ outlineView: NSOutlineView, child index: Int, ofItem item: Any?) -> Any {
        (item as? Node)?.children[index] ?? roots[index]
    }

    func outlineView(_ outlineView: NSOutlineView, isItemExpandable item: Any) -> Bool {
        (item as? Node)?.isFolder ?? false
    }

    func outlineView(_ outlineView: NSOutlineView, rowViewForItem item: Any) -> NSTableRowView? {
        outlineView.makeView(withIdentifier: RowView.id, owner: nil) as? RowView ?? RowView()
    }

    func outlineView(_ outlineView: NSOutlineView, viewFor tableColumn: NSTableColumn?, item: Any) -> NSView? {
        let cell = outlineView.makeView(withIdentifier: Cell.id, owner: nil) as? Cell ?? Cell()
        cell.textField?.stringValue = (item as? Node)?.title ?? ""
        return cell
    }

    func outlineViewItemDidExpand(_ notification: Notification) {
        if let node = notification.userInfo?["NSObject"] as? Node { expanded.insert(node.path) }
    }

    func outlineViewItemDidCollapse(_ notification: Notification) {
        if let node = notification.userInfo?["NSObject"] as? Node { expanded.remove(node.path) }
    }

    func outlineViewSelectionDidChange(_ notification: Notification) {
        guard !selecting, let node = selectedNode, !node.isFolder else { return }
        onOpen(node.path)
    }
}

/// Title-only cell: dimmed, brighter when its row is selected.
final class Cell: NSTableCellView {
    static let id = NSUserInterfaceItemIdentifier("cell")

    init() {
        super.init(frame: .zero)
        identifier = Cell.id
        let label = NSTextField(labelWithString: "")
        label.font = .systemFont(ofSize: 13)
        label.lineBreakMode = .byTruncatingTail
        label.textColor = .secondaryLabelColor
        label.translatesAutoresizingMaskIntoConstraints = false
        addSubview(label)
        textField = label
        NSLayoutConstraint.activate([
            label.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 2),
            label.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -4),
            label.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    required init?(coder: NSCoder) { fatalError() }

    override var backgroundStyle: NSView.BackgroundStyle {
        didSet { textField?.textColor = backgroundStyle == .emphasized ? .labelColor : .secondaryLabelColor }
    }
}

/// Row with a 7 pt rounded hover/selection background that fades in 120 ms.
final class RowView: NSTableRowView {
    static let id = NSUserInterfaceItemIdentifier("row")
    private let fill = CALayer()
    private var hovered = false { didSet { if hovered != oldValue { updateFill() } } }

    init() {
        super.init(frame: .zero)
        identifier = RowView.id
        wantsLayer = true
        fill.cornerRadius = 7
        fill.cornerCurve = .continuous
        layer?.insertSublayer(fill, at: 0)
    }

    required init?(coder: NSCoder) { fatalError() }

    override var isSelected: Bool { didSet { if isSelected != oldValue { updateFill() } } }

    /// Cells read this to pick their title color.
    override var interiorBackgroundStyle: NSView.BackgroundStyle { isSelected ? .emphasized : .normal }

    override func drawSelection(in dirtyRect: NSRect) {}

    override func layout() {
        super.layout()
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        fill.frame = bounds
        CATransaction.commit()
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.forEach(removeTrackingArea)
        addTrackingArea(NSTrackingArea(rect: bounds, options: [.mouseEnteredAndExited, .activeInKeyWindow, .inVisibleRect], owner: self))
    }

    override func mouseEntered(with event: NSEvent) { hovered = true }
    override func mouseExited(with event: NSEvent) { hovered = false }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        updateFill(animated: false)
    }

    private func updateFill(animated: Bool = true) {
        let alpha: CGFloat = isSelected ? 0.10 : hovered ? 0.06 : 0
        var color: CGColor?
        effectiveAppearance.performAsCurrentDrawingAppearance {
            color = NSColor.labelColor.withAlphaComponent(alpha).cgColor
        }
        CATransaction.begin()
        CATransaction.setAnimationDuration(animated ? 0.12 : 0)
        CATransaction.setDisableActions(!animated)
        fill.backgroundColor = color
        CATransaction.commit()
    }
}
