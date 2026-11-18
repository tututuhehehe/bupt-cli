mod contract_support;

use contract_support::{Invocation, Sandbox};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

const SCHEMA: &str = "bupt.v1";
const REPLAY_ENV: &str = "BUPT_CLI_CONTRACT_REPLAY";
const SYNTHETIC_TOKEN: &str = "synthetic-access-token";
const ASSIGNMENT_ID: &str = "1000000000000000001";
const ASSIGNMENT_ID_5: &str = "1000000000000000005";

const COURSES_PATH: &str = "/ykt-site/site/list/student/current";
const STUDENT_LIST_PATH: &str = "/ykt-site/work/student/list";
const DETAIL_PATH: &str = "/ykt-site/work/detail";

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/contracts/homework")
        .join(name)
}

fn run_fixture(sandbox: &Sandbox, name: &str, args: &[&str]) -> Invocation {
    run_replay(sandbox, &fixture_path(name), args)
}

fn run_replay(sandbox: &Sandbox, path: &Path, args: &[&str]) -> Invocation {
    let mut command = sandbox.command();
    command.env(REPLAY_ENV, path);
    Invocation(
        command
            .args(args)
            .output()
            .expect("run isolated CLI with replay fixture"),
    )
}

fn replay_document(exchanges: Vec<Value>) -> Value {
    json!({
        "schema": "bupt.contract-replay.v1",
        "session": {
            "username": "synthetic-student",
            "role": "undergraduate",
            "access_token": SYNTHETIC_TOKEN,
            "user_id": "synthetic-user"
        },
        "now": "2026-11-18T12:00:00",
        "exchanges": exchanges
    })
}

fn write_replay(sandbox: &Sandbox, name: &str, exchanges: Vec<Value>) -> PathBuf {
    let path = sandbox.root().join(name);
    let bytes = serde_json::to_vec_pretty(&replay_document(exchanges))
        .expect("serialize synthetic replay recording");
    fs::write(&path, bytes).expect("write synthetic replay recording");
    path
}

fn get_data(path: &str, query: Value, data: Value) -> Value {
    json!({
        "request": {"method": "GET", "path": path, "query": query},
        "response": {"status": 200, "json": {"code": 200, "data": data}}
    })
}

fn post_data(path: &str, body: Value, data: Value) -> Value {
    json!({
        "request": {"method": "POST", "path": path, "json": body},
        "response": {"status": 200, "json": {"code": 200, "data": data}}
    })
}

fn get_error(path: &str, query: Value, status: u16, message: &str) -> Value {
    json!({
        "request": {"method": "GET", "path": path, "query": query},
        "response": {"status": status, "json": {"code": status, "msg": message}}
    })
}

fn describe(inv: &Invocation) -> String {
    format!(
        "exit={:?}; stdout={:?}; stderr={:?}",
        inv.0.status.code(),
        String::from_utf8_lossy(&inv.0.stdout),
        String::from_utf8_lossy(&inv.0.stderr)
    )
}

fn assert_success(inv: &Invocation) -> Value {
    assert_eq!(inv.0.status.code(), Some(0), "{}", describe(inv));
    assert!(
        inv.0.stderr.is_empty(),
        "success must leave stderr empty: {}",
        describe(inv)
    );
    let document = inv.stdout_json();
    assert_eq!(document["schema"], SCHEMA, "{}", describe(inv));
    assert_eq!(document["ok"], true, "{}", describe(inv));
    document
}

fn assert_error(inv: &Invocation, code: i32, error_type: &str) -> Value {
    assert_eq!(inv.0.status.code(), Some(code), "{}", describe(inv));
    assert!(
        inv.0.stdout.is_empty(),
        "errors must not write a result document: {}",
        describe(inv)
    );
    let document = inv.stderr_json();
    assert_eq!(document["schema"], SCHEMA, "{}", describe(inv));
    assert_eq!(document["ok"], false, "{}", describe(inv));
    assert_eq!(
        document["error"]["type"],
        error_type,
        "wrong error layer: {}",
        describe(inv)
    );
    document
}

fn assert_result_with_diagnostic(inv: &Invocation, code: i32) -> (Value, String) {
    assert_eq!(inv.0.status.code(), Some(code), "{}", describe(inv));
    assert!(
        !inv.0.stderr.is_empty(),
        "nonzero verification exits need a diagnostic: {}",
        describe(inv)
    );
    let document = inv.stdout_json();
    assert_eq!(document["schema"], SCHEMA, "{}", describe(inv));
    assert_eq!(document["ok"], true, "{}", describe(inv));
    assert!(
        document.get("_exit_code").is_none(),
        "private exit markers must not leak: {}",
        describe(inv)
    );
    let diagnostic = String::from_utf8_lossy(&inv.0.stderr).into_owned();
    assert!(
        !diagnostic.contains(SYNTHETIC_TOKEN),
        "diagnostics must redact the synthetic token: {diagnostic}"
    );
    (document, diagnostic)
}

