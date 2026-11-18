mod contract_support;

use contract_support::{Invocation, Sandbox};
use serde_json::{Map, Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const USER: &str = "20260001";
const OTHER_USER: &str = "20260002";
// Synthetic only. Tests must never contain a real campus password.
const PASSWORD: &str = "synthetic-test-password";

// ---------------------------------------------------------------------------
// Time helpers. The cache must look current whenever the suite runs, so the
// fixtures are generated around the UTC day instead of being pinned to a date
// that will eventually fall outside every teaching week.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Date {
    year: i32,
    month: u32,
    day: u32,
}

impl Date {
    fn from_days(days: i64) -> Self {
        // Howard Hinnant's civil-from-days algorithm.
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

    fn days(self) -> i64 {
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
        Self::from_days(self.days() + days)
    }

    fn weekday_index(self) -> usize {
        (self.days() + 3).rem_euclid(7) as usize
    }

    fn iso(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    fn ics_date(self) -> String {
        format!("{:04}{:02}{:02}", self.year, self.month, self.day)
    }
}

fn utc_today() -> Date {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_secs();
    Date::from_days((seconds / 86_400) as i64)
}

fn monday_of(day: Date) -> Date {
    day.add_days(-(day.weekday_index() as i64))
}

// ---------------------------------------------------------------------------
// Sandbox helpers.
// ---------------------------------------------------------------------------

fn config_path(sandbox: &Sandbox) -> PathBuf {
    sandbox.root().join("config").join("credentials.json")
}

fn cache_path(sandbox: &Sandbox) -> PathBuf {
    sandbox.root().join("cache").join("timetable.json")
}

fn write_credentials(sandbox: &Sandbox, username: &str, role: &str) {
    fs::write(
        config_path(sandbox),
        serde_json::to_vec_pretty(&json!({
            "username": username,
            "password": PASSWORD,
            "role": role,
        }))
        .expect("serialize synthetic credentials"),
    )
    .expect("write synthetic credentials");
}

fn write_cache(sandbox: &Sandbox, entry: &Value) -> (PathBuf, Vec<u8>) {
    let path = cache_path(sandbox);
    let bytes = serde_json::to_vec_pretty(entry).expect("serialize timetable fixture");
    fs::write(&path, &bytes).expect("write timetable fixture");
    (path, bytes)
}

fn network_guarded_command(sandbox: &Sandbox) -> Command {
    let mut command = sandbox.command();
    // Best-effort only: HTTP clients that honor proxy variables fail against a
    // closed local port. This does not cover browser/non-HTTP traffic and does
    // not prove the CLI made no network attempt; injected transport scenarios
    // must assert the call count explicitly.
    for name in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"] {
        command.env(name, "http://127.0.0.1:9");
    }
    for name in ["http_proxy", "https_proxy", "all_proxy"] {
        command.env(name, "http://127.0.0.1:9");
    }
    command.env("NO_PROXY", "").env("no_proxy", "");
    command
}

fn run(sandbox: &Sandbox, args: &[&str]) -> Invocation {
    Invocation(
        network_guarded_command(sandbox)
            .args(args)
            .output()
            .expect("run isolated CLI"),
    )
}

fn run_owned(sandbox: &Sandbox, args: &[String]) -> Invocation {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run(sandbox, &refs)
}

fn error_text(payload: &Value) -> String {
    format!(
        "{} {}",
        payload["error"]["message"].as_str().unwrap_or(""),
        payload["error"]["hint"].as_str().unwrap_or("")
    )
}

fn assert_schema(payload: &Value) {
    assert_eq!(
        payload["schema"],
        json!("bupt.v1"),
        "every business document needs the public schema: {payload}"
    );
}

fn assert_usage(invocation: Invocation) -> Value {
    assert_eq!(
        invocation.code(),
        2,
        "expected usage exit; stdout={:?}; stderr={:?}",
        String::from_utf8_lossy(&invocation.0.stdout),
        String::from_utf8_lossy(&invocation.0.stderr)
    );
    assert!(
        invocation.0.stdout.is_empty(),
        "usage errors must not also print a success document: {:?}",
        String::from_utf8_lossy(&invocation.0.stdout)
    );
    let payload = invocation.stderr_json();
    assert_schema(&payload);
    assert_eq!(payload["ok"], json!(false));
    assert_eq!(payload["error"]["type"], json!("usage"));
    payload
}

fn assert_success(invocation: &Invocation) -> Value {
    assert_eq!(
        invocation.code(),
        0,
        "expected success; stdout={:?}; stderr={:?}",
        String::from_utf8_lossy(&invocation.0.stdout),
        String::from_utf8_lossy(&invocation.0.stderr)
    );
    assert!(
        invocation.0.stderr.is_empty(),
        "success must keep stderr empty: {:?}",
        String::from_utf8_lossy(&invocation.0.stderr)
    );
    let payload = invocation.stdout_json();
    assert_schema(&payload);
    assert_eq!(payload["ok"], json!(true));
    payload
}

fn assert_refresh_required(invocation: Invocation, cache: &Path, before: &[u8], label: &str) {
    assert_ne!(
        invocation.code(),
        0,
        "{label}: an unusable or foreign snapshot must not be served"
    );
    assert!(
        invocation.0.stdout.is_empty(),
        "{label}: refusal must not print timetable data: {:?}",
        String::from_utf8_lossy(&invocation.0.stdout)
    );
    let payload = invocation.stderr_json();
    assert_schema(&payload);
    assert_eq!(payload["ok"], json!(false), "{label}: {payload}");
    let text = error_text(&payload);
    assert!(
        text.contains("schedule update"),
        "{label}: refusal must point at the explicit refresh entry, got: {text}"
    );
    assert_eq!(
        fs::read(cache).expect("read cache after refusal"),
        before,
        "{label}: a cached query must not rewrite the snapshot"
    );
    assert!(
        !cache.with_extension("json.tmp").exists(),
        "{label}: no temporary timetable file should be left behind"
    );
}

// ---------------------------------------------------------------------------
// Synthetic timetable snapshot. The owner fields (`username` + `role`) are the
// proposed v2 ownership metadata; v1 has `role` but no account and therefore
// cannot be attributed.
// ---------------------------------------------------------------------------

struct Snapshot {
    entry: Value,
    monday: Date,
    class_date: Date,
    second_date: Date,
    last_date: Date,
}

fn synthetic_snapshot(username: &str, role: &str) -> Snapshot {
    let today = utc_today();
    let monday = monday_of(today.add_days(-21));
    let mut calendar = Map::new();
    for week in 1..=8 {
        let start = monday.add_days(7 * (week - 1));
        let dates: Vec<Value> = (0..7)
            .map(|offset| json!(start.add_days(offset).iso()))
            .collect();
        calendar.insert(week.to_string(), Value::Array(dates));
    }

    let class_date = monday.add_days(7 + 2); // week 2, Wednesday
    let second_date = monday.add_days(14 + 2); // week 3, Wednesday
    let last_date = monday.add_days(49 + 6); // week 8, Sunday
    let source = if role == "graduate" {
        "yjxt"
    } else {
        "mobile_jwgl"
    };
    let term = if today.month >= 8 {
        format!("{}-{}秋学期", today.year, today.year + 1)
    } else {
        format!("{}-{}春学期", today.year - 1, today.year)
    };

    Snapshot {
        entry: json!({
            "version": 2,
            "role": role,
            "username": username,
            "term": term,
            "source": source,
            "fetched_at": "2026-11-15T00:00:00+00:00",
            "courses": [
                {
                    "name": "光纤通信原理",
                    "weekday_index": 2,
                    "weeks": [2, 3],
                    "period_start": 1,
                    "period_end": 2,
                    "time_start": "08:00",
                    "time_end": "09:35",
                    "teachers": ["测试教师甲"],
                    "rooms": ["3-105"],
                    "campus": "西土城"
                },
                {
                    "name": "合成实验",
                    "weekday_index": 6,
                    "weeks": [8],
                    "period_start": 5,
                    "period_end": 5,
                    "time_start": "11:30",
                    "time_end": "12:15",
                    "teachers": ["测试教师乙"],
                    "rooms": ["实验楼-101"],
                    "campus": "沙河"
                },
                {
                    "name": "今日合成课程",
                    "weekday_index": today.weekday_index(),
                    "weeks": [4],
                    "period_start": 7,
                    "period_end": 7,
                    "time_start": "13:50",
                    "time_end": "14:35",
                    "teachers": ["测试教师丙"],
                    "rooms": ["教三楼-201"],
                    "campus": "西土城"
                }
            ],
            "calendar": calendar,
        }),
        monday,
        class_date,
        second_date,
        last_date,
    }
}

fn snapshot_read_commands(class_date: &str, out: &Path) -> Vec<Vec<String>> {
    vec![
        vec![
            "schedule".to_string(),
            "--date".to_string(),
            class_date.to_string(),
        ],
        vec!["schedule".to_string(), "calendar".to_string()],
        vec![
            "schedule".to_string(),
            "export".to_string(),
            "--out".to_string(),
            out.display().to_string(),
            "--no-open-folder".to_string(),
        ],
    ]
}

fn assert_snapshot_refused(entry: &Value, class_date: Date, label: &str) {
    let sandbox = Sandbox::new();
    write_credentials(&sandbox, USER, "graduate");
    let (cache, before) = write_cache(&sandbox, entry);
    let out = sandbox.root().join(format!("{label}.ics"));

    for args in snapshot_read_commands(&class_date.iso(), &out) {
        let invocation = run_owned(&sandbox, &args);
        assert_refresh_required(invocation, &cache, &before, label);
    }
    assert!(
        !out.exists(),
        "{label}: an export from a foreign snapshot must not create a file"
    );
}

// ---------------------------------------------------------------------------
// Invalid arguments come first: before credentials, cache access, network or
// a browser.
// ---------------------------------------------------------------------------

#[test]
fn invalid_arguments_are_usage_errors_without_credentials_or_cache() {
    let sandbox = Sandbox::new();
    let cases: &[&[&str]] = &[
        &["schedule", "--date", "2026-02-30"],
        &["schedule", "--date", "not-a-date"],
        &["schedule", "--week", "0"],
        &["schedule", "--week", "nope"],
        &["schedule", "export", "--out"],
        &["exams", "unexpected"],
        &["scores", "--semester"],
        &["classrooms", "--campus", "99"],
    ];

    for args in cases {
        let payload = assert_usage(run(&sandbox, args));
        let text = error_text(&payload);
        assert!(
            !text.contains("credentials are not configured"),
            "{args:?}: argument validation ran after credential loading: {text}"
        );
        assert!(
            fs::read_dir(sandbox.root().join("cache"))
                .expect("read cache dir")
                .next()
                .is_none(),
            "{args:?}: invalid arguments created cache state"
        );
    }
}

#[test]
fn invalid_arguments_leave_an_existing_snapshot_untouched() {
    let sandbox = Sandbox::new();
    let snapshot = synthetic_snapshot(USER, "graduate");
    write_credentials(&sandbox, USER, "graduate");
    let (cache, before) = write_cache(&sandbox, &snapshot.entry);

    let cases: &[&[&str]] = &[
        &["schedule", "--date", "2026-02-30"],
        &["schedule", "--week", "0"],
        &["schedule", "export", "--out"],
        &["classrooms", "--campus", "99"],
    ];
    for args in cases {
        assert_usage(run(&sandbox, args));
        assert_eq!(
            fs::read(&cache).expect("read unchanged cache"),
            before,
            "{args:?}: invalid arguments rewrote the timetable"
        );
    }
}

#[test]
fn removed_schedule_exams_alias_is_a_usage_error_without_side_effects() {
    let sandbox = Sandbox::new();
    let snapshot = synthetic_snapshot(USER, "graduate");
    write_credentials(&sandbox, USER, "graduate");
    let (cache, before) = write_cache(&sandbox, &snapshot.entry);

    assert_usage(run(&sandbox, &["schedule", "exams"]));
    assert_eq!(fs::read(&cache).expect("read unchanged cache"), before);
}

// ---------------------------------------------------------------------------
// A matching snapshot is read from disk, never refreshed implicitly.
// ---------------------------------------------------------------------------

#[test]
fn cached_schedule_serves_only_the_matching_account_and_role() {
    let sandbox = Sandbox::new();
    let snapshot = synthetic_snapshot(USER, "graduate");
    write_credentials(&sandbox, USER, "graduate");
    let (cache, before) = write_cache(&sandbox, &snapshot.entry);
    let date = snapshot.class_date.iso();

    let payload = assert_success(&run(&sandbox, &["schedule", "--date", &date]));
    assert_eq!(payload["role"], json!("graduate"));
    assert_eq!(payload["from_cache"], json!(true));
    assert_eq!(payload["range"], json!([date.clone(), date.clone()]));
    assert_eq!(payload["week"], json!(2));
    assert_eq!(payload["count"], json!(1));
    assert_eq!(payload["classes"][0]["date"], json!(date));
    assert_eq!(payload["classes"][0]["weekday"], json!("星期三"));
    assert_eq!(payload["classes"][0]["week"], json!(2));
    assert_eq!(payload["classes"][0]["name"], json!("光纤通信原理"));
    assert_eq!(payload["classes"][0]["time_start"], json!("08:00"));
    assert_eq!(payload["classes"][0]["time_end"], json!("09:35"));
    assert_eq!(payload["classes"][0]["teachers"], json!(["测试教师甲"]));
    assert_eq!(payload["classes"][0]["rooms"], json!(["3-105"]));
    assert_eq!(
        fs::read(&cache).expect("read cache after query"),
        before,
        "a cached query must not refresh the snapshot"
    );
}

#[test]
fn cached_calendar_is_week_ordered_and_monday_anchored() {
    let sandbox = Sandbox::new();
    let snapshot = synthetic_snapshot(USER, "graduate");
    write_credentials(&sandbox, USER, "graduate");
    let (cache, before) = write_cache(&sandbox, &snapshot.entry);

    let payload = assert_success(&run(&sandbox, &["schedule", "calendar"]));
    assert_eq!(payload["role"], json!("graduate"));
    assert_eq!(payload["from_cache"], json!(true));
    assert_eq!(payload["week_count"], json!(8));
    assert_eq!(payload["weeks"][0]["week"], json!(1));
    assert_eq!(payload["weeks"][0]["monday"], json!(snapshot.monday.iso()));
    assert_eq!(
        payload["weeks"][0]["sunday"],
        json!(snapshot.monday.add_days(6).iso())
    );
    assert_eq!(payload["weeks"][0]["dates"].as_array().unwrap().len(), 7);
    assert_eq!(payload["weeks"][7]["week"], json!(8));
    assert_eq!(
        payload["weeks"][7]["sunday"],
        json!(snapshot.last_date.iso())
    );
    assert_eq!(fs::read(&cache).expect("read cache after calendar"), before);
}

#[test]
fn cached_week_query_resolves_real_dates_and_inclusive_range() {
    let sandbox = Sandbox::new();
    let snapshot = synthetic_snapshot(USER, "graduate");
    write_credentials(&sandbox, USER, "graduate");
    let (cache, before) = write_cache(&sandbox, &snapshot.entry);

    let payload = assert_success(&run(&sandbox, &["schedule", "--week", "2"]));
    let start = snapshot.monday.add_days(7).iso();
    let end = snapshot.monday.add_days(13).iso();
    assert_eq!(payload["from_cache"], json!(true));
    assert_eq!(payload["range"], json!([start, end]));
    assert_eq!(payload["week"], json!(2));
    assert_eq!(payload["count"], json!(1));
    assert_eq!(
        payload["classes"][0]["date"],
        json!(snapshot.class_date.iso())
    );
    assert_eq!(
        fs::read(&cache).expect("read cache after week query"),
        before
    );
}

#[test]
fn cached_export_writes_an_anonymized_ics_with_real_dates() {
    let sandbox = Sandbox::new();
    let snapshot = synthetic_snapshot(USER, "graduate");
    write_credentials(&sandbox, USER, "graduate");
    let (cache, before) = write_cache(&sandbox, &snapshot.entry);
    let out = sandbox.root().join("week.ics");

    let payload = assert_success(&run(
        &sandbox,
        &[
            "schedule",
            "export",
            "--out",
            out.to_str().unwrap(),
            "--no-open-folder",
        ],
    ));
    assert_eq!(payload["from_cache"], json!(true));
    assert_eq!(payload["events"], json!(4));
    assert_eq!(payload["path"], json!(out.to_str().unwrap()));

    let ics = fs::read_to_string(&out).expect("read exported ICS");
    assert!(ics.contains("\r\n"), "ICS must use CRLF: {ics:?}");
    assert_eq!(ics.matches("BEGIN:VEVENT").count(), 4);
    assert!(ics.contains(&format!(
        "DTSTART;TZID=Asia/Shanghai:{}T080000",
        snapshot.class_date.ics_date()
    )));
    assert!(ics.contains(&format!(
        "DTEND;TZID=Asia/Shanghai:{}T093500",
        snapshot.class_date.ics_date()
    )));
    assert!(ics.contains(&format!(
        "DTSTART;TZID=Asia/Shanghai:{}T080000",
        snapshot.second_date.ics_date()
    )));
    assert!(ics.contains(&format!(
        "DTSTART;TZID=Asia/Shanghai:{}T113000",
        snapshot.last_date.ics_date()
    )));
    assert!(ics.contains("SUMMARY:光纤通信原理"));
    assert!(ics.contains("LOCATION:[西土城] 3-105"));
    for line in ics.split("\r\n") {
        assert!(
            line.as_bytes().len() <= 76,
            "ICS line exceeds the RFC 5545 fold width ({} bytes): {line:?}",
            line.as_bytes().len()
        );
    }
    assert_eq!(
        fs::read(&cache).expect("read cache after export"),
        before,
        "export must not refresh the snapshot"
    );
}

#[test]
fn ics_folds_long_utf8_lines() {
    let sandbox = Sandbox::new();
    let snapshot = synthetic_snapshot(USER, "graduate");
    let mut entry = snapshot.entry.clone();
    let long_name = "很长的合成课程名称".repeat(8);
    entry["courses"] = json!([{
        "name": long_name,
        "weekday_index": 2,
        "weeks": [2],
        "period_start": 1,
        "period_end": 2,
        "time_start": "08:00",
        "time_end": "09:35",
        "teachers": [],
        "rooms": [],
        "campus": ""
    }]);
    write_credentials(&sandbox, USER, "graduate");
    write_cache(&sandbox, &entry);
    let out = sandbox.root().join("long.ics");

    let payload = assert_success(&run(
        &sandbox,
        &[
            "schedule",
            "export",
            "--out",
            out.to_str().unwrap(),
            "--no-open-folder",
        ],
    ));
    assert_eq!(payload["events"], json!(1));
    let ics = fs::read_to_string(&out).expect("read exported ICS");
    assert!(ics.contains("BEGIN:VEVENT"));
    assert!(ics.contains("\r\n"), "ICS must use CRLF: {ics:?}");
    assert!(
        ics.contains("\r\n "),
        "the long SUMMARY must have a folded continuation: {ics:?}"
    );
    for line in ics.split("\r\n") {
        assert!(
            line.as_bytes().len() <= 76,
            "folded line is too wide ({} bytes): {line:?}",
            line.as_bytes().len()
        );
    }

    // RFC 5545 unfolding: remove CRLF plus the single leading continuation
    // space, then compare the original summary byte-for-byte.
    let unfolded = ics.replace("\r\n ", "");
    let summary_line = unfolded
        .lines()
        .find(|line| line.starts_with("SUMMARY:"))
        .expect("unfolded SUMMARY line");
    assert_eq!(summary_line, format!("SUMMARY:{long_name}"));
    assert!(
        !ics.lines().any(|line| line == summary_line),
        "the complete SUMMARY must not fit on one physical line: {ics:?}"
    );
}

#[cfg(unix)]
#[test]
fn export_succeeds_even_when_opening_the_folder_fails() {
    use std::os::unix::fs::PermissionsExt;

    let sandbox = Sandbox::new();
    let snapshot = synthetic_snapshot(USER, "graduate");
    write_credentials(&sandbox, USER, "graduate");
    write_cache(&sandbox, &snapshot.entry);

    let bin = sandbox.root().join("fake-bin");
    fs::create_dir(&bin).expect("create fake bin");
    let sentinel = sandbox.root().join("opener-invoked");
    let opener = bin.join("open");
    fs::write(
        &opener,
        "#!/bin/sh\nprintf invoked > \"$BUPT_TEST_OPEN_SENTINEL\"\nexit 9\n",
    )
    .expect("write failing opener");
    let mut permissions = fs::metadata(&opener).expect("stat opener").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&opener, permissions).expect("make opener executable");

