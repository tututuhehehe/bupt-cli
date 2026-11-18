use crate::cli::{CliError, CliResult, Context, Outcome};
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::time::{SystemTime, UNIX_EPOCH};

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
        "scores" => run_scores(ctx),
        "classrooms" => run_classrooms(matches, ctx),
        _ => Err(CliError::usage("unknown command")),
    }
}

fn run_schedule(matches: &ArgMatches, ctx: &Context<'_>) -> CliResult<Outcome> {
    if let Some((subcommand, sub)) = matches.subcommand() {
        return match subcommand {
            "update" => Err(pending_remote("课表刷新")),
            "calendar" => {
                let snapshot = owned_snapshot(ctx)?;
                Ok(outcome(calendar_payload(&snapshot)))
            }
            "export" => export_schedule(sub, ctx),
            _ => Err(CliError::usage(format!(
                "unknown schedule subcommand: {subcommand}"
            ))),
        };
    }

    let snapshot = owned_snapshot(ctx)?;
    if matches.get_flag("raw") {
        return Ok(outcome(json!({
            "term": snapshot.term,
            "role": snapshot.role,
            "from_cache": true,
            "count": snapshot.courses.len(),
            "courses": snapshot.courses,
        })));
    }

    let today = utc_today();
    let (start, end, label) = if let Some(date) = matches.get_one::<String>("date") {
        let day = Date::parse(date).expect("clap validated --date");
        (day, day, date.clone())
    } else if matches.get_flag("today") {
        (today, today, today.iso())
    } else if matches.get_flag("tomorrow") {
        let day = today.add_days(1);
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
        let (start, end) = day_range(&snapshot, today);
        (start, end, "本周".to_string())
    };

    let sessions = occurrences(&snapshot, start, end);
    Ok(outcome(json!({
        "term": snapshot.term,
        "role": snapshot.role,
        "from_cache": true,
        "range": [start.iso(), end.iso()],
        "week": week_of(&snapshot, start),
        "label": label,
        "count": sessions.len(),
        "classes": sessions.into_iter().map(|session| session.data).collect::<Vec<_>>(),
    })))
}

fn run_exams(ctx: &Context<'_>) -> CliResult<Outcome> {
    let account = load_account(ctx, true)?;
    validate_role(&account)?;
    let label = if account.role == "graduate" {
        "研究生考试数据源"
    } else {
        "本科考试数据源"
    };
    Err(pending_remote(label))
}

fn run_scores(ctx: &Context<'_>) -> CliResult<Outcome> {
    let account = load_account(ctx, true)?;
    validate_role(&account)?;
    require_undergraduate(&account, "scores")?;
    Err(pending_remote("本科成绩数据源"))
}

fn run_classrooms(matches: &ArgMatches, ctx: &Context<'_>) -> CliResult<Outcome> {
    let account = load_account(ctx, true)?;
    validate_role(&account)?;
    require_undergraduate(&account, "classrooms")?;
    let _campus = matches
        .get_one::<String>("campus")
        .expect("campus has a validated default");
    Err(pending_remote("本科空教室数据源"))
}

