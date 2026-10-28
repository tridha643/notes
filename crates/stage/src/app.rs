//! All of stage's state and every decision, with no IO: `App::update` takes a
//! message (a key, a mouse event, a notesd event or a reply) and returns the
//! effects for `main` to perform. Requests are limited to [`StageRequest`], whose
//! writes are comments, session renames and deletes the user confirmed.

use std::collections::{HashMap, HashSet};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use notes_proto::{
    posts::{
        Comment, CommentNewArgs, Part, Post, PostId, PostRender, PostShowArgs, PostSummary, Session,
        SessionHit, SessionId, SessionRenameArgs, SessionSearchArgs,
    },
    ErrorBody, ErrorCode, Event,
};

use ratatui::text::{Line, Span};

use crate::{
    cards::{self, build_cards, Card, CardBody, Fit, Folds, Hit, Laid, Slot, VisualSource},
    commands, ui,
};

/// The open session's posts as one stream, oldest first, the way sideshow's
/// viewer shows a session: each post's title, then its part cards and comments.
#[derive(Default)]
pub struct Feed {
    pub lines: Vec<Line<'static>>,
    /// Per line: the post (index into `App::posts`) and what in it.
    pub hits: Vec<(usize, Hit)>,
    pub slots: Vec<(VisualKey, Slot)>,
    /// First line of each post, by index into `App::posts`.
    pub starts: Vec<usize>,
}

/// Everything the feed reads: per post (id, version, fold generation, built),
/// width, page height, cell px, and the comment and visual generations.
type FeedKey = (Vec<(PostId, u64, u64, bool)>, u16, u16, (u16, u16), u64, u64);

/// One visual card: (post, version, card index).
pub type VisualKey = (PostId, u64, usize);

#[derive(Debug, Clone, PartialEq)]
pub enum VisualState {
    Pending,
    /// Pixels exist at `w`×`h`; `max_scale` caps upscaling (1 for photos,
    /// 2 for vector diagrams rendered at 2× CSS px).
    Ready { w: u32, h: u32, max_scale: f32 },
    Failed(String),
}

/// Pixel budget for a visual: the card's inner width and half the visible height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Target {
    pub width_px: u32,
    pub max_height_px: u32,
}

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
    /// Record the card's text width for agents (`STAGE_CARD_COLS`).
    CardCols(u16),
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
    NeedVisual { key: VisualKey, source: VisualSource, target: Target },
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

pub struct Loaded {
    pub post: Option<Post>,
    pub render: Option<PostRender>,
    pub cards: Option<Vec<Card>>,
}

/// Everything a layout reads: version, width, height, cell px, comment, fold and visual generations.
type LaidKey = (u64, u16, u16, (u16, u16), u64, u64, u64);

/// Per post: folds, a pinned version, and the post's own layout cache.
#[derive(Default)]
pub struct View {
    pub folds: Folds,
    /// A version picked with `v`; `None` follows the latest.
    pub pinned: Option<u64>,
    laid: Option<(LaidKey, Laid)>,
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
    pub views: HashMap<PostId, View>,
    pub shown: Option<PostId>,
    /// The sidebar's selected session (index into `sessions`).
    pub sel: usize,
    pub mode: Mode,
    pub size: (u16, u16),
    /// The card width last sent to notesd, so a resize only writes on change.
    card_cols: u16,
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
    /// Card builds, one per (post, version); tests assert there are no repeats.
    pub builds: usize,
    /// Bumped on every comment-list reply, so the layout cache sees new comments.
    comments_gen: u64,
    pub visuals: HashMap<VisualKey, VisualState>,
    /// Bumped when a visual's state changes, so the layout cache re-sizes it.
    visual_gen: u64,
    /// Visuals drawn this frame (set by `ui::draw`), for `main` to place.
    pub on_screen: Vec<OnScreen>,
    /// Feed scroll (first line on screen) and cursor line.
    pub scroll: usize,
    pub cursor: usize,
    feed: Option<(FeedKey, Feed)>,
    /// A post to bring to the top on the next layout (a sidebar pick, a follow).
    jump: Option<PostId>,
    /// A mouse drag in the feed: where it started and where it is, as (line, cell column).
    pub drag: Option<((usize, u16), (usize, u16))>,
    /// Sessions opened before: their posts, comments and place in the feed, so
    /// coming back shows them at once (then refreshes) instead of a blank pane.
    remembered: HashMap<SessionId, Remembered>,
}