    let out = sandbox.root().join("opener-failure.ics");
    let path = format!("{}:/usr/bin:/bin", bin.display());
    let invocation = Invocation(
        network_guarded_command(&sandbox)
            .env("PATH", path)
            .env("BUPT_TEST_OPEN_SENTINEL", &sentinel)
            .args(["schedule", "export", "--out", out.to_str().unwrap()])
            .output()
            .expect("run export with failing opener"),
    );
    let payload = assert_success(&invocation);
    assert_eq!(payload["events"], json!(4));
    assert!(
        out.exists(),
        "the ICS must exist despite the opener failure"
    );
    assert_eq!(
        fs::read_to_string(&sentinel).expect("fake opener was not invoked"),
        "invoked"
    );
}

// ---------------------------------------------------------------------------
// Unusable, foreign and unattributed snapshots are refused with a refresh hint.
// ---------------------------------------------------------------------------

#[test]
fn corrupt_and_wrong_version_caches_require_an_explicit_update() {
    let cases = [
        ("corrupt", b"{not json".to_vec()),
        (
            "wrong-version",
            serde_json::to_vec(&json!({
                "version": 999,
                "role": "graduate",
                "courses": [],
                "calendar": {}
            }))
            .unwrap(),
        ),
        (
            "missing-courses",
            serde_json::to_vec(&json!({
                "version": 2,
                "username": USER,
                "role": "graduate",
                "calendar": {}
            }))
            .unwrap(),
        ),
    ];

    for (label, bytes) in cases {
        let sandbox = Sandbox::new();
        write_credentials(&sandbox, USER, "graduate");
        let cache = cache_path(&sandbox);
        fs::write(&cache, &bytes).expect("write unusable cache");
        let invocation = run(&sandbox, &["schedule", "calendar"]);
        assert_refresh_required(invocation, &cache, &bytes, label);
    }
}

