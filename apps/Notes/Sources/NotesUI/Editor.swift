import AppKit
import NotesCore

/// The reading/editing pane: one TextKit 2 text view in a centered column, an inline
/// banner for conflicts and save errors, and the save-after-idle logic.
@MainActor
final class Editor: NSObject, NSTextViewDelegate, NSTextStorageDelegate {
    let view = NSView()
    let textView = ColumnTextView(usingTextLayoutManager: true)
    let banner = NSStackView()
    let bannerLabel = NSTextField(wrappingLabelWithString: "")
    private let bannerPrimary = NSButton(title: "", target: nil, action: nil)
    private let bannerSecondary = NSButton(title: "", target: nil, action: nil)

    private(set) var path: String?
    private(set) var version: UInt64?
    private(set) var isDirty = false
    /// The other side of a version conflict; autosave pauses until the user picks.
    private var theirs: Note?
    private var loading = false
    private var saving = false
    private var edits = 0
    /// Exists only while there are unsaved changes.
    private var pendingSave: DispatchWorkItem?

    override init() {
        super.init()
        view.wantsLayer = true
        view.layer?.backgroundColor = NSColor.textBackgroundColor.withAlphaComponent(0.97).cgColor

        textView.delegate = self
        textView.textStorage?.delegate = self
        textView.isRichText = false
        textView.allowsUndo = true
        textView.drawsBackground = false
        textView.font = Style.body
        textView.textColor = .labelColor
        textView.typingAttributes = Style.base
        textView.isAutomaticQuoteSubstitutionEnabled = false
        textView.isAutomaticDashSubstitutionEnabled = false
        textView.isAutomaticTextReplacementEnabled = false
        textView.isVerticallyResizable = true
        textView.isHorizontallyResizable = false
        textView.autoresizingMask = [.width]
        textView.minSize = .zero
        textView.maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: .greatestFiniteMagnitude)
        textView.textContainer?.widthTracksTextView = true
        textView.isEditable = false

        let scroll = NSScrollView()
        scroll.documentView = textView
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true

        bannerLabel.font = .systemFont(ofSize: 12)
        bannerLabel.textColor = .secondaryLabelColor
        bannerLabel.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        for button in [bannerPrimary, bannerSecondary] {
            button.bezelStyle = .push
            button.controlSize = .small
            button.target = self
        }
        bannerPrimary.action = #selector(bannerPrimaryClicked)
        bannerSecondary.action = #selector(bannerSecondaryClicked)
        banner.setViews([bannerLabel, bannerPrimary, bannerSecondary], in: .leading)
        banner.spacing = 8
        banner.edgeInsets = NSEdgeInsets(top: 8, left: 14, bottom: 8, right: 14)
        banner.wantsLayer = true
        banner.layer?.cornerRadius = 10
        banner.layer?.backgroundColor = NSColor.systemYellow.withAlphaComponent(0.14).cgColor
        banner.isHidden = true

