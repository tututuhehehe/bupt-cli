use crate::cli::{CliError, CliResult, Context, Outcome};
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;

mod dorm;
mod library;

pub(crate) fn commands() -> Vec<Command> {
    vec![
        Command::new("library")
            .about("Search the campus library")
            .subcommand_required(true)
            .subcommand(
                Command::new("search")
                    .about("Find a title")
                    .arg(Arg::new("keyword").required(true).value_name("KEYWORD"))
                    .arg(
                        Arg::new("page")
                            .long("page")
                            .value_parser(positive_usize)
                            .default_value("1"),
                    )
                    .arg(
                        Arg::new("page-size")
                            .long("page-size")
                            .value_parser(positive_usize)
                            .default_value("10"),
                    )
                    .arg(
                        Arg::new("location")
                            .long("location")
                            .default_value("all")
                            .value_name("CAMPUS"),
                    )
                    .arg(
                        Arg::new("with-ebook")
                            .long("with-ebook")
                            .action(ArgAction::SetTrue),
                    ),
            )
            .subcommand(
                Command::new("hold")
                    .about("Show physical holdings")
                    .arg(Arg::new("id").required(true).value_name("RECCTRLID")),
            )
            .subcommand(
                Command::new("ebook")
                    .about("Show electronic copies")
                    .arg(Arg::new("id").required(true).value_name("RECCTRLID")),
            ),
        Command::new("electricity")
            .about("Read dorm electricity balance")
            .arg(Arg::new("building").long("building").value_name("BUILDING"))
            .arg(Arg::new("room").long("room").value_name("ROOM"))
            .arg(Arg::new("floor").long("floor").value_name("FLOOR"))
            .arg(
                Arg::new("threshold")
                    .long("threshold")
                    .value_parser(non_negative_f64)
                    .default_value("10"),
            )
            .arg(Arg::new("area").long("area").value_name("AREA").hide(true))
            .subcommand(
                Command::new("buildings").about("List dorm buildings").arg(
                    Arg::new("area")
                        .long("area")
                        .default_value("shahe")
                        .value_name("AREA"),
                ),
            ),
        Command::new("notices")
            .about("List campus notices")
            .arg(Arg::new("type").long("type").value_name("TYPES"))
            .arg(
                Arg::new("count")
                    .long("count")
                    .value_parser(non_negative_u64)
                    .default_value("10"),
            )
            .subcommand(
                Command::new("show")
                    .about("Read a notice")
                    .arg(Arg::new("url").required(true).value_name("URL"))
                    .arg(
                        Arg::new("max-chars")
                            .long("max-chars")
                            .value_parser(non_negative_usize)
                            .default_value("0"),
                    ),
            ),
        Command::new("card").about("Read campus card balance"),
    ]
}

pub(crate) fn run(
    name: &str,
    matches: &ArgMatches,
    context: &mut Context<'_>,
) -> CliResult<Outcome> {
    match name {
        "library" => run_library(matches, context),
        "electricity" => run_electricity(matches, context),
        "notices" => run_notices(matches, context),
        "card" => run_card(context),
        _ => Err(CliError::usage("unknown campus command")),
    }
}

fn positive_usize(text: &str) -> Result<usize, String> {
    let value = text
        .parse::<usize>()
        .map_err(|_| "expected a positive integer")?;
    if value == 0 {
        return Err("expected a positive integer".into());
    }
    Ok(value)
}

fn non_negative_usize(text: &str) -> Result<usize, String> {
    text.parse::<usize>()
        .map_err(|_| "expected a non-negative integer".into())
}

fn non_negative_u64(text: &str) -> Result<u64, String> {
    text.parse::<u64>()
        .map_err(|_| "expected a non-negative integer".into())
}

fn non_negative_f64(text: &str) -> Result<f64, String> {
    let value = text
        .parse::<f64>()
        .map_err(|_| "expected a non-negative number")?;
    if !value.is_finite() || value < 0.0 {
        return Err("expected a finite non-negative number".into());
    }
    Ok(value)
}

fn run_library(matches: &ArgMatches, context: &mut Context<'_>) -> CliResult<Outcome> {
    let Some((subcommand, arguments)) = matches.subcommand() else {
        return Err(CliError::usage("library needs a subcommand"));
    };
    let data = match subcommand {
        "search" => {
            let keyword = arguments
                .get_one::<String>("keyword")
                .map(String::as_str)
                .unwrap_or("");
            let page = *arguments
                .get_one::<usize>("page")
                .expect("page has a default");
            let page_size = *arguments
                .get_one::<usize>("page-size")
                .expect("page-size has a default");
            let location = arguments
                .get_one::<String>("location")
                .map(String::as_str)
                .unwrap_or("all");
            let with_ebook = arguments.get_flag("with-ebook");
            library::library_search(keyword, page, page_size, location, with_ebook, context)?
        }
        "hold" => {
            let id = arguments
                .get_one::<String>("id")
                .map(String::as_str)
                .unwrap_or("");
            library::library_holdings(id, context)?
        }
        "ebook" => {
            let id = arguments
                .get_one::<String>("id")
                .map(String::as_str)
                .unwrap_or("");
            library::electronic_resources(id, context)?
        }
        _ => return Err(CliError::usage("library needs a subcommand")),
    };
    Ok(Outcome {
        data,
        code: 0,
        diagnostic: None,
    })
}

