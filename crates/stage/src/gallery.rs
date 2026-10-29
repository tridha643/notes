//! Pixels for visual cards: the IO side of graphics. `App` only learns a
//! visual's size (`Msg::VisualReady`); the gallery owns everything else:
//!
//! - `stage-helper` (Swift, next to this binary) turns mermaid into SVG with
//!   pele, decodes images, and renders HTML in an offscreen WKWebView, sending
//!   frames as fresh POSIX shared-memory objects while a page animates;
//! - resvg rasterizes mermaid SVG on a worker thread at the card's pixel width;
//! - kitty graphics show it: each image is transmitted once (`t=s`), then
//!   placed, cropped and moved as the feed scrolls; live HTML frames replace
//!   their image in place. An image that leaves the screen loses its placement
//!   and a page stops capturing until it's back.

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    os::unix::{net::UnixStream, process::CommandExt},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{mpsc::Sender, Arc, OnceLock},
    thread,
};

use serde_json::{json, Value};

use crate::{
    app::{Msg, OnScreen, Target, VisualKey},
    cards::VisualSource,
    client::StageClient,
    kitty::{self, Place},
};

/// Graphics events for `main` to hand to [`Gallery::handle`].
#[derive(Debug)]
pub enum Gfx {
    /// One JSON line from stage-helper.
    Helper(String),
    HelperExited,
    /// A mermaid diagram rasterized into shared memory (or why it couldn't be).
    Raster { key: VisualKey, epoch: u64, done: Result<(String, u32, u32), String> },
}

/// CSS px → device px for helper renders (Retina).
const SCALE: f64 = 2.0;
/// Limit very large diagrams without shrinking ordinary tall ones to the viewport.
const MAX_MERMAID_HEIGHT_PX: u32 = 4096;

struct Img {
    id: u32,
    w: u32,
    h: u32,
    placed: Option<Place>,
    /// The old image stays at its original size until the resized one is placed.
    previous: Option<u32>,
    /// The html page streaming into this image, if live.
    page: Option<u64>,
    paused: bool,
    /// The image file these pixels came from (PNG sent as a file, or decoded by
    /// the helper), to send again after a clear.
    file: Option<PathBuf>,
}

/// The connection to the shared helper (see `stage-helper --serve`).
struct Helper {
    stream: UnixStream,
}

enum Pending {
    Mermaid(VisualKey),
    Image(VisualKey, PathBuf),
}

pub struct Gallery {
    tx: Sender<Msg>,
    client: StageClient,
    helper: Option<Helper>,
    next_req: u64,
    pending: HashMap<u64, Pending>,
    pages: HashMap<u64, VisualKey>,
    next_id: u32,
    images: HashMap<VisualKey, Img>,
    /// Mermaid SVG by visual, kept so a resize can rasterize it again crisply.
    svgs: HashMap<VisualKey, String>,
    target: Target,
    focused: Option<VisualKey>,
    /// Invalidates raster jobs started for an earlier focus or pane size.
    raster_epoch: u64,
    cache: PathBuf,
    /// The session folder; assets are read straight from its `assets/` (notesd only writes it).
    stage: PathBuf,
    /// `STAGE_TRACE=<file>`: one line per visual, request → pixels transmitted.
    trace: Option<std::fs::File>,
    asked: HashMap<VisualKey, (std::time::Instant, &'static str)>,
    /// While the user scrolls, live pages hold still so the terminal spends its
    /// time on the scroll, not on uploading frames.
    held: bool,
}

fn fonts() -> Arc<resvg::usvg::fontdb::Database> {
    static DB: OnceLock<Arc<resvg::usvg::fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        // Only the faces pele's SVG names (the helper resolves fonts to these), not
        // every system font: loading them all costs ~40 ms on a cold start.
        let mut db = resvg::usvg::fontdb::Database::new();
        for face in ["HelveticaNeue.ttc", "Helvetica.ttc", "Menlo.ttc"] {
            let _ = db.load_font_file(format!("/System/Library/Fonts/{face}"));
        }
        if db.is_empty() {
            db.load_system_fonts();
        }
        Arc::new(db)
    })
    .clone()
}

/// SVG → RGBA at up to 2× its CSS size, within the supplied pixel budget,
/// on a transparent background so glass shows through. The canvas is padded
/// (diagram centered) to whole `cell` px, so kitty shows it 1:1 instead of
/// stretching it to the cells it rounds up to.
pub fn rasterize(svg: &str, width_px: u32, height_px: u32, (cw, ch): (u16, u16)) -> Result<(Vec<u8>, u32, u32), String> {
    let opt = resvg::usvg::Options { fontdb: fonts(), ..Default::default() };
    let tree = resvg::usvg::Tree::from_str(svg, &opt).map_err(|e| format!("svg: {e}"))?;
    let size = tree.size();
    let scale = (width_px as f32 / size.width()).min(height_px as f32 / size.height()).min(SCALE as f32);
    let (w, h) = (
        ((size.width() * scale).ceil() as u32).clamp(1, width_px.max(1)),
        ((size.height() * scale).ceil() as u32).clamp(1, height_px.max(1)),
    );
    let (cw, ch) = (cw.max(1) as u32, ch.max(1) as u32);
    let (pw, ph) = (w.div_ceil(cw) * cw, h.div_ceil(ch) * ch);
    let mut pixmap = resvg::tiny_skia::Pixmap::new(pw, ph).ok_or("empty diagram")?;
    let at = resvg::tiny_skia::Transform::from_scale(scale, scale).post_translate(((pw - w) / 2) as f32, ((ph - h) / 2) as f32);
    resvg::render(&tree, at, &mut pixmap.as_mut());
    // tiny-skia is premultiplied; kitty wants straight RGBA.
    let rgba = pixmap.pixels().iter().flat_map(|p| {
        let c = p.demultiply();
        [c.red(), c.green(), c.blue(), c.alpha()]
    });
    Ok((rgba.collect(), pw, ph))
}

