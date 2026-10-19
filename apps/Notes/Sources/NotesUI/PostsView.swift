import AppKit
import NotesCore
import os

/// One row of the session timeline. Posts are split into rows (header, one per
/// part, one per trace step, one per comment, footer) so NSTableView only lays out
/// and reuses what's on screen, and an update touches only that post's rows.
enum Row: Hashable {
    case entry(gap: Int, TimelineEntry, gen: Int)
    case header(String, UInt64)
    case part(String, UInt64, Int)
    case step(String, UInt64, Int, Int)
    /// Post id ("" for session comments) and comment id.
    case comment(String, String)
    case footer(String, UInt64)
}

extension OSLog { static let posts = OSLog(subsystem: "dev.notes.app", category: .pointsOfInterest) }

/// The main pane for an agent session: a timeline of post cards with the session
/// trace between them, comment threads, and a floating composer.
@MainActor
final class PostsView: NSObject, NSTableViewDataSource, NSTableViewDelegate {
    let view = PostsRoot()
    let table = NSTableView()
    let scroll = NSScrollView()
    private(set) lazy var composer = Composer(owner: self)

    private(set) var session: String?
    private(set) var posts: [LoadedPost] = []
    private var postIndex: [String: Int] = [:]
    private(set) var comments: [Comment] = []
    private var trace: [TraceStep] = []
    private var traceGen = 0
    private var openFolds = Set<Int>()
    private var openRows = Set<Row>()
    private(set) var rows: [Row] = []
    private var generation = 0

    /// Width-dependent heights, valid for `measuredWidth`.
    private var measured: [Row: CGFloat] = [:]
    private var measuredWidth: CGFloat = 0
    private var htmlHeights: [String: CGFloat] = [:]
    private var jsonState: [Row: (rows: Int, open: Set<ObjectIdentifier>)] = [:]
    /// pele layouts for narrow panes, by "sha@bucket".
    private var mermaidLayouts: [String: PeleDiagram] = [:]
    private var mermaidPending = Set<String>()
    var onLoaded: (() -> Void)?

    override init() {
        super.init()
        view.owner = self
        view.wantsLayer = true
        let column = NSTableColumn(identifier: .init("post"))
        column.resizingMask = .autoresizingMask
        table.addTableColumn(column)
        table.headerView = nil
        table.style = .plain
        table.intercellSpacing = .zero
        table.selectionHighlightStyle = .none
        table.backgroundColor = .clear // the clip view paints the pane color as a layer background
        table.usesAutomaticRowHeights = false
        table.columnAutoresizingStyle = .uniformColumnAutoresizingStyle
        table.dataSource = self
        table.delegate = self
        WebParts.shared.owner = self

        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.drawsBackground = true
        scroll.backgroundColor = Theme.pane
        scroll.automaticallyAdjustsContentInsets = false
        scroll.contentInsets = NSEdgeInsets(top: 44, left: 0, bottom: Composer.height + 28, right: 0)
        scroll.contentView.postsBoundsChangedNotifications = true
        NotificationCenter.default.addObserver(forName: NSView.boundsDidChangeNotification, object: scroll.contentView, queue: nil) { [weak self] _ in
            MainActor.assumeIsolated { self?.layoutCodeViewports() }
        }

        for v in [scroll, composer.view] as [NSView] {
            v.translatesAutoresizingMaskIntoConstraints = false
            view.addSubview(v)
        }
        NSLayoutConstraint.activate([
            scroll.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            scroll.topAnchor.constraint(equalTo: view.topAnchor),
            scroll.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            composer.view.bottomAnchor.constraint(equalTo: view.bottomAnchor, constant: -14),
            composer.view.centerXAnchor.constraint(equalTo: view.centerXAnchor),
            composer.view.widthAnchor.constraint(lessThanOrEqualToConstant: Layout.maxCard),
            composer.view.widthAnchor.constraint(lessThanOrEqualTo: view.widthAnchor, constant: -Layout.margin * 2),
            composer.view.widthAnchor.constraint(equalTo: view.widthAnchor, constant: -Layout.margin * 2).withPriority(.defaultHigh),
            composer.view.heightAnchor.constraint(equalToConstant: Composer.height),
        ])
    }

    // MARK: Loading

    func open(session id: String) {
        guard id != session else { return }
        session = id
        generation += 1
        let gen = generation
        posts = []
        comments = []
        trace = []
        openRows = []
        openFolds = []
        rebuild(reload: true)
        composer.reset()
        PostLoader.run({ try PostLoader.session($0, id) }) { [weak self] result in
            guard let self, gen == generation, case .success(let snapshot) = result else { return }
            posts = snapshot.posts
            comments = snapshot.comments
            trace = snapshot.trace
            traceGen += 1
            rebuild(reload: true)
            if !rows.isEmpty { table.scrollRowToVisible(rows.count - 1) }
            onLoaded?()
        }
    }

