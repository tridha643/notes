//! All of stage's state and every decision, with no IO: `App::update` takes a
//! message (a key, a mouse event, a notesd event or a reply) and returns the
//! effects for `main` to perform. Requests are limited to [`StageRequest`], whose
//! only write is `CommentNew`.

use std::collections::{HashMap, HashSet};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use notes_proto::{
    posts::{
        Comment, CommentNewArgs, Part, Post, PostId, PostRender, PostShowArgs, PostSummary, Session,
        SessionHit, SessionId, SessionSearchArgs,
    },
    ErrorBody, ErrorCode, Event,
};

use crate::{
    cards::{self, build_cards, Card, Folds, Hit, Laid},
    commands, ui,
};

/// The only requests stage can make. `CommentNew` is its only write.
#[derive(Debug, Clone, PartialEq)]
pub enum StageRequest {
    SessionList,
    SessionSearch(SessionSearchArgs),
    PostList(SessionId),
    PostShow(PostShowArgs),
    PostRender(PostShowArgs),
    CommentNew(CommentNewArgs),
    CommentList(SessionId),
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
    Failed(StageRequest, ErrorBody),
}

#[derive(Debug)]
pub enum Msg {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize(u16, u16),
    Event(Event),
    Reply(Reply),
    /// The watch stream dropped; it reconnects on its own with `since = last_seq`.
    WatchLost,
    WatchBack,
    /// Terminal probe replies (cell size in px, kitty graphics support).
    Probed { cell_px: Option<(u16, u16)>, kitty: bool },
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Nav,
    Search { query: String, hits: Vec<SessionHit>, sel: usize },
    Comment { input: String },
    Command { input: String },
}

/// A sidebar row: a session, or one of the open session's posts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Session(usize),
    Post(usize),
}

pub struct Loaded {
    pub post: Option<Post>,
    pub render: Option<PostRender>,
    pub cards: Option<Vec<Card>>,
}

