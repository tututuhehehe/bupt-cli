use crate::cli::{CliError, CliResult, Context, Outcome};
use crate::identity::{self, LocalProfile, Role};
use crate::transport::Transport;
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::NamedTempFile;

mod graduate;
pub(crate) mod mobile_jwgl;

use graduate::GradPageSource as _;

const WEEKDAYS: [&str; 7] = [
    "星期一",
    "星期二",
    "星期三",
    "星期四",
    "星期五",
    "星期六",
    "星期日",
];

pub(crate) fn commands() -> Vec<Command> {
    vec![
        Command::new("schedule")
            .about("Read the campus timetable from the local snapshot")
            .arg(
                Arg::new("today")
                    .long("today")
                    .action(ArgAction::SetTrue)
                    .help("Only today's classes"),
            )
            .arg(
                Arg::new("tomorrow")
                    .long("tomorrow")
                    .action(ArgAction::SetTrue)
                    .help("Only tomorrow's classes"),
            )
            .arg(
                Arg::new("date")
                    .long("date")
                    .value_name("YYYY-MM-DD")
                    .value_parser(date_arg)
                    .help("Classes on one date"),
            )
            .arg(
                Arg::new("week")
                    .long("week")
                    .value_name("N")
                    .value_parser(week_arg)
                    .help("Classes in teaching week N"),
            )
            .arg(
                Arg::new("raw")
                    .long("raw")
                    .action(ArgAction::SetTrue)
                    .help("Dump the cached course grid"),
            )
            .subcommand(Command::new("update").about("Refresh the timetable snapshot"))
            .subcommand(Command::new("calendar").about("Show teaching weeks"))
            .subcommand(
                Command::new("export")
                    .about("Write an iCalendar file")
                    .arg(
                        Arg::new("out")
                            .long("out")
                            .value_name("PATH")
                            .help("Output file"),
                    )
                    .arg(
                        Arg::new("no-open-folder")
                            .long("no-open-folder")
                            .action(ArgAction::SetTrue)
                            .help("Do not reveal the file in the file manager"),
                    ),
            ),
        Command::new("exams").about("Show exam information for the account's role"),
        Command::new("scores")
            .about("Show undergraduate scores")
            .arg(
                Arg::new("semester")
                    .long("semester")
                    .value_name("TERM")
                    .help("Filter by semester"),
            ),
        Command::new("classrooms")
            .about("Show undergraduate classroom availability")
            .arg(
                Arg::new("campus")
                    .long("campus")
                    .value_name("CAMPUS")
                    .value_parser(["01", "04"])
                    .default_value("04")
                    .help("01 = 西土城, 04 = 沙河"),
            ),
    ]
}

pub(crate) fn run(
    command: &str,
    matches: &ArgMatches,
    ctx: &mut Context<'_>,
) -> CliResult<Outcome> {
    match command {
        "schedule" => run_schedule(matches, ctx),
        "exams" => run_exams(ctx),
        "scores" => run_scores(matches, ctx),
        "classrooms" => run_classrooms(matches, ctx),
        _ => Err(CliError::usage("unknown command")),
    }
}

fn run_schedule(matches: &ArgMatches, ctx: &mut Context<'_>) -> CliResult<Outcome> {
    if let Some((subcommand, sub)) = matches.subcommand() {
        return match subcommand {
            "update" => update_schedule(ctx),
            "calendar" => {
                let selected = current_selection(ctx, local_today()?, false)?;
                Ok(outcome(calendar_payload(&selected)))
            }
            "export" => export_schedule(sub, ctx),
            _ => Err(CliError::usage(format!(
                "unknown schedule subcommand: {subcommand}"
            ))),
        };
    }

    let today = local_today()?;
    let selected = current_selection(ctx, today, false)?;
    let snapshot = &selected.snapshot;
    if matches.get_flag("raw") {
        return Ok(outcome(json!({
            "term": snapshot.term,
            "role": snapshot.role,
            "from_cache": selected.from_cache,
            "count": snapshot.courses.len(),
            "courses": snapshot.courses,
        })));
    }

    let (start, end, label) = if let Some(date) = matches.get_one::<String>("date") {
        let day = Date::parse(date).expect("clap validated --date");
        ensure_date_in_term(snapshot, day)?;
        (day, day, date.clone())
    } else if matches.get_flag("today") {
        (today, today, today.iso())
    } else if matches.get_flag("tomorrow") {
        let day = today.add_days(1);
        // Past the term end this is an out-of-range day, not "no classes";
        // report the range exactly like --date does.
        ensure_date_in_term(snapshot, day)?;
        (day, day, day.iso())
    } else if let Some(week) = matches.get_one::<u32>("week") {
        let dates = snapshot
            .calendar
            .get(&i64::from(*week))
            .ok_or_else(|| refresh_required(format!("第 {week} 周不在缓存的学期范围内")))?;
        let mut valid = dates.iter().filter_map(|date| Date::parse(date));
        let start = valid
            .next()
            .ok_or_else(|| refresh_required(format!("第 {week} 周没有可用日期")))?;
        let end = valid.next_back().unwrap_or(start);
        (start, end, format!("第 {week} 周"))
    } else {
        let (start, end) = day_range(snapshot, today);
        (start, end, "本周".to_string())
    };

    let sessions = occurrences(snapshot, start, end);
    Ok(outcome(json!({
        "term": snapshot.term,
        "role": snapshot.role,
        "from_cache": selected.from_cache,
        "range": [start.iso(), end.iso()],
        "week": week_of(snapshot, start),
        "label": label,
        "count": sessions.len(),
        "classes": sessions.into_iter().map(|session| session.data).collect::<Vec<_>>(),
    })))
}

fn update_schedule(ctx: &mut Context<'_>) -> CliResult<Outcome> {
    let selected = current_selection(ctx, local_today()?, true)?;
    let snapshot = &selected.snapshot;
    let (start, end) =
        term_bounds(snapshot).ok_or_else(|| refresh_failed("刷新结果没有可用的学期日期范围"))?;
    Ok(outcome(json!({
        "updated": true,
        "role": snapshot.role,
        "source": snapshot.source,
        "term": snapshot.term,
        "courses": snapshot.courses.len(),
        "weeks": snapshot.calendar.len(),
        "range": [start.iso(), end.iso()],
        "path": ctx.cache_dir.join("timetable.json").display().to_string(),
    })))
}

fn run_exams(ctx: &mut Context<'_>) -> CliResult<Outcome> {
    let account = identity::require_authenticated_profile(&ctx.config_dir)?;
    match required_role(&account)? {
        Role::Graduate => {
            let config_dir = ctx.config_dir.clone();
            let mut source = graduate::HttpsPageSource::new(ctx)?;
            run_exams_with_source(&config_dir, &mut source)
        }
        Role::Undergraduate => {
            let mut auth = mobile_auth(ctx)?;
            run_exams_with_mobile(ctx.transport, &mut auth)
        }
    }
}

fn mobile_auth(ctx: &mut Context<'_>) -> CliResult<mobile_jwgl::NativeMobileJwglAuth> {
    identity::require_mobile_jwgl_token(ctx).map(mobile_jwgl::NativeMobileJwglAuth::new)
}

fn run_exams_with_mobile(
    transport: &mut dyn Transport,
    auth: &mut dyn mobile_jwgl::MobileJwglAuth,
) -> CliResult<Outcome> {
    let mut client = mobile_jwgl::MobileJwglClient::new(transport, auth);
    let weeks = client.exam_weeks()?;
    let exams = client.exam_schedule()?;
    let arrangements = client.exam_course_schedule()?;
    Ok(outcome(mobile_jwgl::exams_payload(
        &weeks,
        &exams,
        &arrangements,
    )))
}

fn run_exams_with_source(
    config_dir: &Path,
    source: &mut impl graduate::GradPageSource,
) -> CliResult<Outcome> {
    let account = identity::require_authenticated_profile(config_dir)?;
    let role = required_role(&account)?;
    if role != Role::Graduate {
        return Err(pending_remote("本科考试数据源"));
    }
    let pages = source.collect(&[graduate::GradPage::Exams])?;
    let html = pages.exams.ok_or_else(|| {
        CliError::new(
            5,
            "upstream",
            "研究生考试页面缺失，无法读取考试安排",
            "重试一次；不会伪造考试日期、场次或地点。",
        )
    })?;
    graduate::parse_exams(&html).map(outcome)
}

fn run_scores(matches: &ArgMatches, ctx: &mut Context<'_>) -> CliResult<Outcome> {
    let account = identity::require_authenticated_profile(&ctx.config_dir)?;
    require_undergraduate(required_role(&account)?, "scores")?;
    let semester = matches.get_one::<String>("semester").map(String::as_str);
    let mut auth = mobile_auth(ctx)?;
    run_scores_with_mobile(ctx.transport, &mut auth, semester)
}

fn run_scores_with_mobile(
    transport: &mut dyn Transport,
    auth: &mut dyn mobile_jwgl::MobileJwglAuth,
    semester: Option<&str>,
) -> CliResult<Outcome> {
    let mut client = mobile_jwgl::MobileJwglClient::new(transport, auth);
    let report = client.score_report()?;
    mobile_jwgl::scores_payload(&report, semester).map(outcome)
}