    func close() {
        session = nil
        generation += 1
        posts = []
        comments = []
        trace = []
        rebuild(reload: true)
    }

    /// Applies one watch event to the open session: only the affected post, comment or trace is fetched.
    func handle(_ event: Event) {
        guard let session else { return }
        if let s = event.session, s != session { return }
        switch event.kind {
        case "post-created", "post-updated":
            if let id = event.id { reload(id) }
        case "post-deleted":
            if let id = event.id { removePost(id) }
        case "comment-created":
            let after = comments.map(\.seq).max()
            let gen = generation
            daemon({ try $0.commentList(CommentListArgs(session: session, afterSeq: after)) }) { [weak self] result in
                guard let self, gen == generation, case .success(let new) = result else { return }
                merge(new)
            }
        case "comment-deleted":
            if let id = event.id, comments.contains(where: { $0.id == id }) {
                comments.removeAll { $0.id == id }
                rebuild()
            }
        case "trace-updated":
            let gen = generation
            daemon({ try $0.traceList(session: session) }) { [weak self] result in
                guard let self, gen == generation, case .success(let steps) = result else { return }
                trace = steps
                traceGen += 1
                rebuild()
            }
        case "session-deleted":
            if event.id == session { close() }
        default: break
        }
    }

    private func reload(_ id: String, version: UInt64? = nil) {
        let gen = generation
        PostLoader.run({ try PostLoader.load($0, id, version: version) }) { [weak self] result in
            guard let self, gen == generation, case .success(let loaded) = result, loaded.post.session == session else { return }
            if let i = postIndex[id] {
                posts[i] = loaded
            } else {
                let at = posts.firstIndex { $0.post.createdAt > loaded.post.createdAt } ?? posts.count
                posts.insert(loaded, at: at)
            }
            rebuild()
        }
    }

    private func removePost(_ id: String) {
        guard postIndex[id] != nil else { return }
        posts.removeAll { $0.id == id }
        rebuild()
    }

    private func merge(_ new: [Comment]) {
        let known = Set(comments.map(\.id))
        let fresh = new.filter { !known.contains($0.id) && $0.session == session }
        guard !fresh.isEmpty else { return }
        comments = (comments + fresh).sorted { $0.seq < $1.seq }
        rebuild()
    }

    // MARK: Rows

    private func buildRows() -> [Row] {
        var out: [Row] = []
        let gaps = timelineGaps(steps: trace, posts: posts.map { Double($0.post.createdAt) }, open: openFolds)
        let byPost = Dictionary(grouping: comments) { $0.post ?? "" }
        for (k, loaded) in posts.enumerated() {
            out += gaps[k].map { .entry(gap: k, $0, gen: traceGen) }
            let id = loaded.id, v = loaded.post.version
            out.append(.header(id, v))
            for (i, part) in loaded.parts.enumerated() {
                out.append(.part(id, v, i))
                if case .trace(let t) = part { out += t.steps.indices.map { .step(id, v, i, $0) } }
            }
            out += (byPost[id] ?? []).map { .comment(id, $0.id) }
            out.append(.footer(id, v))
        }
        out += (gaps.last ?? []).map { .entry(gap: posts.count, $0, gen: traceGen) }
        // Session comments, and comments on posts that are gone.
        out += comments.filter { $0.post.map { postIndex[$0] == nil } ?? true }.map { .comment("", $0.id) }
        return out
    }

    /// Rebuilds rows and applies only the difference: unchanged rows keep their views.
    private func rebuild(reload: Bool = false) {
        postIndex = Dictionary(uniqueKeysWithValues: posts.enumerated().map { ($1.id, $0) })
        composer.update() // its target title follows the posts (default: the last one)
        let new = buildRows()
        if reload || rows.isEmpty {
            rows = new
            measured = [:]
            table.reloadData()
            return
        }
        let diff = new.difference(from: rows)
        guard !diff.isEmpty else { return }
        // Reading the newest post: stay pinned to the bottom as the agent adds more.
        let clip = scroll.contentView
        let atBottom = clip.bounds.maxY >= table.frame.maxY + scroll.contentInsets.bottom - 4
        rows = new
        NSAnimationContext.runAnimationGroup { ctx in
            ctx.duration = 0
            table.beginUpdates()
            for change in diff {
                switch change {
                case .remove(let offset, _, _): table.removeRows(at: [offset], withAnimation: [])
                case .insert(let offset, _, _): table.insertRows(at: [offset], withAnimation: [])
                }
            }
            table.endUpdates()
        }
        if atBottom, !rows.isEmpty { table.scrollRowToVisible(rows.count - 1) }
    }

    func post(_ id: String) -> LoadedPost? { postIndex[id].map { posts[$0] } }

    func partModel(_ row: Row) -> PartModel? {
        switch row {
        case .part(let id, _, let i), .step(let id, _, let i, _):
            guard let p = post(id), p.parts.indices.contains(i) else { return nil }
            return p.parts[i]
        default: return nil
        }
    }

