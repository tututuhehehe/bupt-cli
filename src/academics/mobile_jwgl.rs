//! Undergraduate mobile JWGL JSON client and response mapping.
//!
//! The client is deliberately a scoped read transport: every request is one
//! HTTP exchange through the injected `Transport`, authenticates with a cached
//! mobile-JWGL token, and never submits a password. Token acquisition and
//! refresh belong to the parent identity lane through [`MobileJwglAuth`].
//!
//! Dates in a curriculum response are taken only from `SjdDay.mxrq`. No week
//! arithmetic is used to fill missing dates; a partial grid is rejected by the
//! timetable cache validator before it can replace an existing snapshot.

use super::{Snapshot, TimetableSource};
use crate::cli::{CliError, CliResult};
use crate::transport::{Request, Transport};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

pub(in crate::academics) const MOBILE_JWGL_BASE: &str = "https://jwglweixin.bupt.edu.cn";
const CURRICULUM_PATH: &str = "/bjyddx/student/curriculum";
const EXAM_WEEKS_PATH: &str = "/bjyddx/kaowu/get_kszc";
const EXAM_SCHEDULE_PATH: &str = "/bjyddx/kaowu/getKsapcx";
const EXAM_COURSE_SCHEDULE_PATH: &str = "/bjyddx/kaowu/findKcksapcx";
const TERM_GPA_PATH: &str = "/bjyddx/student/termGPA";
const TODAY_CLASSROOMS_PATH: &str = "/bjyddx/todayClassrooms";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
    AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const ACCEPT: &str = "application/json, text/plain, */*";
const TOKEN_EXPIRY_MARKERS: [&str; 5] = ["失效", "请登录", "重新登录", "过期", "token"];

/// A cached mobile-JWGL token. `expires_at` is epoch seconds, matching the
/// identity session's `mobile_jwgl.expires_at` value when present.
pub(crate) struct MobileJwglToken {
    token: String,
    expires_at: Option<f64>,
}

impl MobileJwglToken {
    pub(crate) fn new(token: impl Into<String>, expires_at: Option<f64>) -> Self {
        Self {
            token: token.into(),
            expires_at,
        }
    }

    fn is_expired(&self, now: f64) -> bool {
        self.expires_at.is_some_and(|expires_at| expires_at <= now)
    }
}

impl fmt::Debug for MobileJwglToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MobileJwglToken")
            .field("token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Narrow seam owned by identity: return an already-bound cached token, and
/// optionally refresh it when the client asks for a retry.
///
/// The production identity lane should implement this from the same
/// `load_owned_cas_session`/`OwnedCasSession::mobile_token` state, so
/// `session.json` is never parsed a second time here. `invalidate` may refresh
/// the token; its default is a no-op because token acquisition is not owned by
/// academics.
pub(crate) trait MobileJwglAuth {
    fn token(&mut self) -> CliResult<MobileJwglToken>;

    fn invalidate(&mut self) {}

    fn can_retry(&self) -> bool {
        true
    }
}

/// One already validated or lazily acquired token for the command. Do not
/// submit a password again, or retry blindly with the same rejected token.
pub(super) struct NativeMobileJwglAuth(String);

impl NativeMobileJwglAuth {
    pub(super) fn new(token: String) -> Self {
        Self(token)
    }
}

impl MobileJwglAuth for NativeMobileJwglAuth {
    fn token(&mut self) -> CliResult<MobileJwglToken> {
        Ok(MobileJwglToken::new(self.0.clone(), None))
    }

    fn can_retry(&self) -> bool {
        false
    }
}

pub(in crate::academics) struct MobileJwglClient<'a> {
    transport: &'a mut dyn Transport,
    auth: &'a mut dyn MobileJwglAuth,
    token: Option<String>,
}

impl<'a> MobileJwglClient<'a> {
    pub(in crate::academics) fn new(
        transport: &'a mut dyn Transport,
        auth: &'a mut dyn MobileJwglAuth,
    ) -> Self {
        Self {
            transport,
            auth,
            token: None,
        }
    }

    pub(in crate::academics) fn curriculum(&mut self) -> CliResult<Vec<CurriculumWeek>> {
        let data = self.request("GET", CURRICULUM_PATH, None)?;
        let weeks = parse_curriculum(&data);
        if weeks.is_empty() {
            return Err(curriculum_error("移动教务没有返回课表数据", ""));
        }
        Ok(weeks)
    }

    pub(in crate::academics) fn exam_weeks(&mut self) -> CliResult<Value> {
        self.request("GET", EXAM_WEEKS_PATH, None)
    }

    pub(in crate::academics) fn exam_schedule(&mut self) -> CliResult<Value> {
        self.request("GET", EXAM_SCHEDULE_PATH, None)
    }

    pub(in crate::academics) fn exam_course_schedule(&mut self) -> CliResult<Value> {
        self.request("GET", EXAM_COURSE_SCHEDULE_PATH, None)
    }

    pub(in crate::academics) fn score_report(&mut self) -> CliResult<Value> {
        self.request("POST", TERM_GPA_PATH, None)
    }

