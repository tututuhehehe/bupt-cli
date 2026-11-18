use crate::cli::{CliError, CliResult, Context, Outcome};
use clap::{Arg, ArgAction, ArgMatches, Command};

pub(crate) fn commands() -> Vec<Command> {
    vec![
        Command::new("update")
            .about("Check for or install a new release")
            .arg(
                Arg::new("check")
                    .long("check")
                    .action(ArgAction::SetTrue)
                    .help("Check only; do not install"),
            )
            .arg(
                Arg::new("repo")
                    .long("repo")
                    .value_name("OWNER/REPO")
                    .help("GitHub release repository"),
            ),
        Command::new("uninstall").about("Remove the current installation"),
    ]
}

pub(crate) fn run(_: &str, _: &ArgMatches, _: &mut Context<'_>) -> CliResult<Outcome> {
    Err(CliError::pending())
}