    func comment(_ id: String) -> Comment? { comments.first { $0.id == id } }

    private func noteHeights(_ indexes: IndexSet) {
        guard !indexes.isEmpty else { return }
        NSAnimationContext.runAnimationGroup { ctx in
            ctx.duration = 0
            table.noteHeightOfRows(withIndexesChanged: indexes)
        }
    }

    private func refresh(_ indexes: IndexSet) {
        guard !indexes.isEmpty else { return }
        noteHeights(indexes)
        table.reloadData(forRowIndexes: indexes, columnIndexes: [0])
    }

    private func indexes(where match: (Row) -> Bool) -> IndexSet {
        IndexSet(rows.indices.filter { match(rows[$0]) })
    }

    // MARK: Heights

    func numberOfRows(in tableView: NSTableView) -> Int { rows.count }

    func tableView(_ tableView: NSTableView, heightOfRow index: Int) -> CGFloat {
        let width = tableView.tableColumns[0].width
        if width != measuredWidth {
            measuredWidth = width
            measured = [:]
        }
        let row = rows[index]
        if let h = measured[row] { return h }
        let h = max(1, height(row, width: width))
        measured[row] = h
        return h
    }

    func tableViewColumnDidResize(_ notification: Notification) {
        guard table.tableColumns[0].width != measuredWidth else { return }
        noteHeights(IndexSet(integersIn: 0..<rows.count))
    }

    static let gap: CGFloat = 10

    private func height(_ row: Row, width: CGFloat) -> CGFloat {
        let contentW = Layout.content(width).w
        switch row {
        case .header: return 48
        case .footer: return Layout.radius + 4 + CardRowView.footerGap // the card segment must be taller than its corner radius
        case .comment(_, let id):
            guard let c = comment(id) else { return 1 }
            return TextCell.height(meta: true, body: Self.commentBody(c), width: width)
        case .entry(_, let entry, _):
            guard case .step(let i, _) = entry, openRows.contains(row), trace.indices.contains(i), let d = trace[i].detail else { return StepCell.height }
            return StepCell.height + detailHeight(d, width: width)
        case .step(_, _, _, let s):
            guard case .trace(let t)? = partModel(row), t.steps.indices.contains(s), openRows.contains(row), let d = t.steps[s].detail else { return StepCell.height }
            return StepCell.height + detailHeight(d, width: width)
        case .part:
            switch partModel(row) {
            case .code(let m)?:
                return (m.title == nil ? 0 : CodeCell.titleHeight) + CGFloat(m.lineCount) * CodeText.lineHeight + CodePaneView.inset * 2 + Self.gap
            case .markdown(let s)?:
                return Measure.height(s, width: contentW) + Self.gap + 4
            case .mermaid(let m)?:
                guard let natural = m.natural else { return TextCell.height(meta: true, body: Self.mermaidError(m), width: width) }
                let d = mermaidLayout(m, width: width).diagram ?? natural
                return ceil(d.height * min(1, contentW / d.width)) + 8 + Self.gap
            case .html(let m)?:
                return (htmlHeights[m.key] ?? 120) + Self.gap
            case .image(let m)?:
                guard let image = m.image else { return TextCell.height(meta: false, body: Self.plain("Image unavailable — it may have been evicted."), width: width) }
                let w = CGFloat(image.width) / 2, h = CGFloat(image.height) / 2
                return ceil(min(h * min(1, contentW / w), 640)) + 8 + (m.caption == nil ? 0 : ImageCell.captionHeight) + Self.gap
            case .json(let node)?:
                let count = jsonState[row]?.rows ?? (node.isExpandable ? node.children.count : 1)
                return CGFloat(count) * JSONCell.rowHeight + 12 + Self.gap
            case .trace(let t)?:
                return TextCell.height(meta: t.note != nil, body: Self.traceTitle(t), width: width)
            case nil: return 1
            }
        }
    }

    private func detailHeight(_ detail: String, width: CGFloat) -> CGFloat {
        let s = NSAttributedString(string: detail, attributes: [.font: NSFont.monospacedSystemFont(ofSize: 11, weight: .regular)])
        return TextCell.height(meta: false, body: s, width: width - 16) - 6
    }

    static func plain(_ s: String, size: CGFloat = 13, color: NSColor = Theme.text) -> NSAttributedString {
        NSAttributedString(string: s, attributes: [.font: NSFont.systemFont(ofSize: size), .foregroundColor: color])
    }

    static func commentBody(_ c: Comment) -> NSAttributedString { plain(c.text) }

    static func commentMeta(_ c: Comment) -> String {
        ([c.author == "user" ? "you" : c.author, relativeTime(c.createdAt)] + (c.anchor.map { [$0.summary] } ?? [])).joined(separator: " · ")
    }

    static func traceTitle(_ t: TraceModel) -> NSAttributedString {
        NSAttributedString(string: t.title, attributes: [.font: NSFont.systemFont(ofSize: 13, weight: .semibold), .foregroundColor: Theme.text])
    }