    pub(in crate::academics) fn today_classrooms(&mut self, campus: &str) -> CliResult<Value> {
        if !matches!(campus, "01" | "04") {
            return Err(CliError::new(
                2,
                "usage",
                "--campus must be 01 (西土城) or 04 (沙河)",
                "",
            ));
        }
        self.request(
            "GET",
            TODAY_CLASSROOMS_PATH,
            Some(&format!("campusId={campus}")),
        )
    }

    fn request(
        &mut self,
        method: &'static str,
        path: &'static str,
        query: Option<&str>,
    ) -> CliResult<Value> {
        let url = match query {
            Some(query) => format!("{MOBILE_JWGL_BASE}{path}?{query}"),
            None => format!("{MOBILE_JWGL_BASE}{path}"),
        };
        let mut retried = false;
        loop {
            let token = self.ensure_token()?;
            let request = Request {
                method,
                url: url.clone(),
                headers: vec![
                    ("User-Agent".to_owned(), USER_AGENT.to_owned()),
                    ("Accept".to_owned(), ACCEPT.to_owned()),
                    ("token".to_owned(), token),
                ],
                body: Vec::new(),
            };
            let response = self.transport.send(request)?;
            match interpret_response(response, path)? {
                Reply::Data(data) => return Ok(data),
                Reply::Empty if !retried && self.auth.can_retry() => {
                    self.retry_after_auth_failure(&mut retried)?
                }
                Reply::Empty => return Ok(Value::Null),
                Reply::Html if !retried && self.auth.can_retry() => {
                    self.retry_after_auth_failure(&mut retried)?
                }
                Reply::Html => return Err(parse_error(path, "返回了 HTML 而不是 JSON")),
                Reply::Retry if !retried && self.auth.can_retry() => {
                    self.retry_after_auth_failure(&mut retried)?
                }
                Reply::Retry => {
                    return Err(auth_error(
                        "移动教务拒绝了当前会话",
                        "请重新执行 `bupt login --role undergraduate`，本次不会再次提交密码。",
                    ));
                }
            }
        }
    }

    fn retry_after_auth_failure(&mut self, retried: &mut bool) -> CliResult<()> {
        *retried = true;
        self.token = None;
        self.auth.invalidate();
        Ok(())
    }

    fn ensure_token(&mut self) -> CliResult<String> {
        if let Some(token) = &self.token {
            return Ok(token.clone());
        }
        let now = unix_now();
        let mut token = self.auth.token()?;
        if token.is_expired(now) {
            self.auth.invalidate();
            token = self.auth.token()?;
            if token.is_expired(now) {
                return Err(auth_error(
                    "缓存的本科 mobile JWGL token 已过期",
                    "由 identity 刷新本科教务 token；必要时重新执行 `bupt login --role undergraduate`。",
                ));
            }
        }
        if token.token.is_empty() {
            return Err(auth_error(
                "identity 返回了空的本科 mobile JWGL token",
                "重新执行 `bupt login --role undergraduate` 建立本科会话；mobile token 由 identity 接入。",
            ));
        }
        let value = token.token.clone();
        self.token = Some(value.clone());
        Ok(value)
    }
}

/// One week of the mobile curriculum. `fetch_undergraduate` only consumes the
/// flat `courses` array; the SDK also carries a nested `item` grid for other
/// callers, which the timetable cache intentionally does not duplicate.
pub(in crate::academics) struct CurriculumWeek {
    pub(in crate::academics) date: Vec<SjdDay>,
    pub(in crate::academics) courses: Vec<SjdCourse>,
    pub(in crate::academics) semester_id: String,
    pub(in crate::academics) week: String,
}

pub(in crate::academics) struct SjdDay {
    pub(in crate::academics) mxrq: String,
    pub(in crate::academics) zc: String,
    pub(in crate::academics) xqid: String,
}

#[derive(Clone)]
pub(in crate::academics) struct SjdCourse {
    class_week: String,
    class_week_details: String,
    week_day: String,
    course_name: String,
    teacher_name: String,
    building_name: String,
    classroom_name: String,
    start_time: String,
    end_time: String,
}

impl SjdCourse {
    fn week_numbers(&self) -> Vec<i64> {
        parse_course_week_numbers(&self.class_week, &self.class_week_details)
    }
}

#[derive(Debug)]
pub(in crate::academics) struct MobileTimetable {
    pub(in crate::academics) term: String,
    pub(in crate::academics) courses: Vec<Value>,
    pub(in crate::academics) calendar: BTreeMap<i64, Vec<String>>,
}

pub(in crate::academics) struct MobileTimetableSource<'a> {
    transport: &'a mut dyn Transport,
    auth: &'a mut dyn MobileJwglAuth,
}

impl<'a> MobileTimetableSource<'a> {
    pub(in crate::academics) fn new(
        transport: &'a mut dyn Transport,
        auth: &'a mut dyn MobileJwglAuth,
    ) -> Self {
        Self { transport, auth }
    }
}

impl TimetableSource for MobileTimetableSource<'_> {
    fn fetch(&mut self, username: &str, role: super::Role) -> CliResult<Snapshot> {
        if role != super::Role::Undergraduate {
            return Err(CliError::new(
                2,
                "usage",
                "本科 mobile JWGL 数据源只用于本科生账号",
                "",
            ));
        }
        let mut client = MobileJwglClient::new(self.transport, self.auth);
        let weeks = client.curriculum()?;
        let timetable = timetable_from_curriculum(&weeks)?;
        Ok(Snapshot {
            username: username.to_owned(),
            role: "undergraduate".to_owned(),
            term: timetable.term,
            source: "mobile_jwgl".to_owned(),
            courses: timetable.courses,
            calendar: timetable.calendar,
        })
    }
}