fn run_classrooms(matches: &ArgMatches, ctx: &mut Context<'_>) -> CliResult<Outcome> {
    let account = identity::require_authenticated_profile(&ctx.config_dir)?;
    require_undergraduate(required_role(&account)?, "classrooms")?;
    let campus = matches
        .get_one::<String>("campus")
        .expect("campus has a validated default")
        .clone();
    let mut auth = mobile_auth(ctx)?;
    run_classrooms_with_mobile(ctx.transport, &mut auth, &campus)
}

fn run_classrooms_with_mobile(
    transport: &mut dyn Transport,
    auth: &mut dyn mobile_jwgl::MobileJwglAuth,
    campus: &str,
) -> CliResult<Outcome> {
    let mut client = mobile_jwgl::MobileJwglClient::new(transport, auth);
    let data = client.today_classrooms(campus)?;
    Ok(outcome(mobile_jwgl::classrooms_payload(campus, &data)))
}

fn require_undergraduate(role: Role, command: &str) -> CliResult<()> {
    if role == Role::Graduate {
        return Err(CliError::new(
            2,
            "usage",
            format!("`bupt {command}` 走的是本科教务系统，研究生学号在那套系统里不存在"),
            "改用 `bupt schedule` / `bupt exams`；若身份登记有误，重新执行 `bupt login --role undergraduate`",
        ));
    }
    Ok(())
}

fn pending_remote(label: &str) -> CliError {
    CliError::new(
        5,
        "upstream",
        format!("{label}尚未接通；本批次只完成本地身份与数据源校验"),
        "等待注入教务响应后接入远端取数。",
    )
}

fn outcome(data: Value) -> Outcome {
    Outcome {
        data,
        code: 0,
        diagnostic: None,
    }
}

// ---------------------------------------------------------------------------
// Local account and snapshot ownership.
// ---------------------------------------------------------------------------

fn required_role(account: &LocalProfile) -> CliResult<Role> {
    account
        .role()
        .ok_or_else(|| role_error("账号身份未登记或不受支持"))
}

fn role_label(role: Role) -> &'static str {
    match role {
        Role::Graduate => "graduate",
        Role::Undergraduate => "undergraduate",
    }
}

fn role_error(message: impl Into<String>) -> CliError {
    CliError::new(
        2,
        "usage",
        message,
        "运行 `bupt login --role graduate` 或 `--role undergraduate` 重新登记身份。",
    )
}

#[derive(Clone, Debug)]
struct Snapshot {
    username: String,
    role: String,
    term: String,
    source: String,
    courses: Vec<Value>,
    calendar: BTreeMap<i64, Vec<String>>,
}

#[derive(Debug)]
struct Selected {
    snapshot: Snapshot,
    from_cache: bool,
}

trait TimetableSource {
    fn fetch(&mut self, username: &str, role: Role) -> CliResult<Snapshot>;
}

struct NativeTimetableSource<'a, 't> {
    context: &'a mut Context<'t>,
}

impl TimetableSource for NativeTimetableSource<'_, '_> {
    fn fetch(&mut self, username: &str, role: Role) -> CliResult<Snapshot> {
        match role {
            Role::Graduate => {
                let mut source = graduate::HttpsPageSource::new(self.context)?;
                let collected = source
                    .collect(&[graduate::GradPage::Schedule, graduate::GradPage::Calendar])?;
                graduate::snapshot_from_pages(username, &collected)
            }
            Role::Undergraduate => {
                let mut auth = mobile_auth(self.context)?;
                let mut source =
                    mobile_jwgl::MobileTimetableSource::new(self.context.transport, &mut auth);
                source.fetch(username, role)
            }
        }
    }
}

fn current_selection(ctx: &mut Context<'_>, today: Date, force: bool) -> CliResult<Selected> {
    let config_dir = ctx.config_dir.clone();
    let cache_dir = ctx.cache_dir.clone();
    let mut source = NativeTimetableSource { context: ctx };
    select_snapshot(&config_dir, &cache_dir, today, force, &mut source)
}

fn select_snapshot(
    config_dir: &Path,
    cache_dir: &Path,
    today: Date,
    force: bool,
    source: &mut impl TimetableSource,
) -> CliResult<Selected> {
    let account = identity::require_authenticated_profile(config_dir)?;
    let role = required_role(&account)?;
    let role_text = role_label(role);
    let path = cache_dir.join("timetable.json");
    let local = read_snapshot(&path);

    if !force
        && let Some(snapshot) = local
        && snapshot.username == account.username()
        && snapshot.role == role_text
        && snapshot_covers(&snapshot, today)
    {
        return Ok(Selected {
            snapshot,
            from_cache: true,
        });
    }

    let fetched = source.fetch(account.username(), role)?;
    if fetched.username != account.username() || fetched.role != role_text {
        return Err(refresh_failed(
            "刷新结果不属于当前学号或校园身份，已保留上一份快照",
        ));
    }
    validate_refreshed_snapshot(&fetched, today)?;
    save_snapshot(&path, &fetched)?;
    Ok(Selected {
        snapshot: fetched,
        from_cache: false,
    })
}

fn read_snapshot(path: &Path) -> Option<Snapshot> {
    let raw = fs::read(path).ok()?;
    let value: Value = serde_json::from_slice(&raw).ok()?;
    snapshot_from_value(&value)
}

fn snapshot_from_value(value: &Value) -> Option<Snapshot> {
    let object = value.as_object()?;
    if object.get("version").and_then(Value::as_i64) != Some(2) {
        return None;
    }
    let username = object.get("username").and_then(Value::as_str)?.to_string();
    let role = object.get("role").and_then(Value::as_str)?.to_string();
    let courses = object.get("courses").and_then(Value::as_array)?.clone();
    let calendar_object = object.get("calendar").and_then(Value::as_object)?;
    let mut calendar = BTreeMap::new();
    for (key, value) in calendar_object {
        let Ok(week) = key.parse::<i64>() else {
            continue;
        };
        let Some(items) = value.as_array() else {
            continue;
        };
        let dates = items
            .iter()
            .map(|item| item.as_str().unwrap_or("").to_string())
            .collect();
        calendar.insert(week, dates);
    }
    Some(Snapshot {
        username,
        role,
        term: object
            .get("term")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        source: object
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        courses,
        calendar,
    })
}

fn save_snapshot(path: &Path, snapshot: &Snapshot) -> CliResult<()> {
    let calendar: Map<String, Value> = snapshot
        .calendar
        .iter()
        .map(|(week, dates)| (week.to_string(), json!(dates)))
        .collect();
    let payload = json!({
        "version": 2,
        "username": snapshot.username,
        "role": snapshot.role,
        "term": snapshot.term,
        "source": snapshot.source,
        "fetched_at": utc_stamp(),
        "courses": snapshot.courses,
        "calendar": calendar,
    });
    let bytes = serde_json::to_vec_pretty(&payload)
        .map_err(|_| CliError::new(5, "upstream", "cannot serialize refreshed timetable", ""))?;
    write_atomically(path, &bytes, "timetable snapshot")
}

fn write_atomically(path: &Path, bytes: &[u8], label: &str) -> CliResult<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .map_err(|error| atomic_write_error(label, "create the parent directory", &error))?;

    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(CliError::new(
                5,
                "upstream",
                format!("refusing to replace a symlink at the {label} path"),
                "Remove the symlink manually; the link and its target were left untouched.",
            ));
        }
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(CliError::new(
                5,
                "upstream",
                format!("refusing to replace a non-regular {label}"),
                "The existing path was left untouched.",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(atomic_write_error(label, "inspect the target", &error)),
    }

    let mut temporary = NamedTempFile::new_in(parent)
        .map_err(|error| atomic_write_error(label, "create a temporary file", &error))?;
    temporary
        .write_all(bytes)
        .map_err(|error| atomic_write_error(label, "write the temporary file", &error))?;
    temporary
        .as_file_mut()
        .flush()
        .map_err(|error| atomic_write_error(label, "flush the temporary file", &error))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| atomic_write_error(label, "sync the temporary file", &error))?;

    let aliases = same_existing_file(temporary.path(), path)
        .map_err(|error| atomic_write_error(label, "compare source and target", &error))?;
    if aliases {
        return Err(CliError::new(
            5,
            "upstream",
            format!("refusing to replace the {label}: source and target are the same file"),
            "The existing file was left untouched.",
        ));
    }

    temporary
        .persist(path)
        .map_err(|error| atomic_write_error(label, "replace the target", &error.error))?;
    Ok(())
}

fn atomic_write_error(label: &str, action: &str, error: &std::io::Error) -> CliError {
    CliError::new(
        5,
        "upstream",
        format!("cannot {action} for the {label}: {error}"),
        "The previous file was left untouched.",
    )
}

#[cfg(unix)]
fn same_existing_file(left: &Path, right: &Path) -> std::io::Result<bool> {
    use std::os::unix::fs::MetadataExt;

    let left = fs::metadata(left)?;
    let right = match fs::metadata(right) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    Ok(left.dev() == right.dev() && left.ino() == right.ino())
}

#[cfg(not(unix))]
fn same_existing_file(left: &Path, right: &Path) -> std::io::Result<bool> {
    let right = match fs::canonicalize(right) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    Ok(fs::canonicalize(left)? == right)
}