fn find_by_id<'a>(items: &'a [Value], id: &str) -> &'a Value {
    items
        .iter()
        .find(|item| item["id"] == id)
        .unwrap_or_else(|| panic!("missing item {id} in {items:?}"))
}

fn write_answer(sandbox: &Sandbox) -> String {
    fs::write(sandbox.root().join("answer.pdf"), b"synthetic answer\n")
        .expect("write synthetic answer file");
    "answer.pdf".to_string()
}

#[test]
fn homework_defaults_to_the_list_command() {
    let sandbox = Sandbox::new();
    assert_error(&sandbox.run(&["homework"]), 3, "auth");
}

#[test]
fn explicit_homework_list_alias_is_rejected_as_usage() {
    let sandbox = Sandbox::new();
    let error = assert_error(&sandbox.run(&["homework", "list"]), 2, "usage");
    assert!(
        error["error"]["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty()),
        "usage error needs an explanation: {error}"
    );
}

#[test]
fn default_homework_accepts_the_list_flags() {
    let sandbox = Sandbox::new();
    for args in [
        vec!["homework", "--undone"],
        vec!["homework", "--limit", "1"],
        vec!["homework", "--course", "site-1"],
        vec!["homework", "--new", "7"],
    ] {
        let invocation = sandbox.run(&args);
        assert_error(&invocation, 3, "auth");
    }
}

#[test]
fn invalid_list_values_are_usage_errors_before_credentials() {
    let sandbox = Sandbox::new();
    for args in [
        vec!["homework", "--limit", "-1"],
        vec!["homework", "--new", "-1"],
    ] {
        assert_error(&sandbox.run(&args), 2, "usage");
    }
}

#[test]
fn show_rejects_a_non_numeric_course_id_before_credentials() {
    let sandbox = Sandbox::new();
    assert_error(
        &sandbox.run(&["homework", "show", ASSIGNMENT_ID, "--course", "abc"]),
        2,
        "usage",
    );
}

#[test]
fn submit_rejects_an_empty_payload_before_credentials() {
    let sandbox = Sandbox::new();
    assert_error(
        &sandbox.run(&["homework", "submit", ASSIGNMENT_ID]),
        2,
        "usage",
    );
}

#[test]
fn submit_rejects_dry_run_with_confirmation_before_credentials() {
    let sandbox = Sandbox::new();
    assert_error(
        &sandbox.run(&[
            "homework",
            "submit",
            ASSIGNMENT_ID,
            "--text",
            "synthetic",
            "--dry-run",
            "--i-confirm-submit",
        ]),
        2,
        "usage",
    );
}

#[test]
fn submit_rejects_a_missing_file_before_credentials() {
    let sandbox = Sandbox::new();
    assert_error(
        &sandbox.run(&[
            "homework",
            "submit",
            ASSIGNMENT_ID,
            "--file",
            "missing.pdf",
            "--dry-run",
        ]),
        2,
        "usage",
    );
}

#[test]
fn pretty_changes_layout_not_values() {
    let sandbox = Sandbox::new();
    let compact = assert_success(&run_fixture(
        &sandbox,
        "courses_empty_ids.json",
        &["courses"],
    ));
    let pretty_invocation =
        run_fixture(&sandbox, "courses_empty_ids.json", &["courses", "--pretty"]);
    let pretty = assert_success(&pretty_invocation);
    assert_eq!(compact, pretty);
    let rendered = String::from_utf8_lossy(&pretty_invocation.0.stdout);
    assert!(rendered.contains('\n'), "pretty output must span lines");
    assert!(rendered.contains("  "), "pretty output must be indented");
}

#[test]
fn courses_concatenates_pages_deduplicates_and_stops_on_a_short_page() {
    let sandbox = Sandbox::new();
    let document = assert_success(&run_fixture(
        &sandbox,
        "courses_pagination.json",
        &["courses"],
    ));
    assert_eq!(document["count"], 51);
    let courses = document["courses"].as_array().expect("courses array");
    assert_eq!(courses.len(), 51);
    assert_eq!(courses[0]["id"], "c0");
    assert_eq!(courses[1]["id"], "c1");
    assert_eq!(courses.last().unwrap()["id"], "c50");
    assert_eq!(
        courses.iter().filter(|course| course["id"] == "c0").count(),
        1,
        "duplicate site ids must be removed: {courses:?}"
    );
    assert_eq!(courses.last().unwrap()["teacher"], Value::Null);
}

#[test]
fn courses_keeps_every_empty_site_id() {
    let sandbox = Sandbox::new();
    let document = assert_success(&run_fixture(
        &sandbox,
        "courses_empty_ids.json",
        &["courses"],
    ));
    assert_eq!(document["count"], 3);
    let courses = document["courses"].as_array().expect("courses array");
    assert_eq!(courses.len(), 3);
    assert!(courses.iter().all(|course| course["id"] == ""));
}

#[test]
fn courses_stops_on_a_short_page_without_a_second_request() {
    let sandbox = Sandbox::new();
    let document = assert_success(&run_fixture(
        &sandbox,
        "courses_short_page.json",
        &["courses"],
    ));
    assert_eq!(document["count"], 2);
    let courses = document["courses"].as_array().expect("courses array");
    assert_eq!(courses[0]["id"], "short-a");
    assert_eq!(courses[1]["id"], "short-b");
}

#[test]
fn courses_stops_on_an_empty_page_without_a_second_request() {
    let sandbox = Sandbox::new();
    let document = assert_success(&run_fixture(
        &sandbox,
        "courses_empty_page.json",
        &["courses"],
    ));
    assert_eq!(document["count"], 0);
    assert_eq!(document["courses"], json!([]));
}

#[test]
fn courses_stops_at_the_safety_page_limit() {
    let sandbox = Sandbox::new();
    let mut exchanges = Vec::new();
    for page in 1..=10 {
        let records: Vec<Value> = (0..50)
            .map(|index| {
                json!({
                    "siteId": format!("p{page}-c{index}"),
                    "siteName": format!("第 {page} 页课程 {index}")
                })
            })
            .collect();
        exchanges.push(get_data(
            COURSES_PATH,
            json!({"current": page.to_string()}),
            json!({"records": records, "total": 550}),
        ));
    }
    let path = write_replay(&sandbox, "courses-page-limit.json", exchanges);
    let document = assert_success(&run_replay(&sandbox, &path, &["courses"]));

    assert_eq!(document["count"], 500);
    let courses = document["courses"].as_array().expect("courses array");
    assert_eq!(courses.len(), 500);
    assert_eq!(courses[0]["id"], "p1-c0");
    assert_eq!(courses.last().unwrap()["id"], "p10-c49");
}

#[test]
fn default_homework_list_sorts_by_deadline_and_exposes_missing_metadata() {
    let sandbox = Sandbox::new();
    let document = assert_success(&run_fixture(
        &sandbox,
        "homework_list_all.json",
        &["homework"],
    ));

    assert_eq!(document["count"], 4);
    assert_eq!(document["method"], "all");
    assert_eq!(document["filters"]["undone"], false);
    assert_eq!(document["filters"]["course"], Value::Null);
    assert_eq!(document["filters"]["new_days"], Value::Null);

    let assignments = document["assignments"].as_array().expect("assignments");
    let ids: Vec<&str> = assignments
        .iter()
        .map(|item| item["id"].as_str().expect("assignment id"))
        .collect();
    assert_eq!(
        ids,
        vec!["hw-past", "hw-submitted", "hw-late", "hw-no-deadline"]
    );

    let past = find_by_id(assignments, "hw-past");
    assert_eq!(past["status"], "overdue");
    assert_eq!(past["submitted"], false);
    assert_eq!(past["course"], "数据结构");
    assert_eq!(past["course_id"], "site-1");
    assert_eq!(past["published_at"], "2026-10-01 08:00:00");

    let submitted = find_by_id(assignments, "hw-submitted");
    assert_eq!(submitted["status"], "submitted");
    assert_eq!(submitted["submitted"], true);
    assert_eq!(submitted["submitted_at"], "2026-11-15 10:00");
    assert_eq!(submitted["score"].as_f64(), Some(88.0));

    let pending = find_by_id(assignments, "hw-late");
    assert_eq!(pending["status"], "pending");
    assert_eq!(pending["submitted"], false);

    let undated = find_by_id(assignments, "hw-no-deadline");
    assert_eq!(undated["deadline"], Value::Null);
    assert_eq!(undated["published_at"], Value::Null);
    assert_eq!(undated["hours_remaining"], Value::Null);
    assert_eq!(undated["overdue"], false);
    assert_eq!(undated["status"], "pending");
    assert_eq!(undated["course"], "数据结构");
}

#[test]
fn undone_filter_uses_the_undone_endpoint_and_sorts_by_deadline() {
    let sandbox = Sandbox::new();
    let document = assert_success(&run_fixture(
        &sandbox,
        "homework_list_undone.json",
        &["homework", "--undone"],
    ));
    assert_eq!(document["count"], 2);
    assert_eq!(document["method"], "undone");
    assert_eq!(document["filters"]["undone"], true);
    let assignments = document["assignments"].as_array().expect("assignments");
    let ids: Vec<&str> = assignments
        .iter()
        .map(|item| item["id"].as_str().expect("assignment id"))
        .collect();
    assert_eq!(ids, vec!["u-1", "u-2"]);
}

#[test]
fn course_and_undone_filters_combine() {
    let sandbox = Sandbox::new();
    let document = assert_success(&run_fixture(
        &sandbox,
        "homework_list_course_undone.json",
        &["homework", "--course", "site-1", "--undone"],
    ));
    assert_eq!(document["count"], 2);
    assert_eq!(document["method"], "course");
    assert_eq!(document["filters"]["course"], "site-1");
    assert_eq!(document["filters"]["undone"], true);
    let assignments = document["assignments"].as_array().expect("assignments");
    let ids: Vec<&str> = assignments
        .iter()
        .map(|item| item["id"].as_str().expect("assignment id"))
        .collect();
    assert_eq!(ids, vec!["hw-b", "hw-c"]);
    assert!(
        assignments.iter().all(|item| item["id"] != "hw-done"),
        "submitted rows must be filtered out: {assignments:?}"
    );
}

#[test]
fn new_filter_uses_list_publish_times_and_sorts_newest_first() {
    let sandbox = Sandbox::new();
    let document = assert_success(&run_fixture(
        &sandbox,
        "homework_new_list_times.json",
        &["homework", "--new", "7"],
    ));

    assert_eq!(document["count"], 2);
    assert_eq!(document["filters"]["new_days"], 7);
    assert_eq!(document["publish_time"]["looked_up"], 0);
    assert_eq!(document["publish_time"]["unknown"], 0);
    assert_eq!(document["publish_time"]["failed"], 0);
    assert_eq!(document["publish_time"]["lookup_limit"], 30);

    let assignments = document["assignments"].as_array().expect("assignments");
    let ids: Vec<&str> = assignments
        .iter()
        .map(|item| item["id"].as_str().expect("assignment id"))
        .collect();
    assert_eq!(ids, vec!["hw-newest", "hw-newer"]);
    assert!(
        assignments[0]["published_at"]
            .as_str()
            .is_some_and(|stamp| stamp.starts_with("2026-11-18"))
    );
}

#[test]
fn new_filter_explains_an_empty_list_and_an_empty_window() {
    let sandbox = Sandbox::new();
    let course = get_data(
        COURSES_PATH,
        json!({"current": "1"}),
        json!({"records": [{"siteId": "site-1", "siteName": "课程一"}], "total": 1}),
    );

    let empty_list = write_replay(
        &sandbox,
        "new-empty-list.json",
        vec![
            course.clone(),
            post_data(
                STUDENT_LIST_PATH,
                json!({"siteId": "site-1", "userId": "synthetic-user", "current": 1}),
                json!({"records": [], "total": 0}),
            ),
        ],
    );
    let document = assert_success(&run_replay(
        &sandbox,
        &empty_list,
        &["homework", "--new", "7"],
    ));
    assert_eq!(document["count"], 0);
    assert_eq!(document["publish_time"]["unknown"], 0);
    assert!(
        document["note"]
            .as_str()
            .is_some_and(|note| note.contains("没有作业")),
        "an empty upstream list needs its own explanation: {document}"
    );

    let empty_window = write_replay(
        &sandbox,
        "new-empty-window.json",
        vec![
            course,
            post_data(
                STUDENT_LIST_PATH,
                json!({"siteId": "site-1", "userId": "synthetic-user", "current": 1}),
                json!({
                    "records": [{
                        "activityId": "hw-old",
                        "assignmentTitle": "旧作业",
                        "assignmentStatus": 99,
                        "assignmentBeginTime": "2026-10-01 08:00",
                        "assignmentEndTime": "2026-12-01 23:59"
                    }],
                    "total": 1
                }),
            ),
        ],
    );
    let document = assert_success(&run_replay(
        &sandbox,
        &empty_window,
        &["homework", "--new", "7"],
    ));
    assert_eq!(document["count"], 0);
    assert_eq!(document["publish_time"]["unknown"], 0);
    assert!(
        document["note"]
            .as_str()
            .is_some_and(|note| note.contains("没有落在时间窗口内")),
        "a fully dated empty result must not look like a lookup failure: {document}"
    );
}

#[test]
fn new_filter_reports_undated_assignments_and_respects_the_lookup_budget() {
    let sandbox = Sandbox::new();
    let mut records = Vec::new();
    for index in 0..40 {
        let (year, month, day) = if index < 20 {
            (2026, 12, index + 1)
        } else {
            (2027, 1, index - 19)
        };
        records.push(json!({
            "activityId": format!("hw-{index:02}"),
            "assignmentTitle": format!("未定发布作业 {index:02}"),
            "assignmentStatus": 99,
            "assignmentEndTime": format!("{year}-{month:02}-{day:02} 23:59")
        }));
    }

    let mut exchanges = vec![
        get_data(
            COURSES_PATH,
            json!({"current": "1"}),
            json!({"records": [{"siteId": "site-1", "siteName": "课程一"}], "total": 1}),
        ),
        post_data(
            STUDENT_LIST_PATH,
            json!({"siteId": "site-1", "userId": "synthetic-user", "current": 1}),
            json!({"records": records, "total": 40}),
        ),
    ];

    // Newest deadlines are hw-39 down to hw-10. The reference spends its
    // 30-lookup budget on exactly those rows and must not ask for hw-09.
    for index in (10..40).rev() {
        let id = format!("hw-{index:02}");
        exchanges.push(get_data(
            DETAIL_PATH,
            json!({"assignmentId": id.clone()}),
            json!({
                "id": id,
                "assignmentTitle": format!("未定发布作业 {index:02}"),
                "className": "课程一"
            }),
        ));
    }

    let path = write_replay(&sandbox, "new-unknown.json", exchanges);
    let document = assert_success(&run_replay(&sandbox, &path, &["homework", "--new", "7"]));

    assert_eq!(document["count"], 0);
    assert_eq!(document["publish_time"]["looked_up"], 30);
    assert_eq!(document["publish_time"]["unknown"], 40);
    assert_eq!(document["publish_time"]["failed"], 0);
    assert_eq!(document["publish_time"]["lookup_limit"], 30);
    assert_eq!(
        document["unknown_publish_time"]
            .as_array()
            .expect("unknown_publish_time")
            .len(),
        40
    );
    assert!(
        document["note"]
            .as_str()
            .is_some_and(|note| note.contains("拿不到发布时间")),
        "the note must disclose undated rows: {document}"
    );
}

#[test]
fn new_filter_auth_failure_is_not_reported_as_missing_publish_time() {
    let sandbox = Sandbox::new();
    let error = assert_error(
        &run_fixture(
            &sandbox,
            "homework_new_auth_failure.json",
            &["homework", "--new", "7"],
        ),
        3,
        "auth",
    );
    assert!(
        error["error"]["message"]
            .as_str()
            .is_some_and(|message| !message.contains(SYNTHETIC_TOKEN)),
        "auth errors must not echo synthetic tokens: {error}"
    );
}

#[test]
fn new_filter_looks_up_only_assignments_that_pass_the_filters() {
    let sandbox = Sandbox::new();
    let document = assert_success(&run_fixture(
        &sandbox,
        "homework_new_filtered.json",
        &["homework", "--course", "site-1", "--undone", "--new", "7"],
    ));

    assert_eq!(document["count"], 2);
    assert_eq!(document["publish_time"]["looked_up"], 1);
    assert_eq!(document["publish_time"]["unknown"], 0);
    let assignments = document["assignments"].as_array().expect("assignments");
    let ids: Vec<&str> = assignments
        .iter()
        .map(|item| item["id"].as_str().expect("assignment id"))
        .collect();
    assert_eq!(ids, vec!["hw-target", "hw-other"]);
}

#[test]
fn new_filter_failed_lookups_do_not_consume_the_budget() {
    let sandbox = Sandbox::new();
    let mut records = Vec::new();
    for index in 0..32 {
        let (year, month, day) = if index < 20 {
            (2026, 12, index + 1)
        } else {
            (2027, 1, index - 19)
        };
        records.push(json!({
            "activityId": format!("hw-{index:02}"),
            "assignmentTitle": format!("部分失败作业 {index:02}"),
            "assignmentStatus": 99,
            "assignmentEndTime": format!("{year}-{month:02}-{day:02} 23:59")
        }));
    }

    let mut exchanges = vec![
        get_data(
            COURSES_PATH,
            json!({"current": "1"}),
            json!({"records": [{"siteId": "site-1", "siteName": "课程一"}], "total": 1}),
        ),
        post_data(
            STUDENT_LIST_PATH,
            json!({"siteId": "site-1", "userId": "synthetic-user", "current": 1}),
            json!({"records": records, "total": 32}),
        ),
    ];

    // The newest 30 deadlines fail transiently; the two oldest must still get
    // their lookups because failures do not consume the budget.
    for index in (2..32).rev() {
        exchanges.push(get_error(
            DETAIL_PATH,
            json!({"assignmentId": format!("hw-{index:02}")}),
            500,
            "synthetic detail down",
        ));
    }
    exchanges.push(get_data(
        DETAIL_PATH,
        json!({"assignmentId": "hw-01"}),
        json!({"id": "hw-01", "assignmentBeginTime": "2026-11-17 08:00"}),
    ));
    exchanges.push(get_data(
        DETAIL_PATH,
        json!({"assignmentId": "hw-00"}),
        json!({"id": "hw-00", "assignmentBeginTime": "2026-11-16 08:00"}),
    ));

    let path = write_replay(&sandbox, "new-lookup-failures.json", exchanges);
    let document = assert_success(&run_replay(&sandbox, &path, &["homework", "--new", "7"]));

    assert_eq!(document["count"], 2);
    assert_eq!(document["publish_time"]["looked_up"], 32);
    assert_eq!(document["publish_time"]["failed"], 30);
    assert_eq!(document["publish_time"]["unknown"], 30);
    let assignments = document["assignments"].as_array().expect("assignments");
    let ids: Vec<&str> = assignments
        .iter()
        .map(|item| item["id"].as_str().expect("assignment id"))
        .collect();
    assert_eq!(ids, vec!["hw-01", "hw-00"]);
}

#[test]
fn show_merges_detail_submit_view_and_student_list() {
    let sandbox = Sandbox::new();
    let document = assert_success(&run_fixture(
        &sandbox,
        "homework_show_submitted.json",
        &["homework", "show", ASSIGNMENT_ID],
    ));
    let assignment = &document["assignment"];

    assert_eq!(assignment["id"], ASSIGNMENT_ID);
    assert_eq!(assignment["title"], "作业一");
    assert_eq!(assignment["course"], "课程一");
    assert_eq!(assignment["published_at"], "2026-10-21 00:00:00");
    assert_eq!(assignment["deadline"], "2026-10-26 23:59");
    assert_eq!(assignment["status"], "submitted");
    assert_eq!(assignment["submitted"], true);
    assert_eq!(assignment["content_text"], "见附件");
    assert_eq!(assignment["submit_content"], "见附件");
    assert_eq!(assignment["submitted_at"], "2026-10-22 16:26");
    assert_eq!(assignment["teacher_comment"], "做得不错");

    let teacher_attachment = &assignment["attachments"][0];
    assert_eq!(teacher_attachment["title"], "作业要求.pdf");
    assert_eq!(
        teacher_attachment["url"],
        "https://files.contract.invalid/ucloud/document/teacher-hash.pdf?response-content-disposition=attachment%3Bfilename%3D%E4%BD%9C%E4%B8%9A%E8%A6%81%E6%B1%82.pdf"
    );

    let submitted_attachment = &assignment["submit_attachments"][0];
    assert_eq!(submitted_attachment["title"], "答案.pdf");
    assert_eq!(submitted_attachment["resource_id"], "student-attachment");
    assert_eq!(
        submitted_attachment["url"],
        "https://files.contract.invalid/ucloud/document/student-hash.pdf?response-content-disposition=attachment%3Bfilename%3D%E7%AD%94%E6%A1%88.pdf"
    );
    assert!(assignment.get("submit_view_error").is_none());
}

#[test]
fn show_not_submitted_does_not_fabricate_submission_fields() {
    let sandbox = Sandbox::new();
    let document = assert_success(&run_fixture(
        &sandbox,
        "homework_show_not_submitted.json",
        &["homework", "show", "1000000000000000002"],
    ));
    let assignment = &document["assignment"];

    assert_eq!(assignment["course"], "课程二");
    assert_eq!(assignment["content_text"], "正文");
    assert_eq!(assignment["deadline"], "2026-12-01 23:59");
    assert_eq!(assignment["status"], "pending");
    assert_eq!(assignment["submitted"], false);
    assert_eq!(assignment["submit_content"], Value::Null);
    assert_eq!(assignment["submit_attachments"], json!([]));
    assert_eq!(assignment["submitted_at"], Value::Null);
    assert!(assignment.get("submit_view_error").is_none());
}

#[test]
fn show_survives_submit_view_failure_and_redacts_the_error() {
    let sandbox = Sandbox::new();
    let document = assert_success(&run_fixture(
        &sandbox,
        "homework_show_submit_view_error.json",
        &["homework", "show", ASSIGNMENT_ID],
    ));
    let assignment = &document["assignment"];

    assert_eq!(assignment["submitted"], true);
    assert_eq!(assignment["submit_content"], "详情里的提交正文");
    assert_eq!(assignment["submitted_at"], "2026-10-22 16:26");
    let error = assignment["submit_view_error"]
        .as_str()
        .expect("submit_view_error must explain the degraded view");
    assert!(!error.is_empty());
    assert!(error.len() <= 200, "error text must stay bounded: {error}");
    assert!(
        !error.contains(SYNTHETIC_TOKEN),
        "error text must be redacted: {error}"
    );
}

#[test]
fn download_saves_attachments_and_skips_entries_without_urls() {
    let sandbox = Sandbox::new();
    let download_dir = sandbox.root().join("downloads");
    let dir = download_dir.to_str().expect("utf-8 download directory");
    let document = assert_success(&run_fixture(
        &sandbox,
        "homework_download.json",
        &["homework", "download", "1000000000000000003", "--dir", dir],
    ));

    assert_eq!(document["count_saved"], 2);
    assert_eq!(document["count_skipped"], 1);
    assert_eq!(document["skipped"][0]["reason"], "no_url");
    assert_eq!(
        fs::read(download_dir.join("作业要求.pdf")).expect("saved pdf"),
        b"synthetic teacher pdf\n"
    );
    assert_eq!(
        fs::read(download_dir.join("data.csv")).expect("saved csv"),
        b"a,b\n1,2\n"
    );
}

#[test]
fn download_does_not_overwrite_without_force() {
    let sandbox = Sandbox::new();
    let download_dir = sandbox.root().join("downloads");
    fs::create_dir_all(&download_dir).expect("create download directory");
    let existing = download_dir.join("作业要求.pdf");
    fs::write(&existing, b"keep-me\n").expect("write existing attachment");
    let dir = download_dir.to_str().expect("utf-8 download directory");

    let document = assert_success(&run_fixture(
        &sandbox,
        "homework_download.json",
        &["homework", "download", "1000000000000000003", "--dir", dir],
    ));
    assert_eq!(document["count_saved"], 1);
    assert_eq!(document["count_skipped"], 2);
    assert_eq!(
        fs::read(&existing).expect("read existing attachment"),
        b"keep-me\n"
    );

    let document = assert_success(&run_fixture(
        &sandbox,
        "homework_download.json",
        &[
            "homework",
            "download",
            "1000000000000000003",
            "--dir",
            dir,
            "--force",
        ],
    ));
    assert_eq!(document["count_saved"], 2);
    assert_eq!(document["count_skipped"], 1);
    assert_eq!(
        fs::read(&existing).expect("read forced attachment"),
        b"synthetic teacher pdf\n"
    );
}

#[test]
fn submit_without_confirmation_exits_two_before_upload() {
    let sandbox = Sandbox::new();
    let file = write_answer(&sandbox);
    let error = assert_error(
        &run_fixture(
            &sandbox,
            "homework_submit_gate.json",
            &[
                "homework",
                "submit",
                "1000000000000000004",
                "--file",
                &file,
                "--text",
                "合成答案",
            ],
        ),
        2,
        "usage",
    );
    assert!(
        error["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("i-confirm-submit")),
        "the gate must name the required flag: {error}"
    );
}

#[test]
fn submit_dry_run_never_uploads_or_submits() {
    let sandbox = Sandbox::new();
    let file = write_answer(&sandbox);
    let document = assert_success(&run_fixture(
        &sandbox,
        "homework_submit_dry_run.json",
        &[
            "homework",
            "submit",
            "1000000000000000004",
            "--file",
            &file,
            "--text",
            "合成答案",
            "--dry-run",
        ],
    ));

    assert_eq!(document["dry_run"], true);
    assert_eq!(document["would_upload"], json!(["answer.pdf"]));
    assert_eq!(document["would_submit"], true);
    assert_eq!(
        document["planned"]["existing_resource_ids"],
        json!(["existing-res"])
    );
    assert_eq!(document["planned"]["files"][0]["name"], "answer.pdf");
    assert!(document.get("verification").is_none());
}

#[test]
fn submit_confirmed_uploads_submits_once_and_verifies() {
    let sandbox = Sandbox::new();
    let file = write_answer(&sandbox);
    let document = assert_success(&run_fixture(
        &sandbox,
        "homework_submit_confirmed.json",
        &[
            "homework",
            "submit",
            ASSIGNMENT_ID_5,
            "--file",
            &file,
            "--text",
            "合成答案",
            "--i-confirm-submit",
        ],
    ));

    assert_eq!(document["submitted"], true);
    assert_eq!(document["verified"], true);
    assert_eq!(document["submitted_at"], "2026-11-18 11:00");
    assert_eq!(document["attachment_ids"], json!(["uploaded-rid"]));
    assert_eq!(document["uploaded"][0]["resource_id"], "uploaded-rid");
    assert_eq!(document["upstream"]["success"], true);
    assert_eq!(document["verification"]["status"], "passed");
    assert_eq!(
        document["verification"]["checks"]["submit_view"]["expected_attachment_ids"],
        json!(["uploaded-rid"])
    );
    assert_eq!(
        document["verification"]["checks"]["submit_view"]["observed_attachment_ids"],
        json!(["uploaded-rid"])
    );
}

#[test]
fn submit_no_verify_skips_read_only_checks() {
    let sandbox = Sandbox::new();
    let file = write_answer(&sandbox);
    let document = assert_success(&run_fixture(
        &sandbox,
        "homework_submit_no_verify.json",
        &[
            "homework",
            "submit",
            ASSIGNMENT_ID_5,
            "--file",
            &file,
            "--text",
            "合成答案",
            "--i-confirm-submit",
            "--no-verify",
        ],
    ));

    assert_eq!(document["submitted"], true);
    assert_eq!(document["verified"], Value::Null);
    assert_eq!(document["submitted_at"], Value::Null);
    assert_eq!(document["verification"], json!({"status": "skipped"}));
    assert_eq!(document["uploaded"][0]["resource_id"], "uploaded-rid");
}

#[test]
fn submit_unverified_exits_six_without_resubmitting() {
    let sandbox = Sandbox::new();
    let file = write_answer(&sandbox);
    let invocation = run_fixture(
        &sandbox,
        "homework_submit_unverified.json",
        &[
            "homework",
            "submit",
            ASSIGNMENT_ID_5,
            "--file",
            &file,
            "--text",
            "合成答案",
            "--i-confirm-submit",
        ],
    );
    let (document, diagnostic) = assert_result_with_diagnostic(&invocation, 6);

    assert_eq!(document["submitted"], true);
    assert_eq!(document["verified"], false);
    assert_eq!(document["verification"]["status"], "failed");
    assert_eq!(document["verification"]["attempts"], 2);
    assert_eq!(
        document["verification"]["failed_checks"],
        json!(["submit_view"])
    );
    assert_eq!(
        document["verification"]["checks"]["submit_view"]["expected_attachment_ids"],
        json!(["uploaded-rid"])
    );
    assert_eq!(
        document["verification"]["checks"]["submit_view"]["observed_attachment_ids"],
        json!(["stale-rid"])
    );
    assert!(
        diagnostic.contains("upstream reported success"),
        "exit 6 must warn against blind resubmission: {diagnostic}"
    );
}

#[test]
fn submit_verification_error_exits_seven_without_resubmitting() {
    let sandbox = Sandbox::new();
    let file = write_answer(&sandbox);
    let invocation = run_fixture(
        &sandbox,
        "homework_submit_verify_error.json",
        &[
            "homework",
            "submit",
            ASSIGNMENT_ID_5,
            "--file",
            &file,
            "--text",
            "合成答案",
            "--i-confirm-submit",
        ],
    );
    let (document, diagnostic) = assert_result_with_diagnostic(&invocation, 7);

    assert_eq!(document["submitted"], true);
    assert_eq!(document["verified"], Value::Null);
    assert_eq!(document["verification"]["status"], "error");
    assert!(
        document["warning"]
            .as_str()
            .is_some_and(|warning| warning.contains("verification could not run")),
        "exit 7 must explain the unverified submit: {document}"
    );
    assert!(!diagnostic.is_empty());
}

#[test]
fn verify_bare_accepts_a_visible_submission_and_normalises_text() {
    let sandbox = Sandbox::new();
    let document = assert_success(&run_fixture(
        &sandbox,
        "homework_verify_pass.json",
        &[
            "homework",
            "verify",
            ASSIGNMENT_ID_5,
            "--course",
            "site-1",
            "--text",
            "hello world",
        ],
    ));

    assert_eq!(document["verified"], true);
    assert_eq!(document["submitted_at"], "2026-11-18 11:00");
    assert!(document.get("submitted").is_none());
    let check = &document["verification"]["checks"]["submit_view"];
    assert_eq!(check["status"], "passed");
    assert_eq!(check["attachments_checked"], false);
    assert_eq!(check["content_checked"], true);
    assert_eq!(check["expected_text"], "hello world");
    assert_eq!(check["observed_text"], "hello world");
}

#[test]
fn verify_resource_mismatch_exits_six_with_the_read_only_hint() {
    let sandbox = Sandbox::new();
    let invocation = run_fixture(
        &sandbox,
        "homework_verify_mismatch.json",
        &[
            "homework",
            "verify",
            ASSIGNMENT_ID_5,
            "--course",
            "site-1",
            "--resource-id",
            "expected-rid",
            "--text",
            "expected body",
        ],
    );
    let (document, diagnostic) = assert_result_with_diagnostic(&invocation, 6);

    assert_eq!(document["verified"], false);
    assert_eq!(document["verification"]["status"], "failed");
    assert_eq!(
        document["verification"]["failed_checks"],
        json!(["submit_view"])
    );
    let check = &document["verification"]["checks"]["submit_view"];
    assert_eq!(check["attachments_checked"], true);
    assert_eq!(check["expected_attachment_ids"], json!(["expected-rid"]));
    assert_eq!(check["observed_attachment_ids"], json!(["other-rid"]));
    assert_eq!(check["content_checked"], true);
    assert_eq!(check["expected_text"], "expected body");
    assert_eq!(check["observed_text"], "goodbye");
    assert!(
        diagnostic.contains("could not be confirmed"),
        "verify-only failures need their own hint: {diagnostic}"
    );
    assert!(
        !diagnostic.contains("upstream reported success"),
        "verify never submitted, so it must not claim upstream success: {diagnostic}"
    );
}

#[test]
fn verify_error_exits_seven_without_submitting() {
    let sandbox = Sandbox::new();
    let invocation = run_fixture(
        &sandbox,
        "homework_verify_error.json",
        &["homework", "verify", ASSIGNMENT_ID_5, "--course", "site-1"],
    );
    let (document, diagnostic) = assert_result_with_diagnostic(&invocation, 7);

    assert_eq!(document["verified"], Value::Null);
    assert_eq!(document["verification"]["status"], "error");
    let check = &document["verification"]["checks"]["submit_view"];
    assert_eq!(check["status"], "error");
    let error = check["error"].as_str().expect("submit-view error text");
    assert!(error.len() <= 200, "error text must stay bounded: {error}");
    assert!(
        !error.contains(SYNTHETIC_TOKEN),
        "error text must be redacted: {error}"
    );
    assert!(
        document["warning"]
            .as_str()
            .is_some_and(|warning| warning.contains("verification could not run")),
        "exit 7 must expose the verification error: {document}"
    );
    assert!(!diagnostic.is_empty());
}
