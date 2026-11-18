mod submit;

use crate::cli::{CliError, CliResult, Context, Outcome};
use crate::identity;
use clap::{Arg, ArgAction, ArgMatches, Command};

pub(crate) fn commands() -> Vec<Command> {
    vec![
        Command::new("courses").about("List every enrolled UCloud course site"),
        Command::new("homework")
            .about("List UCloud assignments, or inspect and submit one")
            .arg(
                Arg::new("undone")
                    .long("undone")
                    .action(ArgAction::SetTrue)
                    .help("Only assignments that are not submitted"),
            )
            .arg(
                Arg::new("course")
                    .long("course")
                    .value_name("ID")
                    .help("Filter by UCloud course/site id"),
            )
            .arg(
                Arg::new("new")
                    .long("new")
                    .value_name("DAYS")
                    .value_parser(clap::value_parser!(u64))
                    .help("Only assignments published within DAYS days"),
            )
            .arg(
                Arg::new("limit")
                    .long("limit")
                    .value_name("N")
                    .default_value("0")
                    .value_parser(clap::value_parser!(u64))
                    .help("Maximum results (0 = all)"),
            )
            .subcommand(
                Command::new("show")
                    .about("Show one assignment and its attachments")
                    .arg(Arg::new("id").value_name("ID").required(true))
                    .arg(
                        Arg::new("course")
                            .long("course")
                            .value_name("SITE_ID")
                            .value_parser(numeric_id)
                            .help("Only look up the submission time in this course/site id"),
                    ),
            )
            .subcommand(
                Command::new("download")
                    .about("Download teacher attachments for one assignment")
                    .arg(Arg::new("id").value_name("ID").required(true))
                    .arg(
                        Arg::new("dir")
                            .long("dir")
                            .value_name("DIR")
                            .default_value(".")
                            .help("Output directory"),
                    )
                    .arg(
                        Arg::new("force")
                            .long("force")
                            .action(ArgAction::SetTrue)
                            .help("Overwrite existing files"),
                    ),
            )
            .subcommand(submit::command())
            .subcommand(
                Command::new("verify")
                    .about("Read-only check that a submission is visible upstream")
                    .arg(Arg::new("id").value_name("ID").required(true))
                    .arg(
                        Arg::new("resource_id")
                            .long("resource-id")
                            .value_name("RID")
                            .action(ArgAction::Append)
                            .help("Expected attachment resource id (repeatable)"),
                    )
                    .arg(
                        Arg::new("text")
                            .long("text")
                            .value_name("TEXT")
                            .help("Expected submitted text"),
                    ),
            ),
    ]
}

fn numeric_id(raw: &str) -> Result<String, String> {
    let text = raw.trim();
    if !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()) {
        Ok(text.to_owned())
    } else {
        Err("id must be numeric".into())
    }
}

pub(crate) fn run(
    command: &str,
    matches: &ArgMatches,
    context: &mut Context<'_>,
) -> CliResult<Outcome> {
    match command {
        "courses" => {
            require_credentials(context)?;
            Err(unsupported("course listing"))
        }
        "homework" => run_homework(matches, context),
        _ => Err(CliError::pending()),
    }
}

fn run_homework(matches: &ArgMatches, context: &mut Context<'_>) -> CliResult<Outcome> {
    match matches.subcommand() {
        Some(("show", _)) => {
            require_credentials(context)?;
            Err(unsupported("homework show"))
        }
        Some(("download", _)) => {
            require_credentials(context)?;
            Err(unsupported("homework download"))
        }
        Some(("submit", submit_matches)) => submit::run(submit_matches, context),
        Some(("verify", _)) => {
            require_credentials(context)?;
            Err(unsupported("homework verify"))
        }
        _ => {
            require_credentials(context)?;
            Err(unsupported("homework listing"))
        }
    }
}

pub(super) fn unsupported(operation: &str) -> CliError {
    CliError::new(
        5,
        "upstream",
        format!("{operation} is not implemented in this migration build"),
        "No campus request was made. This batch only covers local argument validation and the submit gate.",
    )
}

pub(super) fn require_credentials(context: &Context<'_>) -> CliResult<()> {
    identity::require_authenticated_profile(&context.config_dir).map(|_| ())
}