/// One `STAGE_TRACE` line, when tracing.
fn note(trace: &mut Option<std::fs::File>, line: std::fmt::Arguments) {
    if let Some(f) = trace {
        let _ = writeln!(f, "{line}");
    }
}

/// The first bytes of a file (enough for a PNG header).
fn read_head(path: &std::path::Path) -> Vec<u8> {
    use std::io::Read;
    let mut head = vec![0; 32];
    let n = std::fs::File::open(path).and_then(|mut f| f.read(&mut head)).unwrap_or(0);
    head.truncate(n);
    head
}

fn helper_path() -> PathBuf {
    std::env::var_os("STAGE_HELPER").map(PathBuf::from).unwrap_or_else(|| {
        std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|d| d.join("stage-helper")))
            .filter(|p| p.exists())
            .unwrap_or_else(|| PathBuf::from("stage-helper"))
    })
}

impl Gallery {
    pub fn new(tx: Sender<Msg>, client: StageClient, cache: PathBuf, stage: PathBuf, target: Target) -> Gallery {
        Gallery {
            tx,
            client,
            helper: None,
            next_req: 1,
            pending: HashMap::new(),
            pages: HashMap::new(),
            next_id: 1,
            images: HashMap::new(),
            svgs: HashMap::new(),
            target,
            focused: None,
            raster_epoch: 0,
            cache,
            stage,
            trace: std::env::var_os("STAGE_TRACE").and_then(|p| std::fs::File::create(p).ok()),
            asked: HashMap::new(),
            held: false,
        }
    }

    /// Hold (or release) every live page; `place` resumes the visible ones on release.
    pub fn hold_live(&mut self, held: bool) {
        if held == self.held {
            return;
        }
        self.held = held;
        if held {
            let pages: Vec<u64> = self.images.values_mut().filter(|i| !i.paused).filter_map(|i| {
                i.paused = true;
                i.page
            }).collect();
            for page in pages {
                let _ = self.send(json!({"op": "html-pause", "id": page}));
            }
        }
    }

    /// Start the helper and load fonts now (after stage's first frame), so the
    /// first visual doesn't pay for process launch, WebKit and font loading.
    pub fn warm(&mut self) {
        thread::spawn(fonts);
        let _ = self.ask(json!({"op": "warm"}));
    }

