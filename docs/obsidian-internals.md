# Obsidian 1.14.4 internals (REA + source read, 2026-10-18)

Static analysis of `/Applications/Obsidian.app` with REA (`rea-agents` 6.7.0) plus
a direct read of the extracted asars. Nothing was executed. Minified identifiers
and `@N` character offsets into `obsidian.asar/app.js` are grep anchors.

Rerun:
```
rea analyze-javascript-application <Resources>/app.asar --integrity-policy record-and-continue --format json
rea analyze-javascript-application <Resources>/obsidian.asar --max-heap-mb 14336 --analysis-timeout-ms 5400000 --format json
rea inspect-electron-fuses|inspect-signature|inspect-plist|trace-dylib-resolution|inspect-asset-catalog /Applications/Obsidian.app
```
`recover-javascript-sources` needs Wakaru (Linux x64 only), so module recovery wasn't possible on macOS.

## Bundle and native shell
- Electron 43.7.7 (496 MB of the 528 MB app), Squirrel + Mantle + ReactiveObjC for updates, helpers: GPU, Renderer, Plugin.
- Signed `md.obsidian`, team 6JSW4SJWN9, hardened runtime; entitlements: allow-jit, allow-unsigned-executable-memory, disable-library-validation, audio-input, calendars. Built against SDK 26.5, min macOS 12.
- Electron fuses: RunAsNode, NODE_OPTIONS and `--inspect` off; asar integrity validation and OnlyLoadAppFromAsar off; cookie encryption on.
- Info.plist: `obsidian://` URL scheme, editor for `.md`, `.canvas`, `.base`; viewer for everything else; `NSPrincipalClass AtomApplication`.
- Native addons: `btime` (set file birthtime), `get-fonts` (Core Text font list, `fontsMac.mm`).
- `ObsidianQuickLook.appex`: the only native UI. Swift + WebKit, `PreviewViewController`, renders markdown with `quicklook.js` in a `WKWebView`, plus an XPC service for links. Main process extracts quicklook/prism/mathjax/mermaid assets to `userData/quicklook/<sha256>` behind a `current` symlink.

## Process model
- `app.asar` is a loader/updater: loads the newest signed `obsidian-<ver>.asar` from userData, else the bundled one; polls GitHub release JSON hourly.
- `obsidian.asar/main.js` is the real main process: 47 `ipcMain` channels, mostly `sendSync` (`vault*`, `trash`, `print-to-pdf`, `cli`, `register-cli`, `create-browser-session`, `adblock-*`, `request-url`, ...).
- Renderer: `nodeIntegration:true, contextIsolation:false, webviewTag:true`, does its own file I/O. Window: hidden title bar, `trafficLightPosition {19,12}`, transparent background, shown after load.
- Translucency is opt-in ("Translucent window", macOS only): `setVibrancy("sidebar")` + `body.is-translucent`.
- Workers: Metadata Cache (`worker.js`), Graph force sim (`sim.js`, d3-force), pdf.js, a blob worker on selection change.

## Storage
- Vault = plain files. Writes are `fs.writeFile` through a serial queue, not atomic. Editor save debounced 2 s.
- Watcher: `fs.watch(root,{recursive:true})`; reconcile by `mtime`+`size`.
- Metadata cache: IndexedDB `<appId>-cache` v19; `file[path]={mtime,size,hash}`, `metadata[hash]={links,embeds,tags,headings,sections,blocks,listItems,frontmatter,...}`; content-hash keyed, `durability:"relaxed"`; parsed in the worker with old remark-parse.
- File recovery: IndexedDB `<appId>-backup`; full-text snapshots `{path,ts,data}` of md/canvas/base every 5 min on change, kept 7 days; diff via diff-match-patch.
- Config in `<vault>/.obsidian/`: `app.json`, `appearance.json`, `types.json`, `core-plugins.json`, `community-plugins.json`, `hotkeys.json`, `workspace.json`, `plugins/<id>/{manifest.json,main.js,styles.css,data.json}`, `themes/<name>/theme.css`, `snippets/*.css`. localStorage for small prefs.

## Search
- No full-text index. Global search scans `vault.cachedRead()` for every file per query.
- Quick switcher fuzzy: token substring, then char fuzzy; score penalises gaps, misses, span, first index, length; boundary bonus.
- Bases queries are also a full scan of `getMarkdownFiles()` against a filter tree.

