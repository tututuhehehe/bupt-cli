//! Read-only post-submit verification, ported from `bupt_cli.verify`.
//!
//! The algorithm is separated from the network reader so every branch can be
//! tested without a live campus request. The command path uses the same
//! detail/list readers as `homework show`; it never uploads or submits.

use super::detail::{self, ListRecord};
use super::read::Session;
use super::text;
use crate::cli::{CliError, CliResult, Context, Outcome};
use crate::transport::Transport;
use clap::ArgMatches;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::thread;
use std::time::Duration;

pub(super) const MANUAL_CHECK_HINT: &str = "upstream reported success but the submission could not be confirmed; check manually with `bupt homework show ID` or on ucloud.bupt.edu.cn. Do NOT resubmit blindly — every submit overwrites the previous one.";
pub(super) const VERIFY_ONLY_HINT: &str = "the submission could not be confirmed; check manually with `bupt homework show ID` or on ucloud.bupt.edu.cn. Do NOT resubmit blindly — every submit overwrites the previous one.";
const TEXT_LIMIT: usize = 200;
const ATTEMPTS: usize = 2;
const VERIFY_WAIT_SECONDS: f64 = 5.0;

pub(super) trait VerifyReader {
    fn fetch_submit_view(&mut self) -> CliResult<Value>;
    fn fetch_list_record(&mut self) -> CliResult<Option<ListRecord>>;
}