fn validate_refreshed_snapshot(snapshot: &Snapshot, today: Date) -> CliResult<()> {
    if snapshot.calendar.is_empty() {
        return Err(refresh_failed("刷新结果缺少教学周日历，已保留上一份快照"));
    }
    let mut previous_week = None;
    for (week, dates) in &snapshot.calendar {
        if *week <= 0 {
            return Err(refresh_failed(format!(
                "刷新结果包含无效周次 {week}，已保留上一份快照"
            )));
        }
        if let Some(previous) = previous_week
            && *week != previous + 1
        {
            return Err(refresh_failed(format!(
                "刷新结果的教学周次不连续（第 {previous} 周后是第 {week} 周），已保留上一份快照"
            )));
        }
        if previous_week.is_none() && *week != 1 {
            return Err(refresh_failed(format!(
                "刷新结果没有从第 1 教学周开始（首周是第 {week} 周），已保留上一份快照"
            )));
        }
        previous_week = Some(*week);
        if dates.len() != WEEKDAYS.len() {
            return Err(refresh_failed(format!(
                "刷新结果第 {week} 周不是完整的七天日期，已保留上一份快照"
            )));
        }
        let mut previous: Option<Date> = None;
        for raw in dates {
            let Some(date) = Date::parse(raw) else {
                return Err(refresh_failed(format!(
                    "刷新结果第 {week} 周包含无效日期 {raw:?}，已保留上一份快照"
                )));
            };
            if let Some(previous) = previous
                && date != previous.add_days(1)
            {
                return Err(refresh_failed(format!(
                    "刷新结果第 {week} 周日期不连续，已保留上一份快照"
                )));
            }
            previous = Some(date);
        }
    }
    let Some((start, end)) = term_bounds(snapshot) else {
        return Err(refresh_failed(
            "刷新结果缺少可用的学期日期，已保留上一份快照",
        ));
    };
    if today < start || today > end {
        return Err(refresh_failed("刷新结果不覆盖运行当天，已保留上一份快照"));
    }
    if let Err(error) = export_sessions(snapshot) {
        return Err(refresh_failed(format!(
            "刷新结果无法生成完整日历，已保留上一份快照：{}",
            error.message
        )));
    }
    Ok(())
}

fn snapshot_covers(snapshot: &Snapshot, day: Date) -> bool {
    term_bounds(snapshot).is_some_and(|(start, end)| start <= day && day <= end)
}

fn refresh_failed(message: impl Into<String>) -> CliError {
    CliError::new(
        5,
        "upstream",
        format!("课表刷新结果不可用：{}", message.into()),
        "保留上一份快照；不会用它生成课表或日历。",
    )
}

fn ensure_date_in_term(snapshot: &Snapshot, day: Date) -> CliResult<()> {
    let Some((start, end)) = term_bounds(snapshot) else {
        return Err(refresh_required("课表快照里没有可用的学期日期范围"));
    };
    if day < start || day > end {
        return Err(CliError::new(
            2,
            "usage",
            format!(
                "指定日期 {} 不在当前学期 {} 的范围 {} 至 {} 内",
                day.iso(),
                snapshot.term,
                start.iso(),
                end.iso()
            ),
            "只筛选当前学期，不会切换学期或把范围外的日期当作没课。",
        ));
    }
    Ok(())
}

fn refresh_required(message: impl Into<String>) -> CliError {
    CliError::new(
        2,
        "usage",
        message,
        "运行 `bupt schedule update` 刷新当前学号的课表快照。",
    )
}

fn calendar_payload(selected: &Selected) -> Value {
    let snapshot = &selected.snapshot;
    let weeks: Vec<Value> = snapshot
        .calendar
        .iter()
        .map(|(week, dates)| {
            json!({
                "week": week,
                "monday": dates.first().cloned().unwrap_or_default(),
                "sunday": dates.last().cloned().unwrap_or_default(),
                "dates": dates,
            })
        })
        .collect();
    json!({
        "term": snapshot.term,
        "role": snapshot.role,
        "from_cache": selected.from_cache,
        "week_count": weeks.len(),
        "weeks": weeks,
    })
}

fn export_schedule(matches: &ArgMatches, ctx: &mut Context<'_>) -> CliResult<Outcome> {
    let selected = current_selection(ctx, local_today()?, false)?;
    let snapshot = &selected.snapshot;
    let (content, events) = build_ics(snapshot)?;
    let out = matches
        .get_one::<String>("out")
        .map(|path| expand_home(path, &ctx.home_dir))
        .unwrap_or_else(|| default_export_path(snapshot, &ctx.home_dir));
    write_export(&out, &content)?;

    if !matches.get_flag("no-open-folder") {
        reveal(&out);
    }

    Ok(outcome(json!({
        "term": snapshot.term,
        "from_cache": selected.from_cache,
        "courses": snapshot.courses.len(),
        "weeks": snapshot.calendar.len(),
        "events": events,
        "path": out.display().to_string(),
        "hint": "在日历 App 里双击导入；重复导入会生成重复事件",
    })))
}

fn write_export(path: &Path, content: &str) -> CliResult<()> {
    write_atomically(path, content.as_bytes(), "iCalendar export")
}

fn default_export_path(snapshot: &Snapshot, home: &Path) -> PathBuf {
    let mut safe = String::new();
    let mut pending_dash = false;
    for character in snapshot.term.chars() {
        if character.is_alphanumeric() || character == '-' || character == '_' {
            safe.push(character);
            pending_dash = false;
        } else if !pending_dash {
            safe.push('-');
            pending_dash = true;
        }
    }
    let safe = safe.trim_matches('-');
    let safe = if safe.is_empty() { "课表" } else { safe };
    home.join("Downloads").join(format!("bupt-{safe}.ics"))
}

fn expand_home(path: &str, home: &Path) -> PathBuf {
    path.strip_prefix("~/")
        .map_or_else(|| PathBuf::from(path), |rest| home.join(rest))
}

fn reveal(path: &Path) {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = ProcessCommand::new("open");
        command.arg("-R").arg(path);
        command
    };
    #[cfg(windows)]
    let mut command = {
        let mut command = ProcessCommand::new("explorer");
        command.arg(format!("/select,{}", path.display()));
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = ProcessCommand::new("xdg-open");
        command.arg(path.parent().unwrap_or(path));
        command
    };
    let _ = command.status();
}

