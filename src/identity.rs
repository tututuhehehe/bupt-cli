use crate::cli::{CliError, CliResult, Context, Outcome};
use clap::{ArgMatches, Command};

pub(crate) fn commands() -> Vec<Command> {
    vec![
        Command::new("login").about("Register and verify a campus account"),
        Command::new("status").about("Show local account and session state"),
        Command::new("logout").about("Forget saved credentials and sessions"),
        Command::new("diagnose").about("Check service availability"),
    ]
}

pub(crate) fn run(_: &str, _: &ArgMatches, _: &mut Context<'_>) -> CliResult<Outcome> {
    Err(CliError::pending())
}