#[test]
fn unattributed_v1_cache_is_refused_instead_of_guessed() {
    let snapshot = synthetic_snapshot(USER, "graduate");
    let mut entry = snapshot.entry.clone();
    entry["version"] = json!(1);
    entry
        .as_object_mut()
        .expect("snapshot object")
        .remove("username");
    assert_snapshot_refused(&entry, snapshot.class_date, "v1-unattributed");
}

#[test]
fn cache_owned_by_another_account_is_refused() {
    let snapshot = synthetic_snapshot(OTHER_USER, "graduate");
    assert_snapshot_refused(&snapshot.entry, snapshot.class_date, "other-account");
}

#[test]
fn cache_for_another_role_is_refused() {
    let snapshot = synthetic_snapshot(USER, "undergraduate");
    assert_snapshot_refused(&snapshot.entry, snapshot.class_date, "other-role");
}

#[test]
fn out_of_range_week_points_at_explicit_update_and_keeps_the_snapshot() {
    let sandbox = Sandbox::new();
    let snapshot = synthetic_snapshot(USER, "graduate");
    write_credentials(&sandbox, USER, "graduate");
    let (cache, before) = write_cache(&sandbox, &snapshot.entry);

    let payload = assert_usage(run(&sandbox, &["schedule", "--week", "9"]));
    assert!(
        error_text(&payload).contains("schedule update"),
        "out-of-range week should tell the user how to refresh: {payload}"
    );
    assert_eq!(fs::read(&cache).expect("read unchanged cache"), before);
}

