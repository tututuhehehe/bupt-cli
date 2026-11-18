//! Read-only UCloud course paging and undone-homework mapping.
//!
//! This is an in-process scaffold: callers inject both the transport and a
//! synthetic session. The public CLI does not create a real session yet, so no
//! production credential, browser, or network path reaches these helpers.

use crate::cli::{CliError, CliResult};
use crate::transport::{Request, Transport};
use serde_json::{Map, Value};
use std::collections::HashSet;

const UCLOUD_API: &str = "https://apiucloud.bupt.edu.cn";
const COURSES_PATH: &str = "/ykt-site/site/list/student/current";
const UNDONE_PATH: &str = "/ykt-site/site/student/undone";
const COURSE_PAGE_SIZE: usize = 50;
const COURSE_PAGE_LIMIT: u32 = 10;

pub(super) struct Session {
    access_token: String,
    user_id: String,
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

#[derive(Debug, PartialEq, Eq)]
pub(super) struct UndoneItem {
    pub(super) activity_id: String,
    pub(super) activity_name: String,
    pub(super) site_id: Option<String>,
    pub(super) site_name: String,
    pub(super) begin_time: Option<String>,
    pub(super) end_time: Option<String>,
    pub(super) is_submitted: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct UndoneOverview {
    pub(super) total_sites: u64,
    pub(super) undone_count: u64,
    pub(super) items: Vec<UndoneItem>,
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
        let url = format!("{UCLOUD_API}{COURSES_PATH}?size={COURSE_PAGE_SIZE}&current={page}");
        let data = get_data(transport, session, "UCloud course list", url)?;
        let (batch, record_count) = parse_course_page(&data)?;
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

/// Fetch the pending-assignment overview. This maps only fields present in the
/// undone endpoint; it does not sweep course pages to fill in `-1`/blank course
/// metadata (the Python enrichment pass remains deferred).
pub(super) fn fetch_undone(
    transport: &mut dyn Transport,
    session: &Session,
) -> CliResult<UndoneOverview> {
    if session.user_id.is_empty() {
        return Err(CliError::new(
            3,
            "auth",
            "UCloud token is missing user_id",
            "Run `bupt login --force`, then retry.",
        ));
    }
    let url = query_url(UNDONE_PATH, &[("userId", session.user_id.as_str())]);
    let data = get_data(transport, session, "UCloud undone homework", url)?;
    parse_undone(&data)
}

fn get_data(
    transport: &mut dyn Transport,
    session: &Session,
    operation: &str,
    url: String,
) -> CliResult<Value> {
    let response = transport.send(Request {
        method: "GET",
        url,
        headers: auth_headers(&session.access_token),
        body: Vec::new(),
    })?;

    if response.status == 401 || response.status == 403 {
        return Err(CliError::new(
            3,
            "auth",
            format!("{operation} was rejected as unauthorized"),
            "Run `bupt login --force`, then retry.",
        ));
    }
    if !(200..300).contains(&response.status) {
        return Err(CliError::new(
            4,
            "network",
            format!("{operation} failed with HTTP {}", response.status),
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
    object
        .get("data")
        .cloned()
        .ok_or_else(|| CliError::new(5, "parse", format!("{operation} response missing data"), ""))
}

fn parse_course_page(data: &Value) -> CliResult<(Vec<Course>, usize)> {
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
            site_id: first_truthy_string(record, &["siteId", "id"]),
            site_name: first_truthy_string(record, &["siteName", "className"]),
            teacher_name: format_teacher_name(first_present(record, &["teacherName", "teachers"])),
        });
    }
    // `cmd_courses` does not use `total` to stop, but the Python parser still
    // rejects a malformed pagination total. Validate it for parity.
    let _ = pagination_total(object, "UCloud course list")?;
    Ok((courses, records.len()))
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
        items.push(UndoneItem {
            activity_id: first_truthy_string(record, &["activityId", "id"]),
            activity_name: first_truthy_string(record, &["activityName", "assignmentTitle"]),
            site_id: record
                .get("siteId")
                .filter(|value| is_truthy(value))
                .and_then(value_as_string),
            site_name: first_truthy_string(record, &["siteName"]),
            begin_time: record.get("beginTime").and_then(value_as_string),
            end_time: record.get("endTime").and_then(value_as_string),
            is_submitted: false,
        });
    }

    Ok(UndoneOverview {
        total_sites: count_value(object.get("siteNum"), 0),
        undone_count: count_value(object.get("undoneNum"), items.len() as u64),
        items,
    })
}

fn pagination_total(object: &Map<String, Value>, operation: &str) -> CliResult<Option<u64>> {
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

fn first_present<'a>(object: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a Value> {
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

fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_f64().is_some_and(|number| number != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(object) => !object.is_empty(),
    }
}

fn value_as_string(value: &Value) -> Option<String> {
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

fn auth_headers(access_token: &str) -> Vec<(String, String)> {
    vec![
        ("Blade-Auth".into(), access_token.to_owned()),
        ("Tenant-Id".into(), "000000".into()),
        (
            "Authorization".into(),
            "Basic c3dvcmQ6c3dvcmRfc2VjcmV0".into(),
        ),
    ]
}

fn query_url(path: &str, params: &[(&str, &str)]) -> String {
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
    fn upstream_course_error_is_a_single_network_failure() {
        let mut transport = scripted(vec![(
            "GET",
            course_url(1),
            json_response(
                200,
                json!({"code": 500, "msg": "synthetic upstream failure"}),
            ),
        )]);
        let error = fetch_courses(&mut transport, &session()).expect_err("upstream error");

        assert_eq!(error.code, 4);
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
}