    static func mermaidError(_ m: MermaidModel) -> NSAttributedString {
        let s = NSMutableAttributedString(attributedString: plain(m.source, size: 12, color: Theme.muted))
        s.addAttribute(.font, value: NSFont.monospacedSystemFont(ofSize: 12, weight: .regular), range: NSRange(location: 0, length: s.length))
        return s
    }

    // MARK: Views

    func tableView(_ tableView: NSTableView, rowViewForRow index: Int) -> NSTableRowView? {
        let rowView = tableView.makeView(withIdentifier: .init("card"), owner: nil) as? CardRowView ?? {
            let r = CardRowView()
            r.identifier = .init("card")
            return r
        }()
        rowView.edge = switch rows[index] {
        case .header: .top
        case .footer: .bottom
        case .part, .step: .middle
        case .comment(let post, _): post.isEmpty ? .none : .middle
        case .entry: .none
        }
        return rowView
    }

    func tableView(_ tableView: NSTableView, shouldSelectRow row: Int) -> Bool { false }

    private func make<T: PostCell>(_ id: String, _ create: () -> T) -> T {
        let cell = table.makeView(withIdentifier: .init(id), owner: nil) as? T ?? create()
        cell.owner = self
        return cell
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row index: Int) -> NSView? {
        os_signpost(.begin, log: .posts, name: "cell")
        defer { os_signpost(.end, log: .posts, name: "cell") }
        let row = rows[index]
        let cell = view(for: row)
        cell.row = row
        if case .part(let id, _, let i) = row {
            cell.pins = comments.compactMap { c in
                guard c.post == id, let a = c.anchor, a.part == i else { return nil }
                switch a {
                case .point(_, _, let x, let y), .rect(_, _, let x, let y, _, _): return CGPoint(x: x, y: y)
                case .lineRange: return nil
                }
            }
        } else {
            cell.pins = []
        }
        return cell
    }

    private func view(for row: Row) -> PostCell {
        switch row {
        case .header(let id, _):
            let cell = make("header") { HeaderCell() }
            if let p = post(id) { cell.configure(p) }
            return cell
        case .footer:
            return make("footer") { PostCell("footer") }
        case .comment(let post, let id):
            let cell = make("text") { TextCell() }
            if let c = comment(id) { cell.configure(meta: Self.commentMeta(c), body: Self.commentBody(c), anchorable: false) }
            cell.menu = commentMenu(id)
            return cell
        case .entry(_, let entry, _):
            let cell = make("step") { StepCell() }
            switch entry {
            case .step(let i, let style):
                let step = trace.indices.contains(i) ? trace[i] : nil
                cell.configure(step: step, style: style, fold: nil, open: openRows.contains(row))
                cell.onToggle = { [weak self] in if step?.detail != nil { self?.toggle(row) } }
            case .fold(let first, let count, let open):
                cell.configure(step: nil, style: nil, fold: (count, open), open: false)
                cell.onToggle = { [weak self] in self?.toggleFold(first) }
            }
            return cell
        case .step(_, _, _, let s):
            let cell = make("step") { StepCell() }
            if case .trace(let t)? = partModel(row), t.steps.indices.contains(s) {
                let step = t.steps[s]
                cell.configure(step: step, style: nil, fold: nil, open: openRows.contains(row))
                cell.onToggle = { [weak self] in if step.detail != nil { self?.toggle(row) } }
            }
            return cell
        case .part(let id, let version, let i):
            return partView(row, id, version, i)
        }
    }

    private func partView(_ row: Row, _ id: String, _ version: UInt64, _ i: Int) -> PostCell {
        switch partModel(row) {
        case .code(let m)?:
            let cell = make("code") { CodeCell() }
            cell.configure(m) { [weak self] pane, first, last in self?.lineSelection(id, version, i, m, pane, first, last) }
            return cell
        case .markdown(let s)?:
            let cell = make("markdown") { MarkdownCell() }
            cell.configure(s)
            return cell
        case .mermaid(let m)?:
            guard m.natural != nil else {
                let cell = make("text") { TextCell() }
                cell.configure(meta: "Couldn’t render diagram — \(m.error ?? "parse error")", body: Self.mermaidError(m), anchorable: true)
                cell.menu = nil
                return cell
            }
            let cell = make("mermaid") { ImageCell("mermaid") }
            configureMermaid(cell, m, row: row)
            return cell
        case .html(let m)?:
            let cell = make("html") { HTMLCell() }
            if cell.web == nil { cell.attach(WebParts.shared.take(key: m.key)) }
            WebParts.shared.show(m, in: cell.web!, post: id, part: i, dark: Theme.isDark(view.effectiveAppearance))
            return cell
        case .image(let m)?:
            guard let image = m.image else {
                let cell = make("text") { TextCell() }
                cell.configure(meta: nil, body: Self.plain("Image unavailable — it may have been evicted.", color: Theme.muted), anchorable: true)
                cell.menu = nil
                return cell
            }
            let cell = make("image") { ImageCell("image") }
            cell.configure(image: image, size: CGSize(width: CGFloat(image.width) / 2, height: CGFloat(image.height) / 2), caption: m.caption, alt: m.alt)
            cell.onOpen = { Self.openData(m.data, name: "image") }
            return cell
        case .json(let node)?:
            let cell = make("json") { JSONCell() }
            cell.configure(node, expanded: jsonState[row]?.open ?? [])
            cell.onExpandedChange = { [weak self] open in self?.jsonState[row, default: (0, [])].open = open }
            cell.onResize = { [weak self] count in self?.jsonResized(row, rows: count) }
            return cell
        case .trace(let t)?:
            let cell = make("text") { TextCell() }
            cell.configure(meta: t.note, body: Self.traceTitle(t), anchorable: true)
            cell.menu = nil
            return cell
        case nil:
            return make("footer") { PostCell("footer") }
        }
    }

