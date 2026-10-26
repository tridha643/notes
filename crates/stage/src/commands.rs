//! `:` commands. A new command is a row in [`COMMANDS`]; nothing else changes.

use notes_proto::posts::Part;

use crate::{
    app::{App, Effect},
    cards::raw_part,
};

pub struct Command {
    pub name: &'static str,
    pub help: &'static str,
    pub run: fn(&mut App) -> Vec<Effect>,
}

pub const COMMANDS: &[Command] = &[
    Command { name: "open", help: "open the html part in the browser", run: open },
    Command { name: "copy", help: "copy the raw part under the cursor", run: copy },
    Command { name: "versions", help: "list this post's versions (v steps back)", run: versions },
    Command { name: "rerender", help: "fetch the post's render again", run: App::rerender },
    Command { name: "help", help: "keys and commands", run: help },
];

/// Run a command by name (a unique prefix works too).
pub fn run(app: &mut App, input: &str) -> Vec<Effect> {
    let name = input.split_whitespace().next().unwrap_or("");
    let found: Vec<&Command> = COMMANDS.iter().filter(|c| c.name.starts_with(name)).collect();
    match (name, found.as_slice()) {
        ("", _) => vec![],
        (_, [one]) => (one.run)(app),
        _ => {
            app.toast = Some(format!("unknown command :{name} (try :help)"));
            vec![]
        }
    }
}

fn open(app: &mut App) -> Vec<Effect> {
    let html = app
        .part_at_cursor()
        .filter(|p| matches!(p, Part::Html { .. }))
        .or_else(|| app.shown_post()?.parts.iter().find(|p| matches!(p, Part::Html { .. })).cloned());
    match (html, app.shown.clone(), app.shown_version()) {
        (Some(Part::Html { html, .. }), Some(id), Some(v)) => {
            vec![Effect::OpenFile { name: format!("{id}-v{v}.html"), contents: html }]
        }
        _ => {
            app.toast = Some("no html part to open".into());
            vec![]
        }
    }
}

fn copy(app: &mut App) -> Vec<Effect> {
    match app.part_at_cursor() {
        Some(part) => {
            app.toast = Some("copied the raw part".into());
            vec![Effect::Copy(raw_part(&part))]
        }
        None => vec![],
    }
}

fn versions(app: &mut App) -> Vec<Effect> {
    let latest = app.shown.as_ref().and_then(|id| app.posts.iter().find(|p| &p.id == id)).map(|p| p.version);
    app.toast = latest.map(|n| format!("versions v1–v{n}; showing v{} · v steps back", app.shown_version().unwrap_or(n)));
    vec![]
}

fn help(app: &mut App) -> Vec<Effect> {
    let cmds: Vec<String> = COMMANDS.iter().map(|c| format!(":{} {}", c.name, c.help)).collect();
    app.toast = Some(format!(
        "j/k post · ↑↓ line · ⏎ fold/open · space/b page · g/G · / search · c comment · v version · o open · q quit · {}",
        cmds.join(" · ")
    ));
    vec![]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_prefixes_resolve() {
        let mut app = App::new(None);
        assert!(run(&mut app, "he").is_empty());
        assert!(app.toast.as_deref().unwrap().contains(":copy"));
        run(&mut app, "zzz");
        assert!(app.toast.as_deref().unwrap().contains("unknown"));
        let names: std::collections::HashSet<_> = COMMANDS.iter().map(|c| c.name).collect();
        assert_eq!(names.len(), COMMANDS.len());
    }
}
