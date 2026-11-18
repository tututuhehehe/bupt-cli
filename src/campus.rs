use crate::cli::{CliError, CliResult, Context, Outcome};
use clap::{ArgMatches, Command};

pub(crate) fn commands() -> Vec<Command> {
    vec![
        Command::new("library")
            .about("Search the campus library")
            .subcommand_required(true)
            .subcommand(Command::new("search").about("Find a title"))
            .subcommand(Command::new("hold").about("Show physical holdings"))
            .subcommand(Command::new("ebook").about("Show electronic copies")),
        Command::new("electricity")
            .about("Read dorm electricity balance")
            .subcommand(Command::new("buildings").about("List dorm buildings")),
        Command::new("notices")
            .about("List campus notices")
            .subcommand(Command::new("show").about("Read a notice")),
        Command::new("card").about("Read campus card balance"),
    ]
}

pub(crate) fn run(_: &str, _: &ArgMatches, _: &mut Context<'_>) -> CliResult<Outcome> {
    Err(CliError::pending())
}
