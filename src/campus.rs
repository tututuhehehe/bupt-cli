use crate::cli::{CliError, CliResult, Context, Outcome};
use crate::identity;
use clap::parser::ValueSource;
use clap::{Arg, ArgAction, ArgMatches, Command};
use std::path::Path;

mod dorm;
mod electricity;
mod library;
mod portal;
mod portal_session;
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
            )
            .subcommand(
                Command::new("search")
                    .about("Search notice titles")
                    .arg(Arg::new("keyword").required(true).value_name("KEYWORD"))
                    .arg(
                        Arg::new("count")
                            .long("count")
                            .value_parser(non_negative_u64)
                            .default_value("10"),
                    ),
            )
            .subcommand(
                Command::new("download")
                    .about("Save the attachments of one notice")
                    .arg(Arg::new("url").required(true).value_name("URL"))
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
            ),
        Command::new("card").about("Read campus card balance"),
    ]
}

pub(crate) fn run(
    name: &str,
    matches: &ArgMatches,
    context: &mut Context<'_>,
) -> CliResult<Outcome> {
    if name == "electricity" {
        let config_dir = context.config_dir.clone();
        let mut session = session::ElectricitySession::new(context);
        return run_electricity(matches, &config_dir, &mut session);
    }
    if matches!(name, "notices" | "card") {
        let config_dir = context.config_dir.clone();
        let home_dir = context.home_dir.clone();
        let mut session = portal_session::PortalSession::new(context);
        return if name == "notices" {
            run_notices(matches, &config_dir, &home_dir, &mut session)
        } else {
            run_card(&config_dir, &mut session)
        };
    }
    match name {
        "library" => run_library(matches, context),
        _ => Err(CliError::usage("unknown campus command")),
    }
}

