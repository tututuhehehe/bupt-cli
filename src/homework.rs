use crate::cli::{CliError, CliResult, Context, Outcome};
use clap::{ArgMatches, Command};

pub(crate) fn commands() -> Vec<Command> {
    vec![
        Command::new("courses").about("List UCloud courses"),
        Command::new("homework")
            .about("List UCloud homework")
            .subcommand(Command::new("show").about("Show an assignment"))
            .subcommand(Command::new("download").about("Download assignment attachments"))
            .subcommand(Command::new("submit").about("Submit a confirmed assignment"))
            .subcommand(Command::new("verify").about("Verify an existing submission")),
    ]
}

pub(crate) fn run(_: &str, _: &ArgMatches, _: &mut Context<'_>) -> CliResult<Outcome> {
    Err(CliError::pending())
}
