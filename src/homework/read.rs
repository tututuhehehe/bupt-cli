//! Read-only UCloud course, homework-list, and undone mapping.
//!
//! Callers inject both the transport and the verified native session; tests
//! substitute a synthetic session and `Scripted` transport.

use crate::cli::{CliError, CliResult};
use crate::transport::{Request, Transport};
use serde_json::{Map, Value};
use std::collections::HashSet;

const UCLOUD_API: &str = "https://apiucloud.bupt.edu.cn";
const COURSES_PATH: &str = "/ykt-site/site/list/student/current";
const UNDONE_PATH: &str = "/ykt-site/site/student/undone";
const LIST_PATH: &str = "/ykt-site/work/student/list";
const COURSE_PAGE_SIZE: usize = 50;
const COURSE_PAGE_LIMIT: u32 = 10;
/// Python `get_all` stops at 30 course sites and requests 30 per course page.
const HOMEWORK_SITE_LIMIT: usize = 30;
const HOMEWORK_COURSE_PAGE_SIZE: usize = 30;
const LIST_PAGE_SIZE: usize = 50;
const MAX_PAGINATION_PAGES: u32 = 1000;
/// Raw UCloud keys that may carry a publish/created/start timestamp, in
/// Python `publish_time` preference order.
const PUBLISH_TIME_KEYS: &[&str] = &[
    "assignmentBeginTime",
    "beginTime",
    "publishTime",
    "releaseTime",
    "createTime",
    "createdTime",
    "gmtCreate",
    "createDate",
    "startTime",
];

pub(super) struct Session {
    pub(super) access_token: String,
    pub(super) user_id: String,
}