fn build_ics(snapshot: &Snapshot) -> CliResult<(String, usize)> {
    let sessions = export_sessions(snapshot)?;
    let stamp = utc_stamp();
    let term = if snapshot.term.is_empty() {
        "课表"
    } else {
        &snapshot.term
    };
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        format!("PRODID:-//bupt-cli//{}//CN", ics_escape(term)),
        "CALSCALE:GREGORIAN".to_string(),
        "METHOD:PUBLISH".to_string(),
        format!("X-WR-CALNAME:{}", ics_escape(term)),
        "X-WR-TIMEZONE:Asia/Shanghai".to_string(),
        "BEGIN:VTIMEZONE".to_string(),
        "TZID:Asia/Shanghai".to_string(),
        "BEGIN:STANDARD".to_string(),
        "DTSTART:19700101T000000".to_string(),
        "TZOFFSETFROM:+0800".to_string(),
        "TZOFFSETTO:+0800".to_string(),
        "END:STANDARD".to_string(),
        "END:VTIMEZONE".to_string(),
    ];

    let mut emitted = 0;
    for session in sessions {
        let day = session.date.iso().replace('-', "");
        let start_clock = session.time_start.replace(':', "");
        let end_clock = session.time_end.replace(':', "");
        let period_start = session.data["period_start"].as_i64().unwrap_or_default();
        let location = location_text(&session.data);
        let mut details = Vec::new();
        if let Some(teachers) = session.data["teachers"].as_array()
            && !teachers.is_empty()
        {
            details.push(format!(
                "教师: {}",
                teachers
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        details.push(format!("第 {} 周", session.week));

        lines.push("BEGIN:VEVENT".to_string());
        lines.push(format!(
            "UID:bupt-{day}-{}-{period_start}-{emitted}@bupt-cli",
            session.week
        ));
        lines.push(format!("DTSTAMP:{stamp}"));
        lines.push(format!("DTSTART;TZID=Asia/Shanghai:{day}T{start_clock}00"));
        lines.push(format!("DTEND;TZID=Asia/Shanghai:{day}T{end_clock}00"));
        lines.push(ics_fold(format!("SUMMARY:{}", ics_escape(&session.name))));
        if !location.is_empty() {
            lines.push(ics_fold(format!("LOCATION:{}", ics_escape(&location))));
        }
        lines.push(ics_fold(format!(
            "DESCRIPTION:{}",
            ics_escape(&details.join("; "))
        )));
        lines.push("END:VEVENT".to_string());
        emitted += 1;
    }
    lines.push("END:VCALENDAR".to_string());
    Ok((format!("{}\r\n", lines.join("\r\n")), emitted))
}

fn normalize_clock(text: &str) -> Option<String> {
    let text = text.trim().replace('：', ":");
    let (hour, minute) = if text.len() == 4 && text.as_bytes().iter().all(u8::is_ascii_digit) {
        (
            text[0..2].parse::<u32>().ok()?,
            text[2..4].parse::<u32>().ok()?,
        )
    } else {
        let mut parts = text.split(':');
        let hour = parts.next()?.parse::<u32>().ok()?;
        let minute = parts.next()?.parse::<u32>().ok()?;
        if let Some(seconds) = parts.next() {
            if seconds != "00" || parts.next().is_some() {
                return None;
            }
        }
        (hour, minute)
    };
    if hour > 23 || minute > 59 {
        return None;
    }
    Some(format!("{hour:02}:{minute:02}"))
}

fn export_error(course: &str, detail: impl Into<String>) -> CliError {
    CliError::new(
        5,
        "upstream",
        format!("无法生成完整课表日历：课程「{course}」{}", detail.into()),
        "运行 `bupt schedule update` 重新获取完整课表；不会生成半份日历或覆盖已有 .ics。",
    )
}

fn export_sessions(snapshot: &Snapshot) -> CliResult<Vec<Session>> {
    let mut sessions = Vec::new();
    for (index, course) in snapshot.courses.iter().enumerate() {
        let Some(course) = course.as_object() else {
            return Err(export_error(
                &format!("第 {} 门课程", index + 1),
                "不是有效的课程对象",
            ));
        };
        let name = text_field(course, "name");
        let label = if name.trim().is_empty() {
            format!("第 {} 门课程", index + 1)
        } else {
            name.clone()
        };
        let Some(weekday) = course.get("weekday_index").and_then(Value::as_i64) else {
            return Err(export_error(&label, "缺少星期信息"));
        };
        if !(0..=6).contains(&weekday) {
            return Err(export_error(&label, format!("的星期信息无效（{weekday}）")));
        }
        let Some(weeks) = course.get("weeks").and_then(Value::as_array) else {
            return Err(export_error(&label, "缺少周次信息"));
        };
        if weeks.is_empty() {
            return Err(export_error(&label, "没有可导出的周次"));
        }
        for week in weeks {
            let Some(week) = week.as_i64() else {
                return Err(export_error(&label, "包含无效周次"));
            };
            let Some(dates) = snapshot.calendar.get(&week) else {
                return Err(export_error(&label, format!("第 {week} 周缺少教学周日期")));
            };
            let Some(raw_date) = dates.get(weekday as usize) else {
                return Err(export_error(
                    &label,
                    format!("第 {week} 周缺少{}的日期", WEEKDAYS[weekday as usize]),
                ));
            };
            let Some(date) = Date::parse(raw_date) else {
                return Err(export_error(
                    &label,
                    format!("第 {week} 周的日期无效或缺失：{raw_date:?}"),
                ));
            };
            let Some(time_start) = normalize_clock(&text_field(course, "time_start")) else {
                return Err(export_error(
                    &label,
                    format!(
                        "第 {week} 周（{}）缺少或无法识别开始时间：{:?}",
                        date.iso(),
                        text_field(course, "time_start")
                    ),
                ));
            };
            let Some(time_end) = normalize_clock(&text_field(course, "time_end")) else {
                return Err(export_error(
                    &label,
                    format!(
                        "第 {week} 周（{}）缺少或无法识别结束时间：{:?}",
                        date.iso(),
                        text_field(course, "time_end")
                    ),
                ));
            };
            // A hand-edited or glitched snapshot could carry reversed clocks;
            // that must not become a silent "successfully" exported event.
            if time_end < time_start {
                return Err(export_error(
                    &label,
                    format!(
                        "第 {week} 周（{}）的结束时间早于开始时间（{} > {}）",
                        date.iso(),
                        time_start,
                        time_end
                    ),
                ));
            }
            sessions.push(Session {
                date,
                week,
                time_start: time_start.clone(),
                time_end: time_end.clone(),
                name: name.clone(),
                data: json!({
                    "date": date.iso(),
                    "weekday": WEEKDAYS[weekday as usize],
                    "week": week,
                    "time_start": time_start,
                    "time_end": time_end,
                    "name": name,
                    "teachers": string_array(course, "teachers"),
                    "rooms": string_array(course, "rooms"),
                    "campus": text_field(course, "campus"),
                    "period_start": course.get("period_start").cloned().unwrap_or(Value::Null),
                    "period_end": course.get("period_end").cloned().unwrap_or(Value::Null),
                }),
            });
        }
    }
    sessions.sort_by(|a, b| {
        (a.date, a.time_start.as_str(), a.name.as_str()).cmp(&(
            b.date,
            b.time_start.as_str(),
            b.name.as_str(),
        ))
    });
    Ok(sessions)
}

fn location_text(data: &Value) -> String {
    let rooms = data["rooms"]
        .as_array()
        .map(|rooms| {
            rooms
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" / ")
        })
        .unwrap_or_default();
    let campus = data["campus"].as_str().unwrap_or("");
    if campus.is_empty() {
        rooms
    } else {
        format!("[{campus}] {rooms}").trim().to_string()
    }
}

fn ics_escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace("\r\n", "\\n")
        .replace('\r', "\\n")
        .replace('\n', "\\n")
}

