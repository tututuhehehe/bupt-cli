mod detail;
mod download;
mod new;
mod read;
mod show;
mod submit;
mod text;
mod verify;

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
            let session = load_read_session(context)?;
            let courses = read::fetch_courses(context.transport, &session)?;
            Ok(Outcome {
                data: read::courses_json(&courses),
                code: 0,
                diagnostic: None,
            })
        }
        "homework" => run_homework(matches, context),
        _ => Err(CliError::pending()),
    }
}

fn run_homework(matches: &ArgMatches, context: &mut Context<'_>) -> CliResult<Outcome> {
    match matches.subcommand() {
        Some(("show", show_matches)) => show::run(show_matches, context),
        Some(("download", download_matches)) => download::run(download_matches, context),
        Some(("submit", submit_matches)) => submit::run(submit_matches, context),
        Some(("verify", verify_matches)) => verify::run(verify_matches, context),
        None => run_homework_list(matches, context),
        Some((command, _)) => Err(CliError::usage(format!(
            "unknown homework command: {command}"
        ))),
    }
}

fn run_homework_list(matches: &ArgMatches, context: &mut Context<'_>) -> CliResult<Outcome> {
    let undone = matches.get_flag("undone");
    let course = matches
        .get_one::<String>("course")
        .filter(|value| !value.is_empty())
        .cloned();
    let new_days = matches.get_one::<u64>("new").copied();
    let limit = *matches
        .get_one::<u64>("limit")
        .expect("limit has a default");

    let session = load_read_session(context)?;
    let now = local_now()?;
    if let Some(course) = course.as_deref() {
        let items = read::fetch_course_homework(context.transport, &session, course)?;
        let filters = read::ListFilters {
            undone,
            course: Some(course.to_owned()),
            new_days,
        };
        return finish_list(items, "course", filters, limit, now, &session, context);
    }
    if undone {
        let overview = read::fetch_undone(context.transport, &session)?;
        if new_days.is_none() {
            return Ok(Outcome {
                data: read::undone_json(overview, limit, now),
                code: 0,
                diagnostic: None,
            });
        }
        let filters = read::ListFilters {
            undone: true,
            course: None,
            new_days,
        };
        return finish_list(
            overview.items,
            "undone",
            filters,
            limit,
            now,
            &session,
            context,
        );
    }
    let items = read::fetch_all_homework(context.transport, &session)?;
    let filters = read::ListFilters {
        undone: false,
        course: None,
        new_days,
    };
    finish_list(items, "all", filters, limit, now, &session, context)
}

fn finish_list(
    items: Vec<read::HomeworkItem>,
    method: &str,
    filters: read::ListFilters,
    limit: u64,
    now: time::PrimitiveDateTime,
    session: &read::Session,
    context: &mut Context<'_>,
) -> CliResult<Outcome> {
    if let Some(days) = filters.new_days {
        return Ok(Outcome {
            data: new::execute(
                items,
                method,
                filters,
                limit,
                days,
                now,
                session,
                context.transport,
            )?,
            code: 0,
            diagnostic: None,
        });
    }
    Ok(Outcome {
        data: read::homework_list_json(items, method, filters, limit, now),
        code: 0,
        diagnostic: None,
    })
}

fn local_now() -> CliResult<time::PrimitiveDateTime> {
    let now = time::OffsetDateTime::now_local().map_err(|_| {
        CliError::new(
            5,
            "upstream",
            "cannot determine the local time for homework deadlines",
            "",
        )
    })?;
    Ok(time::PrimitiveDateTime::new(now.date(), now.time()))
}

pub(super) fn unsupported(operation: &str) -> CliError {
    CliError::new(
        5,
        "upstream",
        format!("{operation} is not implemented in this migration build"),
        "No campus request was made. This operation is not wired in this migration batch.",
    )
}

fn require_read_profile(context: &Context<'_>) -> CliResult<()> {
    if identity::require_authenticated_profile(&context.config_dir)?
        .role()
        .is_none()
    {
        return Err(CliError::new(
            3,
            "auth",
            "the saved credentials have no academic role",
            "Run `bupt login` with an explicit role.",
        ));
    }
    Ok(())
}

fn load_read_session(context: &mut Context<'_>) -> CliResult<read::Session> {
    require_read_profile(context)?;
    let (access_token, user_id) = identity::require_ucloud_session(context)?;
    Ok(read::Session::new(access_token, user_id))
}

pub(super) fn require_credentials(context: &Context<'_>) -> CliResult<()> {
    identity::require_authenticated_profile(&context.config_dir).map(|_| ())
}

/// The submit write path stays behind its confirmation gate and is not wired
/// to a transport in this batch. Read commands use `load_read_session`.
pub(in crate::homework) fn load_session(context: &Context<'_>) -> CliResult<read::Session> {
    require_credentials(context)?;
    Err(unsupported("homework submit wiring"))
}
