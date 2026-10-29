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
    sync::{mpsc::Sender, Arc, Mutex, OnceLock},
    thread,
};

use notes_proto::helper::{Failed, Frame, HelperReply, HelperRequest, Image, Mermaid};

use crate::{
    app::{Msg, OnScreen, Target, VisualKey, ZoomView},
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
    /// A mermaid diagram rasterized into shared memory for `made_for` (or why it couldn't be).
    Raster { key: VisualKey, made_for: RasterTarget, done: Result<(String, u32, u32), String> },
}

/// What a Mermaid raster depends on: the width and height it must fit, the cell
/// size (all px), and, when zoomed (focused), the window of the diagram shown.
pub type RasterTarget = (u32, u32, (u16, u16), Option<ZoomView>);

/// CSS px → device px for helper renders (Retina).
pub const SCALE: f64 = 2.0;
/// Limit very large diagrams without shrinking ordinary tall ones to the viewport.
const MAX_MERMAID_HEIGHT_PX: u32 = 4096;
/// Diagram pixels the terminal holds for stage. Past this, the diagrams shown least
/// recently (and not on screen) are dropped; they rasterize again from their SVG if seen.
const DIAGRAM_BUDGET_BYTES: u64 = 128 << 20;

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
    /// The `place` call that last showed it, to drop the least recent first.
    shown_at: u64,
}

/// The focused diagram's feed-size pixels while its zoomed raster is shown, so
/// closing focus puts them back in the same frame instead of shrinking the zoomed one.
struct Unzoomed {
    key: VisualKey,
    /// What the feed-size pixels were rasterized for.
    made_for: Option<RasterTarget>,
    img: Option<Img>,
}