/// Exercise the command handlers with a scripted session in tests.
#[cfg(test)]
fn run_with_session(
    name: &str,
    matches: &ArgMatches,
    context: &mut Context<'_>,
    session: &mut dyn SessionDriver,
) -> CliResult<Outcome> {
    match name {
        "library" => run_library(matches, context),
        "electricity" => run_electricity(matches, &context.config_dir, session),
        "notices" => run_notices(matches, &context.config_dir, &context.home_dir, session),
        "card" => run_card(&context.config_dir, session),
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
    config_dir: &Path,
    session: &mut dyn SessionDriver,
    service: Service,
) -> CliResult<()> {
    identity::require_authenticated_profile(config_dir)?;
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
    config_dir: &Path,
    session: &mut dyn SessionDriver,
) -> CliResult<Outcome> {
    if let Some(("buildings", arguments)) = matches.subcommand() {
        let requested = arguments
            .get_one::<String>("area")
            .map(String::as_str)
            .unwrap_or("shahe");
        let area = dorm::resolve_area(requested)?;
        require_session(config_dir, session, Service::Electricity)?;
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
    let dorm = dorm::resolve_dorm(&flags, &dorm::dorm_env(), config_dir)?;
    require_session(config_dir, session, Service::Electricity)?;
    Ok(outcome(electricity::query_with_session(
        session, &dorm, threshold,
    )?))
}

/// `--type` and `--count` shape a listing. A command that works on one notice
/// takes neither, and silently ignoring them would look like they had an effect.
fn reject_listing_flags(matches: &ArgMatches, command: &str) -> CliResult<()> {
    for (flag, id) in [("--type", "type"), ("--count", "count")] {
        if matches.value_source(id) == Some(ValueSource::CommandLine) {
            return Err(CliError::new(
                2,
                "usage",
                format!("notice {command} does not take {flag}"),
                "`--type` and `--count` shape `bupt notices`; this command works on one notice URL",
            ));
        }
    }
    Ok(())
}

fn run_notices(
    matches: &ArgMatches,
    config_dir: &Path,
    home_dir: &Path,
    session: &mut dyn SessionDriver,
) -> CliResult<Outcome> {
    match matches.subcommand() {
        Some(("show", arguments)) => {
            reject_listing_flags(matches, "show")?;
            let url = arguments
                .get_one::<String>("url")
                .map(String::as_str)
                .unwrap_or("");
            // Reject other destinations before loading any credentials or session.
            let url = portal::validate_notice_url(url)?;
            let max_chars = *arguments
                .get_one::<usize>("max-chars")
                .expect("max-chars has a default");
            require_session(config_dir, session, Service::Portal)?;
            return Ok(outcome(portal::fetch_notice_detail_with_session(
                session, &url, max_chars,
            )?));
        }
        Some(("search", arguments)) => {
            if matches.get_one::<String>("type").is_some() {
                return Err(CliError::new(
                    2,
                    "usage",
                    "notice search does not take --type",
                    "it searches 通知公告; choose notice, news, rules, or ways only when listing",
                ));
            }
            let keyword = arguments
                .get_one::<String>("keyword")
                .map(String::as_str)
                .unwrap_or("");
            let keyword = portal::validate_keyword(keyword)?;
            let count = *arguments
                .get_one::<u64>("count")
                .expect("count has a default");
            require_session(config_dir, session, Service::Portal)?;
            return Ok(outcome(portal::fetch_notice_search_with_session(
                session, &keyword, count,
            )?));
        }
        Some(("download", arguments)) => {
            reject_listing_flags(matches, "download")?;
            let url = arguments
                .get_one::<String>("url")
                .map(String::as_str)
                .unwrap_or("");
            // Reject other destinations before loading any credentials or session.
            let url = portal::validate_notice_url(url)?;
            let dir = crate::files::expand_dir(
                arguments
                    .get_one::<String>("dir")
                    .map(String::as_str)
                    .unwrap_or("."),
                home_dir,
            );
            let force = arguments.get_flag("force");
            require_session(config_dir, session, Service::Portal)?;
            return Ok(outcome(portal::fetch_notice_download_with_session(
                session, &url, &dir, force,
            )?));
        }
        Some(_) => return Err(CliError::usage("unknown notices command")),
        None => {}
    }

    let types = portal::resolve_types(matches.get_one::<String>("type").map(String::as_str))?;
    let count = *matches
        .get_one::<u64>("count")
        .expect("count has a default");
    require_session(config_dir, session, Service::Portal)?;
    Ok(outcome(portal::fetch_notices_with_session(
        session,
        types.as_deref(),
        count,
    )?))
}

fn run_card(config_dir: &Path, session: &mut dyn SessionDriver) -> CliResult<Outcome> {
    require_session(config_dir, session, Service::Portal)?;
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
            self.queue_response(method, url, 200, &[], body.into_bytes());
        }

        fn queue_response(
            &mut self,
            method: &'static str,
            url: String,
            status: u16,
            headers: &[(&str, &str)],
            body: Vec<u8>,
        ) {
            self.transport.expected.push_back((
                method,
                url,
                Ok(Response {
                    status,
                    headers: headers
                        .iter()
                        .map(|(key, value)| ((*key).into(), (*value).into()))
                        .collect(),
                    body,
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
    fn notice_search_reads_the_notice_tree_then_the_first_result_page() {
        let dirs = TestDirs::new("notice-search");
        let before = dirs.credentials();
        let list = "<input name='wbtreeids' value=1155,1158>";
        let page = "<span class='p_t'>共1条</span><a href='xntz_content.jsp?urltype=news.NewsContentUrl&wbtreeid=2149&wbnewsid=143528'>奖学金通知</a><span>2026-11-20</span>";
        let url = portal::search_url("奖学金", "1155,1158").unwrap();
        let mut session = ScriptedSession::new();
        session.queue(
            "GET",
            portal::list_url(portal::NoticeType::Notice),
            list.into(),
        );
        session.queue("GET", url, page.into());
        let mut transport = Scripted::default();
        let mut context = context(&dirs, &mut transport);
        let outcome = run_with_session(
            "notices",
            &matches("notices", &["search", "奖学金", "--count", "10"]),
            &mut context,
            &mut session,
        )
        .expect("notice search");
        assert_eq!(outcome.data["keyword"], "奖学金");
        assert_eq!(outcome.data["total"], 1);
        assert_eq!(outcome.data["notices"][0]["title"], "奖学金通知");
        assert_eq!(session.routes.len(), 2);
        assert!(session.transport.expected.is_empty());
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

    /// The attachment link the fixture notice page renders, in the shape the
    /// portal itself uses.
    const ATTACHMENT_URL: &str = "http://my.bupt.edu.cn/system/_content/download.jsp?urltype=news.DownloadAttachUrl&owner=1664271694&wbfileid=3251628";

    fn download_dir(dirs: &TestDirs) -> PathBuf {
        dirs.root.join("out")
    }

    fn saved_files(dirs: &TestDirs) -> Vec<String> {
        let dir = download_dir(dirs);
        let mut names: Vec<String> = fs::read_dir(&dir)
            .expect("read download dir")
            .map(|entry| {
                entry
                    .expect("dir entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    #[test]
    fn notices_download_rejects_listing_flags() {
        // Both flags shape a listing. Accepting one and silently dropping the
        // other would look like it had an effect.
        let dirs = TestDirs::new("notice-download-flags");
        for (arguments, flag) in [
            (vec!["--type", "rules", "download", DETAIL_URL], "--type"),
            (vec!["--count", "5", "download", DETAIL_URL], "--count"),
            (vec!["--count", "5", "show", DETAIL_URL], "--count"),
        ] {
            let mut transport = Scripted::default();
            let mut ctx = context(&dirs, &mut transport);
            let mut session = ScriptedSession::new();
            let error = run_with_session(
                "notices",
                &matches("notices", &arguments),
                &mut ctx,
                &mut session,
            )
            .err()
            .expect("listing flags are rejected");
            assert_eq!((error.code, error.kind), (2, "usage"), "{arguments:?}");
            assert!(
                error.message.contains(flag),
                "{arguments:?}: {:?}",
                error.message
            );
            assert!(session.transport.calls.is_empty());
            assert_eq!(session.renew_calls, 0);
        }
    }

    #[test]
    fn notices_download_saves_the_attachment_under_its_link_text() {
        let dirs = TestDirs::new("notice-download");
        let before = dirs.credentials();
        let mut session = ScriptedSession::new();
        session.queue(
            "GET",
            DETAIL_URL.into(),
            fixture("portal_notice_detail.html"),
        );
        session.queue_response(
            "GET",
            ATTACHMENT_URL.into(),
            200,
            &[],
            b"PK\x03\x04fixture".to_vec(),
        );
        let mut transport = Scripted::default();
        let mut context = context(&dirs, &mut transport);
        let dir = download_dir(&dirs);

        let outcome = run_with_session(
            "notices",
            &matches(
                "notices",
                &["download", DETAIL_URL, "--dir", dir.to_str().unwrap()],
            ),
            &mut context,
            &mut session,
        )
        .expect("notices download");

        assert_eq!(outcome.code, 0);
        assert_eq!(outcome.data["url"], DETAIL_URL);
        assert_eq!(outcome.data["title"], "示例通知一");
        assert_eq!(outcome.data["count_saved"], 1);
        assert_eq!(outcome.data["count_skipped"], 0);
        let saved = &outcome.data["saved"][0];
        assert_eq!(saved["title"], "示例附件.pdf");
        assert_eq!(saved["size"], 11);
        assert_eq!(saved_files(&dirs), ["示例附件.pdf"]);
        assert_eq!(
            fs::read(dir.join("示例附件.pdf")).expect("saved file"),
            b"PK\x03\x04fixture"
        );
        assert_eq!(session.routes.len(), 2);
        assert!(session.routes.iter().all(|route| {
            session
                .transport
                .calls
                .iter()
                .any(|call| route.accepts(call))
        }));
        // The attachment request carries the notice page as its referer; without
        // one the portal answers with a captcha page instead of the file.
        assert_eq!(
            session.transport.calls[1].headers,
            vec![("Referer".to_owned(), DETAIL_URL.to_owned())]
        );
        assert_no_state_writes(&dirs, &before);
    }

    #[test]
    fn notices_download_skips_an_existing_file_and_replaces_it_with_force() {
        let dirs = TestDirs::new("notice-download-force");
        let dir = download_dir(&dirs);
        fs::create_dir_all(&dir).expect("create download dir");
        fs::write(dir.join("示例附件.pdf"), b"old").expect("seed existing file");
        let page = fixture("portal_notice_detail.html");
        let mut session = ScriptedSession::new();
        session.queue("GET", DETAIL_URL.into(), page.clone());
        // Deliberately no attachment response: the skip must be decided from the
        // filesystem, before any transfer.
        let mut transport = Scripted::default();
        let mut first_ctx = context(&dirs, &mut transport);

        let outcome = run_with_session(
            "notices",
            &matches(
                "notices",
                &["download", DETAIL_URL, "--dir", dir.to_str().unwrap()],
            ),
            &mut first_ctx,
            &mut session,
        )
        .expect("notices download skip");

        assert_eq!(outcome.data["count_saved"], 0);
        assert_eq!(outcome.data["count_skipped"], 1);
        assert_eq!(outcome.data["skipped"][0]["reason"], "exists");
        assert_eq!(fs::read(dir.join("示例附件.pdf")).unwrap(), b"old");
        // The page is read; the attachment is not requested at all.
        let requested: Vec<&str> = session
            .transport
            .calls
            .iter()
            .map(|request| request.url.as_str())
            .collect();
        assert!(
            !requested.contains(&ATTACHMENT_URL),
            "an existing file must be skipped before the transfer; requests={requested:?}"
        );

        let mut session = ScriptedSession::new();
        session.queue("GET", DETAIL_URL.into(), page);
        session.queue_response("GET", ATTACHMENT_URL.into(), 200, &[], b"new".to_vec());
        let mut transport = Scripted::default();
        let mut second_ctx = context(&dirs, &mut transport);
        let outcome = run_with_session(
            "notices",
            &matches(
                "notices",
                &[
                    "download",
                    DETAIL_URL,
                    "--dir",
                    dir.to_str().unwrap(),
                    "--force",
                ],
            ),
            &mut second_ctx,
            &mut session,
        )
        .expect("notices download force");

        assert_eq!(outcome.data["count_saved"], 1);
        assert_eq!(fs::read(dir.join("示例附件.pdf")).unwrap(), b"new");
        assert_eq!(saved_files(&dirs), ["示例附件.pdf"]);
    }

    #[test]
    fn notices_download_refuses_a_captcha_page_without_writing_a_file() {
        let dirs = TestDirs::new("notice-download-captcha");
        let mut session = ScriptedSession::new();
        session.queue(
            "GET",
            DETAIL_URL.into(),
            fixture("portal_notice_detail.html"),
        );
        session.queue_response(
            "GET",
            ATTACHMENT_URL.into(),
            200,
            &[("Content-Type", "text/html;charset=UTF-8")],
            "<html><title>附件下载</title><body>请输入验证码下载附件</body></html>".into(),
        );
        let mut transport = Scripted::default();
        let mut context = context(&dirs, &mut transport);
        let dir = download_dir(&dirs);

        let error = run_with_session(
            "notices",
            &matches(
                "notices",
                &["download", DETAIL_URL, "--dir", dir.to_str().unwrap()],
            ),
            &mut context,
            &mut session,
        )
        .err()
        .expect("captcha page must not be saved");

        assert_eq!((error.code, error.kind), (5, "upstream"));
        assert!(error.message.contains("captcha"), "{}", error.message);
        assert!(saved_files(&dirs).is_empty(), "no file may be written");
    }

    #[test]
    fn notices_download_reports_an_unsupported_link_instead_of_failing() {
        let dirs = TestDirs::new("notice-download-unsupported");
        let page = "<title>混合附件</title><div id='vsb_content'><p>正文</p>
<a href='/system/_content/download.jsp?urltype=news.DownloadAttachUrl&amp;owner=1664271694&amp;wbfileid=3251628'>可下载.pdf</a>
<a href='/system/_content/download.jsp?id=FIXTURE-ATTACH-1'>旧式链接.pdf</a></div>";
        let mut session = ScriptedSession::new();
        session.queue("GET", DETAIL_URL.into(), page.into());
        session.queue_response("GET", ATTACHMENT_URL.into(), 200, &[], b"data".to_vec());
        let mut transport = Scripted::default();
        let mut context = context(&dirs, &mut transport);
        let dir = download_dir(&dirs);

        let outcome = run_with_session(
            "notices",
            &matches(
                "notices",
                &["download", DETAIL_URL, "--dir", dir.to_str().unwrap()],
            ),
            &mut context,
            &mut session,
        )
        .expect("unsupported link is reported, not fatal");

        assert_eq!(outcome.data["count_saved"], 1);
        assert_eq!(outcome.data["count_skipped"], 1);
        assert_eq!(outcome.data["skipped"][0]["reason"], "unsupported_link");
        assert_eq!(outcome.data["skipped"][0]["title"], "旧式链接.pdf");
        assert_eq!(saved_files(&dirs), ["可下载.pdf"]);
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
    fn card_requires_a_bound_session_before_network() {
        let dirs = TestDirs::new("pending");
        let mut transport = Scripted::default();
        let error = {
            let mut context = context(&dirs, &mut transport);
            match run("card", &matches("card", &[]), &mut context) {
                Ok(_) => panic!("a profile without a bound session must not succeed"),
                Err(error) => error,
            }
        };
        let expected = if cfg!(unix) {
            (3, "auth")
        } else {
            (5, "upstream")
        };
        assert_eq!((error.code, error.kind), expected);
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
