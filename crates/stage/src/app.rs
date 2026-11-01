//! All of stage's state and every decision, with no IO: `App::update` takes a
//! message (a key, a mouse event, a notesd event or a reply) and returns the
//! effects for `main` to perform. Requests are limited to [`StageRequest`], whose
//! writes are comments, session renames and deletes the user confirmed.

use std::{collections::{HashMap, HashSet}, time::Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use notes_proto::{
    posts::{
        Comment, CommentNewArgs, Part, Post, PostId, PostRender, PostShowArgs, PostSummary, Session,
        SessionHit, SessionId, SessionRenameArgs, SessionSearchArgs, StageSpace,
    },
    ErrorBody, ErrorCode, Event,
};

use ratatui::text::{Line, Span};

use crate::{
    cards::{self, build_cards, Card, CardBody, Fit, Folds, Hit, Laid, Origin, Slot, VisualSource},
    commands, ui,
};

/// The open session's posts as one stream, oldest first, the way sideshow's
/// viewer shows a session: each post's title, then its part cards and comments.
#[derive(Default)]
pub struct Feed {
    pub lines: Vec<Line<'static>>,
    /// Per line: the post (index into `App::posts`) and what in it.
    pub hits: Vec<(usize, Hit)>,
    /// Per line: the card, source line and wrap row it came from.
    pub origins: Vec<Origin>,
    pub slots: Vec<(VisualKey, Slot)>,
    /// First line of each post, by index into `App::posts`.
    pub starts: Vec<usize>,
    /// Ascending lines where a card starts and where a diff hunk header sits.
    pub cards: Vec<usize>,
    pub hunks: Vec<usize>,
    /// Per post: the layout its lines came from, so the next rebuild moves them instead of copying.
    laid: Vec<Option<(PostId, u64)>>,
    #[cfg(debug_assertions)]
    inputs: FeedInputs,
}

impl Feed {
    /// The line of `post` showing `spot`'s source line (its same wrap row, else the
    /// last), or `None` when that source line no longer exists.
    fn locate(&self, post: usize, spot: &Spot) -> Option<usize> {
        let end = self.starts.get(post + 1).copied().unwrap_or(self.lines.len());
        let rows: Vec<usize> = (self.starts[post]..end)
            .filter(|&i| (self.origins[i].card, self.origins[i].src) == (spot.origin.card, spot.origin.src))
            .collect();
        rows.get(spot.origin.wrap).or(rows.last()).copied()
    }

    /// Where `line` is: its post, source origin, and how far into the post.
    fn spot(&self, apps: &[PostSummary], line: usize) -> Option<Spot> {
        let (post, _) = *self.hits.get(line)?;
        Some(Spot { post: apps.get(post)?.id.clone(), origin: self.origins[line], offset: line - self.starts[post] })
    }
}

/// A feed line named by content: the post and source line it shows, plus its row
/// offset in that post for when the source line is gone.
struct Spot {
    post: PostId,
    origin: Origin,
    offset: usize,
}

/// The reader's place across a rebuild: the line at the top of the viewport and the
/// cursor's line, each by source identity, and the cursor's screen row for the fallback.
struct Anchor {
    top: Option<Spot>,
    cursor: Spot,
    cursor_row: usize,
}

/// When the feed was built: `feed_gen`, width, page height and cell px.
type FeedStamp = (u64, u16, u16, (u16, u16));

/// Everything the feed reads besides its stamp: per post (id, version, fold generation,
/// built, styled), the comments, visuals and focused card. Debug builds check that
/// none of it changed without `feed_gen` moving.
#[cfg(debug_assertions)]
type FeedInputs = (Vec<(PostId, u64, u64, bool, bool)>, Vec<Comment>, HashMap<VisualKey, VisualState>, Option<(PostId, usize)>);

/// One visual card: (post, version, card index).
pub type VisualKey = (PostId, u64, usize);

#[derive(Debug, Clone, PartialEq)]
pub enum VisualState {
    Pending,
    /// Pixels exist at `w`×`h`; `max_scale` caps upscaling (1: never).
    Ready { w: u32, h: u32, max_scale: f32 },
    Failed(String),
}

/// Pixel budget for a visual: card width and the live HTML viewport height.
/// Mermaid can extend beyond that height so the feed scrolls through it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Target {
    pub width_px: u32,
    pub max_height_px: u32,
    /// An html page's height cap: the whole feed below the post's title.
    pub html_height_px: u32,
    /// Cell size in px: rasters are padded to whole cells so kitty never resamples them.
    pub cell_px: (u16, u16),
}

/// Where a focused diagram is zoomed: `level` 1.5x steps past fitting the pane, centered
/// on (`cx`, `cy`), per mille of the diagram's width and height.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZoomView {
    pub level: u8,
    pub cx: u16,
    pub cy: u16,
}

impl Default for ZoomView {
    fn default() -> Self {
        ZoomView { level: 0, cx: 500, cy: 500 }
    }
}

const MAX_ZOOM_LEVEL: u8 = 6;

/// A visual on screen this frame: top-left cell, size in cells, and how many
/// of its rows are cut off above (`top`) and shown (`rows` of `total`).
#[derive(Debug, Clone, PartialEq)]
pub struct OnScreen {
    pub key: VisualKey,
    pub col: u16,
    pub row: u16,
    pub cols: u16,
    pub top: u16,
    pub rows: u16,
    pub total: u16,
    /// The pixel size the layout sized this card for (its `VisualState::Ready`).
    pub px: (u32, u32),
}

/// The only requests stage can make. Its writes are `CommentNew`, `SessionRename`
/// and the deletes, each behind a key the user pressed (deletes after a `y`).
#[derive(Debug, Clone, PartialEq)]
pub enum StageRequest {
    SessionList,
    SessionSearch(SessionSearchArgs),
    PostList(SessionId),
    PostShow(PostShowArgs),
    PostRender(PostShowArgs),
    CommentNew(CommentNewArgs),
    CommentList(SessionId),
    SessionRename(SessionRenameArgs),
    SessionDelete(SessionId),
    PostDelete(PostId),
    /// Remove one part (card) of a post's latest version: `post-part rm`.
    PartDelete(PostId, usize),
    /// Record the room a card gives a post, for agents (`STAGE_SPACE`).
    Space(StageSpace),
}

#[derive(Debug)]
pub enum Reply {
    Sessions(Vec<Session>),
    Hits(String, Vec<SessionHit>),
    Posts(SessionId, Vec<PostSummary>),
    Post(Post),
    Render(PostRender),
    Comments(SessionId, Vec<Comment>),
    Commented(Comment),
    Saved,
    Failed(StageRequest, ErrorBody),
}

#[derive(Debug)]
pub enum Msg {
    Key(KeyEvent),
    Mouse(MouseEvent),
    /// A wheel tick and when the terminal reader got it (`Mouse` ticks count as now).
    Wheel(MouseEvent, Instant),
    Resize(u16, u16),
    /// The terminal's cell size in px changed (or became known).
    CellPx((u16, u16)),
    Event(Event),
    Reply(Reply),
    /// The watch stream dropped; it reconnects on its own with `since = last_seq`.
    WatchLost,
    WatchBack,
    /// Terminal probe replies (cell size in px, kitty graphics support).
    Probed { cell_px: Option<(u16, u16)>, kitty: bool },
    /// A visual's pixels are ready, or can't be made.
    VisualReady { key: VisualKey, w: u32, h: u32, max_scale: f32 },
    VisualFailed { key: VisualKey, reason: String },
    /// A focused card's zoomed pixels are showing (ignored once focus has moved on).
    ZoomShown { key: VisualKey, w: u32, h: u32 },
    /// A zoomed raster for `key` is ready ahead of `z` (see `Effect::PrefetchZoom`).
    ZoomReady { key: VisualKey, w: u32, h: u32 },
    /// Graphics plumbing (helper replies, rasterized frames); `main` handles these.
    Gfx(crate::gallery::Gfx),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Request(StageRequest),
    /// OSC 52 copy of this text.
    Copy(String),
    /// Write `contents` to stage's cache folder under `name` and `open` it.
    OpenFile { name: String, contents: String },
    Redraw,
    Quit,
    /// Print and exit 2 (unknown `--session`).
    Fatal(String),
    /// Make pixels for a visual card.
    NeedVisual { key: VisualKey, source: VisualSource },
    /// Rasterize the diagram under the cursor for focus now, so `z` shows it crisp at once.
    PrefetchZoom { key: VisualKey, target: Target },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Nav,
    Search { query: String, hits: Vec<SessionHit>, sel: usize },
    Comment { input: String },
    Command { input: String },
    Rename { id: SessionId, input: String },
    /// Waiting for `y` to send `request`, which deletes `what`.
    Delete { what: String, request: StageRequest },
}

/// The diagram whose zoomed raster was asked for ahead of `z`, for which target,
/// and its size once it's ready.
struct ZoomAhead {
    key: VisualKey,
    target: Target,
    size: Option<(u32, u32)>,
}

pub struct Loaded {
    pub post: Option<Post>,
    pub render: Option<PostRender>,
    pub cards: Option<Vec<Card>>,
}

/// Everything a layout reads: version, width, fold generation, focused card, styled,
/// its comments, and each card's visual fit.
type LaidKey = (u64, u16, u64, Option<usize>, bool, Vec<Comment>, Vec<Fit>);

/// Per post: folds, a pinned version, and the post's own layout cache.
#[derive(Default)]
pub struct View {
    pub folds: Folds,
    /// A version picked with `v`; `None` follows the latest.
    pub pinned: Option<u64>,
    /// The newest version that has cards, shown until the latest has them too.
    built: Option<u64>,
    laid: Option<(LaidKey, u64, Laid)>,
    fold_gen: u64,
}

pub struct App {
    pub sessions: Vec<Session>,
    pub open: Option<SessionId>,
    /// The open session's posts, oldest first.
    pub posts: Vec<PostSummary>,
    pub comments: Vec<Comment>,
    pub unread: HashSet<SessionId>,
    pub unseen: HashSet<PostId>,
    pub loaded: HashMap<(PostId, u64), Loaded>,
    /// Plain posts whose render failed while notesd was away; asked again when the watch is back.
    restyle: HashSet<(PostId, u64)>,
    pub views: HashMap<PostId, View>,
    pub shown: Option<PostId>,
    /// The sidebar's selected session (index into `sessions`).
    pub sel: usize,
    pub mode: Mode,
    /// Whether the session sidebar is hidden outside card focus.
    pub sidebar_hidden: bool,
    /// Full-width session picker on narrow terminals; independent of desktop preference.
    pub session_picker: bool,
    pub size: (u16, u16),
    /// The space last sent to notesd, so a resize only writes on change.
    space: Option<StageSpace>,
    pub cell_px: (u16, u16),
    pub kitty: bool,
    pub toast: Option<String>,
    pub banner: Option<String>,
    /// `--session` until the first session list arrives.
    want: Option<SessionId>,
    /// Show the newest post when one arrives (the user was on the latest).
    follow: bool,
    /// Posts that entered the sidebar since the last paint, for the latency log.
    pub fresh: Vec<(PostId, u64)>,
    seen_rows: HashSet<(PostId, u64)>,
    /// Card builds, plain then styled per (post, version); tests assert there are no repeats.
    pub builds: usize,
    pub visuals: HashMap<VisualKey, VisualState>,
    /// Bumped whenever something the feed reads changes (posts, loads, comments,
    /// visuals, folds, pins), so `feed()` knows it's current without looking at every post.
    feed_gen: u64,
    /// Numbers each post layout, so a feed rebuild can tell which posts' lines it can keep.
    layouts: u64,
    /// Visuals drawn this frame (set by `ui::draw`), for `main` to place.
    pub on_screen: Vec<OnScreen>,
    /// Feed scroll (first line on screen) and cursor line.
    pub scroll: usize,
    pub cursor: usize,
    /// The post and card isolated in the full-width focus view.
    pub focused_card: Option<(PostId, usize)>,
    /// Keep the feed visible until a resized HTML frame is ready for focus.
    pending_html_focus: Option<VisualKey>,
    /// Keep the old layout until each visible HTML card has pixels at the new sidebar width.
    pending_sidebar: Option<(bool, HashSet<VisualKey>)>,
    /// Hold resized visual dimensions until every visible HTML card can switch together.
    pending_sidebar_frames: HashMap<VisualKey, VisualState>,
    /// Feed position to restore when the focus view closes.
    /// Feed position and follow to restore when the focus view closes.
    before_focus: Option<(usize, usize, bool)>,
    /// A focused diagram's feed-size pixels: the gallery puts them back when focus
    /// closes, and the layout must match them in that same frame.
    unzoomed: Option<(VisualKey, VisualState)>,
    zoom_ahead: Option<ZoomAhead>,
    /// What a card showed when focus closed, until the next draw: `z` again before the
    /// gallery has seen focus close finds it still showing those pixels.
    left_focus: Option<(VisualKey, VisualState)>,
    /// The focused diagram's zoom and pan (`+`/`-`/`0`, arrows or hjkl).
    pub zoom_view: ZoomView,
    feed: Option<(FeedStamp, Feed)>,
    /// A post to bring to the top on the next layout (a sidebar pick, a follow).
    jump: Option<PostId>,
    /// A mouse drag in the feed: where it started and where it is, as (line, cell column).
    pub drag: Option<((usize, u16), (usize, u16))>,
    /// Sessions opened before: their posts, comments and place in the feed, so
    /// coming back shows them at once (then refreshes) instead of a blank pane.
    remembered: HashMap<SessionId, Remembered>,
    wheel: WheelAccel,
    /// The search being read: its query's chars (empty: none), lit in the feed and stepped by `n`/`N`.
    pub needle: Vec<char>,
    /// The post a search hit opened, until its first matching line is on screen.
    find_post: Option<PostId>,
}

struct Remembered {
    posts: Vec<PostSummary>,
    comments: Vec<Comment>,
    scroll: usize,
    cursor: usize,
    shown: Option<PostId>,
    follow: bool,
    feed: Option<(FeedStamp, Feed)>,
}

const WHEEL: usize = 3;

/// Wheel acceleration from tick cadence, hunk's tuning of OpenTUI's
/// `MacOSScrollAccel`: a lone notch moves one row, a fast spin ramps to `WHEEL`.
/// Fractions carry between ticks so the ramp is smooth.
#[derive(Default)]
struct WheelAccel {
    last: Option<Instant>,
    gaps: [f64; 3],
    n: usize,
    carry: f64,
}

impl WheelAccel {
    fn rows(&mut self, at: Instant, dir: f64) -> isize {
        let gap = self.last.map(|l| at.saturating_duration_since(l).as_secs_f64() * 1000.0);
        let mult = match gap {
            Some(ms) if ms < 6.0 => 1.0,
            Some(ms) if ms <= 150.0 => {
                self.last = Some(at);
                self.gaps[self.n % 3] = ms;
                self.n += 1;
                let avg = self.gaps[..self.n.min(3)].iter().sum::<f64>() / self.n.min(3) as f64;
                (1.0 + 0.4 * ((100.0 / avg / 4.0).exp() - 1.0)).min(WHEEL as f64)
            }
            _ => {
                *self = WheelAccel { last: Some(at), ..WheelAccel::default() };
                1.0
            }
        };
        // A reversal moves at once instead of first paying back the other way's fraction.
        if self.carry * dir < 0.0 { self.carry = 0.0; }
        self.carry += dir * mult;
        let rows = self.carry.trunc();
        self.carry -= rows;
        rows as isize
    }
}

impl App {
    pub fn new(want: Option<SessionId>) -> App {
        App {
            sessions: Vec::new(),
            open: None,
            posts: Vec::new(),
            comments: Vec::new(),
            unread: HashSet::new(),
            unseen: HashSet::new(),
            loaded: HashMap::new(),
            restyle: HashSet::new(),
            views: HashMap::new(),
            shown: None,
            sel: 0,
            mode: Mode::Nav,
            sidebar_hidden: false,
            session_picker: false,
            size: (80, 24),
            space: None,
            cell_px: (8, 16),
            kitty: false,
            toast: None,
            banner: None,
            want,
            follow: true,
            fresh: Vec::new(),
            seen_rows: HashSet::new(),
            builds: 0,
            visuals: HashMap::new(),
            feed_gen: 0,
            layouts: 0,
            on_screen: Vec::new(),
            scroll: 0,
            cursor: 0,
            focused_card: None,
            pending_html_focus: None,
            pending_sidebar: None,
            pending_sidebar_frames: HashMap::new(),
            before_focus: None,
            unzoomed: None,
            zoom_ahead: None,
            left_focus: None,
            zoom_view: ZoomView::default(),
            feed: None,
            jump: None,
            remembered: HashMap::new(),
            drag: None,
            wheel: WheelAccel::default(),
            needle: Vec::new(),
            find_post: None,
        }
    }

    /// The first requests: who's there. The watch is already open (main opens it first).
    pub fn start(&self) -> Vec<Effect> {
        vec![Effect::Request(StageRequest::SessionList)]
    }

    /// The visual key selected for full-width card focus, if any.
    pub fn focused_visual(&self) -> Option<VisualKey> {
        if let Some(key) = &self.pending_html_focus { return Some(key.clone()); }
        let (id, card) = self.focused_card.as_ref()?;
        Some((id.clone(), self.version_of(id)?, *card))
    }

    /// Pixel budget for visual cards at the current pane size.
    pub fn target(&self) -> Target {
        let width = if self.pending_html_focus.is_some() || self.pending_sidebar.as_ref().is_some_and(|(hidden, _)| *hidden) {
            self.size.0
        } else if self.pending_sidebar.is_some() {
            ui::geometry(self.size).main.width
        } else {
            ui::geometry_for(self).main.width
        };
        let inner = width.saturating_sub(5).max(1) as u32;
        let rows = if self.focused_card.is_some() || self.pending_html_focus.is_some() { self.page().saturating_sub(3) } else { self.page() / 2 };
        Target {
            width_px: inner * self.cell_px.0 as u32,
            max_height_px: rows.max(1) as u32 * self.cell_px.1 as u32,
            html_height_px: self.page().saturating_sub(3).max(1) as u32 * self.cell_px.1 as u32,
            cell_px: self.cell_px,
        }
    }

    /// Ask once for the pixels of the visual cards on screen and, once none of
    /// those is still rendering, of the nearest one within a page above and
    /// below, so off-screen diagrams never compete with visible ones for CPU.
    /// Runs after each draw (see `after_draw`), when the feed is laid out for what's shown.
    fn request_visuals(&mut self) -> Vec<Effect> {
        let page = self.page();
        self.feed();
        let (scroll, Some((_, feed))) = (self.scroll, &self.feed) else { return vec![] };
        let hits = &feed.hits;
        let end = (scroll + page).min(hits.len());
        // A card's lines are contiguous, so each (post, card) is looked up once per run.
        let visuals = |lines: &mut dyn Iterator<Item = usize>| -> Vec<VisualKey> {
            let mut last = None;
            lines
                .filter_map(|i| match hits[i] { (p, Hit::Card(c)) => Some((p, c)), _ => None })
                .filter(|card| last.replace(*card) != Some(*card))
                .filter_map(|card| self.visual_key(card))
                .collect()
        };
        let mut want = visuals(&mut (scroll..end));
        let rendering = want.iter().any(|k| matches!(self.visuals.get(k), None | Some(VisualState::Pending)));
        if !rendering {
            want.extend(visuals(&mut (end..(end + page).min(hits.len()))).into_iter().next());
            want.extend(visuals(&mut (scroll.saturating_sub(page)..scroll.min(hits.len())).rev()).into_iter().next());
        }
        let mut fx = Vec::new();
        for key in want {
            if self.visuals.contains_key(&key) {
                continue;
            }
            if let Some(source) = self.visual_source(&key) {
                fx.push(Effect::NeedVisual { key: key.clone(), source: source.clone() });
                self.visuals.insert(key, VisualState::Pending);
                self.feed_gen += 1;
            }
        }
        fx
    }