#[derive(Default)]
pub struct View {
    pub scroll: usize,
    pub cursor: usize,
    pub folds: Folds,
    /// A version picked with `v`; `None` follows the latest.
    pub pinned: Option<u64>,
    laid: Option<((u64, u16, u64, u64), Laid)>,
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
    pub sel: usize,
    pub mode: Mode,
    pub size: (u16, u16),
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
        }
    }

    /// The first requests: who's there. The watch is already open (main opens it first).
    pub fn start(&self) -> Vec<Effect> {
        vec![Effect::Request(StageRequest::SessionList)]
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
            Msg::Mouse(m) => self.mouse(m),
            Msg::Resize(w, h) => {
                self.size = (w, h);
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
                vec![]
            }
        }
    }

    // ---- Sidebar ----------------------------------------------------------------

    pub fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for (i, s) in self.sessions.iter().enumerate() {
            rows.push(Row::Session(i));
            if self.open.as_ref() == Some(&s.id) {
                rows.extend((0..self.posts.len()).map(Row::Post));
            }
        }
        rows
    }

    fn row_of_post(&self, id: &PostId) -> Option<usize> {
        let i = self.posts.iter().position(|p| &p.id == id)?;
        self.rows().iter().position(|r| *r == Row::Post(i))
    }

    fn open_session(&mut self, id: SessionId) -> Vec<Effect> {
        self.unread.remove(&id);
        if self.open.as_ref() == Some(&id) {
            return vec![];
        }
        self.open = Some(id.clone());
        self.posts.clear();
        self.comments.clear();
        self.shown = None;
        self.follow = true;
        self.banner = None;
        if let Some(i) = self.sessions.iter().position(|s| s.id == id) {
            self.sel = self.rows().iter().position(|r| *r == Row::Session(i)).unwrap_or(0);
        }
        vec![
            Effect::Request(StageRequest::PostList(id.clone())),
            Effect::Request(StageRequest::CommentList(id)),
        ]
    }

    fn select_row(&mut self, row: usize, activate: bool) -> Vec<Effect> {
        let rows = self.rows();
        let Some(&r) = rows.get(row) else {
            return vec![];
        };
        self.sel = row;
        match r {
            Row::Session(i) if activate => self.open_session(self.sessions[i].id.clone()),
            Row::Session(_) => vec![],
            Row::Post(i) => {
                let id = self.posts[i].id.clone();
                self.follow = i + 1 == self.posts.len();
                self.show(id)
            }
        }
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

    pub fn shown_comments(&self) -> Vec<Comment> {
        let Some(id) = &self.shown else { return vec![] };
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

    /// The shown post laid out for the main column. Memoized on every input it
    /// reads (version, width, comments generation, folds generation), so a
    /// change to any of them is a different key and nothing is invalidated.
    pub fn laid(&mut self) -> Option<&Laid> {
        let id = self.shown.clone()?;
        let version = self.shown_version()?;
        let width = ui::geometry(self.size).main.width.saturating_sub(1);
        let comments = self.shown_comments();
        let cards = self.loaded.get(&(id.clone(), version))?.cards.as_ref()?;
        let view = self.views.entry(id).or_default();
        let key = (version, width, self.comments_gen, view.fold_gen);
        if view.laid.as_ref().is_none_or(|(k, _)| *k != key) {
            view.laid = Some((key, cards::layout(cards, &comments, width, &view.folds)));
        }
        view.laid.as_ref().map(|(_, l)| l)
    }

    fn page(&self) -> usize {
        ui::geometry(self.size).main.height.saturating_sub(1).max(1) as usize
    }

    fn view(&mut self) -> Option<&mut View> {
        let id = self.shown.clone()?;
        self.views.get_mut(&id)
    }

    /// Move the cursor by `delta` lines, keeping it on screen.
    fn cursor(&mut self, delta: isize) {
        let total = self.laid().map_or(0, |l| l.lines.len());
        let page = self.page();
        if let Some(v) = self.view() {
            v.cursor = (v.cursor as isize + delta).clamp(0, total.saturating_sub(1) as isize) as usize;
            if v.cursor < v.scroll {
                v.scroll = v.cursor;
            } else if v.cursor >= v.scroll + page {
                v.scroll = v.cursor + 1 - page;
            }
        }
    }

    /// Scroll the view by `delta` lines, dragging the cursor along.
    fn scroll(&mut self, delta: isize) {
        let total = self.laid().map_or(0, |l| l.lines.len());
        let page = self.page();
        if let Some(v) = self.view() {
            let max = total.saturating_sub(page);
            v.scroll = (v.scroll as isize + delta).clamp(0, max as isize) as usize;
            v.cursor = v.cursor.clamp(v.scroll, (v.scroll + page).saturating_sub(1).max(v.scroll));
        }
    }

    pub fn hit_at_cursor(&mut self) -> Hit {
        let cursor = self.views.get(self.shown.as_ref().unwrap_or(&PostId::default())).map_or(0, |v| v.cursor);
        self.laid().and_then(|l| l.hits.get(cursor).copied()).unwrap_or(Hit::None)
    }

    fn toggle(&mut self, hit: Hit) -> bool {
        let Hit::Node(card, node) = hit else { return false };
        let Some(v) = self.view() else { return false };
        let set = v.folds.entry(card).or_default();
        if !set.remove(&node) {
            set.insert(node);
        }
        v.fold_gen += 1;
        true
    }

    /// The part under the cursor (else the first part).
    pub fn part_at_cursor(&mut self) -> Option<Part> {
        let card = match self.hit_at_cursor() {
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
        }
    }

    fn nav_key(&mut self, k: KeyEvent) -> Vec<Effect> {
        let page = self.page() as isize;
        match k.code {
            KeyCode::Char('q') => vec![Effect::Quit],
            KeyCode::Char('j') => self.select_row(self.sel + 1, false),
            KeyCode::Char('k') => self.select_row(self.sel.saturating_sub(1), false),
            KeyCode::Enter => match self.rows().get(self.sel) {
                Some(Row::Session(i)) if self.open.as_ref() != Some(&self.sessions[*i].id) => {
                    self.select_row(self.sel, true)
                }
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
                self.select_row(row, true)
            }
            MouseEventKind::Down(MouseButton::Left) if inside(g.main) && m.row > g.main.y => {
                let scroll = self.view().map_or(0, |v| v.scroll);
                let line = scroll + (m.row - g.main.y - 1) as usize;
                let total = self.laid().map_or(0, |l| l.lines.len());
                if line < total {
                    if let Some(v) = self.view() {
                        v.cursor = line;
                    }
                    let hit = self.hit_at_cursor();
                    self.toggle(hit);
                }
                vec![]
            }
            // Motion and other buttons change nothing, so they don't redraw.
            _ => return vec![],
        };
        let mut fx = fx;
        fx.push(Effect::Redraw);
        fx
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
                if e.kind == "post-updated" && self.shown.as_ref() == Some(&id) {
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
            Reply::Sessions(sessions) => {
                let selected = self.rows().get(self.sel).copied().and_then(|r| match r {
                    Row::Session(i) => self.sessions.get(i).map(|s| (true, s.id.0.clone())),
                    Row::Post(i) => self.posts.get(i).map(|p| (false, p.id.0.clone())),
                });
                self.sessions = sessions;
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
                } else if let Some((is_session, id)) = selected {
                    // Keep the selection on the same thing as rows move.
                    let rows = self.rows();
                    if let Some(row) = rows.iter().position(|r| match r {
                        Row::Session(i) => is_session && self.sessions[*i].id.0 == id,
                        Row::Post(i) => !is_session && self.posts[*i].id.0 == id,
                    }) {
                        self.sel = row;
                    }
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
                if let Some(id) = target {
                    fx.extend(self.show(id.clone()));
                    if let Some(row) = self.row_of_post(&id) {
                        self.sel = row;
                    }
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
            Reply::Commented(_) => self.toast = Some("comment sent".into()),
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
        assert_eq!(app.rows(), [Row::Session(0), Row::Post(0), Row::Post(1), Row::Session(1)]);
        assert_eq!(app.sel, 2);
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
    fn jk_moves_across_sessions_and_posts() {
        let db = fake();
        let mut app = booted(&db);
        let fx = app.update(key(KeyCode::Char('k')));
        assert_eq!(app.shown.as_deref(), Some("a"));
        assert_eq!(requests(&fx), [StageRequest::PostShow(PostShowArgs { id: "a".into(), version: Some(1) }), StageRequest::PostRender(PostShowArgs { id: "a".into(), version: Some(1) })]);
        serve(&mut app, fx, &db);
        app.update(key(KeyCode::Char('k')));
        assert_eq!(app.rows()[app.sel], Row::Session(0));
        // j past the posts lands on session 2; enter opens it.
        for _ in 0..3 {
            app.update(key(KeyCode::Char('j')));
        }
        assert_eq!(app.rows()[app.sel], Row::Session(1));
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
        app.update(wheel(MouseEventKind::ScrollDown));
        assert_eq!(app.views[&PostId::from("b")].scroll, WHEEL);
        app.update(wheel(MouseEventKind::ScrollUp));
        assert_eq!(app.views[&PostId::from("b")].scroll, 0);
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
        let before = app.laid().unwrap().lines.len();
        let g = ui::geometry(app.size);
        // Row 0 of the laid post is the card border; row 1 is the root node.
        let click = Msg::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: g.main.x + 2, row: g.main.y + 2, modifiers: KeyModifiers::NONE });
        assert_eq!(app.update(click), [Effect::Redraw]);
        assert_eq!(app.laid().unwrap().lines.len(), before - 2);
    }

    #[test]
    fn resize_is_exactly_one_redraw() {
        let mut app = booted(&fake());
        assert_eq!(app.update(Msg::Resize(120, 40)), [Effect::Redraw]);
        assert_eq!(app.size, (120, 40));
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
    fn the_only_write_is_comment_new() {
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
}
