//! stage: the session's sideshow posts as a TUI in a Rex pane. See
//! docs/stage-plan.md. `app` decides, `ui` draws, `client` talks to notesd,
//! `main` does the terminal IO.

pub mod app;
pub mod cards;
pub mod client;
pub mod commands;
pub mod gallery;
pub mod kitty;
pub mod latency;
pub mod page;
pub mod probe;
pub mod sanitize;
pub mod screen;
pub mod ui;

use std::collections::VecDeque;

use app::{App, Effect, Msg};
use client::StageClient;

/// Feed one message through the app, performing its requests (and the
/// requests their replies cause) until only non-request effects are left.
/// ponytail: requests run synchronously on the loop thread; a slow first
/// render (cold highlighter) stalls input that long. Move to a request thread
/// if that shows up.
pub fn pump(app: &mut App, client: &mut StageClient, msg: Msg) -> Vec<Effect> {
    let mut queue: VecDeque<Effect> = app.update(msg).into();
    let mut out = Vec::new();
    while let Some(effect) = queue.pop_front() {
        match effect {
            Effect::Request(req) => queue.extend(app.update(Msg::Reply(client.perform(&req)))),
            other => out.push(other),
        }
    }
    out
}

/// Like [`pump`] for effects the app returned outside `update` (`App::start`).
pub fn perform(app: &mut App, client: &mut StageClient, effects: Vec<Effect>) -> Vec<Effect> {
    let mut out = Vec::new();
    for effect in effects {
        match effect {
            Effect::Request(req) => {
                let reply = client.perform(&req);
                out.extend(pump(app, client, Msg::Reply(reply)));
            }
            other => out.push(other),
        }
    }
    out
}