#[test]
fn invalid_weekday_entries_are_ignored_not_wrapped_or_fabricated() {
    let sandbox = Sandbox::new();
    let snapshot = synthetic_snapshot(USER, "graduate");
    let mut entry = snapshot.entry.clone();
    entry["courses"] = json!([
        {
            "name": "越界课程A",
            "weekday_index": -1,
            "weeks": [2],
            "period_start": 1,
            "period_end": 1,
            "time_start": "08:00",
            "time_end": "08:45",
            "teachers": [],
            "rooms": [],
            "campus": ""
        },
        {
            "name": "越界课程B",
            "weekday_index": 7,
            "weeks": [2],
            "period_start": 1,
            "period_end": 1,
            "time_start": "08:00",
            "time_end": "08:45",
            "teachers": [],
            "rooms": [],
            "campus": ""
        },
        {
            "name": "越界课程C",
            "weekday_index": 99,
            "weeks": [2],
            "period_start": 1,
            "period_end": 1,
            "time_start": "08:00",
            "time_end": "08:45",
            "teachers": [],
            "rooms": [],
            "campus": ""
        }
    ]);
    write_credentials(&sandbox, USER, "graduate");
    let (cache, before) = write_cache(&sandbox, &entry);
    let date = snapshot.class_date.iso();

    let payload = assert_success(&run(&sandbox, &["schedule", "--date", &date]));
    assert_eq!(payload["count"], json!(0));
    assert_eq!(payload["classes"], json!([]));
    assert_eq!(fs::read(&cache).expect("read cache after query"), before);
}