fn run_electricity(matches: &ArgMatches, context: &mut Context<'_>) -> CliResult<Outcome> {
    if let Some(("buildings", arguments)) = matches.subcommand() {
        let requested = arguments
            .get_one::<String>("area")
            .map(String::as_str)
            .unwrap_or("shahe");
        let area = dorm::resolve_area(requested)?;
        return Err(session_unavailable(
            context,
            &format!("electricity buildings ({})", area.name()),
        ));
    }
    if matches.get_one::<String>("area").is_some() {
        return Err(CliError::new(
            2,
            "usage",
            "--area is only valid for `bupt electricity buildings`",
            "try `bupt electricity buildings --area shahe` or `bupt electricity buildings --area xitucheng`",
        ));
    }

    let flags = dorm::DormFlags {
        building: matches.get_one::<String>("building").cloned(),
        room: matches.get_one::<String>("room").cloned(),
        floor: matches.get_one::<String>("floor").cloned(),
    };
    let dorm = dorm::resolve_dorm(&flags, &dorm::dorm_env(), &context.config_dir)?;
    let _floor = dorm.floor.as_deref();
    let _source = dorm.source;
    Err(session_unavailable(
        context,
        &format!("electricity {} {}", dorm.building.name, dorm.room),
    ))
}

fn run_notices(matches: &ArgMatches, context: &mut Context<'_>) -> CliResult<Outcome> {
    if let Some(("show", arguments)) = matches.subcommand() {
        let url = arguments
            .get_one::<String>("url")
            .map(String::as_str)
            .unwrap_or("");
        if url.trim().is_empty() {
            return Err(CliError::new(
                2,
                "usage",
                "notices show URL must not be empty",
                "pass a notice URL from `bupt notices`",
            ));
        }
        // Pending portal adapter: validate the allowed destination and never
        // attach campus credentials to an arbitrary notice URL.
        let _max_chars = *arguments
            .get_one::<usize>("max-chars")
            .expect("max-chars has a default");
        return Err(session_unavailable(context, "notices show"));
    }

    let _types = notice_types(matches.get_one::<String>("type"))?;
    let _count = *matches
        .get_one::<u64>("count")
        .expect("count has a default");
    Err(session_unavailable(context, "notices"))
}

fn notice_types(value: Option<&String>) -> CliResult<Option<Vec<String>>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let mut resolved = Vec::new();
    for raw in value.split(',') {
        let key = raw.trim().to_ascii_lowercase();
        if key.is_empty() {
            continue;
        }
        if !matches!(key.as_str(), "notice" | "news" | "rules" | "ways") {
            return Err(CliError::new(
                2,
                "usage",
                format!("unknown notice type '{key}'"),
                "choose from notice, news, rules, ways",
            ));
        }
        resolved.push(key);
    }
    Ok(if resolved.is_empty() {
        None
    } else {
        Some(resolved)
    })
}

fn run_card(context: &mut Context<'_>) -> CliResult<Outcome> {
    Err(session_unavailable(context, "card"))
}

fn has_local_profile(context: &Context<'_>) -> bool {
    let mut username = std::env::var("BUPT_USERNAME")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let mut password = std::env::var("BUPT_PASSWORD")
        .ok()
        .filter(|value| !value.is_empty());
    if username.is_some() && password.is_some() {
        return true;
    }

    let path: PathBuf = context.config_dir.join("credentials.json");
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(Value::Object(object)) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    if username.is_none() {
        username = object
            .get("username")
            .and_then(Value::as_str)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
    }
    if password.is_none() {
        password = object
            .get("password")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
    }
    username.is_some() && password.is_some()
}

fn session_unavailable(context: &Context<'_>, service: &str) -> CliError {
    if !has_local_profile(context) {
        return CliError::new(
            3,
            "auth",
            "BUPT credentials are not configured",
            "run `bupt login` (credentials come from BUPT_USERNAME/BUPT_PASSWORD or credentials.json)",
        );
    }
    CliError::new(
        5,
        "upstream",
        format!("{service} session adapter is not connected in this migration build"),
        "",
    )
}
