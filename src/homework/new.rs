//! `homework --new DAYS` publish-time filtering.
//!
//! The list row often carries a publish time; only undated rows spend the
//! bounded detail budget. A row that cannot be dated is reported through
//! `unknown_publish_time` instead of being dropped silently.

use super::detail;
use super::read::{self, HomeworkItem, ListFilters, Session};
use super::text;
use crate::cli::{CliError, CliResult};
use crate::transport::Transport;
use serde_json::{Value, json};
use std::collections::HashMap;

/// Python `PUBLISH_LOOKUP_LIMIT`: cap detail lookups for one list command.
const PUBLISH_LOOKUP_LIMIT: usize = 30;
const ERROR_TEXT_LIMIT: usize = 200;

#[derive(Default)]
struct LookupStats {
    looked_up: usize,
    failed: usize,
    last_error: Option<String>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn execute(
    items: Vec<HomeworkItem>,
    method: &str,
    filters: ListFilters,
    limit: u64,
    new_days: u64,
    now: time::PrimitiveDateTime,
    session: &Session,
    transport: &mut dyn Transport,
) -> CliResult<Value> {
    let (items_total, selected) = read::select_items(items, &filters);
    let selected_before = selected.len();
    let cutoff = cutoff(now, new_days)?;
    let (published, unknown, stats) = resolve_publish_times(transport, session, &selected)?;

    let mut fresh: Vec<&HomeworkItem> = selected
        .iter()
        .filter(|item| {
            published
                .get(&item.activity_id)
                .is_some_and(|publish| *publish >= cutoff)
        })
        .collect();
    fresh.sort_by(|left, right| {
        published
            .get(&right.activity_id)
            .cmp(&published.get(&left.activity_id))
    });
    let truncated = limit != 0 && fresh.len() > usize::try_from(limit).unwrap_or(usize::MAX);
    if truncated {
        fresh.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    }

    let assignments: Vec<Value> = fresh
        .into_iter()
        .map(|item| {
            let mut entry = read::assignment_json(item, now);
            if let Some(publish) = published.get(&item.activity_id) {
                entry["published_at"] = json!(read::iso_local_timestamp(*publish));
            }
            entry
        })
        .collect();

    let resolved = published.len();
    let unknown_count = unknown.len();
    let note = note(
        items_total,
        selected_before,
        resolved,
        unknown_count,
        &stats,
    );
    let mut payload = json!({
        "count": assignments.len(),
        "method": method,
        "filters": {
            "undone": filters.undone,
            "course": filters.course,
            "new_days": Some(new_days),
        },
        "assignments": assignments,
        "publish_time": {
            "looked_up": stats.looked_up,
            "resolved": resolved,
            "unknown": unknown_count,
            "failed": stats.failed,
            "lookup_limit": PUBLISH_LOOKUP_LIMIT,
        },
        "note": note,
    });
    if !unknown.is_empty() {
        payload["unknown_publish_time"] = json!(unknown);
    }
    if truncated {
        payload["truncated"] = Value::Bool(true);
    }
    Ok(payload)
}

fn cutoff(now: time::PrimitiveDateTime, days: u64) -> CliResult<time::PrimitiveDateTime> {
    let days = i64::try_from(days)
        .ok()
        .and_then(|days| days.checked_mul(86_400))
        .and_then(|seconds| now.checked_sub(time::Duration::seconds(seconds)))
        .ok_or_else(|| {
            CliError::new(
                5,
                "upstream",
                "--new range is too large for the local clock",
                "Use a smaller day count.",
            )
        })?;
    Ok(days)
}

fn resolve_publish_times(
    transport: &mut dyn Transport,
    session: &Session,
    selected: &[HomeworkItem],
) -> CliResult<(
    HashMap<String, time::PrimitiveDateTime>,
    Vec<String>,
    LookupStats,
)> {
    let mut published = HashMap::new();
    for item in selected {
        if item.activity_id.is_empty() {
            continue;
        }
        if let Some(stamp) = read::publish_time(item) {
            published.insert(item.activity_id.clone(), stamp);
        }
    }

    let mut ordered: Vec<&HomeworkItem> = selected.iter().collect();
    // Match Python's reverse sort literally: undated rows sort before dated
    // rows, then dated rows by deadline newest-first. Stable ties keep the
    // list order.
    ordered.sort_by(|left, right| {
        let left_due = left.parsed_end_time();
        let right_due = right.parsed_end_time();
        right_due
            .is_none()
            .cmp(&left_due.is_none())
            .then_with(|| right_due.cmp(&left_due))
    });

    let mut stats = LookupStats::default();
    let mut budget = PUBLISH_LOOKUP_LIMIT;
    for item in ordered {
        if item.activity_id.is_empty() || published.contains_key(&item.activity_id) {
            continue;
        }
        if budget == 0 {
            break;
        }
        stats.looked_up += 1;
        match detail::fetch_detail(transport, session, &item.activity_id) {
            Ok(detail) => {
                budget -= 1;
                if let Some(stamp) = detail_publish_time(&detail.raw) {
                    published.insert(item.activity_id.clone(), stamp);
                }
            }
            Err(error) if is_auth_error(&error) => return Err(error),
            Err(error) => {
                stats.failed += 1;
                stats.last_error = Some(short_error(&error, &session.access_token));
            }
        }
    }

    let unknown = selected
        .iter()
        .filter(|item| !published.contains_key(&item.activity_id))
        .map(|item| item.activity_id.clone())
        .collect();
    Ok((published, unknown, stats))
}

fn detail_publish_time(raw: &serde_json::Map<String, Value>) -> Option<time::PrimitiveDateTime> {
    let value = ["assignmentBeginTime", "publishTime", "createTime"]
        .into_iter()
        .find_map(|key| raw.get(key).filter(|value| read::is_truthy(value)))?;
    read::value_timestamp(value)
}

fn is_auth_error(error: &CliError) -> bool {
    if error.code == 3 || error.kind == "auth" {
        return true;
    }
    let text = error.message.to_ascii_lowercase();
    [
        "401",
        "403",
        "unauthorized",
        "token expired",
        "invalid token",
        "login required",
        "not logged in",
    ]
    .into_iter()
    .any(|marker| text.contains(marker))
}

fn short_error(error: &CliError, access_token: &str) -> String {
    let message = if error.message.trim().is_empty() {
        "detail lookup failed".to_owned()
    } else {
        error.message.clone()
    };
    let message = text::one_line(&message);
    let message = text::redact(message, &[access_token]);
    text::truncate_chars(&message, ERROR_TEXT_LIMIT)
}

fn note(
    items_total: usize,
    selected_before: usize,
    resolved: usize,
    unknown: usize,
    stats: &LookupStats,
) -> String {
    if items_total == 0 {
        return "没有作业（上游返回空列表）".to_owned();
    }
    if selected_before == 0 {
        return "过滤条件（--course / --undone）下没有作业".to_owned();
    }
    let mut parts = vec![format!(
        "按发布时间过滤：{selected_before} 条候选中解析出 {resolved} 条发布时间"
    )];
    if stats.looked_up > 0 {
        parts.push(format!(
            "逐个查详情 {} 次（上限 {PUBLISH_LOOKUP_LIMIT}）",
            stats.looked_up
        ));
    }
    if stats.failed > 0 {
        parts.push(format!("{} 次详情请求失败", stats.failed));
        if let Some(error) = stats.last_error.as_deref().filter(|text| !text.is_empty()) {
            parts.push(format!("最后一个错误：{error}"));
        }
    }
    if unknown > 0 {
        parts.push(format!(
            "{unknown} 条拿不到发布时间，已排除（见 unknown_publish_time），结果可能不完整"
        ));
    }
    if resolved > 0 && unknown == 0 {
        parts.push("没有落在时间窗口内的作业".to_owned());
    }
    parts.join("；")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::Response;
    use crate::transport::testing::Scripted;
    use serde_json::json;
    use std::collections::VecDeque;

    const TOKEN: &str = "synthetic-access-token";
    const API: &str = "https://apiucloud.bupt.edu.cn";

    fn session() -> Session {
        Session::new(TOKEN.into(), "synthetic-user".into())
    }

    fn now() -> time::PrimitiveDateTime {
        read::local_timestamp("2026-11-18 12:00:00").expect("synthetic clock")
    }

    fn item(index: usize, deadline_days: i64, publish: Option<&str>) -> HomeworkItem {
        let deadline = now() + time::Duration::days(deadline_days);
        let mut raw = serde_json::Map::new();
        if let Some(publish) = publish {
            raw.insert("assignmentBeginTime".into(), json!(publish));
        }
        HomeworkItem {
            activity_id: format!("hw{index}"),
            activity_name: format!("作业 {index}"),
            site_id: Some("site-1".into()),
            site_name: "测试课程".into(),
            begin_time: None,
            end_time: Some(read::iso_local_timestamp(deadline)),
            submit_time: None,
            is_submitted: false,
            score: None,
            raw,
        }
    }

    fn filters(undone: bool) -> ListFilters {
        ListFilters {
            undone,
            course: None,
            new_days: Some(7),
        }
    }

    fn scripted(entries: Vec<(&'static str, String, CliResult<Response>)>) -> Scripted {
        Scripted {
            expected: VecDeque::from(entries),
            calls: Vec::new(),
        }
    }

    fn response(status: u16, body: Value) -> CliResult<Response> {
        Ok(Response {
            status,
            headers: Vec::new(),
            body: serde_json::to_vec(&body).expect("serialize synthetic response"),
        })
    }

    fn detail_url(id: &str) -> String {
        format!("{API}/ykt-site/work/detail?assignmentId={id}")
    }

    fn detail_response(publish: &str) -> CliResult<Response> {
        response(
            200,
            json!({"code": 200, "data": {
                "assignmentTitle": "合成作业",
                "assignmentBeginTime": publish,
            }}),
        )
    }

    #[test]
    fn new_keeps_recent_list_times_and_sorts_newest_first() {
        let items = vec![
            item(1, 30, Some("2026-09-01 08:00:00")),
            item(2, 5, Some("2026-11-17 08:00:00")),
            item(3, 2, Some("2026-11-16 08:00:00")),
            item(4, 1, Some("2026-11-01 08:00:00")),
        ];
        let mut transport = scripted(vec![]);

        let payload = execute(
            items,
            "all",
            filters(false),
            0,
            7,
            now(),
            &session(),
            &mut transport,
        )
        .expect("new filter");

        assert_eq!(payload["count"], 2);
        assert_eq!(payload["assignments"][0]["id"], "hw2");
        assert_eq!(payload["assignments"][1]["id"], "hw3");
        assert_eq!(payload["publish_time"]["looked_up"], 0);
        assert_eq!(payload["publish_time"]["resolved"], 4);
        assert_eq!(payload["publish_time"]["unknown"], 0);
        assert!(
            payload["note"]
                .as_str()
                .unwrap()
                .contains("没有落在时间窗口内")
        );
        assert!(transport.calls.is_empty());
    }

    #[test]
    fn new_detail_failures_do_not_consume_the_budget() {
        let items: Vec<_> = (0..40)
            .map(|index| item(index, index as i64, None))
            .collect();
        let mut entries = Vec::new();
        for index in (10..40).rev() {
            entries.push((
                "GET",
                detail_url(&format!("hw{index}")),
                Err(CliError::new(4, "network", "synthetic detail failure", "")),
            ));
        }
        for index in (0..10).rev() {
            entries.push((
                "GET",
                detail_url(&format!("hw{index}")),
                detail_response("2026-11-17 08:00:00"),
            ));
        }
        let mut transport = scripted(entries);

        let payload = execute(
            items,
            "all",
            filters(false),
            0,
            7,
            now(),
            &session(),
            &mut transport,
        )
        .expect("new filter with failures");

        assert_eq!(payload["count"], 10);
        assert_eq!(payload["publish_time"]["looked_up"], 40);
        assert_eq!(payload["publish_time"]["failed"], 30);
        assert_eq!(payload["publish_time"]["unknown"], 30);
        assert_eq!(
            payload["unknown_publish_time"].as_array().unwrap().len(),
            30
        );
        assert!(payload["note"].as_str().unwrap().contains("详情请求失败"));
        assert_eq!(transport.calls.len(), 40);
    }

    #[test]
    fn new_reports_the_budget_and_unknown_rows() {
        let items: Vec<_> = (0..40)
            .map(|index| item(index, index as i64, None))
            .collect();
        let entries: Vec<_> = (10..40)
            .rev()
            .map(|index| {
                (
                    "GET",
                    detail_url(&format!("hw{index}")),
                    detail_response("2026-09-01 08:00:00"),
                )
            })
            .collect();
        let mut transport = scripted(entries);

        let payload = execute(
            items,
            "all",
            filters(false),
            0,
            7,
            now(),
            &session(),
            &mut transport,
        )
        .expect("new filter budget");

        assert_eq!(payload["count"], 0);
        assert_eq!(payload["publish_time"]["looked_up"], 30);
        assert_eq!(payload["publish_time"]["lookup_limit"], 30);
        assert_eq!(payload["publish_time"]["unknown"], 10);
        assert_eq!(
            payload["unknown_publish_time"],
            json!([
                "hw0", "hw1", "hw2", "hw3", "hw4", "hw5", "hw6", "hw7", "hw8", "hw9"
            ])
        );
    }

    #[test]
    fn new_uses_detail_only_for_rows_that_pass_the_filters() {
        let mut items: Vec<_> = (0..30)
            .map(|index| {
                let mut item = item(index, index as i64, None);
                item.is_submitted = true;
                item
            })
            .collect();
        items.push(item(100, 40, None));
        let mut transport = scripted(vec![(
            "GET",
            detail_url("hw100"),
            detail_response("2026-11-17 08:00:00"),
        )]);

        let payload = execute(
            items,
            "all",
            filters(true),
            0,
            7,
            now(),
            &session(),
            &mut transport,
        )
        .expect("new with undone filter");

        assert_eq!(payload["count"], 1);
        assert_eq!(
            payload["assignments"][0]["published_at"],
            "2026-11-17 08:00:00"
        );
        assert_eq!(transport.calls.len(), 1);
        assert_eq!(transport.calls[0].url, detail_url("hw100"));
    }

    #[test]
    fn new_auth_failure_is_not_reported_as_unknown() {
        let items = vec![item(1, 5, None)];
        let mut transport = scripted(vec![(
            "GET",
            detail_url("hw1"),
            response(401, json!({"msg": "token expired"})),
        )]);

        let error = execute(
            items,
            "all",
            filters(false),
            0,
            7,
            now(),
            &session(),
            &mut transport,
        )
        .expect_err("auth failure");

        assert_eq!(error.code, 3);
        assert_eq!(error.kind, "auth");
        assert_eq!(transport.calls.len(), 1);
    }

    #[test]
    fn new_empty_and_empty_window_notes_are_distinguishable() {
        let mut transport = scripted(vec![]);
        let empty = execute(
            vec![],
            "all",
            filters(false),
            0,
            7,
            now(),
            &session(),
            &mut transport,
        )
        .expect("empty new filter");
        assert!(empty["note"].as_str().unwrap().contains("没有作业"));

        let mut transport = scripted(vec![]);
        let stale = execute(
            vec![item(1, 5, Some("2026-09-01 08:00:00"))],
            "all",
            filters(false),
            0,
            7,
            now(),
            &session(),
            &mut transport,
        )
        .expect("empty window");
        assert!(
            stale["note"]
                .as_str()
                .unwrap()
                .contains("没有落在时间窗口内")
        );
    }

    #[test]
    fn new_filter_with_no_candidates_does_not_lookup() {
        let mut submitted = item(1, 5, None);
        submitted.is_submitted = true;
        let mut transport = scripted(vec![]);
        let payload = execute(
            vec![submitted],
            "all",
            filters(true),
            0,
            7,
            now(),
            &session(),
            &mut transport,
        )
        .expect("no candidates after filters");

        assert_eq!(payload["count"], 0);
        assert_eq!(payload["publish_time"]["looked_up"], 0);
        assert!(payload["note"].as_str().unwrap().contains("过滤条件"));
        assert!(transport.calls.is_empty());
    }

    #[test]
    fn new_limit_sets_truncated_after_the_time_filter() {
        let items = vec![
            item(1, 5, Some("2026-11-17 08:00:00")),
            item(2, 5, Some("2026-11-16 08:00:00")),
        ];
        let mut transport = scripted(vec![]);
        let payload = execute(
            items,
            "all",
            filters(false),
            1,
            7,
            now(),
            &session(),
            &mut transport,
        )
        .expect("limited new filter");

        assert_eq!(payload["count"], 1);
        assert_eq!(payload["truncated"], true);
        assert_eq!(payload["assignments"][0]["id"], "hw1");
    }

    #[test]
    fn detail_failure_text_is_redacted_and_bounded() {
        let items = vec![item(1, 5, None)];
        let message = format!("boom {TOKEN} {}", "x".repeat(300));
        let mut transport = scripted(vec![(
            "GET",
            detail_url("hw1"),
            Err(CliError::new(4, "network", message, "")),
        )]);
        let payload = execute(
            items,
            "all",
            filters(false),
            0,
            7,
            now(),
            &session(),
            &mut transport,
        )
        .expect("failed lookup remains informational");
        let note = payload["note"].as_str().unwrap();

        assert!(!note.contains(TOKEN));
        assert!(note.contains("详情请求失败"));
        assert!(note.chars().count() <= 400);
    }
}
