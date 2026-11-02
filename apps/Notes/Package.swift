// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "Notes",
    platforms: [.macOS(.v26)],
    targets: [
        // stage-helper's wire models and pele (JavaScriptCore); no AppKit.
        // pele 1.6.0 (MIT, Resources/pele-LICENSE).
        .target(name: "NotesCore", resources: [.copy("Resources/pele.global.js"), .copy("Resources/pele-LICENSE")]),
        // stage's pixel helper (JSON lines on stdin/stdout): pele SVG, image decode, live
        // HTML frames into POSIX shm. StageShm is a C shim because shm_open is variadic.
        .target(name: "StageShm", path: "Sources/StageHelper/Shm"),
        .executableTarget(name: "stage-helper", dependencies: ["NotesCore", "StageShm"], path: "Sources/StageHelper", exclude: ["Shm"]),
        .testTarget(name: "NotesCoreTests", dependencies: ["NotesCore", "StageShm", "stage-helper"]),
    ]
)