    /// Visual cards with a line on screen in the current feed: not the last drawn frame,
    /// which keys handled in the same batch may already have scrolled away from.
    fn visuals_in_view(&mut self) -> Vec<VisualKey> {
        let page = self.page();
        self.feed();
        let (scroll, Some((_, feed))) = (self.scroll, &self.feed) else { return vec![] };
        let end = (scroll + page).min(feed.hits.len());
        let mut last = None;
        feed.hits[scroll.min(end)..end]
            .iter()
            .filter_map(|(p, hit)| match hit { Hit::Card(c) => Some((*p, *c)), _ => None })
            .filter(|card| last.replace(*card) != Some(*card))
            .filter_map(|card| self.visual_key(card))
            .collect()
    }

    /// Post `p`'s card `c`, if it's a visual card.
    fn visual_key(&self, (p, c): (usize, usize)) -> Option<VisualKey> {
        let id = &self.posts.get(p)?.id;
        let key = (id.clone(), self.version_of(id)?, c);
        self.visual_source(&key).map(|_| key)
    }

    fn visual_source(&self, (id, version, c): &VisualKey) -> Option<&VisualSource> {
        match &self.loaded.get(&(id.clone(), *version))?.cards.as_ref()?.get(*c)?.body {
            CardBody::Visual { source, .. } => Some(source),
            _ => None,
        }
    }

    pub fn update(&mut self, msg: Msg) -> Vec<Effect> {
        match msg {
            Msg::Key(k) if k.kind != KeyEventKind::Release => {
                self.toast = None;
                let mut fx = self.key(k);
                fx.push(Effect::Redraw);
                fx
            }
            Msg::Key(_) => vec![],
            Msg::Mouse(m) => self.mouse(m, Instant::now()),
            Msg::Wheel(m, at) => self.mouse(m, at),
            Msg::Resize(w, h) => {
                self.size = (w, h);
                if w >= ui::SIDEBAR_MIN_WIDTH { self.session_picker = false; }
                let mut fx = vec![Effect::Redraw];
                fx.extend(self.report_space());
                fx
            }
            Msg::CellPx(px) => {
                if px == self.cell_px {
                    return vec![];
                }
                self.cell_px = px;
                let mut fx = vec![Effect::Redraw];
                fx.extend(self.report_space());
                fx
            }
            Msg::Event(e) => self.event(e),
            Msg::Reply(r) => self.reply(r),
            Msg::WatchLost => {
                self.banner = Some("reconnecting to notesd…".into());
                vec![Effect::Redraw]
            }
            Msg::WatchBack => {
                self.banner = None;
                let mut fx = vec![Effect::Request(StageRequest::SessionList)];
                if let Some(s) = self.open.clone() {
                    fx.push(Effect::Request(StageRequest::PostList(s.clone())));
                    fx.push(Effect::Request(StageRequest::CommentList(s)));
                }
                for (id, v) in std::mem::take(&mut self.restyle) {
                    if self.loaded.get(&(id.clone(), v)).is_some_and(|l| l.render.is_none()) {
                        fx.push(Effect::Request(StageRequest::PostRender(PostShowArgs { id, version: Some(v) })));
                    }
                }
                fx.push(Effect::Redraw);
                fx
            }
            Msg::Probed { cell_px, kitty } => {
                if let Some(px) = cell_px {
                    self.cell_px = px;
                }
                self.kitty = kitty;
                vec![Effect::Redraw]
            }
            Msg::VisualReady { key, w, h, max_scale } => {
                let target = self.target();
                if self.sidebar_waits_for(&key) {
                    if w != target.width_px { return vec![]; }
                    let state = VisualState::Ready { w, h, max_scale };
                    self.pending_sidebar_frames.insert(key.clone(), state);
                    let mut fx = self.sidebar_frame_done(&key);
                    fx.push(Effect::Redraw);
                    return fx;
                }
                let resized_html = self.pending_html_focus.as_ref() == Some(&key) && w == target.width_px;
                // Feed pixels that land while their card is zoomed are what leaving focus puts back.
                if let Some((_, state)) = self.unzoomed.as_mut().filter(|(unzoomed, _)| *unzoomed == key) {
                    *state = VisualState::Ready { w, h, max_scale };
                }
                self.visuals.insert(key, VisualState::Ready { w, h, max_scale });
                self.feed_gen += 1;
                let mut fx = if resized_html { self.enter_card_focus() } else { vec![] };
                fx.push(Effect::Redraw);
                fx
            }
            Msg::VisualFailed { key, reason } => {
                if self.sidebar_waits_for(&key) {
                    self.pending_sidebar_frames.insert(key.clone(), VisualState::Failed(reason));
                    let mut fx = self.sidebar_frame_done(&key);
                    fx.push(Effect::Redraw);
                    return fx;
                }
                let failed_html = self.pending_html_focus.as_ref() == Some(&key);
                self.visuals.insert(key, VisualState::Failed(reason));
                self.feed_gen += 1;
                let mut fx = if failed_html { self.enter_card_focus() } else { vec![] };
                fx.push(Effect::Redraw);
                fx
            }
            Msg::ZoomShown { key, w, h } => {
                if self.focused_visual().as_ref() != Some(&key) {
                    // Focus closed but the gallery hasn't drawn since: it shows these now.
                    if let Some((_, shown)) = self.left_focus.as_mut().filter(|(left, _)| *left == key) {
                        *shown = VisualState::Ready { w, h, max_scale: 1.0 };
                    }
                    return vec![];
                }
                self.visuals.insert(key, VisualState::Ready { w, h, max_scale: 1.0 });
                self.feed_gen += 1;
                vec![Effect::Redraw]
            }
            Msg::ZoomReady { key, w, h } => {
                if let Some(ahead) = self.zoom_ahead.as_mut().filter(|ahead| ahead.key == key) {
                    ahead.size = Some((w, h));
                }
                vec![]
            }
            Msg::Gfx(_) => vec![],
        }
    }

    // ---- Sidebar ----------------------------------------------------------------

    fn open_session(&mut self, id: SessionId) -> Vec<Effect> {
        self.session_picker = false;
        self.unread.remove(&id);
        if self.open.as_ref() == Some(&id) {
            return vec![];
        }
        if let Some(old) = self.open.take() {
            let r = Remembered {
                posts: std::mem::take(&mut self.posts),
                comments: std::mem::take(&mut self.comments),
                scroll: self.scroll,
                cursor: self.cursor,
                shown: self.shown.take(),
                follow: self.follow,
                feed: self.feed.take(),
            };
            self.remembered.insert(old, r);
        }
        self.leave_focus();
        self.pending_html_focus = None;
        self.pending_sidebar = None;
        self.pending_sidebar_frames.clear();
        self.before_focus = None;
        self.open = Some(id.clone());
        self.feed = None;
        self.jump = None;
        match self.remembered.remove(&id) {
            Some(r) => {
                (self.posts, self.comments, self.scroll, self.cursor, self.shown, self.follow, self.feed) =
                    (r.posts, r.comments, r.scroll, r.cursor, r.shown, r.follow, r.feed);
                self.feed_gen += 1;
            }
            None => {
                (self.scroll, self.cursor, self.shown, self.follow) = (0, 0, None, true);
            }
        }
        self.banner = None;
        if let Some(i) = self.sessions.iter().position(|s| s.id == id) {
            self.sel = i;
        }
        vec![
            Effect::Request(StageRequest::PostList(id.clone())),
            Effect::Request(StageRequest::CommentList(id)),
        ]
    }

    fn select_session(&mut self, i: usize, activate: bool) -> Vec<Effect> {
        let Some(id) = self.sessions.get(i).map(|s| s.id.clone()) else { return vec![] };
        self.sel = i;
        if activate {
            self.open_session(id)
        } else {
            vec![]
        }
    }

    /// `j`/`k`: bring the next post's title (or this one's, then the previous) to the top.
    fn step_post(&mut self, forward: bool) {
        let cursor = self.cursor;
        let feed = self.feed();
        let Some(&(at, _)) = feed.hits.get(cursor) else { return };
        let target = match (forward, cursor > feed.starts[at]) {
            (true, _) => at + 1,
            (false, true) => at,
            (false, false) => at.saturating_sub(1),
        };
        let Some(&line) = feed.starts.get(target) else { return };
        self.goto(line);
    }

    /// Put `line` at the top of the viewport (as far as the feed's end allows), cursor on it.
    fn goto(&mut self, line: usize) {
        let page = self.page();
        let max = self.feed().lines.len().saturating_sub(page);
        (self.cursor, self.scroll) = (line, line.min(max));
        self.follow_cursor();
    }

    /// `]`/`[` (hunks, else cards) and `.`/`,` (cards): the next or previous stop.
    fn step_stop(&mut self, hunks: bool, forward: bool) {
        let cursor = self.cursor;
        let feed = self.feed();
        let pick = |stops: &[usize]| {
            if forward {
                stops.get(stops.partition_point(|&l| l <= cursor))
            } else {
                stops[..stops.partition_point(|&l| l < cursor)].last()
            }
            .copied()
        };
        let stop = if hunks { pick(&feed.hunks).or_else(|| pick(&feed.cards)) } else { pick(&feed.cards) };
        if let Some(line) = stop {
            self.goto(line);
        }
    }

    /// `n`/`N`: the next or previous line matching the search, wrapping round the feed.
    fn step_match(&mut self, forward: bool) {
        let needle = self.needle.clone();
        let found = crate::search::lines(&self.feed().lines, &needle);
        let cursor = self.cursor;
        let at = if forward { found.iter().position(|&l| l > cursor).unwrap_or(0) } else { found.iter().rposition(|&l| l < cursor).unwrap_or(found.len().saturating_sub(1)) };
        match found.get(at) {
            Some(&line) => {
                self.goto(line);
                self.toast = Some(format!("match {}/{}", at + 1, found.len()));
            }
            None => self.toast = Some("no matches".into()),
        }
    }

    /// Bring the first line of the search hit's post that matches the query to the top.
    fn land_find(&mut self) {
        let (Some(id), Some((_, feed))) = (self.find_post.clone(), &self.feed) else { return };
        let Some(i) = self.posts.iter().position(|p| p.id == id) else { return };
        if !self.loaded.get(&(id, self.listed_version(&self.posts[i]))).is_some_and(|l| l.cards.is_some()) {
            return;
        }
        let end = feed.starts.get(i + 1).copied().unwrap_or(feed.lines.len());
        let found = crate::search::lines(&feed.lines, &self.needle);
        let first = found.iter().position(|&l| l >= feed.starts[i] && l < end);
        let line = first.map_or(feed.starts[i], |n| found[n]);
        let toast = first.map(|n| format!("match {}/{}", n + 1, found.len()));
        self.find_post = None;
        self.goto(line);
        self.toast = toast;
    }

    // ---- Posts ------------------------------------------------------------------

    fn version_of(&self, id: &PostId) -> Option<u64> {
        self.posts.iter().find(|p| &p.id == id).map(|p| self.listed_version(p))
    }

    /// The post owning the top visible feed row, and the diff file that row is in.
    pub fn top_row(&mut self) -> Option<(usize, Option<String>)> {
        let scroll = self.scroll;
        let feed = self.feed();
        let (post, _) = *feed.hits.get(scroll)?;
        let Origin { card, src, .. } = feed.origins[scroll];
        let p = self.posts.get(post)?;
        let cards = self.loaded.get(&(p.id.clone(), self.listed_version(p))).and_then(|l| l.cards.as_ref());
        let file = cards.and_then(|cards| match &cards.get(card)?.body {
            CardBody::Diff(diff) => diff.files.iter().rev().find(|(row, _)| diff.src.get(*row).is_some_and(|&s| s < src)).map(|(_, f)| f.clone()),
            _ => None,
        });
        Some((post, file))
    }

    /// The version shown for a listed post: the pinned one, else its latest, or while
    /// the latest has no cards yet the newest one that has, so an update never drops
    /// the post to "loading…" under the reader.
    pub fn listed_version(&self, p: &PostSummary) -> u64 {
        self.views.get(&p.id).and_then(|v| v.pinned.or(v.built.filter(|b| *b < p.version))).unwrap_or(p.version)
    }

    /// The version a listed post should have loaded: the pinned one, else its latest.
    fn wanted_version(&self, p: &PostSummary) -> u64 {
        self.views.get(&p.id).and_then(|v| v.pinned).unwrap_or(p.version)
    }

    fn show(&mut self, id: PostId) -> Vec<Effect> {
        self.unseen.remove(&id);
        self.shown = Some(id.clone());
        self.views.entry(id.clone()).or_default();
        match self.posts.iter().find(|p| p.id == id).map(|p| self.wanted_version(p)) {
            Some(v) => self.load(id, v),
            None => vec![],
        }
    }

    /// Request a version-pinned show + render unless it's loaded or in flight.
    fn load(&mut self, id: PostId, version: u64) -> Vec<Effect> {
        if self.loaded.contains_key(&(id.clone(), version)) {
            return vec![];
        }
        self.loaded.insert((id.clone(), version), Loaded { post: None, render: None, cards: None });
        self.feed_gen += 1;
        let args = PostShowArgs { id, version: Some(version) };
        vec![
            Effect::Request(StageRequest::PostShow(args.clone())),
            Effect::Request(StageRequest::PostRender(args)),
        ]
    }

    pub fn shown_version(&self) -> Option<u64> {
        self.version_of(self.shown.as_ref()?)
    }

    pub fn shown_post(&self) -> Option<&Post> {
        let id = self.shown.clone()?;
        self.loaded.get(&(id, self.shown_version()?))?.post.as_ref()
    }

    pub fn shown_cards(&self) -> Option<&[Card]> {
        let id = self.shown.clone()?;
        self.loaded.get(&(id, self.shown_version()?))?.cards.as_deref()
    }

    fn comments_on(&self, id: &PostId) -> Vec<Comment> {
        self.comments.iter().filter(|c| c.post.as_ref() == Some(id)).cloned().collect()
    }

    /// Cards from what has arrived: plain text from the post alone, so it reads at
    /// once, then styled when its render lands (`Reply::Render` drops the plain ones).
    fn built(&mut self, key: (PostId, u64)) {
        if let Some(l) = self.loaded.get_mut(&key) {
            if let (Some(post), None) = (&l.post, &l.cards) {
                l.cards = Some(build_cards(post, l.render.as_ref()));
                self.builds += 1;
            }
            if l.cards.is_some() {
                let view = self.views.entry(key.0.clone()).or_default();
                view.built = view.built.max(Some(key.1));
            }
        }
        // A new version without the focused part: nothing left to focus on.
        let gone = self.focused_card.as_ref().is_some_and(|(id, card)| {
            self.version_of(id) == Some(key.1) && &key.0 == id
                && self.loaded.get(&key).and_then(|l| l.cards.as_ref()).is_some_and(|cards| *card >= cards.len())
        });
        if gone {
            self.leave_focus();
        }
    }

    /// One post laid out at `width`, memoized in its view on everything it reads:
    /// its own comments and visual sizes, so another post's comments or pixels never redo it.
    /// Returns the layout with its number, which changes only when it is redone.
    fn post_laid(&mut self, id: &PostId, version: u64, width: u16, page: u16) -> Option<(u64, &Laid)> {
        let loaded = self.loaded.get(&(id.clone(), version))?;
        let cards = loaded.cards.as_ref()?;
        let styled = loaded.render.is_some();
        let (cell, visuals) = (self.cell_px, &self.visuals);
        let selected = self.focused_card.as_ref().filter(|(post, _)| post == id).map(|(_, card)| *card);
        let comments = if selected.is_some() { vec![] } else { self.comments_on(id) };
        let inner = width.saturating_sub(4).max(1);
        let rows = if selected.is_some() { page.saturating_sub(3) } else { page / 2 };
        // A new version holds the size its card's last pixels had while it renders,
        // so a patch never collapses the card to a placeholder and shifts the feed.
        let last_ready = |c: usize| {
            (1..version).rev().find_map(|v| visuals.get(&(id.clone(), v, c)).filter(|s| matches!(s, VisualState::Ready { .. })))
        };
        let fits: Vec<Fit> = cards
            .iter()
            .enumerate()
            .map(|(c, card)| match visuals.get(&(id.clone(), version, c)).filter(|s| **s != VisualState::Pending).or_else(|| last_ready(c)) {
                Some(VisualState::Ready { w, h, max_scale }) => {
                    // Scroll through tall Mermaid diagrams instead of shrinking their labels;
                    // a focused one shows its pixels 1:1 until the full-width raster lands.
                    let rows = match &card.body {
                        CardBody::Visual { source: VisualSource::Mermaid(_), .. } => u16::MAX,
                        CardBody::Visual { source: VisualSource::Html(_), .. } => page.saturating_sub(3),
                        _ => rows,
                    };
                    fit_cells((*w, *h), *max_scale, inner, rows, cell)
                },
                Some(VisualState::Failed(why)) => Fit::Failed(why.clone()),
                // Hold about the room a diagram or page usually takes.
                Some(VisualState::Pending) | None => Fit::Pending { rows: (page / 3).clamp(3, 12) },
            })
            .collect();
        let view = self.views.entry(id.clone()).or_default();
        let key = (version, width, view.fold_gen, selected, styled, comments, fits);
        if view.laid.as_ref().is_none_or(|(k, ..)| *k != key) {
            let laid = cards::layout(cards, &key.5, width, &view.folds, &|c| key.6[c].clone(), selected);
            self.layouts += 1;
            view.laid = Some((key, self.layouts, laid));
        }
        view.laid.as_ref().map(|(_, n, l)| (*n, l))
    }

    /// The feed for the main column, rebuilt only when something it reads changed.
    /// A rebuild (a post loading above, a resize rewrapping everything, a fold)
    /// keeps the source line at the top of the viewport (and the cursor's) in place.
    pub fn feed(&mut self) -> &Feed {
        let width = ui::geometry_for(self).main.width.saturating_sub(1);
        let page = self.page() as u16;
        let stamp: FeedStamp = (self.feed_gen, width, page, self.cell_px);
        #[cfg(debug_assertions)]
        if let Some((s, f)) = self.feed.as_ref().filter(|(s, _)| *s == stamp) {
            assert!(f.inputs == self.feed_inputs(), "the feed's inputs changed without bumping feed_gen (stamp {s:?})");
        }
        if self.feed.as_ref().is_none_or(|(s, _)| *s != stamp) {
            let anchor = self.feed.as_ref().and_then(|(_, f)| {
                Some(Anchor {
                    top: f.spot(&self.posts, self.scroll),
                    cursor: f.spot(&self.posts, self.cursor)?,
                    cursor_row: self.cursor.saturating_sub(self.scroll),
                })
            });
            // A post whose layout hasn't changed keeps its lines, moved out of the old
            // feed rather than copied from the layout again: a post loading, a comment
            // or a diagram's pixels costs that post's lines, not the whole session's.
            let mut old = self.feed.take().map(|(_, f)| f).unwrap_or_default();
            let mut feed = Feed::default();
            let posts: Vec<(PostId, u64)> = self.posts.iter().map(|p| (p.id.clone(), self.listed_version(p))).collect();
            for (i, (id, version)) in posts.iter().enumerate() {
                feed.starts.push(feed.lines.len());
                if self.focused_card.as_ref().is_some_and(|(post, _)| post != id) {
                    feed.laid.push(None);
                    continue;
                }
                feed.lines.push(self.header(i));
                feed.hits.push((i, Hit::None));
                feed.origins.push(Origin { card: usize::MAX, src: 0, wrap: 0 });
                let start = feed.lines.len();
                match self.post_laid(id, *version, width, page) {
                    Some((n, laid)) => {
                        match old.laid.get(i) {
                            Some(Some((was, m))) if was == id && *m == n => {
                                let from = old.starts[i] + 1;
                                feed.lines.extend(old.lines[from..from + laid.lines.len()].iter_mut().map(std::mem::take));
                            }
                            _ => feed.lines.extend(laid.lines.iter().cloned()),
                        }
                        feed.hits.extend(laid.hits.iter().map(|h| (i, *h)));
                        feed.cards.extend(laid.cards.iter().map(|l| l + start));
                        feed.hunks.extend(laid.hunks.iter().map(|l| l + start));
                        feed.origins.extend(laid.origins.iter().copied());
                        feed.slots.extend(laid.slots.iter().map(|s| {
                            ((id.clone(), *version, s.card), Slot { line: s.line + start, ..*s })
                        }));
                        feed.laid.push(Some((id.clone(), n)));
                    }
                    None => {
                        feed.lines.push(Line::styled("  loading…", ui::dim()));
                        feed.lines.push(Line::default());
                        feed.hits.extend([(i, Hit::None); 2]);
                        feed.origins.extend([1, 2].map(|src| Origin { card: usize::MAX, src, wrap: 0 }));
                        feed.laid.push(None);
                    }
                }
            }
            #[cfg(debug_assertions)]
            {
                feed.inputs = self.feed_inputs();
            }
            let total = feed.lines.len();
            if let Some(id) = self.jump.take() {
                if let Some(i) = self.posts.iter().position(|p| p.id == id) {
                    self.cursor = feed.starts[i];
                    self.scroll = self.cursor;
                }
            } else if let Some(anchor) = anchor {
                let find = |spot: &Spot| {
                    let i = self.posts.iter().position(|p| p.id == spot.post)?;
                    let end = feed.starts.get(i + 1).copied().unwrap_or(total);
                    Some(feed.locate(i, spot).unwrap_or((feed.starts[i] + spot.offset).min(end.saturating_sub(1))))
                };
                if let Some(cursor) = find(&anchor.cursor) {
                    self.cursor = cursor;
                    self.scroll = anchor.top.as_ref().and_then(|t| {
                        let i = self.posts.iter().position(|p| p.id == t.post)?;
                        feed.locate(i, t)
                    }).unwrap_or(cursor.saturating_sub(anchor.cursor_row));
                }
            }
            self.scroll = self.scroll.min(total.saturating_sub(page as usize));
            self.cursor = self.cursor.min(total.saturating_sub(1)).clamp(self.scroll, self.scroll + page as usize - 1);
            self.feed = Some((stamp, feed));
        }
        if self.find_post.is_some() {
            self.land_find();
        }
        &self.feed.as_ref().expect("built above").1
    }

