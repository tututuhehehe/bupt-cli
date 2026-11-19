//! `homework show` read-only assembly.
//!
//! The CLI still stops at the shared session seam before a real request. Tests
//! inject a synthetic session and `Scripted` transport, then assert the exact
//! read-only request journal.

use super::detail::{self, Attachment, Detail};
use super::read::Session;
use super::text;
use crate::cli::{CliError, CliResult, Context, Outcome};
use crate::transport::Transport;
use clap::ArgMatches;
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};

const SUBMIT_VIEW_ERROR_LIMIT: usize = 200;

pub(super) fn run(matches: &ArgMatches, context: &mut Context<'_>) -> CliResult<Outcome> {
    let assignment_id = matches.get_one::<String>("id").cloned().unwrap_or_default();
    let course_id = matches.get_one::<String>("course").map(String::as_str);
    let session = super::load_read_session(context)?;
    execute_show(&assignment_id, course_id, &session, context.transport)
}

pub(super) fn execute_show(
    assignment_id: &str,
    course_id: Option<&str>,
    session: &Session,
    transport: &mut dyn Transport,
) -> CliResult<Outcome> {
    let secrets = [session.access_token.as_str()];
    execute_show_inner(assignment_id, course_id, session, transport)
        .map_err(|error| redact_error(error, &secrets))
}

fn execute_show_inner(
    assignment_id: &str,
    course_id: Option<&str>,
    session: &Session,
    transport: &mut dyn Transport,
) -> CliResult<Outcome> {
    let detail = detail::fetch_detail(transport, session, assignment_id)?;
    let mut attachments = detail.attachments.clone();
    detail::resolve_attachment_urls(transport, session, &mut attachments, false);

    let mut submitted = detail.submitted;
    let mut submit_content = detail.submit_content.clone();
    let mut submit_attachments = detail.submit_attachments.clone();
    let mut score = detail.score;
    let mut teacher_comment = detail.teacher_comment.clone();
    let mut submit_view_error = None;

    match detail::fetch_submit_view(transport, session, assignment_id) {
        Ok(submit_view) => {
            if submit_view
                .as_object()
                .is_some_and(|object| !object.is_empty())
            {
                submitted = detail::submit_view_has_submission(&submit_view);
                if submitted {
                    let content = text::html_to_text(
                        submit_view
                            .get("assignmentContent")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    );
                    submit_content = (!content.is_empty()).then_some(content);
                    submit_attachments = detail::submit_view_attachments(&submit_view);
                    detail::resolve_attachment_urls(
                        transport,
                        session,
                        &mut submit_attachments,
                        true,
                    );
                    if let Some(parsed) = submit_view_score(submit_view.get("score")) {
                        score = Some(parsed);
                    }
                    if let Some(evaluation) = submit_view
                        .get("evaluation")
                        .and_then(Value::as_str)
                        .filter(|value| !value.trim().is_empty())
                    {
                        teacher_comment = Some(evaluation.to_owned());
                    }
                }
            } else {
                submit_view_error = Some("submit-view returned no data".to_owned());
            }
        }
        Err(error) => {
            let message = text::one_line(&error.message);
            let message = if message.is_empty() {
                "submit-view failed".to_owned()
            } else {
                message
            };
            let redacted = text::redact(message, &[session.access_token.as_str()]);
            submit_view_error = Some(text::truncate_chars(&redacted, SUBMIT_VIEW_ERROR_LIMIT));
        }
    }

    let submitted_at = if submitted {
        lookup_submitted_at(transport, session, assignment_id, course_id, &detail)
    } else {
        submit_content = None;
        submit_attachments.clear();
        None
    };

    let end_time = detail.raw.get("assignmentEndTime").cloned();
    let mut assignment = json!({
        "id": detail.assignment_id,
        "title": detail.title,
        "course": detail::show_course_name(&detail),
        "published_at": detail
            .raw
            .get("assignmentBeginTime")
            .and_then(parse_datetime)
            .map(|parsed| parsed.normalized),
        "deadline": end_time,
        "status": status_for(submitted, detail.raw.get("assignmentEndTime")),
        "content_text": detail.content_text,
        "content_truncated": false,
        "attachments": attachments.iter().map(Attachment::as_json).collect::<Vec<_>>(),
        "submitted": submitted,
        "submit_content": submit_content,
        "submit_attachments": submit_attachments
            .iter()
            .map(Attachment::as_json)
            .collect::<Vec<_>>(),
        "score": score,
        "teacher_comment": teacher_comment,
        "submitted_at": submitted_at,
    });
    if let Some(error) = submit_view_error {
        assignment["submit_view_error"] = Value::String(error);
    }
    Ok(Outcome {
        data: json!({"assignment": assignment}),
        code: 0,
        diagnostic: None,
    })
}