// ---------------------------------------------------------------------------
// Role routing: undergraduate-only commands refuse graduate accounts before
// any source is contacted. Unsupported roles are not guessed either.
// ---------------------------------------------------------------------------

#[test]
fn graduate_accounts_are_refused_by_undergraduate_jwgl_commands() {
    let sandbox = Sandbox::new();
    write_credentials(&sandbox, USER, "graduate");

    for command in ["scores", "classrooms"] {
        let payload = assert_usage(run(&sandbox, &[command]));
        let text = error_text(&payload);
        assert!(
            text.contains("本科教务"),
            "{command} must explain that it reads the undergraduate registrar: {text}"
        );
        assert!(
            fs::read_dir(sandbox.root().join("cache"))
                .expect("read cache dir")
                .next()
                .is_none(),
            "{command} contacted a source before refusing the role"
        );
    }
}

#[test]
fn role_guards_only_refuse_graduate_accounts_not_unknown_passwords() {
    let sandbox = Sandbox::new();
    // Missing password stops before any source is contacted, so this can test
    // role routing without a browser or campus network.
    for (command, role) in [("exams", "graduate"), ("scores", "undergraduate")] {
        fs::write(
            config_path(&sandbox),
            serde_json::to_vec_pretty(&json!({
                "username": USER,
                "role": role
            }))
            .expect("serialize incomplete synthetic credentials"),
        )
        .expect("write incomplete synthetic credentials");

        let invocation = run(&sandbox, &[command]);
        assert_eq!(
            invocation.code(),
            3,
            "{command}: expected auth exit for missing password; stdout={:?}; stderr={:?}",
            String::from_utf8_lossy(&invocation.0.stdout),
            String::from_utf8_lossy(&invocation.0.stderr)
        );
        assert!(invocation.0.stdout.is_empty());
        let payload = invocation.stderr_json();
        assert_schema(&payload);
        assert_eq!(payload["ok"], json!(false));
        assert_eq!(payload["error"]["type"], json!("auth"));
        let text = error_text(&payload);
        assert!(
            !text.contains("本科教务"),
            "{command}: the undergraduate-only guard fired for role={role}: {text}"
        );
        assert!(
            fs::read_dir(sandbox.root().join("cache"))
                .expect("read cache dir")
                .next()
                .is_none(),
            "{command}: source state was written before credential validation"
        );
    }
}