pub(in crate::academics) fn exams_payload(
    weeks: &Value,
    exams: &Value,
    course_arrangements: &Value,
) -> Value {
    let weeks = raw_list(weeks)
        .into_iter()
        .map(|entry| {
            json!({
                "key": text(entry.get("key")),
                "value": text(entry.get("value")),
            })
        })
        .collect::<Vec<_>>();
    json!({
        "weeks": weeks,
        "exams": raw_list(exams),
        "course_arrangements": raw_list(course_arrangements),
        "note": "mobile JWGL exam endpoints return empty data between semesters",
    })
}

pub(in crate::academics) fn scores_payload(
    report: &Value,
    semester: Option<&str>,
) -> CliResult<Value> {
    let report = score_report(report)?;
    let semester = semester.filter(|wanted| !wanted.is_empty());
    let courses = report
        .courses
        .iter()
        .filter(|course| semester.is_none_or(|wanted| course.semester_name == wanted))
        .collect::<Vec<_>>();
    let mut semesters: Map<String, Value> = Map::new();
    for course in &courses {
        let key = if course.semester_name.is_empty() {
            "unknown"
        } else {
            &course.semester_name
        };
        let entry = semesters
            .entry(key.to_owned())
            .or_insert_with(|| Value::Array(Vec::new()));
        entry
            .as_array_mut()
            .expect("semester bucket is an array")
            .push(course.as_value());
    }
    Ok(json!({
        "student_id": report.student_id,
        "average_score": report.average_score,
        "semesters": semesters,
        "gpa_entries": report.term_gpas,
        "count": courses.len(),
    }))
}