    /// Code text views sit in their own horizontal scroll view, so TextKit 2 never hears
    /// that the timeline scrolled: lines that were offscreen when a row appeared would
    /// stay unlaid-out (clipped). Re-run the viewport for the code panes on screen.
    private func layoutCodeViewports() {
        let visible = table.rows(in: table.visibleRect)
        for i in visible.lowerBound..<NSMaxRange(visible) {
            guard let cell = table.view(atColumn: 0, row: i, makeIfNecessary: false) as? CodeCell else { continue }
            for pane in cell.panes where !pane.isHidden {
                pane.text.textLayoutManager?.textViewportLayoutController.layoutViewport()
            }
        }
    }

    /// Pooled web views go back to the pool as soon as their row leaves the screen.
    func tableView(_ tableView: NSTableView, didRemove rowView: NSTableRowView, forRow row: Int) {
        if let cell = rowView.view(atColumn: 0) as? HTMLCell, let web = cell.detach() { WebParts.shared.give(web) }
    }

    // MARK: Interaction

    private func toggle(_ row: Row) {
        if openRows.remove(row) == nil { openRows.insert(row) }
        measured[row] = nil
        refresh(indexes { $0 == row })
    }

    private func toggleFold(_ first: Int) {
        if openFolds.remove(first) == nil { openFolds.insert(first) }
        rebuild()
    }

    private func jsonResized(_ row: Row, rows count: Int) {
        jsonState[row, default: (0, [])].rows = count
        measured[row] = nil
        noteHeights(indexes { $0 == row })
    }

    func htmlResized(key: String, height: CGFloat) {
        guard htmlHeights[key] != height else { return }
        htmlHeights[key] = height
        let changed = indexes { if case .html(let m)? = self.partModel($0) { m.key == key } else { false } }
        changed.forEach { measured[rows[$0]] = nil }
        noteHeights(changed)
    }

    func pointAnchor(_ row: Row, x: Double, y: Double) {
        guard case .part(let id, let version, let i) = row else { return }
        composer.set(post: id, anchor: .point(part: UInt32(i), postVersion: version, x: x, y: y))
    }

    private func lineSelection(_ id: String, _ version: UInt64, _ part: Int, _ model: CodeModel, _ pane: Int, _ first: Int, _ last: Int) {
        guard let anchor = model.lineAnchor(part: part, version: version, pane: pane, first: first, last: last) else { return }
        composer.set(post: id, anchor: anchor)
    }

    func showVersion(_ id: String, _ version: UInt64) {
        guard let p = post(id) else { return }
        reload(id, version: version == p.latest ? nil : version)
    }

    func deletePost(_ id: String) {
        guard let p = post(id), let window = view.window else { return }
        let alert = NSAlert()
        alert.messageText = "Delete “\(p.post.title)”?"
        alert.informativeText = "The agent's post and its comments are removed for everyone."
        alert.addButton(withTitle: "Delete").hasDestructiveAction = true
        alert.addButton(withTitle: "Cancel")
        alert.beginSheetModal(for: window) { [weak self] response in
            guard response == .alertFirstButtonReturn else { return }
            daemon({ try $0.postDelete(id) }) { result in
                if case .success = result { self?.removePost(id) } else { NSSound.beep() }
            }
        }
    }