#[test]
fn unsupported_role_is_rejected_before_backend_selection() {
    let sandbox = Sandbox::new();
    write_credentials(&sandbox, USER, "visitor");

    for command in ["scores", "classrooms"] {
        let payload = assert_usage(run(&sandbox, &[command]));
        let text = error_text(&payload);
        assert!(
            text.contains("role") || text.contains("身份"),
            "{command} must reject an unsupported role explicitly: {text}"
        );
        assert!(
            fs::read_dir(sandbox.root().join("cache"))
                .expect("read cache dir")
                .next()
                .is_none(),
            "{command} touched the cache/session for an unsupported role"
        );
    }
}

#[test]
fn passwordless_profile_cannot_read_a_matching_snapshot() {
    let sandbox = Sandbox::new();
    let snapshot = synthetic_snapshot(USER, "graduate");
    fs::write(
        config_path(&sandbox),
        serde_json::to_vec_pretty(&json!({
            "username": USER,
            "role": "graduate"
        }))
        .expect("serialize passwordless synthetic profile"),
    )
    .expect("write passwordless synthetic profile");
    let (cache, before) = write_cache(&sandbox, &snapshot.entry);
    let out = sandbox.root().join("passwordless.ics");
    let date = snapshot.class_date.iso();

    for args in snapshot_read_commands(&date, &out) {
        let invocation = run_owned(&sandbox, &args);
        assert_eq!(
            invocation.code(),
            3,
            "passwordless profile must fail auth; stdout={:?}; stderr={:?}",
            String::from_utf8_lossy(&invocation.0.stdout),
            String::from_utf8_lossy(&invocation.0.stderr)
        );
        assert!(invocation.0.stdout.is_empty());
        let payload = invocation.stderr_json();
        assert_schema(&payload);
        assert_eq!(payload["ok"], json!(false));
        assert_eq!(payload["error"]["type"], json!("auth"));
        assert_eq!(
            fs::read(&cache).expect("read snapshot after auth refusal"),
            before
        );
        assert!(!cache.with_extension("json.tmp").exists());
    }
    assert!(
        !out.exists(),
        "passwordless profile must not create an export file"
    );
}

