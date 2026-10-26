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
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
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
    Raster { key: VisualKey, done: Result<(String, u32, u32), String> },
}

/// Mermaid SVG background: the card's own color, so the image has no seam.
const BG: [u8; 4] = [0x1e, 0x1e, 0x2e, 0xff];
/// CSS px → device px for helper renders (Retina).
const SCALE: f64 = 2.0;

struct Img {
    id: u32,
    w: u32,
    h: u32,
    placed: Option<Place>,
    /// The html page streaming into this image, if live.
    page: Option<u64>,
    paused: bool,
}

struct Helper {
    stdin: ChildStdin,
    _child: Child,
}

enum Pending {
    Mermaid(VisualKey),
    Image(VisualKey),
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
    cache: PathBuf,
}

fn fonts() -> Arc<resvg::usvg::fontdb::Database> {
    static DB: OnceLock<Arc<resvg::usvg::fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = resvg::usvg::fontdb::Database::new();
        db.load_system_fonts();
        Arc::new(db)
    })
    .clone()
}

/// SVG → RGBA at `width_px` (never past 2× its CSS size), over the card color.
pub fn rasterize(svg: &str, width_px: u32) -> Result<(Vec<u8>, u32, u32), String> {
    let opt = resvg::usvg::Options { fontdb: fonts(), ..Default::default() };
    let tree = resvg::usvg::Tree::from_str(svg, &opt).map_err(|e| format!("svg: {e}"))?;
    let size = tree.size();
    let scale = (width_px as f32 / size.width()).min(SCALE as f32);
    let (w, h) = ((size.width() * scale).ceil() as u32, (size.height() * scale).ceil() as u32);
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w.max(1), h.max(1)).ok_or("empty diagram")?;
    pixmap.fill(resvg::tiny_skia::Color::from_rgba8(BG[0], BG[1], BG[2], BG[3]));
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    // Opaque background, so premultiplied RGBA is already straight RGBA.
    Ok((pixmap.take(), w.max(1), h.max(1)))
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
    pub fn new(tx: Sender<Msg>, client: StageClient, cache: PathBuf, target: Target) -> Gallery {
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
            cache,
        }
    }

    /// Send one request to stage-helper, starting it if needed.
    fn ask(&mut self, mut req: Value) -> Result<u64, String> {
        if self.helper.is_none() {
            let mut child = Command::new(helper_path())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("stage-helper isn't available ({e})"))?;
            let stdout = child.stdout.take().expect("piped");
            let tx = self.tx.clone();
            thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if tx.send(Msg::Gfx(Gfx::Helper(line))).is_err() {
                        return;
                    }
                }
                let _ = tx.send(Msg::Gfx(Gfx::HelperExited));
            });
            let stdin = child.stdin.take().expect("piped");
            self.helper = Some(Helper { stdin, _child: child });
        }
        let id = self.next_req;
        self.next_req += 1;
        req["id"] = json!(id);
        let line = format!("{req}\n");
        let helper = self.helper.as_mut().expect("started above");
        if helper.stdin.write_all(line.as_bytes()).and_then(|()| helper.stdin.flush()).is_err() {
            self.helper = None;
            return Err("stage-helper stopped".into());
        }
        Ok(id)
    }

    fn write_cache(&self, name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
        let path = self.cache.join(name);
        std::fs::create_dir_all(&self.cache).and_then(|()| std::fs::write(&path, bytes)).map_err(|e| e.to_string())?;
        Ok(path)
    }

    fn html_size(&self) -> (f64, f64) {
        (self.target.width_px as f64 / SCALE, self.target.max_height_px as f64 / SCALE)
    }

    /// Start making a visual's pixels. Failures come back as messages for the app.
    pub fn need(&mut self, key: VisualKey, source: VisualSource, target: Target) -> Vec<Msg> {
        self.target = target;
        let result = match &source {
            VisualSource::Mermaid(src) => self
                .ask(json!({"op": "mermaid", "source": src, "max_width": target.width_px as f64 / SCALE, "font_size": 14}))
                .map(|req| {
                    self.pending.insert(req, Pending::Mermaid(key.clone()));
                }),
            VisualSource::Asset(asset) => self
                .client
                .asset_get(asset.clone())
                .map_err(|e| e.message)
                .and_then(|data| self.write_cache(&format!("asset-{asset}"), &data))
                .and_then(|path| self.ask(json!({"op": "image", "path": path})))
                .map(|req| {
                    self.pending.insert(req, Pending::Image(key.clone()));
                }),
            VisualSource::Html(html) => {
                let name = format!("page-{}-{}-{}.html", key.0, key.1, key.2);
                let (width, max_height) = self.html_size();
                self.write_cache(&name, html.as_bytes())
                    .and_then(|path| {
                        self.ask(json!({"op": "html-open", "path": path, "width": width, "max_height": max_height, "scale": SCALE, "fps": 60}))
                    })
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

    /// The card width or pane height changed: re-render what depends on it.
    pub fn retarget(&mut self, target: Target) {
        if target == self.target {
            return;
        }
        self.target = target;
        for (key, svg) in self.svgs.clone() {
            self.raster(key, svg);
        }
        let (width, max_height) = self.html_size();
        for page in self.pages.keys().copied().collect::<Vec<_>>() {
            let _ = self.send(json!({"op": "html-resize", "id": page, "width": width, "max_height": max_height, "scale": SCALE}));
        }
    }

    /// A request about an existing page (its id is the page id, not a new one).
    fn send(&mut self, req: Value) -> Result<(), String> {
        let helper = self.helper.as_mut().ok_or("stage-helper isn't running")?;
        helper.stdin.write_all(format!("{req}\n").as_bytes()).and_then(|()| helper.stdin.flush()).map_err(|e| e.to_string())
    }

    fn raster(&self, key: VisualKey, svg: String) {
        let (tx, width) = (self.tx.clone(), self.target.width_px);
        thread::spawn(move || {
            let done = rasterize(&svg, width).and_then(|(px, w, h)| {
                kitty::shm_write(&px).map(|shm| (shm, w, h)).map_err(|e| format!("shared memory: {e}"))
            });
            let _ = tx.send(Msg::Gfx(Gfx::Raster { key, done }));
        });
    }

    /// New pixels for `key`: transmit them, re-placing the image if it's on screen.
    fn show(&mut self, out: &mut impl Write, key: VisualKey, shm: &str, (w, h): (u32, u32), page: Option<u64>) -> Option<Msg> {
        let fresh = !self.images.contains_key(&key);
        let next_id = &mut self.next_id;
        let img = self.images.entry(key.clone()).or_insert_with(|| {
            let id = *next_id;
            *next_id += 1;
            Img { id, w, h, placed: None, page, paused: false }
        });
        let resized = (img.w, img.h) != (w, h);
        (img.w, img.h) = (w, h);
        // Same size and on screen: replace the frame in place. Otherwise just
        // transmit; the next `place` positions it.
        // `placed` already holds the pixel rect (see `place`); don't crop it twice.
        let place = img.placed.filter(|_| !resized);
        let _ = out.write_all(b"\x1b[?2026h");
        let _ = out.write_all(&kitty::transmit(img.id, shm, (w, h), place.as_ref()));
        let _ = out.write_all(b"\x1b[?2026l");
        let _ = out.flush();
        if place.is_none() {
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
                        Pending::Mermaid(key) | Pending::Image(key) => Msg::VisualFailed { key, reason: "stage-helper stopped".into() },
                    })
                    .collect();
                msgs.extend(self.pages.drain().map(|(_, key)| Msg::VisualFailed { key, reason: "stage-helper stopped".into() }));
                msgs
            }
            Gfx::Raster { key, done } => match done {
                Ok((shm, w, h)) => self.show(out, key, &shm, (w, h), None).into_iter().collect(),
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
                    let msg = self.show(out, key, shm, size, Some(id));
                    let _ = self.send(json!({"op": "html-ack", "id": id, "frame": frame}));
                    return msg.into_iter().collect();
                }
                if v["ok"] == json!(false) {
                    let reason = v["error"].as_str().unwrap_or("stage-helper failed").to_owned();
                    let key = match self.pending.remove(&id) {
                        Some(Pending::Mermaid(k) | Pending::Image(k)) => Some(k),
                        None => self.pages.remove(&id),
                    };
                    return key.map(|key| Msg::VisualFailed { key, reason }).into_iter().collect();
                }
                match self.pending.remove(&id) {
                    Some(Pending::Mermaid(key)) => {
                        let svg = v["svg"].as_str().unwrap_or_default().to_owned();
                        self.svgs.insert(key.clone(), svg.clone());
                        self.raster(key, svg);
                        vec![]
                    }
                    Some(Pending::Image(key)) => match (v["shm"].as_str(), dims()) {
                        (Some(shm), Some(size)) => self.show(out, key, shm, size, None).into_iter().collect(),
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
            let wanted = on_screen.iter().find(|o| &o.key == key).map(|o| {
                crop(&Place { col: o.col, row: o.row, cols: o.cols, rows: o.rows, src: (0, o.top as u32, 0, o.total as u32) }, (img.w, img.h))
            });
            match (wanted, img.placed) {
                (Some(p), Some(q)) if p == q => {}
                (Some(p), _) => {
                    let _ = out.write_all(&kitty::place(img.id, &p));
                    img.placed = Some(p);
                }
                (None, Some(_)) => {
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
                    (true, true) => {
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

    /// The screen was cleared (a resize): placements are gone, so place again.
    pub fn replace_all(&mut self) {
        for img in self.images.values_mut() {
            img.placed = None;
        }
    }

    /// Remove every image (on quit).
    pub fn clear(&mut self, out: &mut impl Write) {
        let _ = out.write_all(kitty::FREE_ALL);
        self.images.clear();
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
    fn live_frames_reuse_the_placed_pixel_rect() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut g = Gallery::new(tx, StageClient::new("/nonexistent".into()), std::env::temp_dir(), Target { width_px: 660, max_height_px: 360 });
        let key: VisualKey = ("p".into(), 1, 0);
        g.pages.insert(9, key.clone());
        let frame = |n: u64| Gfx::Helper(format!(r#"{{"id":9,"frame":{n},"shm":"/stg-test-{n}","w":660,"h":360}}"#));
        let mut out = Vec::new();
        g.handle(&mut out, frame(1));
        g.place(&mut Vec::new(), &[OnScreen { key: key.clone(), col: 3, row: 4, cols: 40, top: 0, rows: 10, total: 10 }]);
        out.clear();
        g.handle(&mut out, frame(2));
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains("a=T") && s.contains("x=0,y=0,w=660,h=360,c=40,r=10"), "{s}");
    }

    #[test]
    fn rasterizes_svg_at_the_card_width() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"><rect width="100" height="50" fill="#89b4fa"/></svg>"##;
        let (px, w, h) = rasterize(svg, 150).unwrap();
        assert_eq!((w, h), (150, 75));
        assert_eq!(&px[..4], &[0x89, 0xb4, 0xfa, 0xff]);
        // Never past 2× the SVG's own size.
        assert_eq!(rasterize(svg, 1000).unwrap().1, 200);
    }
}