    private func commentMenu(_ id: String) -> NSMenu {
        let menu = NSMenu()
        let copy = NSMenuItem(title: "Copy Comment", action: #selector(copyComment(_:)), keyEquivalent: "")
        let delete = NSMenuItem(title: "Delete Comment", action: #selector(deleteComment(_:)), keyEquivalent: "")
        for item in [copy, delete] {
            item.target = self
            item.representedObject = id
            menu.addItem(item)
        }
        return menu
    }

    @objc private func copyComment(_ item: NSMenuItem) {
        guard let c = comment(item.representedObject as? String ?? "") else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(c.text, forType: .string)
    }

    @objc private func deleteComment(_ item: NSMenuItem) {
        guard let id = item.representedObject as? String else { return }
        daemon({ try $0.commentDelete(id) }) { [weak self] result in
            guard let self, case .success = result else { return NSSound.beep() }
            comments.removeAll { $0.id == id }
            rebuild()
        }
    }

    /// Posts a `user` comment; the composer keeps the text if it fails.
    func sendComment(post: String?, text: String, anchor: CommentAnchor?, done: @escaping @MainActor (Bool) -> Void) {
        guard let session else { return done(false) }
        let args = CommentNewArgs(post: post, session: post == nil ? session : nil, text: text, anchor: anchor, author: "user")
        daemon({ try $0.commentNew(args) }) { [weak self] result in
            switch result {
            case .success(let c):
                self?.merge([c])
                done(true)
            case .failure(let error):
                NSLog("comment-new failed: %@", error.message)
                NSSound.beep()
                done(false)
            }
        }
    }

    /// The html bridge's window.sendPrompt: a `user` comment on that part's post.
    func sendPrompt(post: String, text: String) {
        sendComment(post: post, text: text, anchor: nil) { _ in }
    }

    var lastPostID: String? { posts.last?.id }

    static func openData(_ data: Data?, name: String) {
        guard let data else { return }
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("\(name)-\(sha256(String(decoding: data.prefix(4096), as: UTF8.self)).prefix(8))")
        try? data.write(to: url)
        NSWorkspace.shared.open(url)
    }

    // MARK: Mermaid

    /// The layout key and pele layout for the pane width: the natural layout when it
    /// fits, else a `maxWidth` render per 80 pt bucket (nil until it finishes).
    private func mermaidLayout(_ m: MermaidModel, width: CGFloat) -> (key: String, diagram: PeleDiagram?) {
        let hash = sha256(m.source)
        guard let natural = m.natural else { return (hash, nil) }
        let avail = Layout.content(width).w
        if natural.width <= avail { return ("\(hash)@0", natural) }
        let bucket = max(160, floor(avail / 80) * 80)
        let key = "\(hash)@\(Int(bucket))"
        if let d = mermaidLayouts[key] { return (key, d) }
        if mermaidPending.insert(key).inserted {
            let source = m.source
            DispatchQueue.global(qos: .userInitiated).async { [weak self] in
                let d = try? Pele.shared.render(source, maxWidth: Double(bucket))
                guard self != nil else { return }
                onMain { [weak self] in
                    guard let self else { return }
                    mermaidPending.remove(key)
                    mermaidLayouts[key] = d ?? natural
                    let changed = indexes { if case .mermaid(let x)? = self.partModel($0) { x.source == source } else { false } }
                    changed.forEach { measured[rows[$0]] = nil }
                    noteHeights(changed)
                    reconfigureMermaid(source: source)
                }
            }
        }
        return (key, nil)
    }

    private func configureMermaid(_ cell: ImageCell, _ m: MermaidModel, row: Row) {
        let width = table.tableColumns[0].width
        let (key, layout) = mermaidLayout(m, width: width)
        let d = layout ?? m.natural!
        let fit = min(1, Layout.content(width).w / d.width)
        let size = CGSize(width: d.width * fit, height: d.height * fit)
        let dark = Theme.isDark(view.effectiveAppearance)
        let scale = view.window?.backingScaleFactor ?? 2
        let rasterKey = sha256(m.source + (dark ? "dark" : "light") + key + "\(Int(size.width))")
        cell.configure(image: MermaidRaster.cached(rasterKey), size: size, caption: nil, alt: m.source)
        cell.onOpen = { MermaidRaster.openFullscreen(d, dark: dark) }
        guard MermaidRaster.cached(rasterKey) == nil else { return }
        MermaidRaster.render(d, dark: dark, size: size, scale: scale, key: rasterKey) { [weak self] in
            self?.reconfigureMermaid(source: m.source)
        }
    }

    /// Re-runs configuration on mermaid cells the table already holds. reloadData(forRowIndexes:)
    /// skips rows the table prepared but doesn't count as visible, leaving them blank.
    private func reconfigureMermaid(source: String) {
        for i in indexes(where: { if case .mermaid(let x)? = self.partModel($0) { x.source == source } else { false } }) {
            guard let cell = table.view(atColumn: 0, row: i, makeIfNecessary: false) as? ImageCell,
                  case .mermaid(let m)? = partModel(rows[i]) else { continue }
            configureMermaid(cell, m, row: rows[i])
        }
    }

    /// Appearance flips: mermaid re-rasterizes for the theme, html reloads with its tokens.
    func appearanceChanged() {
        scroll.backgroundColor = Theme.pane
        let visible = table.rows(in: table.visibleRect)
        refresh(IndexSet(integersIn: visible.lowerBound..<NSMaxRange(visible)).filteredIndexSet {
            switch partModel(rows[$0]) {
            case .mermaid?, .html?: true
            default: false
            }
        })
    }

    // MARK: Scroll benchmark

    /// Scrolls the timeline top to bottom and back in 6 pt steps (720 pt/s at 120 Hz),
    /// forcing layout and display after each step, and returns each step's main-thread ms.
    func benchmarkScroll(step: CGFloat = 6) -> [Double] {
        guard let window = view.window else { return [] }
        let clip = scroll.contentView
        let top = -scroll.contentInsets.top
        let bottom = max(top, table.frame.height - clip.bounds.height + scroll.contentInsets.bottom)
        var times: [Double] = []
        var ys = Array(stride(from: top, through: bottom, by: step))
        ys += ys.reversed()
        for y in ys {
            let start = DispatchTime.now().uptimeNanoseconds
            os_signpost(.begin, log: .posts, name: "scroll-step")
            clip.scroll(to: NSPoint(x: 0, y: y))
            scroll.reflectScrolledClipView(clip)
            window.displayIfNeeded()
            CATransaction.flush()
            os_signpost(.end, log: .posts, name: "scroll-step")
            let ms = Double(DispatchTime.now().uptimeNanoseconds - start) / 1e6
            times.append(ms)
            if ms > 8.33 {
                let visible = table.rows(in: table.visibleRect)
                let kinds = (visible.lowerBound..<NSMaxRange(visible)).map { i -> String in
                    if case .part = rows[i] { return partModel(rows[i]).map { "\($0)".prefix(while: { $0 != "(" }) }.map(String.init) ?? "?" }
                    return "\(rows[i])".prefix(while: { $0 != "(" }).description
                }
                NSLog("scroll bench: %.1f ms at y=%.0f with rows %@", ms, y, kinds.joined(separator: ","))
            }
        }
        return times
    }
}

extension NSLayoutConstraint {
    func withPriority(_ p: NSLayoutConstraint.Priority) -> NSLayoutConstraint {
        priority = p
        return self
    }
}

/// Root view of the posts pane; forwards appearance changes.
final class PostsRoot: NSView {
    weak var owner: PostsView?
    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        MainActor.assumeIsolated { owner?.appearanceChanged() }
    }
}

/// Text heights from an offscreen TextKit 2 stack configured like the cells' text views.
@MainActor
enum Measure {
    private static let content = NSTextContentStorage()
    private static let layout: NSTextLayoutManager = {
        let l = NSTextLayoutManager()
        let c = NSTextContainer(size: NSSize(width: 100, height: CGFloat.greatestFiniteMagnitude))
        c.lineFragmentPadding = 0
        l.textContainer = c
        content.addTextLayoutManager(l)
        return l
    }()

