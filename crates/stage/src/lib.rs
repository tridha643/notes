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
pub mod search;
pub mod ui;