## Editor and rendering
- CodeMirror 6 bundled in `app.js`; `lib/codemirror` is CM5 for language modes and vim. Live Preview = CM6 + replace-widget decorations; source mode toggles a class.
- Reading view: per-section render with cached heights, only `max(viewport, 500px)` above/below rendered; post-processors per section; recyclers keep Mermaid SVG across virtualization.
- Lazy-loaded: Mermaid (stock, `securityLevel:"strict"`, not pele), MathJax CHTML, Prism, pdf.js, pixi.js (graph, WebGL).
- Canvas is DOM nodes + one SVG for edges. File explorer is a full DOM tree.
- Theming: `app.css` 668 KB, 1177 CSS variables; `theme-light`/`theme-dark` body classes driven by `prefers-color-scheme`.
- i18n: 51 locales as line-per-key `.txt` files.

## Features
- **Bases** (`.base`, YAML): `views, filters, formulas, properties, summaries`; views table/cards/list/kanban; filter ops (`contains`, `hasTag`, `inFolder`, `hasLink`, ...); formula library (`if`, `date`, `map`, `filter`, `sum`, `median`, ...); built-in `file.*` props; CLI `base:query` outputs json/csv/tsv/md/paths.
- **Canvas** (`.canvas`, JSON): nodes `{id,x,y,width,height,color,type:text|file|link|group}`, edges `{fromNode,fromSide,toNode,toSide,fromEnd,toEnd,label}`; unknown keys round-trip.
- **Properties**: `types.json`; widgets text, multitext, number, checkbox, date, datetime, aliases, tags; type inferred from values.
- **Web viewer**: sandboxed `<webview>` in its own session partition, easylist adblock refreshed every 4 days, Readability for reader mode, UA stripped.
- **Sync**: file-level over WebSocket to `*.obsidian.md` only, E2E fields; conflicts via 3-way text merge (backup first) or "Conflicted copy"; JSON key merge. No CRDT/OT.

## Extensibility
- `obsidian` module shim: 159 exports (Plugin, ItemView, Modal, Setting, Workspace*, Vault, MetadataCache, MarkdownRenderer, Bases*, SecretStorage, `requestUrl`, `loadMermaid`, ...).
- Plugins: `main.js` read from disk and run with `window.eval` wrapped in a CommonJS function; full Node access, no sandbox. 22 core plugins share the same API.
- Workspace: tree of `split` / `tabs` / `leaf` / `floating`/`window` (popouts), serialized to `workspace.json` with 1 s debounce.
- ~134 commands with default hotkeys; `hotkeys.json` overrides.

## CLI and agent surface
- `obsidian://` URIs.
- Official CLI: a second app instance connects to `~/.obsidian-cli.sock`, sends `{"argv":[...],"tty":bool,"cwd":"..."}\n`, streams text back; main runs `handleCli(argv)` in the renderer via `executeJavaScript`.
- ~100 commands (`search`, `read`, `create`, `append`, `move`, `property:set`, `backlinks`, `tasks`, `base:query`, `history:*`, `sync:*`, `eval`, `dev:screenshot`, ...), many with `format=json`. Off by default; `register-cli` symlinks `/usr/local/bin/obsidian`.

## Network
- `releases.obsidian.md` (CORS proxy, theme stats, YouTube shim), `api.obsidian.md` (account, vaults, subscription, publish), `publish.obsidian.md`, GitHub raw for plugin/theme registries and releases, easylist for adblock, sync WebSocket.
- No telemetry SDK found (Sentry, Mixpanel, PostHog, Amplitude, Datadog, Bugsnag, crashReporter all absent). GA only inside Publish sites with a user-supplied ID.

## Security posture
- Unsandboxed renderer with Node; defences are navigation blocks, a denying window-open handler, forced-sandbox webviews, an `app://` handler confined to the vault root, DOMPurify for HTML, strict Mermaid. `index.html` CSP covers styles only.

## What this means for us
- **CLI:** same shape, cheaper path. Theirs pays an Electron launch plus a renderer JS hop per call; ours is a ~7 ms Rust binary to a resident daemon. Their verb list and `format=json` convention are a good reference.
- **Search and Bases-style queries:** they scan every file every time; SQLite FTS5 plus indexed property columns gives us real indexes.
- **Durability:** non-atomic writes and a 2 s debounce define their loss window; SQLite WAL with an explicit checkpoint policy is better. Their 5-minute full-text snapshots are what Loro history gives us for free.
- **Copy:** content-hash-keyed parse cache; section virtualization with recyclers (maps to per-block parse and reused `WKWebView`s); lazy-load every heavy renderer; their Canvas/Base file formats if we ever want import.
- **Avoid:** eval'd unsandboxed plugins; renderer with Node.
- **Glass:** their vibrancy is opt-in and CSS-driven; ours is `NSGlassEffectView` by default.