#[test]
fn environment_credentials_do_not_fill_a_passwordless_profile() {
    let sandbox = Sandbox::new();
    let snapshot = synthetic_snapshot(USER, "graduate");
    fs::write(
        config_path(&sandbox),
        serde_json::to_vec_pretty(&json!({
            "username": USER,
            "role": "graduate"
        }))
        .expect("serialize passwordless synthetic profile"),
    )
    .expect("write passwordless synthetic profile");
    let (cache, before) = write_cache(&sandbox, &snapshot.entry);
    let date = snapshot.class_date.iso();

    let invocation = Invocation(
        network_guarded_command(&sandbox)
            .env("BUPT_USERNAME", USER)
            .env("BUPT_PASSWORD", PASSWORD)
            .env("BUPT_ROLE", "graduate")
            .args(["schedule", "--date", &date])
            .output()
            .expect("run with explicit synthetic environment credentials"),
    );
    assert_eq!(
        invocation.code(),
        3,
        "environment passwords must not activate credentials.json; stdout={:?}; stderr={:?}",
        String::from_utf8_lossy(&invocation.0.stdout),
        String::from_utf8_lossy(&invocation.0.stderr)
    );
    assert!(invocation.0.stdout.is_empty());
    let payload = invocation.stderr_json();
    assert_schema(&payload);
    assert_eq!(payload["ok"], json!(false));
    assert_eq!(payload["error"]["type"], json!("auth"));
    assert_eq!(
        fs::read(&cache).expect("read cache after auth refusal"),
        before
    );
}

#[test]
fn environment_credentials_do_not_override_saved_account_or_role() {
    let sandbox = Sandbox::new();
    let snapshot = synthetic_snapshot(USER, "graduate");
    write_credentials(&sandbox, USER, "graduate");
    write_cache(&sandbox, &snapshot.entry);
    let date = snapshot.class_date.iso();

    let invocation = Invocation(
        network_guarded_command(&sandbox)
            .env("BUPT_USERNAME", OTHER_USER)
            .env("BUPT_PASSWORD", PASSWORD)
            .env("BUPT_ROLE", "undergraduate")
            .args(["schedule", "--date", &date])
            .output()
            .expect("run with conflicting synthetic environment credentials"),
    );
    let payload = assert_success(&invocation);
    assert_eq!(payload["role"], json!("graduate"));
    assert_eq!(payload["from_cache"], json!(true));
    assert_eq!(payload["count"], json!(1));
}