fn ics_fold(line: String) -> String {
    if line.len() <= 75 {
        return line;
    }
    let mut segments: Vec<Vec<u8>> = Vec::new();
    let mut current = Vec::new();
    for character in line.chars() {
        let mut raw = [0_u8; 4];
        let bytes = character.encode_utf8(&mut raw).as_bytes();
        if current.len() + bytes.len() > 75 {
            segments.push(current);
            current = vec![b' '];
        }
        current.extend_from_slice(bytes);
    }
    if !current.is_empty() {
        segments.push(current);
    }
    let mut out = Vec::new();
    for (index, segment) in segments.iter().enumerate() {
        if index > 0 {
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(segment);
    }
    String::from_utf8(out).expect("folded ICS is valid UTF-8")
}

fn term_bounds(snapshot: &Snapshot) -> Option<(Date, Date)> {
    let mut start: Option<Date> = None;
    let mut end: Option<Date> = None;
    for dates in snapshot.calendar.values() {
        for raw in dates {
            let Some(date) = Date::parse(raw) else {
                continue;
            };
            start = Some(start.map_or(date, |current| current.min(date)));
            end = Some(end.map_or(date, |current| current.max(date)));
        }
    }
    start.zip(end)
}

fn week_of(snapshot: &Snapshot, day: Date) -> Option<i64> {
    let iso = day.iso();
    snapshot
        .calendar
        .iter()
        .find(|(_, dates)| dates.iter().any(|date| date == &iso))
        .map(|(week, _)| *week)
}

fn day_range(snapshot: &Snapshot, day: Date) -> (Date, Date) {
    let iso = day.iso();
    for dates in snapshot.calendar.values() {
        if !dates.iter().any(|date| date == &iso) {
            continue;
        }
        let Some(first) = dates.first().and_then(|date| Date::parse(date)) else {
            continue;
        };
        let Some(last) = dates.last().and_then(|date| Date::parse(date)) else {
            continue;
        };
        return (first, last);
    }
    let monday = day.add_days(-(day.weekday_index() as i64));
    (monday, monday.add_days(6))
}

struct Session {
    date: Date,
    week: i64,
    time_start: String,
    time_end: String,
    name: String,
    data: Value,
}

fn occurrences(snapshot: &Snapshot, start: Date, end: Date) -> Vec<Session> {
    let mut sessions = Vec::new();
    for course in &snapshot.courses {
        let Some(course) = course.as_object() else {
            continue;
        };
        let Some(weekday) = course.get("weekday_index").and_then(Value::as_i64) else {
            continue;
        };
        if !(0..=6).contains(&weekday) {
            continue;
        }
        let Some(weeks) = course.get("weeks").and_then(Value::as_array) else {
            continue;
        };
        for week in weeks.iter().filter_map(Value::as_i64) {
            let Some(dates) = snapshot.calendar.get(&week) else {
                continue;
            };
            let Some(raw_date) = dates.get(weekday as usize) else {
                continue;
            };
            let Some(date) = Date::parse(raw_date) else {
                continue;
            };
            if date < start || date > end {
                continue;
            }
            let time_start = text_field(course, "time_start");
            let time_end = text_field(course, "time_end");
            let name = text_field(course, "name");
            sessions.push(Session {
                date,
                week,
                time_start: time_start.clone(),
                time_end: time_end.clone(),
                name: name.clone(),
                data: json!({
                    "date": date.iso(),
                    "weekday": WEEKDAYS[weekday as usize],
                    "week": week,
                    "time_start": time_start,
                    "time_end": time_end,
                    "name": name,
                    "teachers": string_array(course, "teachers"),
                    "rooms": string_array(course, "rooms"),
                    "campus": text_field(course, "campus"),
                    "period_start": course.get("period_start").cloned().unwrap_or(Value::Null),
                    "period_end": course.get("period_end").cloned().unwrap_or(Value::Null),
                }),
            });
        }
    }
    sessions.sort_by(|a, b| {
        (a.date, a.time_start.as_str(), a.name.as_str()).cmp(&(
            b.date,
            b.time_start.as_str(),
            b.name.as_str(),
        ))
    });
    sessions
}

fn text_field(object: &Map<String, Value>, key: &str) -> String {
    object
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn string_array(object: &Map<String, Value>, key: &str) -> Vec<String> {
    object
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Small Gregorian date helpers (no date crate in this build).
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Date {
    year: i32,
    month: u32,
    day: u32,
}

impl Date {
    fn parse(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != 10
            || bytes[4] != b'-'
            || bytes[7] != b'-'
            || !bytes
                .iter()
                .enumerate()
                .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
        {
            return None;
        }
        let year = text[0..4].parse::<i32>().ok()?;
        let month = text[5..7].parse::<u32>().ok()?;
        let day = text[8..10].parse::<u32>().ok()?;
        if !(1..=9999).contains(&year)
            || month == 0
            || month > 12
            || day == 0
            || day > days_in_month(year, month)
        {
            return None;
        }
        Some(Self { year, month, day })
    }

    fn from_days(days: i64) -> Self {
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = mp + if mp < 10 { 3 } else { -9 };
        Self {
            year: (y + i64::from(month <= 2)) as i32,
            month: month as u32,
            day: day as u32,
        }
    }

    fn to_days(self) -> i64 {
        let y = i64::from(self.year) - i64::from(self.month <= 2);
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let month = i64::from(self.month);
        let doy =
            (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(self.day) - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    fn add_days(self, days: i64) -> Self {
        Self::from_days(self.to_days() + days)
    }

    fn weekday_index(self) -> usize {
        (self.to_days() + 3).rem_euclid(7) as usize
    }

    fn iso(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        _ => 0,
    }
}

fn local_today() -> CliResult<Date> {
    let local = time::OffsetDateTime::now_local().map_err(|_| {
        CliError::new(
            5,
            "upstream",
            "无法确定本机日期，不能确认当前学期",
            "检查系统时区设置后重试。",
        )
    })?;
    Ok(Date {
        year: local.year(),
        month: u32::from(u8::from(local.month())),
        day: u32::from(local.day()),
    })
}

fn utc_stamp() -> String {
    let seconds = unix_seconds();
    let date = Date::from_days((seconds / 86_400) as i64);
    let clock = seconds % 86_400;
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        date.year,
        date.month,
        date.day,
        clock / 3_600,
        (clock % 3_600) / 60,
        clock % 60,
    )
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn date_arg(text: &str) -> Result<String, String> {
    Date::parse(text)
        .map(|_| text.to_string())
        .ok_or_else(|| "expected a real date in YYYY-MM-DD form".to_string())
}

fn week_arg(text: &str) -> Result<u32, String> {
    let week = text
        .parse::<u32>()
        .map_err(|_| "week must be an integer".to_string())?;
    if week == 0 {
        return Err("week must be >= 1".to_string());
    }
    Ok(week)
}

#[cfg(test)]
mod tests {
    use super::graduate::{GradPage, GradPageSource, GradPages};
    use super::mobile_jwgl::{MobileJwglAuth, MobileJwglToken, MobileTimetableSource};
    use super::*;
    use crate::transport::Response;
    use crate::transport::testing::Scripted;
    use std::collections::VecDeque;
    use tempfile::tempdir;

    const USER: &str = "20260001";
    const OTHER_USER: &str = "20260002";
    const SESSION_PASSWORD: &str = "synthetic-test-password";
    const JWT_TOKEN: &str = "TEST-ONLY-JWGL-TOKEN";
    const MOBILE_CURRICULUM: &str =
        include_str!("../tests/fixtures/contracts/academics/mobile_jwgl_curriculum.json");
    const MOBILE_CURRICULUM_PARTIAL: &str =
        include_str!("../tests/fixtures/contracts/academics/mobile_jwgl_curriculum_partial.json");
    const MOBILE_EXAM_WEEKS: &str =
        include_str!("../tests/fixtures/contracts/academics/mobile_jwgl_exam_weeks.json");
    const MOBILE_EXAM_SCHEDULE: &str =
        include_str!("../tests/fixtures/contracts/academics/mobile_jwgl_exam_schedule.json");
    const MOBILE_EXAM_COURSES: &str =
        include_str!("../tests/fixtures/contracts/academics/mobile_jwgl_exam_course_schedule.json");
    const MOBILE_SCORES: &str =
        include_str!("../tests/fixtures/contracts/academics/mobile_jwgl_scores.json");
    const MOBILE_CLASSROOMS: &str =
        include_str!("../tests/fixtures/contracts/academics/mobile_jwgl_classrooms.json");

    fn today() -> Date {
        Date::parse("2026-09-23").expect("fixed synthetic Wednesday")
    }

    fn monday_of(day: Date) -> Date {
        day.add_days(-(day.weekday_index() as i64))
    }

    fn snapshot(
        username: &str,
        role: Role,
        week_start: Date,
        course_name: &str,
        start: &str,
        end: &str,
    ) -> Snapshot {
        let dates = (0..7)
            .map(|offset| week_start.add_days(offset).iso())
            .collect();
        let mut calendar = BTreeMap::new();
        calendar.insert(1, dates);
        Snapshot {
            username: username.to_string(),
            role: role_label(role).to_string(),
            term: "2026-2027秋学期".to_string(),
            source: "fake".to_string(),
            courses: vec![json!({
                "name": course_name,
                "weekday_index": today().weekday_index(),
                "weeks": [1],
                "period_start": 1,
                "period_end": 1,
                "time_start": start,
                "time_end": end,
                "teachers": [],
                "rooms": [],
                "campus": ""
            })],
            calendar,
        }
    }

    fn fresh() -> Snapshot {
        snapshot(
            USER,
            Role::Undergraduate,
            monday_of(today()),
            "当前学期课程",
            "08:00",
            "08:45",
        )
    }

    fn stale() -> Snapshot {
        snapshot(
            USER,
            Role::Undergraduate,
            monday_of(today()).add_days(-28),
            "旧学期课程",
            "08:00",
            "08:45",
        )
    }

    fn account_with_role(config: &Path, role: &str) {
        fs::create_dir_all(config).expect("create config directory");
        fs::write(
            config.join("credentials.json"),
            serde_json::to_vec_pretty(&json!({
                "username": USER,
                "password": SESSION_PASSWORD,
                "role": role,
            }))
            .expect("serialize synthetic account"),
        )
        .expect("write synthetic account");
    }

    fn account(config: &Path) {
        account_with_role(config, "undergraduate");
    }

    fn graduate_account(config: &Path) {
        account_with_role(config, "graduate");
    }

    struct FakeSource {
        responses: VecDeque<CliResult<Snapshot>>,
        calls: usize,
    }

    impl FakeSource {
        fn with(responses: Vec<CliResult<Snapshot>>) -> Self {
            Self {
                responses: responses.into(),
                calls: 0,
            }
        }
    }

    impl TimetableSource for FakeSource {
        fn fetch(&mut self, _: &str, _: Role) -> CliResult<Snapshot> {
            self.calls += 1;
            self.responses
                .pop_front()
                .expect("selection fetched more times than the test allowed")
        }
    }

    struct TestMobileAuth {
        token: String,
        calls: usize,
    }

    impl TestMobileAuth {
        fn valid() -> Self {
            Self {
                token: JWT_TOKEN.to_owned(),
                calls: 0,
            }
        }
    }

    impl MobileJwglAuth for TestMobileAuth {
        fn token(&mut self) -> CliResult<MobileJwglToken> {
            self.calls += 1;
            Ok(MobileJwglToken::new(
                self.token.clone(),
                Some(4_102_444_800.0),
            ))
        }
    }

    fn queue_mobile(scripted: &mut Scripted, method: &'static str, url: &str, body: &str) {
        scripted.expected.push_back((
            method,
            url.to_owned(),
            Ok(Response {
                status: 200,
                headers: Vec::new(),
                body: body.as_bytes().to_vec(),
            }),
        ));
    }

    struct FakePageSource {
        pages: GradPages,
        calls: usize,
        wanted: Vec<Vec<GradPage>>,
    }

    impl FakePageSource {
        fn with(pages: GradPages) -> Self {
            Self {
                pages,
                calls: 0,
                wanted: Vec::new(),
            }
        }
    }

    impl GradPageSource for FakePageSource {
        fn collect(&mut self, wanted: &[GradPage]) -> CliResult<GradPages> {
            self.calls += 1;
            self.wanted.push(wanted.to_vec());
            Ok(self.pages.clone())
        }
    }

    struct GraduatePageTimetable<S> {
        pages: S,
    }

    impl<S: GradPageSource> TimetableSource for GraduatePageTimetable<S> {
        fn fetch(&mut self, username: &str, role: Role) -> CliResult<Snapshot> {
            if role != Role::Graduate {
                return Err(CliError::new(
                    2,
                    "usage",
                    "test timetable source only serves graduate accounts",
                    "",
                ));
            }
            let pages = self
                .pages
                .collect(&[GradPage::Schedule, GradPage::Calendar])?;
            graduate::snapshot_from_pages(username, &pages)
        }
    }

    fn chinese_week(week: i64) -> String {
        let digit = |value: i64| match value {
            1 => "一",
            2 => "二",
            3 => "三",
            4 => "四",
            5 => "五",
            6 => "六",
            7 => "七",
            8 => "八",
            9 => "九",
            _ => "",
        };
        match week {
            1..=9 => digit(week).to_string(),
            10 => "十".to_string(),
            11..=19 => format!("十{}", digit(week - 10)),
            _ => panic!("test calendar only needs weeks 1..=19"),
        }
    }

    fn complete_calendar() -> String {
        let start = Date::parse("2026-08-31").expect("synthetic term Monday");
        let mut html =
            String::from("<td class=\"tb_line\"> 学期：2026-2027秋学期 </td><table id=\"dgData\">");
        for week in 1..=19 {
            html.push_str("<tr>");
            html.push_str(&format!("<td>{}</td>", chinese_week(week)));
            for day in 0..7 {
                let date = start.add_days((week - 1) * 7 + day);
                html.push_str(&format!("<td>{}&nbsp;({}月)</td>", date.day, date.month));
            }
            html.push_str("</tr>");
        }
        html.push_str("</table>");
        html
    }

    fn cache_file(directory: &Path) -> PathBuf {
        directory.join("timetable.json")
    }

    fn read_bytes(path: &Path) -> Vec<u8> {
        fs::read(path).expect("read snapshot for comparison")
    }

    #[test]
    fn fresh_owned_snapshot_is_served_without_fetching() {
        let root = tempdir().unwrap();
        let config = root.path().join("config");
        let cache = root.path().join("cache");
        account(&config);
        let current = fresh();
        save_snapshot(&cache_file(&cache), &current).unwrap();
        let before = read_bytes(&cache_file(&cache));
        let mut source = FakeSource::with(vec![]);

        let selected =
            select_snapshot(&config, &cache, today(), false, &mut source).expect("fresh snapshot");

        assert_eq!(source.calls, 0, "a fresh owned snapshot must not fetch");
        assert!(selected.from_cache);
        assert_eq!(selected.snapshot.username, USER);
        assert_eq!(selected.snapshot.role, "undergraduate");
        assert_eq!(read_bytes(&cache_file(&cache)), before);
    }

    #[test]
    fn freshness_includes_both_term_endpoints_and_ignores_fetch_age() {
        for (label, day) in [
            ("first day", monday_of(today())),
            ("last day", monday_of(today()).add_days(6)),
        ] {
            let root = tempdir().unwrap();
            let config = root.path().join("config");
            let cache = root.path().join("cache");
            account(&config);
            let path = cache_file(&cache);
            save_snapshot(&path, &fresh()).unwrap();

            let mut value: Value = serde_json::from_slice(&read_bytes(&path)).unwrap();
            value["fetched_at"] = json!("2000-01-01T00:00:00+00:00");
            let old_bytes = serde_json::to_vec_pretty(&value).unwrap();
            fs::write(&path, &old_bytes).unwrap();

            let mut source = FakeSource::with(vec![]);
            let selected = select_snapshot(&config, &cache, day, false, &mut source)
                .unwrap_or_else(|error| panic!("{label}: {error:?}"));

            assert_eq!(source.calls, 0, "{label}: inclusive term bounds are fresh");
            assert!(selected.from_cache, "{label}: old fetched_at is not a TTL");
            assert_eq!(
                read_bytes(&path),
                old_bytes,
                "{label}: a fresh read must not rewrite the snapshot"
            );
        }
    }

    #[test]
    fn missing_snapshot_fetches_exactly_once_and_writes_the_new_term() {
        let root = tempdir().unwrap();
        let config = root.path().join("config");
        let cache = root.path().join("cache");
        account(&config);
        let mut source = FakeSource::with(vec![Ok(fresh())]);

        let selected =
            select_snapshot(&config, &cache, today(), false, &mut source).expect("refreshed");

        assert_eq!(
            source.calls, 1,
            "a missing snapshot must fetch exactly once"
        );
        assert!(!selected.from_cache);
        assert_eq!(selected.snapshot.term, "2026-2027秋学期");
        assert!(cache_file(&cache).exists());
        let written = read_snapshot(&cache_file(&cache)).expect("written snapshot");
        assert_eq!(written.username, USER);
        assert_eq!(written.role, "undergraduate");
    }

    #[test]
    fn stale_snapshot_fetches_exactly_once_and_replaces_the_cache() {
        let root = tempdir().unwrap();
        let config = root.path().join("config");
        let cache = root.path().join("cache");
        account(&config);
        save_snapshot(&cache_file(&cache), &stale()).unwrap();
        let mut source = FakeSource::with(vec![Ok(fresh())]);

        let selected =
            select_snapshot(&config, &cache, today(), false, &mut source).expect("refreshed");

        assert_eq!(
            source.calls, 1,
            "a snapshot whose term excludes today must fetch exactly once"
        );
        assert!(!selected.from_cache);
        assert_eq!(selected.snapshot.courses[0]["name"], json!("当前学期课程"));
        let written = read_snapshot(&cache_file(&cache)).expect("refreshed cache");
        assert_eq!(written.courses[0]["name"], json!("当前学期课程"));
    }

    #[test]
    fn foreign_snapshot_is_never_used_even_when_refresh_fails() {
        let root = tempdir().unwrap();
        let config = root.path().join("config");
        let cache = root.path().join("cache");
        account(&config);
        let foreign = snapshot(
            OTHER_USER,
            Role::Undergraduate,
            monday_of(today()),
            "他人课程",
            "08:00",
            "08:45",
        );
        save_snapshot(&cache_file(&cache), &foreign).unwrap();
        let before = read_bytes(&cache_file(&cache));
        let mut source = FakeSource::with(vec![Err(CliError::new(
            5,
            "upstream",
            "synthetic refresh failure",
            "",
        ))]);

        let error = select_snapshot(&config, &cache, today(), false, &mut source)
            .expect_err("foreign snapshot must not be a fallback");

        assert_eq!(source.calls, 1);
        assert_eq!(error.code, 5);
        assert_eq!(
            read_bytes(&cache_file(&cache)),
            before,
            "a failed refresh must leave the foreign snapshot untouched"
        );
    }

    #[test]
    fn foreign_snapshot_is_replaced_only_after_a_clean_refresh() {
        let root = tempdir().unwrap();
        let config = root.path().join("config");
        let cache = root.path().join("cache");
        account(&config);
        let foreign = snapshot(
            OTHER_USER,
            Role::Undergraduate,
            monday_of(today()),
            "他人课程",
            "08:00",
            "08:45",
        );
        save_snapshot(&cache_file(&cache), &foreign).unwrap();
        let mut source = FakeSource::with(vec![Ok(fresh())]);

        let selected = select_snapshot(&config, &cache, today(), false, &mut source)
            .expect("refresh from the current account");

        assert_eq!(source.calls, 1);
        assert_eq!(selected.snapshot.username, USER);
        assert_eq!(selected.snapshot.courses[0]["name"], json!("当前学期课程"));
        let written = read_snapshot(&cache_file(&cache)).expect("refreshed cache");
        assert_eq!(written.username, USER);
    }

    #[test]
    fn date_outside_the_selected_term_never_fetches_again() {
        let root = tempdir().unwrap();
        let config = root.path().join("config");
        let cache = root.path().join("cache");
        account(&config);
        save_snapshot(&cache_file(&cache), &fresh()).unwrap();
        let mut source = FakeSource::with(vec![]);

        let selected =
            select_snapshot(&config, &cache, today(), false, &mut source).expect("fresh snapshot");
        let outside = Date::parse("2026-10-15").expect("valid synthetic date");
        let error = ensure_date_in_term(&selected.snapshot, outside)
            .expect_err("outside --date must report the term range");

        assert_eq!(source.calls, 0, "an out-of-range --date must not fetch");
        assert_eq!(error.code, 2);
        assert!(error.message.contains("2026-10-15"));
        assert!(error.message.contains("2026-09-21"));
        assert!(error.message.contains("2026-09-27"));
    }

    #[test]
    fn export_rejects_reversed_clocks_and_keeps_ordered_clocks() {
        let base = fresh();
        // Ordered clocks export normally.
        let ordered = export_sessions(&base).expect("ordered clocks export");
        assert!(!ordered.is_empty());

        // Reversed clocks are an upstream defect, not a valid event.
        let mut reversed = fresh();
        reversed.courses[0]["time_start"] = json!("20:00");
        reversed.courses[0]["time_end"] = json!("08:45");
        let error = match export_sessions(&reversed) {
            Ok(_) => panic!("reversed clocks must fail"),
            Err(error) => error,
        };
        assert_eq!(error.code, 5);
        assert!(error.message.contains("结束时间早于开始时间"));
    }

    #[test]
    fn tomorrow_past_the_term_end_reports_the_range_like_date() {
        let root = tempdir().unwrap();
        let config = root.path().join("config");
        let cache = root.path().join("cache");
        account(&config);
        // A term whose last day is today: --tomorrow is out of range.
        let last = today();
        let first = last.add_days(-6);
        let dates: Vec<_> = (0..7).map(|offset| first.add_days(offset).iso()).collect();
        let snapshot = snapshot(
            USER,
            Role::Undergraduate,
            first,
            "当前学期课程",
            "08:00",
            "08:45",
        );
        let mut snapshot = snapshot;
        snapshot.calendar.insert(1, dates.clone());
        save_snapshot(&cache_file(&cache), &snapshot).unwrap();
        let mut source = FakeSource::with(vec![]);

        let selected =
            select_snapshot(&config, &cache, last, false, &mut source).expect("fresh snapshot");
        let day = last.add_days(1);
        let error = ensure_date_in_term(&selected.snapshot, day)
            .expect_err("--tomorrow past the term must report the range");

        assert_eq!(source.calls, 0, "an out-of-range --tomorrow must not fetch");
        assert_eq!(error.code, 2);
        assert!(error.message.contains(&day.iso()));
    }

    #[test]
    fn failed_incomplete_or_out_of_term_refresh_preserves_the_old_snapshot() {
        let failed = Err(CliError::new(
            5,
            "upstream",
            "synthetic refresh failure",
            "",
        ));
        let incomplete = Ok({
            let mut snapshot = fresh();
            snapshot.courses[0]["time_start"] = json!("");
            snapshot
        });
        let out_of_term = Ok(snapshot(
            USER,
            Role::Undergraduate,
            monday_of(today()).add_days(-28),
            "刷新返回旧学期",
            "08:00",
            "08:45",
        ));
        let malformed_calendar_without_courses = Ok({
            let mut snapshot = fresh();
            snapshot.courses.clear();
            snapshot.calendar.get_mut(&1).expect("week one")[3] = "not-a-date".to_string();
            snapshot
        });
        let empty_calendar_without_courses = Ok({
            let mut snapshot = fresh();
            snapshot.courses.clear();
            snapshot.calendar.clear();
            snapshot
        });

        for (label, response) in [
            ("failed", failed),
            ("incomplete", incomplete),
            ("out-of-term", out_of_term),
            (
                "malformed-calendar-without-courses",
                malformed_calendar_without_courses,
            ),
            (
                "empty-calendar-without-courses",
                empty_calendar_without_courses,
            ),
        ] {
            let root = tempdir().unwrap();
            let config = root.path().join("config");
            let cache = root.path().join("cache");
            account(&config);
            save_snapshot(&cache_file(&cache), &stale()).unwrap();
            let before = read_bytes(&cache_file(&cache));
            let mut source = FakeSource::with(vec![response]);

            let error = select_snapshot(&config, &cache, today(), false, &mut source)
                .expect_err("bad refresh must not be served");

            assert_eq!(source.calls, 1, "{label}: exactly one refresh");
            assert_eq!(error.code, 5, "{label}: upstream failure");
            assert_eq!(
                read_bytes(&cache_file(&cache)),
                before,
                "{label}: the previous snapshot must survive"
            );
            assert!(
                !cache_file(&cache).with_extension("json.tmp").exists(),
                "{label}: no temporary cache is left behind"
            );
        }
    }

    #[test]
    fn forced_update_fetches_even_for_a_fresh_snapshot_and_preserves_on_failure() {
        let root = tempdir().unwrap();
        let config = root.path().join("config");
        let cache = root.path().join("cache");
        account(&config);
        save_snapshot(&cache_file(&cache), &fresh()).unwrap();
        let mut source = FakeSource::with(vec![Ok(snapshot(
            USER,
            Role::Undergraduate,
            monday_of(today()),
            "手动刷新课程",
            "10:00",
            "10:45",
        ))]);

        let selected =
            select_snapshot(&config, &cache, today(), true, &mut source).expect("forced refresh");
        assert_eq!(source.calls, 1, "update must fetch even when fresh");
        assert!(!selected.from_cache);
        assert_eq!(selected.snapshot.courses[0]["name"], json!("手动刷新课程"));

        let before = read_bytes(&cache_file(&cache));
        let mut failing = FakeSource::with(vec![Err(CliError::new(
            5,
            "upstream",
            "synthetic forced refresh failure",
            "",
        ))]);
        let error = select_snapshot(&config, &cache, today(), true, &mut failing)
            .expect_err("forced refresh failure");
        assert_eq!(failing.calls, 1);
        assert_eq!(error.code, 5);
        assert_eq!(
            read_bytes(&cache_file(&cache)),
            before,
            "a failed manual update must preserve the previous snapshot"
        );
    }

    #[test]
    fn injected_graduate_pages_refresh_a_complete_current_snapshot() {
        let root = tempdir().unwrap();
        let config = root.path().join("config");
        let cache = root.path().join("cache");
        graduate_account(&config);
        let pages = GradPages {
            schedule: Some(
                include_str!("../tests/fixtures/contracts/academics/timetable_graduate.html")
                    .to_string(),
            ),
            calendar: Some(complete_calendar()),
            exams: None,
        };
        let mut source = GraduatePageTimetable {
            pages: FakePageSource::with(pages),
        };

        let selected = select_snapshot(&config, &cache, today(), false, &mut source)
            .expect("injected graduate pages");

        assert_eq!(source.pages.calls, 1);
        assert_eq!(
            source.pages.wanted,
            vec![vec![GradPage::Schedule, GradPage::Calendar]]
        );
        assert!(!selected.from_cache);
        assert_eq!(selected.snapshot.role, "graduate");
        assert_eq!(selected.snapshot.source, "yjxt");
        assert_eq!(selected.snapshot.term, "2026-2027秋学期");
        assert_eq!(selected.snapshot.calendar.len(), 19);
        assert_eq!(selected.snapshot.courses.len(), 2);
        let written = read_snapshot(&cache_file(&cache)).expect("written graduate cache");
        assert_eq!(written.username, USER);
        assert_eq!(written.role, "graduate");
        assert_eq!(written.courses[0]["name"], json!("合成通信课程"));
    }

    #[test]
    fn malformed_graduate_calendar_preserves_the_previous_snapshot() {
        let root = tempdir().unwrap();
        let config = root.path().join("config");
        let cache = root.path().join("cache");
        graduate_account(&config);
        let old = snapshot(
            USER,
            Role::Graduate,
            monday_of(today()).add_days(-28),
            "旧研究生课程",
            "08:00",
            "08:45",
        );
        save_snapshot(&cache_file(&cache), &old).unwrap();
        let before = read_bytes(&cache_file(&cache));
        let pages = GradPages {
            schedule: Some(
                include_str!("../tests/fixtures/contracts/academics/timetable_graduate.html")
                    .to_string(),
            ),
            // The parser fixture deliberately only has the first and last
            // teaching weeks; the timetable references weeks 4..19.
            calendar: Some(
                include_str!("../tests/fixtures/contracts/academics/calendar_graduate.html")
                    .to_string(),
            ),
            exams: None,
        };
        let mut source = GraduatePageTimetable {
            pages: FakePageSource::with(pages),
        };

        let error = select_snapshot(&config, &cache, today(), false, &mut source)
            .expect_err("an incomplete calendar must not replace the snapshot");

        assert_eq!(source.pages.calls, 1);
        assert_eq!(error.code, 5);
        assert!(error.message.contains("教学周"));
        assert_eq!(read_bytes(&cache_file(&cache)), before);
        assert!(!cache_file(&cache).with_extension("json.tmp").exists());
    }

    #[test]
    fn exams_command_uses_injected_pages_and_keeps_the_empty_marker_explicit() {
        let root = tempdir().unwrap();
        let config = root.path().join("config");
        graduate_account(&config);
        let rows = GradPages {
            exams: Some(
                include_str!("../tests/fixtures/contracts/academics/exams_graduate_rows.html")
                    .to_string(),
            ),
            ..GradPages::default()
        };
        let mut source = FakePageSource::with(rows);
        let outcome = run_exams_with_source(&config, &mut source).expect("injected exam rows");
        assert_eq!(source.calls, 1);
        assert_eq!(source.wanted, vec![vec![GradPage::Exams]]);
        assert_eq!(outcome.data["count"], json!(1));
        assert_eq!(outcome.data["exams"][0]["course"], json!("合成通信网课程"));

        let empty = GradPages {
            exams: Some(
                include_str!("../tests/fixtures/contracts/academics/exams_graduate_empty.html")
                    .to_string(),
            ),
            ..GradPages::default()
        };
        let mut source = FakePageSource::with(empty);
        let outcome = run_exams_with_source(&config, &mut source).expect("empty exam table");
        assert_eq!(outcome.data["count"], json!(0));
        assert_eq!(outcome.data["exams"], json!([]));
        assert!(
            outcome.data["note"]
                .as_str()
                .is_some_and(|note| note.contains("暂无"))
        );
    }

    #[test]
    fn undergraduate_exams_are_refused_before_the_graduate_page_source() {
        let root = tempdir().unwrap();
        let config = root.path().join("config");
        fs::create_dir_all(&config).unwrap();
        fs::write(
            config.join("credentials.json"),
            serde_json::to_vec_pretty(&json!({
                "username": USER,
                "password": SESSION_PASSWORD,
                "role": "undergraduate",
            }))
            .unwrap(),
        )
        .unwrap();
        let mut source = FakePageSource::with(GradPages::default());

        let error = match run_exams_with_source(&config, &mut source) {
            Ok(_) => panic!("undergraduate exams must not use the graduate source"),
            Err(error) => error,
        };

        assert_eq!(error.code, 5);
        assert_eq!(source.calls, 0, "the source must not be contacted");
    }

    #[test]
    fn atomic_writes_leave_predictable_temp_files_untouched() {
        let root = tempdir().unwrap();
        let cache = root.path().join("cache");
        fs::create_dir_all(&cache).unwrap();
        let snapshot_path = cache_file(&cache);
        let snapshot_temp = snapshot_path.with_extension("json.tmp");
        fs::write(&snapshot_temp, b"PRESERVE CACHE TEMP").unwrap();

        save_snapshot(&snapshot_path, &fresh()).unwrap();

        assert_eq!(
            fs::read(&snapshot_temp).unwrap(),
            b"PRESERVE CACHE TEMP",
            "the old predictable cache temp path must not be reused"
        );
        assert!(read_snapshot(&snapshot_path).is_some());

        let export_path = root.path().join("export.ics");
        let export_temp = export_path.with_extension("ics.tmp");
        fs::write(&export_temp, b"PRESERVE EXPORT TEMP").unwrap();

        write_export(&export_path, "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n").unwrap();

        assert_eq!(
            fs::read(&export_temp).unwrap(),
            b"PRESERVE EXPORT TEMP",
            "the old predictable export temp path must not be reused"
        );
        assert!(
            fs::read_to_string(&export_path)
                .unwrap()
                .contains("BEGIN:VCALENDAR")
        );
    }

    #[cfg(unix)]
    #[test]
    fn atomic_writes_do_not_follow_planted_temp_symlinks() {
        use std::os::unix::fs::symlink;

        let root = tempdir().unwrap();
        let victim = root.path().join("victim");
        fs::write(&victim, b"UNRELATED VICTIM").unwrap();
        let cache = root.path().join("cache");
        fs::create_dir_all(&cache).unwrap();
        let snapshot_path = cache_file(&cache);
        let snapshot_temp = snapshot_path.with_extension("json.tmp");
        symlink(&victim, &snapshot_temp).unwrap();

        save_snapshot(&snapshot_path, &fresh()).unwrap();

        assert_eq!(fs::read(&victim).unwrap(), b"UNRELATED VICTIM");
        assert!(
            fs::symlink_metadata(&snapshot_temp)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_link(&snapshot_temp).unwrap(), victim);

        let export_path = root.path().join("export.ics");
        let export_temp = export_path.with_extension("ics.tmp");
        symlink(&victim, &export_temp).unwrap();

        write_export(&export_path, "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n").unwrap();

        assert_eq!(fs::read(&victim).unwrap(), b"UNRELATED VICTIM");
        assert!(
            fs::symlink_metadata(&export_temp)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_link(&export_temp).unwrap(), victim);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_refuses_a_symlink_target_without_touching_it() {
        use std::os::unix::fs::symlink;

        let root = tempdir().unwrap();
        let target = root.path().join("cache").join("timetable.json");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        let victim = root.path().join("victim");
        fs::write(&victim, b"UNRELATED VICTIM").unwrap();
        symlink(&victim, &target).unwrap();

        let error = save_snapshot(&target, &fresh()).expect_err("symlink target must be refused");

        assert_eq!(error.code, 5);
        assert!(error.message.contains("symlink"));
        assert_eq!(fs::read(&victim).unwrap(), b"UNRELATED VICTIM");
        assert!(
            fs::symlink_metadata(&target)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_link(&target).unwrap(), victim);
    }

    #[test]
    fn concurrent_atomic_writes_leave_one_complete_file() {
        let root = tempdir().unwrap();
        let target = root.path().join("shared.txt");
        let mut handles = Vec::new();
        for index in 0..8 {
            let target = target.clone();
            handles.push(std::thread::spawn(move || {
                let payload = format!("payload-{index}");
                write_atomically(&target, payload.as_bytes(), "test file").unwrap();
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }

        let content = fs::read_to_string(&target).unwrap();
        assert!(
            (0..8).any(|index| content == format!("payload-{index}")),
            "concurrent writers must leave one complete payload, got {content:?}"
        );
    }

    #[test]
    fn mobile_curriculum_refresh_uses_sjd_dates_and_writes_one_complete_snapshot() {
        let root = tempdir().unwrap();
        let config = root.path().join("config");
        let cache = root.path().join("cache");
        account(&config);
        let mut transport = Scripted::default();
        queue_mobile(
            &mut transport,
            "GET",
            "https://jwglweixin.bupt.edu.cn/bjyddx/student/curriculum",
            MOBILE_CURRICULUM,
        );
        let mut auth = TestMobileAuth::valid();
        let today = Date::parse("2026-09-01").expect("synthetic Tuesday");

        let selected = {
            let mut source = MobileTimetableSource::new(&mut transport, &mut auth);
            select_snapshot(&config, &cache, today, false, &mut source)
                .expect("complete undergraduate curriculum")
        };

        assert_eq!(transport.calls.len(), 1);
        assert_eq!(auth.calls, 1);
        assert_eq!(transport.calls[0].headers[2].1, JWT_TOKEN);
        assert!(!selected.from_cache);
        assert_eq!(selected.snapshot.role, "undergraduate");
        assert_eq!(selected.snapshot.source, "mobile_jwgl");
        assert_eq!(selected.snapshot.term, "2026-2027-1");
        assert_eq!(selected.snapshot.calendar[&1][0], "2026-08-31");
        assert_eq!(selected.snapshot.courses.len(), 2);
        assert_eq!(selected.snapshot.courses[0]["name"], json!("数字信号处理"));
        assert_eq!(selected.snapshot.courses[0]["period_start"], Value::Null);
        assert_eq!(selected.snapshot.courses[0]["period_end"], Value::Null);

        let written = read_snapshot(&cache_file(&cache)).expect("written mobile snapshot");
        assert_eq!(written.username, USER);
        assert_eq!(written.role, "undergraduate");
        assert_eq!(written.source, "mobile_jwgl");
        assert_eq!(written.calendar[&2][6], "2026-09-13");
    }

    #[test]
    fn mobile_current_week_only_response_never_replaces_the_old_snapshot() {
        let root = tempdir().unwrap();
        let config = root.path().join("config");
        let cache = root.path().join("cache");
        account(&config);
        save_snapshot(&cache_file(&cache), &stale()).unwrap();
        let before = read_bytes(&cache_file(&cache));

        let mut transport = Scripted::default();
        queue_mobile(
            &mut transport,
            "GET",
            "https://jwglweixin.bupt.edu.cn/bjyddx/student/curriculum",
            MOBILE_CURRICULUM_PARTIAL,
        );
        let mut auth = TestMobileAuth::valid();
        let today = Date::parse("2026-09-16").expect("synthetic Wednesday in week 3");
        let error = {
            let mut source = MobileTimetableSource::new(&mut transport, &mut auth);
            select_snapshot(&config, &cache, today, false, &mut source)
                .expect_err("a current-week-only grid must not replace the old term")
        };

        assert_eq!(error.code, 5);
        assert!(error.message.contains("第 1 教学周"));
        assert_eq!(transport.calls.len(), 1);
        assert_eq!(
            read_bytes(&cache_file(&cache)),
            before,
            "partial dates must leave the old snapshot byte-for-byte intact"
        );
    }

    #[test]
    fn mobile_exam_command_uses_three_reads_and_keeps_raw_rows() {
        let mut transport = Scripted::default();
        queue_mobile(
            &mut transport,
            "GET",
            "https://jwglweixin.bupt.edu.cn/bjyddx/kaowu/get_kszc",
            MOBILE_EXAM_WEEKS,
        );
        queue_mobile(
            &mut transport,
            "GET",
            "https://jwglweixin.bupt.edu.cn/bjyddx/kaowu/getKsapcx",
            MOBILE_EXAM_SCHEDULE,
        );
        queue_mobile(
            &mut transport,
            "GET",
            "https://jwglweixin.bupt.edu.cn/bjyddx/kaowu/findKcksapcx",
            MOBILE_EXAM_COURSES,
        );
        let mut auth = TestMobileAuth::valid();

        let outcome = run_exams_with_mobile(&mut transport, &mut auth).expect("exam rows");

        assert_eq!(outcome.data["weeks"][0]["key"], json!("1"));
        assert_eq!(outcome.data["exams"][0]["kcmc"], json!("合成考试课程"));
        assert_eq!(
            outcome.data["course_arrangements"][0]["kcmc"],
            json!("合成课程安排")
        );
        assert_eq!(auth.calls, 1);
        assert_eq!(transport.calls.len(), 3);
        assert!(
            transport
                .calls
                .iter()
                .all(|call| call.headers[2].1 == JWT_TOKEN)
        );
    }

    #[test]
    fn mobile_scores_and_classrooms_commands_preserve_filters_and_campus_query() {
        let mut score_transport = Scripted::default();
        queue_mobile(
            &mut score_transport,
            "POST",
            "https://jwglweixin.bupt.edu.cn/bjyddx/student/termGPA",
            MOBILE_SCORES,
        );
        let mut score_auth = TestMobileAuth::valid();
        let scores =
            run_scores_with_mobile(&mut score_transport, &mut score_auth, Some("2025-2026-2"))
                .expect("scores");

        assert_eq!(scores.data["count"], json!(1));
        assert_eq!(scores.data["student_id"], json!("20260001"));
        assert_eq!(scores.data["average_score"], json!(87.25));
        assert_eq!(
            scores.data["semesters"]["2025-2026-2"][0]["credit"],
            json!(3.5)
        );
        assert_eq!(score_transport.calls.len(), 1);
        assert_eq!(score_transport.calls[0].method, "POST");
        assert!(score_transport.calls[0].body.is_empty());

        let mut classroom_transport = Scripted::default();
        queue_mobile(
            &mut classroom_transport,
            "GET",
            "https://jwglweixin.bupt.edu.cn/bjyddx/todayClassrooms?campusId=01",
            MOBILE_CLASSROOMS,
        );
        let mut classroom_auth = TestMobileAuth::valid();
        let classrooms =
            run_classrooms_with_mobile(&mut classroom_transport, &mut classroom_auth, "01")
                .expect("classrooms");

        assert_eq!(classrooms.data["campus"], json!("01"));
        assert_eq!(classrooms.data["count"], json!(2));
        assert_eq!(
            classrooms.data["periods"][0]["rooms"][0],
            json!({"name":"教一楼-101","capacity":40})
        );
    }

    #[test]
    fn native_mobile_token_refusal_does_not_retry_or_echo_the_response() {
        let mut transport = Scripted::default();
        transport.expected.push_back((
            "POST",
            "https://jwglweixin.bupt.edu.cn/bjyddx/student/termGPA".into(),
            Ok(Response {
                status: 401,
                headers: vec![],
                body: b"synthetic-secret".to_vec(),
            }),
        ));
        let mut auth = mobile_jwgl::NativeMobileJwglAuth::new("TEST-ONLY-TOKEN".into());
        let error = match run_scores_with_mobile(&mut transport, &mut auth, None) {
            Ok(_) => panic!("refused token must not succeed"),
            Err(error) => error,
        };
        assert_eq!(error.code, 3);
        assert!(!error.message.contains("synthetic-secret"));
        assert_eq!(transport.calls.len(), 1);
    }
}
