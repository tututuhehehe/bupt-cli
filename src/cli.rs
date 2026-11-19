use crate::transport::{Offline, Transport};
use clap::{Arg, ArgAction, ArgMatches, Command, error::ErrorKind};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

const SCHEMA: &str = "bupt.v1";
pub(crate) type CliResult<T> = Result<T, CliError>;

#[derive(Debug)]
pub(crate) struct CliError {
    pub code: u8,
    pub kind: &'static str,
    pub message: String,
    pub hint: String,
}

impl CliError {
    pub(crate) fn new(
        code: u8,
        kind: &'static str,
        message: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        Self {
            code,
            kind,
            message: message.into(),
            hint: hint.into(),
        }
    }

    pub(crate) fn usage(message: impl Into<String>) -> Self {
        Self::new(2, "usage", message, "Run `bupt help` for usage.")
    }

    pub(crate) fn pending() -> Self {
        Self::new(
            5,
            "upstream",
            "command is not implemented in this migration build",
            "",
        )
    }
}

pub(crate) struct Outcome {
    pub data: Value,
    pub code: u8,
    pub diagnostic: Option<String>,
}

pub(crate) struct Context<'a> {
    pub home_dir: PathBuf,
    pub config_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub transport: &'a mut dyn Transport,
}

impl<'a> Context<'a> {
    fn from_environment(transport: &'a mut dyn Transport) -> CliResult<Self> {
        let home_dir =
            std::env::home_dir().ok_or_else(|| CliError::usage("home directory unavailable"))?;
        let default = home_dir.join(".bupt-cli");
        let directory = |key| {
            std::env::var_os(key)
                .filter(|v| !v.is_empty())
                .map_or_else(|| default.clone(), PathBuf::from)
        };
        Ok(Self {
            config_dir: directory("BUPT_CLI_CONFIG_DIR"),
            cache_dir: directory("BUPT_CLI_CACHE_DIR"),
            home_dir,
            transport,
        })
    }
}

fn timeout_value(text: &str) -> Result<f64, String> {
    let value = text
        .parse::<f64>()
        .map_err(|_| "expected seconds as a number")?;
    if !value.is_finite() || value <= 0.0 || Duration::try_from_secs_f64(value).is_err() {
        return Err("expected a finite positive timeout".into());
    }
    Ok(value)
}