    /// Connect to the shared helper, launching it (detached, so it outlives this
    /// stage and the next one starts warm) if nobody answers.
    fn connect(&self) -> Result<UnixStream, String> {
        // Versioned so a stage never talks to a helper from an older protocol behavior.
        let sock = self.cache.join("helper-v3.sock");
        if let Ok(stream) = UnixStream::connect(&sock) {
            return Ok(stream);
        }
        std::fs::create_dir_all(&self.cache).map_err(|e| e.to_string())?;
        Command::new(helper_path())
            .arg("--serve")
            .arg(&sock)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            // `STAGE_HELPER_LOG=<file>` keeps the helper's stderr (with STAGE_HELPER_TRACE=1, its phase timings).
            .stderr(std::env::var_os("STAGE_HELPER_LOG").and_then(|p| std::fs::File::create(p).ok()).map_or(Stdio::null(), Stdio::from))
            .process_group(0)
            .spawn()
            .map_err(|e| format!("stage-helper isn't available ({e})"))?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            match UnixStream::connect(&sock) {
                Ok(stream) => return Ok(stream),
                Err(_) if std::time::Instant::now() < deadline => thread::sleep(std::time::Duration::from_millis(2)),
                Err(e) => return Err(format!("stage-helper didn't start ({e})")),
            }
        }
    }

    /// Send one request to stage-helper, connecting first if needed.
    fn ask(&mut self, mut req: Value) -> Result<u64, String> {
        if self.helper.is_none() {
            let stream = self.connect()?;
            let reader = stream.try_clone().map_err(|e| e.to_string())?;
            let tx = self.tx.clone();
            thread::spawn(move || {
                for line in BufReader::new(reader).lines().map_while(Result::ok) {
                    if tx.send(Msg::Gfx(Gfx::Helper(line))).is_err() {
                        return;
                    }
                }
                let _ = tx.send(Msg::Gfx(Gfx::HelperExited));
            });
            self.helper = Some(Helper { stream });
        }
        let id = self.next_req;
        self.next_req += 1;
        req["id"] = json!(id);
        self.send(req).map(|()| id)
    }

    fn write_cache(&self, name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
        let path = self.cache.join(name);
        std::fs::create_dir_all(&self.cache).and_then(|()| std::fs::write(&path, bytes)).map_err(|e| e.to_string())?;
        Ok(path)
    }

    fn html_size(&self) -> (f64, f64) {
        (self.target.width_px as f64 / SCALE, self.target.max_height_px as f64 / SCALE)
    }

    fn resize_pages(&mut self) {
        let (width, max_height) = self.html_size();
        let pages: Vec<_> = self.pages.iter()
            .filter(|(_, key)| self.focused.as_ref().is_none_or(|selected| selected == *key))
            .map(|(id, _)| *id)
            .collect();
        for page in pages {
            let _ = self.send(json!({"op": "html-resize", "id": page, "width": width, "max_height": max_height, "scale": SCALE}));
        }
    }

    /// Start making a visual's pixels. Failures come back as messages for the app.
    /// Asked right after a draw, so `self.target` is the current one.
    pub fn need(&mut self, out: &mut impl Write, key: VisualKey, source: VisualSource) -> Vec<Msg> {
        let kind = match &source {
            VisualSource::Mermaid(_) => "mermaid",
            VisualSource::Asset(_) => "image",
            VisualSource::Html(_) => "html",
        };
        self.asked.insert(key.clone(), (std::time::Instant::now(), kind));
        let result = match &source {
            VisualSource::Mermaid(src) => self
                .ask(json!({"op": "mermaid", "source": src, "max_width": self.target.width_px as f64 / SCALE, "font_size": 14}))
                .map(|req| {
                    self.pending.insert(req, Pending::Mermaid(key.clone()));
                }),
            VisualSource::Asset(asset) => {
                // The file in the session folder, else the bytes over the socket.
                let local = self.stage.join("assets").join(asset.as_str());
                let path = if asset.bytes().all(|b| b.is_ascii_hexdigit()) && local.is_file() {
                    Ok(local)
                } else {
                    self.client
                        .asset_get(asset.clone())
                        .map_err(|e| e.message)
                        .and_then(|data| self.write_cache(&format!("asset-{asset}"), &data))
                };
                // A PNG goes to the terminal as a file it decodes itself; anything else
                // (jpeg, heic, gif…) is decoded by the helper.
                match path.map(|p| (kitty::png_size(&read_head(&p)), p)) {
                    Ok((Some(size), path)) => {
                        if let Some(msg) = self.show_png(out, key.clone(), &path, size) {
                            return vec![msg];
                        }
                        Ok(())
                    }
                    Ok((None, path)) => self.decode(key.clone(), path),
                    Err(e) => Err(e),
                }
            }
            VisualSource::Html(html) => {
                let name = format!("page-{}-{}-{}.html", key.0, key.1, key.2);
                let path = match self.write_cache(&name, html.as_bytes()) {
                    Ok(path) => path,
                    Err(reason) => return vec![Msg::VisualFailed { key, reason }],
                };
                if let Some(msgs) = self.update_page(&key, &path) {
                    return msgs;
                }
                let (width, max_height) = self.html_size();
                self.ask(json!({"op": "html-open", "path": path, "width": width, "max_height": max_height, "scale": SCALE, "fps": 60}))
                    .map(|req| {
                        self.pages.insert(req, key.clone());
                    })
            }
        };
        match result {
            Ok(()) => vec![],
            Err(reason) => vec![Msg::VisualFailed { key, reason }],
        }
    }

    /// A new revision of a part that already has a page updates that page (patched
    /// in place when it can be), keeping its pixels on screen. `None`: no such page.
    fn update_page(&mut self, key: &VisualKey, path: &std::path::Path) -> Option<Vec<Msg>> {
        let (page, old) = self.pages.iter().find(|(_, k)| k.0 == key.0 && k.2 == key.2).map(|(page, k)| (*page, k.clone()))?;
        self.send(json!({"op": "html-update", "id": page, "path": path})).ok()?;
        self.pages.insert(page, key.clone());
        self.asked.remove(key);
        let Some(img) = self.images.remove(&old) else { return Some(vec![]) };
        let (w, h) = (img.w, img.h);
        self.images.insert(key.clone(), img);
        Some(vec![Msg::VisualReady { key: key.clone(), w, h, max_scale: 1.0 }])
    }

    /// Ask the helper to decode a non-PNG image file.
    fn decode(&mut self, key: VisualKey, path: PathBuf) -> Result<(), String> {
        let req = self.ask(json!({"op": "image", "path": path}))?;
        self.pending.insert(req, Pending::Image(key, path));
        Ok(())
    }

    /// Re-render only the focused Mermaid card when its full-width target changes.
    /// Other cards retain their pixels until focus closes; then they render at
    /// the normal target again. This avoids competing with the card the user sees.
    pub fn retarget(&mut self, target: Target, focused: Option<&VisualKey>) {
        if target == self.target && focused == self.focused.as_ref() {
            return;
        }
        // Diagrams depend on the focus, card width and cell size, not the pane height.
        let rerender = focused != self.focused.as_ref()
            || (target.width_px, target.cell_px) != (self.target.width_px, self.target.cell_px);
        self.target = target;
        self.focused = focused.cloned();
        if rerender {
            self.raster_epoch += 1;
            self.raster_target();
        }
        self.resize_pages();
    }

    /// A request about an existing page (its id is the page id, not a new one).
    fn send(&mut self, req: Value) -> Result<(), String> {
        let helper = self.helper.as_mut().ok_or("stage-helper isn't running")?;
        if let Err(e) = helper.stream.write_all(format!("{req}\n").as_bytes()) {
            self.helper = None;
            return Err(format!("stage-helper stopped ({e})"));
        }
        Ok(())
    }

    fn raster_target(&self) {
        if let Some(key) = &self.focused {
            if let Some(svg) = self.svgs.get(key) {
                self.raster(key.clone(), svg.clone());
            }
        } else {
            for (key, svg) in &self.svgs {
                self.raster(key.clone(), svg.clone());
            }
        }
    }

    fn raster(&self, key: VisualKey, svg: String) {
        let (tx, width, cell, epoch) = (self.tx.clone(), self.target.width_px, self.target.cell_px, self.raster_epoch);
        thread::spawn(move || {
            let t0 = std::time::Instant::now();
            let raster = rasterize(&svg, width, MAX_MERMAID_HEIGHT_PX, cell);
            if std::env::var_os("STAGE_HELPER_TRACE").is_some() {
                eprintln!("resvg {:.1} ms", t0.elapsed().as_secs_f64() * 1000.0);
            }
            let done = raster.and_then(|(px, w, h)| {
                kitty::shm_write(&px).map(|shm| (shm, w, h)).map_err(|e| format!("shared memory: {e}"))
            });
            let _ = tx.send(Msg::Gfx(Gfx::Raster { key, epoch, done }));
        });
    }

    /// Trace a visual's first pixels: how long since it was asked for.
    fn traced(&mut self, key: &VisualKey, (w, h): (u32, u32)) {
        if let Some((at, kind)) = self.asked.remove(key) {
            note(&mut self.trace, format_args!("{kind} {:.1} ms {w}x{h}", at.elapsed().as_secs_f64() * 1000.0));
        }
    }

    /// A PNG file for `key`: the terminal decodes it; the next `place` positions it.
    fn show_png(&mut self, out: &mut impl Write, key: VisualKey, path: &std::path::Path, (w, h): (u32, u32)) -> Option<Msg> {
        let next_id = &mut self.next_id;
        let img = self.images.entry(key.clone()).or_insert_with(|| {
            let id = *next_id;
            *next_id += 1;
            Img { id, w, h, placed: None, previous: None, page: None, paused: false, file: None }
        });
        img.file = Some(path.to_owned());
        let _ = out.write_all(&kitty::transmit_png(img.id, path));
        note(&mut self.trace, format_args!("transmit png {} {}", img.id, path.display()));
        let _ = out.flush();
        img.placed = None;
        self.traced(&key, (w, h));
        Some(Msg::VisualReady { key, w, h, max_scale: 1.0 })
    }

    /// New pixels for `key`: transmit them, re-placing the image if it's on screen.
    fn show(&mut self, out: &mut impl Write, key: VisualKey, shm: &str, (w, h): (u32, u32), page: Option<u64>, file: Option<PathBuf>) -> Option<Msg> {
        let fresh = !self.images.contains_key(&key);
        self.traced(&key, (w, h));
        let next_id = &mut self.next_id;
        let img = self.images.entry(key.clone()).or_insert_with(|| {
            let id = *next_id;
            *next_id += 1;
            Img { id, w, h, placed: None, previous: None, page, paused: false, file: None }
        });
        if file.is_some() {
            img.file = file;
        }
        let resized = (img.w, img.h) != (w, h);
        if resized && (img.placed.is_some() || img.previous.is_some()) {
            // Reusing the id would replace the pixels in the old placement before
            // the layout switches widths, visibly stretching the HTML card.
            if img.previous.is_some() {
                let _ = out.write_all(&kitty::delete(img.id));
            } else {
                img.previous = Some(img.id);
            }
            img.id = *next_id;
            *next_id += 1;
        }
        (img.w, img.h) = (w, h);
        // Same size and on screen: replace the frame in place. Otherwise just
        // transmit; the next `place` positions it.
        // `placed` already holds the pixel rect (see `place`); don't crop it twice.
        let place = img.placed.filter(|_| !resized && img.previous.is_none());
        let _ = out.write_all(b"\x1b[?2026h");
        let _ = out.write_all(&kitty::transmit(img.id, shm, (w, h), place.as_ref()));
        let _ = out.write_all(b"\x1b[?2026l");
        let _ = out.flush();
        if place.is_none() && img.previous.is_none() {
            img.placed = None;
        }
        (fresh || resized).then_some(Msg::VisualReady { key, w, h, max_scale: 1.0 })
    }

    /// Handle a graphics event; returns messages for the app.
    pub fn handle(&mut self, out: &mut impl Write, gfx: Gfx) -> Vec<Msg> {
        match gfx {
            Gfx::HelperExited => {
                self.helper = None;
                let mut msgs: Vec<Msg> = self
                    .pending
                    .drain()
                    .map(|(_, p)| match p {
                        Pending::Mermaid(key) | Pending::Image(key, _) => Msg::VisualFailed { key, reason: "stage-helper stopped".into() },
                    })
                    .collect();
                msgs.extend(self.pages.drain().map(|(_, key)| Msg::VisualFailed { key, reason: "stage-helper stopped".into() }));
                msgs
            }
            Gfx::Raster { epoch, done, .. } if epoch != self.raster_epoch => {
                if let Ok((shm, ..)) = done { kitty::unlink(&shm); }
                vec![]
            }
            Gfx::Raster { key, done, .. } => match done {
                Ok((shm, w, h)) => self.show(out, key, &shm, (w, h), None, None).into_iter().collect(),
                Err(reason) => vec![Msg::VisualFailed { key, reason }],
            },
            Gfx::Helper(line) => {
                let Ok(v) = serde_json::from_str::<Value>(&line) else { return vec![] };
                let id = v["id"].as_u64().unwrap_or(0);
                let dims = || Some((v["w"].as_u64()? as u32, v["h"].as_u64()? as u32));
                if let Some(frame) = v["frame"].as_u64() {
                    let (Some(key), Some(shm), Some(size)) = (self.pages.get(&id).cloned(), v["shm"].as_str(), dims()) else {
                        return vec![];
                    };
                    // A frame captured before a focus/resize can arrive after the
                    // target changes. Never show it at an intermediate width.
                    if size.0 != self.target.width_px {
                        kitty::unlink(shm);
                        let _ = self.send(json!({"op": "html-ack", "id": id, "frame": frame}));
                        return vec![];
                    }
                    let msg = self.show(out, key, shm, size, Some(id), None);
                    let _ = self.send(json!({"op": "html-ack", "id": id, "frame": frame}));
                    return msg.into_iter().collect();
                }
                if v["ok"] == json!(false) {
                    let reason = v["error"].as_str().unwrap_or("stage-helper failed").to_owned();
                    let key = match self.pending.remove(&id) {
                        Some(Pending::Mermaid(k) | Pending::Image(k, _)) => Some(k),
                        None => self.pages.remove(&id),
                    };
                    return key.map(|key| Msg::VisualFailed { key, reason }).into_iter().collect();
                }
                match self.pending.remove(&id) {
                    Some(Pending::Mermaid(key)) => {
                        let svg = v["svg"].as_str().unwrap_or_default().to_owned();
                        self.svgs.insert(key.clone(), svg.clone());
                        if self.focused.as_ref().is_none_or(|focused| focused == &key) {
                            self.raster(key, svg);
                        }
                        vec![]
                    }
                    Some(Pending::Image(key, path)) => match (v["shm"].as_str(), dims()) {
                        (Some(shm), Some(size)) => self.show(out, key, shm, size, None, Some(path)).into_iter().collect(),
                        _ => vec![Msg::VisualFailed { key, reason: "stage-helper sent no pixels".into() }],
                    },
                    None => vec![],
                }
            }
        }
    }

    /// Match kitty placements to what's on screen this frame. Call between a
    /// frame's synchronized-output brackets, after ratatui drew it.
    pub fn place(&mut self, out: &mut impl Write, on_screen: &[OnScreen]) {
        let mut pause = Vec::new();
        let mut resume = Vec::new();
        for (key, img) in self.images.iter_mut() {
            let shown = on_screen.iter().find(|o| &o.key == key);
            let wanted = shown.map(|o| {
                crop(&Place { col: o.col, row: o.row, cols: o.cols, rows: o.rows, src: (0, o.top as u32, 0, o.total as u32) }, (img.w, img.h))
            });
            // Resized pixels wait (the old image stays) until the layout is sized for
            // them: the app holds old sizes while a sidebar change waits for every HTML card.
            if img.previous.is_some() && img.placed.is_some() && shown.is_some_and(|o| o.px != (img.w, img.h)) {
                continue;
            }
            if let Some(old) = img.previous.take() {
                let _ = out.write_all(&kitty::delete(old));
                img.placed = None;
            }
            match (wanted, img.placed) {
                (Some(p), Some(q)) if p == q => {}
                (Some(p), _) => {
                    let _ = out.write_all(&kitty::place(img.id, &p));
                    note(&mut self.trace, format_args!("place {} at {},{} {}x{} cells src {:?}", img.id, p.col, p.row, p.cols, p.rows, p.src));
                    img.placed = Some(p);
                }
                (None, Some(_)) => {
                    note(&mut self.trace, format_args!("unplace {}", img.id));
                    let _ = out.write_all(&kitty::unplace(img.id));
                    img.placed = None;
                }
                (None, None) => {}
            }
            if let Some(page) = img.page {
                match (img.placed.is_some(), img.paused) {
                    (false, false) => {
                        img.paused = true;
                        pause.push(page);
                    }
                    (true, true) if !self.held => {
                        img.paused = false;
                        resume.push(page);
                    }
                    _ => {}
                }
            }
        }
        for page in pause {
            let _ = self.send(json!({"op": "html-pause", "id": page}));
        }
        for page in resume {
            let _ = self.send(json!({"op": "html-resume", "id": page}));
        }
    }

    /// The screen was cleared (a resize). Placements are gone, and Rex drops the
    /// images too, so send every one again (at `target`); the next `place` shows them.
    pub fn replace_all(&mut self, out: &mut impl Write, target: Target, focused: Option<&VisualKey>) {
        self.target = target;
        self.focused = focused.cloned();
        self.raster_epoch += 1;
        note(&mut self.trace, format_args!("clear: sending {} images again", self.images.len()));
        let mut decode = Vec::new();
        for (key, img) in self.images.iter_mut() {
            img.placed = None;
            img.previous = None;
            match img.file.as_ref().map(|f| (kitty::png_size(&read_head(f)).is_some(), f)) {
                Some((true, file)) => {
                    let _ = out.write_all(&kitty::transmit_png(img.id, file));
                }
                Some((false, file)) => decode.push((key.clone(), file.clone())),
                None => {}
            }
        }
        for (key, file) in decode {
            let _ = self.decode(key, file);
        }
        self.raster_target();
        // Only the focused page needs the new size while it fills the pane.
        self.resize_pages();
    }

    /// Remove every image (on quit).
    pub fn clear(&mut self, out: &mut impl Write) {
        let _ = out.write_all(kitty::FREE_ALL);
        self.images.clear();
    }
}

