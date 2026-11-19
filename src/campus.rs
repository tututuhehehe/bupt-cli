use crate::cli::{CliError, CliResult, Context, Outcome};
use crate::identity;
use clap::{Arg, ArgAction, ArgMatches, Command};

mod dorm;
mod electricity;
mod library;
mod portal;
mod session;

#[allow(unused_imports)] // part of the parent-facing session seam
pub(crate) use session::{Route, Service, SessionDriver};

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
    let mut session = session::PendingSession;
    run_with_session(name, matches, context, &mut session)
}

/// Command dispatcher for a parent-supplied authenticated session driver.
///
/// Kept separate from [`run`] so the parent CLI/session layer can attach its
/// credential, cookie, and renewal implementation without changing any campus
/// request or parser. The default [`run`] uses a pending driver and therefore
/// preserves the migration build's upstream-5 failure after local validation.
pub(crate) fn run_with_session(
    name: &str,
    matches: &ArgMatches,
    context: &mut Context<'_>,
    session: &mut dyn SessionDriver,
) -> CliResult<Outcome> {
    match name {
        "library" => run_library(matches, context),
        "electricity" => run_electricity(matches, context, session),
        "notices" => run_notices(matches, context, session),
        "card" => run_card(context, session),
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

fn outcome(data: serde_json::Value) -> Outcome {
    Outcome {
        data,
        code: 0,
        diagnostic: None,
    }
}

fn require_session(
    context: &Context<'_>,
    session: &mut dyn SessionDriver,
    service: Service,
) -> CliResult<()> {
    identity::require_authenticated_profile(&context.config_dir)?;
    session.ensure(service)
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
    Ok(outcome(data))
}

fn run_electricity(
    matches: &ArgMatches,
    context: &mut Context<'_>,
    session: &mut dyn SessionDriver,
) -> CliResult<Outcome> {
    if let Some(("buildings", arguments)) = matches.subcommand() {
        let requested = arguments
            .get_one::<String>("area")
            .map(String::as_str)
            .unwrap_or("shahe");
        let area = dorm::resolve_area(requested)?;
        require_session(context, session, Service::Electricity)?;
        return Ok(outcome(electricity::get_buildings_with_session(
            session, area,
        )?));
    }
    if matches.get_one::<String>("area").is_some() {
        return Err(CliError::new(
            2,
            "usage",
            "--area is only valid for `bupt electricity buildings`",
            "try `bupt electricity buildings --area shahe` or `bupt electricity buildings --area xitucheng`",
        ));
    }

    let threshold = *matches
        .get_one::<f64>("threshold")
        .expect("threshold has a default");
    let flags = dorm::DormFlags {
        building: matches.get_one::<String>("building").cloned(),
        room: matches.get_one::<String>("room").cloned(),
        floor: matches.get_one::<String>("floor").cloned(),
    };
    let dorm = dorm::resolve_dorm(&flags, &dorm::dorm_env(), &context.config_dir)?;
    require_session(context, session, Service::Electricity)?;
    Ok(outcome(electricity::query_with_session(
        session, &dorm, threshold,
    )?))
}

fn run_notices(
    matches: &ArgMatches,
    context: &mut Context<'_>,
    session: &mut dyn SessionDriver,
) -> CliResult<Outcome> {
    if let Some(("show", arguments)) = matches.subcommand() {
        let url = arguments
            .get_one::<String>("url")
            .map(String::as_str)
            .unwrap_or("");
        // Reject other destinations before loading any credentials or session.
        let url = portal::validate_notice_url(url)?;
        let max_chars = *arguments
            .get_one::<usize>("max-chars")
            .expect("max-chars has a default");
        require_session(context, session, Service::Portal)?;
        return Ok(outcome(portal::fetch_notice_detail_with_session(
            session, &url, max_chars,
        )?));
    }

    let types = portal::resolve_types(matches.get_one::<String>("type").map(String::as_str))?;
    let count = *matches
        .get_one::<u64>("count")
        .expect("count has a default");
    require_session(context, session, Service::Portal)?;
    Ok(outcome(portal::fetch_notices_with_session(
        session,
        types.as_deref(),
        count,
    )?))
}

fn run_card(context: &mut Context<'_>, session: &mut dyn SessionDriver) -> CliResult<Outcome> {
    require_session(context, session, Service::Portal)?;
    Ok(outcome(portal::fetch_card_with_session(session)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::testing::Scripted;
    use crate::transport::{Response, Transport};
    use serde_json::Value;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    const CREDENTIALS: &str =
        r#"{"username":"2026000090","password":"TEST-ONLY-CAMPUS-PASSWORD-6f2a"}"#;
    const DETAIL_URL: &str = "http://my.bupt.edu.cn/2026/1101/c1154a000001/page.htm";

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    struct TestDirs {
        root: PathBuf,
    }

    impl TestDirs {
        fn new(label: &str) -> Self {
            let serial = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "bupt-campus-{label}-{}-{serial}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(root.join("config")).expect("create config dir");
            fs::create_dir_all(root.join("cache")).expect("create cache dir");
            fs::write(root.join("config/credentials.json"), CREDENTIALS)
                .expect("write synthetic credentials");
            Self { root }
        }

        fn config_dir(&self) -> PathBuf {
            self.root.join("config")
        }

        fn cache_dir(&self) -> PathBuf {
            self.root.join("cache")
        }

        fn credentials(&self) -> Vec<u8> {
            fs::read(self.config_dir().join("credentials.json")).expect("read credentials")
        }
    }

    impl Drop for TestDirs {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    struct ScriptedSession {
        transport: Scripted,
        routes: Vec<Route>,
        ensure_calls: usize,
        ensure_services: Vec<Service>,
        renew_calls: usize,
        renew_services: Vec<Service>,
    }

    impl ScriptedSession {
        fn new() -> Self {
            Self {
                transport: Scripted::default(),
                routes: Vec::new(),
                ensure_calls: 0,
                ensure_services: Vec::new(),
                renew_calls: 0,
                renew_services: Vec::new(),
            }
        }

        fn queue(&mut self, method: &'static str, url: String, body: String) {
            self.transport.expected.push_back((
                method,
                url,
                Ok(Response {
                    status: 200,
                    headers: Vec::new(),
                    body: body.into_bytes(),
                }),
            ));
        }
    }

    impl SessionDriver for ScriptedSession {
        fn ensure(&mut self, service: Service) -> CliResult<()> {
            self.ensure_calls += 1;
            self.ensure_services.push(service);
            Ok(())
        }

        fn send(
            &mut self,
            route: Route,
            request: crate::transport::Request,
        ) -> CliResult<Response> {
            self.routes.push(route);
            self.transport.send(request)
        }

        fn renew(&mut self, service: Service) -> CliResult<()> {
            self.renew_calls += 1;
            self.renew_services.push(service);
            Ok(())
        }
    }

    fn fixture(name: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/contracts/campus")
            .join(name);
        fs::read_to_string(path).expect("read synthetic fixture")
    }

    fn matches(command_name: &str, arguments: &[&str]) -> ArgMatches {
        let command = commands()
            .into_iter()
            .find(|command| command.get_name() == command_name)
            .expect("campus command");
        let mut argv = vec![command_name.to_string()];
        argv.extend(arguments.iter().map(|argument| (*argument).to_string()));
        command
            .try_get_matches_from(argv)
            .expect("parse campus arguments")
    }

    fn context<'a>(dirs: &TestDirs, transport: &'a mut Scripted) -> Context<'a> {
        Context {
            home_dir: dirs.root.clone(),
            config_dir: dirs.config_dir(),
            cache_dir: dirs.cache_dir(),
            timeout: Duration::from_secs(20),
            transport,
        }
    }

    fn assert_no_state_writes(dirs: &TestDirs, credentials_before: &[u8]) {
        assert_eq!(
            dirs.credentials(),
            credentials_before,
            "campus command must not rewrite credentials"
        );
        assert!(
            !dirs.cache_dir().join("session.json").exists(),
            "campus command must not create a session file"
        );
        assert!(
            fs::read_dir(dirs.cache_dir())
                .expect("read cache dir")
                .next()
                .is_none(),
            "campus command must not write cache state"
        );
    }

    #[test]
    fn notices_success_runs_through_the_command_without_writing_state() {
        let dirs = TestDirs::new("notices");
        let before = dirs.credentials();
        let mut session = ScriptedSession::new();
        session.queue(
            "GET",
            portal::list_url(portal::NoticeType::Notice),
            fixture("portal_notices_notice.html"),
        );
        session.queue(
            "GET",
            portal::list_url(portal::NoticeType::News),
            fixture("portal_notices_news.html"),
        );
        let mut transport = Scripted::default();
        let mut context = context(&dirs, &mut transport);

        let outcome = run_with_session(
            "notices",
            &matches("notices", &["--count", "1"]),
            &mut context,
            &mut session,
        )
        .expect("notices success");

        assert_eq!(outcome.code, 0);
        assert_eq!(outcome.data["count"], 1);
        assert_eq!(outcome.data["notices"][0]["title"], "示例新闻一");
        assert_eq!(outcome.data["notices"][0]["type"], "news");
        assert_eq!(session.ensure_calls, 1);
        assert_eq!(session.ensure_services, [Service::Portal]);
        assert_eq!(session.renew_calls, 0);
        assert_eq!(session.routes.len(), 2);
        assert!(session.routes[0].accepts(&session.transport.calls[0]));
        assert!(session.routes[1].accepts(&session.transport.calls[1]));
        assert!(transport.calls.is_empty());
        assert_no_state_writes(&dirs, &before);
    }

    #[test]
    fn notice_detail_success_runs_through_the_command_without_writing_state() {
        let dirs = TestDirs::new("notice-detail");
        let before = dirs.credentials();
        let mut session = ScriptedSession::new();
        session.queue(
            "GET",
            DETAIL_URL.into(),
            fixture("portal_notice_detail.html"),
        );
        let mut transport = Scripted::default();
        let mut context = context(&dirs, &mut transport);

        let outcome = run_with_session(
            "notices",
            &matches("notices", &["show", DETAIL_URL, "--max-chars", "6"]),
            &mut context,
            &mut session,
        )
        .expect("notice detail success");

        assert_eq!(outcome.code, 0);
        assert_eq!(outcome.data["notice"]["title"], "示例通知一");
        assert_eq!(outcome.data["notice"]["content_text"], "第一段示例正");
        assert_eq!(outcome.data["notice"]["content_truncated"], true);
        assert_eq!(session.ensure_calls, 1);
        assert_eq!(session.renew_calls, 0);
        assert_eq!(session.routes.len(), 1);
        assert!(session.routes[0].accepts(&session.transport.calls[0]));
        assert_no_state_writes(&dirs, &before);
    }

    #[test]
    fn card_success_runs_through_the_command_without_writing_state() {
        let dirs = TestDirs::new("card");
        let before = dirs.credentials();
        let mut session = ScriptedSession::new();
        session.queue("GET", portal::CARD_URL.into(), fixture("portal_card.json"));
        let mut transport = Scripted::default();
        let mut context = context(&dirs, &mut transport);

        let outcome = run_with_session("card", &matches("card", &[]), &mut context, &mut session)
            .expect("card success");

        assert_eq!(outcome.code, 0);
        assert_eq!(outcome.data["student_id"], "fixture-student");
        assert_eq!(outcome.data["main_wallet"], 123.45);
        assert_eq!(outcome.data["subsidy"], 6.78);
        assert_eq!(outcome.data["unread_mail"], 3);
        assert_eq!(session.ensure_calls, 1);
        assert_eq!(session.renew_calls, 0);
        assert_eq!(session.routes.len(), 1);
        assert!(session.routes[0].accepts(&session.transport.calls[0]));
        assert_no_state_writes(&dirs, &before);
    }

    #[test]
    fn buildings_success_runs_through_the_command_without_writing_state() {
        let dirs = TestDirs::new("buildings");
        let before = dirs.credentials();
        let mut session = ScriptedSession::new();
        session.queue(
            "POST",
            electricity::request_url("/part"),
            fixture("electricity_buildings_shahe.json"),
        );
        let mut transport = Scripted::default();
        let mut context = context(&dirs, &mut transport);

        let outcome = run_with_session(
            "electricity",
            &matches("electricity", &["buildings", "--area", "shahe"]),
            &mut context,
            &mut session,
        )
        .expect("buildings success");

        assert_eq!(outcome.code, 0);
        assert_eq!(outcome.data["area"], "shahe");
        assert_eq!(outcome.data["count"], 2);
        assert_eq!(
            outcome.data["buildings"][0]["partmentName"],
            "沙河校区雁北园A楼"
        );
        assert_eq!(session.ensure_calls, 1);
        assert_eq!(session.ensure_services, [Service::Electricity]);
        assert_eq!(session.renew_calls, 0);
        assert_eq!(session.routes.len(), 1);
        assert!(session.routes[0].accepts(&session.transport.calls[0]));
        assert_eq!(
            String::from_utf8_lossy(&session.transport.calls[0].body),
            "areaid=2"
        );
        assert_no_state_writes(&dirs, &before);
    }

    #[test]
    fn electricity_success_runs_floor_room_and_balance_through_the_command() {
        let dirs = TestDirs::new("electricity");
        let before = dirs.credentials();
        let mut session = ScriptedSession::new();
        session.queue(
            "POST",
            electricity::request_url("/floor"),
            fixture("electricity_floors_xitucheng.json"),
        );
        session.queue(
            "POST",
            electricity::request_url("/drom"),
            fixture("electricity_rooms.json"),
        );
        session.queue(
            "POST",
            electricity::request_url("/search"),
            fixture("electricity_meter.json"),
        );
        let mut transport = Scripted::default();
        let mut context = context(&dirs, &mut transport);

        let outcome = run_with_session(
            "electricity",
            &matches("electricity", &[]),
            &mut context,
            &mut session,
        )
        .expect("electricity success");

        let data = &outcome.data;
        assert_eq!(data["building"], "学一楼");
        assert_eq!(data["floor"], "1");
        assert_eq!(data["room"], "101");
        assert_eq!(data["surplus"], 42.5);
        assert_eq!(data["remaining_kwh"], 88.54);
        assert_eq!(data["source"], "default");
        assert_eq!(data["low_balance"], false);
        assert_eq!(data["threshold"], 10.0);
        assert_eq!(session.ensure_calls, 1);
        assert_eq!(session.renew_calls, 0);
        assert_eq!(session.routes.len(), 3);
        for (route, call) in session.routes.iter().zip(&session.transport.calls) {
            assert!(route.accepts(call));
        }
        assert_eq!(
            String::from_utf8_lossy(&session.transport.calls[0].body),
            "partmentId=f8067fcf9c4f48c6b2f8e793d64b7b65&areaid=1"
        );
        assert_eq!(
            String::from_utf8_lossy(&session.transport.calls[1].body),
            "partmentId=f8067fcf9c4f48c6b2f8e793d64b7b65&floorId=fixture-floor-1&areaid=1"
        );
        assert_eq!(
            String::from_utf8_lossy(&session.transport.calls[2].body),
            "partmentId=f8067fcf9c4f48c6b2f8e793d64b7b65&floorId=fixture-floor-1&dromNumber=fixture-meter-101&areaid=1"
        );
        assert_no_state_writes(&dirs, &before);
    }

    #[test]
    fn portal_auth_failure_renews_once_and_restarts_the_list() {
        let dirs = TestDirs::new("portal-renew");
        let before = dirs.credentials();
        let mut session = ScriptedSession::new();
        session.queue(
            "GET",
            portal::list_url(portal::NoticeType::Notice),
            fixture("portal_login_page.html"),
        );
        session.queue(
            "GET",
            portal::list_url(portal::NoticeType::Notice),
            fixture("portal_notices_notice.html"),
        );
        session.queue(
            "GET",
            portal::list_url(portal::NoticeType::News),
            fixture("portal_notices_news.html"),
        );
        let mut transport = Scripted::default();
        let mut context = context(&dirs, &mut transport);

        let outcome = run_with_session(
            "notices",
            &matches("notices", &[]),
            &mut context,
            &mut session,
        )
        .expect("notices succeeds after one renewal");

        assert_eq!(outcome.data["count"], 3);
        assert_eq!(session.renew_calls, 1);
        assert_eq!(session.renew_services, [Service::Portal]);
        assert_eq!(session.routes.len(), 3);
        assert_no_state_writes(&dirs, &before);
    }

    #[test]
    fn electricity_auth_failure_renews_only_the_failed_post_once() {
        let dirs = TestDirs::new("electricity-renew");
        let before = dirs.credentials();
        let mut session = ScriptedSession::new();
        session.queue(
            "POST",
            electricity::request_url("/part"),
            fixture("electricity_auth_error.json"),
        );
        session.queue(
            "POST",
            electricity::request_url("/part"),
            fixture("electricity_buildings_shahe.json"),
        );
        let mut transport = Scripted::default();
        let mut context = context(&dirs, &mut transport);

        let outcome = run_with_session(
            "electricity",
            &matches("electricity", &["buildings"]),
            &mut context,
            &mut session,
        )
        .expect("buildings succeeds after one renewal");

        assert_eq!(outcome.data["count"], 2);
        assert_eq!(session.renew_calls, 1);
        assert_eq!(session.renew_services, [Service::Electricity]);
        assert_eq!(session.routes.len(), 2);
        assert_no_state_writes(&dirs, &before);
    }

    #[test]
    fn electricity_stops_after_one_renewal_when_auth_still_fails() {
        let dirs = TestDirs::new("electricity-auth-stop");
        let before = dirs.credentials();
        let mut session = ScriptedSession::new();
        session.queue(
            "POST",
            electricity::request_url("/part"),
            fixture("electricity_auth_error.json"),
        );
        session.queue(
            "POST",
            electricity::request_url("/part"),
            fixture("electricity_auth_error.json"),
        );
        let mut transport = Scripted::default();
        let mut context = context(&dirs, &mut transport);

        let error = match run_with_session(
            "electricity",
            &matches("electricity", &["buildings"]),
            &mut context,
            &mut session,
        ) {
            Ok(_) => panic!("second auth failure must stay an error"),
            Err(error) => error,
        };

        assert_eq!((error.code, error.kind), (3, "auth"));
        assert_eq!(session.renew_calls, 1);
        assert_eq!(session.routes.len(), 2);
        assert_no_state_writes(&dirs, &before);
    }

    #[test]
    fn default_pending_driver_keeps_the_local_auth_then_upstream_layering() {
        let dirs = TestDirs::new("pending");
        let mut transport = Scripted::default();
        let error = {
            let mut context = context(&dirs, &mut transport);
            match run("card", &matches("card", &[]), &mut context) {
                Ok(_) => panic!("pending driver must not succeed"),
                Err(error) => error,
            }
        };
        assert_eq!((error.code, error.kind), (5, "upstream"));
        assert!(transport.calls.is_empty());

        fs::remove_file(dirs.config_dir().join("credentials.json"))
            .expect("remove synthetic credentials");
        let error = {
            let mut context = context(&dirs, &mut transport);
            match run("card", &matches("card", &[]), &mut context) {
                Ok(_) => panic!("missing profile must stay an auth error"),
                Err(error) => error,
            }
        };
        assert_eq!((error.code, error.kind), (3, "auth"));
    }

    #[test]
    fn run_rejects_an_unknown_command_before_any_session_use() {
        let dirs = TestDirs::new("unknown");
        let mut session = ScriptedSession::new();
        let mut transport = Scripted::default();
        let mut context = context(&dirs, &mut transport);
        let command = Command::new("unknown");
        let parsed = command
            .try_get_matches_from(["unknown"])
            .expect("parse empty command");

        let error = match run_with_session("unknown", &parsed, &mut context, &mut session) {
            Ok(_) => panic!("unknown command must fail"),
            Err(error) => error,
        };
        assert_eq!((error.code, error.kind), (2, "usage"));
        assert_eq!(session.ensure_calls, 0);
        assert!(session.routes.is_empty());
    }

    #[test]
    fn command_json_shape_is_not_added_by_the_campus_layer() {
        let dirs = TestDirs::new("shape");
        let mut session = ScriptedSession::new();
        session.queue("GET", portal::CARD_URL.into(), fixture("portal_card.json"));
        let mut transport = Scripted::default();
        let mut context = context(&dirs, &mut transport);
        let outcome = run_with_session("card", &matches("card", &[]), &mut context, &mut session)
            .expect("card success");
        let object: &serde_json::Map<String, Value> =
            outcome.data.as_object().expect("object output");
        assert!(object.get("schema").is_none());
        assert!(object.get("ok").is_none());
    }
}