fn lookup_submitted_at(
    transport: &mut dyn Transport,
    session: &Session,
    assignment_id: &str,
    course_id: Option<&str>,
    detail: &Detail,
) -> Option<String> {
    let site_ids = match course_id {
        Some(course_id) => vec![course_id.to_owned()],
        None => {
            let target = detail::target_site_name(detail);
            detail::course_site_ids(transport, session, target.as_deref()).ok()?
        }
    };
    if site_ids.is_empty() {
        return None;
    }
    detail::find_list_record(transport, session, assignment_id, &site_ids)
        .ok()
        .flatten()
        .and_then(|record| record.submit_time)
}

fn submit_view_score(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    if matches!(value, Value::Bool(_)) {
        return None;
    }
    let score = match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) if !text.trim().is_empty() => text.trim().parse().ok(),
        _ => None,
    }?;
    (score >= 0.0).then_some(score)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedDateTime {
    normalized: String,
    epoch_seconds: i64,
}

fn parse_datetime(value: &Value) -> Option<ParsedDateTime> {
    let text = value.as_str()?.trim().replace('/', "-");
    if text.is_empty() || matches!(text.as_str(), "0" | "-" | "null" | "None") {
        return None;
    }
    let text = text.strip_suffix('Z').unwrap_or(&text);
    let (date, clock) = text
        .split_once('T')
        .or_else(|| text.split_once(' '))
        .map_or((text, "00:00:00"), |(date, clock)| (date, clock));
    let (year, month, day) = parse_date(date)?;
    let (hour, minute, second) = parse_clock(clock)?;
    let epoch_seconds = days_from_civil(year, month, day) * 86_400
        + i64::from(hour) * 3_600
        + i64::from(minute) * 60
        + i64::from(second);
    Some(ParsedDateTime {
        normalized: format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}"),
        epoch_seconds,
    })
}

fn parse_date(value: &str) -> Option<(i32, u32, u32)> {
    let mut parts = value.split('-');
    let year = parts.next()?.parse::<i32>().ok()?;
    let month = parts.next()?.parse::<u32>().ok()?;
    let day = parts.next()?.parse::<u32>().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) {
        return None;
    }
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => return None,
    };
    (1..=days).contains(&day).then_some((year, month, day))
}

fn parse_clock(value: &str) -> Option<(u32, u32, u32)> {
    let value = value.split_once('.').map_or(value, |(clock, _)| clock);
    let value = value.split(['+', '-']).next().unwrap_or(value);
    let mut parts = value.split(':');
    let hour = parts.next()?.parse::<u32>().ok()?;
    let minute = parts.next()?.parse::<u32>().ok()?;
    let second = parts
        .next()
        .map_or(Some(0), |value| value.parse::<u32>().ok())?;
    if parts.next().is_some() || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    Some((hour, minute, second))
}

fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let year = i64::from(year) - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day = i64::from(day);
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn status_for(submitted: bool, end_time: Option<&Value>) -> &'static str {
    if submitted {
        return "submitted";
    }
    if end_time
        .and_then(parse_datetime)
        .is_some_and(|parsed| parsed.epoch_seconds < now_epoch_seconds())
    {
        return "overdue";
    }
    "pending"
}

fn now_epoch_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64)
}