impl Drop for Gallery {
    /// Hang up on the helper (its reader thread holds a clone of the socket, so
    /// dropping ours alone wouldn't); it closes this stage's pages.
    fn drop(&mut self) {
        if let Some(h) = &self.helper {
            let _ = h.stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// Turn a placement's row crop into source pixels. `src` arrives as
/// (_, rows cut off above, _, total rows); it leaves as an exact pixel rect.
fn crop(p: &Place, (w, h): (u32, u32)) -> Place {
    let (top, total) = (p.src.1, p.src.3.max(1));
    let y = top * h / total;
    let rows_h = (p.rows as u32 * h).div_ceil(total).min(h - y.min(h));
    Place { src: (0, y, w, rows_h.max(1)), ..*p }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_maps_cut_rows_to_pixels() {
        // 10 rows of a 300 px tall image; rows 3..7 visible.
        let p = Place { col: 1, row: 2, cols: 20, rows: 4, src: (0, 3, 0, 10) };
        assert_eq!(crop(&p, (400, 300)).src, (0, 90, 400, 120));
        // Fully visible.
        let p = Place { src: (0, 0, 0, 10), rows: 10, ..p };
        assert_eq!(crop(&p, (400, 300)).src, (0, 0, 400, 300));
    }

    #[test]
    fn html_frame_from_before_a_resize_is_never_transmitted() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut gallery = Gallery::new(tx, StageClient::new("/nonexistent".into()), std::env::temp_dir(), std::env::temp_dir(), Target { width_px: 800, max_height_px: 400, cell_px: (1, 1) });
        let key: VisualKey = ("page".into(), 1, 0);
        gallery.pages.insert(9, key.clone());
        let mut out = Vec::new();
        let old = Gfx::Helper(r#"{"id":9,"frame":3,"shm":"/stg-old","w":600,"h":200}"#.into());
        assert!(gallery.handle(&mut out, old).is_empty());
        assert!(out.is_empty());
        assert!(!gallery.images.contains_key(&key));
        let resized = Gfx::Helper(r#"{"id":9,"frame":4,"shm":"/stg-new","w":800,"h":200}"#.into());
        assert!(matches!(gallery.handle(&mut out, resized).as_slice(), [Msg::VisualReady { w: 800, .. }]));
    }

    #[test]
    fn live_frames_reuse_the_placed_pixel_rect() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut g = Gallery::new(tx, StageClient::new("/nonexistent".into()), std::env::temp_dir(), std::env::temp_dir(), Target { width_px: 660, max_height_px: 360, cell_px: (10, 20) });
        let key: VisualKey = ("p".into(), 1, 0);
        g.pages.insert(9, key.clone());
        let frame = |n: u64| Gfx::Helper(format!(r#"{{"id":9,"frame":{n},"shm":"/stg-test-{n}","w":660,"h":360}}"#));
        let mut out = Vec::new();
        g.handle(&mut out, frame(1));
        g.place(&mut Vec::new(), &[OnScreen { key: key.clone(), col: 3, row: 4, cols: 40, top: 0, rows: 10, total: 10, px: (660, 360) }]);
        out.clear();
        g.handle(&mut out, frame(2));
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains("a=T") && s.contains("x=0,y=0,w=660,h=360,c=40,r=10"), "{s}");
    }

    #[test]
    fn resized_html_keeps_old_pixels_until_the_card_moves_to_its_new_width() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut gallery = Gallery::new(tx, StageClient::new("/nonexistent".into()), std::env::temp_dir(), std::env::temp_dir(), Target { width_px: 600, max_height_px: 400, cell_px: (1, 1) });
        let key: VisualKey = ("page".into(), 1, 0);
        gallery.pages.insert(9, key.clone());
        let frame = |number, width| Gfx::Helper(format!(r#"{{"id":9,"frame":{number},"shm":"/stg-test-{number}","w":{width},"h":200}}"#));
        gallery.handle(&mut Vec::new(), frame(1, 600));
        let old = OnScreen { key: key.clone(), col: 25, row: 4, cols: 70, top: 0, rows: 10, total: 10, px: (600, 200) };
        gallery.place(&mut Vec::new(), std::slice::from_ref(&old));
        let old_id = gallery.images[&key].id;

        gallery.target.width_px = 800;
        let mut transmitted = Vec::new();
        gallery.handle(&mut transmitted, frame(2, 800));
        let new_id = gallery.images[&key].id;
        assert_ne!(new_id, old_id);
        assert!(!String::from_utf8_lossy(&transmitted).contains(&format!("a=d,d=i,i={old_id}")));
        let mut still_old = Vec::new();
        gallery.place(&mut still_old, std::slice::from_ref(&old));
        assert!(still_old.is_empty(), "the old sidebar layout keeps the old pixels");

        let mut switched = Vec::new();
        gallery.place(&mut switched, &[OnScreen { col: 0, cols: 95, px: (800, 200), ..old }]);
        let output = String::from_utf8_lossy(&switched);
        assert!(output.contains(&format!("a=d,d=i,i={old_id}")));
        assert!(output.contains(&format!("a=p,i={new_id}")));
    }

    #[test]
    fn leaving_focus_places_the_narrow_raster_once_the_layout_uses_it() {
        // z→z with the sidebar shown: the full-width raster is still on screen in the
        // normal column (same cell rect) when the normal-width one lands.
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut gallery = Gallery::new(tx, StageClient::new("/nonexistent".into()), std::env::temp_dir(), std::env::temp_dir(), Target { width_px: 950, max_height_px: 400, cell_px: (10, 20) });
        let key: VisualKey = ("diagram".into(), 1, 0);
        let shms: Vec<String> = [(950, 400), (690, 300)].iter().map(|(w, h)| kitty::shm_write(&vec![0; w * h * 4]).unwrap()).collect();
        let raster = |shm: &str, w, h| Gfx::Raster { key: key.clone(), epoch: 0, done: Ok((shm.to_owned(), w, h)) };
        gallery.handle(&mut Vec::new(), raster(&shms[0], 950, 400));
        let at = OnScreen { key: key.clone(), col: 29, row: 7, cols: 69, top: 0, rows: 20, total: 20, px: (950, 400) };
        gallery.place(&mut Vec::new(), std::slice::from_ref(&at));
        let wide_id = gallery.images[&key].id;
        gallery.handle(&mut Vec::new(), raster(&shms[1], 690, 300));
        let narrow_id = gallery.images[&key].id;
        let mut out = Vec::new();
        gallery.place(&mut out, std::slice::from_ref(&at));
        assert!(out.is_empty(), "the layout still sizes the card for the wide pixels");
        gallery.place(&mut out, &[OnScreen { rows: 15, total: 15, px: (690, 300), ..at }]);
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains(&format!("a=d,d=i,i={wide_id}")) && s.contains(&format!("a=p,i={narrow_id},p=1,x=0,y=0,w=690,h=300,c=69,r=15")), "{s}");
        shms.iter().for_each(|shm| kitty::unlink(shm)); // no terminal here to consume them
    }

    #[test]
    fn focusing_rasterizes_only_the_selected_mermaid_and_ignores_old_pixels() {
        let (tx, rx) = std::sync::mpsc::channel();
        let normal = Target { width_px: 100, max_height_px: 100, cell_px: (1, 1) };
        let focused_target = Target { width_px: 200, max_height_px: 200, cell_px: (1, 1) };
        let mut gallery = Gallery::new(tx, StageClient::new("/nonexistent".into()), std::env::temp_dir(), std::env::temp_dir(), normal);
        let first: VisualKey = ("first".into(), 1, 0);
        let second: VisualKey = ("second".into(), 1, 0);
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"><rect width="100" height="50" fill="#89b4fa"/></svg>"##;
        gallery.svgs.insert(first.clone(), svg.into());
        gallery.svgs.insert(second.clone(), svg.into());
        gallery.retarget(focused_target, Some(&second));
        let Msg::Gfx(Gfx::Raster { key, epoch, done }) = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap() else { panic!("no raster") };
        assert_eq!(key, second);
        assert!(rx.try_recv().is_err(), "offscreen Mermaid cards must not render during focus");
        let (shm, ..) = done.unwrap();
        assert!(gallery.handle(&mut Vec::new(), Gfx::Raster { key, epoch: epoch - 1, done: Ok((shm.clone(), 200, 100)) }).is_empty());
        assert!(!gallery.images.contains_key(&second));
        // The stale completion owns its shm and must unlink it.
        assert!(kitty::shm_read(&shm, 1).is_err());
        gallery.retarget(normal, None);
        let mut keys = [rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(), rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap()]
            .into_iter().map(|msg| match msg { Msg::Gfx(Gfx::Raster { key, done: Ok((shm, ..)), .. }) => { kitty::unlink(&shm); key }, _ => panic!("no raster") }).collect::<Vec<_>>();
        keys.sort();
        assert_eq!(keys, [first, second]);
    }

    #[test]
    fn rasterizes_svg_at_the_card_width() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"><rect width="100" height="50" fill="#89b4fa"/></svg>"##;
        let (px, w, h) = rasterize(svg, 150, 1000, (1, 1)).unwrap();
        assert_eq!((w, h), (150, 75));
        assert_eq!(&px[..4], &[0x89, 0xb4, 0xfa, 0xff]);
        // Never past 2× the SVG's own size or the explicit height budget.
        assert_eq!(rasterize(svg, 1000, 1000, (1, 1)).unwrap().1, 200);
        assert_eq!(rasterize(svg, 1000, 40, (1, 1)).unwrap().2, 40);
    }

    #[test]
    fn resize_cannot_replace_crisp_pixels_with_an_older_raster() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut gallery = Gallery::new(tx, StageClient::new("/nonexistent".into()), std::env::temp_dir(), std::env::temp_dir(), Target { width_px: 100, max_height_px: 100, cell_px: (1, 1) });
        let key: VisualKey = ("diagram".into(), 1, 0);
        gallery.svgs.insert(key.clone(), r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"/>"#.into());
        gallery.retarget(Target { width_px: 120, max_height_px: 100, cell_px: (1, 1) }, None);
        gallery.retarget(Target { width_px: 180, max_height_px: 100, cell_px: (1, 1) }, None);
        let jobs = [rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(), rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap()];
        let mut old = None;
        let mut latest = None;
        for msg in jobs {
            let Msg::Gfx(Gfx::Raster { key: job_key, epoch, done }) = msg else { panic!("expected raster") };
            assert_eq!(job_key, key);
            if epoch == gallery.raster_epoch { latest = Some(done.unwrap()); } else { old = Some(done.unwrap()); }
        }
        let (shm, w, h) = latest.unwrap();
        assert_eq!((w, h), (180, 90));
        assert_eq!(gallery.handle(&mut Vec::new(), Gfx::Raster { key: key.clone(), epoch: gallery.raster_epoch, done: Ok((shm.clone(), w, h)) }).len(), 1);
        kitty::unlink(&shm); // the test has no terminal to consume the transmitted pixels
        let (stale_shm, stale_w, stale_h) = old.unwrap();
        assert!(gallery.handle(&mut Vec::new(), Gfx::Raster { key: key.clone(), epoch: gallery.raster_epoch - 1, done: Ok((stale_shm.clone(), stale_w, stale_h)) }).is_empty());
        assert!(kitty::shm_read(&stale_shm, 1).is_err());
        assert_eq!((gallery.images[&key].w, gallery.images[&key].h), (180, 90));
    }

    #[test]
    fn tall_mermaid_retains_card_width_instead_of_shrinking_to_viewport() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="650" height="816"><rect width="650" height="816"/></svg>"#;
        let (_, w, h) = rasterize(svg, 700, MAX_MERMAID_HEIGHT_PX, (1, 1)).unwrap();
        assert_eq!((w, h), (700, 879));
    }

    #[test]
    fn mermaid_fills_whole_cells_so_kitty_never_resamples() {
        // 420×415.5 CSS px at 2× is 840×831; with 10×20 px cells that rounded up
        // to 84×42 cells (840×840 px) and kitty stretched it ~1% vertically.
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="420" height="415.5"><rect width="420" height="415.5" fill="red"/></svg>"#;
        let cell = (10, 20);
        let (px, w, h) = rasterize(svg, 840, MAX_MERMAID_HEIGHT_PX, cell).unwrap();
        assert_eq!((w, h), (840, 840));
        // The padding is transparent: the last row was not drawn on.
        assert!(px[((h - 1) * w * 4) as usize..].chunks(4).all(|p| p[3] == 0));
        let crate::cards::Fit::Ready { cols, rows } = crate::app::fit_cells((w, h), 1.0, 84, u16::MAX, cell) else { panic!("fits") };
        assert_eq!((cols as u32 * 10, rows as u32 * 20), (w, h), "one image px per screen px");
        // A 25-row crop scrolled 3 rows in: exactly 25 cells of source pixels.
        let p = crop(&Place { col: 0, row: 0, cols, rows: 25, src: (0, 3, 0, rows as u32) }, (w, h));
        assert_eq!(p.src, (0, 60, 840, 500));
    }
}
