# stage-helper

The Swift side of stage: a helper process that stage (the Rust TUI) talks to with JSON lines on
stdin/stdout. It renders mermaid to SVG with pele, decodes images, and runs live html parts in
a WKWebView, handing pixels back through POSIX shared memory for kitty graphics. The package
keeps its `Notes` name.

## Layout

- `Sources/NotesCore`: the helper's wire types (`HelperProtocol.swift`, mirrored from
  `crates/proto/src/helper.rs`) and mermaid via pele in one JavaScriptCore context (`Pele.swift`,
  vendored `Resources/pele.global.js` 1.6.0, MIT). No AppKit.
- `Sources/StageHelper`: the `stage-helper` executable (`main.swift`, html pages in `Page.swift`).
- `Sources/StageHelper/Shm`: `StageShm`, a C shim because `shm_open` is variadic.
- `Tests/NotesCoreTests`: the wire fixtures in `crates/proto/tests/fixtures/helper` and the
  helper's behavior.

## Build and test

```sh
swift build && swift test            # in apps/Notes
```

The animating-page frame-rate test (`animatingPageStreamsFramesWhileAcked`) needs a real
display and a quiet machine.
