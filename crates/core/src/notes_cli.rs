use std::{
    env,
    ffi::OsString,
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
};

use clap::{Parser, Subcommand};
use notes_core::{NoteId, NoteStore};

#[derive(Debug, Parser)]
#[command(
    name = "notes",
    version,
    about = "Create, read, and append local notes"
)]
struct NotesCli {
    #[command(subcommand)]
    command: NotesCommand,
}

#[derive(Debug, Subcommand)]
enum NotesCommand {
    /// Create a note and print its ID.
    New {
        #[arg(allow_hyphen_values = true)]
        title: String,
        #[arg(long, default_value = "", allow_hyphen_values = true)]
        body: String,
    },
    /// List IDs and titles, most recently updated first.
    List,
    /// Print a note's title and body.
    Show { id: NoteId },
    /// Append literal text without adding a separator and print the note ID.
    Append {
        id: NoteId,
        #[arg(allow_hyphen_values = true)]
        text: String,
    },
}

pub(crate) fn run_notes_cli() -> ExitCode {
    let cli = NotesCli::parse();
    match execute_notes_command(cli.command, &mut io::stdout().lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error)
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|error| error.kind() == io::ErrorKind::BrokenPipe) =>
        {
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("notes: {error}");
            ExitCode::FAILURE
        }
    }
}

fn resolve_notes_db_path(
    override_path: Option<OsString>,
    home: Option<OsString>,
) -> io::Result<PathBuf> {
    if let Some(path) = override_path {
        if path.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "NOTES_DB must not be empty",
            ));
        }
        return Ok(PathBuf::from(path));
    }
    let home = home.filter(|path| !path.is_empty()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "HOME is unavailable; set NOTES_DB to the database path",
        )
    })?;
    Ok(PathBuf::from(home).join(".local/share/notes-gpui/notes.db"))
}

fn execute_notes_command(
    command: NotesCommand,
    output: &mut impl Write,
) -> Result<(), Box<dyn std::error::Error>> {
    let path = resolve_notes_db_path(env::var_os("NOTES_DB"), env::var_os("HOME"))?;
    let store = NoteStore::open(&path)?;
    match command {
        NotesCommand::New { title, body } => {
            writeln!(output, "{}", store.create_note(&title, &body)?.id)?;
        }
        NotesCommand::List => {
            for note in store.list_notes()? {
                writeln!(output, "{}\t{}", note.id, note.title)?;
            }
        }
        NotesCommand::Show { id } => {
            let note = store.get_note(id)?;
            writeln!(output, "{}\n\n{}", note.title, note.body)?;
        }
        NotesCommand::Append { id, text } => {
            writeln!(output, "{}", store.append_note(id, &text)?.id)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_four_commands_and_optional_body() {
        for args in [
            vec!["notes", "new", "Title"],
            vec!["notes", "new", "Title", "--body", "Body"],
            vec!["notes", "list"],
            vec!["notes", "show", "1"],
            vec!["notes", "append", "1", "More text"],
        ] {
            assert!(NotesCli::try_parse_from(args).is_ok());
        }
        let cli = NotesCli::try_parse_from(["notes", "new", "Title"]).unwrap();
        assert!(matches!(cli.command, NotesCommand::New { body, .. } if body.is_empty()));
        assert!(NotesCli::try_parse_from(["notes", "show", "invalid"]).is_err());
        assert!(NotesCli::try_parse_from(["notes", "append", "1"]).is_err());
    }

    #[test]
    fn resolves_default_path_and_override_without_home() {
        assert_eq!(
            resolve_notes_db_path(None, Some("/home/test".into())).unwrap(),
            PathBuf::from("/home/test/.local/share/notes-gpui/notes.db")
        );
        assert_eq!(
            resolve_notes_db_path(Some("custom.db".into()), None).unwrap(),
            PathBuf::from("custom.db")
        );
        assert!(resolve_notes_db_path(None, None).is_err());
        assert!(resolve_notes_db_path(Some("".into()), Some("/home/test".into())).is_err());
    }
}
