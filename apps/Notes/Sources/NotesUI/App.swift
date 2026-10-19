import AppKit
import NotesCore

@MainActor
final class App: NSObject, NSApplicationDelegate, NSWindowDelegate, NSMenuItemValidation {
    private let window = NSWindow(
        contentRect: NSRect(x: 0, y: 0, width: 1000, height: 680),
        styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
        backing: .buffered, defer: false)
    private let sidebar = Sidebar()
    private let editor = Editor()
    private let main = NSView()
    private let offline = NSStackView()
    private let offlineDetail = NSTextField(wrappingLabelWithString: "")

    private var connecting = false
    private var watch: Watch?
    private var lastSeq: UInt64?
    private var listing = false
    private var listAgain = false

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.mainMenu = menu()
        buildWindow()
        sidebar.onOpen = { [weak self] path in self?.open(path) }
        sidebar.onCreate = { [weak self] name in self?.create(name) }
        window.makeKeyAndOrderFront(nil)
        NSApp.activate()
        connect()
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }

    func applicationWillTerminate(_ notification: Notification) {
        editor.saveSynchronously()
        watch?.cancel()
    }

    // MARK: Window

    private func buildWindow() {
        window.titlebarAppearsTransparent = true
        window.titleVisibility = .hidden
        window.title = "Notes"
        window.minSize = NSSize(width: 600, height: 400)
        window.delegate = self
        window.center()
        window.setFrameAutosaveName("Main")

        let root = NSView()
        for v in [sidebar.view, editor.view] {
            v.translatesAutoresizingMaskIntoConstraints = false
            main.addSubview(v)
        }
        let divider = NSBox()
        divider.boxType = .separator
        divider.translatesAutoresizingMaskIntoConstraints = false
        main.addSubview(divider)

        let title = NSTextField(labelWithString: "notesd isn't running")
        title.font = .systemFont(ofSize: 17, weight: .semibold)
        offlineDetail.alignment = .center
        offlineDetail.textColor = .secondaryLabelColor
        offlineDetail.preferredMaxLayoutWidth = 380
        let retry = NSButton(title: "Retry", target: self, action: #selector(retry))
        offline.setViews([title, offlineDetail, retry], in: .center)
        offline.orientation = .vertical
        offline.spacing = 10
        offline.isHidden = true

        for v in [main, offline] as [NSView] {
            v.translatesAutoresizingMaskIntoConstraints = false
            root.addSubview(v)
        }
        NSLayoutConstraint.activate([
            main.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            main.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            main.topAnchor.constraint(equalTo: root.topAnchor),
            main.bottomAnchor.constraint(equalTo: root.bottomAnchor),
            sidebar.view.leadingAnchor.constraint(equalTo: main.leadingAnchor),
            sidebar.view.topAnchor.constraint(equalTo: main.topAnchor),
            sidebar.view.bottomAnchor.constraint(equalTo: main.bottomAnchor),
            sidebar.view.widthAnchor.constraint(equalToConstant: 200),
            divider.leadingAnchor.constraint(equalTo: sidebar.view.trailingAnchor),
            divider.topAnchor.constraint(equalTo: main.topAnchor),
            divider.bottomAnchor.constraint(equalTo: main.bottomAnchor),
            divider.widthAnchor.constraint(equalToConstant: 1),
            editor.view.leadingAnchor.constraint(equalTo: divider.trailingAnchor),
            editor.view.trailingAnchor.constraint(equalTo: main.trailingAnchor),
            editor.view.topAnchor.constraint(equalTo: main.topAnchor),
            editor.view.bottomAnchor.constraint(equalTo: main.bottomAnchor),
            offline.centerXAnchor.constraint(equalTo: root.centerXAnchor),
            offline.centerYAnchor.constraint(equalTo: root.centerYAnchor),
            offline.widthAnchor.constraint(lessThanOrEqualTo: root.widthAnchor, constant: -40),
        ])
        window.contentView = root
    }

    // Reconnect only when the user comes back to the window (no polling).
    func windowDidBecomeKey(_ notification: Notification) {
        if watch == nil { connect() }
    }

    // MARK: Daemon

    @objc private func retry() { connect() }

    /// List the vault, start watching, open @last. Unreachable shows the offline state.
    /// Watch starts before the list so no change slips between them.
    private func connect() {
        guard !connecting else { return }
        do { try startWatch() } catch { return setOffline(error) }
        connecting = true
        daemon({ try $0.list() }) { [weak self] result in
            guard let self else { return }
            connecting = false
            switch result {
            case .success(let entries):
                setOffline(nil)
                sidebar.update(entries)
                if editor.path == nil { open("@last") }
            case .failure(let error):
                setOffline(error)
            }
        }
    }

    private func setOffline(_ error: Error?) {
        if let error {
            offlineDetail.stringValue = """
                Notes reads and writes through the notesd daemon. Start it (launchd starts it on \
                first connect once its agent is loaded, or run `notesd` in a terminal), then retry.

                \(error.message)
                """
        }
        offline.isHidden = error == nil
        main.isHidden = error != nil
    }

    /// One long-lived `watch` connection on its own thread, parked in read() while idle.
    private func startWatch() throws {
        guard watch == nil else { return }
        let watch = try client.watch(since: lastSeq)
        self.watch = watch
        Thread.detachNewThread { [self] in
            while let event = try? watch.next() {
                onMain { self.handle(event) }
            }
            onMain { if self.watch === watch { self.watch = nil } }
        }
    }

    private func handle(_ event: Event) {
        lastSeq = max(lastSeq ?? 0, event.seq)
        refreshList()
        if let path = event.path, path == editor.path, !editor.isDirty, event.version != editor.version {
            daemon({ try $0.read(path) }) { [weak self] result in
                switch result {
                case .success(let note): self?.editor.refresh(note)
                case .failure(let error) where error.daemonCode == .notFound:
                    if self?.editor.path == path, self?.editor.isDirty == false { self?.editor.clear() }
                case .failure: break
                }
            }
        }
    }

    /// Re-lists the vault; bursts of events coalesce into one extra list.
    private func refreshList() {
        guard !listing else { return listAgain = true }
        listing = true
        daemon({ try $0.list() }) { [weak self] result in
            guard let self else { return }
            listing = false
            if case .success(let entries) = result { sidebar.update(entries) }
            if listAgain {
                listAgain = false
                refreshList()
            }
        }
    }

    private func open(_ ref: String) {
        editor.flush()
        daemon({ try $0.read(ref) }) { [weak self] result in
            guard let self, case .success(let note) = result else { return }
            editor.load(note)
            sidebar.select(path: note.path)
        }
    }

    private func create(_ name: String) {
        let path = sidebar.currentFolder.map { "\($0)/\(name)" } ?? name
        daemon({ try $0.new(path) }) { [weak self] result in
            guard let self else { return }
            switch result {
            case .success(let written):
                daemon({ try $0.list() }) { [weak self] listed in
                    if case .success(let entries) = listed { self?.sidebar.update(entries) }
                    self?.open(written.path)
                    self?.window.makeFirstResponder(self?.editor.textView)
                }
            case .failure(let error):
                NSSound.beep()
                NSLog("new %@ failed: %@", path, error.message)
            }
        }
    }

    // MARK: Menu

    @objc private func newNote(_ sender: Any?) { sidebar.askForName() }

    @objc private func deleteNote(_ sender: Any?) {
        guard let node = sidebar.selectedNode, !node.isFolder else { return }
        let path = node.path
        daemon({ try $0.delete(path) }) { [weak self] result in
            guard let self else { return }
            if case .failure(let error) = result { return NSLog("delete %@ failed: %@", path, error.message) }
            if editor.path == path { editor.clear() }
            refreshList()
        }
    }

    func validateMenuItem(_ item: NSMenuItem) -> Bool {
        switch item.action {
        // ⌘⌫ only deletes from the sidebar; in the editor it stays "delete to line start".
        case #selector(deleteNote(_:)):
            window.firstResponder === sidebar.outline && sidebar.selectedNode?.isFolder == false
        case #selector(newNote(_:)):
            main.isHidden == false
        default: true
        }
    }

    private func menu() -> NSMenu {
        let bar = NSMenu()
        func submenu(_ title: String, _ items: [NSMenuItem]) {
            let menu = NSMenu(title: title)
            items.forEach(menu.addItem)
            let item = NSMenuItem(title: title, action: nil, keyEquivalent: "")
            item.submenu = menu
            bar.addItem(item)
        }
        func item(_ title: String, _ action: Selector, _ key: String, _ mods: NSEvent.ModifierFlags = .command) -> NSMenuItem {
            let item = NSMenuItem(title: title, action: action, keyEquivalent: key)
            item.keyEquivalentModifierMask = mods
            return item
        }
        submenu("Notes", [
            item("Hide Notes", #selector(NSApplication.hide(_:)), "h"),
            .separator(),
            item("Quit Notes", #selector(NSApplication.terminate(_:)), "q"),
        ])
        let newItem = item("New Note", #selector(newNote(_:)), "n")
        let deleteItem = item("Delete Note", #selector(deleteNote(_:)), "\u{8}")
        newItem.target = self
        deleteItem.target = self
        submenu("File", [newItem, deleteItem, .separator(), item("Close", #selector(NSWindow.performClose(_:)), "w")])
        submenu("Edit", [
            item("Undo", Selector(("undo:")), "z"),
            item("Redo", Selector(("redo:")), "z", [.command, .shift]),
            .separator(),
            item("Cut", #selector(NSText.cut(_:)), "x"),
            item("Copy", #selector(NSText.copy(_:)), "c"),
            item("Paste", #selector(NSText.paste(_:)), "v"),
            item("Select All", #selector(NSText.selectAll(_:)), "a"),
        ])
        submenu("Window", [item("Minimize", #selector(NSWindow.performMiniaturize(_:)), "m")])
        return bar
    }
}

/// Runs the app; never returns.
@MainActor
public func runApp() -> Never {
    let app = NSApplication.shared
    let delegate = App()
    app.delegate = delegate
    app.setActivationPolicy(.regular)
    app.run()
    exit(0)
}