    static func height(_ s: NSAttributedString, width: CGFloat) -> CGFloat {
        let l = layout
        l.textContainer?.size = NSSize(width: width, height: .greatestFiniteMagnitude)
        content.attributedString = s
        l.ensureLayout(for: l.documentRange)
        return ceil(l.usageBoundsForTextContainer.height)
    }
}

/// Rasterized diagrams by sha256(source + theme + width), capped at 48 MB. Drawing
/// the SVG happens off the main thread; the cell only sets layer contents.
enum MermaidRaster {
    private final class Box { let image: CGImage; init(_ i: CGImage) { image = i } }
    nonisolated(unsafe) private static let cache: NSCache<NSString, Box> = {
        let c = NSCache<NSString, Box>()
        c.totalCostLimit = 48 << 20
        return c
    }()
    @MainActor private static var pending = Set<String>()
    private static let queue = DispatchQueue(label: "dev.notes.app.mermaid", qos: .userInitiated)

    static func cached(_ key: String) -> CGImage? { cache.object(forKey: key as NSString)?.image }

    @MainActor
    static func render(_ d: PeleDiagram, dark: Bool, size: CGSize, scale: CGFloat, key: String, done: @escaping @MainActor () -> Void) {
        guard pending.insert(key).inserted, size.width >= 1, size.height >= 1 else { return }
        let theme = Theme.pele(dark: dark)
        queue.async {
            let svg = resolvePeleSVG(d.svg, theme: theme)
            if let image = rasterize(svg, size: size, scale: scale) {
                cache.setObject(Box(image), forKey: key as NSString, cost: image.bytesPerRow * image.height)
            }
            onMain {
                pending.remove(key)
                done()
            }
        }
    }

    static func rasterize(_ svg: String, size: CGSize, scale: CGFloat) -> CGImage? {
        guard let image = NSImage(data: Data(svg.utf8)) else { return nil }
        let w = Int(ceil(size.width * scale)), h = Int(ceil(size.height * scale))
        guard let ctx = CGContext(data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: 0,
                                  space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return nil }
        ctx.scaleBy(x: scale, y: scale)
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(cgContext: ctx, flipped: false)
        image.draw(in: NSRect(origin: .zero, size: size))
        NSGraphicsContext.restoreGraphicsState()
        return ctx.makeImage()
    }