#[test]
fn date_arguments_require_ascii_yyyy_mm_dd_and_valid_year() {
    let sandbox = Sandbox::new();
    let cases = [
        "+026-01-01",
        "0000-01-01",
        "2026-1-01",
        "2026-01-1",
        "2026-01-+1",
        "２０２６-01-01",
        "2026-02-30",
        "2023-02-29",
    ];
    for date in cases {
        assert_usage(run(&sandbox, &["schedule", "--date", date]));
    }
    assert!(
        fs::read_dir(sandbox.root().join("cache"))
            .expect("read cache dir")
            .next()
            .is_none(),
        "invalid dates must be rejected before cache access"
    );
}

#[test]
fn valid_leap_day_is_accepted_by_the_local_query() {
    let sandbox = Sandbox::new();
    let snapshot = synthetic_snapshot(USER, "graduate");
    write_credentials(&sandbox, USER, "graduate");
    let (cache, before) = write_cache(&sandbox, &snapshot.entry);

    let payload = assert_success(&run(&sandbox, &["schedule", "--date", "2024-02-29"]));
    assert_eq!(payload["range"], json!(["2024-02-29", "2024-02-29"]));
    assert_eq!(payload["count"], json!(0));
    assert_eq!(payload["classes"], json!([]));
    assert_eq!(fs::read(&cache).expect("read unchanged snapshot"), before);
}

#[test]
fn export_omits_events_with_invalid_clock_times() {
    let sandbox = Sandbox::new();
    let snapshot = synthetic_snapshot(USER, "graduate");
    let mut entry = snapshot.entry.clone();
    entry["courses"] = json!([
        {
            "name": "有效合成课程",
            "weekday_index": 2,
            "weeks": [2],
            "period_start": 1,
            "period_end": 2,
            "time_start": "08:00",
            "time_end": "09:35",
            "teachers": [],
            "rooms": [],
            "campus": ""
        },
        {
            "name": "坏开始时钟课程",
            "weekday_index": 2,
            "weeks": [2],
            "period_start": 1,
            "period_end": 1,
            "time_start": "25:00",
            "time_end": "09:35",
            "teachers": [],
            "rooms": [],
            "campus": ""
        },
        {
            "name": "坏结束时钟课程",
            "weekday_index": 2,
            "weeks": [2],
            "period_start": 1,
            "period_end": 1,
            "time_start": "08:00",
            "time_end": "09:60",
            "teachers": [],
            "rooms": [],
            "campus": ""
        }
    ]);
    write_credentials(&sandbox, USER, "graduate");
    write_cache(&sandbox, &entry);
    let out = sandbox.root().join("clocks.ics");

    let payload = assert_success(&run(
        &sandbox,
        &[
            "schedule",
            "export",
            "--out",
            out.to_str().unwrap(),
            "--no-open-folder",
        ],
    ));
    assert_eq!(payload["events"], json!(1));
    let ics = fs::read_to_string(&out).expect("read clock-filtered ICS");
    assert!(ics.contains("有效合成课程"));
    assert!(!ics.contains("坏开始时钟课程"));
    assert!(!ics.contains("坏结束时钟课程"));
    assert!(!ics.contains("T250000"));
    assert!(!ics.contains("T096000"));
}

#[test]
fn export_counts_real_events_and_escapes_injected_line_breaks() {
    let sandbox = Sandbox::new();
    let snapshot = synthetic_snapshot(USER, "graduate");
    let mut entry = snapshot.entry.clone();
    entry["courses"] = json!([{
        "name": "BEGIN:VEVENT\r\n伪造课程\nEND:VEVENT",
        "weekday_index": 2,
        "weeks": [2],
        "period_start": 1,
        "period_end": 2,
        "time_start": "08:00",
        "time_end": "09:35",
        "teachers": [],
        "rooms": ["BEGIN:VEVENT"],
        "campus": ""
    }]);
    write_credentials(&sandbox, USER, "graduate");
    write_cache(&sandbox, &entry);
    let out = sandbox.root().join("literals.ics");

    let payload = assert_success(&run(
        &sandbox,
        &[
            "schedule",
            "export",
            "--out",
            out.to_str().unwrap(),
            "--no-open-folder",
        ],
    ));
    assert_eq!(payload["events"], json!(1));
    let ics = fs::read_to_string(&out).expect("read escaped ICS");
    assert_eq!(
        ics.split("\r\n")
            .filter(|line| *line == "BEGIN:VEVENT")
            .count(),
        1,
        "literal BEGIN:VEVENT text must not create another event line: {ics:?}"
    );
    assert_eq!(
        ics.split("\r\n")
            .filter(|line| *line == "END:VEVENT")
            .count(),
        1,
        "literal END:VEVENT text must not close the event early: {ics:?}"
    );
    let unfolded = ics.replace("\r\n ", "");
    assert!(unfolded.contains("SUMMARY:BEGIN:VEVENT\\n伪造课程\\nEND:VEVENT"));
    assert!(unfolded.contains("LOCATION:BEGIN:VEVENT"));
    assert!(!ics.lines().any(|line| line == "伪造课程"));
}