impl Session {
    pub(super) fn new(access_token: String, user_id: String) -> Self {
        Self {
            access_token,
            user_id,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Course {
    pub(super) site_id: String,
    pub(super) site_name: String,
    pub(super) teacher_name: String,
}

impl Course {
    fn as_json(&self) -> Value {
        serde_json::json!({
            "id": self.site_id,
            "name": self.site_name,
            "teacher": if self.teacher_name.is_empty() {
                Value::Null
            } else {
                Value::String(self.teacher_name.clone())
            },
        })
    }
}

pub(super) fn courses_json(courses: &[Course]) -> Value {
    serde_json::json!({
        "count": courses.len(),
        "courses": courses.iter().map(Course::as_json).collect::<Vec<_>>(),
    })
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct HomeworkItem {
    pub(super) activity_id: String,
    pub(super) activity_name: String,
    pub(super) site_id: Option<String>,
    pub(super) site_name: String,
    pub(super) begin_time: Option<String>,
    pub(super) end_time: Option<String>,
    pub(super) submit_time: Option<String>,
    pub(super) is_submitted: bool,
    pub(super) score: Option<f64>,
    pub(super) raw: Map<String, Value>,
}

impl HomeworkItem {
    fn parsed_end_time(&self) -> Option<time::PrimitiveDateTime> {
        self.end_time.as_deref().and_then(local_timestamp)
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct UndoneOverview {
    pub(super) total_sites: u64,
    pub(super) undone_count: u64,
    pub(super) items: Vec<HomeworkItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ListFilters {
    pub(super) undone: bool,
    pub(super) course: Option<String>,
    pub(super) new_days: Option<u64>,
}

pub(super) fn undone_json(
    overview: UndoneOverview,
    limit: u64,
    now: time::PrimitiveDateTime,
) -> Value {
    homework_list_json(
        overview.items,
        "undone",
        ListFilters {
            undone: true,
            course: None,
            new_days: None,
        },
        limit,
        now,
    )
}

/// Normalize homework rows into the Python `assignment_dict` shape, then
/// deduplicate by non-empty activity id, filter, sort by deadline/title, and
/// apply `--limit` (0 means all). `now` is injected so overdue and
/// `hours_remaining` stay deterministic in tests.
pub(super) fn homework_list_json(
    items: Vec<HomeworkItem>,
    method: &str,
    filters: ListFilters,
    limit: u64,
    now: time::PrimitiveDateTime,
) -> Value {
    let mut seen = HashSet::new();
    let mut selected: Vec<HomeworkItem> = items
        .into_iter()
        .filter(|item| item.activity_id.is_empty() || seen.insert(item.activity_id.clone()))
        .collect();
    if let Some(course) = filters
        .course
        .as_deref()
        .filter(|course| !course.is_empty())
    {
        selected.retain(|item| item.site_id.as_deref() == Some(course));
    }
    if filters.undone {
        selected.retain(|item| !item.is_submitted);
    }
    selected.sort_by(|left, right| {
        let left_due = left.parsed_end_time();
        let right_due = right.parsed_end_time();
        left_due
            .is_none()
            .cmp(&right_due.is_none())
            .then(left_due.cmp(&right_due))
            .then(left.activity_name.cmp(&right.activity_name))
    });
    if limit != 0 {
        selected.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    }
    let assignments: Vec<Value> = selected
        .iter()
        .map(|item| assignment_json(item, now))
        .collect();
    serde_json::json!({
        "count": assignments.len(),
        "method": method,
        "filters": {
            "undone": filters.undone,
            "course": filters.course,
            "new_days": filters.new_days,
        },
        "assignments": assignments,
    })
}

fn assignment_json(item: &HomeworkItem, now: time::PrimitiveDateTime) -> Value {
    let due = item.parsed_end_time();
    let overdue = due.is_some_and(|due| now > due);
    let status = if item.is_submitted {
        "submitted"
    } else if overdue {
        "overdue"
    } else {
        "pending"
    };
    let hours = due.map(|due| (((due - now).as_seconds_f64() / 36.0).round_ties_even()) / 100.0);
    serde_json::json!({
        "id": item.activity_id,
        "title": item.activity_name,
        "course": (!item.site_name.is_empty()).then_some(item.site_name.clone()),
        "course_id": item.site_id,
        "published_at": publish_time(item).map(iso_local_timestamp),
        "deadline": item.end_time,
        "begin_time": item.begin_time,
        "status": status,
        "submitted": item.is_submitted,
        "submitted_at": item.submit_time,
        "score": item.score,
        "hours_remaining": hours,
        "overdue": overdue,
    })
}

fn publish_time(item: &HomeworkItem) -> Option<time::PrimitiveDateTime> {
    for key in PUBLISH_TIME_KEYS {
        if let Some(parsed) = item
            .raw
            .get(*key)
            .and_then(Value::as_str)
            .and_then(local_timestamp)
        {
            return Some(parsed);
        }
    }
    item.begin_time.as_deref().and_then(local_timestamp)
}

fn local_timestamp(text: &str) -> Option<time::PrimitiveDateTime> {
    use time::macros::format_description;
    let text = text.split('.').next()?.trim();
    let text = text.replace('T', " ");
    time::PrimitiveDateTime::parse(
        &text,
        format_description!("[year]-[month]-[day] [hour]:[minute]:[second]"),
    )
    .ok()
    .or_else(|| {
        time::PrimitiveDateTime::parse(
            &text,
            format_description!("[year]-[month]-[day] [hour]:[minute]"),
        )
        .ok()
    })
    .or_else(|| {
        time::Date::parse(&text, format_description!("[year]-[month]-[day]"))
            .ok()
            .map(time::Date::midnight)
    })
}

fn iso_local_timestamp(value: time::PrimitiveDateTime) -> String {
    format!(
        "{} {:02}:{:02}:{:02}",
        value.date(),
        value.hour(),
        value.minute(),
        value.second()
    )
}

/// Fetch every enrolled course page, preserving page order and dropping only
/// repeated non-empty site ids. Empty ids are always retained because they
/// cannot be deduplicated without losing a course.
pub(super) fn fetch_courses(
    transport: &mut dyn Transport,
    session: &Session,
) -> CliResult<Vec<Course>> {
    let mut courses = Vec::new();
    let mut seen = HashSet::new();

    for page in 1..=COURSE_PAGE_LIMIT {
        let (batch, record_count) = fetch_course_page(transport, session, page)?;
        let mut fresh = 0usize;
        for course in batch {
            if !course.site_id.is_empty() && !seen.insert(course.site_id.clone()) {
                continue;
            }
            courses.push(course);
            fresh += 1;
        }
        // A duplicate-only page also ends the walk: upstream is no longer
        // making progress, even when the page itself is full.
        if fresh == 0 || record_count < COURSE_PAGE_SIZE {
            break;
        }
    }

    Ok(courses)
}

/// Fetch one course page. List/submission-time lookups use only the first
/// page, matching Python ``get_enrolled_courses()``.
pub(super) fn fetch_course_page(
    transport: &mut dyn Transport,
    session: &Session,
    page: u32,
) -> CliResult<(Vec<Course>, usize)> {
    let parsed = fetch_course_page_with_size(transport, session, page, COURSE_PAGE_SIZE)?;
    Ok((parsed.courses, parsed.record_count))
}

#[derive(Debug)]
struct CoursePage {
    courses: Vec<Course>,
    record_count: usize,
    total: Option<u64>,
    signature: String,
}

fn fetch_course_page_with_size(
    transport: &mut dyn Transport,
    session: &Session,
    page: u32,
    size: usize,
) -> CliResult<CoursePage> {
    let url = format!("{UCLOUD_API}{COURSES_PATH}?size={size}&current={page}");
    let data = get_data(transport, session, "UCloud course list", url)?;
    parse_course_page(&data)
}

/// Walk the course list for the default `homework` view. Python `get_all`
/// requests up to 30 sites, follows a page when `total` says more rows remain,
/// and refuses to loop on an unchanged page.
fn fetch_all_courses_for_homework(
    transport: &mut dyn Transport,
    session: &Session,
) -> CliResult<Vec<Course>> {
    let mut courses = Vec::new();
    let mut fetched_records = 0usize;
    let mut seen_pages = HashSet::new();
    let mut finished = false;
    for page in 1..=MAX_PAGINATION_PAGES {
        let CoursePage {
            courses: batch,
            record_count,
            total,
            signature,
        } = fetch_course_page_with_size(transport, session, page, HOMEWORK_COURSE_PAGE_SIZE)?;
        if record_count > 0 && !seen_pages.insert(signature) {
            return Err(CliError::new(
                5,
                "parse",
                "UCloud course pagination repeated a page",
                "",
            ));
        }
        let remaining = HOMEWORK_SITE_LIMIT - courses.len();
        courses.extend(batch.into_iter().take(remaining));
        fetched_records += record_count;
        if courses.len() >= HOMEWORK_SITE_LIMIT
            || pagination_finished(
                record_count,
                HOMEWORK_COURSE_PAGE_SIZE,
                fetched_records,
                total,
            )
        {
            finished = true;
            break;
        }
    }
    if !finished {
        return Err(CliError::new(
            5,
            "parse",
            "UCloud course pagination exceeded the safety limit",
            "",
        ));
    }
    Ok(courses)
}

/// Fetch one course's homework pages. The CLI uses this for `--course`, so a
/// course with more than one page is not silently truncated.
pub(super) fn fetch_course_homework(
    transport: &mut dyn Transport,
    session: &Session,
    site_id: &str,
) -> CliResult<Vec<HomeworkItem>> {
    if session.user_id.is_empty() {
        return Err(missing_user_id());
    }
    let mut items = Vec::new();
    let mut fetched_records = 0usize;
    let mut seen_pages = HashSet::new();
    let mut finished = false;
    for page in 1..=MAX_PAGINATION_PAGES {
        let HomeworkPage {
            items: batch,
            record_count,
            total,
            signature,
        } = fetch_homework_page(transport, session, site_id, page)?;
        if record_count > 0 && !seen_pages.insert(signature) {
            return Err(CliError::new(
                5,
                "parse",
                format!("UCloud homework pagination repeated a page for site '{site_id}'"),
                "",
            ));
        }
        items.extend(batch);
        fetched_records += record_count;
        if pagination_finished(record_count, LIST_PAGE_SIZE, fetched_records, total) {
            finished = true;
            break;
        }
    }
    if !finished {
        return Err(CliError::new(
            5,
            "parse",
            format!("UCloud homework pagination exceeded the safety limit for site '{site_id}'"),
            "",
        ));
    }
    Ok(items)
}

/// Fetch all homework for up to 30 course sites, matching Python `get_all`:
/// a course's own `siteName` fills rows that omit it.
pub(super) fn fetch_all_homework(
    transport: &mut dyn Transport,
    session: &Session,
) -> CliResult<Vec<HomeworkItem>> {
    if session.user_id.is_empty() {
        return Err(missing_user_id());
    }
    let courses = fetch_all_courses_for_homework(transport, session)?;
    let mut items = Vec::new();
    for course in courses {
        if course.site_id.is_empty() {
            continue;
        }
        let mut batch = fetch_course_homework(transport, session, &course.site_id)?;
        for item in &mut batch {
            if item.site_name.is_empty() {
                item.site_name = course.site_name.clone();
            }
        }
        items.extend(batch);
    }
    Ok(items)
}

#[derive(Debug)]
struct HomeworkPage {
    items: Vec<HomeworkItem>,
    record_count: usize,
    total: Option<u64>,
    signature: String,
}

fn fetch_homework_page(
    transport: &mut dyn Transport,
    session: &Session,
    site_id: &str,
    page: u32,
) -> CliResult<HomeworkPage> {
    let body = serde_json::json!({
        "siteId": site_id,
        "userId": session.user_id,
        "current": page,
        "size": LIST_PAGE_SIZE,
    });
    let data = post_data(
        transport,
        session,
        "UCloud homework list",
        format!("{UCLOUD_API}{LIST_PATH}"),
        &body,
    )?;
    parse_homework_page(&data, site_id)
}

fn parse_homework_page(data: &Value, site_id: &str) -> CliResult<HomeworkPage> {
    let object = data.as_object().ok_or_else(|| {
        CliError::new(
            5,
            "parse",
            "UCloud homework list returned malformed data",
            "",
        )
    })?;
    let records = object
        .get("records")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            CliError::new(
                5,
                "parse",
                "UCloud homework list returned malformed records",
                "",
            )
        })?;

    let mut items = Vec::with_capacity(records.len());
    for record in records {
        let record = record.as_object().ok_or_else(|| {
            CliError::new(
                5,
                "parse",
                "UCloud homework list returned malformed records",
                "",
            )
        })?;
        let status = first_present(record, &["assignmentStatus", "status"]);
        let submitted = status.is_some_and(list_status_submitted)
            || record.get("submitTime").is_some_and(is_truthy);
        items.push(HomeworkItem {
            activity_id: first_present_or_empty(record, &["activityId", "id"]),
            activity_name: first_present_or_empty(record, &["assignmentTitle", "activityName"]),
            site_id: Some(site_id.to_owned()),
            site_name: record
                .get("siteName")
                .filter(|value| is_truthy(value))
                .and_then(value_as_string)
                .unwrap_or_default(),
            begin_time: first_present(record, &["assignmentBeginTime", "beginTime"])
                .and_then(value_as_string),
            end_time: first_present(record, &["assignmentEndTime", "endTime"])
                .and_then(value_as_string),
            submit_time: record
                .get("submitTime")
                .filter(|value| is_truthy(value))
                .and_then(value_as_string),
            is_submitted: submitted,
            score: record.get("score").and_then(parse_score),
            raw: record.clone(),
        });
    }
    let total = pagination_total(object, "UCloud homework list")?;
    let signature = serde_json::to_string(records).unwrap_or_default();
    Ok(HomeworkPage {
        items,
        record_count: records.len(),
        total,
        signature,
    })
}

fn list_status_submitted(value: &Value) -> bool {
    match value {
        Value::Bool(_) => false,
        Value::Number(number) => number.as_f64() == Some(0.0),
        Value::String(text) => text == "0" || text == "SUBMITTED",
        _ => false,
    }
}

fn parse_score(value: &Value) -> Option<f64> {
    match value {
        Value::Bool(_) | Value::Null | Value::Array(_) | Value::Object(_) => None,
        Value::Number(number) => number.as_f64(),
        Value::String(text) if !text.trim().is_empty() => text.trim().parse().ok(),
        Value::String(_) => None,
    }
}

fn pagination_finished(
    record_count: usize,
    requested_size: usize,
    fetched_records: usize,
    total: Option<u64>,
) -> bool {
    if record_count == 0 {
        return true;
    }
    if let Some(total) = total {
        return fetched_records as u64 >= total;
    }
    record_count < requested_size
}

fn missing_user_id() -> CliError {
    CliError::new(
        3,
        "auth",
        "UCloud token is missing user_id",
        "Run `bupt login --force`, then retry.",
    )
}

/// Fetch the pending-assignment overview. This maps only fields present in the
/// undone endpoint; it does not sweep course pages to fill in `-1`/blank course
/// metadata (the Python enrichment pass remains deferred).
pub(super) fn fetch_undone(
    transport: &mut dyn Transport,
    session: &Session,
) -> CliResult<UndoneOverview> {
    if session.user_id.is_empty() {
        return Err(missing_user_id());
    }
    let url = query_url(UNDONE_PATH, &[("userId", session.user_id.as_str())]);
    let data = get_data(transport, session, "UCloud undone homework", url)?;
    parse_undone(&data)
}

pub(super) fn get_data(
    transport: &mut dyn Transport,
    session: &Session,
    operation: &str,
    url: String,
) -> CliResult<Value> {
    let object = send_payload(transport, session, operation, "GET", url, None)?;
    object
        .get("data")
        .cloned()
        .ok_or_else(|| CliError::new(5, "parse", format!("{operation} response missing data"), ""))
}

pub(super) fn post_data(
    transport: &mut dyn Transport,
    session: &Session,
    operation: &str,
    url: String,
    body: &Value,
) -> CliResult<Value> {
    let object = send_payload(transport, session, operation, "POST", url, Some(body))?;
    object
        .get("data")
        .cloned()
        .ok_or_else(|| CliError::new(5, "parse", format!("{operation} response missing data"), ""))
}

/// Send one authenticated JSON request and return the top-level object after
/// HTTP/envelope checks. Transport failures keep their original network layer.
pub(super) fn send_payload(
    transport: &mut dyn Transport,
    session: &Session,
    operation: &str,
    method: &'static str,
    url: String,
    body: Option<&Value>,
) -> CliResult<Map<String, Value>> {
    let mut headers = auth_headers(&session.access_token);
    let body = match body {
        Some(body) => {
            headers.push(("Content-Type".into(), "application/json".into()));
            serde_json::to_vec(body).map_err(|_| {
                CliError::new(
                    5,
                    "parse",
                    format!("{operation} request could not be serialized"),
                    "",
                )
            })?
        }
        None => Vec::new(),
    };
    let response = transport.send(Request {
        method,
        url,
        headers,
        body,
    })?;

    let status = response.status;
    if status == 401 || status == 403 {
        return Err(CliError::new(
            3,
            "auth",
            format!("{operation} was rejected as unauthorized"),
            "Run `bupt login --force`, then retry.",
        ));
    }
    if (400..600).contains(&status) {
        return Err(CliError::new(
            5,
            "upstream",
            format!("{operation} was explicitly refused with HTTP {status}"),
            "The command did not retry.",
        ));
    }
    if !(200..300).contains(&status) {
        return Err(CliError::new(
            4,
            "network",
            format!("{operation} failed with HTTP {status}"),
            "The command did not retry.",
        ));
    }

    let payload: Value = serde_json::from_slice(&response.body)
        .map_err(|_| CliError::new(5, "parse", format!("{operation} returned invalid JSON"), ""))?;
    let object = payload.as_object().ok_or_else(|| {
        CliError::new(
            5,
            "parse",
            format!("{operation} returned a non-object response"),
            "",
        )
    })?;
    if !code_is_ok(object.get("code")) {
        let message = object.get("msg").and_then(Value::as_str).unwrap_or("");
        if looks_like_auth_error(message) {
            return Err(CliError::new(
                3,
                "auth",
                format!("{operation} failed authentication"),
                "Run `bupt login --force`, then retry.",
            ));
        }
        return Err(CliError::new(
            4,
            "network",
            format!("{operation} failed with an upstream error"),
            "The command did not retry.",
        ));
    }
    Ok(object.clone())
}

fn parse_course_page(data: &Value) -> CliResult<CoursePage> {
    let object = data.as_object().ok_or_else(|| {
        CliError::new(5, "parse", "UCloud course list returned malformed data", "")
    })?;
    let records = object
        .get("records")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            CliError::new(
                5,
                "parse",
                "UCloud course list returned malformed records",
                "",
            )
        })?;

    let mut courses = Vec::with_capacity(records.len());
    for record in records {
        let record = record.as_object().ok_or_else(|| {
            CliError::new(
                5,
                "parse",
                "UCloud course list returned malformed records",
                "",
            )
        })?;
        courses.push(Course {
            // Match Python `_first_present(record, ...) or ""`: an explicit
            // zero/false is present, then becomes empty; it must not fall back
            // to the generic key. Only null/empty-string values fall through.
            site_id: first_present_or_empty(record, &["siteId", "id"]),
            site_name: first_present_or_empty(record, &["siteName", "className"]),
            teacher_name: format_teacher_name(first_present(record, &["teacherName", "teachers"])),
        });
    }
    // `cmd_courses` does not use `total` to stop, but the Python parser still
    // rejects a malformed pagination total. Validate it for parity.
    let total = pagination_total(object, "UCloud course list")?;
    let signature = serde_json::to_string(records).unwrap_or_default();
    Ok(CoursePage {
        courses,
        record_count: records.len(),
        total,
        signature,
    })
}

fn parse_undone(data: &Value) -> CliResult<UndoneOverview> {
    let object = data.as_object().ok_or_else(|| {
        CliError::new(
            5,
            "parse",
            "UCloud undone homework returned malformed data",
            "",
        )
    })?;
    let records = object
        .get("undoneList")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            CliError::new(
                5,
                "parse",
                "UCloud undone homework returned malformed records",
                "",
            )
        })?;

    let mut items = Vec::with_capacity(records.len());
    for record in records {
        let record = record.as_object().ok_or_else(|| {
            CliError::new(
                5,
                "parse",
                "UCloud undone homework returned malformed records",
                "",
            )
        })?;
        items.push(HomeworkItem {
            activity_id: first_truthy_string(record, &["activityId", "id"]),
            activity_name: first_truthy_string(record, &["activityName", "assignmentTitle"]),
            site_id: record
                .get("siteId")
                .filter(|value| is_truthy(value))
                .and_then(value_as_string),
            site_name: first_truthy_string(record, &["siteName"]),
            begin_time: record.get("beginTime").and_then(value_as_string),
            end_time: record.get("endTime").and_then(value_as_string),
            submit_time: None,
            is_submitted: false,
            score: None,
            raw: record.clone(),
        });
    }

    Ok(UndoneOverview {
        total_sites: count_value(object.get("siteNum"), 0),
        undone_count: count_value(object.get("undoneNum"), items.len() as u64),
        items,
    })
}

pub(super) fn pagination_total(
    object: &Map<String, Value>,
    operation: &str,
) -> CliResult<Option<u64>> {
    for key in ["total", "totalCount", "totalElements"] {
        let Some(value) = object.get(key) else {
            continue;
        };
        if value.is_null() || value.as_str() == Some("") {
            continue;
        }
        let invalid = || {
            CliError::new(
                5,
                "parse",
                format!("{operation} returned an invalid pagination total"),
                "",
            )
        };
        let total = match value {
            Value::Bool(_) => return Err(invalid()),
            Value::Number(number) => number.as_u64().ok_or_else(invalid)?,
            Value::String(text) => text.parse::<u64>().map_err(|_| invalid())?,
            _ => return Err(invalid()),
        };
        return Ok(Some(total));
    }
    Ok(None)
}

pub(super) fn first_present<'a>(
    object: &'a Map<String, Value>,
    keys: &[&str],
) -> Option<&'a Value> {
    for key in keys {
        if let Some(value) = object.get(*key)
            && !value.is_null()
            && value.as_str() != Some("")
        {
            return Some(value);
        }
    }
    None
}