pub(super) fn run(matches: &ArgMatches, context: &mut Context<'_>) -> CliResult<Outcome> {
    let assignment_id = matches.get_one::<String>("id").cloned().unwrap_or_default();
    let expected_ids = matches
        .get_many::<String>("resource_id")
        .map(|values| values.cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let expected_text = matches.get_one::<String>("text").cloned();
    let session = super::load_session(context)?;
    execute_verify(
        &assignment_id,
        &expected_ids,
        expected_text.as_deref(),
        &session,
        context.transport,
    )
}

pub(super) fn execute_verify(
    assignment_id: &str,
    expected_ids: &[String],
    expected_text: Option<&str>,
    session: &Session,
    transport: &mut dyn Transport,
) -> CliResult<Outcome> {
    execute_verify_with(
        assignment_id,
        expected_ids,
        expected_text,
        session,
        transport,
        VERIFY_WAIT_SECONDS,
        &mut |seconds| thread::sleep(Duration::from_secs_f64(seconds)),
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn execute_verify_with(
    assignment_id: &str,
    expected_ids: &[String],
    expected_text: Option<&str>,
    session: &Session,
    transport: &mut dyn Transport,
    wait: f64,
    sleep: &mut dyn FnMut(f64),
) -> CliResult<Outcome> {
    let secrets = [session.access_token.as_str()];
    execute_verify_inner(
        assignment_id,
        expected_ids,
        expected_text,
        session,
        transport,
        wait,
        sleep,
        &secrets,
    )
    .map_err(|error| redact_error(error, &secrets))
}

#[allow(clippy::too_many_arguments)]
fn execute_verify_inner(
    assignment_id: &str,
    expected_ids: &[String],
    expected_text: Option<&str>,
    session: &Session,
    transport: &mut dyn Transport,
    wait: f64,
    sleep: &mut dyn FnMut(f64),
    secrets: &[&str],
) -> CliResult<Outcome> {
    // This detail request only narrows the course sites. Like Python
    // `_safe_target_name`, any failure is harmless and verification continues
    // with every enrolled site.
    let target_name = detail::fetch_detail(transport, session, assignment_id)
        .ok()
        .and_then(|detail| detail::target_site_name(&detail));
    let mut reader = NetworkReader {
        transport,
        session,
        target_name,
        assignment_id: assignment_id.to_owned(),
    };
    let compare_attachments = !expected_ids.is_empty();
    let mut verification = verify_submission(
        &mut reader,
        expected_ids,
        expected_text,
        compare_attachments,
        wait,
        sleep,
        secrets,
    );
    let status = verification
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("error");
    if matches!(status, "failed" | "error") {
        verification["hint"] = Value::String(VERIFY_ONLY_HINT.into());
    }
    verification_payload(verification)
}

struct NetworkReader<'a> {
    transport: &'a mut dyn Transport,
    session: &'a Session,
    target_name: Option<String>,
    assignment_id: String,
}

impl VerifyReader for NetworkReader<'_> {
    fn fetch_submit_view(&mut self) -> CliResult<Value> {
        let assignment_id = self.assignment_id.clone();
        detail::fetch_submit_view(self.transport, self.session, &assignment_id)
    }

    fn fetch_list_record(&mut self) -> CliResult<Option<ListRecord>> {
        let site_ids =
            detail::course_site_ids(self.transport, self.session, self.target_name.as_deref())?;
        let assignment_id = self.assignment_id.clone();
        detail::find_list_record(self.transport, self.session, &assignment_id, &site_ids)
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn verify_submission(
    reader: &mut dyn VerifyReader,
    expected_attachment_ids: &[String],
    expected_text: Option<&str>,
    compare_attachments: bool,
    wait: f64,
    sleep: &mut dyn FnMut(f64),
    secrets: &[&str],
) -> Value {
    let mut checks = json!({});
    let mut last_attempt = 0;
    for attempt in 1..=ATTEMPTS {
        if attempt > 1 {
            sleep(wait);
        }
        last_attempt = attempt;
        let submit_view = check_submit_view(
            reader,
            expected_attachment_ids,
            expected_text,
            compare_attachments,
            secrets,
        );
        let list = check_list(reader, secrets);
        let all_passed = submit_view["status"] == "passed" && list["status"] == "passed";
        checks = json!({
            "submit_view": submit_view,
            "list": list,
        });
        if all_passed {
            break;
        }
    }

    let mut failed = Vec::new();
    if checks["submit_view"]["status"] == "failed" {
        failed.push("submit_view");
    }
    if checks["list"]["status"] == "failed" {
        failed.push("list");
    }
    let errored = [
        ("submit_view", &checks["submit_view"]),
        ("list", &checks["list"]),
    ]
    .into_iter()
    .any(|(_, check)| check["status"] == "error");

    let status = if !failed.is_empty() {
        "failed"
    } else if errored {
        "error"
    } else {
        "passed"
    };
    let mut result = json!({
        "status": status,
        "attempts": last_attempt,
        "wait_seconds": wait,
        "checks": checks,
    });
    if status == "failed" {
        result["failed_checks"] = json!(failed);
        result["hint"] = Value::String(MANUAL_CHECK_HINT.into());
    } else if status == "error" {
        result["hint"] = Value::String(MANUAL_CHECK_HINT.into());
    }
    result
}

fn check_submit_view(
    reader: &mut dyn VerifyReader,
    expected_attachment_ids: &[String],
    expected_text: Option<&str>,
    compare_attachments: bool,
    secrets: &[&str],
) -> Value {
    let mut check = json!({
        "status": "failed",
        "expected_attachment_ids": expected_attachment_ids,
        "observed_attachment_ids": [],
        "has_submission": false,
        "content_checked": false,
    });
    let submit_view = match reader.fetch_submit_view() {
        Ok(submit_view) => submit_view,
        Err(error) => {
            check["status"] = Value::String("error".into());
            check["error"] = Value::String(error_text(&error, secrets));
            return check;
        }
    };
    let object = submit_view.as_object().cloned().unwrap_or_default();
    let has_submission = detail::submit_view_has_submission(&submit_view);
    let observed_ids = detail::submit_view_ids(&submit_view);
    let content_checked = expected_text.is_some_and(|text| !text.trim().is_empty());
    let (expected_normalized, observed_text) = if content_checked {
        let expected = text::normalize_text(expected_text.unwrap_or_default());
        let observed = text::normalize_text(&text::html_to_text(
            object
                .get("assignmentContent")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        ));
        (expected, observed)
    } else {
        (String::new(), String::new())
    };
    let attachments_match = if compare_attachments {
        set(&observed_ids) == set(expected_attachment_ids)
    } else {
        true
    };
    let passed = has_submission
        && attachments_match
        && (!content_checked || observed_text == expected_normalized);
    check["status"] = Value::String(if passed { "passed" } else { "failed" }.into());
    check["observed_attachment_ids"] = json!(observed_ids);
    check["has_submission"] = Value::Bool(has_submission);
    check["content_checked"] = Value::Bool(content_checked);
    check["attachments_checked"] = Value::Bool(compare_attachments);
    if content_checked {
        check["expected_text"] = Value::String(text::truncate_chars(
            expected_text.unwrap_or_default(),
            TEXT_LIMIT,
        ));
        check["observed_text"] = Value::String(text::truncate_chars(&observed_text, TEXT_LIMIT));
    }
    check
}

fn check_list(reader: &mut dyn VerifyReader, secrets: &[&str]) -> Value {
    let mut check = json!({
        "status": "failed",
        "found": false,
        "submitted": false,
        "submitted_at": Value::Null,
    });
    let record = match reader.fetch_list_record() {
        Ok(record) => record,
        Err(error) => {
            check["status"] = Value::String("error".into());
            check["error"] = Value::String(error_text(&error, secrets));
            return check;
        }
    };
    let Some(record) = record else {
        return check;
    };
    let submitted = record.submit_time.is_some() || record.submitted;
    check["status"] = Value::String(if submitted { "passed" } else { "failed" }.into());
    check["found"] = Value::Bool(true);
    check["submitted"] = Value::Bool(submitted);
    check["submitted_at"] = record.submit_time.map(Value::String).unwrap_or(Value::Null);
    check
}

fn set(values: &[String]) -> HashSet<&str> {
    values.iter().map(String::as_str).collect()
}

fn error_text(error: &CliError, secrets: &[&str]) -> String {
    let message = text::one_line(&error.message);
    let message = if message.is_empty() {
        "verification error".to_owned()
    } else {
        message
    };
    text::truncate_chars(&text::redact(message, secrets), TEXT_LIMIT)
}

fn verification_payload(verification: Value) -> CliResult<Outcome> {
    let status = verification
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("error");
    let submitted_at = verification
        .get("checks")
        .and_then(|checks| checks.get("list"))
        .and_then(|list| list.get("submitted_at"))
        .cloned()
        .unwrap_or(Value::Null);
    let mut payload = json!({
        "verified": Value::Null,
        "submitted_at": submitted_at,
        "verification": verification,
    });
    let (code, diagnostic) = match status {
        "passed" => {
            payload["verified"] = Value::Bool(true);
            (0, None)
        }
        "failed" => {
            payload["verified"] = Value::Bool(false);
            (
                6,
                payload["verification"]["hint"].as_str().map(str::to_owned),
            )
        }
        _ => {
            let hint = payload["verification"]["hint"]
                .as_str()
                .unwrap_or(MANUAL_CHECK_HINT)
                .to_owned();
            let mut summaries = Vec::new();
            if let Some(checks) = payload["verification"]["checks"].as_object() {
                for name in ["submit_view", "list"] {
                    let Some(check) = checks.get(name) else {
                        continue;
                    };
                    if check["status"] == "error"
                        && let Some(error) = check["error"].as_str()
                    {
                        summaries.push(format!("{name}: {error}"));
                    }
                }
            }
            let summary = if summaries.is_empty() {
                "verification could not run".to_owned()
            } else {
                summaries.join("; ")
            };
            payload["warning"] =
                Value::String(format!("verification could not run: {summary}. {hint}"));
            (7, Some(hint))
        }
    };
    Ok(Outcome {
        data: payload,
        code,
        diagnostic,
    })
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

    const ASSIGNMENT: &str = "1000000000000000005";
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

    fn courses_url() -> String {
        format!("{API}/ykt-site/site/list/student/current?size=50&current=1")
    }

    fn list_url() -> String {
        format!("{API}/ykt-site/work/student/list")
    }

    fn detail_response() -> CliResult<Response> {
        response(
            200,
            json!({"code": 200, "data": {"assignmentTitle": "确认提交", "className": "课程一"}}),
        )
    }

    fn submit_view_response(
        assignment_id: Value,
        attachment_ids: Value,
        content: &str,
    ) -> CliResult<Response> {
        response(
            200,
            json!({"code": 200, "data": {
                "assignmentId": assignment_id,
                "attachmentIds": attachment_ids,
                "assignmentContent": content,
                "assignmentStatus": 0
            }}),
        )
    }

    fn courses_response() -> CliResult<Response> {
        response(
            200,
            json!({"code": 200, "data": {
                "records": [{"siteId": "site-1", "siteName": "课程一"}],
                "total": 1
            }}),
        )
    }

    fn list_response(status: Value, submit_time: Value) -> CliResult<Response> {
        response(
            200,
            json!({"code": 200, "data": {
                "records": [{
                    "activityId": ASSIGNMENT,
                    "assignmentStatus": status,
                    "submitTime": submit_time
                }],
                "total": 1
            }}),
        )
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
                "verification must stay read-only: {} {}",
                call.method, call.url
            );
        }
    }

    fn verify(
        transport: &mut Scripted,
        expected_ids: &[String],
        expected_text: Option<&str>,
    ) -> CliResult<Outcome> {
        execute_verify_with(
            ASSIGNMENT,
            expected_ids,
            expected_text,
            &session(),
            transport,
            0.0,
            &mut |_| {},
        )
    }

    #[test]
    fn pass_compares_attachments_and_normalizes_text() {
        let mut transport = scripted(vec![
            ("GET", detail_url(), detail_response()),
            (
                "GET",
                submit_view_url(),
                submit_view_response(
                    json!(ASSIGNMENT),
                    json!(["expected-rid"]),
                    "<p>hello    world</p>",
                ),
            ),
            ("GET", courses_url(), courses_response()),
            (
                "POST",
                list_url(),
                list_response(json!(0), json!("2026-11-18 11:00")),
            ),
        ]);
        let expected = vec!["expected-rid".to_owned()];
        let outcome =
            verify(&mut transport, &expected, Some("hello world")).expect("visible submission");

        assert_eq!(outcome.code, 0);
        assert_eq!(outcome.data["verified"], true);
        assert_eq!(outcome.data["submitted_at"], "2026-11-18 11:00");
        assert!(outcome.data.get("submitted").is_none());
        let verification = &outcome.data["verification"];
        assert_eq!(verification["status"], "passed");
        assert_eq!(verification["attempts"], 1);
        let check = &verification["checks"]["submit_view"];
        assert_eq!(check["status"], "passed");
        assert_eq!(check["attachments_checked"], true);
        assert_eq!(check["content_checked"], true);
        assert_eq!(check["expected_text"], "hello world");
        assert_eq!(check["observed_text"], "hello world");
        assert_eq!(check["expected_attachment_ids"], json!(["expected-rid"]));
        assert_eq!(check["observed_attachment_ids"], json!(["expected-rid"]));
        assert_eq!(transport.calls.len(), 4);
        assert_no_writes(&transport);
    }

    #[test]
    fn second_attempt_can_confirm_eventual_consistency() {
        let mut transport = scripted(vec![
            ("GET", detail_url(), detail_response()),
            (
                "GET",
                submit_view_url(),
                submit_view_response(json!(-1), json!([]), ""),
            ),
            ("GET", courses_url(), courses_response()),
            (
                "POST",
                list_url(),
                list_response(json!(0), json!("2026-11-18 11:00")),
            ),
            (
                "GET",
                submit_view_url(),
                submit_view_response(json!(ASSIGNMENT), json!(["visible-rid"]), "<p>hello</p>"),
            ),
            ("GET", courses_url(), courses_response()),
            (
                "POST",
                list_url(),
                list_response(json!(0), json!("2026-11-18 11:00")),
            ),
        ]);
        let expected = vec!["visible-rid".to_owned()];
        let outcome = verify(&mut transport, &expected, Some("hello"))
            .expect("eventually visible submission");

        assert_eq!(outcome.code, 0);
        assert_eq!(outcome.data["verified"], true);
        assert_eq!(outcome.data["verification"]["status"], "passed");
        assert_eq!(outcome.data["verification"]["attempts"], 2);
        assert_eq!(transport.calls.len(), 7);
        assert_no_writes(&transport);
    }

    #[test]
    fn mismatch_exits_6_after_two_read_only_attempts() {
        let mut entries = vec![("GET", detail_url(), detail_response())];
        for _ in 0..2 {
            entries.push((
                "GET",
                submit_view_url(),
                submit_view_response(json!(ASSIGNMENT), json!(["other-rid"]), "<p>goodbye</p>"),
            ));
            entries.push(("GET", courses_url(), courses_response()));
            entries.push((
                "POST",
                list_url(),
                list_response(json!(0), json!("2026-11-18 11:00")),
            ));
        }
        let mut transport = scripted(entries);
        let expected = vec!["expected-rid".to_owned()];
        let outcome =
            verify(&mut transport, &expected, Some("expected body")).expect("verification result");

        assert_eq!(outcome.code, 6);
        assert_eq!(outcome.data["verified"], false);
        let verification = &outcome.data["verification"];
        assert_eq!(verification["status"], "failed");
        assert_eq!(verification["attempts"], 2);
        assert_eq!(verification["failed_checks"], json!(["submit_view"]));
        let check = &verification["checks"]["submit_view"];
        assert_eq!(check["observed_attachment_ids"], json!(["other-rid"]));
        assert_eq!(check["expected_text"], "expected body");
        assert_eq!(check["observed_text"], "goodbye");
        assert_eq!(outcome.diagnostic.as_deref(), Some(VERIFY_ONLY_HINT));
        assert_eq!(transport.calls.len(), 7);
        assert_no_writes(&transport);
    }

    #[test]
    fn sentinel_submission_is_not_treated_as_submitted() {
        let mut entries = vec![("GET", detail_url(), detail_response())];
        for _ in 0..2 {
            entries.push((
                "GET",
                submit_view_url(),
                submit_view_response(json!(-1), json!([]), ""),
            ));
            entries.push(("GET", courses_url(), courses_response()));
            entries.push(("POST", list_url(), list_response(json!(99), Value::Null)));
        }
        let mut transport = scripted(entries);
        let outcome = verify(&mut transport, &[], None).expect("verification result");

        assert_eq!(outcome.code, 6);
        assert_eq!(outcome.data["verified"], false);
        let verification = &outcome.data["verification"];
        assert_eq!(verification["status"], "failed");
        assert_eq!(
            verification["failed_checks"],
            json!(["submit_view", "list"])
        );
        assert_eq!(
            verification["checks"]["submit_view"]["has_submission"],
            false
        );
        assert_eq!(
            verification["checks"]["submit_view"]["observed_attachment_ids"],
            json!([])
        );
        assert_eq!(verification["checks"]["list"]["found"], true);
        assert_eq!(verification["checks"]["list"]["submitted"], false);
        assert_eq!(verification["checks"]["list"]["submitted_at"], Value::Null);
        assert_no_writes(&transport);
    }

    #[test]
    fn submit_view_http_401_is_a_verification_error_exit_7_without_retry_writes() {
        let mut entries = vec![("GET", detail_url(), detail_response())];
        for _ in 0..2 {
            entries.push((
                "GET",
                submit_view_url(),
                response(401, json!({"msg": format!("expired {TOKEN}")})),
            ));
            entries.push(("GET", courses_url(), courses_response()));
            entries.push((
                "POST",
                list_url(),
                list_response(json!(0), json!("2026-11-18 11:00")),
            ));
        }
        let mut transport = scripted(entries);
        let outcome = verify(&mut transport, &[], None).expect("verification result");

        assert_eq!(outcome.code, 7);
        assert_eq!(outcome.data["verified"], Value::Null);
        let verification = &outcome.data["verification"];
        assert_eq!(verification["status"], "error");
        let check = &verification["checks"]["submit_view"];
        assert_eq!(check["status"], "error");
        let error = check["error"].as_str().expect("check error");
        assert!(!error.contains(TOKEN));
        assert!(error.chars().count() <= 200);
        assert!(
            outcome.data["warning"]
                .as_str()
                .is_some_and(|warning| !warning.is_empty())
        );
        assert_eq!(outcome.diagnostic.as_deref(), Some(VERIFY_ONLY_HINT));
        assert_no_writes(&transport);
    }

    #[test]
    fn upstream_failure_text_is_redacted_and_bounded() {
        let long_message = format!("synthetic down {TOKEN} {}", "x".repeat(300));
        let mut entries = vec![("GET", detail_url(), detail_response())];
        for _ in 0..2 {
            entries.push((
                "GET",
                submit_view_url(),
                Err(CliError::new(5, "upstream", long_message.clone(), "")),
            ));
            entries.push(("GET", courses_url(), courses_response()));
            entries.push((
                "POST",
                list_url(),
                list_response(json!(0), json!("2026-11-18 11:00")),
            ));
        }
        let mut transport = scripted(entries);
        let outcome = verify(&mut transport, &[], None).expect("verification result");

        assert_eq!(outcome.code, 7);
        let error = outcome.data["verification"]["checks"]["submit_view"]["error"]
            .as_str()
            .expect("check error");
        assert!(!error.contains(TOKEN));
        assert!(error.chars().count() <= 200);
        assert_no_writes(&transport);
    }
}
