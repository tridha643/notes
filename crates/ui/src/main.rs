use gpui::{
    App, Bounds, Context, FocusHandle, FontWeight, KeyBinding, MouseButton, TitlebarOptions,
    Window, WindowBackgroundAppearance, WindowBounds, WindowOptions, actions, div, point,
    prelude::*, px, rgba, size,
};
use gpui_platform::application;

actions!(notes, [SelectPrev, SelectNext, Quit]);

// Palette from docs/mock/index.html.
const TX: u32 = 0xf5f5f7ff;
const T2: u32 = 0xaeaeb2ff;
const T3: u32 = 0x78787eff;
const WHITE_05: u32 = 0xffffff0d;
const WHITE_06: u32 = 0xffffff0f;
const WHITE_07: u32 = 0xffffff12;
const WHITE_08: u32 = 0xffffff14;
const WHITE_09: u32 = 0xffffff17;
const WHITE_10: u32 = 0xffffff1a;
const SIDEBAR_BG: u32 = 0x28282c59; // rgb(40 40 44 / .35) over the blurred window
const PANE_BG: u32 = 0x161618f5; // rgb(22 22 24 / .96): reading pane stays near-opaque

struct Comment {
    who: &'static str,
    when: &'static str,
    text: &'static str,
}

struct Note {
    title: &'static str,
    author: &'static str,
    meta: &'static str,
    body: &'static [&'static str],
    comments: &'static [Comment],
}

// ponytail: hardcoded sample notes until crates/core's store is wired in.
const NOTES: &[Note] = &[
    Note {
        title: "Glass layer notes",
        author: "You",
        meta: "Edited by You · now",
        body: &[
            "Only the sidebar is vibrant. The reading pane stays nearly opaque so text is always legible.",
            "Try NSGlassEffectView first. Custom blur only if needed.",
        ],
        comments: &[
            Comment {
                who: "claude-code",
                when: "2m",
                text: "Added a section on frame pacing. Want a checklist?",
            },
            Comment {
                who: "You",
                when: "1m",
                text: "Yes, add it.",
            },
        ],
    },
    Note {
        title: "Frame budget",
        author: "claude-code",
        meta: "Edited by claude-code · 14m ago",
        body: &[
            "Target 8.3 ms per frame at 120 Hz and about 0% CPU when idle. Redraw only the changed view.",
        ],
        comments: &[],
    },
    Note {
        title: "Launch plan",
        author: "You",
        meta: "Edited by You · 1h ago",
        body: &[
            "Keep a resident daemon and paint the last view from a cached snapshot, so the window appears before anything loads.",
        ],
        comments: &[Comment {
            who: "perf",
            when: "1h",
            text: "Target 30 ms warm start.",
        }],
    },
];

struct NotesApp {
    selected: usize,
    focus: FocusHandle,
}

impl NotesApp {
    // Only notify (and so re-render) when the selection actually moves.
    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix != self.selected {
            self.selected = ix;
            cx.notify();
        }
    }

    fn select_prev(&mut self, _: &SelectPrev, _: &mut Window, cx: &mut Context<Self>) {
        self.select(self.selected.saturating_sub(1), cx);
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.select((self.selected + 1).min(NOTES.len() - 1), cx);
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(200.))
            .h_full()
            .flex()
            .flex_col()
            .flex_none()
            .bg(rgba(SIDEBAR_BG))
            .border_r(px(0.5))
            .border_color(rgba(WHITE_10))
            // 46px top strip holds the native traffic lights.
            .child(div().h(px(46.)).flex_none())
            .child(
                div()
                    .mx(px(18.))
                    .mt(px(10.))
                    .mb(px(4.))
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgba(T3))
                    .child("Notes"),
            )
            .child(
                div()
                    .px(px(8.))
                    .flex()
                    .flex_col()
                    .children(NOTES.iter().enumerate().map(|(ix, note)| {
                        let on = ix == self.selected;
                        div()
                            .id(ix)
                            .h(px(28.))
                            .px(px(10.))
                            .flex()
                            .items_center()
                            .rounded(px(8.))
                            .text_size(px(13.))
                            .text_color(rgba(if on { TX } else { T2 }))
                            .when(on, |d| d.bg(rgba(WHITE_09)))
                            .when(!on, |d| d.hover(|s| s.bg(rgba(WHITE_06))))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| this.select(ix, cx)),
                            )
                            .child(div().truncate().child(note.title))
                    })),
            )
    }

    fn pane(&self) -> impl IntoElement {
        let note = &NOTES[self.selected];
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgba(PANE_BG))
            .child(
                div()
                    .h(px(46.))
                    .flex_none()
                    .px(px(22.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b(px(0.5))
                    .border_color(rgba(WHITE_07))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(note.title))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgba(T3))
                            .child(note.author),
                    ),
            )
            .child(
                div()
                    .id("main")
                    .flex_1()
                    .overflow_y_scroll()
                    .pt(px(28.))
                    .pb(px(110.))
                    .child(
                        div()
                            .max_w(px(680.))
                            .mx_auto()
                            .px(px(28.))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .mb(px(2.))
                                    .text_size(px(22.))
                                    .font_weight(FontWeight::BOLD)
                                    .child(note.title),
                            )
                            .child(
                                div()
                                    .mb(px(18.))
                                    .text_size(px(12.))
                                    .text_color(rgba(T3))
                                    .child(note.meta),
                            )
                            .children(note.body.iter().map(|p| div().mb(px(13.)).child(*p)))
                            .children(note.comments.iter().map(comment_card)),
                    ),
            )
    }
}

fn comment_card(c: &Comment) -> impl IntoElement {
    div()
        .my(px(10.))
        .px(px(14.))
        .py(px(10.))
        .rounded(px(12.))
        .bg(rgba(WHITE_05))
        .border(px(0.5))
        .border_color(rgba(WHITE_08))
        .child(
            div()
                .mb(px(2.))
                .text_size(px(12.))
                .flex()
                .gap(px(4.))
                .child(div().font_weight(FontWeight::SEMIBOLD).child(c.who))
                .child(div().text_color(rgba(T3)).child(c.when)),
        )
        .child(c.text)
}

impl Render for NotesApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus)
            .key_context("Notes")
            .on_action(cx.listener(Self::select_prev))
            .on_action(cx.listener(Self::select_next))
            .size_full()
            .flex()
            .text_size(px(13.))
            .line_height(px(19.5))
            .text_color(rgba(TX))
            .font_family(".SystemUIFont")
            .child(self.sidebar(cx))
            .child(self.pane())
    }
}

fn main() {
    application().run(|cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("up", SelectPrev, Some("Notes")),
            KeyBinding::new("down", SelectNext, Some("Notes")),
            KeyBinding::new("cmd-q", Quit, None),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());

        let bounds = Bounds::centered(None, size(px(960.), px(640.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Notes".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(14.), px(17.))),
                }),
                window_background: WindowBackgroundAppearance::Blurred,
                ..Default::default()
            },
            |window, cx| {
                cx.new(|cx| {
                    let focus = cx.focus_handle();
                    window.focus(&focus, cx);
                    NotesApp { selected: 0, focus }
                })
            },
        )
        .unwrap();
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
    });
}