fn require_undergraduate(account: &Account, command: &str) -> CliResult<()> {
    if account.role == "graduate" {
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

struct Account {
    username: String,
    role: String,
}

fn load_account(ctx: &Context<'_>, require_password: bool) -> CliResult<Account> {
    let path = ctx.config_dir.join("credentials.json");
    let mut config = Map::new();
    if path.exists() {
        let raw = fs::read(&path).map_err(|_| credential_error(&path))?;
        let value: Value = serde_json::from_slice(&raw).map_err(|_| credential_error(&path))?;
        config = value
            .as_object()
            .cloned()
            .ok_or_else(|| credential_error(&path))?;
    }

    let env = |key: &str| {
        std::env::var(key)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    let from_config = |key: &str| {
        config
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };

    let username = env("BUPT_USERNAME")
        .or_else(|| from_config("username"))
        .unwrap_or_default();
    let password = env("BUPT_PASSWORD")
        .or_else(|| from_config("password"))
        .unwrap_or_default();
    let role = env("BUPT_ROLE")
        .or_else(|| from_config("role"))
        .map(|role| normalize_role(&role))
        .unwrap_or_default();

    if username.is_empty() || (require_password && password.is_empty()) {
        return Err(credential_error(&path));
    }
    Ok(Account { username, role })
}

fn credential_error(path: &Path) -> CliError {
    CliError::new(
        3,
        "auth",
        "BUPT credentials are not configured",
        format!(
            "设置 BUPT_USERNAME/BUPT_PASSWORD，或检查 {}",
            path.display()
        ),
    )
}

fn normalize_role(role: &str) -> String {
    match role.trim().to_lowercase().as_str() {
        "graduate" | "研究生" | "硕士" | "博士" | "grad" | "postgrad" => "graduate".into(),
        "undergraduate" | "本科生" | "本科" | "undergrad" | "ug" => "undergraduate".into(),
        other => other.to_string(),
    }
}

fn validate_role(account: &Account) -> CliResult<()> {
    match account.role.as_str() {
        "graduate" | "undergraduate" => Ok(()),
        "" => Err(role_error("账号身份未登记")),
        other => Err(role_error(format!("账号身份 `{other}` 不受支持"))),
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

struct Snapshot {
    term: String,
    role: String,
    courses: Vec<Value>,
    calendar: BTreeMap<i64, Vec<String>>,
}

fn owned_snapshot(ctx: &Context<'_>) -> CliResult<Snapshot> {
    let account = load_account(ctx, false)?;
    validate_role(&account)?;
    let path = ctx.cache_dir.join("timetable.json");
    let raw = fs::read(&path)
        .map_err(|_| refresh_required("课表快照不存在或无法读取，不能确认它属于当前账号"))?;
    let value: Value =
        serde_json::from_slice(&raw).map_err(|_| refresh_required("课表快照不是有效的 JSON"))?;
    let object = value
        .as_object()
        .ok_or_else(|| refresh_required("课表快照不是 JSON 对象"))?;

    if object.get("version").and_then(Value::as_i64) != Some(2) {
        return Err(refresh_required("课表快照版本不受支持，无法确认旧快照归属"));
    }
    let username = object.get("username").and_then(Value::as_str).unwrap_or("");
    if username != account.username {
        return Err(refresh_required("课表快照属于另一个学号，不能用于当前账号"));
    }
    let role = object.get("role").and_then(Value::as_str).unwrap_or("");
    if role != account.role {
        return Err(refresh_required(
            "课表快照属于另一种校园身份，不能用于当前账号",
        ));
    }
    let courses = object
        .get("courses")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| refresh_required("课表快照缺少课程列表"))?;
    let calendar_object = object
        .get("calendar")
        .and_then(Value::as_object)
        .ok_or_else(|| refresh_required("课表快照缺少教学周日历"))?;
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

    Ok(Snapshot {
        term: object
            .get("term")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        role: role.to_string(),
        courses,
        calendar,
    })
}

fn refresh_required(message: impl Into<String>) -> CliError {
    CliError::new(
        2,
        "usage",
        message,
        "运行 `bupt schedule update` 刷新当前学号的课表快照。",
    )
}

// ---------------------------------------------------------------------------
// Snapshot queries and iCalendar export.
// ---------------------------------------------------------------------------

fn calendar_payload(snapshot: &Snapshot) -> Value {
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
        "from_cache": true,
        "week_count": weeks.len(),
        "weeks": weeks,
    })
}

fn export_schedule(matches: &ArgMatches, ctx: &Context<'_>) -> CliResult<Outcome> {
    let snapshot = owned_snapshot(ctx)?;
    let content = build_ics(&snapshot)?;
    let out = matches
        .get_one::<String>("out")
        .map(|path| expand_home(path, &ctx.home_dir))
        .unwrap_or_else(|| default_export_path(&snapshot, &ctx.home_dir));
    if let Some(parent) = out.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .map_err(|_| CliError::new(5, "upstream", "cannot create export directory", ""))?;
    }
    fs::write(&out, &content)
        .map_err(|_| CliError::new(5, "upstream", "cannot write iCalendar file", ""))?;

    if !matches.get_flag("no-open-folder") {
        reveal(&out);
    }

    Ok(outcome(json!({
        "term": snapshot.term,
        "from_cache": true,
        "courses": snapshot.courses.len(),
        "weeks": snapshot.calendar.len(),
        "events": content.matches("BEGIN:VEVENT").count(),
        "path": out.display().to_string(),
        "hint": "在日历 App 里双击导入；重复导入会生成重复事件",
    })))
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

fn build_ics(snapshot: &Snapshot) -> CliResult<String> {
    let (start, end) =
        week_bounds(snapshot).ok_or_else(|| refresh_required("课表快照里没有可用的日期信息"))?;
    let sessions = occurrences(snapshot, start, end);
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
        if session.time_start.is_empty() || session.time_end.is_empty() {
            continue;
        }
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
    Ok(format!("{}\r\n", lines.join("\r\n")))
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

fn week_bounds(snapshot: &Snapshot) -> Option<(Date, Date)> {
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
        if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
            return None;
        }
        let year = text[0..4].parse::<i32>().ok()?;
        let month = text[5..7].parse::<u32>().ok()?;
        let day = text[8..10].parse::<u32>().ok()?;
        if month == 0 || month > 12 || day == 0 || day > days_in_month(year, month) {
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

fn utc_today() -> Date {
    Date::from_days((unix_seconds() / 86_400) as i64)
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