type Run = fn(&str, &ArgMatches, &mut Context<'_>) -> CliResult<Outcome>;

type Lane = (fn() -> Vec<Command>, Run);

fn uses_public_library(name: &str) -> bool {
    name == "library"
}

/// Allow the CAS protocol and the implemented read-only UCloud commands.
fn uses_campus_transport(name: &str, arguments: &ArgMatches) -> bool {
    matches!(
        name,
        "login"
            | "courses"
            | "electricity"
            | "schedule"
            | "exams"
            | "scores"
            | "classrooms"
            | "notices"
            | "card"
    ) || (name == "homework"
        && match arguments.subcommand() {
            Some(("show" | "verify" | "download", _)) => true,
            Some(("submit", submit)) => {
                // The write path is gated by the confirmation flag; the
                // read-only preview is routed with or without a session.
                submit.get_flag("dry_run") || submit.get_flag("i_confirm_submit")
            }
            // Every parent-level list mode is now read-only and routed:
            // default/all, --course, --undone, and --new.
            None => true,
            _ => false,
        })
}

const LANES: &[Lane] = &[
    (crate::identity::commands, crate::identity::run),
    (crate::homework::commands, crate::homework::run),
    (crate::academics::commands, crate::academics::run),
    (crate::campus::commands, crate::campus::run),
    (crate::lifecycle::commands, crate::lifecycle::run),
];

fn command_tree() -> (Command, Vec<(String, Run)>) {
    let mut command = Command::new("bupt")
        .version(env!("CARGO_PKG_VERSION"))
        .about("北邮校园服务命令行工具")
        .disable_help_subcommand(true)
        .subcommand_required(true)
        .arg(
            Arg::new("pretty")
                .long("pretty")
                .action(ArgAction::SetTrue)
                .global(true)
                .help("Pretty-print JSON"),
        )
        .arg(
            Arg::new("timeout")
                .long("timeout")
                .global(true)
                .value_parser(timeout_value)
                .default_value("20")
                .help("Request timeout in seconds"),
        )
        .subcommand(
            Command::new("help")
                .about("Show help for bupt or a command")
                .arg(Arg::new("topic").num_args(0..=2)),
        );
    let mut routes = Vec::new();
    for (commands, run) in LANES {
        for spec in commands() {
            routes.push((spec.get_name().into(), *run));
            command = command.subcommand(spec);
        }
    }
    (command, routes)
}

fn show_help(mut root: Command, matches: &ArgMatches, stdout: &mut dyn Write) -> CliResult<()> {
    root.build();
    let mut current = &mut root;
    for topic in matches.get_many::<String>("topic").into_iter().flatten() {
        current = current
            .find_subcommand_mut(topic)
            .ok_or_else(|| CliError::usage(format!("unknown help topic: {topic}")))?;
    }
    writeln!(stdout, "{}", current.render_long_help()).map_err(write_error)
}

fn write_error(_: io::Error) -> CliError {
    CliError::new(5, "upstream", "cannot write command output", "")
}

fn redact(mut text: String, args: &[OsString]) -> String {
    let mut values = Vec::new();
    for (index, arg) in args.iter().enumerate() {
        let arg = arg.to_string_lossy();
        if arg == "--password" {
            values.extend(
                args.get(index + 1)
                    .map(|value| value.to_string_lossy().into_owned()),
            );
        } else if let Some(value) = arg.strip_prefix("--password=") {
            values.push(value.into());
        }
    }
    for value in values.into_iter().filter(|value| !value.is_empty()) {
        text = text.replace(&value, "[REDACTED]");
    }
    text
}

fn write_json(stream: &mut dyn Write, data: &Value, pretty: bool) -> CliResult<()> {
    let text = if pretty {
        serde_json::to_string_pretty(data)
    } else {
        serde_json::to_string(data)
    }
    .map_err(|_| CliError::new(5, "upstream", "cannot serialize command output", ""))?;
    writeln!(stream, "{text}").map_err(write_error)
}

fn run(
    args: &[OsString],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    offline: &mut dyn Transport,
    public: Option<&mut dyn Transport>,
) -> u8 {
    let (command, routes) = command_tree();
    let pretty_on_error = args
        .iter()
        .skip(1)
        .take_while(|arg| *arg != "--")
        .any(|arg| arg == "--pretty");
    let mut pretty = pretty_on_error;
    let result = (|| -> CliResult<u8> {
        let matches = match command.clone().try_get_matches_from(args) {
            Ok(matches) => matches,
            Err(err)
                if matches!(
                    err.kind(),
                    ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
                ) =>
            {
                write!(stdout, "{err}").map_err(write_error)?;
                return Ok(0);
            }
            Err(err) => return Err(CliError::usage(err.to_string().trim())),
        };
        pretty = matches.get_flag("pretty");
        let (name, arguments) = matches
            .subcommand()
            .ok_or_else(|| CliError::usage("missing command"))?;
        if name == "help" {
            show_help(command, arguments, stdout)?;
            return Ok(0);
        }
        let timeout = Duration::from_secs_f64(
            *matches
                .get_one::<f64>("timeout")
                .expect("timeout has a validated default"),
        );
        let mut library_curl = crate::transport::Curl::new(timeout);
        let mut campus_curl = crate::transport::CampusCurl::new(timeout);
        let mut diagnose_curl = (name == "diagnose").then(|| {
            let seconds = arguments.get_one::<f64>("request_timeout").copied();
            let timeout = seconds.map_or(timeout, |seconds| {
                Duration::from_secs_f64(seconds.min(120.0)).max(Duration::from_millis(1))
            });
            crate::transport::CampusCurl::for_diagnose(timeout)
        });
        let mut release_curl = crate::transport::ReleaseCurl::new(
            timeout,
            name == "update" && !arguments.get_flag("check"),
        );
        // An injected public transport marks the entire dispatch as a test;
        // the first injected transport then handles all non-library commands.
        let transport: &mut dyn Transport = match public {
            Some(transport) if uses_public_library(name) => transport,
            Some(_) => offline,
            None if uses_public_library(name) => &mut library_curl,
            None if uses_campus_transport(name, arguments) => &mut campus_curl,
            None if name == "diagnose" => diagnose_curl.as_mut().expect("diagnose has a transport"),
            None if name == "update" => &mut release_curl,
            None => offline,
        };
        let mut context = Context::from_environment(transport)?;
        let (_, handler) = routes
            .iter()
            .find(|(key, _)| key == name)
            .ok_or_else(|| CliError::usage("unknown command"))?;
        let outcome = handler(name, arguments, &mut context)?;
        let Some(mut object) = outcome.data.as_object().cloned() else {
            return Err(CliError::new(
                5,
                "upstream",
                "expected command JSON object",
                "",
            ));
        };
        object.entry("ok").or_insert(Value::Bool(true));
        object.insert("schema".into(), json!(SCHEMA));
        write_json(stdout, &Value::Object(object), pretty)?;
        if let Some(diagnostic) = outcome.diagnostic {
            writeln!(stderr, "{}", redact(diagnostic, args)).map_err(write_error)?;
        }
        Ok(outcome.code)
    })();
    match result {
        Ok(code) => code,
        Err(err) => {
            let payload = json!({"schema": SCHEMA, "ok": false, "error": {
                "type": err.kind,
                "message": redact(err.message, args),
                "hint": redact(err.hint, args),
            }});
            let _ = write_json(stderr, &payload, pretty);
            err.code
        }
    }
}

pub fn main_entry() -> u8 {
    let mut offline = Offline;
    run(
        &std::env::args_os().collect::<Vec<_>>(),
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
        &mut offline,
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{Request, Response, testing::Scripted};

    #[test]
    fn offline_transport_records_even_an_unmatched_write() {
        let mut transport = Scripted::default();
        transport.expected.push_back((
            "GET",
            "https://example.invalid/expected".into(),
            Ok(Response {
                status: 200,
                headers: vec![],
                body: vec![],
            }),
        ));
        let unexpected = Request {
            method: "POST",
            url: "https://example.invalid/unexpected".into(),
            headers: vec![],
            body: vec![7],
        };
        assert!(transport.send(unexpected).is_err());
        assert_eq!(transport.calls.len(), 1);
        assert_eq!(transport.calls[0].method, "POST");
        assert_eq!(transport.calls[0].body, [7]);
    }

    #[test]
    fn parser_redacts_password_in_usage_errors() {
        let args = ["bupt", "--password=fake-secret"].map(OsString::from);
        let mut out = vec![];
        let mut err = vec![];
        let mut offline = Scripted::default();
        let mut public = Scripted::default();
        assert_eq!(
            run(&args, &mut out, &mut err, &mut offline, Some(&mut public)),
            2
        );
        assert!(out.is_empty());
        assert!(!String::from_utf8(err).unwrap().contains("fake-secret"));
        assert!(offline.calls.is_empty());
        assert!(public.calls.is_empty());
    }

    #[test]
    fn library_search_uses_the_injected_public_transport() {
        let args = ["bupt", "library", "search", "synthetic"].map(OsString::from);
        let mut out = vec![];
        let mut err = vec![];
        let mut offline = Scripted::default();
        let mut public = Scripted::default();
        public.expected.push_back((
            "POST",
            "https://opac.bupt.edu.cn/search-full-result.json".into(),
            Ok(Response {
                status: 200,
                headers: vec![],
                body: br#"{"result":{"code":0},"total":0,"pageNo":1,"totalPage":0,"data":[]}"#
                    .to_vec(),
            }),
        ));
        assert_eq!(
            run(&args, &mut out, &mut err, &mut offline, Some(&mut public)),
            0
        );
        assert!(err.is_empty());
        assert!(offline.calls.is_empty());
        assert_eq!(public.calls.len(), 1);
        let document: Value = serde_json::from_slice(&out).expect("parse CLI output");
        assert_eq!(document["schema"], "bupt.v1");
        assert_eq!(document["ok"], true);
        assert_eq!(document["count"], 0);
    }

    #[test]
    fn release_check_handles_a_redirect_through_the_cli_without_following_it() {
        let args = ["bupt", "update", "--check", "--repo", "owner/repo"].map(OsString::from);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut scoped = Scripted::default();
        let mut library = Scripted::default();
        scoped.expected.push_back((
            "GET",
            "https://github.com/owner/repo/releases/latest".into(),
            Ok(Response {
                status: 302,
                headers: vec![(
                    "location".into(),
                    "https://github.com/owner/repo/releases/tag/v10.0.0".into(),
                )],
                body: Vec::new(),
            }),
        ));
        assert_eq!(
            run(
                &args,
                &mut stdout,
                &mut stderr,
                &mut scoped,
                Some(&mut library)
            ),
            0
        );
        assert!(stderr.is_empty());
        assert!(library.calls.is_empty());
        assert_eq!(scoped.calls.len(), 1);
        let value: Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(value["schema"], "bupt.v1");
        assert_eq!(value["latest"], "10.0.0");
        assert_eq!(value["update_available"], true);
    }

    #[test]
    fn private_transport_is_only_routed_to_the_first_cas_and_read_flows() {
        for (args, allowed) in [
            (&["bupt", "login"][..], true),
            (&["bupt", "courses"][..], true),
            (&["bupt", "homework", "--undone"][..], true),
            (&["bupt", "homework"][..], true),
            (&["bupt", "homework", "--course", "site-1"][..], true),
            (&["bupt", "homework", "show", "1"][..], true),
            (&["bupt", "homework", "verify", "1"][..], true),
            (&["bupt", "homework", "--new", "7"][..], true),
            (&["bupt", "homework", "submit", "1"][..], false),
            (&["bupt", "homework", "submit", "1", "--dry-run"][..], true),
            (
                &["bupt", "homework", "submit", "1", "--i-confirm-submit"][..],
                true,
            ),
            (&["bupt", "homework", "download", "1"][..], true),
            (&["bupt", "electricity", "buildings"][..], true),
            (&["bupt", "schedule", "update"][..], true),
            (&["bupt", "exams"][..], true),
            (&["bupt", "scores"][..], true),
            (&["bupt", "classrooms"][..], true),
            (&["bupt", "notices"][..], true),
            (&["bupt", "card"][..], true),
            (&["bupt", "status"][..], false),
            (&["bupt", "diagnose"][..], false),
            (&["bupt", "library", "search", "example"][..], false),
        ] {
            let (command, _) = command_tree();
            let matches = command.try_get_matches_from(args).unwrap();
            let (name, arguments) = matches.subcommand().unwrap();
            assert_eq!(uses_campus_transport(name, arguments), allowed);
        }
    }

    #[test]
    fn only_the_library_lane_uses_the_public_network() {
        let public: Vec<String> = LANES
            .iter()
            .flat_map(|(commands, _)| commands())
            .map(|command| command.get_name().to_string())
            .filter(|name| uses_public_library(name))
            .collect();
        assert_eq!(public, ["library"]);
    }
}