        for v in [scroll, banner] as [NSView] {
            v.translatesAutoresizingMaskIntoConstraints = false
            view.addSubview(v)
        }
        NSLayoutConstraint.activate([
            scroll.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            scroll.topAnchor.constraint(equalTo: view.topAnchor),
            scroll.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            banner.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor, constant: 8),
            banner.centerXAnchor.constraint(equalTo: view.centerXAnchor),
            banner.widthAnchor.constraint(lessThanOrEqualToConstant: 640),
            banner.widthAnchor.constraint(lessThanOrEqualTo: view.widthAnchor, constant: -32),
        ])
    }

    // MARK: Loading

    /// Shows a note; any unsaved text for the previous note is saved first.
    func load(_ note: Note) {
        if path != note.path { flush() }
        path = note.path
        version = note.version
        isDirty = false
        theirs = nil
        hideBanner()
        setText(note.body)
        textView.isEditable = true
        textView.undoManager?.removeAllActions()
        textView.setSelectedRange(NSRange(location: 0, length: 0))
        textView.scrollToBeginningOfDocument(nil)
    }

    func clear() {
        flush()
        path = nil
        version = nil
        isDirty = false
        theirs = nil
        hideBanner()
        setText("")
        textView.isEditable = false
    }

    /// The open note changed elsewhere and we have nothing unsaved: take theirs in place.
    func refresh(_ note: Note) {
        guard note.path == path, !isDirty, theirs == nil else { return }
        version = note.version
        guard note.body != textView.string else { return }
        let selection = textView.selectedRange()
        setText(note.body)
        let length = (note.body as NSString).length
        textView.setSelectedRange(NSRange(location: min(selection.location, length), length: 0))
    }

    private func setText(_ body: String) {
        loading = true
        textView.string = body
        loading = false
    }

    // MARK: Saving

    func textDidChange(_ notification: Notification) {
        guard !loading, path != nil else { return }
        isDirty = true
        edits += 1
        scheduleSave()
    }

    private func scheduleSave() {
        pendingSave?.cancel()
        guard theirs == nil else { return pendingSave = nil }
        let item = DispatchWorkItem { [weak self] in MainActor.assumeIsolated { self?.save() } }
        pendingSave = item
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.4, execute: item)
    }

    /// Saves now instead of waiting for the idle pause. Leaving a note mid-conflict
    /// keeps the user's text; theirs stays in the daemon's history.
    func flush() {
        guard isDirty else { return }
        save(ifVersion: theirs?.version)
    }

    private func save(ifVersion override: UInt64? = nil) {
        pendingSave?.cancel()
        pendingSave = nil
        guard let path, isDirty, !saving else { return }
        let body = textView.string
        let sentEdits = edits
        let ifVersion = override ?? version
        saving = true
        daemon({ try $0.write(path, body: body, ifVersion: ifVersion) }) { [weak self] result in
            guard let self else { return }
            saving = false
            // ponytail: the result of a save for a note we've already left is dropped, and
            // a switch during an in-flight save skips the flush; a per-note outbox fixes both.
            guard path == self.path else { return isDirty ? scheduleSave() : () }
            switch result {
            case .success(let written):
                version = written.version
                theirs = nil
                hideBanner()
                if edits == sentEdits { isDirty = false } else { scheduleSave() }
            case .failure(let error) where error.daemonCode == .versionConflict:
                conflict(path)
            case .failure(let error):
                showBanner("Couldn't save: \(error.message)", primary: "Retry", secondary: nil)
            }
        }
    }

    /// Someone else wrote first. Keep the user's text on screen and let them pick.
    private func conflict(_ path: String) {
        daemon({ try $0.read(path) }) { [weak self] result in
            guard let self, path == self.path else { return }
            switch result {
            case .success(let note):
                theirs = note
                showBanner("This note changed elsewhere (now v\(note.version)). Your text is kept here.",
                           primary: "Keep Mine", secondary: "Use Theirs")
            case .failure(let error):
                showBanner("Couldn't save: \(error.message)", primary: "Retry", secondary: nil)
            }
        }
    }

    @objc func bannerPrimaryClicked() {
        // "Keep Mine" overwrites their version; "Retry" just saves again.
        let over = theirs?.version
        theirs = nil
        hideBanner()
        save(ifVersion: over)
    }

    @objc func bannerSecondaryClicked() {
        guard let note = theirs else { return }
        isDirty = false
        load(note)
    }

    private func showBanner(_ text: String, primary: String, secondary: String?) {
        bannerLabel.stringValue = text
        bannerPrimary.title = primary
        bannerSecondary.title = secondary ?? ""
        bannerSecondary.isHidden = secondary == nil
        banner.isHidden = false
    }

    private func hideBanner() { banner.isHidden = true }

    /// Blocking save for quit, so typing right before ⌘Q isn't lost.
    func saveSynchronously() {
        guard let path, isDirty else { return }
        pendingSave?.cancel()
        if let written = try? client.write(path, body: textView.string, ifVersion: theirs?.version ?? version) {
            version = written.version
            isDirty = false
        }
    }

    // MARK: Markdown styling

    nonisolated func textStorage(_ storage: NSTextStorage, didProcessEditing mask: NSTextStorageEditActions, range: NSRange, changeInLength delta: Int) {
        guard mask.contains(.editedCharacters) else { return }
        Style.restyle(storage, edited: range)
    }
}