fn redact_error(mut error: CliError, secrets: &[&str]) -> CliError {
    error.message = text::redact(error.message, secrets);
    error.hint = text::redact(error.hint, secrets);
    error
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::Response;
    use crate::transport::testing::Scripted;
    use serde_json::json;
    use std::collections::VecDeque;

    const ASSIGNMENT: &str = "1000000000000000001";
    const TOKEN: &str = "synthetic-access-token";
    const API: &str = "https://apiucloud.bupt.edu.cn";

    fn session() -> Session {
        Session {
            access_token: TOKEN.into(),
            user_id: "synthetic-user".into(),
        }
    }

    fn response(status: u16, body: Value) -> CliResult<Response> {
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

    fn detail_url() -> String {
        format!("{API}/ykt-site/work/detail?assignmentId={ASSIGNMENT}")
    }

    fn submit_view_url() -> String {
        format!("{API}/ykt-site/work/submit-view?assignmentId={ASSIGNMENT}")
    }

    fn preview_url(resource_id: &str) -> String {
        format!("{API}/blade-source/resource/preview-url?resourceId={resource_id}")
    }

    fn courses_url() -> String {
        format!("{API}/ykt-site/site/list/student/current?size=50&current=1")
    }

    fn list_url() -> String {
        format!("{API}/ykt-site/work/student/list")
    }

    fn assert_no_writes(transport: &Scripted) {
        for call in &transport.calls {
            let allowed = match call.method {
                "GET" => true,
                "POST" => call.url.ends_with("/ykt-site/work/student/list"),
                _ => false,
            };
            assert!(
                allowed,
                "show must stay read-only: {} {}",
                call.method, call.url
            );
        }
    }

    #[test]
    fn submitted_show_merges_detail_submit_view_and_student_list() {
        let mut transport = scripted(vec![
            (
                "GET",
                detail_url(),
                response(
                    200,
                    json!({"code": 200, "data": {
                        "assignmentTitle": "作业一",
                        "className": "课程一",
                        "assignmentBeginTime": "2026-10-21 00:00",
                        "assignmentEndTime": "2099-10-26 23:59",
                        "assignmentStatus": 0,
                        "assignmentContent": "<p>见附件</p>",
                        "assignmentResource": [{
                            "resourceId": "teacher-rid",
                            "resourceName": "作业要求.pdf"
                        }]
                    }}),
                ),
            ),
            (
                "GET",
                preview_url("teacher-rid"),
                response(
                    200,
                    json!({"code": 200, "data": {
                        "previewUrl": "https://files.invalid/teacher.pdf?response-content-disposition=attachment%3Bfilename%3D%E4%BD%9C%E4%B8%9A%E8%A6%81%E6%B1%82.pdf"
                    }}),
                ),
            ),
            (
                "GET",
                submit_view_url(),
                response(
                    200,
                    json!({"code": 200, "data": {
                        "assignmentId": ASSIGNMENT,
                        "assignmentContent": "<p>答案正文</p>",
                        "attachmentIds": ["student-rid"],
                        "score": 95,
                        "evaluation": "做得不错",
                        "assignmentStatus": 0
                    }}),
                ),
            ),
            (
                "GET",
                preview_url("student-rid"),
                response(
                    200,
                    json!({"code": 200, "data": {
                        "previewUrl": "https://files.invalid/student.pdf?response-content-disposition=attachment%3Bfilename%3D%E7%AD%94%E6%A1%88.pdf"
                    }}),
                ),
            ),
            (
                "GET",
                courses_url(),
                response(
                    200,
                    json!({"code": 200, "data": {
                        "records": [{"siteId": "site-1", "siteName": "课程一"}],
                        "total": 1
                    }}),
                ),
            ),
            (
                "POST",
                list_url(),
                response(
                    200,
                    json!({"code": 200, "data": {
                        "records": [{
                            "activityId": ASSIGNMENT,
                            "assignmentStatus": 0,
                            "submitTime": "2026-10-22 16:26"
                        }],
                        "total": 1
                    }}),
                ),
            ),
        ]);
        let outcome = execute_show(ASSIGNMENT, None, &session(), &mut transport)
            .expect("show submitted assignment");
        let assignment = &outcome.data["assignment"];

        assert_eq!(assignment["id"], ASSIGNMENT);
        assert_eq!(assignment["title"], "作业一");
        assert_eq!(assignment["course"], "课程一");
        assert_eq!(assignment["published_at"], "2026-10-21 00:00:00");
        assert_eq!(assignment["status"], "submitted");
        assert_eq!(assignment["content_text"], "见附件");
        assert_eq!(assignment["submit_content"], "答案正文");
        assert_eq!(assignment["submitted"], true);
        assert_eq!(assignment["submitted_at"], "2026-10-22 16:26");
        assert_eq!(assignment["score"], 95.0);
        assert_eq!(assignment["teacher_comment"], "做得不错");
        assert_eq!(assignment["attachments"][0]["title"], "作业要求.pdf");
        assert_eq!(assignment["submit_attachments"][0]["title"], "答案.pdf");
        assert_eq!(
            assignment["submit_attachments"][0]["resource_id"],
            "student-rid"
        );
        assert!(assignment.get("submit_view_error").is_none());
        assert_eq!(transport.calls.len(), 6);
        assert_no_writes(&transport);
    }

    #[test]
    fn not_submitted_show_does_not_fabricate_status_or_submission_fields() {
        let mut transport = scripted(vec![
            (
                "GET",
                detail_url(),
                response(
                    200,
                    json!({"code": 200, "data": {
                        "assignmentTitle": "未交作业",
                        "className": "课程二",
                        "assignmentEndTime": "2099-12-01 23:59",
                        "assignmentContent": "<p>正文</p>"
                    }}),
                ),
            ),
            (
                "GET",
                submit_view_url(),
                response(
                    200,
                    json!({"code": 200, "data": {
                        "assignmentId": -1,
                        "attachmentIds": [],
                        "assignmentContent": ""
                    }}),
                ),
            ),
        ]);
        let outcome =
            execute_show(ASSIGNMENT, None, &session(), &mut transport).expect("show pending");
        let assignment = &outcome.data["assignment"];

        assert_eq!(assignment["submitted"], false);
        assert_eq!(assignment["status"], "pending");
        assert_eq!(assignment["submit_content"], Value::Null);
        assert_eq!(assignment["submit_attachments"], json!([]));
        assert_eq!(assignment["submitted_at"], Value::Null);
        assert_eq!(assignment["attachments"], json!([]));
        assert!(assignment.get("submit_view_error").is_none());
        assert_eq!(transport.calls.len(), 2);
        assert_no_writes(&transport);
    }

    #[test]
    fn submit_view_failure_stays_redacted_and_bounded() {
        let long_error = format!("synthetic boom {TOKEN} {}", "x".repeat(300));
        let mut transport = scripted(vec![
            (
                "GET",
                detail_url(),
                response(
                    200,
                    json!({"code": 200, "data": {
                        "assignmentTitle": "作业一",
                        "assignmentStatus": 0,
                        "submitInfo": {"submitContent": "详情里的提交正文"}
                    }}),
                ),
            ),
            (
                "GET",
                submit_view_url(),
                Err(CliError::new(4, "network", long_error, "synthetic hint")),
            ),
            (
                "GET",
                courses_url(),
                response(
                    200,
                    json!({"code": 200, "data": {"records": [], "total": 0}}),
                ),
            ),
        ]);
        let outcome = execute_show(ASSIGNMENT, None, &session(), &mut transport)
            .expect("submit-view failure must not fail show");
        let assignment = &outcome.data["assignment"];
        let error = assignment["submit_view_error"]
            .as_str()
            .expect("degraded submit-view error");

        assert_eq!(assignment["submitted"], true);
        assert_eq!(assignment["submit_content"], "详情里的提交正文");
        assert!(!error.contains(TOKEN));
        assert!(error.chars().count() <= 200);
        assert_no_writes(&transport);
    }

    #[test]
    fn explicit_course_limits_the_list_lookup_without_fetching_courses() {
        let mut transport = scripted(vec![
            (
                "GET",
                detail_url(),
                response(
                    200,
                    json!({"code": 200, "data": {
                        "assignmentTitle": "作业一",
                        "assignmentStatus": 0
                    }}),
                ),
            ),
            (
                "GET",
                submit_view_url(),
                response(
                    200,
                    json!({"code": 200, "data": {
                        "assignmentId": ASSIGNMENT,
                        "attachmentIds": [],
                        "assignmentContent": "",
                        "assignmentStatus": 0
                    }}),
                ),
            ),
            (
                "POST",
                list_url(),
                response(
                    200,
                    json!({"code": 200, "data": {
                        "records": [{
                            "activityId": ASSIGNMENT,
                            "assignmentStatus": 0,
                            "submitTime": "2026-10-22 16:26"
                        }],
                        "total": 1
                    }}),
                ),
            ),
        ]);
        let outcome = execute_show(ASSIGNMENT, Some("site-9"), &session(), &mut transport)
            .expect("show with explicit course");
        let assignment = &outcome.data["assignment"];

        assert_eq!(assignment["submitted_at"], "2026-10-22 16:26");
        assert_eq!(transport.calls.len(), 3);
        assert_no_writes(&transport);
    }

    #[test]
    fn empty_submit_view_keeps_detail_fields_and_reports_no_data() {
        let mut transport = scripted(vec![
            (
                "GET",
                detail_url(),
                response(
                    200,
                    json!({"code": 200, "data": {
                        "assignmentTitle": "作业一",
                        "assignmentStatus": 0,
                        "submitInfo": {"submitContent": "详情里的提交正文"}
                    }}),
                ),
            ),
            (
                "GET",
                submit_view_url(),
                response(200, json!({"code": 200, "data": {}})),
            ),
            (
                "GET",
                courses_url(),
                response(
                    200,
                    json!({"code": 200, "data": {"records": [], "total": 0}}),
                ),
            ),
        ]);
        let outcome = execute_show(ASSIGNMENT, None, &session(), &mut transport)
            .expect("empty submit-view must not fail show");
        let assignment = &outcome.data["assignment"];

        assert_eq!(assignment["submitted"], true);
        assert_eq!(assignment["submit_content"], "详情里的提交正文");
        assert_eq!(
            assignment["submit_view_error"],
            "submit-view returned no data"
        );
        assert_eq!(assignment["submitted_at"], Value::Null);
        assert_eq!(transport.calls.len(), 3);
        assert_no_writes(&transport);
    }

    #[test]
    fn detail_http_401_is_auth_3_without_retry_or_write() {
        let mut transport = scripted(vec![(
            "GET",
            detail_url(),
            response(401, json!({"msg": format!("reject {TOKEN}")})),
        )]);
        let error = execute_show(ASSIGNMENT, None, &session(), &mut transport)
            .err()
            .expect("unauthorized detail");

        assert_eq!(error.code, 3);
        assert_eq!(error.kind, "auth");
        assert!(!error.message.contains(TOKEN));
        assert_eq!(transport.calls.len(), 1);
        assert_no_writes(&transport);
    }

    #[test]
    fn detail_transport_failure_is_network_4_without_retry_or_write() {
        let mut transport = scripted(vec![(
            "GET",
            detail_url(),
            Err(CliError::new(
                4,
                "network",
                "synthetic transport unavailable",
                "synthetic hint",
            )),
        )]);
        let error = execute_show(ASSIGNMENT, None, &session(), &mut transport)
            .err()
            .expect("transport failure");

        assert_eq!(error.code, 4);
        assert_eq!(error.kind, "network");
        assert_eq!(transport.calls.len(), 1);
        assert_no_writes(&transport);
    }

    #[test]
    fn detail_http_503_is_upstream_5_without_retry_or_write() {
        let mut transport = scripted(vec![(
            "GET",
            detail_url(),
            response(503, json!({"msg": format!("down {TOKEN}")})),
        )]);
        let error = execute_show(ASSIGNMENT, None, &session(), &mut transport)
            .err()
            .expect("upstream refusal");

        assert_eq!(error.code, 5);
        assert_eq!(error.kind, "upstream");
        assert_eq!(transport.calls.len(), 1);
        assert_no_writes(&transport);
    }

    #[test]
    fn datetime_and_status_helpers_do_not_invent_missing_values() {
        assert_eq!(
            parse_datetime(&json!("2026-10-21 00:00"))
                .expect("minute timestamp")
                .normalized,
            "2026-10-21 00:00:00"
        );
        assert_eq!(parse_datetime(&json!("not-a-date")), None);
        assert_eq!(status_for(false, Some(&json!("2099-01-01"))), "pending");
        assert_eq!(status_for(false, Some(&json!("2000-01-01"))), "overdue");
        assert_eq!(status_for(true, Some(&json!("2000-01-01"))), "submitted");
        assert_eq!(status_for(false, None), "pending");
    }
}