    #[cfg(debug_assertions)]
    fn feed_inputs(&self) -> FeedInputs {
        let posts = self
            .posts
            .iter()
            .map(|p| {
                let v = self.listed_version(p);
                let fold_gen = self.views.get(&p.id).map_or(0, |v| v.fold_gen);
                let (built, styled) = self.loaded.get(&(p.id.clone(), v)).map_or((false, false), |l| (l.cards.is_some(), l.render.is_some()));
                (p.id.clone(), v, fold_gen, built, styled)
            })
            .collect();
        (posts, self.comments.clone(), self.visuals.clone(), self.focused_card.clone())
    }

    /// A post's title row in the feed: bold title, dim version and time.
    fn header(&self, i: usize) -> Line<'static> {
        let p = &self.posts[i];
        let title = crate::sanitize::sanitize(&p.title).into_owned();
        Line::from(vec![
            Span::styled(format!(" {title}"), ui::bold().fg(ui::TEXT)),
            Span::styled(format!("  v{} · {}", p.version, ui::when(p.updated_at)), ui::dim()),
        ])
    }

    fn page(&self) -> usize {
        ui::geometry_for(self).main.height.saturating_sub(1).max(1) as usize
    }

    /// The shown post follows the cursor through the feed, and new posts are
    /// followed only while the cursor is in the last one.
    fn follow_cursor(&mut self) {
        let cursor = self.cursor;
        let Some(post) = self.feed().hits.get(cursor).map(|(p, _)| *p) else { return };
        if let Some(id) = self.posts.get(post).map(|p| p.id.clone()) {
            self.follow = post + 1 == self.posts.len();
            self.unseen.remove(&id);
            self.shown = Some(id);
        }
    }

    /// Move the cursor by `delta` lines, keeping it on screen.
    fn cursor(&mut self, delta: isize) {
        let total = self.feed().lines.len();
        let page = self.page();
        self.cursor = (self.cursor as isize + delta).clamp(0, total.saturating_sub(1) as isize) as usize;
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + page {
            self.scroll = self.cursor + 1 - page;
        }
        self.follow_cursor();
    }

    fn wheel_rows(&mut self, down: bool, at: Instant) -> isize {
        let dir = if down { 1 } else { -1 };
        // Touch clients already emit a stream of wheel ticks during a swipe.
        // Accelerating them skips a fifth of a short phone viewport.
        if self.size.0 < ui::SIDEBAR_MIN_WIDTH || self.page() < 20 { dir } else { self.wheel.rows(at, dir as f64) }
    }

    /// Scroll the feed by `delta` lines, dragging the cursor along.
    fn scroll(&mut self, delta: isize) {
        let total = self.feed().lines.len();
        let page = self.page();
        let max = total.saturating_sub(page);
        self.scroll = (self.scroll as isize + delta).clamp(0, max as isize) as usize;
        self.cursor = self.cursor.clamp(self.scroll, (self.scroll + page).saturating_sub(1).max(self.scroll));
        self.follow_cursor();
        // Reading earlier lines of the *last* post must not let a new post yank
        // the viewport away. Resume following only at the bottom of the feed.
        self.follow = self.scroll == max;
    }

    pub fn hit_at_cursor(&mut self) -> (Option<usize>, Hit) {
        let cursor = self.cursor;
        self.feed().hits.get(cursor).map_or((None, Hit::None), |(p, h)| (Some(*p), *h))
    }

    fn toggle(&mut self, (post, hit): (Option<usize>, Hit)) -> bool {
        let (Some(post), Hit::Node(card, node)) = (post, hit) else { return false };
        let Some(id) = self.posts.get(post).map(|p| p.id.clone()) else { return false };
        let v = self.views.entry(id).or_default();
        let set = v.folds.entry(card).or_default();
        if !set.remove(&node) {
            set.insert(node);
        }
        v.fold_gen += 1;
        self.feed_gen += 1;
        true
    }

    /// The part under the cursor (else the shown post's first part).
    pub fn part_at_cursor(&mut self) -> Option<Part> {
        let card = match self.hit_at_cursor().1 {
            Hit::Card(c) | Hit::Node(c, _) => c,
            Hit::None => 0,
        };
        self.shown_post()?.parts.get(card).cloned()
    }

    pub fn rerender(&mut self) -> Vec<Effect> {
        let (Some(id), Some(v)) = (self.shown.clone(), self.shown_version()) else {
            return vec![];
        };
        self.loaded.remove(&(id.clone(), v));
        self.load(id, v)
    }

    /// `v`: step back one version, wrapping to the latest.
    fn cycle_version(&mut self) -> Vec<Effect> {
        let Some(id) = self.shown.clone() else { return vec![] };
        let Some(latest) = self.posts.iter().find(|p| p.id == id).map(|p| p.version) else {
            return vec![];
        };
        let view = self.views.entry(id.clone()).or_default();
        let current = view.pinned.unwrap_or(latest);
        view.pinned = (current > 1).then(|| current - 1);
        self.feed_gen += 1;
        self.toast = Some(format!("v{} of {latest}", view.pinned.unwrap_or(latest)));
        let v = view.pinned.unwrap_or(latest);
        self.load(id, v)
    }

    // ---- Input ------------------------------------------------------------------

    /// Enter card focus, preserving the feed position for when it closes.
    fn enter_card_focus(&mut self) -> Vec<Effect> {
        let Some((id, _, card)) = self.pending_html_focus.take() else { return vec![] };
        // Following would jump the focused view to each new post.
        self.before_focus = Some((self.scroll, self.cursor, self.follow));
        self.follow = false;
        let key = self.version_of(&id).map(|version| (id.clone(), version, card));
        self.unzoomed = key.clone().filter(|key| matches!(self.visual_source(key), Some(VisualSource::Mermaid(_))))
            .and_then(|key| Some((key.clone(), self.visuals.get(&key)?.clone())));
        // Back before a draw: the gallery still shows what focus showed.
        if let Some((key, shown)) = self.left_focus.take().filter(|(left, _)| Some(left) == key.as_ref()) {
            self.visuals.insert(key, shown);
            self.feed_gen += 1;
        }
        // Its zoomed raster was made ahead: lay it out at that size from the first frame.
        let target = self.zoom_target();
        if let Some(ZoomAhead { key, size: Some((w, h)), .. }) = self.zoom_ahead.take().filter(|ahead| Some(&ahead.key) == key.as_ref() && ahead.target == target) {
            self.visuals.insert(key, VisualState::Ready { w, h, max_scale: 1.0 });
            self.feed_gen += 1;
        }
        self.focused_card = Some((id, card));
        self.feed = None;
        self.scroll = 0;
        self.cursor = 0;
        self.follow_cursor();
        self.report_space()
    }

    fn toggle_sidebar(&mut self) -> Vec<Effect> {
        if self.size.0 < ui::SIDEBAR_MIN_WIDTH {
            self.session_picker = !self.session_picker;
            self.drag = None;
            return vec![];
        }
        if self.pending_sidebar.take().is_some() {
            self.pending_sidebar_frames.clear();
            return vec![];
        }
        let hidden = !self.sidebar_hidden;
        let visible_html: HashSet<_> = self.visuals_in_view().into_iter().filter(|key| {
            matches!(self.visual_source(key), Some(VisualSource::Html(_)))
                && matches!(self.visuals.get(key), Some(VisualState::Ready { .. }))
        }).collect();
        if !visible_html.is_empty() && self.focused_card.is_none()
            && ui::geometry(self.size).sidebar.width > 0 {
            self.pending_sidebar = Some((hidden, visible_html));
            return vec![];
        }
        self.sidebar_hidden = hidden;
        self.report_space()
    }

    /// After each draw: a sidebar change still waiting on an HTML card that has since
    /// left the screen happens now (that card re-renders at the new width later), then
    /// the visuals this frame shows are asked for.
    pub fn after_draw(&mut self) -> Vec<Effect> {
        self.left_focus = None;
        let mut fx = Vec::new();
        let left = self.pending_sidebar.as_ref().is_some_and(|(_, waiting)| {
            waiting.iter().any(|key| !self.on_screen.iter().any(|shown| &shown.key == key))
        });
        if let Some((hidden, _)) = self.pending_sidebar.take_if(|_| left) {
            self.sidebar_hidden = hidden;
            self.visuals.extend(self.pending_sidebar_frames.drain());
            self.feed_gen += 1;
            fx.extend(self.report_space());
            fx.push(Effect::Redraw);
        }
        if ui::session_area(self).is_none() {
            fx.extend(self.request_visuals());
            fx.extend(self.zoom_ahead());
        }
        fx
    }

    /// Once nothing on screen is still rendering, the Mermaid card under the cursor gets
    /// its zoomed raster made ahead, so `z` shows it at full resolution in its first frame.
    fn zoom_ahead(&mut self) -> Option<Effect> {
        if self.focused_card.is_some() || self.pending_html_focus.is_some() {
            return None;
        }
        if self.on_screen.iter().any(|o| matches!(self.visuals.get(&o.key), None | Some(VisualState::Pending))) {
            return None;
        }
        let key = self.card_at_cursor()?;
        let ready = matches!(self.visual_source(&key), Some(VisualSource::Mermaid(_)))
            && matches!(self.visuals.get(&key), Some(VisualState::Ready { .. }));
        let target = self.zoom_target();
        if !ready || self.zoom_ahead.as_ref().is_some_and(|ahead| ahead.key == key && ahead.target == target) {
            return None;
        }
        self.zoom_ahead = Some(ZoomAhead { key: key.clone(), target, size: None });
        Some(Effect::PrefetchZoom { key, target })
    }

    /// `target()` as it will be once a card is focused (no sidebar, nearly the whole pane).
    fn zoom_target(&self) -> Target {
        let inner = self.size.0.saturating_sub(5).max(1) as u32;
        Target {
            width_px: inner * self.cell_px.0 as u32,
            max_height_px: self.page().saturating_sub(3).max(1) as u32 * self.cell_px.1 as u32,
            html_height_px: self.page().saturating_sub(3).max(1) as u32 * self.cell_px.1 as u32,
            cell_px: self.cell_px,
        }
    }

    /// The card `z` would focus: the one under the cursor, or a post's first on its title.
    fn card_at_cursor(&mut self) -> Option<VisualKey> {
        let (post, hit) = self.hit_at_cursor();
        let id = self.posts.get(post?)?.id.clone();
        let card = match hit { Hit::Card(c) | Hit::Node(c, _) => c, Hit::None => 0 };
        let version = self.version_of(&id)?;
        Some((id, version, card))
    }

    fn sidebar_waits_for(&self, key: &VisualKey) -> bool {
        self.pending_sidebar.as_ref().is_some_and(|(_, waiting)| waiting.contains(key))
    }

    fn sidebar_frame_done(&mut self, key: &VisualKey) -> Vec<Effect> {
        let Some((hidden, waiting)) = &mut self.pending_sidebar else { return vec![]; };
        waiting.remove(key);
        if !waiting.is_empty() { return vec![]; }
        self.sidebar_hidden = *hidden;
        self.pending_sidebar = None;
        self.visuals.extend(self.pending_sidebar_frames.drain());
        self.feed_gen += 1;
        self.report_space()
    }

    /// A card's text width (the feed column less its border and padding), sent to notesd on change.
    fn report_space(&mut self) -> Vec<Effect> {
        let (cols, rows) = (ui::geometry_for(self).main.width.saturating_sub(5), self.page() as u16);
        let css = |cells: u16, px: u16| (cells as f64 * px as f64 / crate::gallery::content_scale(self.cell_px)) as u32;
        let space = StageSpace {
            cols,
            rows,
            html_width: css(cols.max(1), self.cell_px.0),
            html_height: css(rows.saturating_sub(3).max(1), self.cell_px.1),
        };
        if self.space == Some(space) {
            return vec![];
        }
        self.space = Some(space);
        vec![Effect::Request(StageRequest::Space(space))]
    }

    /// `z` focuses the card at the cursor; `z` or Escape restores the feed.
    fn toggle_card_focus(&mut self) -> Vec<Effect> {
        self.pending_sidebar = None;
        self.pending_sidebar_frames.clear();
        if self.pending_html_focus.take().is_some() { return vec![]; }
        if self.focused_card.is_some() {
            self.leave_focus();
            return self.report_space();
        }
        let Some(key) = self.card_at_cursor() else { return vec![] };
        let Some(selected) = self.loaded.get(&(key.0.clone(), key.1)).and_then(|loaded| loaded.cards.as_ref()?.get(key.2)) else { return vec![] };
        let html_ready = matches!(selected.body, CardBody::Visual { source: VisualSource::Html(_), .. })
            && matches!(self.visuals.get(&key), Some(VisualState::Ready { .. }))
            && self.visuals_in_view().contains(&key)
            && ui::geometry_for(self).main.width != self.size.0;
        self.pending_html_focus = Some(key);
        if html_ready { vec![] } else { self.enter_card_focus() }
    }

    /// Zoom (`+`/`-`/`0`) and pan (arrows, hjkl) a focused diagram; `None` for any other
    /// card or key. Panning moves a quarter of what's in view, never past the edges.
    fn zoom_key(&mut self, code: KeyCode) -> Option<Vec<Effect>> {
        let key = self.focused_visual()?;
        if !matches!(self.visual_source(&key), Some(VisualSource::Mermaid(_))) {
            return None;
        }
        let v = &mut self.zoom_view;
        let half = |level: u8| (500.0 / 1.5f32.powi(level as i32)) as u16;
        let step = half(v.level) / 2;
        match code {
            KeyCode::Char('+' | '=') => v.level = (v.level + 1).min(MAX_ZOOM_LEVEL),
            KeyCode::Char('-') => v.level = v.level.saturating_sub(1),
            KeyCode::Char('0') => *v = ZoomView::default(),
            KeyCode::Left | KeyCode::Char('h') => v.cx = v.cx.saturating_sub(step),
            KeyCode::Right | KeyCode::Char('l') => v.cx += step,
            KeyCode::Up | KeyCode::Char('k') => v.cy = v.cy.saturating_sub(step),
            KeyCode::Down | KeyCode::Char('j') => v.cy += step,
            _ => return None,
        }
        let h = half(v.level);
        (v.cx, v.cy) = (v.cx.clamp(h, 1000 - h), v.cy.clamp(h, 1000 - h));
        Some(vec![])
    }

    /// Back to the feed where it was, following again if it was.
    fn leave_focus(&mut self) {
        self.zoom_view = ZoomView::default();
        self.focused_card = None;
        self.feed = None;
        if let Some((key, state)) = self.unzoomed.take() {
            let shown = self.visuals.insert(key.clone(), state);
            self.left_focus = shown.map(|shown| (key, shown));
            self.feed_gen += 1;
        }
        self.zoom_ahead = None;
        if let Some((scroll, cursor, follow)) = self.before_focus.take() {
            (self.scroll, self.cursor, self.follow) = (scroll, cursor, follow);
        }
    }

    fn key(&mut self, k: KeyEvent) -> Vec<Effect> {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            return vec![Effect::Quit];
        }
        match std::mem::replace(&mut self.mode, Mode::Nav) {
            Mode::Nav => self.nav_key(k),
            Mode::Search { query, hits, sel } => self.search_key(k, query, hits, sel),
            Mode::Comment { input } => self.comment_key(k, input),
            Mode::Command { input } => self.command_key(k, input),
            Mode::Rename { id, input } => self.rename_key(k, id, input),
            Mode::Delete { what, request } => match k.code {
                KeyCode::Char('y') => {
                    self.toast = Some(match &request {
                        StageRequest::SessionDelete(id) => format!("deleted {what}; restore: notes session restore {id}"),
                        _ => format!("deleted {what}"),
                    });
                    vec![Effect::Request(request)]
                }
                _ => vec![],
            },
        }
    }

    /// `r`: retitle the selected session.
    fn start_rename(&mut self) {
        if let Some(s) = self.sessions.get(self.sel) {
            self.mode = Mode::Rename { id: s.id.clone(), input: s.title.clone().unwrap_or_default() };
        }
    }

    /// `D`: delete the selected session; `d`: the card under the cursor, else the
    /// shown post (a post's only card deletes the post). `y` confirms.
    fn start_delete(&mut self, session: bool) {
        let clean = |t: &str| crate::sanitize::sanitize(t).into_owned();
        let card = match self.hit_at_cursor() {
            (Some(_), Hit::Card(c) | Hit::Node(c, _)) => Some(c),
            _ => None,
        };
        let doomed = match session.then(|| self.sessions.get(self.sel)).flatten() {
            Some(s) => Some({
                (format!("session “{}”", clean(s.title.as_deref().unwrap_or(&s.agent))), StageRequest::SessionDelete(s.id.clone()))
            }),
            None if session => return,
            None => {
                let Some(p) = self.shown.as_ref().and_then(|id| self.posts.iter().find(|p| &p.id == id)).cloned() else { return };
                let cards = self.shown_cards().map_or(0, |c| c.len());
                match card.filter(|_| cards > 1) {
                    // Card indexes are the shown version's; post-part edits the latest.
                    Some(_) if self.shown_version() != Some(p.version) => {
                        self.toast = Some("cards delete from the latest version (v cycles back to it)".into());
                        return;
                    }
                    Some(c) => {
                        let label = self.shown_cards().and_then(|cs| cs.get(c)).map_or(String::new(), |c| c.label.clone());
                        Some((format!("card “{label}” from “{}”", clean(&p.title)), StageRequest::PartDelete(p.id.clone(), c)))
                    }
                    None => Some((format!("post “{}”", clean(&p.title)), StageRequest::PostDelete(p.id.clone()))),
                }
            }
        };
        if let Some((what, request)) = doomed {
            self.mode = Mode::Delete { what, request };
        }
    }

    fn nav_key(&mut self, k: KeyEvent) -> Vec<Effect> {
        let page = self.page().saturating_sub(1).max(1) as isize;
        if self.session_picker {
            return match k.code {
                KeyCode::Esc | KeyCode::Char('s') => { self.session_picker = false; vec![] }
                KeyCode::Down | KeyCode::Char('j' | 'J') => self.select_session(self.sel + 1, false),
                KeyCode::Up | KeyCode::Char('k' | 'K') => self.select_session(self.sel.saturating_sub(1), false),
                KeyCode::Enter => self.select_session(self.sel, true),
                KeyCode::Char('/') => {
                    self.session_picker = false;
                    self.mode = Mode::Search { query: String::new(), hits: Vec::new(), sel: 0 };
                    vec![]
                }
                KeyCode::Char('q') => vec![Effect::Quit],
                _ => vec![],
            };
        }
        if let Some(fx) = self.zoom_key(k.code) {
            return fx;
        }
        self.find_post = None;
        if k.modifiers.contains(KeyModifiers::CONTROL) {
            if let KeyCode::Char(c @ ('d' | 'u')) = k.code {
                let half = (self.page() / 2).max(1) as isize;
                self.scroll(if c == 'd' { half } else { -half });
                return vec![];
            }
        }
        match k.code {
            KeyCode::Char('q') => vec![Effect::Quit],
            KeyCode::Char('z') => self.toggle_card_focus(),
            KeyCode::Char('s') => self.toggle_sidebar(),
            KeyCode::Esc if self.focused_card.is_some() || self.pending_html_focus.is_some() => self.toggle_card_focus(),
            KeyCode::Char('j') if self.focused_card.is_none() => {
                self.step_post(true);
                vec![]
            }
            KeyCode::Char('k') if self.focused_card.is_none() => {
                self.step_post(false);
                vec![]
            }
            KeyCode::Char(c @ ('[' | ']' | ',' | '.')) => {
                self.step_stop(matches!(c, '[' | ']'), matches!(c, ']' | '.'));
                vec![]
            }
            KeyCode::Char(c @ ('n' | 'N')) => {
                self.step_match(c == 'n');
                vec![]
            }
            KeyCode::Esc if !self.needle.is_empty() => {
                self.needle.clear();
                vec![]
            }
            KeyCode::Char('J') => self.select_session(self.sel + 1, false),
            KeyCode::Char('K') => self.select_session(self.sel.saturating_sub(1), false),
            KeyCode::Enter => match self.sessions.get(self.sel) {
                Some(s) if self.open.as_ref() != Some(&s.id) => self.select_session(self.sel, true),
                _ => {
                    let hit = self.hit_at_cursor();
                    self.toggle(hit);
                    vec![]
                }
            },
            KeyCode::Down => {
                self.cursor(1);
                vec![]
            }
            KeyCode::Up => {
                self.cursor(-1);
                vec![]
            }
            KeyCode::PageDown | KeyCode::Char(' ') => {
                self.scroll(page);
                vec![]
            }
            KeyCode::PageUp | KeyCode::Char('b') => {
                self.scroll(-page);
                vec![]
            }
            KeyCode::Char('g') => {
                self.cursor(isize::MIN / 2);
                vec![]
            }
            KeyCode::Char('G') => {
                self.cursor(isize::MAX / 2);
                vec![]
            }
            KeyCode::Char('/') => {
                self.mode = Mode::Search { query: String::new(), hits: Vec::new(), sel: 0 };
                vec![]
            }
            KeyCode::Char(':') => {
                self.mode = Mode::Command { input: String::new() };
                vec![]
            }
            KeyCode::Char('c') if self.shown.is_some() => {
                self.mode = Mode::Comment { input: String::new() };
                vec![]
            }
            KeyCode::Char('v') => self.cycle_version(),
            KeyCode::Char('o') => commands::run(self, "open"),
            KeyCode::Char('r') => {
                self.start_rename();
                vec![]
            }
            KeyCode::Char(c @ ('d' | 'D')) => {
                self.start_delete(c == 'D');
                vec![]
            }
            _ => vec![],
        }
    }

    fn edit(input: &mut String, k: &KeyEvent) -> bool {
        match k.code {
            KeyCode::Char(c) => input.push(c),
            KeyCode::Backspace => {
                input.pop();
            }
            _ => return false,
        }
        true
    }

    fn search_key(&mut self, k: KeyEvent, mut query: String, hits: Vec<SessionHit>, sel: usize) -> Vec<Effect> {
        match k.code {
            KeyCode::Esc => vec![],
            KeyCode::Down => {
                self.mode = Mode::Search { sel: (sel + 1).min(hits.len().saturating_sub(1)), query, hits };
                vec![]
            }
            KeyCode::Up => {
                self.mode = Mode::Search { sel: sel.saturating_sub(1), query, hits };
                vec![]
            }
            KeyCode::Enter => match hits.get(sel) {
                Some(hit) => {
                    self.needle = crate::search::needle(&query);
                    self.find_post = hit.post.clone();
                    let mut fx = self.open_session(hit.session.id.clone());
                    // The hit's post is shown once the post list arrives.
                    if let Some(post) = &hit.post {
                        self.follow = false;
                        self.shown = Some(post.clone());
                    }
                    if fx.is_empty() {
                        if let Some(post) = hit.post.clone() {
                            fx = self.show(post);
                        }
                    }
                    fx
                }
                None => {
                    self.mode = Mode::Search { query, hits, sel };
                    vec![]
                }
            },
            _ if Self::edit(&mut query, &k) => {
                let fx = if query.trim().is_empty() {
                    vec![]
                } else {
                    vec![Effect::Request(StageRequest::SessionSearch(SessionSearchArgs {
                        query: query.clone(),
                        limit: Some(50),
                    }))]
                };
                self.mode = Mode::Search { query, hits, sel: 0 };
                fx
            }
            _ => {
                self.mode = Mode::Search { query, hits, sel };
                vec![]
            }
        }
    }

    fn comment_key(&mut self, k: KeyEvent, mut input: String) -> Vec<Effect> {
        match k.code {
            KeyCode::Esc => vec![],
            KeyCode::Enter if !input.trim().is_empty() => match self.shown.clone() {
                Some(post) => vec![Effect::Request(StageRequest::CommentNew(CommentNewArgs {
                    post: Some(post),
                    session: None,
                    text: input,
                    anchor: None,
                    author: Some("user".into()),
                }))],
                None => vec![],
            },
            _ => {
                Self::edit(&mut input, &k);
                self.mode = Mode::Comment { input };
                vec![]
            }
        }
    }

    fn rename_key(&mut self, k: KeyEvent, id: SessionId, mut input: String) -> Vec<Effect> {
        match k.code {
            KeyCode::Esc => vec![],
            // An empty title clears it (the sidebar falls back to the agent).
            KeyCode::Enter => vec![Effect::Request(StageRequest::SessionRename(SessionRenameArgs { id, title: input }))],
            _ => {
                Self::edit(&mut input, &k);
                self.mode = Mode::Rename { id, input };
                vec![]
            }
        }
    }

    fn command_key(&mut self, k: KeyEvent, mut input: String) -> Vec<Effect> {
        match k.code {
            KeyCode::Esc => vec![],
            KeyCode::Enter => commands::run(self, input.trim()),
            _ => {
                Self::edit(&mut input, &k);
                self.mode = Mode::Command { input };
                vec![]
            }
        }
    }

    fn mouse(&mut self, m: MouseEvent, at: Instant) -> Vec<Effect> {
        let g = ui::geometry_for(self);
        let inside = |r: ratatui::layout::Rect| {
            m.column >= r.x && m.column < r.x + r.width && m.row >= r.y && m.row < r.y + r.height
        };
        let session_area = ui::session_area(self);
        let fx = match m.kind {
            MouseEventKind::Down(MouseButton::Left) if m.row == g.header.y && m.column < 3 => {
                if matches!(self.mode, Mode::Search { .. }) {
                    self.mode = Mode::Nav;
                    return vec![Effect::Redraw];
                }
                if !matches!(self.mode, Mode::Nav) { return vec![]; }
                self.toggle_sidebar()
            }
            MouseEventKind::Down(MouseButton::Left) if m.row == g.keybar.y && matches!(self.mode, Mode::Nav) => {
                match ui::compact_action(self, m.column) {
                    Some(key) => self.nav_key(KeyEvent::new(key, KeyModifiers::NONE)),
                    None => vec![],
                }
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp if session_area.is_some() || matches!(self.mode, Mode::Search { .. }) => {
                let down = m.kind == MouseEventKind::ScrollDown;
                if let Mode::Search { sel, hits, .. } = &mut self.mode {
                    *sel = if down { (*sel + 1).min(hits.len().saturating_sub(1)) } else { sel.saturating_sub(1) };
                    vec![]
                } else {
                    self.select_session(if down { self.sel + 1 } else { self.sel.saturating_sub(1) }, false)
                }
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp if matches!(self.mode, Mode::Nav) => {
                let before = (self.scroll, self.cursor);
                self.drag = None;
                let rows = self.wheel_rows(m.kind == MouseEventKind::ScrollDown, at);
                self.scroll(rows);
                if before == (self.scroll, self.cursor) { return vec![]; }
                vec![]
            }
            MouseEventKind::Down(MouseButton::Left) if session_area.is_some_and(inside) || inside(g.sidebar) => {
                let area = session_area.unwrap_or(g.sidebar);
                if let Mode::Search { sel, hits, .. } = &mut self.mode {
                    let row = (m.row - area.y) as usize;
                    if row == 0 { return vec![]; }
                    let offset = ui::search_offset(*sel, area.height);
                    let index = offset + row - 1;
                    if index >= hits.len() { return vec![]; }
                    *sel = index;
                    self.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
                } else {
                    let row = (m.row - area.y) as usize + ui::sidebar_offset(self, area.height);
                    self.select_session(row, true)
                }
            }
            // In the feed: press starts a selection, drag extends it, release
            // copies it; a press and release in one place is a click.
            MouseEventKind::Down(MouseButton::Left) if session_area.is_none() && inside(g.main) && m.row > g.main.y => {
                let at = self.feed_point(m.column, m.row);
                self.drag = Some((at, at));
                vec![]
            }
            MouseEventKind::Drag(MouseButton::Left) if self.drag.is_some() => {
                let at = self.feed_point(m.column, m.row);
                if let Some((_, end)) = &mut self.drag {
                    *end = at;
                }
                vec![]
            }
            MouseEventKind::Up(MouseButton::Left) if self.drag.is_some() => {
                let (start, end) = self.drag.take().expect("checked");
                if start != end {
                    let text = self.selected_text(start, end);
                    let n = text.lines().count();
                    self.toast = Some(format!("copied {n} line{}", if n == 1 { "" } else { "s" }));
                    vec![Effect::Copy(text)]
                } else if start.0 < self.feed().lines.len() {
                    self.cursor = start.0;
                    self.follow_cursor();
                    let hit = self.hit_at_cursor();
                    self.toggle(hit);
                    vec![]
                } else {
                    vec![]
                }
            }
            // Motion and other buttons change nothing, so they don't redraw.
            _ => return vec![],
        };
        let mut fx = fx;
        fx.push(Effect::Redraw);
        fx
    }

    /// A mouse position in the feed as (line, cell column inside the content area).
    fn feed_point(&mut self, column: u16, row: u16) -> (usize, u16) {
        let g = ui::geometry_for(self);
        let line = self.scroll + row.saturating_sub(g.main.y + 1) as usize;
        (line, column.saturating_sub(g.main.x + 1))
    }

    /// The text between two feed points, without card frames or wrap markers.
    pub fn selected_text(&mut self, a: (usize, u16), b: (usize, u16)) -> String {
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        let lines = &self.feed().lines;
        let mut out = Vec::new();
        for (i, line) in lines.iter().enumerate().take(end.0 + 1).skip(start.0) {
            let text = line.to_string();
            let from = if i == start.0 { start.1 as usize } else { 0 };
            let to = if i == end.0 { end.1 as usize + 1 } else { usize::MAX };
            let cut = ui::cells(&text, from, to);
            let cut = cut.trim_start_matches(['│', '╭', '╰']).trim_end_matches(['│', '╮', '╯']);
            let cut = cut.strip_prefix(' ').unwrap_or(cut).trim_end();
            out.push(cut.strip_prefix("↪ ").unwrap_or(cut).to_owned());
        }
        out.join("\n")
    }

    // ---- notesd ---------------------------------------------------------------

    fn event(&mut self, e: Event) -> Vec<Effect> {
        let session = e.session.clone().map(SessionId::from);
        let mine = session.is_some() && session == self.open;
        let mut fx = Vec::new();
        match e.kind.as_str() {
            "session-created" | "session-updated" | "session-restored" => fx.push(Effect::Request(StageRequest::SessionList)),
            "session-deleted" => {
                if mine {
                    self.banner = Some("session ended".into());
                }
                fx.push(Effect::Request(StageRequest::SessionList));
            }
            "post-created" | "post-updated" | "post-deleted" if mine => {
                let id = PostId::from(e.id.clone().unwrap_or_default());
                // Events trail the post list: in a burst, one reply already holds the
                // posts of the hundreds of events queued behind it, so list only for news.
                let listed = self.posts.iter().find(|p| p.id == id).map(|p| p.version);
                let known = match e.kind.as_str() {
                    "post-deleted" => listed.is_none(),
                    _ => listed.is_some_and(|v| v >= e.version.unwrap_or(1)),
                };
                if !known {
                    fx.push(Effect::Request(StageRequest::PostList(self.open.clone().unwrap())));
                }
                if e.kind == "post-updated" {
                    if let Some(v) = e.version {
                        let pinned = self.views.get(&id).and_then(|v| v.pinned).is_some();
                        if !pinned {
                            fx.extend(self.load(id, v));
                        }
                    }
                }
                fx.push(Effect::Request(StageRequest::SessionList));
            }
            "comment-created" | "comment-deleted" if mine => {
                fx.push(Effect::Request(StageRequest::CommentList(self.open.clone().unwrap())));
            }
            k if k.starts_with("post-") || k.starts_with("comment-") => {
                if let Some(s) = session {
                    self.unread.insert(s);
                }
                fx.push(Effect::Request(StageRequest::SessionList));
            }
            _ => return vec![],
        }
        fx.push(Effect::Redraw);
        fx
    }

    fn reply(&mut self, r: Reply) -> Vec<Effect> {
        let mut fx = Vec::new();
        self.feed_gen += 1;
        match r {
            Reply::Saved => return fx,
            Reply::Sessions(sessions) => {
                let selected = self.sessions.get(self.sel).map(|s| s.id.clone());
                self.sessions = sessions;
                self.remembered.retain(|id, _| self.sessions.iter().any(|s| &s.id == id));
                // The open session was deleted: drop it and open the newest.
                if self.open.as_ref().is_some_and(|id| !self.sessions.iter().any(|s| &s.id == id)) {
                    self.leave_focus();
                    (self.open, self.shown, self.feed) = (None, None, None);
                    self.pending_html_focus = None;
                    self.posts.clear();
                    self.comments.clear();
                }
                if self.open.is_none() {
                    let target = match self.want.take() {
                        Some(want) if !self.sessions.iter().any(|s| s.id == want) => {
                            return vec![Effect::Fatal(format!("stage: no session \"{want}\""))];
                        }
                        Some(want) => Some(want),
                        None => self.sessions.first().map(|s| s.id.clone()),
                    };
                    if let Some(id) = target {
                        fx.extend(self.open_session(id));
                    }
                } else {
                    // Keep the selection on the same session as rows move.
                    self.sel = selected
                        .and_then(|id| self.sessions.iter().position(|s| s.id == id))
                        .unwrap_or(self.sel.min(self.sessions.len().saturating_sub(1)));
                }
            }
            Reply::Hits(query, found) => {
                if let Mode::Search { query: q, hits, sel } = &mut self.mode {
                    if *q == query {
                        *hits = found;
                        *sel = 0;
                    }
                }
            }
            Reply::Posts(session, mut posts) => {
                if self.open.as_ref() != Some(&session) {
                    return vec![];
                }
                posts.reverse();
                let before: Vec<PostId> = self.posts.iter().map(|p| p.id.clone()).collect();
                for p in &posts {
                    if self.seen_rows.insert((p.id.clone(), p.version)) {
                        self.fresh.push((p.id.clone(), p.version));
                        if self.shown.as_ref() != Some(&p.id) && !self.posts.is_empty() {
                            self.unseen.insert(p.id.clone());
                        }
                    }
                }
                self.posts = posts;
                if self.pending_html_focus.as_ref().is_some_and(|(id, version, _)| self.version_of(id) != Some(*version)) {
                    self.pending_html_focus = None;
                }
                if self.focused_card.as_ref().is_some_and(|(id, _)| !self.posts.iter().any(|p| &p.id == id)) {
                    self.leave_focus();
                }
                // Posts never move: a patch or a deleted card keeps the view where it
                // is, and only a post appended while following brings the feed along.
                let last = self.posts.last().map(|p| p.id.clone());
                let appended = last.as_ref().is_some_and(|id| !before.contains(id));
                let gone = self.shown.as_ref().filter(|id| !self.posts.iter().any(|p| &p.id == *id)).cloned();
                let target = if let Some(id) = gone {
                    // The shown post was deleted: the one that took its place, else the one before.
                    self.toast = Some(format!("post {id} was deleted"));
                    let at = before.iter().position(|b| *b == id).unwrap_or(usize::MAX);
                    let next = self.posts.get(at).or(self.posts.last()).map(|p| p.id.clone());
                    self.jump = next.clone();
                    next
                } else if let Some((id, _)) = &self.focused_card {
                    Some(id.clone())
                } else if self.follow && (appended || self.shown.is_none()) {
                    self.jump = last.clone();
                    last
                } else {
                    self.shown.clone().or(last)
                };
                // The feed shows every post, so load them all.
                let versions: Vec<_> = self.posts.iter().map(|p| (p.id.clone(), self.wanted_version(p))).collect();
                for (id, v) in versions {
                    fx.extend(self.load(id, v));
                }
                if let Some(id) = target {
                    fx.extend(self.show(id));
                } else {
                    self.shown = None;
                }
            }
            Reply::Post(post) => {
                let key = (post.id.clone(), post.version);
                if let Some(l) = self.loaded.get_mut(&key) {
                    l.post = Some(post);
                }
                self.built(key);
            }
            Reply::Render(render) => {
                let key = (render.id.clone(), render.version);
                if let Some(l) = self.loaded.get_mut(&key) {
                    l.render = Some(render);
                    l.cards = None;
                }
                self.built(key);
            }
            Reply::Comments(session, comments) => {
                if self.open.as_ref() == Some(&session) {
                    self.comments = comments;
                }
            }
            Reply::Commented(c) => {
                // Named from the comment, since the user may have opened another session by now.
                let agent = self.sessions.iter().find(|s| s.id == c.session).map_or("the agent".into(), |s| s.agent.clone());
                let post = c.post.as_ref().and_then(|p| self.loaded.iter().filter(|((id, _), _)| id == p).filter_map(|(_, l)| l.post.as_ref()).max_by_key(|p| p.version));
                let on = post.map_or(String::new(), |p| format!(" on {}", p.title));
                self.toast = Some(format!("sent{on} · {agent} sees it with its next post or wait"));
            }
            Reply::Failed(req, e) => {
                if let StageRequest::PostShow(a) | StageRequest::PostRender(a) = &req {
                    if let Some(v) = a.version {
                        // A render that failed after its post arrived keeps the plain cards up
                        // instead of the post dropping back to "loading…". Only one notesd
                        // never answered is worth asking again; any other would fail again.
                        let key = (a.id.clone(), v);
                        let plain = matches!(req, StageRequest::PostRender(_)) && self.loaded.get(&key).is_some_and(|l| l.post.is_some());
                        if !plain {
                            self.loaded.remove(&key);
                        } else if e.code == ErrorCode::DaemonUnreachable {
                            self.restyle.insert(key);
                        }
                    }
                }
                self.banner = (e.code == ErrorCode::DaemonUnreachable).then(|| "notesd is unreachable".into());
                self.toast = Some(format!("{}: {}", e.code_name(), e.message));
            }
        }
        fx.push(Effect::Redraw);
        fx
    }

    /// Rows painted since last call (for the latency log).
    pub fn take_fresh(&mut self) -> Vec<(PostId, u64)> {
        std::mem::take(&mut self.fresh)
    }
}

trait CodeName {
    fn code_name(&self) -> String;
}

impl CodeName for ErrorBody {
    fn code_name(&self) -> String {
        serde_json::to_value(self.code).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()
    }
}

/// Cells for a `w`×`h` image: as large as fits `cols` × `rows` cells, never
/// past `max_scale`, aspect kept.
pub fn fit_cells((w, h): (u32, u32), max_scale: f32, cols: u16, rows: u16, (cw, ch): (u16, u16)) -> Fit {
    if w == 0 || h == 0 || cw == 0 || ch == 0 {
        return Fit::Failed("empty image".into());
    }
    let scale = max_scale
        .min(cols as f32 * cw as f32 / w as f32)
        .min(rows.max(1) as f32 * ch as f32 / h as f32);
    Fit::Ready {
        cols: ((w as f32 * scale / cw as f32).ceil() as u16).clamp(1, cols.max(1)),
        rows: ((h as f32 * scale / ch as f32).ceil() as u16).clamp(1, rows.max(1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventState;
    use notes_proto::posts::{DiffFileRender, DiffHunk, DiffLayout, DiffLine, DiffLineKind, Part, PartRender, Span as WireSpan};

    fn key(c: KeyCode) -> Msg {
        Msg::Key(KeyEvent { code: c, modifiers: KeyModifiers::NONE, kind: KeyEventKind::Press, state: KeyEventState::NONE })
    }

    fn session(id: &str, at: i64) -> Session {
        Session { id: id.into(), agent: "claude".into(), title: Some(id.into()), cwd: None, created_at: at, last_active_at: at, post_count: 2 }
    }

    fn summary(id: &str, s: &str, v: u64) -> PostSummary {
        PostSummary { id: id.into(), session: s.into(), title: id.into(), kinds: vec!["markdown".into()], version: v, updated_at: 0 }
    }

    fn post(id: &str, s: &str, v: u64, md: &str) -> Post {
        Post { id: id.into(), session: s.into(), title: id.into(), parts: vec![Part::Markdown { markdown: md.into(), id: None }], version: v, created_at: 0, updated_at: 0 }
    }

    fn requests(fx: &[Effect]) -> Vec<StageRequest> {
        fx.iter().filter_map(|e| match e { Effect::Request(r) => Some(r.clone()), _ => None }).collect()
    }

    /// Answer requests from a fake notesd until none are left, like `pump`.
    fn serve(app: &mut App, fx: Vec<Effect>, db: &Fake) -> Vec<Effect> {
        let mut queue: std::collections::VecDeque<Effect> = fx.into();
        let mut out = Vec::new();
        while let Some(e) = queue.pop_front() {
            match e {
                Effect::Request(r) => queue.extend(app.update(Msg::Reply(db.answer(&r)))),
                other => out.push(other),
            }
        }
        out
    }

    struct Fake {
        sessions: Vec<Session>,
        posts: Vec<PostSummary>,
        md: String,
    }

    impl Fake {
        fn answer(&self, r: &StageRequest) -> Reply {
            match r {
                StageRequest::SessionList => Reply::Sessions(self.sessions.clone()),
                StageRequest::PostList(s) => {
                    Reply::Posts(s.clone(), self.posts.iter().filter(|p| &p.session == s).rev().cloned().collect())
                }
                StageRequest::PostShow(a) => {
                    let p = self.posts.iter().find(|p| p.id == a.id).unwrap();
                    Reply::Post(post(&p.id, &p.session, a.version.unwrap(), &self.md))
                }
                StageRequest::PostRender(a) => Reply::Render(PostRender {
                    id: a.id.clone(),
                    version: a.version.unwrap(),
                    parts: vec![notes_proto::posts::PartRender::Native],
                }),
                StageRequest::CommentList(s) => Reply::Comments(s.clone(), vec![]),
                StageRequest::Space(_)
                | StageRequest::SessionRename(_)
                | StageRequest::SessionDelete(_)
                | StageRequest::PostDelete(_)
                | StageRequest::PartDelete(..) => Reply::Saved,
                StageRequest::SessionSearch(a) => Reply::Hits(
                    a.query.clone(),
                    self.sessions.iter().filter(|s| s.id.contains(a.query.as_str())).map(|s| SessionHit { session: s.clone(), post: None, text: None }).collect(),
                ),
                StageRequest::CommentNew(a) => Reply::Commented(Comment {
                    id: "c".into(),
                    seq: 1,
                    session: "s1".into(),
                    post: a.post.clone(),
                    author: "user".into(),
                    text: a.text.clone(),
                    created_at: 0,
                    anchor: None,
                }),
            }
        }
    }

    fn fake() -> Fake {
        Fake {
            sessions: vec![session("s1", 2), session("s2", 1)],
            posts: vec![summary("a", "s1", 1), summary("b", "s1", 2), summary("z", "s2", 1)],
            md: "hello".into(),
        }
    }

    fn booted(db: &Fake) -> App {
        let mut app = App::new(None);
        app.size = (100, 30);
        let start = app.start();
        serve(&mut app, start, db);
        app
    }

    #[test]
    fn visuals_render_on_screen_first_then_their_nearest_neighbors() {
        let mut app = booted(&fake());
        let mut post = post("b", "s1", 2, "");
        post.parts = (0..8).map(|i| Part::Mermaid { mermaid: format!("graph TD\nA{i}-->B"), id: None }).collect();
        let render = PostRender { id: "b".into(), version: 2, parts: vec![notes_proto::posts::PartRender::Native; 8] };
        app.loaded.get_mut(&("b".into(), 2)).unwrap().cards = Some(cards::build_cards(&post, Some(&render)));
        app.views.get_mut(&PostId::from("b")).unwrap().laid = None;
        app.feed = None;
        // As `main` does: update, draw, then ask for what the drawn feed shows.
        let needed = |app: &mut App, msg: Msg| -> Vec<usize> {
            app.update(msg);
            app.after_draw().into_iter().filter_map(|e| match e { Effect::NeedVisual { key, .. } => Some(key.2), _ => None }).collect()
        };
        let first = needed(&mut app, Msg::Resize(100, 30));
        let (scroll, page) = (app.scroll, app.page());
        let shown: HashSet<usize> = app.feed().hits[scroll..scroll + page].iter().filter_map(|h| match h.1 { Hit::Card(c) => Some(c), _ => None }).collect();
        assert!(!first.is_empty() && first.iter().all(|c| shown.contains(c)), "{first:?} vs on screen {shown:?}");
        // Nothing off screen starts while an on-screen diagram is still rendering.
        assert!(needed(&mut app, Msg::Resize(100, 30)).is_empty());
        for &c in &first {
            app.update(Msg::VisualReady { key: ("b".into(), 2, c), w: 80, h: 40, max_scale: 1.0 });
        }
        let mut asked: HashSet<usize> = first.iter().copied().collect();
        asked.extend(needed(&mut app, Msg::Resize(100, 30)));
        assert!(asked.len() <= first.len() + 2, "the nearest one above and below, not the whole session: {asked:?}");
        // Scrolling to the end reaches the last diagram.
        for _ in 0..8 {
            asked.extend(needed(&mut app, key(KeyCode::Char('G'))));
            for c in asked.clone() {
                app.update(Msg::VisualReady { key: ("b".into(), 2, c), w: 80, h: 40, max_scale: 1.0 });
            }
        }
        assert!(asked.contains(&7), "{asked:?}");
    }

    #[test]
    fn tall_mermaid_scrolls_at_readable_size_while_html_stays_within_viewport() {
        let mut app = booted(&fake());
        app.cell_px = (10, 20);
        let mut post = post("b", "s1", 2, "");
        post.parts = vec![
            Part::Mermaid { mermaid: "flowchart TD\n A --> B".into(), id: None },
            Part::Html { html: "<p>page</p>".into(), kits: vec![], id: None },
        ];
        let render = PostRender { id: "b".into(), version: 2, parts: vec![notes_proto::posts::PartRender::Native; 2] };
        app.loaded.get_mut(&("b".into(), 2)).unwrap().cards = Some(cards::build_cards(&post, Some(&render)));
        for card in 0..2 {
            app.update(Msg::VisualReady { key: ("b".into(), 2, card), w: 700, h: 879, max_scale: 1.0 });
        }
        let page = app.page() as u16;
        let slots = &app.feed().slots;
        assert!(slots[0].1.rows > page, "the Mermaid card must scroll instead of shrinking to half a screen");
        assert!(slots[1].1.rows > page / 2 && slots[1].1.rows <= page - 3, "live HTML gets the feed below its title, not half a screen");
    }

    #[test]
    fn boots_into_the_newest_session_and_its_latest_post() {
        let db = fake();
        let app = booted(&db);
        assert_eq!(app.open.as_deref(), Some("s1"));
        assert_eq!(app.posts.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(app.shown.as_deref(), Some("b"));
        assert_eq!(app.sel, 0);
        assert!(app.shown_cards().is_some());
    }

    #[test]
    fn keys_between_a_request_and_its_reply_are_handled_and_the_reply_still_lands() {
        let db = fake();
        let mut app = booted(&db);
        app.feed(); // the first draw lays the feed out before any key arrives
        // Open s2; its replies are held, as the request worker holds them while notesd is slow.
        app.update(key(KeyCode::Char('J')));
        let held = requests(&app.update(key(KeyCode::Enter)));
        assert!(held.contains(&StageRequest::PostList("s2".into())), "{held:?}");
        // Keys keep working meanwhile: back to s1, then down a line.
        app.update(key(KeyCode::Char('K')));
        let back = app.update(key(KeyCode::Enter));
        assert_eq!(app.update(key(KeyCode::Down)), [Effect::Redraw]);
        assert_eq!(app.open.as_deref(), Some("s1"));
        // s2's late replies are for a session the user left: s1 stays as it is.
        for r in &held {
            app.update(Msg::Reply(db.answer(r)));
        }
        assert_eq!(app.open.as_deref(), Some("s1"));
        assert_eq!(app.posts.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        serve(&mut app, back, &db);
        assert_eq!(app.shown.as_deref(), Some("b"));
        // A failure for a post nobody waits on changes nothing but the toast.
        let stale = StageRequest::PostShow(PostShowArgs { id: "z".into(), version: Some(1) });
        app.update(Msg::Reply(Reply::Failed(stale, ErrorBody { code: ErrorCode::Other, message: "gone".into(), hint: None, details: None })));
        assert!(app.shown_cards().is_some() && app.toast.is_some());
        // s2 loads in full when the user returns to it.
        app.update(key(KeyCode::Char('J')));
        let fx = app.update(key(KeyCode::Enter));
        serve(&mut app, fx, &db);
        assert_eq!((app.open.as_deref(), app.shown.as_deref()), (Some("s2"), Some("z")));
        assert!(app.shown_cards().is_some());
    }

    #[test]
    fn a_post_reads_plain_before_its_render_and_the_viewport_holds_when_it_lands() {
        use notes_proto::posts::{DiffFileRender, DiffHunk, DiffLayout, DiffLine, DiffLineKind, PartRender, Span as WireSpan};
        let mut db = fake();
        db.md = (1..=20).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n\n");
        let mut app = booted(&db);
        app.update(Msg::Resize(100, 15));
        // Reading post b from its title row, which is the top of the viewport.
        let top = app.feed().starts[1];
        assert_eq!((app.scroll, app.cursor), (top, top));
        // Post a above gets a new version; only the lists come back, its show and render are in flight.
        db.posts[0].version = 2;
        let fx = app.update(Msg::Event(Event { seq: 9, kind: "post-updated".into(), path: None, version: Some(2), id: Some("a".into()), session: Some("s1".into()) }));
        let lists: Vec<_> = fx.into_iter().filter(|e| !matches!(e, Effect::Request(StageRequest::PostShow(_) | StageRequest::PostRender(_)))).collect();
        serve(&mut app, lists, &db);
        let patch = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old\n+new\n";
        let diff = Part::Diff { patch: Some(patch.into()), files: vec![], layout: DiffLayout::default(), id: None };
        let post = Post { id: "a".into(), session: "s1".into(), title: "a".into(), parts: vec![diff], version: 2, created_at: 0, updated_at: 0 };
        app.update(Msg::Reply(Reply::Post(post)));
        let feed = app.feed();
        assert!(feed.lines.iter().any(|l| l.to_string().contains("+new")), "the patch reads as plain text before its render");
        let top = feed.starts[1];
        assert_eq!((app.scroll, app.cursor), (top, top), "post b stays at the top of the viewport");
        let line = |kind, old, new, text: &str| DiffLine { kind, old_line: old, new_line: new, spans: vec![WireSpan { text: text.into(), class: None }] };
        let file = DiffFileRender {
            filename: "f".into(),
            language: None,
            added: 1,
            removed: 1,
            hunks: vec![DiffHunk { header: "@@ -1 +1 @@".into(), lines: vec![line(DiffLineKind::Removed, Some(1), None, "old"), line(DiffLineKind::Added, None, Some(1), "new")] }],
        };
        let render = PostRender { id: "a".into(), version: 2, parts: vec![PartRender::Diff { layout: DiffLayout::default(), files: vec![file] }] };
        app.update(Msg::Reply(Reply::Render(render)));
        let feed = app.feed();
        assert!(feed.lines.iter().any(|l| l.to_string().contains("+1 −1")), "the render's file header replaced the raw patch");
        assert!(!feed.lines.iter().any(|l| l.to_string().contains("diff --git")));
        let moved = feed.starts[1];
        assert_ne!(moved, top, "the rendered diff is shorter than the raw patch, so b moved");
        assert_eq!((app.scroll, app.cursor), (moved, moved), "b is still at the top of the viewport");
    }

    #[test]
    fn a_render_that_fails_leaves_the_plain_cards_up() {
        let db = fake();
        let mut app = booted(&db);
        db_less_version(&mut app, &db);
        let err = ErrorBody { code: ErrorCode::Other, message: "no highlighter".into(), hint: None, details: None };
        app.update(Msg::Reply(Reply::Failed(StageRequest::PostRender(PostShowArgs { id: "b".into(), version: Some(3) }), err)));
        assert!(app.feed().lines.iter().any(|l| l.to_string().contains("hello")), "the post is still readable");
        assert!(app.toast.as_deref().is_some_and(|t| t.contains("no highlighter")));
    }

    #[test]
    fn a_render_lost_to_a_notesd_restart_is_asked_again_when_the_watch_is_back() {
        let db = fake();
        let mut app = booted(&db);
        db_less_version(&mut app, &db);
        let render = StageRequest::PostRender(PostShowArgs { id: "b".into(), version: Some(3) });
        let err = ErrorBody { code: ErrorCode::DaemonUnreachable, message: "notesd: refused".into(), hint: None, details: None };
        app.update(Msg::Reply(Reply::Failed(render.clone(), err)));
        app.update(Msg::WatchLost);
        let fx = app.update(Msg::WatchBack);
        assert!(requests(&fx).contains(&render), "{:?}", requests(&fx));
        serve(&mut app, fx, &db);
        assert!(app.loaded[&("b".into(), 3)].render.is_some(), "styled once notesd answers");
        assert!(!requests(&app.update(Msg::WatchBack)).contains(&render), "asked once, not on every reconnect");
    }

    /// Post b at version 3 with its post reply landed and its render still in flight.
    fn db_less_version(app: &mut App, db: &Fake) {
        let fx = app.update(Msg::Event(Event { seq: 9, kind: "post-updated".into(), path: None, version: Some(3), id: Some("b".into()), session: Some("s1".into()) }));
        for e in fx {
            match e {
                Effect::Request(StageRequest::PostRender(_)) => {}
                Effect::Request(r) => {
                    let fx = app.update(Msg::Reply(db.answer(&r)));
                    serve(app, fx, db);
                }
                _ => {}
            }
        }
        assert!(app.feed().lines.iter().any(|l| l.to_string().contains("hello")));
    }

    #[test]
    fn a_comment_reply_after_a_session_switch_names_its_own_agent_and_post() {
        let mut db = fake();
        db.sessions[1].agent = "codex".into();
        let mut app = booted(&db);
        assert_eq!(app.open.as_deref(), Some("s1"));
        app.mode = Mode::Comment { input: "nit".into() };
        let send = requests(&app.update(key(KeyCode::Enter)));
        let fx = app.open_session("s2".into());
        serve(&mut app, fx, &db);
        app.update(Msg::Reply(db.answer(&send[0])));
        let toast = app.toast.clone().unwrap_or_default();
        assert!(toast.contains("on b") && toast.contains("claude") && !toast.contains("codex"), "{toast}");
    }

    #[test]
    fn unknown_session_is_fatal() {
        let db = fake();
        let mut app = App::new(Some("nope".into()));
        let start = app.start();
        let fx = serve(&mut app, start, &db);
        assert!(fx.iter().any(|e| matches!(e, Effect::Fatal(m) if m.contains("nope"))));
    }

    #[test]
    fn jk_moves_posts_and_shift_jk_sessions() {
        let db = fake();
        let mut app = booted(&db);
        let fx = app.update(key(KeyCode::Char('k')));
        assert_eq!(app.shown.as_deref(), Some("a"));
        let first = app.feed().starts[0];
        assert_eq!(app.cursor, first);
        // The feed loaded every post when the session opened, so moving asks for nothing.
        assert!(requests(&fx).is_empty());
        app.update(key(KeyCode::Char('j')));
        assert_eq!(app.shown.as_deref(), Some("b"));
        // J moves the sidebar to session 2; enter opens it.
        app.update(key(KeyCode::Char('J')));
        assert_eq!(app.sel, 1);
        assert_eq!(app.open.as_deref(), Some("s1"));
        let fx = app.update(key(KeyCode::Enter));
        assert_eq!(requests(&fx), [StageRequest::PostList("s2".into()), StageRequest::CommentList("s2".into())]);
        serve(&mut app, fx, &db);
        assert_eq!(app.shown.as_deref(), Some("z"));
    }

    #[test]
    fn search_flow() {
        let db = fake();
        let mut app = booted(&db);
        app.update(key(KeyCode::Char('/')));
        let fx = app.update(key(KeyCode::Char('2')));
        assert_eq!(requests(&fx), [StageRequest::SessionSearch(SessionSearchArgs { query: "2".into(), limit: Some(50) })]);
        serve(&mut app, fx, &db);
        let Mode::Search { hits, .. } = &app.mode else { panic!("{:?}", app.mode) };
        assert_eq!(hits.len(), 1);
        let fx = app.update(key(KeyCode::Enter));
        serve(&mut app, fx, &db);
        assert_eq!(app.mode, Mode::Nav);
        assert_eq!(app.open.as_deref(), Some("s2"));
        // Esc leaves search without doing anything.
        app.update(key(KeyCode::Char('/')));
        assert!(requests(&app.update(key(KeyCode::Esc))).is_empty());
        assert_eq!(app.mode, Mode::Nav);
    }

    #[test]
    fn wheel_accelerates_with_cadence_and_settles() {
        let mut db = fake();
        db.md = (0..400).map(|i| format!("line {i}\n\n")).collect();
        let mut app = booted(&db);
        app.update(key(KeyCode::Char('g')));
        let t0 = Instant::now();
        let tick = |app: &mut App, ms: u64, kind| {
            let before = app.scroll as isize;
            app.update(Msg::Wheel(MouseEvent { kind, column: 60, row: 10, modifiers: KeyModifiers::NONE }, t0 + std::time::Duration::from_millis(ms)));
            app.scroll as isize - before
        };
        assert_eq!(tick(&mut app, 0, MouseEventKind::ScrollDown), 1, "a lone notch moves one row");
        let fast: isize = (1..=20).map(|i| tick(&mut app, i * 10, MouseEventKind::ScrollDown)).sum();
        assert_eq!(fast, 20 * WHEEL as isize, "a fast spin ramps to the cap");
        assert_eq!(tick(&mut app, 205, MouseEventKind::ScrollDown), 1, "under 6 ms apart counts as one row");
        assert_eq!(tick(&mut app, 500, MouseEventKind::ScrollDown), 1, "a pause resets the ramp");
        assert_eq!(tick(&mut app, 550, MouseEventKind::ScrollDown), 1, "a slow notch stays near one row");
        assert_eq!(tick(&mut app, 600, MouseEventKind::ScrollUp), -1, "a reversal moves at once");
    }

    #[test]
    fn wheel_scrolls_and_click_folds() {
        let mut db = fake();
        db.md = (0..80).map(|i| format!("line {i}\n\n")).collect();
        let mut app = booted(&db);
        let wheel = |kind| Msg::Mouse(MouseEvent { kind, column: 60, row: 10, modifiers: KeyModifiers::NONE });
        app.update(wheel(MouseEventKind::ScrollUp));
        let top = app.scroll;
        app.update(wheel(MouseEventKind::ScrollDown));
        assert_eq!(app.scroll, top + 1, "a lone notch moves one row");
        app.update(wheel(MouseEventKind::ScrollUp));
        assert_eq!(app.scroll, top);
        // Motion doesn't redraw.
        assert!(app.update(wheel(MouseEventKind::Moved)).is_empty());

        // A json post: click its root node to fold it.
        let key = (PostId::from("b"), 2);
        let data = serde_json::json!({"a": 1, "b": 2});
        let p = Post { parts: vec![Part::Json { data, id: None }], ..post("b", "s1", 2, "") };
        let r = PostRender { id: "b".into(), version: 2, parts: vec![notes_proto::posts::PartRender::Native] };
        app.loaded.get_mut(&key).unwrap().cards = Some(build_cards(&p, Some(&r)));
        app.loaded.get_mut(&key).unwrap().post = Some(p);
        app.views.get_mut(&PostId::from("b")).unwrap().laid = None;
        app.feed = None;
        let before = app.feed().lines.len();
        let b = app.posts.iter().position(|p| p.id == "b").unwrap();
        let start = app.feed().starts[b];
        // In the feed: the post's title, its card border, then the root node.
        let root = start + 2;
        assert!(root >= app.scroll, "root on screen");
        let g = ui::geometry(app.size);
        let row = g.main.y + 1 + (root - app.scroll) as u16;
        let at = |kind| Msg::Mouse(MouseEvent { kind, column: g.main.x + 2, row, modifiers: KeyModifiers::NONE });
        // A click acts on release (a press could be the start of a drag).
        assert_eq!(app.update(at(MouseEventKind::Down(MouseButton::Left))), [Effect::Redraw]);
        assert_eq!(app.update(at(MouseEventKind::Up(MouseButton::Left))), [Effect::Redraw]);
        assert_eq!(app.feed().lines.len(), before - 2);

        // A drag across lines copies their text, without the card frame.
        let press = Msg::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: g.main.x + 1, row: g.main.y + 1, modifiers: KeyModifiers::NONE });
        let drag = Msg::Mouse(MouseEvent { kind: MouseEventKind::Drag(MouseButton::Left), column: g.main.x + 40, row: g.main.y + 4, modifiers: KeyModifiers::NONE });
        let release = Msg::Mouse(MouseEvent { kind: MouseEventKind::Up(MouseButton::Left), column: g.main.x + 40, row: g.main.y + 4, modifiers: KeyModifiers::NONE });
        app.update(press);
        app.update(drag);
        let fx = app.update(release);
        let copied = fx.iter().find_map(|e| match e { Effect::Copy(t) => Some(t.clone()), _ => None }).expect("a copy");
        assert!(!copied.contains('│') && !copied.contains('╭'), "{copied:?}");
        assert!(app.toast.as_deref().is_some_and(|t| t.starts_with("copied")));
    }

    /// Every pane size from 1×1 to 160×50, with real content: drawing never
    /// panics, no row runs past the pane, and the feed keeps the cursor's
    /// source line in place across a resize.
    #[test]
    fn every_size_draws_cleanly_and_keeps_its_place() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut db = fake();
        db.md = (0..40).map(|i| format!("para {i} with some words to wrap at narrow widths\n\n")).collect();
        let mut app = booted(&db);
        for w in (1..=160u16).step_by(7).chain([1, 2, 59, 60, 61]) {
            for h in (1..=50u16).step_by(5).chain([1, 2, 3]) {
                app.update(Msg::Resize(w, h));
                let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
                term.draw(|f| ui::draw(f, &mut app)).unwrap();
                let buf = term.backend().buffer();
                assert_eq!(buf.area.width, w);
                for o in &app.on_screen {
                    assert!(o.col + o.cols <= w && o.row + o.rows <= h, "visual outside {w}×{h}: {o:?}");
                }
            }
        }
        // Resize with the cursor mid-feed: the same post stays under it.
        app.update(Msg::Resize(100, 30));
        for _ in 0..12 {
            app.update(Msg::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        }
        let cursor = app.cursor;
        let before = (app.shown.clone(), app.feed().origins[cursor]);
        app.update(Msg::Resize(70, 30));
        let _ = app.feed();
        assert_eq!(app.shown, before.0);
        let cursor = app.cursor;
        let now = app.feed().origins[cursor];
        assert_eq!((now.card, now.src), (before.1.card, before.1.src), "the cursor keeps its source line across a rewrap");
    }

    #[test]
    fn focus_isolates_the_selected_card_and_restores_the_feed() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut app = booted(&fake());
        let id = PostId::from("b");
        let post_key = (id.clone(), 2);
        let p = Post {
            parts: vec![
                Part::Markdown { markdown: "first card\n\n".repeat(50), id: None },
                Part::Markdown { markdown: "second card".into(), id: None },
            ],
            ..post("b", "s1", 2, "")
        };
        let r = PostRender { id: id.clone(), version: 2, parts: vec![notes_proto::posts::PartRender::Native; 2] };
        app.loaded.get_mut(&post_key).unwrap().cards = Some(build_cards(&p, Some(&r)));
        app.loaded.get_mut(&post_key).unwrap().post = Some(p);
        app.feed = None;
        let b = app.posts.iter().position(|post| post.id == id).unwrap();
        let second = app.feed().hits.iter().position(|hit| *hit == (b, Hit::Card(1))).unwrap();
        app.cursor = second;
        app.scroll = second.saturating_sub(app.page() - 2).min(app.feed().lines.len().saturating_sub(app.page()));
        let original = (app.scroll, app.cursor);
        let normal_width = ui::geometry_for(&app).main.width;
        app.update(key(KeyCode::Char('z')));
        assert_eq!(app.focused_card, Some((id, 1)));
        assert_eq!(ui::geometry_for(&app).main.width, app.size.0);
        assert!(app.target().width_px > (normal_width as u32 - 5) * app.cell_px.0 as u32);
        let feed = app.feed();
        assert!(feed.lines.iter().any(|line| line.to_string().contains("second card")));
        assert!(!feed.lines.iter().any(|line| line.to_string().contains("first card")));
        assert!(feed.hits.contains(&(b, Hit::Card(1))));
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| ui::draw(f, &mut app)).unwrap();
        app.update(key(KeyCode::Esc));
        let _ = app.feed();
        assert_eq!(app.focused_card, None);
        assert_eq!((app.scroll, app.cursor), original);
        assert!(app.feed().lines.iter().any(|line| line.to_string().contains("first card")));
    }

    fn phone_draw(app: &mut App) -> String {
        use ratatui::{backend::TestBackend, Terminal};
        let mut term = Terminal::new(TestBackend::new(app.size.0, app.size.1)).unwrap();
        term.draw(|f| ui::draw(f, app)).unwrap();
        let buf = term.backend().buffer();
        (0..app.size.1).map(|y| (0..app.size.0).map(|x| buf[(x, y)].symbol()).collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn phone_picker_opens_by_tap_and_returns_to_the_same_feed() {
        let mut app = booted(&fake());
        app.update(Msg::Resize(49, 19));
        app.feed();
        let original = (app.scroll, app.cursor, app.target());
        let tap = |column, row| Msg::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: KeyModifiers::NONE });
        app.update(tap(1, 0));
        assert!(app.session_picker);
        assert!(phone_draw(&mut app).contains("Sessions"));
        assert!(app.on_screen.is_empty(), "graphics cannot cover the picker");
        assert_eq!(app.target(), original.2, "a picker must not resize live HTML");
        app.update(tap(4, 1)); // the already-open session must close the picker too
        assert!(!app.session_picker);
        app.feed();
        assert_eq!((app.scroll, app.cursor), (original.0, original.1));
        app.update(key(KeyCode::Char('s')));
        app.update(key(KeyCode::Down));
        let fx = app.update(key(KeyCode::Enter));
        serve(&mut app, fx, &fake());
        assert_eq!(app.open.as_deref(), Some("s2"));
        assert!(!app.session_picker);
        assert!(!app.sidebar_hidden, "phone navigation must preserve desktop preference");
        app.update(Msg::Resize(100, 30));
        assert!(ui::geometry_for(&app).sidebar.width > 0);
    }

    #[test]
    fn search_is_visible_and_tappable_without_a_sidebar() {
        for width in [49, 100] {
            let db = fake();
            let mut app = booted(&db);
            app.sidebar_hidden = true;
            app.update(Msg::Resize(width, 19));
            app.update(key(KeyCode::Char('/')));
            let fx = app.update(key(KeyCode::Char('2')));
            serve(&mut app, fx, &db);
            assert!(phone_draw(&mut app).contains("1 matches"));
            let fx = app.update(Msg::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 4, row: 2, modifiers: KeyModifiers::NONE }));
            serve(&mut app, fx, &db);
            assert_eq!(app.open.as_deref(), Some("s2"));
            assert_eq!(app.mode, Mode::Nav);
        }
    }

    #[test]
    fn phone_swipes_are_line_precise_bounded_and_do_not_resume_following_mid_post() {
        let mut db = fake();
        db.md = (0..100).map(|i| format!("line {i}\n\n")).collect();
        let mut app = booted(&db);
        app.update(Msg::Resize(49, 19));
        app.update(key(KeyCode::Char('G')));
        let bottom = app.scroll;
        let wheel = |kind| Msg::Mouse(MouseEvent { kind, column: 20, row: 9, modifiers: KeyModifiers::NONE });
        for i in 1..=12 {
            app.update(wheel(MouseEventKind::ScrollUp));
            assert_eq!(app.scroll, bottom - i);
            assert!(!app.follow, "scrolling back in the last post must stop auto-follow");
            assert_eq!(app.shown.as_deref(), Some("b"));
        }
        for _ in 0..12 { app.update(wheel(MouseEventKind::ScrollDown)); }
        assert_eq!(app.scroll, bottom);
        assert!(app.follow);
        assert!(app.update(wheel(MouseEventKind::ScrollDown)).is_empty(), "no redraw for inertia at the edge");
        app.update(key(KeyCode::Char('g')));
        for _ in 0..2000 { app.update(wheel(MouseEventKind::ScrollUp)); }
        assert_eq!(app.scroll, 0);
        // The bottom controls are tappable pages, with one line of overlap.
        app.update(Msg::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 9, row: 18, modifiers: KeyModifiers::NONE }));
        assert_eq!(app.scroll, app.page() - 1);
        // An active picker scrolls its selection, not the feed behind it.
        let scroll = app.scroll;
        app.update(key(KeyCode::Char('s')));
        app.update(wheel(MouseEventKind::ScrollDown));
        assert_eq!(app.sel, 1);
        assert_eq!(app.scroll, scroll);
    }

    #[test]
    fn phone_prompts_keep_the_end_of_the_input_visible() {
        let mut app = booted(&fake());
        for width in [24, 39, 49, 59, 100] {
            app.update(Msg::Resize(width, 19));
            let input = format!("{}END", "long comment 界 ".repeat(20));
            for mode in [Mode::Comment { input: input.clone() }, Mode::Command { input: input.clone() }, Mode::Search { query: input, hits: vec![], sel: 0 }] {
                app.mode = mode;
                let screen = phone_draw(&mut app);
                assert!(screen.lines().last().unwrap().contains("END▏"), "{screen}");
            }
        }
    }

    #[test]
    fn sidebar_toggles_by_key_and_header_click() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut app = booted(&fake());
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let normal_width = ui::geometry_for(&app).main.width;
        let _ = app.feed();
        let original = (app.scroll, app.cursor);
        let hidden = app.update(key(KeyCode::Char('s')));
        assert!(app.sidebar_hidden);
        assert_eq!(ui::geometry_for(&app).main.width, 100);
        assert_eq!(reported_cols(&hidden), Some(95));
        term.draw(|f| ui::draw(f, &mut app)).unwrap();
        let click = Msg::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 2, row: 0, modifiers: KeyModifiers::NONE });
        app.update(click);
        assert!(!app.sidebar_hidden);
        assert_eq!(ui::geometry_for(&app).main.width, normal_width);
        let _ = app.feed();
        assert_eq!((app.scroll, app.cursor), original);
    }

    #[test]
    fn sidebar_waits_for_visible_html_at_the_new_width() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut app = booted(&fake());
        let id = PostId::from("b");
        let post_key = (id.clone(), 2);
        let post = Post { parts: vec![Part::Html { html: "<p>hello</p>".into(), kits: vec![], id: None }], ..post("b", "s1", 2, "") };
        let render = PostRender { id: id.clone(), version: 2, parts: vec![notes_proto::posts::PartRender::Native] };
        app.loaded.get_mut(&post_key).unwrap().cards = Some(build_cards(&post, Some(&render)));
        app.loaded.get_mut(&post_key).unwrap().post = Some(post);
        let visual = (id, 2, 0);
        let narrow = app.target().width_px;
        app.visuals.insert(visual.clone(), VisualState::Ready { w: narrow, h: 200, max_scale: 1.0 });
        app.feed = None;
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| ui::draw(f, &mut app)).unwrap();
        assert!(app.on_screen.iter().any(|shown| shown.key == visual));

        let old_geometry = ui::geometry_for(&app).main;
        let effects = app.update(key(KeyCode::Char('s')));
        assert!(!app.sidebar_hidden);
        assert_eq!(ui::geometry_for(&app).main, old_geometry);
        assert_eq!(reported_cols(&effects), None);
        app.after_draw();
        assert!(app.pending_sidebar.is_some(), "a redraw with the card still on screen keeps waiting");
        let wide = app.target().width_px;
        assert!(wide > narrow);
        app.update(Msg::VisualReady { key: visual.clone(), w: narrow, h: 200, max_scale: 1.0 });
        assert!(!app.sidebar_hidden, "an old frame cannot complete the toggle");
        let effects = app.update(Msg::VisualReady { key: visual.clone(), w: wide, h: 200, max_scale: 1.0 });
        assert!(app.sidebar_hidden);
        assert_eq!(ui::geometry_for(&app).main.width, 100);
        assert_eq!(app.target().width_px, wide);
        assert_eq!(reported_cols(&effects), Some(95));

        term.draw(|f| ui::draw(f, &mut app)).unwrap();
        app.update(key(KeyCode::Char('s')));
        assert!(app.sidebar_hidden, "showing the sidebar also waits for the narrow frame");
        assert_eq!(app.target().width_px, narrow);
        app.update(Msg::VisualReady { key: visual.clone(), w: narrow, h: 200, max_scale: 1.0 });
        assert!(!app.sidebar_hidden);
        assert_eq!(ui::geometry_for(&app).main, old_geometry);

        term.draw(|f| ui::draw(f, &mut app)).unwrap();
        app.update(key(KeyCode::Char('s')));
        assert!(app.pending_sidebar.is_some());
        app.on_screen.clear();
        let effects = app.after_draw();
        assert!(app.pending_sidebar.is_none(), "scrolling away cannot leave the toggle waiting forever");
        assert!(app.sidebar_hidden, "and the toggle the user asked for still happens");
        assert_eq!(reported_cols(&effects), Some(95));
        term.draw(|f| ui::draw(f, &mut app)).unwrap();
        app.update(key(KeyCode::Char('s')));
        assert!(app.pending_sidebar.is_some());
        app.update(key(KeyCode::Char('s')));
        assert!(app.pending_sidebar.is_none(), "a second s cancels a waiting one");
        assert!(app.sidebar_hidden);
        assert_eq!(app.target().width_px, wide);
    }

    #[test]
    fn sidebar_switches_multiple_visible_html_cards_together() {
        let mut app = booted(&fake());
        let id = PostId::from("b");
        let html = Part::Html { html: "<p>hello</p>".into(), kits: vec![], id: None };
        let post = Post { parts: vec![html.clone(), html], ..post("b", "s1", 2, "") };
        let render = PostRender { id: id.clone(), version: 2, parts: vec![notes_proto::posts::PartRender::Native; 2] };
        app.loaded.get_mut(&(id.clone(), 2)).unwrap().cards = Some(build_cards(&post, Some(&render)));
        let narrow = app.target().width_px;
        let keys: Vec<_> = (0..2).map(|card| (id.clone(), 2, card)).collect();
        for (card, key) in keys.iter().enumerate() {
            app.visuals.insert(key.clone(), VisualState::Ready { w: narrow, h: 200, max_scale: 1.0 });
            app.on_screen.push(OnScreen { key: key.clone(), col: 26, row: 3 + card as u16 * 12, cols: 70, top: 0, rows: 10, total: 10, px: (narrow, 200) });
        }
        app.update(key(KeyCode::Char('s')));
        let wide = app.target().width_px;
        app.update(Msg::VisualReady { key: keys[0].clone(), w: wide, h: 300, max_scale: 1.0 });
        assert!(!app.sidebar_hidden);
        assert!(matches!(app.visuals[&keys[0]], VisualState::Ready { w, h: 200, .. } if w == narrow));
        app.update(Msg::VisualReady { key: keys[1].clone(), w: wide, h: 300, max_scale: 1.0 });
        assert!(app.sidebar_hidden);
        assert!(keys.iter().all(|key| matches!(app.visuals[key], VisualState::Ready { w, .. } if w == wide)));
    }

    #[test]
    fn html_focus_waits_for_the_resized_frame_without_a_narrow_intermediate_card() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut app = booted(&fake());
        let id = PostId::from("b");
        let post_key = (id.clone(), 2);
        let p = Post { parts: vec![Part::Html { html: "<p>hello</p>".into(), kits: vec![], id: None }], ..post("b", "s1", 2, "") };
        let r = PostRender { id: id.clone(), version: 2, parts: vec![notes_proto::posts::PartRender::Native] };
        app.loaded.get_mut(&post_key).unwrap().cards = Some(build_cards(&p, Some(&r)));
        app.loaded.get_mut(&post_key).unwrap().post = Some(p);
        let visual = (id, 2, 0);
        let normal_width = app.target().width_px;
        app.visuals.insert(visual.clone(), VisualState::Ready { w: normal_width, h: 200, max_scale: 1.0 });
        app.feed = None;
        app.cursor = app.feed().starts[1] + 2;
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| ui::draw(f, &mut app)).unwrap();
        assert!(app.on_screen.iter().any(|shown| shown.key == visual));
        let original = (app.scroll, app.cursor);
        app.update(key(KeyCode::Char('z')));
        assert_eq!(app.focused_card, None);
        assert_eq!(app.pending_html_focus, Some(visual.clone()));
        assert_eq!(ui::geometry_for(&app).sidebar.width, ui::geometry(app.size).sidebar.width);
        assert_eq!((app.scroll, app.cursor), original);
        let full_width = app.target().width_px;
        assert!(full_width > normal_width);
        app.update(Msg::VisualReady { key: visual.clone(), w: normal_width, h: 200, max_scale: 1.0 });
        assert_eq!(app.pending_html_focus, Some(visual.clone()), "an old animation frame cannot enter focus");
        app.update(Msg::VisualReady { key: visual.clone(), w: full_width, h: 230, max_scale: 1.0 });
        assert_eq!(app.focused_card, Some((visual.0.clone(), 0)));
        assert_eq!(app.pending_html_focus, None);
        assert_eq!(ui::geometry_for(&app).sidebar.width, 0);
        app.update(key(KeyCode::Esc));
        let _ = app.feed();
        assert_eq!((app.scroll, app.cursor), original);
        term.draw(|f| ui::draw(f, &mut app)).unwrap();
        app.update(key(KeyCode::Char('z')));
        assert_eq!(app.pending_html_focus, Some(visual));
        app.update(key(KeyCode::Esc));
        assert_eq!(app.pending_html_focus, None);
        assert_eq!(app.focused_card, None);
        assert_eq!(app.target().width_px, normal_width);
    }

    #[test]
    fn focused_mermaid_keeps_its_pixels_1_to_1_until_the_full_width_raster() {
        let mut app = booted(&fake());
        let id = PostId::from("b");
        let post_key = (id.clone(), 2);
        let p = Post { parts: vec![Part::Mermaid { mermaid: "flowchart TD\n A-->B".into(), id: None }], ..post("b", "s1", 2, "") };
        let r = PostRender { id: id.clone(), version: 2, parts: vec![notes_proto::posts::PartRender::Native] };
        app.loaded.get_mut(&post_key).unwrap().cards = Some(build_cards(&p, Some(&r)));
        app.loaded.get_mut(&post_key).unwrap().post = Some(p);
        let visual = (id, 2, 0);
        app.visuals.insert(visual.clone(), VisualState::Ready { w: 200, h: 200, max_scale: 1.0 });
        app.feed = None;
        let post_index = app.posts.iter().position(|p| p.id == visual.0).unwrap();
        app.cursor = app.feed().starts[post_index];
        let normal_slot = app.feed().slots.iter().find(|(key, _)| key == &visual).unwrap().1;
        app.update(key(KeyCode::Char('z')));
        let preview_slot = app.feed().slots.iter().find(|(key, _)| key == &visual).unwrap().1;
        // Upscaling the old raster would blur it: same size until the crisp one lands.
        assert_eq!((preview_slot.cols, preview_slot.rows), (normal_slot.cols, normal_slot.rows));
        app.update(Msg::VisualReady { key: visual.clone(), w: 380, h: 380, max_scale: 1.0 });
        let crisp_slot = app.feed().slots.iter().find(|(key, _)| key == &visual).unwrap().1;
        assert!(crisp_slot.cols > preview_slot.cols);
        assert_eq!(app.visuals[&visual], VisualState::Ready { w: 380, h: 380, max_scale: 1.0 });
    }

    #[test]
    fn leaving_focus_lays_the_diagram_out_at_its_feed_size_at_once() {
        let mut app = booted(&fake());
        let id = PostId::from("b");
        let p = Post { parts: vec![Part::Mermaid { mermaid: "flowchart TD\n A-->B".into(), id: None }], ..post("b", "s1", 2, "") };
        let r = PostRender { id: id.clone(), version: 2, parts: vec![notes_proto::posts::PartRender::Native] };
        app.loaded.get_mut(&(id.clone(), 2)).unwrap().cards = Some(build_cards(&p, Some(&r)));
        let visual = (id, 2, 0);
        app.visuals.insert(visual.clone(), VisualState::Ready { w: 360, h: 260, max_scale: 1.0 });
        app.feed = None;
        let post_index = app.posts.iter().position(|p| p.id == visual.0).unwrap();
        app.cursor = app.feed().starts[post_index];
        let feed_slot = app.feed().slots.iter().find(|(key, _)| key == &visual).unwrap().1;
        app.update(key(KeyCode::Char('z')));
        app.update(Msg::ZoomShown { key: visual.clone(), w: 1150, h: 830 });
        app.update(key(KeyCode::Char('z')));
        let slot = app.feed().slots.iter().find(|(key, _)| key == &visual).unwrap().1;
        assert_eq!((slot.cols, slot.rows), (feed_slot.cols, feed_slot.rows), "no frame at the zoomed size squeezed into the card");
    }

    #[test]
    fn z_lays_out_a_zoom_made_ahead_from_its_first_frame() {
        let mut app = booted(&fake());
        let id = PostId::from("b");
        let p = Post { parts: vec![Part::Mermaid { mermaid: "flowchart TD\n A-->B".into(), id: None }], ..post("b", "s1", 2, "") };
        let r = PostRender { id: id.clone(), version: 2, parts: vec![notes_proto::posts::PartRender::Native] };
        app.loaded.get_mut(&(id.clone(), 2)).unwrap().cards = Some(build_cards(&p, Some(&r)));
        let visual = (id, 2, 0);
        app.visuals.insert(visual.clone(), VisualState::Ready { w: 360, h: 260, max_scale: 1.0 });
        app.feed = None;
        let post_index = app.posts.iter().position(|p| p.id == visual.0).unwrap();
        app.cursor = app.feed().starts[post_index];
        let fx = app.after_draw();
        let target = app.zoom_target();
        assert!(fx.contains(&Effect::PrefetchZoom { key: visual.clone(), target }), "{fx:?}");
        assert!(!app.after_draw().iter().any(|e| matches!(e, Effect::PrefetchZoom { .. })), "asked once");
        let wide = target.width_px;
        app.update(Msg::ZoomReady { key: visual.clone(), w: wide, h: wide / 2 });
        app.update(key(KeyCode::Char('z')));
        let slot = app.feed().slots.iter().find(|(key, _)| key == &visual).unwrap().1;
        assert_eq!(slot.cols as u32 * app.cell_px.0 as u32, wide, "the first focused layout is already the zoomed size");
    }

    #[test]
    fn a_focused_diagram_zooms_and_pans_within_its_edges() {
        let mut app = booted(&fake());
        let id = PostId::from("b");
        let p = Post { parts: vec![Part::Mermaid { mermaid: "flowchart TD\n A-->B".into(), id: None }], ..post("b", "s1", 2, "") };
        let r = PostRender { id: id.clone(), version: 2, parts: vec![notes_proto::posts::PartRender::Native] };
        app.loaded.get_mut(&(id.clone(), 2)).unwrap().cards = Some(build_cards(&p, Some(&r)));
        app.visuals.insert((id.clone(), 2, 0), VisualState::Ready { w: 360, h: 260, max_scale: 1.0 });
        app.feed = None;
        let post_index = app.posts.iter().position(|p| p.id == id).unwrap();
        app.cursor = app.feed().starts[post_index];
        app.update(key(KeyCode::Char('+')));
        assert_eq!(app.zoom_view, ZoomView::default(), "outside focus + does nothing");
        app.update(key(KeyCode::Char('z')));
        app.update(key(KeyCode::Char('l')));
        assert_eq!(app.zoom_view, ZoomView::default(), "fitted: nothing to pan");
        app.update(key(KeyCode::Char('+')));
        app.update(key(KeyCode::Char('+')));
        assert_eq!(app.zoom_view.level, 2);
        for _ in 0..20 {
            app.update(key(KeyCode::Right));
        }
        let edge = 1000 - (500.0 / 1.5f32.powi(2)) as u16;
        assert_eq!(app.zoom_view.cx, edge, "panning stops at the diagram's edge");
        app.update(key(KeyCode::Char('0')));
        assert_eq!(app.zoom_view, ZoomView::default());
        app.update(key(KeyCode::Char('+')));
        app.update(key(KeyCode::Char('z')));
        assert_eq!(app.zoom_view, ZoomView::default(), "leaving focus resets the zoom");
    }

    #[test]
    fn z_right_after_moving_onto_an_html_card_waits_for_its_wide_frame() {
        // Codex review: keys drained in one batch saw the last frame's on_screen, so z on an
        // HTML card just scrolled to focused at once on its narrow pixels.
        let mut app = booted(&fake());
        let id = PostId::from("b");
        let p = Post { parts: vec![Part::Html { html: "<p>hi</p>".into(), kits: vec![], id: None }], ..post("b", "s1", 2, "") };
        let r = PostRender { id: id.clone(), version: 2, parts: vec![notes_proto::posts::PartRender::Native] };
        app.loaded.get_mut(&(id.clone(), 2)).unwrap().cards = Some(build_cards(&p, Some(&r)));
        let narrow = app.target().width_px;
        app.visuals.insert((id.clone(), 2, 0), VisualState::Ready { w: narrow, h: 200, max_scale: 1.0 });
        app.feed = None;
        app.on_screen.clear(); // nothing drawn since the cursor moved
        let post_index = app.posts.iter().position(|p| p.id == id).unwrap();
        app.cursor = app.feed().starts[post_index];
        app.scroll = app.cursor;
        app.update(key(KeyCode::Char('z')));
        assert_eq!(app.pending_html_focus, Some((id, 2, 0)), "waits for the full-width frame");
        assert_eq!(app.focused_card, None);
    }

    #[test]
    fn resize_is_exactly_one_redraw() {
        let mut app = booted(&fake());
        // A new card size is reported to notesd once; the same size again only redraws.
        let cols = ui::geometry((120, 40)).main.width - 5;
        let fx = app.update(Msg::Resize(120, 40));
        assert_eq!((fx[0].clone(), reported_cols(&fx)), (Effect::Redraw, Some(cols)));
        let Some(Effect::Request(StageRequest::Space(space))) = app.update(Msg::Resize(120, 50)).pop() else { panic!("taller pane not reported") };
        assert_eq!((space.cols, space.rows, space.html_height), (cols, app.page() as u16, (app.page() as u32 - 3) * 16 / 2));
        assert_eq!(app.update(Msg::Resize(120, 50)), [Effect::Redraw]);
        assert_eq!(app.size, (120, 50));
    }

    #[test]
    fn events_dedupe_and_rebuild_only_new_versions() {
        let mut db = fake();
        let mut app = booted(&db);
        let builds = app.builds;
        let ev = |kind: &str, id: &str, v| Msg::Event(Event { seq: 9, kind: kind.into(), path: None, version: Some(v), id: Some(id.into()), session: Some("s1".into()) });
        // An event for the version already loaded builds nothing.
        let fx = app.update(ev("post-updated", "b", 2));
        serve(&mut app, fx, &db);
        assert_eq!(app.builds, builds);
        // A new version builds once (plain, then styled), even when its event repeats.
        db.posts[1].version = 3;
        for _ in 0..2 {
            let fx = app.update(ev("post-updated", "b", 3));
            serve(&mut app, fx, &db);
        }
        assert_eq!(app.builds, builds + 2);
        // Another session's post marks it unread and builds nothing.
        let fx = app.update(Msg::Event(Event { seq: 10, kind: "post-created".into(), path: None, version: Some(1), id: Some("z".into()), session: Some("s2".into()) }));
        serve(&mut app, fx, &db);
        assert!(app.unread.contains("s2"));
        assert_eq!(app.builds, builds + 2);
    }

    fn reported_cols(fx: &[Effect]) -> Option<u16> {
        fx.iter().find_map(|e| match e {
            Effect::Request(StageRequest::Space(s)) => Some(s.cols),
            _ => None,
        })
    }

    fn post_event(kind: &str, id: &str, version: u64) -> Msg {
        Msg::Event(Event { seq: 20, kind: kind.into(), path: None, version: Some(version), id: Some(id.into()), session: Some("s1".into()) })
    }

    /// Session s1 with posts a, b, c, d (made in that order), each taller than the page.
    fn tall() -> Fake {
        let mut db = fake();
        db.posts = ["a", "b", "c", "d"].iter().map(|id| summary(id, "s1", 1)).collect();
        db.md = "line\n\n".repeat(30);
        db
    }

    fn send(app: &mut App, msg: Msg, db: &Fake) {
        let fx = app.update(msg);
        serve(app, fx, db);
    }

    fn order(app: &App) -> Vec<&str> {
        app.posts.iter().map(|p| p.id.as_str()).collect()
    }

    #[test]
    fn a_patch_never_moves_a_post_or_the_view() {
        let mut db = tall();
        let mut app = booted(&db);
        for k in [KeyCode::Char('g'), KeyCode::Char('j')] {
            send(&mut app, key(k), &db);
        }
        assert_eq!(app.shown.as_deref(), Some("b"));
        let place = (app.scroll, app.cursor);
        for id in ["d", "b", "a"] {
            let p = db.posts.iter_mut().find(|p| p.id == id).unwrap();
            p.version += 1;
            let v = p.version;
            send(&mut app, post_event("post-updated", id, v), &db);
            app.feed();
            assert_eq!(order(&app), ["a", "b", "c", "d"]);
            assert_eq!((app.shown.as_deref(), app.scroll, app.cursor), (Some("b"), place.0, place.1), "after patching {id}");
        }
        // Reading partway down the last post: a patch above doesn't pull it back to its title.
        send(&mut app, key(KeyCode::Char('G')), &db);
        send(&mut app, key(KeyCode::Up), &db);
        let place = (app.scroll, app.cursor);
        db.posts[0].version += 1;
        send(&mut app, post_event("post-updated", "a", db.posts[0].version), &db);
        app.feed();
        assert_eq!((app.shown.as_deref(), app.scroll, app.cursor), (Some("d"), place.0, place.1));
    }

    #[test]
    fn a_new_post_pulls_the_feed_only_while_reading_the_last_one() {
        let mut db = tall();
        let mut app = booted(&db);
        assert_eq!(app.shown.as_deref(), Some("d"));
        // Scrolling up into c by line, not by post, stops following.
        for _ in 0..3 {
            send(&mut app, key(KeyCode::Up), &db);
        }
        assert_eq!(app.shown.as_deref(), Some("c"));
        let place = (app.scroll, app.cursor);
        db.posts.push(summary("e", "s1", 1));
        send(&mut app, post_event("post-created", "e", 1), &db);
        app.feed();
        assert_eq!((app.shown.as_deref(), app.scroll, app.cursor), (Some("c"), place.0, place.1));
        // Back on the last post, the next one is followed to its title.
        send(&mut app, key(KeyCode::Char('G')), &db);
        db.posts.push(summary("f", "s1", 1));
        send(&mut app, post_event("post-created", "f", 1), &db);
        let start = app.feed().starts[5];
        assert_eq!((app.shown.as_deref(), app.cursor), (Some("f"), start));
    }

    #[test]
    fn deleting_the_shown_post_lands_on_its_neighbour() {
        let mut db = tall();
        let mut app = booted(&db);
        for k in [KeyCode::Char('g'), KeyCode::Char('j')] {
            send(&mut app, key(k), &db);
        }
        db.posts.retain(|p| p.id != "b");
        send(&mut app, post_event("post-deleted", "b", 1), &db);
        let start = app.feed().starts[1];
        assert_eq!((order(&app), app.shown.as_deref(), app.cursor), (vec!["a", "c", "d"], Some("c"), start));
        // The last post has no next one, so the one before it takes over.
        send(&mut app, key(KeyCode::Char('G')), &db);
        db.posts.retain(|p| p.id != "d");
        send(&mut app, post_event("post-deleted", "d", 1), &db);
        assert_eq!(app.shown.as_deref(), Some("c"));
    }

    #[test]
    fn a_patched_diagram_keeps_its_height_until_its_new_pixels_land() {
        let mut app = booted(&fake());
        let diagram = |v| Post { parts: vec![Part::Mermaid { mermaid: "graph TD\nA-->B".into(), id: None }], ..post("b", "s1", v, "") };
        let render = |v| PostRender { id: "b".into(), version: v, parts: vec![notes_proto::posts::PartRender::Native] };
        app.loaded.get_mut(&("b".into(), 2)).unwrap().cards = Some(cards::build_cards(&diagram(2), Some(&render(2))));
        app.visuals.insert(("b".into(), 2, 0), VisualState::Ready { w: 400, h: 1200, max_scale: 1.0 });
        app.feed_gen += 1;
        let shown = app.feed().lines.len();
        app.posts.iter_mut().find(|p| p.id == "b").unwrap().version = 3;
        let cards = Some(cards::build_cards(&diagram(3), Some(&render(3))));
        app.loaded.insert(("b".into(), 3), Loaded { post: Some(diagram(3)), render: Some(render(3)), cards });
        app.visuals.insert(("b".into(), 3, 0), VisualState::Pending);
        app.feed_gen += 1;
        assert_eq!(app.feed().lines.len(), shown);
    }

    #[test]
    fn the_feed_shows_each_version_comment_and_post_as_it_changes() {
        let mut db = fake();
        let mut app = booted(&db);
        let shows = |app: &mut App, text: &str| app.feed().lines.iter().any(|l| l.to_string().contains(text));
        // `v` to an older version, then back to the latest, which is still loaded.
        for r in requests(&app.update(key(KeyCode::Char('v')))) {
            let reply = match db.answer(&r) {
                Reply::Post(p) => Reply::Post(Post { parts: vec![Part::Markdown { markdown: "older text".into(), id: None }], ..p }),
                reply => reply,
            };
            app.update(Msg::Reply(reply));
        }
        assert!(shows(&mut app, "older text"));
        assert!(requests(&app.update(key(KeyCode::Char('v')))).is_empty(), "the latest is still loaded");
        assert!(!shows(&mut app, "older text") && shows(&mut app, "hello"));
        let comment = Comment { id: "c1".into(), seq: 1, session: "s1".into(), post: Some("b".into()), author: "claude".into(), text: "nice catch".into(), created_at: 0, anchor: None };
        app.update(Msg::Reply(Reply::Comments("s1".into(), vec![comment])));
        assert!(shows(&mut app, "nice catch"));
        db.posts.push(summary("c", "s1", 1));
        db.md = "a new post".into();
        let fx = app.update(post_event("post-created", "c", 1));
        serve(&mut app, fx, &db);
        assert!(shows(&mut app, "a new post"));
    }

    #[test]
    fn a_burst_of_events_lists_posts_only_for_changes_the_list_lacks() {
        let mut db = fake();
        let mut app = booted(&db);
        let mut lists = 0;
        let mut send = |app: &mut App, msg, db: &Fake| {
            let fx = app.update(msg);
            lists += requests(&fx).iter().filter(|r| matches!(r, StageRequest::PostList(_))).count();
            serve(app, fx, db);
        };
        // Three posts land before stage hears of any: the first event's list shows them all.
        for id in ["c", "d", "e"] {
            db.posts.push(summary(id, "s1", 1));
        }
        for id in ["c", "d", "e"] {
            send(&mut app, post_event("post-created", id, 1), &db);
        }
        assert_eq!((order(&app), app.shown.as_deref()), (vec!["a", "b", "c", "d", "e"], Some("e")));
        // A version or a deletion the list hasn't seen is listed.
        db.posts.iter_mut().find(|p| p.id == "c").unwrap().version = 2;
        send(&mut app, post_event("post-updated", "c", 2), &db);
        db.posts.retain(|p| p.id != "d");
        send(&mut app, post_event("post-deleted", "d", 1), &db);
        send(&mut app, post_event("post-deleted", "d", 1), &db);
        assert_eq!((order(&app), app.version_of(&"c".into())), (vec!["a", "b", "c", "e"], Some(2)));
        assert_eq!(lists, 3);
    }

    #[test]
    fn a_feed_grown_by_new_posts_matches_one_laid_out_from_scratch() {
        let mut db = tall();
        let mut app = booted(&db);
        app.feed();
        for id in ["e", "f"] {
            db.posts.push(summary(id, "s1", 1));
            send(&mut app, post_event("post-created", id, 1), &db);
            app.feed();
        }
        let grown = std::mem::take(&mut app.feed).unwrap().1;
        let fresh = app.feed();
        assert_eq!((&grown.lines, &grown.hits, &grown.starts, &grown.slots), (&fresh.lines, &fresh.hits, &fresh.starts, &fresh.slots));
        assert_eq!((app.shown.as_deref(), app.cursor), (Some("f"), grown.starts[5]));
    }

    #[test]
    fn new_post_is_followed_when_on_the_latest() {
        let mut db = fake();
        let mut app = booted(&db);
        db.posts.push(summary("c", "s1", 1));
        let fx = app.update(Msg::Event(Event { seq: 11, kind: "post-created".into(), path: None, version: Some(1), id: Some("c".into()), session: Some("s1".into()) }));
        serve(&mut app, fx, &db);
        assert_eq!(app.shown.as_deref(), Some("c"));
        assert!(app.take_fresh().iter().any(|(id, _)| id == "c"));
    }

    #[test]
    fn browsing_writes_only_the_comment() {
        let db = fake();
        let mut app = booted(&db);
        let mut all = Vec::new();
        let keys: Vec<KeyCode> = "jkjkvo/x\x1b:copy\r:help\r:versions\r:rerender\r:open\rc hi\r gGb"
            .chars()
            .map(|c| match c {
                '\r' => KeyCode::Enter,
                '\x1b' => KeyCode::Esc,
                c => KeyCode::Char(c),
            })
            .chain([KeyCode::Up, KeyCode::Down, KeyCode::PageDown, KeyCode::PageUp, KeyCode::Enter])
            .collect();
        for k in keys {
            let fx = app.update(key(k));
            all.extend(requests(&fx));
            serve(&mut app, fx, &db);
        }
        let writes: Vec<_> = all.iter().filter(|r| matches!(r, StageRequest::CommentNew(_))).collect();
        assert_eq!(writes.len(), 1, "{all:?}");
        let StageRequest::CommentNew(a) = writes[0] else { unreachable!() };
        assert_eq!((a.text.as_str(), a.author.as_deref()), (" hi", Some("user")));
    }

    #[test]
    fn rename_and_delete_session_and_post() {
        let mut db = fake();
        let mut app = booted(&db);
        // On post b, d asks about it; anything but y cancels.
        app.update(key(KeyCode::Char('d')));
        assert!(matches!(&app.mode, Mode::Delete { what, .. } if what == "post “b”"), "{:?}", app.mode);
        assert!(requests(&app.update(key(KeyCode::Char('n')))).is_empty());
        app.update(key(KeyCode::Char('d')));
        assert_eq!(requests(&app.update(key(KeyCode::Char('y')))), [StageRequest::PostDelete("b".into())]);
        // With the cursor on one of b's two cards, d deletes that card; with one card, the post.
        let two = Post { parts: vec![Part::Markdown { markdown: "x".into(), id: None }; 2], ..post("b", "s1", 2, "") };
        let r = PostRender { id: "b".into(), version: 2, parts: vec![notes_proto::posts::PartRender::Native; 2] };
        app.loaded.get_mut(&(PostId::from("b"), 2)).unwrap().cards = Some(build_cards(&two, Some(&r)));
        app.loaded.get_mut(&(PostId::from("b"), 2)).unwrap().post = Some(two);
        app.views.get_mut(&PostId::from("b")).unwrap().laid = None;
        app.feed = None;
        let b = app.posts.iter().position(|p| p.id == "b").unwrap();
        app.cursor = app.feed().starts[b] + 2;
        assert_eq!(app.hit_at_cursor(), (Some(b), Hit::Card(0)));
        app.update(key(KeyCode::Char('d')));
        assert_eq!(requests(&app.update(key(KeyCode::Char('y')))), [StageRequest::PartDelete("b".into(), 0)]);
        // r renames the open session, starting from its title.
        app.update(key(KeyCode::Char('r')));
        app.update(key(KeyCode::Backspace));
        app.update(key(KeyCode::Char('x')));
        let rename = SessionRenameArgs { id: "s1".into(), title: "sx".into() };
        assert_eq!(requests(&app.update(key(KeyCode::Enter))), [StageRequest::SessionRename(rename)]);
        // D deletes the selected session; once it's gone the next one opens.
        app.update(key(KeyCode::Char('D')));
        assert_eq!(requests(&app.update(key(KeyCode::Char('y')))), [StageRequest::SessionDelete("s1".into())]);
        assert!(app.toast.as_deref().is_some_and(|t| t.contains("notes session restore s1")), "{:?}", app.toast);
        db.sessions.remove(0);
        let ev = Event { seq: 12, kind: "session-deleted".into(), path: None, version: None, id: Some("s1".into()), session: Some("s1".into()) };
        let fx = app.update(Msg::Event(ev));
        serve(&mut app, fx, &db);
        assert_eq!((app.open.as_deref(), app.shown.as_deref()), (Some("s2"), Some("z")));
        assert_eq!((app.sel, app.posts.len()), (0, 1));
    }

    fn ctrl(c: char) -> Msg {
        Msg::Key(KeyEvent { code: KeyCode::Char(c), modifiers: KeyModifiers::CONTROL, kind: KeyEventKind::Press, state: KeyEventState::NONE })
    }

    /// The text of the line under the cursor.
    fn row(app: &mut App) -> String {
        let line = app.cursor;
        app.feed().lines[line].spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn paragraphs(db: &mut Fake, n: usize) {
        db.md = (0..n).map(|i| format!("row {i}")).collect::<Vec<_>>().join("\n\n");
    }

    #[test]
    fn ctrl_d_and_u_scroll_half_a_page_dragging_the_cursor() {
        let mut db = fake();
        paragraphs(&mut db, 120);
        let mut app = booted(&db);
        app.update(key(KeyCode::Char('g')));
        let half = app.page() / 2;
        assert_eq!((app.scroll, app.cursor), (0, 0));
        app.update(ctrl('d'));
        assert_eq!((app.scroll, app.cursor), (half, half), "viewport and cursor move together");
        app.update(ctrl('d'));
        assert_eq!(app.scroll, 2 * half);
        app.update(ctrl('u'));
        assert_eq!(app.scroll, half);
        assert!(app.cursor >= app.scroll && app.cursor < app.scroll + app.page(), "cursor stays in view");
        assert!(matches!(app.mode, Mode::Nav), "ctrl-d is not the delete key");
    }

    fn diff_post(app: &mut App) {
        let hunk = |n: u32| DiffHunk {
            header: format!("@@ -{n} +{n} @@"),
            lines: (0..12)
                .map(|i| DiffLine { kind: DiffLineKind::Context, old_line: Some(n + i), new_line: Some(n + i), spans: vec![WireSpan { text: format!("ctx {n} {i}"), class: None }] })
                .collect(),
        };
        let file = DiffFileRender { filename: "a.rs".into(), language: None, added: 0, removed: 0, hunks: vec![hunk(1), hunk(100), hunk(200)] };
        let mut post = post("b", "s1", 2, "");
        post.parts = vec![
            Part::Markdown { markdown: "intro".into(), id: None },
            Part::Diff { patch: Some("x".into()), files: vec![], layout: DiffLayout::Unified, id: None },
            Part::Markdown { markdown: "tail\n\n".repeat(40), id: None },
        ];
        let render = PostRender {
            id: "b".into(),
            version: 2,
            parts: vec![PartRender::Native, PartRender::Diff { layout: DiffLayout::Unified, files: vec![file] }, PartRender::Native],
        };
        app.loaded.get_mut(&("b".into(), 2)).unwrap().cards = Some(cards::build_cards(&post, Some(&render)));
        app.views.get_mut(&PostId::from("b")).unwrap().laid = None;
        app.feed = None;
    }

    #[test]
    fn brackets_step_hunks_then_cards_and_comma_period_step_cards() {
        let mut app = booted(&fake());
        diff_post(&mut app);
        app.update(key(KeyCode::Char('g')));
        let at = |app: &mut App| (app.scroll, app.cursor, row(app));
        for want in ["@@ -1 +1 @@", "@@ -100 +100 @@", "@@ -200 +200 @@"] {
            app.update(key(KeyCode::Char(']')));
            let (scroll, cursor, text) = at(&mut app);
            assert!(text.contains(want), "{text}");
            assert_eq!(scroll, cursor, "the stop sits at the top of the viewport");
        }
        app.update(key(KeyCode::Char(']')));
        assert!(at(&mut app).2.contains("markdown"), "no hunk ahead falls back to the next card: {}", at(&mut app).2);
        app.update(key(KeyCode::Char('[')));
        assert!(at(&mut app).2.contains("@@ -200"), "back to the last hunk");
        app.update(key(KeyCode::Char('g')));
        let mut labels = Vec::new();
        for _ in 0..4 {
            app.update(key(KeyCode::Char('.')));
            labels.push(at(&mut app).2);
        }
        assert!(labels[0].contains("markdown") && labels[1].contains("markdown") && labels[2].contains("diff") && labels[3].contains("markdown"), "{labels:?}");
        app.update(key(KeyCode::Char(',')));
        assert!(at(&mut app).2.contains("diff"));
        assert_eq!(app.shown.as_deref(), Some("b"));
    }

    #[test]
    fn search_hit_lands_on_the_matching_line_and_n_steps_matches() {
        let mut db = fake();
        db.md = (0..60).map(|i| if i % 20 == 5 { format!("see the Needle {i}") } else { format!("row {i}") }).collect::<Vec<_>>().join("\n\n");
        let mut app = booted(&db);
        let hit = SessionHit { session: session("s2", 1), post: Some("z".into()), text: None };
        app.mode = Mode::Search { query: "needle".into(), hits: vec![hit], sel: 0 };
        let fx = app.update(key(KeyCode::Enter));
        serve(&mut app, fx, &db);
        app.feed();
        assert_eq!(app.open.as_deref(), Some("s2"));
        assert_eq!(app.scroll, app.cursor, "the match is the top line");
        assert!(row(&mut app).contains("Needle 5"), "{}", row(&mut app));
        assert_eq!(app.toast.as_deref(), Some("match 1/3"));
        app.update(key(KeyCode::Char('n')));
        assert!(row(&mut app).contains("Needle 25"));
        assert_eq!((app.toast.as_deref(), app.scroll), (Some("match 2/3"), app.cursor));
        app.update(key(KeyCode::Char('n')));
        app.update(key(KeyCode::Char('n')));
        assert!(row(&mut app).contains("Needle 5"), "n wraps round");
        assert_eq!(app.toast.as_deref(), Some("match 1/3"));
        app.update(key(KeyCode::Char('N')));
        assert!(row(&mut app).contains("Needle 45"), "N wraps back");
        assert_eq!(app.toast.as_deref(), Some("match 3/3"));
        // The matches are lit in the drawn feed, and esc puts them out.
        let lit = |app: &mut App| {
            let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
            term.draw(|f| ui::draw(f, app)).unwrap();
            term.backend().buffer().content().iter().filter(|c| c.bg == ui::MATCH).map(|c| c.symbol().to_string()).collect::<String>()
        };
        assert_eq!(lit(&mut app), "Needle");
        app.update(key(KeyCode::Esc));
        assert_eq!(lit(&mut app), "");
        app.update(key(KeyCode::Char('n')));
        assert_eq!(app.toast.as_deref(), Some("no matches"));
    }

    fn md_post(db: &Fake) -> App {
        let mut app = booted(db);
        app.update(Msg::Resize(100, 20));
        app
    }

    /// The whole source line behind feed line `at`, whitespace-normalized.
    fn source_at(app: &mut App, at: usize) -> String {
        let feed = app.feed();
        let (post, o) = (feed.hits[at].0, feed.origins[at]);
        (0..feed.lines.len())
            .filter(|&i| feed.hits[i].0 == post && (feed.origins[i].card, feed.origins[i].src) == (o.card, o.src))
            .map(|i| feed.lines[i].to_string())
            .collect::<String>()
            .replace(['│', '↪'], " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn resize_keeps_the_same_source_line_at_the_viewport_top() {
        let mut db = fake();
        db.md = (0..40).map(|i| format!("para {i} with plenty of words so that this paragraph wraps differently at every width\n\n")).collect();
        let mut app = md_post(&db);
        scroll_to(&mut app, 30);
        let top = app.scroll;
        let want = source_at(&mut app, top);
        assert!(want.starts_with("para "), "{want}");
        for (w, h) in [(60, 20), (40, 20), (150, 20), (100, 20), (35, 12)] {
            app.update(Msg::Resize(w, h));
            app.feed();
            let top = app.scroll;
            assert_eq!(source_at(&mut app, top), want, "after resizing to {w}x{h}");
        }
    }

    #[test]
    fn resize_from_a_wrapped_row_stays_in_the_same_paragraph() {
        let mut db = fake();
        db.md = (0..20).map(|i| format!("para {i} {}\n\n", "word ".repeat(60))).collect();
        let mut app = md_post(&db);
        app.update(Msg::Resize(40, 20));
        let first = (0..app.feed().lines.len()).find(|&i| app.feed().origins[i].wrap == 2).unwrap();
        app.scroll = first;
        app.cursor = first;
        let want = source_at(&mut app, first);
        app.update(Msg::Resize(150, 20));
        app.feed();
        let top = app.scroll;
        assert_eq!(source_at(&mut app, top), want);
    }

    #[test]
    fn folding_a_card_above_leaves_the_top_line_unchanged() {
        let mut app = booted(&fake());
        app.update(Msg::Resize(100, 20));
        let id = PostId::from("b");
        let data: serde_json::Value = (0..30).map(|i| (format!("k{i}"), serde_json::json!(i))).collect::<serde_json::Map<_, _>>().into();
        let p = Post {
            parts: vec![
                Part::Json { data, id: None },
                Part::Markdown { markdown: (0..40).map(|i| format!("para {i}\n\n")).collect(), id: None },
            ],
            ..post("b", "s1", 2, "")
        };
        let r = PostRender { id: id.clone(), version: 2, parts: vec![notes_proto::posts::PartRender::Native; 2] };
        app.loaded.get_mut(&(id.clone(), 2)).unwrap().cards = Some(build_cards(&p, Some(&r)));
        app.views.get_mut(&id).unwrap().laid = None;
        app.feed = None;
        let b = app.posts.iter().position(|post| post.id == id).unwrap();
        let (node_line, node) = {
            let feed = app.feed();
            let line = (feed.starts[b]..feed.lines.len()).find(|&i| matches!(feed.hits[i].1, Hit::Node(..))).unwrap();
            (line, feed.hits[line].1)
        };
        let to = app.feed().starts[b] as isize + 46 - app.scroll as isize;
        app.scroll(to);
        let top = app.scroll;
        assert!(top > node_line + 30, "scrolled past the json card");
        let want = source_at(&mut app, top);
        assert!(want.starts_with("para "), "{want:?}");
        let before = app.feed().lines.len();
        assert!(app.toggle((Some(b), node)));
        assert!(app.feed().lines.len() < before - 20, "the fold removed rows above");
        let top = app.scroll;
        assert_eq!(source_at(&mut app, top), want);
    }

    fn scroll_to(app: &mut App, line: usize) {
        let delta = line as isize - app.scroll as isize;
        app.scroll(delta);
    }

    fn bar_text(app: &mut App) -> String {
        use ratatui::{backend::TestBackend, Terminal};
        let (w, h) = app.size;
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| ui::draw(f, app)).unwrap();
        let y = ui::geometry(app.size).main.y;
        let buf = term.backend().buffer();
        (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()
    }

    #[test]
    fn sticky_title_names_the_post_at_the_top_of_the_viewport_not_the_cursor() {
        let mut db = fake();
        db.md = (0..30).map(|i| format!("para {i}\n\n")).collect();
        let mut app = md_post(&db);
        let (a, b) = (app.feed().starts[0], app.feed().starts[1]);
        assert!(bar_text(&mut app).trim().is_empty() || !bar_text(&mut app).contains("a  v1"), "header on screen: nothing sticky");
        scroll_to(&mut app, a + 20);
        assert!(app.scroll > a && app.scroll < b);
        assert!(bar_text(&mut app).contains(" a  v1"), "{}", bar_text(&mut app));
        // The cursor is in the next post while the viewport top is still in the first.
        app.cursor = b + 2;
        assert!(bar_text(&mut app).contains(" a  v1"));
        scroll_to(&mut app, b + 8);
        assert!(bar_text(&mut app).contains(" b  v2"), "{}", bar_text(&mut app));
    }

    #[test]
    fn sticky_title_adds_the_diff_file_at_the_viewport_top() {
        use notes_proto::posts::{DiffFileRender, DiffHunk, DiffLayout, DiffLine, DiffLineKind, Span as WireSpan};
        let mut app = booted(&fake());
        app.update(Msg::Resize(100, 20));
        let id = PostId::from("b");
        let file = |name: &str| DiffFileRender {
            filename: name.into(),
            language: None,
            added: 20,
            removed: 0,
            hunks: vec![DiffHunk {
                header: "@@ -0,0 +1,20 @@".into(),
                lines: (1..=20)
                    .map(|n| DiffLine { kind: DiffLineKind::Added, old_line: None, new_line: Some(n), spans: vec![WireSpan { text: format!("line {n}"), class: None }] })
                    .collect(),
            }],
        };
        let p = Post {
            parts: vec![Part::Diff { patch: Some("x".into()), files: vec![], layout: DiffLayout::Unified, id: None }],
            ..post("b", "s1", 2, "")
        };
        let r = PostRender {
            id: id.clone(),
            version: 2,
            parts: vec![notes_proto::posts::PartRender::Diff { layout: DiffLayout::Unified, files: vec![file("src/one.rs"), file("src/two.rs")] }],
        };
        app.loaded.get_mut(&(id.clone(), 2)).unwrap().cards = Some(build_cards(&p, Some(&r)));
        app.views.get_mut(&id).unwrap().laid = None;
        app.feed = None;
        let b = app.posts.iter().position(|post| post.id == id).unwrap();
        let start = app.feed().starts[b];
        scroll_to(&mut app, start + 8);
        assert!(bar_text(&mut app).contains("b › src/one.rs"), "{}", bar_text(&mut app));
        scroll_to(&mut app, start + 8 + 20);
        assert!(bar_text(&mut app).contains("b › src/two.rs"), "{}", bar_text(&mut app));
    }

    #[test]
    fn a_render_landing_under_the_viewport_top_keeps_the_same_diff_line_there() {
        let mut app = booted(&fake());
        app.update(Msg::Resize(100, 20));
        let id = PostId::from("b");
        let body = |name: &str| (0..12).map(|i| format!(" {name} {i}")).collect::<Vec<_>>().join("\n");
        let patch = format!(
            "diff --git a/one.rs b/one.rs\n--- a/one.rs\n+++ b/one.rs\n@@ -1,12 +1,12 @@\n{}\ndiff --git a/two.rs b/two.rs\n--- a/two.rs\n+++ b/two.rs\n@@ -1,12 +1,12 @@\n{}\n",
            body("one"),
            body("two")
        );
        let tail = Part::Markdown { markdown: "tail\n\n".repeat(80), id: None };
        let p = Post { parts: vec![Part::Diff { patch: Some(patch), files: vec![], layout: DiffLayout::Unified, id: None }, tail], ..post("b", "s1", 2, "") };
        let file = |name: &str| DiffFileRender {
            filename: format!("{name}.rs"),
            language: None,
            added: 0,
            removed: 0,
            hunks: vec![DiffHunk {
                header: "@@ -1,12 +1,12 @@".into(),
                lines: (0..12)
                    .map(|i| DiffLine { kind: DiffLineKind::Context, old_line: Some(i + 1), new_line: Some(i + 1), spans: vec![WireSpan { text: format!("{name} {i}"), class: None }] })
                    .collect(),
            }],
        };
        let render = PostRender { id: id.clone(), version: 2, parts: vec![PartRender::Diff { layout: DiffLayout::Unified, files: vec![file("one"), file("two")] }, PartRender::Native] };
        app.loaded.insert((id.clone(), 2), Loaded { post: Some(p), render: None, cards: None });
        app.built((id.clone(), 2));
        let b = app.posts.iter().position(|post| post.id == id).unwrap();
        let line = |app: &mut App, text: &str| {
            let feed = app.feed();
            (feed.starts[b]..feed.lines.len()).find(|&i| feed.lines[i].to_string().contains(text)).unwrap()
        };
        let at = line(&mut app, "two 5");
        scroll_to(&mut app, at);
        assert_eq!(app.scroll, at);
        assert!(bar_text(&mut app).contains("b › two.rs"), "the raw patch names its file: {}", bar_text(&mut app));
        app.update(Msg::Reply(Reply::Render(render)));
        app.feed();
        let top = app.scroll;
        let row = app.feed().lines[top].to_string();
        assert!(row.contains(" 6  6   two 5"), "the styled row for the same diff line is at the top: {row}");
        assert_ne!(top, at, "the styled diff is shorter, so the line moved in the feed");
        assert!(bar_text(&mut app).contains("b › two.rs"), "{}", bar_text(&mut app));
    }

    #[test]
    fn an_updated_post_keeps_the_reader_on_the_same_line_until_and_after_its_new_version_lands() {
        let mut db = fake();
        db.md = (0..40).map(|i| format!("para {i}\n\n")).collect();
        let mut app = md_post(&db);
        let b = app.posts.iter().position(|p| p.id == "b").unwrap();
        let start = app.feed().starts[b];
        scroll_to(&mut app, start + 30);
        let top = app.scroll;
        let (want_line, want_src) = (app.feed().lines[top].to_string(), source_at(&mut app, top));
        assert!(want_src.starts_with("para "), "{want_src}");
        db.posts[1].version = 3;
        db.md.push_str("one more para\n\n");
        let fx = app.update(Msg::Event(Event { seq: 9, kind: "post-updated".into(), path: None, version: Some(3), id: Some("b".into()), session: Some("s1".into()) }));
        let reqs = requests(&fx);
        let list = reqs.iter().find(|r| matches!(r, StageRequest::PostList(_))).unwrap();
        app.update(Msg::Reply(db.answer(list)));
        app.feed();
        let top = app.scroll;
        assert_eq!(app.feed().lines[top].to_string(), want_line, "the old version stays up while the new one loads");
        let show = reqs.iter().find(|r| matches!(r, StageRequest::PostShow(_))).unwrap();
        app.update(Msg::Reply(db.answer(show)));
        app.feed();
        let top = app.scroll;
        assert_eq!(source_at(&mut app, top), want_src, "the new version's same line is at the top");
        assert!(bar_text(&mut app).contains("b  v3"), "{}", bar_text(&mut app));
    }
}
