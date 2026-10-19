// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "Notes",
    platforms: [.macOS(.v26)],
    targets: [
        // Protocol models, socket client, markdown spans, pele (JavaScriptCore); no AppKit.
        // pele 1.6.0 (MIT, Resources/pele-LICENSE); build-app.sh copies both into the app.
        .target(name: "NotesCore", resources: [.copy("Resources/pele.global.js"), .copy("Resources/pele-LICENSE")]),
        // Window, sidebar, editor and posts; a library so tests can drive the views.
        .target(name: "NotesUI", dependencies: ["NotesCore"]),
        .executableTarget(name: "Notes", dependencies: ["NotesUI"]),
        .testTarget(name: "NotesCoreTests", dependencies: ["NotesCore", "NotesUI"]),
    ]
)