    /// The diagram in its own resizable, zoomable window, drawn as vector.
    @MainActor
    static func openFullscreen(_ d: PeleDiagram, dark: Bool) {
        guard let image = NSImage(data: Data(resolvePeleSVG(d.svg, theme: Theme.pele(dark: dark)).utf8)) else { return }
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: min(1200, d.width + 80), height: min(900, d.height + 80)),
                              styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.title = "Diagram"
        let imageView = NSImageView(image: image)
        imageView.imageScaling = .scaleProportionallyUpOrDown
        imageView.frame = NSRect(x: 0, y: 0, width: d.width, height: d.height)
        let scroll = NSScrollView()
        scroll.documentView = imageView
        scroll.allowsMagnification = true
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        scroll.backgroundColor = Theme.card
        window.contentView = scroll
        window.center()
        window.makeKeyAndOrderFront(nil)
    }
}

/// The flat composer floating over the bottom of the timeline. It comments on one
/// post (the last, or the one picked), optionally anchored to lines or a point.
@MainActor
final class Composer: NSObject, NSTextFieldDelegate {
    static let height: CGFloat = 62
    let view = NSView()
    let context = label(size: 11, color: Theme.muted)
    let clearAnchor = NSButton()
    let field = NSTextField()
    private weak var owner: PostsView?
    private(set) var post: String?
    private(set) var anchor: CommentAnchor?
    private var sending = false

    init(owner: PostsView) {
        self.owner = owner
        super.init()
        view.wantsLayer = true
        view.layer?.cornerRadius = 12
        view.layer?.cornerCurve = .continuous
        view.layer?.borderWidth = 1
        field.placeholderString = "Leave a comment…"
        field.font = .systemFont(ofSize: 13)
        field.isBordered = false
        field.drawsBackground = false
        field.focusRingType = .none
        field.delegate = self
        field.cell?.wraps = false
        field.cell?.isScrollable = true
        clearAnchor.image = NSImage(systemSymbolName: "xmark.circle.fill", accessibilityDescription: "Clear anchor")
        clearAnchor.isBordered = false
        clearAnchor.contentTintColor = Theme.faint
        clearAnchor.target = self
        clearAnchor.action = #selector(dropAnchor)
        clearAnchor.isHidden = true
        for v in [context, clearAnchor, field] as [NSView] {
            v.translatesAutoresizingMaskIntoConstraints = false
            view.addSubview(v)
        }
        NSLayoutConstraint.activate([
            context.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 14),
            context.topAnchor.constraint(equalTo: view.topAnchor, constant: 9),
            clearAnchor.leadingAnchor.constraint(equalTo: context.trailingAnchor, constant: 4),
            clearAnchor.centerYAnchor.constraint(equalTo: context.centerYAnchor),
            clearAnchor.trailingAnchor.constraint(lessThanOrEqualTo: view.trailingAnchor, constant: -14),
            field.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 12),
            field.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -12),
            field.bottomAnchor.constraint(equalTo: view.bottomAnchor, constant: -10),
        ])
        context.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        updateColors()
        update()
    }

    func updateColors() {
        view.effectiveAppearance.performAsCurrentDrawingAppearance {
            view.layer?.backgroundColor = Theme.card.cgColor
            view.layer?.borderColor = Theme.cardLine.cgColor
        }
    }

    func reset() {
        post = nil
        anchor = nil
        update()
    }

    /// Picks the post to comment on (the header's comment button).
    func target(post id: String) {
        if id != post { anchor = nil }
        post = id
        update()
        view.window?.makeFirstResponder(field)
    }

    /// Option-click or a line selection: comment on that spot.
    func set(post id: String, anchor: CommentAnchor) {
        post = id
        self.anchor = anchor
        update()
        if case .point = anchor { view.window?.makeFirstResponder(field) }
    }

    @objc func dropAnchor() {
        anchor = nil
        update()
    }

    func update() {
        updateColors()
        let id = post ?? owner?.lastPostID
        let title = id.flatMap { owner?.post($0)?.post.title }
        let target = title.map { "Comment on “\($0)”" } ?? (id == nil ? "Comment on this session" : "Comment on this post")
        context.stringValue = target + (anchor.map { " · \($0.summary)" } ?? "")
        clearAnchor.isHidden = anchor == nil
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        switch selector {
        case #selector(NSResponder.insertNewline(_:)):
            send()
            return true
        case #selector(NSResponder.cancelOperation(_:)):
            if anchor != nil { dropAnchor() } else { view.window?.makeFirstResponder(nil) }
            return true
        default:
            return false
        }
    }

    func send() {
        let text = field.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, !sending, let owner else { return }
        sending = true
        owner.sendComment(post: post ?? owner.lastPostID, text: text, anchor: anchor) { [weak self] ok in
            guard let self else { return }
            sending = false
            guard ok else { return }
            field.stringValue = ""
            anchor = nil
            update()
        }
    }
}