pub(in crate::academics) fn classrooms_payload(campus: &str, data: &Value) -> Value {
    let periods = raw_list(data)
        .into_iter()
        .map(|entry| {
            let raw = first_truthy_text(&entry, &["CLASSROOMS", "classrooms"]);
            json!({
                "raw": raw,
                "rooms": parse_classrooms(&raw)
                    .into_iter()
                    .map(|(name, capacity)| json!({"name": name, "capacity": capacity}))
                    .collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    json!({
        "campus": campus,
        "count": periods.len(),
        "periods": periods,
    })
}

pub(in crate::academics) fn parse_curriculum(data: &Value) -> Vec<CurriculumWeek> {
    let Some(items) = data.as_array() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let object = item.as_object()?;
            Some(CurriculumWeek {
                date: object
                    .get("date")
                    .and_then(Value::as_array)
                    .map(|days| days.iter().filter_map(SjdDay::from_value).collect())
                    .unwrap_or_default(),
                courses: object
                    .get("courses")
                    .and_then(Value::as_array)
                    .map(|courses| courses.iter().filter_map(SjdCourse::from_value).collect())
                    .unwrap_or_default(),
                semester_id: first_text(item, &["semesterId", "xnxq01id"]),
                week: text(object.get("week")),
            })
        })
        .collect()
}

pub(in crate::academics) fn timetable_from_curriculum(
    weeks: &[CurriculumWeek],
) -> CliResult<MobileTimetable> {
    let mut calendar: BTreeMap<i64, Vec<String>> = BTreeMap::new();
    let mut term = String::new();
    for week in weeks {
        if term.is_empty() {
            term = week.semester_id.clone();
        }
        for day in &week.date {
            if day.zc.is_empty() || day.mxrq.is_empty() || day.xqid.is_empty() {
                continue;
            }
            let Ok(week_number) = day.zc.parse::<i64>() else {
                continue;
            };
            let Ok(weekday) = day.xqid.parse::<i64>() else {
                continue;
            };
            let weekday = weekday - 1;
            if !(0..7).contains(&weekday) {
                continue;
            }
            calendar
                .entry(week_number)
                .or_insert_with(|| vec![String::new(); 7])[weekday as usize] = day.mxrq.clone();
        }
    }
    if calendar.is_empty() {
        return Err(curriculum_error("移动教务的课表里没有可用的日期信息", ""));
    }

    let mut courses = Vec::new();
    let mut seen = HashSet::new();
    for week in weeks {
        for course in &week.courses {
            let Ok(weekday) = course.week_day.parse::<i64>() else {
                continue;
            };
            let weekday = weekday - 1;
            if !(0..7).contains(&weekday) {
                continue;
            }
            let mut course_weeks = course.week_numbers();
            if course_weeks.is_empty() {
                let Ok(week_number) = week.week.parse::<i64>() else {
                    continue;
                };
                course_weeks = vec![week_number];
            }
            let mut rooms = Vec::new();
            if !course.building_name.is_empty() {
                rooms.push(course.building_name.clone());
            }
            if !course.classroom_name.is_empty() {
                rooms.push(course.classroom_name.clone());
            }
            let key = (
                course.course_name.clone(),
                weekday,
                course_weeks.clone(),
                course.start_time.clone(),
                rooms.clone(),
            );
            if !seen.insert(key) {
                continue;
            }
            courses.push(json!({
                "name": course.course_name.clone(),
                "weekday_index": weekday,
                "weeks": course_weeks,
                "period_start": Value::Null,
                "period_end": Value::Null,
                "time_start": course.start_time.clone(),
                "time_end": course.end_time.clone(),
                "teachers": if course.teacher_name.is_empty() {
                    Vec::<String>::new()
                } else {
                    vec![course.teacher_name.clone()]
                },
                "rooms": rooms,
                "campus": "",
            }));
        }
    }
    if courses.is_empty() {
        return Err(upstream_error(
            "curriculum",
            "移动教务的课表数据里没有可解析的课程",
        ));
    }
    Ok(MobileTimetable {
        term: if term.is_empty() {
            "本学期".to_owned()
        } else {
            term
        },
        courses,
        calendar,
    })
}

impl SjdDay {
    fn from_value(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        Some(Self {
            mxrq: text(object.get("mxrq")),
            zc: text(object.get("zc")),
            xqid: text(object.get("xqid")),
        })
    }
}

impl SjdCourse {
    fn from_value(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        Some(Self {
            class_week: text(object.get("classWeek")),
            class_week_details: text(object.get("classWeekDetails")),
            week_day: text(object.get("weekDay")),
            course_name: text(object.get("courseName")),
            teacher_name: text(object.get("teacherName")),
            building_name: text(object.get("buildingName")),
            classroom_name: text(object.get("classroomName")),
            start_time: text(object.get("startTime")),
            end_time: first_text(value, &["endTIme", "endTime"]),
        })
    }
}

fn parse_course_week_numbers(class_week: &str, class_week_details: &str) -> Vec<i64> {
    let mut weeks = expand_week_numbers(class_week);
    if weeks.is_empty() {
        weeks = expand_week_numbers(class_week_details);
    }
    if weeks.is_empty() {
        weeks = digit_runs(class_week_details);
    }
    weeks.sort_unstable();
    weeks.dedup();
    weeks
}

fn expand_week_numbers(text: &str) -> Vec<i64> {
    let mut raw = text.replace('，', ",").replace([' ', '周'], "");
    let global_odd = raw.contains('单');
    let global_even = raw.contains('双');
    raw = remove_bracket_segments(&raw);
    let mut weeks = Vec::new();
    for item in raw.split(',') {
        if item.is_empty() {
            continue;
        }
        let item_odd = item.contains('单');
        let item_even = item.contains('双');
        let clean = item.replace(['单', '双'], "");
        let mut expanded = Vec::new();
        if let Some((left, right)) = clean.split_once('-') {
            let (Ok(left), Ok(right)) = (left.parse::<i64>(), right.parse::<i64>()) else {
                continue;
            };
            if left <= right {
                expanded.extend(left..=right);
            }
        } else if let Ok(week) = clean.parse::<i64>() {
            expanded.push(week);
        }
        if item_odd || (!item_even && global_odd) {
            expanded.retain(|week| week % 2 == 1);
        } else if item_even || (!item_odd && global_even) {
            expanded.retain(|week| week % 2 == 0);
        }
        weeks.extend(expanded);
    }
    weeks.sort_unstable();
    weeks.dedup();
    weeks
}

fn remove_bracket_segments(text: &str) -> String {
    let mut output = String::new();
    let mut skip = None;
    for character in text.chars() {
        match (skip, character) {
            (None, '[') => skip = Some(']'),
            (None, '(') => skip = Some(')'),
            (Some(close), character) if character == close => skip = None,
            (Some(_), _) => {}
            (None, character) => output.push(character),
        }
    }
    output
}

fn digit_runs(text: &str) -> Vec<i64> {
    text.split(|character: char| !character.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<i64>().ok())
        .collect()
}

fn score_report(value: &Value) -> CliResult<ScoreReport> {
    let Some(items) = value.as_array() else {
        return Err(parse_error("termGPA", "返回的数据不是成绩报告列表"));
    };
    let Some(report) = items.iter().find_map(Value::as_object) else {
        return Err(parse_error("termGPA", "没有可用的成绩报告"));
    };
    Ok(ScoreReport {
        student_id: text(report.get("studentID")),
        average_score: optional_float(report.get("pjcj")),
        term_gpas: report
            .get("xqgpa")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter(|entry| entry.is_object())
                    .cloned()
                    .collect()
            })
            .unwrap_or_default(),
        courses: report
            .get("achievement")
            .and_then(Value::as_array)
            .map(|courses| courses.iter().filter_map(CourseScore::from_value).collect())
            .unwrap_or_default(),
    })
}

struct ScoreReport {
    student_id: String,
    average_score: Option<f64>,
    term_gpas: Vec<Value>,
    courses: Vec<CourseScore>,
}

struct CourseScore {
    course_name: String,
    course_code: String,
    credit: Option<f64>,
    curriculum_attributes: String,
    course_nature: String,
    examination_nature: String,
    fraction: String,
    score_flag: String,
    semester_name: String,
    score_id: String,
}