/// Text view whose text column stays ~680 pt wide and centered.
final class ColumnTextView: NSTextView {
    override func setFrameSize(_ size: NSSize) {
        super.setFrameSize(size)
        let inset = NSSize(width: max(28, (size.width - 680) / 2), height: 28)
        if textContainerInset != inset { textContainerInset = inset }
    }
}

/// Editor attributes. Fonts and colors are immutable, so sharing them is safe.
enum Style {
    nonisolated(unsafe) static let body = NSFont.systemFont(ofSize: 15)
    nonisolated(unsafe) static let mono = NSFont.monospacedSystemFont(ofSize: 13, weight: .regular)
    nonisolated(unsafe) static let paragraph: NSParagraphStyle = {
        let p = NSMutableParagraphStyle()
        p.lineHeightMultiple = 1.3
        p.paragraphSpacing = 4
        return p
    }()
    nonisolated(unsafe) static let base: [NSAttributedString.Key: Any] = [
        .font: body, .foregroundColor: NSColor.labelColor, .paragraphStyle: paragraph,
    ]
    static let headingSizes: [CGFloat] = [26, 21, 18, 16, 15, 15]
    /// Per paragraph: is a fenced code block still open after it.
    static let inCode = NSAttributedString.Key("notesInCode")

    /// Restyles the paragraphs touched by an edit, then keeps going only while a
    /// fence opened or closed by the edit changes the state of the lines after it.
    static func restyle(_ storage: NSTextStorage, edited: NSRange) {
        let text = storage.string as NSString
        guard text.length > 0 else { return }
        let editedEnd = NSMaxRange(text.paragraphRange(for: edited))
        var location = text.paragraphRange(for: NSRange(location: edited.location, length: 0)).location
        var inCode = location > 0 && storage.attribute(inCode, at: location - 1, effectiveRange: nil) as? Bool == true
        while location < text.length {
            var end = 0, contentsEnd = 0
            text.getParagraphStart(nil, end: &end, contentsEnd: &contentsEnd, for: NSRange(location: location, length: 0))
            let paragraph = NSRange(location: location, length: end - location)
            let wasInCode = storage.attribute(Style.inCode, at: end - 1, effectiveRange: nil) as? Bool == true
            let line = text.substring(with: NSRange(location: location, length: contentsEnd - location))
            let (spans, after) = markdownSpans(line: line, inCode: inCode)
            storage.setAttributes(base, range: paragraph)
            storage.addAttribute(Style.inCode, value: after, range: paragraph)
            for span in spans { apply(span, at: location, to: storage) }
            location = end
            inCode = after
            if location >= editedEnd && after == wasInCode { break }
        }
    }

    private static func apply(_ span: MarkdownSpan, at offset: Int, to storage: NSTextStorage) {
        let range = NSRange(location: span.range.location + offset, length: span.range.length)
        guard range.length > 0 else { return }
        switch span.style {
        case .heading(let level):
            storage.addAttribute(.font, value: NSFont.systemFont(ofSize: headingSizes[level - 1], weight: .bold), range: range)
        case .marker:
            storage.addAttribute(.foregroundColor, value: NSColor.tertiaryLabelColor, range: range)
        case .bold, .italic:
            let trait: NSFontTraitMask = span.style == .bold ? .boldFontMask : .italicFontMask
            storage.enumerateAttribute(.font, in: range) { value, sub, _ in
                guard let font = value as? NSFont else { return }
                storage.addAttribute(.font, value: NSFontManager.shared.convert(font, toHaveTrait: trait), range: sub)
            }
        case .code:
            storage.addAttributes([.font: mono, .backgroundColor: NSColor.quaternarySystemFill], range: range)
        case .codeBlock:
            storage.addAttributes([.font: mono, .backgroundColor: NSColor.quinarySystemFill], range: range)
        }
    }
}