fn first_present_or_empty(object: &Map<String, Value>, keys: &[&str]) -> String {
    first_present(object, keys)
        .filter(|value| is_truthy(value))
        .and_then(value_as_string)
        .unwrap_or_default()
}

fn first_truthy_string(object: &Map<String, Value>, keys: &[&str]) -> String {
    for key in keys {
        if let Some(value) = object.get(*key)
            && is_truthy(value)
        {
            return value_as_string(value).unwrap_or_default();
        }
    }
    String::new()
}

fn format_teacher_name(value: Option<&Value>) -> String {
    match value {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| match item {
                Value::Object(object) => {
                    let name = first_truthy_string(object, &["realName", "name"]);
                    (!name.is_empty()).then_some(name)
                }
                value if is_truthy(value) => value_as_string(value),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(", "),
        Some(Value::Object(object)) => first_truthy_string(object, &["realName", "name"]),
        Some(value) if is_truthy(value) => value_as_string(value).unwrap_or_default(),
        _ => String::new(),
    }
}

fn count_value(value: Option<&Value>, default: u64) -> u64 {
    match value {
        Some(Value::Number(number)) => number.as_u64().unwrap_or(default),
        Some(Value::String(text)) => text.parse().unwrap_or(default),
        _ => default,
    }
}

pub(super) fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_f64().is_some_and(|number| number != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(object) => !object.is_empty(),
    }
}