/// A zoomed raster made ahead of `z` for the diagram under the cursor: its pixels wait
/// in shared memory until focus opens on it (then they're shown in that first frame).
struct Prefetch {
    key: VisualKey,
    made_for: RasterTarget,
    done: Option<(String, u32, u32)>,
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
    /// The focused diagram's zoom and pan.
    view: ZoomView,
    /// The focused diagram, parsed once, so zooming and panning only draw.
    parsed: Arc<Mutex<Option<(VisualKey, Parsed)>>>,
    /// The target each diagram's latest raster job was started for. Diagrams are
    /// rasterized for the current target only when on screen (or focused).
    rastered: HashMap<VisualKey, RasterTarget>,
    zoom: Option<Unzoomed>,
    prefetch: Option<Prefetch>,
    /// Prefetched zoomed pixels to show at the next `place`, the first frame of focus.
    ready_zoom: Option<(VisualKey, RasterTarget, (String, u32, u32))>,
    /// Images to delete at the next `place` (which has the terminal).
    dead: Vec<u32>,
    /// Counts `place` calls, for `Img::shown_at`.
    frame: u64,
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

/// SVG → RGBA at up to `max_scale`× its CSS size, within the supplied pixel budget,
/// on a transparent background so glass shows through. The canvas is padded
/// (diagram centered) to whole `cell` px, so kitty shows it 1:1 instead of
/// stretching it to the cells it rounds up to.
pub fn rasterize(svg: &str, width_px: u32, height_px: u32, cell: (u16, u16), max_scale: f32) -> Result<(Vec<u8>, u32, u32), String> {
    let (tree, at, pw, ph) = fit_svg(svg, width_px, height_px, cell, max_scale)?;
    let mut px = vec![0; (pw * ph * 4) as usize];
    draw(&tree, at, &mut px, (pw, ph))?;
    Ok((px, pw, ph))
}

/// A diagram parsed by usvg (which also lays out its text: the costly part).
type Parsed = Arc<resvg::usvg::Tree>;

fn parse(svg: &str) -> Result<Parsed, String> {
    let opt = resvg::usvg::Options { fontdb: fonts(), ..Default::default() };
    resvg::usvg::Tree::from_str(svg, &opt).map(Arc::new).map_err(|e| format!("svg: {e}"))
}

/// A fitted diagram drawn straight into a new shared-memory object for the terminal,
/// so no diagram-sized buffer is ever allocated (or kept by malloc) in stage itself.
fn draw_shm((tree, at, pw, ph): (Parsed, resvg::tiny_skia::Transform, u32, u32)) -> Result<(String, u32, u32), String> {
    let mut drawn = Ok(());
    let shm = kitty::shm_fill((pw * ph * 4) as usize, |px| drawn = draw(&tree, at, px, (pw, ph))).map_err(|e| format!("shared memory: {e}"))?;
    drawn.inspect_err(|_| kitty::unlink(&shm))?;
    Ok((shm, pw, ph))
}

/// The parsed diagram, where it's drawn, and the canvas: whole cells, diagram centered.
fn fit_svg(svg: &str, width_px: u32, height_px: u32, (cw, ch): (u16, u16), max_scale: f32) -> Result<(Parsed, resvg::tiny_skia::Transform, u32, u32), String> {
    let tree = parse(svg)?;
    let size = tree.size();
    let scale = (width_px as f32 / size.width()).min(height_px as f32 / size.height()).min(max_scale);
    let (w, h) = (
        ((size.width() * scale).ceil() as u32).clamp(1, width_px.max(1)),
        ((size.height() * scale).ceil() as u32).clamp(1, height_px.max(1)),
    );
    let (cw, ch) = (cw.max(1) as u32, ch.max(1) as u32);
    let (pw, ph) = (w.div_ceil(cw) * cw, h.div_ceil(ch) * ch);
    let at = resvg::tiny_skia::Transform::from_scale(scale, scale).post_translate(((pw - w) / 2) as f32, ((ph - h) / 2) as f32);
    Ok((tree, at, pw, ph))
}

/// A zoomed diagram: `view.level` 1.5x steps past fitting `width_px`x`height_px` (at most
/// 8 px per CSS px unless fitting needs more), and of that only the window of the pane's
/// size around (`cx`, `cy`), kept inside the diagram. Only the window is drawn, so any
/// zoom costs about one pane of pixels.
fn fit_view(tree: Parsed, width_px: u32, height_px: u32, (cw, ch): (u16, u16), view: ZoomView) -> Result<(Parsed, resvg::tiny_skia::Transform, u32, u32), String> {
    let size = tree.size();
    let fit = (width_px as f32 / size.width()).min(height_px as f32 / size.height());
    let scale = (fit * 1.5f32.powi(view.level as i32)).min(fit.max(8.0));
    let (dw, dh) = (size.width() * scale, size.height() * scale);
    let (w, h) = ((dw.ceil() as u32).clamp(1, width_px.max(1)), (dh.ceil() as u32).clamp(1, height_px.max(1)));
    let (cw, ch) = (cw.max(1) as u32, ch.max(1) as u32);
    let (pw, ph) = (w.div_ceil(cw) * cw, h.div_ceil(ch) * ch);
    let origin = |center: u16, whole: f32, shown: u32| (center as f32 / 1000.0 * whole - shown as f32 / 2.0).clamp(0.0, (whole - shown as f32).max(0.0));
    let (ox, oy) = (origin(view.cx, dw, w), origin(view.cy, dh, h));
    let at = resvg::tiny_skia::Transform::from_scale(scale, scale)
        .post_translate(((pw - w) / 2) as f32 - ox, ((ph - h) / 2) as f32 - oy);
    Ok((tree, at, pw, ph))
}

/// Draw onto `px` (zeroed: transparent, so glass shows through) as straight RGBA.
fn draw(tree: &resvg::usvg::Tree, at: resvg::tiny_skia::Transform, px: &mut [u8], (w, h): (u32, u32)) -> Result<(), String> {
    resvg::render(tree, at, &mut resvg::tiny_skia::PixmapMut::from_bytes(px, w, h).ok_or("empty diagram")?);
    // tiny-skia is premultiplied; kitty wants straight RGBA.
    for p in px.as_chunks_mut::<4>().0 {
        let a = p[3] as u32;
        if a != 0 && a != 255 {
            for c in &mut p[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    Ok(())
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
            view: ZoomView::default(),
            parsed: Arc::default(),
            rastered: HashMap::new(),
            zoom: None,
            prefetch: None,
            ready_zoom: None,
            dead: Vec::new(),
            frame: 0,
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
                let _ = self.send(&HelperRequest::HtmlPause { id: page });
            }
        }
    }

    /// Start the helper and load fonts now (after stage's first frame), so the
    /// first visual doesn't pay for process launch, WebKit and font loading.
    pub fn warm(&mut self) {
        thread::spawn(fonts);
        let _ = self.ask(|id| HelperRequest::Warm { id });
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
    fn ask(&mut self, req: impl FnOnce(u64) -> HelperRequest) -> Result<u64, String> {
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
        self.send(&req(id)).map(|()| id)
    }

    fn write_cache(&self, name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
        let path = self.cache.join(name);
        std::fs::create_dir_all(&self.cache).and_then(|()| std::fs::write(&path, bytes)).map_err(|e| e.to_string())?;
        Ok(path)
    }

    fn html_size(&self) -> (f64, f64) {
        (self.target.width_px as f64 / SCALE, self.target.html_height_px as f64 / SCALE)
    }

    fn resize_pages(&mut self) {
        let (width, max_height) = self.html_size();
        let pages: Vec<_> = self.pages.iter()
            .filter(|(_, key)| self.focused.as_ref().is_none_or(|selected| selected == *key))
            .map(|(id, _)| *id)
            .collect();
        for page in pages {
            let _ = self.send(&HelperRequest::HtmlResize { id: page, width, max_height, scale: SCALE });
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
            VisualSource::Mermaid(src) => {
                let max_width = self.target.width_px as f64 / SCALE;
                self.ask(|id| HelperRequest::Mermaid { id, source: src.clone(), max_width, font_size: 14.0 })
                    .map(|req| {
                        self.pending.insert(req, Pending::Mermaid(key.clone()));
                    })
            }
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
                self.ask(|id| HelperRequest::HtmlOpen { id, path, width, max_height, scale: SCALE, fps: 60.0 })
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
        self.send(&HelperRequest::HtmlUpdate { id: page, path: path.to_owned() }).ok()?;
        self.pages.insert(page, key.clone());
        self.asked.remove(key);
        let Some(img) = self.images.remove(&old) else { return Some(vec![]) };
        let (w, h) = (img.w, img.h);
        self.images.insert(key.clone(), img);
        Some(vec![Msg::VisualReady { key: key.clone(), w, h, max_scale: 1.0 }])
    }

    /// Ask the helper to decode a non-PNG image file.
    fn decode(&mut self, key: VisualKey, path: PathBuf) -> Result<(), String> {
        let req = self.ask(|id| HelperRequest::Image { id, path: path.clone() })?;
        self.pending.insert(req, Pending::Image(key, path));
        Ok(())
    }

    /// The focused diagram's zoom and pan, from the app; `place` rasterizes it again on change.
    pub fn view(&mut self, view: ZoomView) {
        self.view = view;
    }

    /// A new target or focus: resize live pages. Diagrams follow in `place`, which
    /// re-rasterizes only the ones on screen (just the focused one while focused).
    pub fn retarget(&mut self, target: Target, focused: Option<&VisualKey>) {
        if target == self.target && focused == self.focused.as_ref() {
            return;
        }
        self.target = target;
        if focused != self.focused.as_ref() {
            // Keep the zoom made ahead's parse for the card being focused; drop any other.
            let mut parsed = self.parsed.lock().unwrap_or_else(|e| e.into_inner());
            if parsed.as_ref().is_some_and(|(key, _)| Some(key) != focused) {
                *parsed = None;
            }
        }
        self.focused = focused.cloned();
        if let Some(ahead) = self.prefetch.take_if(|ahead| focused == Some(&ahead.key)) {
            let (width, height, cell, _) = self.raster_target(&ahead.key);
            if (ahead.made_for.0, ahead.made_for.1, ahead.made_for.2) == (width, height, cell) {
                // Made for this pane (fitted): shown at once, even if the view already moved
                // on, since the app laid the card out at its size; `place` then asks for the view.
                self.zoom = Some(Unzoomed { key: ahead.key.clone(), made_for: self.rastered.get(&ahead.key).copied(), img: None });
                self.rastered.insert(ahead.key.clone(), ahead.made_for);
                self.ready_zoom = ahead.done.map(|done| (ahead.key, ahead.made_for, done));
            } else if let Some((shm, ..)) = ahead.done {
                kitty::unlink(&shm);
            }
        }
        if let Some(zoom) = self.zoom.take_if(|zoom| focused != Some(&zoom.key)) {
            if let Some(zoomed) = zoom.img.and_then(|img| self.images.insert(zoom.key.clone(), img)) {
                self.dead.extend([Some(zoomed.id), zoomed.previous].into_iter().flatten());
            }
            match zoom.made_for {
                Some(made_for) => self.rastered.insert(zoom.key, made_for),
                None => self.rastered.remove(&zoom.key),
            };
        }
        self.resize_pages();
    }

    fn raster_target(&self, key: &VisualKey) -> RasterTarget {
        match self.focused.as_ref() == Some(key) {
            // The whole diagram in the pane: no scrolling a zoomed one.
            true => (self.target.width_px, self.target.max_height_px, self.target.cell_px, Some(self.view)),
            false => (self.target.width_px, MAX_MERMAID_HEIGHT_PX, self.target.cell_px, None),
        }
    }

    /// A request about an existing page (its id is the page id, not a new one).
    fn send(&mut self, req: &HelperRequest) -> Result<(), String> {
        let helper = self.helper.as_mut().ok_or("stage-helper isn't running")?;
        let line = serde_json::to_string(req).map_err(|e| e.to_string())?;
        if let Err(e) = helper.stream.write_all(format!("{line}\n").as_bytes()) {
            self.helper = None;
            return Err(format!("stage-helper stopped ({e})"));
        }
        Ok(())
    }

    /// Rasterize `key` for the current target, unless a job for it already exists.
    fn raster(&mut self, key: &VisualKey) {
        let made_for = self.raster_target(key);
        if self.rastered.get(key) == Some(&made_for) {
            return;
        }
        let Some(svg) = self.svgs.get(key).cloned() else { return };
        if made_for.3.is_some() && self.zoom.as_ref().is_none_or(|zoom| &zoom.key != key) {
            self.zoom = Some(Unzoomed { key: key.clone(), made_for: self.rastered.get(key).copied(), img: None });
        }
        self.rastered.insert(key.clone(), made_for);
        self.spawn_raster(key.clone(), svg, made_for);
    }

    /// Set the card's feed image aside while it's zoomed. Pixels it still holds back for
    /// a resize are deleted now: hidden under the zoom, they'd stay on screen.
    fn park(&mut self, key: &VisualKey) {
        if !self.zoom.as_ref().is_some_and(|zoom| &zoom.key == key && zoom.img.is_none()) {
            return;
        }
        let Some(mut img) = self.images.remove(key) else { return };
        if let Some(previous) = img.previous.take() {
            self.dead.push(previous);
            img.placed = None;
        }
        self.zoom.as_mut().expect("checked").img = Some(img);
    }

    /// Make the zoomed raster for `key` at `target` ahead of `z` (one at a time). Asked
    /// again for one already made, it says so again: the app may have forgotten its size.
    pub fn prefetch_zoom(&mut self, key: VisualKey, target: Target) -> Option<Msg> {
        let made_for = (target.width_px, target.max_height_px, target.cell_px, Some(ZoomView::default()));
        if let Some(ahead) = self.prefetch.as_ref().filter(|ahead| ahead.key == key && ahead.made_for == made_for) {
            return ahead.done.as_ref().map(|(_, w, h)| Msg::ZoomReady { key, w: *w, h: *h });
        }
        if self.focused.is_some() {
            return None;
        }
        let svg = self.svgs.get(&key).cloned()?;
        if let Some((shm, ..)) = self.prefetch.take().and_then(|ahead| ahead.done) {
            kitty::unlink(&shm);
        }
        self.prefetch = Some(Prefetch { key: key.clone(), made_for, done: None });
        self.spawn_raster(key, svg, made_for);
        None
    }

    fn spawn_raster(&self, key: VisualKey, svg: String, made_for: RasterTarget) {
        let (tx, parsed, (width, height, cell, zoom)) = (self.tx.clone(), self.parsed.clone(), made_for);
        thread::spawn(move || {
            let t0 = std::time::Instant::now();
            let done = match zoom {
                Some(view) => {
                    let mut cached = parsed.lock().unwrap_or_else(|e| e.into_inner());
                    let tree = match cached.as_ref().filter(|(k, _)| *k == key) {
                        Some((_, tree)) => Ok(tree.clone()),
                        None => parse(&svg).inspect(|tree| *cached = Some((key.clone(), tree.clone()))),
                    };
                    drop(cached);
                    tree.and_then(|tree| fit_view(tree, width, height, cell, view)).and_then(draw_shm)
                }
                None => fit_svg(&svg, width, height, cell, SCALE as f32).and_then(draw_shm),
            };
            if std::env::var_os("STAGE_HELPER_TRACE").is_some() {
                eprintln!("resvg {:.1} ms", t0.elapsed().as_secs_f64() * 1000.0);
            }
            let _ = tx.send(Msg::Gfx(Gfx::Raster { key, made_for, done }));
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
            Img { id, w, h, placed: None, previous: None, page: None, paused: false, file: None, shown_at: 0 }
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
            Img { id, w, h, placed: None, previous: None, page, paused: false, file: None, shown_at: 0 }
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
            Gfx::Raster { key, made_for, done }
                if self.focused.as_ref() != Some(&key)
                    && self.prefetch.as_ref().is_some_and(|ahead| ahead.key == key && ahead.made_for == made_for && ahead.done.is_none()) =>
            {
                match done {
                    Ok((shm, w, h)) => {
                        self.prefetch.as_mut().expect("checked").done = Some((shm, w, h));
                        vec![Msg::ZoomReady { key, w, h }]
                    }
                    Err(_) => {
                        self.prefetch = None;
                        vec![]
                    }
                }
            }
            Gfx::Raster { key, made_for, done } if made_for != self.raster_target(&key) => {
                // Made for an earlier width: drop it, and let `place` ask again if it's on screen.
                if let Ok((shm, ..)) = done { kitty::unlink(&shm); }
                if self.rastered.get(&key) == Some(&made_for) {
                    self.rastered.remove(&key);
                }
                // Nor do the feed pixels set aside while zoomed come from it.
                if let Some(zoom) = self.zoom.as_mut().filter(|zoom| zoom.key == key && zoom.made_for == Some(made_for)) {
                    zoom.made_for = None;
                }
                vec![]
            }
            Gfx::Raster { key, made_for, done } => match done {
                Ok((shm, w, h)) => {
                    // A zoomed raster gets its own image; the feed-size one waits for focus to close.
                    if made_for.3.is_some() {
                        self.park(&key);
                    }
                    let shown = self.show(out, key, &shm, (w, h), None, None);
                    // Zoomed pixels only count while the app still focuses this card: a `z`
                    // back in the same loop turn must not leave the feed laid out zoomed.
                    match shown {
                        Some(Msg::VisualReady { key, w, h, .. }) if made_for.3.is_some() => vec![Msg::ZoomShown { key, w, h }],
                        other => other.into_iter().collect(),
                    }
                }
                Err(reason) => vec![Msg::VisualFailed { key, reason }],
            },
            Gfx::Helper(line) => {
                let Ok(reply) = serde_json::from_str::<HelperReply>(&line) else { return vec![] };
                match reply {
                    HelperReply::Frame(Frame { id, frame, shm, w, h }) => {
                        let Some(key) = self.pages.get(&id).cloned() else { return vec![] };
                        // A frame captured before a focus/resize can arrive after the
                        // target changes. Never show it at an intermediate width.
                        if w != self.target.width_px {
                            kitty::unlink(&shm);
                            let _ = self.send(&HelperRequest::HtmlAck { id, frame });
                            return vec![];
                        }
                        let msg = self.show(out, key, &shm, (w, h), Some(id), None);
                        let _ = self.send(&HelperRequest::HtmlAck { id, frame });
                        msg.into_iter().collect()
                    }
                    HelperReply::Failed(Failed { id, error: reason, .. }) => {
                        let id = id.unwrap_or(0);
                        let key = match self.pending.remove(&id) {
                            Some(Pending::Mermaid(k) | Pending::Image(k, _)) => Some(k),
                            None => self.pages.remove(&id),
                        };
                        key.map(|key| Msg::VisualFailed { key, reason }).into_iter().collect()
                    }
                    HelperReply::Mermaid(Mermaid { id, svg, .. }) => {
                        let Some(Pending::Mermaid(key)) = self.pending.remove(&id) else { return vec![] };
                        self.svgs.insert(key.clone(), svg);
                        if self.focused.as_ref().is_none_or(|focused| focused == &key) {
                            self.raster(&key);
                        }
                        vec![]
                    }
                    HelperReply::Image(Image { id, shm, w, h, .. }) => match self.pending.remove(&id) {
                        Some(Pending::Image(key, path)) => self.show(out, key, &shm, (w, h), None, Some(path)).into_iter().collect(),
                        _ => vec![],
                    },
                    HelperReply::Patched(_) | HelperReply::Done(_) => vec![],
                }
            }
        }
    }

    /// Match kitty placements to what's on screen this frame. Call between a
    /// frame's synchronized-output brackets, after ratatui drew it.
    pub fn place(&mut self, out: &mut impl Write, on_screen: &[OnScreen]) {
        if let Some((key, made_for, (shm, w, h))) = self.ready_zoom.take() {
            if on_screen.iter().any(|o| o.key == key && o.px == (w, h)) {
                // The app laid the card out at this size (`Msg::ZoomReady`): show it in this frame.
                self.park(&key);
                let _ = self.show(out, key, &shm, (w, h), None, None);
            } else {
                // The app doesn't know its size yet: hand it over like a raster just made
                // (for what it was made for: if the view changed since, it's dropped).
                let _ = self.tx.send(Msg::Gfx(Gfx::Raster { key, made_for, done: Ok((shm, w, h)) }));
            }
        }
        // Diagrams on screen (only the focused one while focused) at the current target.
        for o in on_screen {
            if self.focused.as_ref().is_none_or(|focused| focused == &o.key) {
                self.raster(&o.key);
            }
        }
        let mut pause = Vec::new();
        let mut resume = Vec::new();
        for (key, img) in self.images.iter_mut() {
            let shown = on_screen.iter().find(|o| &o.key == key);
            if shown.is_some() {
                img.shown_at = self.frame;
            }
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
        // The zoomed image just took the card's place: hide (not delete) the feed-size one.
        if let Some(img) = self.zoom.as_mut().and_then(|zoom| zoom.img.as_mut()) {
            if img.placed.take().is_some() {
                let _ = out.write_all(&kitty::unplace(img.id));
            }
        }
        for id in self.dead.drain(..) {
            let _ = out.write_all(&kitty::delete(id));
        }
        self.keep_diagrams_within_budget(out);
        self.frame += 1;
        for page in pause {
            let _ = self.send(&HelperRequest::HtmlPause { id: page });
        }
        for page in resume {
            let _ = self.send(&HelperRequest::HtmlResume { id: page });
        }
    }

    /// Drop the least recently shown diagrams that aren't on screen while the terminal
    /// holds more diagram pixels than `DIAGRAM_BUDGET_BYTES`. No allocation unless over.
    fn keep_diagrams_within_budget(&mut self, out: &mut impl Write) {
        let bytes = |img: &Img| img.w as u64 * img.h as u64 * 4;
        let mut held: u64 = self.images.iter().filter(|(key, _)| self.svgs.contains_key(*key)).map(|(_, img)| bytes(img)).sum();
        if held <= DIAGRAM_BUDGET_BYTES {
            return;
        }
        let mut idle: Vec<(u64, VisualKey)> = self.images.iter()
            .filter(|(key, img)| self.svgs.contains_key(*key) && img.placed.is_none())
            .map(|(key, img)| (img.shown_at, key.clone()))
            .collect();
        idle.sort_unstable();
        for (_, key) in idle {
            if held <= DIAGRAM_BUDGET_BYTES {
                break;
            }
            let img = self.images.remove(&key).expect("listed above");
            held -= bytes(&img);
            for id in [Some(img.id), img.previous].into_iter().flatten() {
                let _ = out.write_all(&kitty::delete(id));
            }
            self.rastered.remove(&key);
            note(&mut self.trace, format_args!("evict {} ({} KB)", img.id, bytes(&img) / 1024));
        }
    }

    /// The screen was cleared (a resize). Placements are gone, and Rex drops the
    /// images too, so send every one again (at `target`); the next `place` shows them.
    pub fn replace_all(&mut self, out: &mut impl Write, target: Target, focused: Option<&VisualKey>) {
        self.target = target;
        self.focused = focused.cloned();
        // The terminal dropped every image: diagrams rasterize again as they come on screen.
        self.rastered.clear();
        let ahead = self.prefetch.take().and_then(|ahead| ahead.done);
        for (shm, ..) in ahead.into_iter().chain(self.ready_zoom.take().map(|(_, _, done)| done)) {
            kitty::unlink(&shm);
        }
        if let Some(zoom) = self.zoom.as_mut() {
            zoom.made_for = None;
            if let Some(img) = zoom.img.as_mut() {
                img.placed = None;
            }
        }
        note(&mut self.trace, format_args!("clear: sending {} images again", self.images.len()));
        let mut decode = Vec::new();
        for (key, img) in self.images.iter_mut() {
            img.placed = None;
            if let Some(old) = img.previous.take() {
                let _ = out.write_all(&kitty::delete(old));
            }
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
        let mut gallery = Gallery::new(tx, StageClient::new("/nonexistent".into()), std::env::temp_dir(), std::env::temp_dir(), Target { width_px: 800, max_height_px: 400, cell_px: (1, 1), html_height_px: 400 });
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
        let mut g = Gallery::new(tx, StageClient::new("/nonexistent".into()), std::env::temp_dir(), std::env::temp_dir(), Target { width_px: 660, max_height_px: 360, cell_px: (10, 20), html_height_px: 360 });
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
        let mut gallery = Gallery::new(tx, StageClient::new("/nonexistent".into()), std::env::temp_dir(), std::env::temp_dir(), Target { width_px: 600, max_height_px: 400, cell_px: (1, 1), html_height_px: 400 });
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
        assert!(!String::from_utf8_lossy(&transmitted).contains(&format!("a=d,d=I,i={old_id}")));
        let mut still_old = Vec::new();
        gallery.place(&mut still_old, std::slice::from_ref(&old));
        assert!(still_old.is_empty(), "the old sidebar layout keeps the old pixels");

        let mut switched = Vec::new();
        gallery.place(&mut switched, &[OnScreen { col: 0, cols: 95, px: (800, 200), ..old }]);
        let output = String::from_utf8_lossy(&switched);
        assert!(output.contains(&format!("a=d,d=I,i={old_id}")));
        assert!(output.contains(&format!("a=p,i={new_id}")));
    }

    #[test]
    fn leaving_focus_places_the_narrow_raster_once_the_layout_uses_it() {
        // z→z with the sidebar shown: the full-width raster is still on screen in the
        // normal column (same cell rect) when the normal-width one lands.
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut gallery = Gallery::new(tx, StageClient::new("/nonexistent".into()), std::env::temp_dir(), std::env::temp_dir(), Target { width_px: 950, max_height_px: 400, cell_px: (10, 20), html_height_px: 400 });
        let key: VisualKey = ("diagram".into(), 1, 0);
        let shms: Vec<String> = [(950, 400), (690, 300)].iter().map(|(w, h)| kitty::shm_write(&vec![0; w * h * 4]).unwrap()).collect();
        let raster = |shm: &str, w, h| Gfx::Raster { key: key.clone(), made_for: (950, MAX_MERMAID_HEIGHT_PX, (10, 20), None), done: Ok((shm.to_owned(), w, h)) };
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
        assert!(s.contains(&format!("a=d,d=I,i={wide_id}")) && s.contains(&format!("a=p,i={narrow_id},p=1,x=0,y=0,w=690,h=300,c=69,r=15")), "{s}");
        shms.iter().for_each(|shm| kitty::unlink(shm)); // no terminal here to consume them
    }

    /// `place` with these cards on screen, as the app would after a draw.
    fn shown(keys: &[&VisualKey]) -> Vec<OnScreen> {
        keys.iter().map(|k| OnScreen { key: (*k).clone(), col: 0, row: 0, cols: 10, top: 0, rows: 5, total: 5, px: (0, 0) }).collect()
    }

    fn next_raster(rx: &std::sync::mpsc::Receiver<Msg>) -> (VisualKey, RasterTarget, Result<(String, u32, u32), String>) {
        let Msg::Gfx(Gfx::Raster { key, made_for, done }) = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap() else { panic!("no raster") };
        (key, made_for, done)
    }

    #[test]
    fn only_diagrams_on_screen_rasterize_and_only_the_focused_one_while_focused() {
        let (tx, rx) = std::sync::mpsc::channel();
        let normal = Target { width_px: 100, max_height_px: 100, cell_px: (1, 1), html_height_px: 100 };
        let mut gallery = Gallery::new(tx, StageClient::new("/nonexistent".into()), std::env::temp_dir(), std::env::temp_dir(), normal);
        let (first, second, off): (VisualKey, VisualKey, VisualKey) = (("first".into(), 1, 0), ("second".into(), 1, 0), ("off".into(), 1, 0));
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"><rect width="100" height="50" fill="#89b4fa"/></svg>"##;
        for k in [&first, &second, &off] {
            gallery.svgs.insert(k.clone(), svg.into());
        }
        let unlink = |done: Result<(String, u32, u32), String>| kitty::unlink(&done.unwrap().0);
        gallery.place(&mut Vec::new(), &shown(&[&first, &second]));
        let mut keys = [next_raster(&rx), next_raster(&rx)].map(|(key, _, done)| { unlink(done); key });
        keys.sort();
        assert_eq!(keys, [first.clone(), second.clone()]);
        // Drawn again at the same target: nothing new.
        gallery.place(&mut Vec::new(), &shown(&[&first, &second]));
        assert!(rx.try_recv().is_err());

        gallery.retarget(Target { width_px: 200, ..normal }, Some(&second));
        gallery.place(&mut Vec::new(), &shown(&[&second]));
        let (key, made_for, done) = next_raster(&rx);
        assert_eq!((key, made_for), (second.clone(), (200, 100, (1, 1), Some(ZoomView::default()))));
        unlink(done);
        assert!(rx.try_recv().is_err(), "nothing but the focused card renders during focus");

        // Back to the normal width: both already have feed-size pixels, so nothing renders.
        gallery.retarget(normal, None);
        gallery.place(&mut Vec::new(), &shown(&[&first, &second]));
        assert!(rx.recv_timeout(std::time::Duration::from_millis(200)).is_err(), "an off-screen or already-current diagram never rasterizes");
    }

    #[test]
    fn rasterizes_svg_at_the_card_width() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"><rect width="100" height="50" fill="#89b4fa"/></svg>"##;
        let (px, w, h) = rasterize(svg, 150, 1000, (1, 1), 2.0).unwrap();
        assert_eq!((w, h), (150, 75));
        assert_eq!(&px[..4], &[0x89, 0xb4, 0xfa, 0xff]);
        // Never past 2× the SVG's own size or the explicit height budget.
        assert_eq!(rasterize(svg, 1000, 1000, (1, 1), 2.0).unwrap().1, 200);
        assert_eq!(rasterize(svg, 1000, 40, (1, 1), 2.0).unwrap().2, 40);
    }

    #[test]
    fn a_focused_diagram_fills_the_pane_at_full_resolution() {
        // Vector: zooming re-rasterizes as large as the pane allows (width and height, so
        // nothing to scroll) instead of stopping at 2x the diagram's own size.
        let (tx, rx) = std::sync::mpsc::channel();
        let target = Target { width_px: 1000, max_height_px: 400, cell_px: (1, 1), html_height_px: 400 };
        let mut gallery = Gallery::new(tx, StageClient::new("/nonexistent".into()), std::env::temp_dir(), std::env::temp_dir(), target);
        let key: VisualKey = ("small".into(), 1, 0);
        gallery.svgs.insert(key.clone(), r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"/>"#.into());
        gallery.place(&mut Vec::new(), &shown(&[&key]));
        let (_, _, done) = next_raster(&rx);
        let (shm, w, h) = done.unwrap();
        kitty::unlink(&shm);
        assert_eq!((w, h), (200, 100), "in the feed: native size (2x CSS px)");
        gallery.retarget(target, Some(&key));
        gallery.place(&mut Vec::new(), &shown(&[&key]));
        let (_, _, done) = next_raster(&rx);
        let (shm, w, h) = done.unwrap();
        kitty::unlink(&shm);
        assert_eq!((w, h), (800, 400), "focused: as large as fits the 1000x400 pane");
    }

    #[test]
    fn closing_focus_puts_the_feed_pixels_back_without_a_new_raster() {
        let (tx, rx) = std::sync::mpsc::channel();
        let target = Target { width_px: 1000, max_height_px: 400, cell_px: (1, 1), html_height_px: 400 };
        let mut gallery = Gallery::new(tx, StageClient::new("/nonexistent".into()), std::env::temp_dir(), std::env::temp_dir(), target);
        let key: VisualKey = ("small".into(), 1, 0);
        gallery.svgs.insert(key.clone(), r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"/>"#.into());
        let at = |px| OnScreen { key: key.clone(), col: 0, row: 0, cols: 10, top: 0, rows: 5, total: 5, px };
        gallery.place(&mut Vec::new(), &[at((0, 0))]);
        let (key_, made_for, done) = next_raster(&rx);
        gallery.handle(&mut Vec::new(), Gfx::Raster { key: key_, made_for, done });
        gallery.place(&mut Vec::new(), &[at((200, 100))]);
        let feed_id = gallery.images[&key].id;
        gallery.retarget(target, Some(&key));
        gallery.place(&mut Vec::new(), &[at((200, 100))]);
        let (key_, made_for, done) = next_raster(&rx);
        gallery.handle(&mut Vec::new(), Gfx::Raster { key: key_, made_for, done });
        let zoomed_id = gallery.images[&key].id;
        let mut out = Vec::new();
        gallery.place(&mut out, &[at((800, 400))]);
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains(&format!("a=p,i={zoomed_id},")) && s.contains(&format!("a=d,d=i,i={feed_id},p=1")), "zoomed shown, feed pixels hidden but kept: {s}");
        assert!(!s.contains(&format!("a=d,d=I,i={feed_id},q=2")));
        gallery.retarget(target, None);
        let mut out = Vec::new();
        gallery.place(&mut out, &[at((200, 100))]);
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains(&format!("a=p,i={feed_id},")) && s.contains(&format!("a=d,d=I,i={zoomed_id},q=2")), "{s}");
        assert!(rx.recv_timeout(std::time::Duration::from_millis(200)).is_err(), "the feed pixels are still right: no raster");
    }

    #[test]
    fn a_zoom_made_ahead_shows_in_the_first_focused_frame() {
        let (tx, rx) = std::sync::mpsc::channel();
        let feed = Target { width_px: 800, max_height_px: 400, cell_px: (1, 1), html_height_px: 400 };
        let focus = Target { width_px: 1000, ..feed };
        let mut gallery = Gallery::new(tx, StageClient::new("/nonexistent".into()), std::env::temp_dir(), std::env::temp_dir(), feed);
        let key: VisualKey = ("chart".into(), 1, 0);
        gallery.svgs.insert(key.clone(), r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"/>"#.into());
        assert!(gallery.prefetch_zoom(key.clone(), focus).is_none());
        let (key_, made_for, done) = next_raster(&rx);
        assert_eq!(made_for, (1000, 400, (1, 1), Some(ZoomView::default())));
        assert!(matches!(gallery.handle(&mut Vec::new(), Gfx::Raster { key: key_, made_for, done }).as_slice(), [Msg::ZoomReady { w: 800, h: 400, .. }]));
        gallery.retarget(focus, Some(&key));
        let mut out = Vec::new();
        gallery.place(&mut out, &[OnScreen { key: key.clone(), col: 0, row: 0, cols: 800, top: 0, rows: 400, total: 400, px: (800, 400) }]);
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains("s=800,v=400") && s.contains("a=p,") && s.contains("w=800,h=400"), "{s}");
        assert!(rx.recv_timeout(std::time::Duration::from_millis(200)).is_err(), "focus starts no raster: it was made ahead");
    }

    #[test]
    fn diagrams_past_the_budget_are_dropped_least_recent_first_and_come_back() {
        let (tx, rx) = std::sync::mpsc::channel();
        let target = Target { width_px: 4096, max_height_px: 400, cell_px: (1, 1), html_height_px: 400 };
        let mut gallery = Gallery::new(tx, StageClient::new("/nonexistent".into()), std::env::temp_dir(), std::env::temp_dir(), target);
        // 16 MB each: nine of them are past the 128 MB budget.
        let keys: Vec<VisualKey> = (0..9).map(|i| (format!("d{i}").into(), 1, 0)).collect();
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="2048" height="512"/>"#;
        let mut out = Vec::new();
        for key in &keys {
            gallery.svgs.insert(key.clone(), svg.into());
            gallery.place(&mut out, &shown(&[key]));
            let (key_, made_for, done) = next_raster(&rx);
            gallery.handle(&mut Vec::new(), Gfx::Raster { key: key_, made_for, done });
            gallery.place(&mut out, &shown(&[key]));
        }
        let first = String::from_utf8_lossy(&out);
        assert!(!gallery.images.contains_key(&keys[0]) && gallery.images.contains_key(&keys[1]), "only the least recent one goes");
        assert!(first.contains("a=d,d=I,"), "and its pixels are freed in the terminal: {first}");
        gallery.place(&mut Vec::new(), &shown(&[&keys[0]]));
        let (key, _, done) = next_raster(&rx);
        assert_eq!(key, keys[0], "seen again, it rasterizes again");
        kitty::unlink(&done.unwrap().0);
    }

    #[test]
    fn asking_again_for_a_zoom_already_made_says_its_size_again() {
        // Found by the stage-pixels model: z, z before a draw, then z again showed
        // zoomed pixels squeezed into the feed layout, because the app forgot the size.
        let (tx, rx) = std::sync::mpsc::channel();
        let feed = Target { width_px: 800, max_height_px: 400, cell_px: (1, 1), html_height_px: 400 };
        let focus = Target { width_px: 1000, ..feed };
        let mut gallery = Gallery::new(tx, StageClient::new("/nonexistent".into()), std::env::temp_dir(), std::env::temp_dir(), feed);
        let key: VisualKey = ("chart".into(), 1, 0);
        gallery.svgs.insert(key.clone(), r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"/>"#.into());
        assert!(gallery.prefetch_zoom(key.clone(), focus).is_none());
        let (key_, made_for, done) = next_raster(&rx);
        gallery.handle(&mut Vec::new(), Gfx::Raster { key: key_, made_for, done });
        assert!(matches!(gallery.prefetch_zoom(key.clone(), focus), Some(Msg::ZoomReady { w: 800, h: 400, .. })));
        assert!(rx.recv_timeout(std::time::Duration::from_millis(200)).is_err(), "and doesn't rasterize it again");
        if let Some(Prefetch { done: Some((shm, ..)), .. }) = gallery.prefetch.take() { kitty::unlink(&shm); }
    }

    #[test]
    fn a_zoomed_view_draws_only_its_window_at_full_resolution() {
        // Left half red, right half blue; a 400x400 pane at level 2 (2.25x past fitting).
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><rect width="50" height="100" fill="#ff0000"/><rect x="50" width="50" height="100" fill="#0000ff"/></svg>"##;
        let window = |view| {
            let (tree, at, w, h) = fit_view(parse(svg).unwrap(), 400, 400, (1, 1), view).unwrap();
            let mut px = vec![0; (w * h * 4) as usize];
            draw(&tree, at, &mut px, (w, h)).unwrap();
            (w, h, px)
        };
        let (w, h, fit) = window(ZoomView::default());
        assert_eq!((w, h), (400, 400), "level 0 fits the pane");
        assert_eq!((&fit[..4], &fit[fit.len() - 4..]), (&[255, 0, 0, 255][..], &[0, 0, 255, 255][..]));
        let (w, h, left) = window(ZoomView { level: 2, cx: 0, cy: 500 });
        assert_eq!((w, h), (400, 400), "zoomed in: still one pane of pixels");
        assert!(left.chunks(4).all(|p| p == [255, 0, 0, 255]), "panned left: only the red half");
        let (_, _, right) = window(ZoomView { level: 2, cx: 1000, cy: 500 });
        assert!(right.chunks(4).all(|p| p == [0, 0, 255, 255]), "panned right: only the blue half");
    }

    #[test]
    fn resize_cannot_replace_crisp_pixels_with_an_older_raster() {
        let (tx, rx) = std::sync::mpsc::channel();
        let at = |width_px| Target { width_px, max_height_px: 100, cell_px: (1, 1), html_height_px: 100 };
        let mut gallery = Gallery::new(tx, StageClient::new("/nonexistent".into()), std::env::temp_dir(), std::env::temp_dir(), at(120));
        let key: VisualKey = ("diagram".into(), 1, 0);
        gallery.svgs.insert(key.clone(), r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"/>"#.into());
        gallery.place(&mut Vec::new(), &shown(&[&key]));
        let old = next_raster(&rx);
        gallery.retarget(at(180), None);
        gallery.place(&mut Vec::new(), &shown(&[&key]));
        let latest = next_raster(&rx);
        let (shm, w, h) = latest.2.unwrap();
        assert_eq!((w, h), (180, 90));
        assert_eq!(gallery.handle(&mut Vec::new(), Gfx::Raster { key: key.clone(), made_for: latest.1, done: Ok((shm.clone(), w, h)) }).len(), 1);
        kitty::unlink(&shm); // the test has no terminal to consume the transmitted pixels
        let (stale_shm, stale_w, stale_h) = old.2.unwrap();
        assert!(gallery.handle(&mut Vec::new(), Gfx::Raster { key: key.clone(), made_for: old.1, done: Ok((stale_shm.clone(), stale_w, stale_h)) }).is_empty());
        assert!(kitty::shm_read(&stale_shm, 1).is_err(), "a dropped raster's shm is unlinked");
        assert_eq!((gallery.images[&key].w, gallery.images[&key].h), (180, 90));
    }

    #[test]
    fn a_dropped_raster_is_asked_for_again_when_its_width_comes_back() {
        let (tx, rx) = std::sync::mpsc::channel();
        let at = |width_px| Target { width_px, max_height_px: 100, cell_px: (1, 1), html_height_px: 100 };
        let mut gallery = Gallery::new(tx, StageClient::new("/nonexistent".into()), std::env::temp_dir(), std::env::temp_dir(), at(100));
        let key: VisualKey = ("diagram".into(), 1, 0);
        gallery.svgs.insert(key.clone(), r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"/>"#.into());
        gallery.place(&mut Vec::new(), &shown(&[&key]));
        let first = next_raster(&rx);
        // It lands while the width is briefly different (a focus round trip), so it's dropped…
        gallery.retarget(at(200), None);
        gallery.handle(&mut Vec::new(), Gfx::Raster { key: key.clone(), made_for: first.1, done: first.2 });
        gallery.retarget(at(100), None);
        // …and the next draw asks again instead of waiting forever for pixels already thrown away.
        gallery.place(&mut Vec::new(), &shown(&[&key]));
        let again = next_raster(&rx);
        assert_eq!(again.1, (100, MAX_MERMAID_HEIGHT_PX, (1, 1), None));
        kitty::unlink(&again.2.unwrap().0);
    }

    #[test]
    fn tall_mermaid_retains_card_width_instead_of_shrinking_to_viewport() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="650" height="816"><rect width="650" height="816"/></svg>"#;
        let (_, w, h) = rasterize(svg, 700, MAX_MERMAID_HEIGHT_PX, (1, 1), 2.0).unwrap();
        assert_eq!((w, h), (700, 879));
    }

    #[test]
    fn mermaid_fills_whole_cells_so_kitty_never_resamples() {
        // 420×415.5 CSS px at 2× is 840×831; with 10×20 px cells that rounded up
        // to 84×42 cells (840×840 px) and kitty stretched it ~1% vertically.
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="420" height="415.5"><rect width="420" height="415.5" fill="red"/></svg>"#;
        let cell = (10, 20);
        let (px, w, h) = rasterize(svg, 840, MAX_MERMAID_HEIGHT_PX, cell, 2.0).unwrap();
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