impl CourseScore {
    fn from_value(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        Some(Self {
            course_name: text(object.get("courseName")),
            course_code: text(object.get("kcbh")),
            credit: optional_float(object.get("credit")),
            curriculum_attributes: text(object.get("curriculumAttributes")),
            course_nature: text(object.get("courseNature")),
            examination_nature: text(object.get("examinationNature")),
            fraction: text(object.get("fraction")),
            score_flag: text(object.get("cjbs")),
            semester_name: text(object.get("curSemesterName")),
            score_id: text(object.get("cj0708id")),
        })
    }

    fn as_value(&self) -> Value {
        json!({
            "name": self.course_name,
            "code": self.course_code,
            "credit": self.credit,
            "score": self.fraction,
            "attributes": self.curriculum_attributes,
            "nature": self.course_nature,
            "exam_nature": self.examination_nature,
            "score_flag": self.score_flag,
            "id": self.score_id,
        })
    }
}

fn parse_classrooms(raw: &str) -> Vec<(String, Option<i64>)> {
    let mut rooms = Vec::new();
    for room in raw.split(',') {
        let room = room.trim();
        if room.is_empty() {
            continue;
        }
        if let Some((name, tail)) = room.rsplit_once('(')
            && let Some(capacity) = tail.strip_suffix(')')
        {
            let capacity = capacity.trim();
            rooms.push((
                name.trim().to_owned(),
                capacity
                    .parse::<i64>()
                    .ok()
                    .filter(|_| capacity.bytes().all(|byte| byte.is_ascii_digit())),
            ));
        } else {
            rooms.push((room.to_owned(), None));
        }
    }
    rooms
}

enum Reply {
    Data(Value),
    Empty,
    Html,
    Retry,
}

fn interpret_response(response: crate::transport::Response, path: &str) -> CliResult<Reply> {
    if matches!(response.status, 401 | 403) {
        return Ok(Reply::Retry);
    }
    if !(200..300).contains(&response.status) {
        return Err(upstream_error(
            path,
            &format!("返回 HTTP {}", response.status),
        ));
    }
    let text = String::from_utf8_lossy(&response.body);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(Reply::Empty);
    }
    if trimmed.starts_with('<') {
        return Ok(Reply::Html);
    }
    let payload: Value =
        serde_json::from_str(trimmed).map_err(|_| parse_error(path, "返回了无法解析的 JSON"))?;
    let Some(object) = payload.as_object() else {
        return Err(parse_error(path, "JSON 外壳不是对象"));
    };
    let success = match object.get("code") {
        Some(Value::String(code)) => code == "1",
        Some(Value::Number(code)) => code.as_f64() == Some(1.0),
        _ => false,
    };
    if success {
        return Ok(Reply::Data(
            object.get("data").cloned().unwrap_or(Value::Null),
        ));
    }
    let message = first_text(&payload, &["Msg", "msg"]);
    if message.starts_with("未授权访问") {
        return Err(upstream_error(path, "该接口不在学生 token 权限范围内"));
    }
    if TOKEN_EXPIRY_MARKERS
        .iter()
        .any(|marker| message.contains(marker))
    {
        return Ok(Reply::Retry);
    }
    Err(upstream_error(path, "mobile JWGL 返回失败外壳"))
}

fn raw_list(data: &Value) -> Vec<Value> {
    match data {
        Value::Array(items) => items
            .iter()
            .filter(|item| item.is_object())
            .cloned()
            .collect(),
        Value::Object(_) => vec![data.clone()],
        _ => Vec::new(),
    }
}

fn first_truthy_text(value: &Value, keys: &[&str]) -> String {
    let Some(object) = value.as_object() else {
        return String::new();
    };
    for key in keys {
        let Some(value) = object.get(*key) else {
            continue;
        };
        if is_truthy(value) {
            return text(Some(value));
        }
    }
    String::new()
}

fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

fn first_text(value: &Value, keys: &[&str]) -> String {
    let Some(object) = value.as_object() else {
        return String::new();
    };
    for key in keys {
        if let Some(value) = object.get(*key) {
            let text = text(Some(value));
            if !text.is_empty() {
                return text;
            }
        }
    }
    String::new()
}

fn text(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(value)) => value.trim().to_owned(),
        Some(Value::Bool(value)) => {
            if *value {
                "True".to_owned()
            } else {
                "False".to_owned()
            }
        }
        Some(value) => value.to_string(),
    }
}

fn optional_float(value: Option<&Value>) -> Option<f64> {
    let value = match value? {
        Value::Number(value) => value.as_f64(),
        Value::String(value) => value.trim().parse::<f64>().ok(),
        _ => None,
    }?;
    value.is_finite().then_some(value)
}

fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or_default()
}

fn auth_error(message: impl Into<String>, hint: impl Into<String>) -> CliError {
    CliError::new(3, "auth", message, hint)
}

fn curriculum_error(message: &str, hint: &str) -> CliError {
    CliError::new(
        5,
        "upstream",
        message,
        if hint.is_empty() {
            "该后端可能只返回当前周；不会用周次推导日期，也不会用局部日期覆盖整学期快照。"
        } else {
            hint
        },
    )
}

fn parse_error(path: &str, message: &str) -> CliError {
    upstream_error(path, message)
}