pub(super) fn value_as_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

fn code_is_ok(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::Number(number)) => number.as_i64().is_some_and(|code| code == 0 || code == 200),
        Some(Value::String(text)) => text == "0" || text == "200",
        _ => false,
    }
}

fn looks_like_auth_error(message: &str) -> bool {
    let text = message.to_ascii_lowercase();
    text.contains("401")
        || text.contains("403")
        || text.contains("token")
        || text.contains("auth")
        || text.contains("login")
        || message.contains("登录")
}

pub(super) fn auth_headers(access_token: &str) -> Vec<(String, String)> {
    vec![
        ("Blade-Auth".into(), access_token.to_owned()),
        ("Tenant-Id".into(), "000000".into()),
        (
            "Authorization".into(),
            "Basic c3dvcmQ6c3dvcmRfc2VjcmV0".into(),
        ),
    ]
}

pub(super) fn query_url(path: &str, params: &[(&str, &str)]) -> String {
    let mut url = format!("{UCLOUD_API}{path}");
    for (index, (key, value)) in params.iter().enumerate() {
        url.push(if index == 0 { '?' } else { '&' });
        url.push_str(key);
        url.push('=');
        url.push_str(&percent_encode(value.as_bytes()));
    }
    url
}

fn percent_encode(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len());
    for byte in bytes {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(*byte));
        } else {
            encoded.push('%');
            encoded.push_str(&format!("{byte:02X}"));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::Response;
    use crate::transport::testing::Scripted;
    use serde_json::json;
    use std::collections::VecDeque;
    use std::fs;
    use std::path::Path;

    fn session() -> Session {
        Session {
            access_token: "synthetic-access-token".into(),
            user_id: "synthetic-user".into(),
        }
    }

    fn json_response(status: u16, body: Value) -> CliResult<Response> {
        Ok(Response {
            status,
            headers: Vec::new(),
            body: serde_json::to_vec(&body).expect("serialize synthetic response"),
        })
    }

    fn scripted(entries: Vec<(&'static str, String, CliResult<Response>)>) -> Scripted {
        Scripted {
            expected: VecDeque::from(entries),
            calls: Vec::new(),
        }
    }

    fn fixture(name: &str) -> Value {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/contracts/homework")
            .join(name);
        serde_json::from_slice(&fs::read(path).expect("read synthetic fixture"))
            .expect("parse synthetic fixture")
    }

    fn fixture_response(name: &str, exchange: usize) -> CliResult<Response> {
        let document = fixture(name);
        assert_eq!(document["schema"], "bupt.contract-replay.v1");
        let response = &document["exchanges"][exchange]["response"];
        let status = response["status"].as_u64().expect("fixture status") as u16;
        json_response(status, response["json"].clone())
    }

    fn course_url(page: u32) -> String {
        format!("{UCLOUD_API}{COURSES_PATH}?size={COURSE_PAGE_SIZE}&current={page}")
    }

    fn undone_url() -> String {
        query_url(UNDONE_PATH, &[("userId", "synthetic-user")])
    }

    fn assert_single_call(transport: &Scripted, method: &str, url: &str) {
        assert_eq!(transport.calls.len(), 1, "expected exactly one request");
        assert_eq!(transport.calls[0].method, method);
        assert_eq!(transport.calls[0].url, url);
        let token = transport.calls[0]
            .headers
            .iter()
            .find(|(name, _)| name == "Blade-Auth")
            .map(|(_, value)| value.as_str());
        assert_eq!(token, Some("synthetic-access-token"));
    }

    fn page(records: Value, total: Option<u64>) -> CliResult<Response> {
        let mut data = serde_json::json!({"records": records});
        if let Some(total) = total {
            data["total"] = json!(total);
        }
        json_response(200, json!({"code": 200, "data": data}))
    }

    #[test]
    fn courses_first_page_keeps_all_eleven_unique_site_ids() {
        let mut transport = scripted(vec![(
            "GET",
            course_url(1),
            fixture_response("courses_first_page.json", 0),
        )]);
        let courses = fetch_courses(&mut transport, &session()).expect("course page");
        let ids: Vec<&str> = courses
            .iter()
            .map(|course| course.site_id.as_str())
            .collect();

        assert_eq!(courses.len(), 11);
        assert_eq!(ids.first(), Some(&"course-01"));
        assert_eq!(ids.last(), Some(&"course-11"));
        assert_eq!(ids.iter().collect::<HashSet<_>>().len(), 11);
        assert_eq!(courses_json(&courses)["count"], 11);
        assert_single_call(&transport, "GET", &course_url(1));
    }

    #[test]
    fn courses_short_page_stops_without_a_second_request() {
        let mut transport = scripted(vec![(
            "GET",
            course_url(1),
            fixture_response("courses_short_page.json", 0),
        )]);
        let courses = fetch_courses(&mut transport, &session()).expect("short page");

        assert_eq!(courses.len(), 2);
        assert_single_call(&transport, "GET", &course_url(1));
    }

    #[test]
    fn courses_empty_page_stops_without_a_second_request() {
        let mut transport = scripted(vec![(
            "GET",
            course_url(1),
            fixture_response("courses_empty_page.json", 0),
        )]);
        let courses = fetch_courses(&mut transport, &session()).expect("empty page");

        assert!(courses.is_empty());
        assert_eq!(courses_json(&courses), json!({"count": 0, "courses": []}));
        assert_single_call(&transport, "GET", &course_url(1));
    }

    #[test]
    fn courses_concatenate_pages_and_deduplicate_repeated_ids() {
        let mut transport = scripted(vec![
            (
                "GET",
                course_url(1),
                fixture_response("courses_pagination.json", 0),
            ),
            (
                "GET",
                course_url(2),
                fixture_response("courses_pagination.json", 1),
            ),
        ]);
        let courses = fetch_courses(&mut transport, &session()).expect("paginated courses");
        let ids: Vec<&str> = courses
            .iter()
            .map(|course| course.site_id.as_str())
            .collect();

        assert_eq!(courses.len(), 51);
        assert_eq!(ids.iter().filter(|id| **id == "c0").count(), 1);
        assert_eq!(ids.iter().filter(|id| **id == "c1").count(), 1);
        assert_eq!(ids.first(), Some(&"c0"));
        assert_eq!(ids.last(), Some(&"c50"));
        assert_eq!(courses[1].teacher_name, "");
        assert_eq!(transport.calls.len(), 2);
    }

    #[test]
    fn courses_keep_every_empty_site_id() {
        let mut transport = scripted(vec![(
            "GET",
            course_url(1),
            fixture_response("courses_empty_ids.json", 0),
        )]);
        let courses = fetch_courses(&mut transport, &session()).expect("courses without ids");

        assert_eq!(courses.len(), 3);
        assert!(courses.iter().all(|course| course.site_id.is_empty()));
        assert_single_call(&transport, "GET", &course_url(1));
    }

    #[test]
    fn courses_stop_when_a_full_page_adds_no_new_ids() {
        let records: Vec<Value> = (0..50)
            .map(|index| json!({"siteId": format!("c{index}"), "siteName": "合成课程"}))
            .collect();
        let mut transport = scripted(vec![
            ("GET", course_url(1), page(json!(records), Some(100))),
            ("GET", course_url(2), page(json!(records), Some(100))),
        ]);
        let courses = fetch_courses(&mut transport, &session()).expect("duplicate-only page");

        assert_eq!(courses.len(), 50);
        assert_eq!(transport.calls.len(), 2);
    }

    #[test]
    fn courses_stop_at_the_ten_page_safety_limit() {
        let mut entries = Vec::new();
        for page_number in 1..=COURSE_PAGE_LIMIT {
            let records: Vec<Value> = (0..COURSE_PAGE_SIZE)
                .map(|index| {
                    json!({
                        "siteId": format!("p{page_number}-c{index}"),
                        "siteName": format!("第 {page_number} 页"),
                    })
                })
                .collect();
            entries.push((
                "GET",
                course_url(page_number),
                page(json!(records), Some(550)),
            ));
        }
        let mut transport = scripted(entries);
        let courses = fetch_courses(&mut transport, &session()).expect("bounded paging");

        assert_eq!(courses.len(), 500);
        assert_eq!(transport.calls.len(), COURSE_PAGE_LIMIT as usize);
        assert_eq!(transport.calls.last().unwrap().url, course_url(10));
    }

    #[test]
    fn unauthorized_courses_and_undone_stop_after_one_request() {
        for status in [401, 403] {
            let mut courses_transport = scripted(vec![(
                "GET",
                course_url(1),
                json_response(
                    status,
                    json!({"msg": "synthetic rejection synthetic-access-token"}),
                ),
            )]);
            let error = fetch_courses(&mut courses_transport, &session())
                .expect_err("unauthorized courses must fail");
            assert_eq!(error.code, 3, "courses HTTP {status}");
            assert_eq!(courses_transport.calls.len(), 1);
            assert!(!error.message.contains("synthetic-access-token"));

            let mut undone_transport = scripted(vec![(
                "GET",
                undone_url(),
                json_response(
                    status,
                    json!({"msg": "synthetic rejection synthetic-access-token"}),
                ),
            )]);
            let error = fetch_undone(&mut undone_transport, &session())
                .expect_err("unauthorized undone must fail");
            assert_eq!(error.code, 3, "undone HTTP {status}");
            assert_eq!(undone_transport.calls.len(), 1);
            assert!(!error.message.contains("synthetic-access-token"));
        }
    }

    #[test]
    fn unexpected_course_payloads_are_parse_errors_without_retry() {
        let bodies = [
            json!({"code": 200, "data": {"records": "not-an-array"}}),
            json!({"code": 200, "data": []}),
            json!({"code": 200}),
            json!({"code": 200, "data": {"records": [{"siteId": "ok"}, 7]}}),
            json!({"code": 200, "data": {"records": [], "total": "not-a-number"}}),
        ];
        for body in bodies {
            let mut transport = scripted(vec![(
                "GET",
                course_url(1),
                json_response(200, body.clone()),
            )]);
            let error = fetch_courses(&mut transport, &session())
                .expect_err("malformed course payload must fail");
            assert_eq!(error.code, 5, "payload: {body}");
            assert_eq!(transport.calls.len(), 1);
            assert!(!error.message.contains("synthetic-access-token"));
        }
    }

    #[test]
    fn course_mapping_does_not_replace_explicit_zero_or_false_with_fallback_keys() {
        let body = json!({
            "code": 200,
            "data": {
                "records": [
                    {
                        "siteId": 0,
                        "id": "fallback-id",
                        "siteName": false,
                        "className": "fallback-name"
                    },
                    {
                        "siteId": "",
                        "id": "id-from-empty",
                        "siteName": "",
                        "className": "name-from-empty"
                    },
                    {
                        "siteId": "site-1",
                        "id": "ignored-id",
                        "siteName": "课程一",
                        "className": "ignored-name"
                    }
                ],
                "total": 3
            }
        });
        let mut transport = scripted(vec![("GET", course_url(1), json_response(200, body))]);
        let courses = fetch_courses(&mut transport, &session()).expect("course mapping");

        assert_eq!(courses[0].site_id, "", "explicit zero must not use id");
        assert_eq!(
            courses[0].site_name, "",
            "explicit false must not use className"
        );
        assert_eq!(courses[1].site_id, "id-from-empty");
        assert_eq!(courses[1].site_name, "name-from-empty");
        assert_eq!(courses[2].site_id, "site-1");
        assert_eq!(courses[2].site_name, "课程一");
        assert_single_call(&transport, "GET", &course_url(1));
    }

    #[test]
    fn http_503_is_upstream_5_without_retry() {
        let mut transport = scripted(vec![(
            "GET",
            course_url(1),
            json_response(
                503,
                json!({"msg": "synthetic unavailable synthetic-access-token"}),
            ),
        )]);
        let error = fetch_courses(&mut transport, &session()).expect_err("HTTP 503");

        assert_eq!(error.code, 5);
        assert_eq!(error.kind, "upstream");
        assert_eq!(transport.calls.len(), 1);
        assert!(!error.message.contains("synthetic-access-token"));
    }

    #[test]
    fn transport_error_remains_network_4_without_retry() {
        let mut transport = scripted(vec![(
            "GET",
            course_url(1),
            Err(CliError::new(
                4,
                "network",
                "synthetic transport unavailable",
                "synthetic transport hint",
            )),
        )]);
        let error = fetch_courses(&mut transport, &session()).expect_err("transport error");

        assert_eq!(error.code, 4);
        assert_eq!(error.kind, "network");
        assert_eq!(transport.calls.len(), 1);
    }

    #[test]
    fn api_envelope_error_remains_network_4_without_retry() {
        let mut transport = scripted(vec![(
            "GET",
            course_url(1),
            json_response(
                200,
                json!({"code": 500, "msg": "synthetic upstream failure"}),
            ),
        )]);
        let error = fetch_courses(&mut transport, &session()).expect_err("API envelope error");

        assert_eq!(error.code, 4);
        assert_eq!(error.kind, "network");
        assert_eq!(transport.calls.len(), 1);
        assert!(!error.message.contains("synthetic-access-token"));
    }

    #[test]
    fn undone_mapping_matches_the_python_fields() {
        let mut transport = scripted(vec![(
            "GET",
            undone_url(),
            fixture_response("homework_list_undone.json", 0),
        )]);
        let overview = fetch_undone(&mut transport, &session()).expect("undone overview");

        assert_eq!(overview.total_sites, 1);
        assert_eq!(overview.undone_count, 2);
        assert_eq!(overview.items.len(), 2);
        assert_eq!(overview.items[0].activity_id, "u-2");
        assert_eq!(overview.items[0].activity_name, "未交二");
        assert_eq!(overview.items[0].site_id.as_deref(), Some("site-1"));
        assert_eq!(overview.items[0].site_name, "课程一");
        assert_eq!(
            overview.items[0].begin_time.as_deref(),
            Some("2026-11-10 08:00")
        );
        assert_eq!(
            overview.items[0].end_time.as_deref(),
            Some("2026-11-22 23:59")
        );
        assert!(!overview.items[0].is_submitted);
        assert_eq!(overview.items[1].activity_id, "u-1");
        assert_single_call(&transport, "GET", &undone_url());
    }

    #[test]
    fn undone_mapping_keeps_python_fallback_rules_without_course_enrichment() {
        let body = json!({
            "code": 200,
            "data": {
                "siteNum": 2,
                "undoneList": [
                    {
                        "id": "fallback-1",
                        "assignmentTitle": "备用标题",
                        "siteId": 0,
                        "siteName": "",
                    },
                    {
                        "activityId": "",
                        "id": "fallback-2",
                        "activityName": "",
                        "assignmentTitle": "备用乙",
                        "siteId": "-1",
                        "siteName": "课程二",
                        "beginTime": "2026-11-12 08:00",
                        "endTime": null
                    }
                ]
            }
        });
        let mut transport = scripted(vec![("GET", undone_url(), json_response(200, body))]);
        let overview = fetch_undone(&mut transport, &session()).expect("undone fallbacks");

        assert_eq!(overview.total_sites, 2);
        assert_eq!(overview.undone_count, 2, "undoneNum defaults to item count");
        assert_eq!(overview.items[0].activity_id, "fallback-1");
        assert_eq!(overview.items[0].activity_name, "备用标题");
        assert_eq!(overview.items[0].site_id, None, "zero siteId is absent");
        assert_eq!(overview.items[0].site_name, "");
        assert_eq!(overview.items[0].begin_time, None);
        assert_eq!(overview.items[0].end_time, None);
        assert_eq!(overview.items[1].activity_id, "fallback-2");
        assert_eq!(overview.items[1].activity_name, "备用乙");
        assert_eq!(overview.items[1].site_id.as_deref(), Some("-1"));
        assert_eq!(
            overview.items[1].end_time, None,
            "course enrichment must not invent a deadline"
        );
    }

    #[test]
    fn undone_cli_output_sorts_deduplicates_and_preserves_null_values() {
        let overview = parse_undone(&json!({"undoneList": [
            {"activityId": "2", "activityName": "Unknown"},
            {"activityId": "1", "activityName": "Due", "beginTime": "2026-11-16 09:00", "endTime": "2026-11-18 10:30:00.000"},
            {"activityId": "1", "activityName": "Duplicate"}
        ]})).unwrap();
        let now = local_timestamp("2026-11-18 11:00:00").unwrap();
        let result = undone_json(overview, 0, now);
        assert_eq!(result["count"], 2);
        assert_eq!(result["method"], "undone");
        assert_eq!(result["assignments"][0]["title"], "Due");
        assert_eq!(
            result["assignments"][0]["published_at"],
            "2026-11-16 09:00:00"
        );
        assert_eq!(result["assignments"][0]["hours_remaining"], -0.5);
        assert_eq!(result["assignments"][0]["status"], "overdue");
        assert!(result["assignments"][1]["hours_remaining"].is_null());
        assert_eq!(result["assignments"][1]["status"], "pending");
    }

    #[test]
    fn empty_undone_is_success_and_not_an_auth_error() {
        let mut transport = scripted(vec![(
            "GET",
            undone_url(),
            fixture_response("homework_undone_empty.json", 0),
        )]);
        let overview = fetch_undone(&mut transport, &session()).expect("empty undone overview");

        assert_eq!(overview.total_sites, 0);
        assert_eq!(overview.undone_count, 0);
        assert!(overview.items.is_empty());
        assert_single_call(&transport, "GET", &undone_url());
    }

    #[test]
    fn unexpected_undone_payloads_are_parse_errors_without_retry() {
        let bodies = [
            json!({"code": 200, "data": []}),
            json!({"code": 200, "data": {}}),
            json!({"code": 200, "data": {"undoneList": [7]}}),
        ];
        for body in bodies {
            let mut transport = scripted(vec![(
                "GET",
                undone_url(),
                json_response(200, body.clone()),
            )]);
            let error = fetch_undone(&mut transport, &session())
                .expect_err("malformed undone payload must fail");
            assert_eq!(error.code, 5, "payload: {body}");
            assert_eq!(transport.calls.len(), 1);
        }
    }

    #[test]
    fn undone_without_user_id_fails_before_transport() {
        let mut transport = scripted(Vec::new());
        let session = Session {
            access_token: "synthetic-access-token".into(),
            user_id: String::new(),
        };
        let error = fetch_undone(&mut transport, &session).expect_err("missing user id");

        assert_eq!(error.code, 3);
        assert!(transport.calls.is_empty());
    }

    fn course_url_sized(page: u32, size: usize) -> String {
        format!("{UCLOUD_API}{COURSES_PATH}?size={size}&current={page}")
    }

    fn homework_list_url() -> String {
        format!("{UCLOUD_API}{LIST_PATH}")
    }

    fn homework_page(records: Value, total: Option<u64>) -> CliResult<Response> {
        let mut data = json!({"records": records});
        if let Some(total) = total {
            data["total"] = json!(total);
        }
        json_response(200, json!({"code": 200, "data": data}))
    }

    fn course_page(records: Value, total: Option<u64>) -> CliResult<Response> {
        let mut data = json!({"records": records});
        if let Some(total) = total {
            data["total"] = json!(total);
        }
        json_response(200, json!({"code": 200, "data": data}))
    }

    fn list_record(index: usize) -> Value {
        json!({
            "activityId": format!("hw-{index:02}"),
            "assignmentTitle": format!("作业 {index:02}"),
            "assignmentStatus": 99,
            "assignmentEndTime": "2026-12-01 23:59",
        })
    }

    fn homework_items() -> Vec<HomeworkItem> {
        let data = json!({
            "records": [
                {
                    "activityId": "hw-submitted",
                    "assignmentTitle": "已交作业",
                    "siteName": "课程一",
                    "assignmentStatus": 0,
                    "assignmentBeginTime": "2026-11-01 08:00",
                    "assignmentEndTime": "2026-11-20 23:59",
                    "submitTime": "2026-11-15 10:00",
                    "score": 88
                },
                {
                    "activityId": "hw-past",
                    "assignmentTitle": "已过期作业",
                    "siteName": "课程一",
                    "assignmentStatus": 99,
                    "assignmentEndTime": "2026-11-01 23:59"
                },
                {
                    "activityId": "hw-no-deadline",
                    "assignmentTitle": "无截止时间",
                    "siteName": "课程一",
                    "assignmentStatus": 99
                },
                {
                    "activityId": "hw-late",
                    "assignmentTitle": "待完成作业",
                    "siteName": "课程一",
                    "assignmentStatus": 99,
                    "assignmentEndTime": "2026-11-30 23:59"
                }
            ],
            "total": 4
        });
        parse_homework_page(&data, "site-1")
            .expect("synthetic homework page")
            .items
    }

    fn now() -> time::PrimitiveDateTime {
        local_timestamp("2026-11-18 12:00:00").expect("synthetic clock")
    }

    #[test]
    fn homework_all_paginates_course_and_student_pages() {
        let first: Vec<Value> = (0..50).map(list_record).collect();
        let second: Vec<Value> = (50..60).map(list_record).collect();
        let mut transport = scripted(vec![
            (
                "GET",
                course_url_sized(1, HOMEWORK_COURSE_PAGE_SIZE),
                course_page(json!([{"siteId": "site-1", "siteName": "课程一"}]), Some(1)),
            ),
            (
                "POST",
                homework_list_url(),
                homework_page(json!(first), Some(60)),
            ),
            (
                "POST",
                homework_list_url(),
                homework_page(json!(second), Some(60)),
            ),
        ]);
        let items = fetch_all_homework(&mut transport, &session()).expect("all homework");

        assert_eq!(items.len(), 60);
        assert_eq!(items[0].site_id.as_deref(), Some("site-1"));
        assert_eq!(items[0].site_name, "课程一");
        assert_eq!(transport.calls.len(), 3);
        let first_body: Value =
            serde_json::from_slice(&transport.calls[1].body).expect("first page body");
        assert_eq!(
            first_body,
            json!({
                "siteId": "site-1",
                "userId": "synthetic-user",
                "current": 1,
                "size": LIST_PAGE_SIZE,
            })
        );
        let second_body: Value =
            serde_json::from_slice(&transport.calls[2].body).expect("second page body");
        assert_eq!(second_body["current"], 2);
    }

    #[test]
    fn homework_course_list_paginates_until_total() {
        let first: Vec<Value> = (0..50).map(list_record).collect();
        let second: Vec<Value> = (50..55).map(list_record).collect();
        let mut transport = scripted(vec![
            (
                "POST",
                homework_list_url(),
                homework_page(json!(first), Some(55)),
            ),
            (
                "POST",
                homework_list_url(),
                homework_page(json!(second), Some(55)),
            ),
        ]);
        let items = fetch_course_homework(&mut transport, &session(), "site-1")
            .expect("course homework pages");

        assert_eq!(items.len(), 55);
        assert_eq!(transport.calls.len(), 2);
        assert_eq!(transport.calls[0].method, "POST");
        assert_eq!(transport.calls[0].url, homework_list_url());
    }

    #[test]
    fn homework_course_list_short_page_stops_without_a_second_request() {
        let records = json!([
            {"activityId": "hw-1", "assignmentTitle": "一", "assignmentStatus": 99},
            {"activityId": "hw-2", "assignmentTitle": "二", "assignmentStatus": 99}
        ]);
        let mut transport = scripted(vec![(
            "POST",
            homework_list_url(),
            homework_page(records, None),
        )]);
        let items =
            fetch_course_homework(&mut transport, &session(), "site-1").expect("short course list");

        assert_eq!(items.len(), 2);
        assert_eq!(transport.calls.len(), 1);
    }

    #[test]
    fn homework_course_list_repeated_page_is_a_parse_error() {
        let records: Vec<Value> = (0..50).map(list_record).collect();
        let mut transport = scripted(vec![
            (
                "POST",
                homework_list_url(),
                homework_page(json!(records), Some(100)),
            ),
            (
                "POST",
                homework_list_url(),
                homework_page(json!(records), Some(100)),
            ),
        ]);
        let error = fetch_course_homework(&mut transport, &session(), "site-1")
            .expect_err("repeated page must fail");

        assert_eq!(error.code, 5);
        assert_eq!(transport.calls.len(), 2);
    }

    #[test]
    fn homework_course_walk_paginates_by_total_and_stops_at_thirty_sites() {
        let first: Vec<Value> = (0..20)
            .map(|index| json!({"siteId": format!("site-{index:02}"), "siteName": "课程"}))
            .collect();
        let mut transport = scripted(vec![
            (
                "GET",
                course_url_sized(1, HOMEWORK_COURSE_PAGE_SIZE),
                course_page(json!(first), Some(21)),
            ),
            (
                "GET",
                course_url_sized(2, HOMEWORK_COURSE_PAGE_SIZE),
                course_page(json!([{"siteId": "site-20", "siteName": "课程"}]), Some(21)),
            ),
        ]);
        let courses =
            fetch_all_courses_for_homework(&mut transport, &session()).expect("course walk");

        assert_eq!(courses.len(), 21);
        assert_eq!(courses.last().unwrap().site_id, "site-20");
        assert_eq!(transport.calls.len(), 2);
        assert_eq!(transport.calls[0].url, course_url_sized(1, 30));

        let first_page: Vec<Value> = (0..20)
            .map(|index| json!({"siteId": format!("site-{index:02}"), "siteName": "课程"}))
            .collect();
        let second_page: Vec<Value> = (20..40)
            .map(|index| json!({"siteId": format!("site-{index:02}"), "siteName": "课程"}))
            .collect();
        let mut capped = scripted(vec![
            (
                "GET",
                course_url_sized(1, HOMEWORK_COURSE_PAGE_SIZE),
                course_page(json!(first_page), Some(100)),
            ),
            (
                "GET",
                course_url_sized(2, HOMEWORK_COURSE_PAGE_SIZE),
                course_page(json!(second_page), Some(100)),
            ),
        ]);
        let courses =
            fetch_all_courses_for_homework(&mut capped, &session()).expect("capped course walk");

        assert_eq!(courses.len(), HOMEWORK_SITE_LIMIT);
        assert_eq!(capped.calls.len(), 2);
    }

    #[test]
    fn homework_course_walk_repeated_page_is_a_parse_error() {
        let records: Vec<Value> = (0..20)
            .map(|index| json!({"siteId": format!("site-{index:02}"), "siteName": "课程"}))
            .collect();
        let mut transport = scripted(vec![
            (
                "GET",
                course_url_sized(1, HOMEWORK_COURSE_PAGE_SIZE),
                course_page(json!(records), Some(40)),
            ),
            (
                "GET",
                course_url_sized(2, HOMEWORK_COURSE_PAGE_SIZE),
                course_page(json!(records), Some(40)),
            ),
        ]);
        let error = fetch_all_courses_for_homework(&mut transport, &session())
            .expect_err("repeated course page must fail");

        assert_eq!(error.code, 5);
        assert_eq!(transport.calls.len(), 2);
    }

    #[test]
    fn homework_all_skips_courses_without_a_site_id() {
        let mut transport = scripted(vec![
            (
                "GET",
                course_url_sized(1, HOMEWORK_COURSE_PAGE_SIZE),
                course_page(
                    json!([
                        {"siteId": "", "siteName": "无编号课程"},
                        {"siteId": "site-1", "siteName": "课程一"}
                    ]),
                    Some(2),
                ),
            ),
            (
                "POST",
                homework_list_url(),
                homework_page(
                    json!([{"activityId": "hw-1", "assignmentTitle": "作业"}]),
                    Some(1),
                ),
            ),
        ]);
        let items = fetch_all_homework(&mut transport, &session()).expect("all homework");

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].site_name, "课程一");
        assert_eq!(transport.calls.len(), 2);
        assert_eq!(transport.calls[1].method, "POST");
    }

    #[test]
    fn homework_page_mapping_matches_python_status_and_fallback_rules() {
        let data = json!({
            "records": [
                {
                    "activityId": "hw-submitted",
                    "assignmentTitle": "已交",
                    "assignmentStatus": 0,
                    "submitTime": "2026-11-15 10:00",
                    "score": 88,
                    "assignmentBeginTime": "2026-11-01 08:00",
                    "assignmentEndTime": "2026-11-20 23:59"
                },
                {
                    "activityId": "",
                    "id": "fallback-id",
                    "activityName": "备用标题",
                    "status": "SUBMITTED",
                    "siteName": false,
                    "beginTime": "",
                    "endTime": null
                },
                {
                    "activityId": "hw-bool",
                    "assignmentTitle": "布尔状态",
                    "assignmentStatus": false,
                    "submitTime": ""
                }
            ],
            "total": 3
        });
        let page = parse_homework_page(&data, "site-1").expect("mapped homework page");

        assert_eq!(page.record_count, 3);
        assert!(page.items[0].is_submitted);
        assert_eq!(
            page.items[0].submit_time.as_deref(),
            Some("2026-11-15 10:00")
        );
        assert_eq!(page.items[0].score, Some(88.0));
        assert_eq!(
            page.items[0].begin_time.as_deref(),
            Some("2026-11-01 08:00")
        );
        assert_eq!(page.items[0].end_time.as_deref(), Some("2026-11-20 23:59"));
        assert_eq!(page.items[1].activity_id, "fallback-id");
        assert_eq!(page.items[1].activity_name, "备用标题");
        assert!(page.items[1].is_submitted);
        assert_eq!(page.items[1].site_name, "");
        assert_eq!(page.items[1].begin_time, None);
        assert_eq!(page.items[1].end_time, None);
        assert!(!page.items[2].is_submitted);
        assert_eq!(page.items[2].activity_id, "hw-bool");
    }

    #[test]
    fn homework_list_output_sorts_filters_and_limits_like_python() {
        let now = now();
        let result = homework_list_json(
            homework_items(),
            "all",
            ListFilters {
                undone: false,
                course: None,
                new_days: None,
            },
            0,
            now,
        );

        assert_eq!(result["count"], 4);
        assert_eq!(result["method"], "all");
        assert_eq!(result["filters"]["undone"], false);
        assert_eq!(result["filters"]["course"], Value::Null);
        assert_eq!(result["filters"]["new_days"], Value::Null);
        let assignments = result["assignments"].as_array().expect("assignments");
        let ids: Vec<&str> = assignments
            .iter()
            .map(|item| item["id"].as_str().expect("assignment id"))
            .collect();
        assert_eq!(
            ids,
            ["hw-past", "hw-submitted", "hw-late", "hw-no-deadline"]
        );

        let submitted = &assignments[1];
        assert_eq!(submitted["status"], "submitted");
        assert_eq!(submitted["submitted"], true);
        assert_eq!(submitted["submitted_at"], "2026-11-15 10:00");
        assert_eq!(submitted["published_at"], "2026-11-01 08:00:00");
        assert_eq!(submitted["score"], 88.0);
        assert_eq!(submitted["course"], "课程一");
        assert_eq!(submitted["course_id"], "site-1");
        assert_eq!(submitted["overdue"], false);

        let overdue = &assignments[0];
        assert_eq!(overdue["status"], "overdue");
        assert_eq!(overdue["overdue"], true);
        assert!(
            overdue["hours_remaining"]
                .as_f64()
                .is_some_and(|hours| hours < 0.0)
        );

        let undated = &assignments[3];
        assert_eq!(undated["deadline"], Value::Null);
        assert_eq!(undated["published_at"], Value::Null);
        assert_eq!(undated["hours_remaining"], Value::Null);
        assert_eq!(undated["overdue"], false);
        assert_eq!(undated["status"], "pending");

        let filtered = homework_list_json(
            homework_items(),
            "course",
            ListFilters {
                undone: true,
                course: Some("site-1".into()),
                new_days: None,
            },
            1,
            now,
        );
        assert_eq!(filtered["count"], 1);
        assert_eq!(filtered["method"], "course");
        assert_eq!(filtered["filters"]["course"], "site-1");
        assert_eq!(filtered["filters"]["undone"], true);
        assert_eq!(filtered["assignments"][0]["id"], "hw-past");
    }

    #[test]
    fn homework_list_deduplicates_nonempty_ids_and_keeps_empty_ids() {
        fn item(id: &str, title: &str) -> HomeworkItem {
            HomeworkItem {
                activity_id: id.into(),
                activity_name: title.into(),
                site_id: Some("site-1".into()),
                site_name: "课程一".into(),
                begin_time: None,
                end_time: None,
                submit_time: None,
                is_submitted: false,
                score: None,
                raw: Map::new(),
            }
        }

        let result = homework_list_json(
            vec![
                item("dup", "甲"),
                item("", "空一"),
                item("", "空二"),
                item("dup", "乙"),
            ],
            "all",
            ListFilters {
                undone: false,
                course: None,
                new_days: None,
            },
            0,
            now(),
        );

        assert_eq!(result["count"], 3);
        let ids: Vec<&str> = result["assignments"]
            .as_array()
            .expect("assignments")
            .iter()
            .map(|item| item["id"].as_str().expect("assignment id"))
            .collect();
        assert_eq!(ids, ["dup", "", ""]);
    }

    #[test]
    fn homework_list_without_user_id_fails_before_transport() {
        let mut transport = scripted(Vec::new());
        let session = Session::new("synthetic-access-token".into(), String::new());

        let error =
            fetch_course_homework(&mut transport, &session, "site-1").expect_err("missing user id");
        assert_eq!(error.code, 3);
        assert!(transport.calls.is_empty());

        let error = fetch_all_homework(&mut transport, &session).expect_err("missing user id");
        assert_eq!(error.code, 3);
        assert!(transport.calls.is_empty());
    }

    #[test]
    fn homework_list_auth_and_malformed_pages_fail_without_retry() {
        let mut transport = scripted(vec![(
            "POST",
            homework_list_url(),
            json_response(401, json!({"msg": "synthetic unauthorized"})),
        )]);
        let error = fetch_course_homework(&mut transport, &session(), "site-1")
            .expect_err("unauthorized list");
        assert_eq!(error.code, 3);
        assert_eq!(transport.calls.len(), 1);

        for data in [
            json!([]),
            json!({"records": "not-an-array"}),
            json!({"records": [7]}),
            json!({"records": [], "total": "not-a-number"}),
        ] {
            let mut transport = scripted(vec![(
                "POST",
                homework_list_url(),
                json_response(200, json!({"code": 200, "data": data})),
            )]);
            let error = fetch_course_homework(&mut transport, &session(), "site-1")
                .expect_err("malformed list page");
            assert_eq!(error.code, 5, "payload: {data}");
            assert_eq!(transport.calls.len(), 1);
        }
    }
}