struct Remembered {
    posts: Vec<PostSummary>,
    comments: Vec<Comment>,
    scroll: usize,
    cursor: usize,
    shown: Option<PostId>,
    follow: bool,
}

const WHEEL: usize = 3;

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
            views: HashMap::new(),
            shown: None,
            sel: 0,
            mode: Mode::Nav,
            size: (80, 24),
            card_cols: 0,
            cell_px: (8, 16),
            kitty: false,
            toast: None,
            banner: None,
            want,
            follow: true,
            fresh: Vec::new(),
            seen_rows: HashSet::new(),
            builds: 0,
            comments_gen: 0,
            visuals: HashMap::new(),
            visual_gen: 0,
            on_screen: Vec::new(),
            scroll: 0,
            cursor: 0,
            feed: None,
            jump: None,
            remembered: HashMap::new(),
            drag: None,
        }
    }

    /// The first requests: who's there. The watch is already open (main opens it first).
    pub fn start(&self) -> Vec<Effect> {
        vec![Effect::Request(StageRequest::SessionList)]
    }

    pub fn update(&mut self, msg: Msg) -> Vec<Effect> {
        let mut fx = self.step(msg);
        fx.extend(self.request_visuals());
        fx
    }

    /// Pixel budget for visual cards at the current pane size.
    pub fn target(&self) -> Target {
        let inner = ui::geometry(self.size).main.width.saturating_sub(5).max(1) as u32;
        Target {
            width_px: inner * self.cell_px.0 as u32,
            max_height_px: (self.page() as u32 / 2).max(1) * self.cell_px.1 as u32,
        }
    }

    /// Ask once for the pixels of each visual card in the open session's posts.
    fn request_visuals(&mut self) -> Vec<Effect> {
        let target = self.target();
        let mut fx = Vec::new();
        let posts: Vec<(PostId, u64)> =
            self.posts.iter().filter_map(|p| Some((p.id.clone(), self.version_of(&p.id)?))).collect();
        for (id, version) in posts {
            let Some(cards) = self.loaded.get(&(id.clone(), version)).and_then(|l| l.cards.as_ref()) else { continue };
            for (c, card) in cards.iter().enumerate() {
                if let CardBody::Visual { source, .. } = &card.body {
                    let key = (id.clone(), version, c);
                    if !self.visuals.contains_key(&key) {
                        self.visuals.insert(key.clone(), VisualState::Pending);
                        fx.push(Effect::NeedVisual { key, source: source.clone(), target });
                    }
                }
            }
        }
        fx
    }

    fn step(&mut self, msg: Msg) -> Vec<Effect> {
        match msg {
            Msg::Key(k) if k.kind != KeyEventKind::Release => {
                self.toast = None;
                let mut fx = self.key(k);
                fx.push(Effect::Redraw);
                fx
            }
            Msg::Key(_) => vec![],
            Msg::Mouse(m) => self.mouse(m),
            Msg::Resize(w, h) => {
                self.size = (w, h);
                let mut fx = vec![Effect::Redraw];
                // A card's text width: the feed column less its border and padding.
                let cols = ui::geometry(self.size).main.width.saturating_sub(5);
                if cols != self.card_cols {
                    self.card_cols = cols;
                    fx.push(Effect::Request(StageRequest::CardCols(cols)));
                }
                fx
            }
            Msg::CellPx(px) => {
                if px == self.cell_px {
                    return vec![];
                }
                self.cell_px = px;
                vec![Effect::Redraw]
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
                self.visuals.insert(key, VisualState::Ready { w, h, max_scale });
                self.visual_gen += 1;
                vec![Effect::Redraw]
            }
            Msg::VisualFailed { key, reason } => {
                self.visuals.insert(key, VisualState::Failed(reason));
                self.visual_gen += 1;
                vec![Effect::Redraw]
            }
            Msg::Gfx(_) => vec![],
        }
    }

    // ---- Sidebar ----------------------------------------------------------------

    fn open_session(&mut self, id: SessionId) -> Vec<Effect> {
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
            };
            self.remembered.insert(old, r);
        }
        self.open = Some(id.clone());
        self.feed = None;
        self.jump = None;
        match self.remembered.remove(&id) {
            Some(r) => {
                (self.posts, self.comments, self.scroll, self.cursor, self.shown, self.follow) =
                    (r.posts, r.comments, r.scroll, r.cursor, r.shown, r.follow);
                self.comments_gen += 1;
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
        let (cursor, page) = (self.cursor, self.page());
        let feed = self.feed();
        let Some(&(at, _)) = feed.hits.get(cursor) else { return };
        let target = match (forward, cursor > feed.starts[at]) {
            (true, _) => at + 1,
            (false, true) => at,
            (false, false) => at.saturating_sub(1),
        };
        let Some(&line) = feed.starts.get(target) else { return };
        let max = feed.lines.len().saturating_sub(page);
        self.follow = target + 1 == self.posts.len();
        (self.cursor, self.scroll) = (line, line.min(max));
        self.follow_cursor();
    }

    // ---- Posts ------------------------------------------------------------------

    fn version_of(&self, id: &PostId) -> Option<u64> {
        let latest = self.posts.iter().find(|p| &p.id == id).map(|p| p.version)?;
        Some(self.views.get(id).and_then(|v| v.pinned).unwrap_or(latest))
    }

    fn show(&mut self, id: PostId) -> Vec<Effect> {
        self.unseen.remove(&id);
        self.shown = Some(id.clone());
        self.views.entry(id.clone()).or_default();
        match self.version_of(&id) {
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

    fn built(&mut self, key: (PostId, u64)) {
        if let Some(l) = self.loaded.get_mut(&key) {
            if let (Some(post), Some(render), None) = (&l.post, &l.render, &l.cards) {
                l.cards = Some(build_cards(post, render));
                self.builds += 1;
            }
        }
    }

    /// One post laid out at `width`, memoized in its view on everything it reads.
    fn post_laid(&mut self, id: &PostId, version: u64, width: u16, page: u16) -> Option<&Laid> {
        let comments = self.comments_on(id);
        let cards = self.loaded.get(&(id.clone(), version))?.cards.as_ref()?;
        let (cell, visuals) = (self.cell_px, &self.visuals);
        let view = self.views.entry(id.clone()).or_default();
        let key = (version, width, page, cell, self.comments_gen, view.fold_gen, self.visual_gen);
        if view.laid.as_ref().is_none_or(|(k, _)| *k != key) {
            let inner = width.saturating_sub(4).max(1);
            let fit = |c: usize| match visuals.get(&(id.clone(), version, c)) {
                Some(VisualState::Ready { w, h, max_scale }) => fit_cells((*w, *h), *max_scale, inner, page / 2, cell),
                Some(VisualState::Failed(why)) => Fit::Failed(why.clone()),
                // Hold about the room a diagram or page usually takes.
                Some(VisualState::Pending) | None => Fit::Pending { rows: (page / 3).clamp(3, 12) },
            };
            view.laid = Some((key, cards::layout(cards, &comments, width, &view.folds, &fit)));
        }
        view.laid.as_ref().map(|(_, l)| l)
    }

    /// The feed for the main column, rebuilt only when something it reads changed.
    /// A rebuild (a post loading above, a resize rewrapping everything, a fold)
    /// keeps the line under the cursor where it was on screen.
    pub fn feed(&mut self) -> &Feed {
        let width = ui::geometry(self.size).main.width.saturating_sub(1);
        let page = self.page() as u16;
        let posts: Vec<(PostId, u64, u64, bool)> = self
            .posts
            .iter()
            .filter_map(|p| {
                let v = self.version_of(&p.id)?;
                let fold_gen = self.views.get(&p.id).map_or(0, |v| v.fold_gen);
                let built = self.loaded.get(&(p.id.clone(), v)).is_some_and(|l| l.cards.is_some());
                Some((p.id.clone(), v, fold_gen, built))
            })
            .collect();
        let key: FeedKey = (posts.clone(), width, page, self.cell_px, self.comments_gen, self.visual_gen);
        if self.feed.as_ref().is_none_or(|(k, _)| *k != key) {
            // Where the cursor was: which post, how far into it, how far down the screen.
            let anchor = self.feed.as_ref().and_then(|(_, f)| {
                let (post, _) = *f.hits.get(self.cursor)?;
                Some((self.posts.get(post)?.id.clone(), self.cursor - f.starts[post], self.cursor.saturating_sub(self.scroll)))
            });
            let mut feed = Feed::default();
            for (i, (id, version, _, _)) in posts.iter().enumerate() {
                feed.starts.push(feed.lines.len());
                feed.lines.push(self.header(i));
                feed.hits.push((i, Hit::None));
                let start = feed.lines.len();
                match self.post_laid(id, *version, width, page) {
                    Some(laid) => {
                        feed.lines.extend(laid.lines.iter().cloned());
                        feed.hits.extend(laid.hits.iter().map(|h| (i, *h)));
                        feed.slots.extend(laid.slots.iter().map(|s| {
                            ((id.clone(), *version, s.card), Slot { line: s.line + start, ..*s })
                        }));
                    }
                    None => {
                        feed.lines.push(Line::styled("  loading…", ui::dim()));
                        feed.hits.push((i, Hit::None));
                        feed.lines.push(Line::default());
                        feed.hits.push((i, Hit::None));
                    }
                }
            }
            let total = feed.lines.len();
            if let Some(id) = self.jump.take() {
                if let Some(i) = self.posts.iter().position(|p| p.id == id) {
                    self.cursor = feed.starts[i];
                    self.scroll = self.cursor;
                }
            } else if let Some((id, offset, row)) = anchor {
                if let Some(i) = self.posts.iter().position(|p| p.id == id) {
                    let end = feed.starts.get(i + 1).copied().unwrap_or(total);
                    self.cursor = (feed.starts[i] + offset).min(end.saturating_sub(1));
                    self.scroll = self.cursor.saturating_sub(row);
                }
            }
            self.cursor = self.cursor.min(total.saturating_sub(1));
            self.scroll = self.scroll.min(total.saturating_sub(page as usize)).min(self.cursor);
            self.feed = Some((key, feed));
        }
        &self.feed.as_ref().expect("built above").1
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
        ui::geometry(self.size).main.height.saturating_sub(1).max(1) as usize
    }

    /// The shown post follows the cursor through the feed.
    fn follow_cursor(&mut self) {
        let cursor = self.cursor;
        let post = self.feed().hits.get(cursor).map(|(p, _)| *p);
        if let Some(id) = post.and_then(|p| self.posts.get(p)).map(|p| p.id.clone()) {
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

    /// Scroll the feed by `delta` lines, dragging the cursor along.
    fn scroll(&mut self, delta: isize) {
        let total = self.feed().lines.len();
        let page = self.page();
        let max = total.saturating_sub(page);
        self.scroll = (self.scroll as isize + delta).clamp(0, max as isize) as usize;
        self.cursor = self.cursor.clamp(self.scroll, (self.scroll + page).saturating_sub(1).max(self.scroll));
        self.follow_cursor();
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
        self.toast = Some(format!("v{} of {latest}", view.pinned.unwrap_or(latest)));
        let v = view.pinned.unwrap_or(latest);
        self.load(id, v)
    }

    // ---- Input ------------------------------------------------------------------

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
                    self.toast = Some(format!("deleted {what}"));
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
        let page = self.page() as isize;
        match k.code {
            KeyCode::Char('q') => vec![Effect::Quit],
            KeyCode::Char('j') => {
                self.step_post(true);
                vec![]
            }
            KeyCode::Char('k') => {
                self.step_post(false);
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
                self.cursor(0);
                vec![]
            }
            KeyCode::PageUp | KeyCode::Char('b') => {
                self.scroll(-page);
                self.cursor(0);
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

    fn mouse(&mut self, m: MouseEvent) -> Vec<Effect> {
        let g = ui::geometry(self.size);
        let inside = |r: ratatui::layout::Rect| {
            m.column >= r.x && m.column < r.x + r.width && m.row >= r.y && m.row < r.y + r.height
        };
        let fx = match m.kind {
            MouseEventKind::ScrollDown => {
                self.scroll(WHEEL as isize);
                vec![]
            }
            MouseEventKind::ScrollUp => {
                self.scroll(-(WHEEL as isize));
                vec![]
            }
            MouseEventKind::Down(MouseButton::Left) if inside(g.sidebar) => {
                let row = (m.row - g.sidebar.y) as usize + ui::sidebar_offset(self, g.sidebar.height);
                self.select_session(row, true)
            }
            // In the feed: press starts a selection, drag extends it, release
            // copies it; a press and release in one place is a click.
            MouseEventKind::Down(MouseButton::Left) if inside(g.main) && m.row > g.main.y => {
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
        let g = ui::geometry(self.size);
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
            "session-created" | "session-updated" => fx.push(Effect::Request(StageRequest::SessionList)),
            "session-deleted" => {
                if mine {
                    self.banner = Some("session ended".into());
                }
                fx.push(Effect::Request(StageRequest::SessionList));
            }
            "post-created" | "post-updated" | "post-deleted" if mine => {
                let id = PostId::from(e.id.clone().unwrap_or_default());
                fx.push(Effect::Request(StageRequest::PostList(self.open.clone().unwrap())));
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
        match r {
            Reply::Saved => return fx,
            Reply::Sessions(sessions) => {
                let selected = self.sessions.get(self.sel).map(|s| s.id.clone());
                self.sessions = sessions;
                self.remembered.retain(|id, _| self.sessions.iter().any(|s| &s.id == id));
                // The open session was deleted: drop it and open the newest.
                if self.open.as_ref().is_some_and(|id| !self.sessions.iter().any(|s| &s.id == id)) {
                    (self.open, self.shown, self.feed) = (None, None, None);
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
                for p in &posts {
                    if self.seen_rows.insert((p.id.clone(), p.version)) {
                        self.fresh.push((p.id.clone(), p.version));
                        if self.shown.as_ref() != Some(&p.id) && !self.posts.is_empty() {
                            self.unseen.insert(p.id.clone());
                        }
                    }
                }
                self.posts = posts;
                let target = match &self.shown {
                    Some(id) if !self.follow && self.posts.iter().any(|p| &p.id == id) => Some(id.clone()),
                    Some(id) if !self.follow => {
                        // The shown post was deleted: select its neighbour.
                        self.toast = Some(format!("post {id} was deleted"));
                        self.posts.last().map(|p| p.id.clone())
                    }
                    _ => self.posts.last().map(|p| p.id.clone()),
                };
                // The feed shows every post, so load them all.
                for p in self.posts.clone() {
                    if let Some(v) = self.version_of(&p.id) {
                        fx.extend(self.load(p.id.clone(), v));
                    }
                }
                if let Some(id) = target {
                    if self.follow {
                        self.jump = Some(id.clone());
                    }
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
                }
                self.built(key);
            }
            Reply::Comments(session, comments) => {
                if self.open.as_ref() == Some(&session) {
                    self.comments = comments;
                    self.comments_gen += 1;
                }
            }
            Reply::Commented(c) => {
                let agent = self.open.as_ref().and_then(|id| self.sessions.iter().find(|s| &s.id == id)).map_or("the agent".into(), |s| s.agent.clone());
                let on = c.post.as_ref().and_then(|p| self.posts.iter().find(|x| &x.id == p)).map_or(String::new(), |p| format!(" on {}", p.title));
                self.toast = Some(format!("sent{on} · {agent} sees it with its next post or wait"));
            }
            Reply::Failed(req, e) => {
                if let StageRequest::PostShow(a) | StageRequest::PostRender(a) = &req {
                    if let Some(v) = a.version {
                        self.loaded.remove(&(a.id.clone(), v));
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
        rows: ((h as f32 * scale / ch as f32).ceil() as u16).max(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventState;
    use notes_proto::posts::Part;

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
                StageRequest::CardCols(_)
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
    fn wheel_scrolls_and_click_folds() {
        let mut db = fake();
        db.md = (0..80).map(|i| format!("line {i}\n\n")).collect();
        let mut app = booted(&db);
        let wheel = |kind| Msg::Mouse(MouseEvent { kind, column: 60, row: 10, modifiers: KeyModifiers::NONE });
        app.update(wheel(MouseEventKind::ScrollUp));
        let top = app.scroll;
        app.update(wheel(MouseEventKind::ScrollDown));
        assert_eq!(app.scroll, top + WHEEL);
        app.update(wheel(MouseEventKind::ScrollUp));
        assert_eq!(app.scroll, top);
        // Motion doesn't redraw.
        assert!(app.update(wheel(MouseEventKind::Moved)).is_empty());

        // A json post: click its root node to fold it.
        let key = (PostId::from("b"), 2);
        let data = serde_json::json!({"a": 1, "b": 2});
        let p = Post { parts: vec![Part::Json { data, id: None }], ..post("b", "s1", 2, "") };
        let r = PostRender { id: "b".into(), version: 2, parts: vec![notes_proto::posts::PartRender::Native] };
        app.loaded.get_mut(&key).unwrap().cards = Some(build_cards(&p, &r));
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
    /// panics, no row runs past the pane, and the feed keeps the line under
    /// the cursor in place across a resize.
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
        let before = (app.shown.clone(), app.cursor - app.scroll);
        app.update(Msg::Resize(70, 30));
        let _ = app.feed();
        assert_eq!(app.shown, before.0);
        assert_eq!(app.cursor - app.scroll, before.1, "the cursor's row on screen survives a rewrap");
    }

    #[test]
    fn resize_is_exactly_one_redraw() {
        let mut app = booted(&fake());
        // A new card width is reported to notesd once; a height change only redraws.
        let cols = ui::geometry((120, 40)).main.width - 5;
        assert_eq!(app.update(Msg::Resize(120, 40)), [Effect::Redraw, Effect::Request(StageRequest::CardCols(cols))]);
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
        // A new version builds once, even when its event repeats.
        db.posts[1].version = 3;
        for _ in 0..2 {
            let fx = app.update(ev("post-updated", "b", 3));
            serve(&mut app, fx, &db);
        }
        assert_eq!(app.builds, builds + 1);
        // Another session's post marks it unread and builds nothing.
        let fx = app.update(Msg::Event(Event { seq: 10, kind: "post-created".into(), path: None, version: Some(1), id: Some("z".into()), session: Some("s2".into()) }));
        serve(&mut app, fx, &db);
        assert!(app.unread.contains("s2"));
        assert_eq!(app.builds, builds + 1);
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
        app.loaded.get_mut(&(PostId::from("b"), 2)).unwrap().cards = Some(build_cards(&two, &r));
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
        db.sessions.remove(0);
        let ev = Event { seq: 12, kind: "session-deleted".into(), path: None, version: None, id: Some("s1".into()), session: Some("s1".into()) };
        let fx = app.update(Msg::Event(ev));
        serve(&mut app, fx, &db);
        assert_eq!((app.open.as_deref(), app.shown.as_deref()), (Some("s2"), Some("z")));
        assert_eq!((app.sel, app.posts.len()), (0, 1));
    }
}