fn upstream_error(path: &str, message: &str) -> CliError {
    CliError::new(
        5,
        "upstream",
        format!("本科 mobile JWGL {path}：{message}"),
        "读取保持只读；由 identity 刷新本科教务 token，不会提交 JWGL 密码。",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::Response;
    use crate::transport::testing::Scripted;
    use std::collections::VecDeque;

    const TOKEN: &str = "TEST-ONLY-MOBILE-JWGL-TOKEN";
    const CURRICULUM: &str =
        include_str!("../../tests/fixtures/contracts/academics/mobile_jwgl_curriculum.json");
    const PARTIAL: &str = include_str!(
        "../../tests/fixtures/contracts/academics/mobile_jwgl_curriculum_partial.json"
    );
    const EXAMS_WEEKS: &str =
        include_str!("../../tests/fixtures/contracts/academics/mobile_jwgl_exam_weeks.json");
    const EXAMS_SCHEDULE: &str =
        include_str!("../../tests/fixtures/contracts/academics/mobile_jwgl_exam_schedule.json");
    const EXAMS_COURSES: &str = include_str!(
        "../../tests/fixtures/contracts/academics/mobile_jwgl_exam_course_schedule.json"
    );
    const SCORES: &str =
        include_str!("../../tests/fixtures/contracts/academics/mobile_jwgl_scores.json");
    const CLASSROOMS: &str =
        include_str!("../../tests/fixtures/contracts/academics/mobile_jwgl_classrooms.json");

    struct FakeAuth {
        tokens: VecDeque<CliResult<MobileJwglToken>>,
        calls: usize,
        invalidations: usize,
    }

    impl FakeAuth {
        fn valid() -> Self {
            Self {
                tokens: VecDeque::from([Ok(MobileJwglToken::new(TOKEN, Some(4102444800.0)))]),
                calls: 0,
                invalidations: 0,
            }
        }

        fn with(tokens: Vec<CliResult<MobileJwglToken>>) -> Self {
            Self {
                tokens: tokens.into(),
                calls: 0,
                invalidations: 0,
            }
        }
    }

    impl MobileJwglAuth for FakeAuth {
        fn token(&mut self) -> CliResult<MobileJwglToken> {
            self.calls += 1;
            self.tokens
                .pop_front()
                .expect("mobile JWGL auth was called more times than the test allowed")
        }

        fn invalidate(&mut self) {
            self.invalidations += 1;
        }
    }

    fn response(body: &str) -> Response {
        Response {
            status: 200,
            headers: Vec::new(),
            body: body.as_bytes().to_vec(),
        }
    }

    fn queue(scripted: &mut Scripted, method: &'static str, url: &str, body: &str) {
        scripted
            .expected
            .push_back((method, url.to_owned(), Ok(response(body))));
    }

    fn client<'a>(transport: &'a mut Scripted, auth: &'a mut FakeAuth) -> MobileJwglClient<'a> {
        MobileJwglClient::new(transport, auth)
    }

    #[test]
    fn curriculum_fixture_uses_sjd_dates_and_preserves_python_nulls() {
        let data: Value = serde_json::from_str(CURRICULUM).expect("fixture JSON");
        let weeks = parse_curriculum(data.get("data").expect("fixture data"));
        let timetable = timetable_from_curriculum(&weeks).expect("curriculum mapping");

        assert_eq!(timetable.term, "2026-2027-1");
        assert_eq!(timetable.calendar.len(), 2);
        assert_eq!(timetable.calendar[&1][0], "2026-08-31");
        assert_eq!(timetable.calendar[&1][2], "2026-09-02");
        assert_eq!(timetable.calendar[&2][6], "2026-09-13");
        assert_eq!(
            timetable.courses.len(),
            2,
            "duplicate weekly rows are removed"
        );

        let first = &timetable.courses[0];
        assert_eq!(first["name"], json!("数字信号处理"));
        assert_eq!(first["weekday_index"], json!(2));
        assert_eq!(first["weeks"], json!([1, 2]));
        assert_eq!(first["period_start"], Value::Null);
        assert_eq!(first["period_end"], Value::Null);
        assert_eq!(first["time_start"], json!("08:00"));
        assert_eq!(first["time_end"], json!("09:35"));
        assert_eq!(first["teachers"], json!(["张老师"]));
        assert_eq!(first["rooms"], json!(["教三", "101"]));
        assert_eq!(first["campus"], json!(""));

        let second = &timetable.courses[1];
        assert_eq!(second["weeks"], json!([1, 2]));
        assert_eq!(second["teachers"], json!([]));
        assert_eq!(second["rooms"], json!(["教一", "教一101"]));
    }

    #[test]
    fn curriculum_without_dates_is_an_upstream_error() {
        let data = json!([{"semesterId":"2026-2027-1","week":"1","date":[],"courses":[]}]);
        let weeks = parse_curriculum(&data);
        let error = timetable_from_curriculum(&weeks).expect_err("dates are required");
        assert_eq!(error.code, 5);
        assert!(error.message.contains("日期"));
    }

    #[test]
    fn week_expansion_matches_python_week_specs() {
        assert_eq!(
            parse_course_week_numbers("1-17单,2-18双", ""),
            (1..=18).collect::<Vec<_>>()
        );
        assert_eq!(parse_course_week_numbers("", ",3,4,5,"), vec![3, 4, 5]);
        assert_eq!(parse_course_week_numbers("", "第1-3周"), vec![1, 3]);
        assert_eq!(parse_course_week_numbers("1-3,5", ""), vec![1, 2, 3, 5]);
        assert_eq!(parse_course_week_numbers("1-3[实验]", ""), vec![1, 2, 3]);
        assert_eq!(
            parse_course_week_numbers("5-3", ""),
            Vec::<i64>::new(),
            "Python does not reverse an invalid range"
        );
    }

    #[test]
    fn semester_and_end_time_fallbacks_match_the_sdk() {
        let data = json!([{
            "semesterId": "",
            "xnxq01id": "2026-2027-2",
            "week": "1",
            "date": [
                {"zc":"1","mxrq":"2026-08-31","xqid":"1"},
                {"zc":"1","mxrq":"2026-09-01","xqid":"2"},
                {"zc":"1","mxrq":"2026-09-02","xqid":"3"},
                {"zc":"1","mxrq":"2026-09-03","xqid":"4"},
                {"zc":"1","mxrq":"2026-09-04","xqid":"5"},
                {"zc":"1","mxrq":"2026-09-05","xqid":"6"},
                {"zc":"1","mxrq":"2026-09-06","xqid":"7"}
            ],
            "courses": [{
                "classWeek":"1",
                "weekDay":"1",
                "courseName":"合成课程",
                "startTime":"08:00",
                "endTIme":"",
                "endTime":"09:35"
            }]
        }]);
        let timetable = timetable_from_curriculum(&parse_curriculum(&data)).expect("fallback map");
        assert_eq!(timetable.term, "2026-2027-2");
        assert_eq!(timetable.courses[0]["time_end"], json!("09:35"));
    }

    #[test]
    fn empty_curriculum_is_an_upstream_error() {
        let mut transport = Scripted::default();
        queue(
            &mut transport,
            "GET",
            "https://jwglweixin.bupt.edu.cn/bjyddx/student/curriculum",
            r#"{"code":"1","data":[]}"#,
        );
        let mut auth = FakeAuth::valid();
        let error = {
            let mut client = client(&mut transport, &mut auth);
            match client.curriculum() {
                Ok(_) => panic!("empty curriculum"),
                Err(error) => error,
            }
        };
        assert_eq!(error.code, 5);
        assert!(error.message.contains("没有返回课表数据"));
    }

    #[test]
    fn token_debug_redacts_the_value() {
        let token = MobileJwglToken::new("TEST-ONLY-SECRET", Some(4_102_444_800.0));
        let debug = format!("{token:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("TEST-ONLY-SECRET"));
    }

    #[test]
    fn curriculum_uses_only_sjd_dates_and_never_invents_missing_days() {
        let data: Value = serde_json::from_str(PARTIAL).expect("fixture JSON");
        let weeks = parse_curriculum(data.get("data").expect("fixture data"));
        let timetable = timetable_from_curriculum(&weeks).expect("current week mapping");
        assert_eq!(timetable.calendar.len(), 1);
        assert_eq!(timetable.calendar[&3][0], "2026-09-14");
        assert_eq!(timetable.calendar[&3][6], "2026-09-20");
        assert_eq!(timetable.courses[0]["period_start"], Value::Null);
        assert_eq!(timetable.courses[0]["period_end"], Value::Null);
    }

    #[test]
    fn exam_client_is_lazy_and_preserves_raw_rows() {
        let mut transport = Scripted::default();
        queue(
            &mut transport,
            "GET",
            "https://jwglweixin.bupt.edu.cn/bjyddx/kaowu/get_kszc",
            EXAMS_WEEKS,
        );
        queue(
            &mut transport,
            "GET",
            "https://jwglweixin.bupt.edu.cn/bjyddx/kaowu/getKsapcx",
            EXAMS_SCHEDULE,
        );
        queue(
            &mut transport,
            "GET",
            "https://jwglweixin.bupt.edu.cn/bjyddx/kaowu/findKcksapcx",
            EXAMS_COURSES,
        );
        let mut auth = FakeAuth::valid();
        let (weeks, exams, arrangements) = {
            let mut client = client(&mut transport, &mut auth);
            (
                client.exam_weeks().unwrap(),
                client.exam_schedule().unwrap(),
                client.exam_course_schedule().unwrap(),
            )
        };
        let payload = exams_payload(&weeks, &exams, &arrangements);
        assert_eq!(payload["weeks"][0], json!({"key":"1","value":"第1周"}));
        assert_eq!(payload["exams"][0]["kcmc"], json!("合成考试课程"));
        assert_eq!(
            payload["course_arrangements"][0]["kcmc"],
            json!("合成课程安排")
        );
        assert_eq!(
            auth.calls, 1,
            "token must be fetched once for three requests"
        );
        assert_eq!(transport.calls.len(), 3);
        assert_eq!(transport.calls[0].headers[2].0, "token");
        assert_eq!(transport.calls[0].headers[2].1, TOKEN);
        assert!(transport.calls.iter().all(|call| call.body.is_empty()));
    }

    #[test]
    fn scores_payload_filters_semester_and_preserves_optional_fields() {
        let data: Value = serde_json::from_str(SCORES).expect("fixture JSON");
        let all = scores_payload(data.get("data").unwrap(), None).expect("all scores");
        assert_eq!(all["student_id"], json!("20260001"));
        assert_eq!(all["average_score"], json!(87.25));
        assert_eq!(all["count"], json!(2));
        assert_eq!(all["semesters"]["2025-2026-2"][0]["credit"], json!(3.5));
        assert_eq!(all["semesters"]["2025-2026-1"][0]["credit"], Value::Null);
        assert_eq!(all["semesters"]["2025-2026-1"][0]["score"], json!("85"));
        assert_eq!(
            all["gpa_entries"],
            json!([{"term":"2025-2026-2","gpa":3.75}])
        );

        let filtered =
            scores_payload(data.get("data").unwrap(), Some("2025-2026-2")).expect("filtered");
        assert_eq!(filtered["count"], json!(1));
        assert_eq!(filtered["semesters"].as_object().unwrap().len(), 1);
        assert_eq!(
            filtered["semesters"]["2025-2026-2"][0]["name"],
            json!("合成课程一")
        );

        let empty_filter = scores_payload(data.get("data").unwrap(), Some(""))
            .expect("empty filter is falsey, like the Python command");
        assert_eq!(empty_filter["count"], json!(2));
    }

    #[test]
    fn classrooms_payload_preserves_raw_text_and_capacity_nulls() {
        let data: Value = serde_json::from_str(CLASSROOMS).expect("fixture JSON");
        let payload = classrooms_payload("04", data.get("data").unwrap());
        assert_eq!(payload["campus"], json!("04"));
        assert_eq!(payload["count"], json!(2));
        assert_eq!(
            payload["periods"][0]["raw"],
            json!("教一楼-101(40),教二楼-202(),教三楼-303,教四楼-404(x),, (0)")
        );
        assert_eq!(
            payload["periods"][0]["rooms"],
            json!([
                {"name":"教一楼-101","capacity":40},
                {"name":"教二楼-202","capacity":null},
                {"name":"教三楼-303","capacity":null},
                {"name":"教四楼-404","capacity":null},
                {"name":"","capacity":0}
            ])
        );
        assert_eq!(payload["periods"][1]["rooms"][0]["capacity"], json!(50));
    }

    #[test]
    fn classrooms_query_uses_the_allowed_campus() {
        let mut transport = Scripted::default();
        queue(
            &mut transport,
            "GET",
            "https://jwglweixin.bupt.edu.cn/bjyddx/todayClassrooms?campusId=01",
            CLASSROOMS,
        );
        let mut auth = FakeAuth::valid();
        let data = {
            let mut client = client(&mut transport, &mut auth);
            client.today_classrooms("01").unwrap()
        };
        let payload = classrooms_payload("01", &data);
        assert_eq!(payload["campus"], json!("01"));
        assert_eq!(
            transport.calls[0].url,
            "https://jwglweixin.bupt.edu.cn/bjyddx/todayClassrooms?campusId=01"
        );
    }

    #[test]
    fn auth_failure_happens_before_transport() {
        let mut transport = Scripted::default();
        let mut auth = FakeAuth::with(vec![Err(auth_error(
            "missing token",
            "login --role undergraduate",
        ))]);
        let error = {
            let mut client = client(&mut transport, &mut auth);
            client.score_report().expect_err("missing token")
        };
        assert_eq!(error.code, 3);
        assert!(transport.calls.is_empty());
        assert_eq!(auth.calls, 1);
    }

    #[test]
    fn empty_or_expired_response_retries_once_via_the_auth_seam() {
        let mut transport = Scripted::default();
        queue(
            &mut transport,
            "GET",
            "https://jwglweixin.bupt.edu.cn/bjyddx/student/curriculum",
            "",
        );
        queue(
            &mut transport,
            "GET",
            "https://jwglweixin.bupt.edu.cn/bjyddx/student/curriculum",
            CURRICULUM,
        );
        let mut auth = FakeAuth::with(vec![
            Ok(MobileJwglToken::new("first", Some(4102444800.0))),
            Ok(MobileJwglToken::new("second", Some(4102444800.0))),
        ]);
        let data = {
            let mut client = client(&mut transport, &mut auth);
            client.curriculum().unwrap()
        };
        assert_eq!(data.len(), 2);
        assert_eq!(auth.invalidations, 1);
        assert_eq!(auth.calls, 2);
        assert_eq!(transport.calls.len(), 2);
        assert_eq!(transport.calls[1].headers[2].1, "second");
    }

    #[test]
    fn permission_denial_is_not_retried() {
        let body = json!({"code":"0","Msg":"未授权访问：/bjyddx/student/termGPA"}).to_string();
        let mut transport = Scripted::default();
        queue(
            &mut transport,
            "POST",
            "https://jwglweixin.bupt.edu.cn/bjyddx/student/termGPA",
            &body,
        );
        let mut auth = FakeAuth::valid();
        let error = {
            let mut client = client(&mut transport, &mut auth);
            client.score_report().expect_err("permission denial")
        };
        assert_eq!(error.code, 5);
        assert_eq!(auth.invalidations, 0);
        assert_eq!(transport.calls.len(), 1);
    }
}
