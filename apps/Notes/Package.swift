// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "Notes",
    platforms: [.macOS(.v26)],
    targets: [
        // Protocol models, socket client and markdown spans; no AppKit.
        .target(name: "NotesCore"),
        // Window, sidebar and editor; a library so tests can drive the editor.
        .target(name: "NotesUI", dependencies: ["NotesCore"]),
        .executableTarget(name: "Notes", dependencies: ["NotesUI"]),
        .testTarget(name: "NotesCoreTests", dependencies: ["NotesCore", "NotesUI"]),
    ]
)
