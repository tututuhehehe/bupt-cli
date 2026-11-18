use crate::cli::{CliError, CliResult, Context, Outcome};
use clap::{ArgMatches, Command};

pub(crate) fn commands() -> Vec<Command> {
    vec![
        Command::new("schedule")
            .about("Read the campus timetable")
            .subcommand(Command::new("update").about("Refresh the timetable"))
            .subcommand(Command::new("calendar").about("Show teaching weeks"))
            .subcommand(Command::new("export").about("Write an iCalendar file")),
        Command::new("exams").about("Show exam information for the account's role"),
        Command::new("scores").about("Show undergraduate scores"),
        Command::new("classrooms").about("Show undergraduate classroom availability"),
    ]
}

pub(crate) fn run(_: &str, _: &ArgMatches, _: &mut Context<'_>) -> CliResult<Outcome> {
    Err(CliError::pending())
}
