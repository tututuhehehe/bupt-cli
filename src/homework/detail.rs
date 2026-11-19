//! Shared read-only UCloud assignment detail, submit-view, attachment, and
//! student-list helpers. `homework show` and `homework verify` both use this
//! module so they cannot drift into separate request or error semantics.

use super::read::{self, Session};
use super::text;
use crate::cli::{CliError, CliResult};
use crate::transport::Transport;
use serde_json::{Map, Value, json};
use std::collections::HashSet;

const UCLOUD_API: &str = "https://apiucloud.bupt.edu.cn";
const DETAIL_PATH: &str = "/ykt-site/work/detail";
const SUBMIT_VIEW_PATH: &str = "/ykt-site/work/submit-view";
const LIST_PATH: &str = "/ykt-site/work/student/list";
const PREVIEW_PATH: &str = "/blade-source/resource/preview-url";
const LIST_PAGE_SIZE: usize = 50;
const MAX_PAGINATION_PAGES: usize = 1000;

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Attachment {
    pub(super) title: String,
    pub(super) url: Option<String>,
    pub(super) resource_id: Option<String>,
    pub(super) size: Value,
}

impl Attachment {
    pub(super) fn as_json(&self) -> Value {
        json!({
            "title": self.title,
            "url": self.url,
            "resource_id": self.resource_id,
            "size": self.size,
        })
    }
}

#[derive(Debug, Clone)]
pub(super) struct Detail {
    pub(super) assignment_id: String,
    pub(super) title: String,
    pub(super) content_text: String,
    pub(super) attachments: Vec<Attachment>,
    pub(super) submitted: bool,
    pub(super) submit_content: Option<String>,
    pub(super) submit_attachments: Vec<Attachment>,
    pub(super) score: Option<f64>,
    pub(super) teacher_comment: Option<String>,
    pub(super) raw: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ListRecord {
    pub(super) activity_id: String,
    pub(super) raw_id: String,
    pub(super) submit_time: Option<String>,
    pub(super) submitted: bool,
}

pub(super) fn fetch_detail(
    transport: &mut dyn Transport,
    session: &Session,
    assignment_id: &str,
) -> CliResult<Detail> {
    let url = read::query_url(DETAIL_PATH, &[("assignmentId", assignment_id)]);
    let data = read::get_data(transport, session, "UCloud assignment detail", url)?;
    parse_detail(assignment_id, data)
}

pub(super) fn parse_detail(assignment_id: &str, data: Value) -> CliResult<Detail> {
    let Some(object) = data.as_object() else {
        if data.is_null() {
            return Err(CliError::new(
                5,
                "not_found",
                format!("assignment detail not found for id {assignment_id}"),
                "",
            ));
        }
        return Err(CliError::new(
            5,
            "parse",
            "UCloud assignment detail returned malformed data",
            "",
        ));
    };
    if object.is_empty() {
        return Err(CliError::new(
            5,
            "not_found",
            format!("assignment detail not found for id {assignment_id}"),
            "",
        ));
    }
    if object
        .get("submitInfo")
        .is_some_and(|value| !value.is_null() && !value.is_object())
    {
        return Err(CliError::new(
            5,
            "parse",
            "UCloud assignment detail returned malformed submitInfo",
            "",
        ));
    }
    let submit_info = object.get("submitInfo").and_then(Value::as_object);
    let content_html =
        first_truthy_string(object, &["assignmentContent", "content"]).unwrap_or_default();
    let score = read::first_present(submit_info.unwrap_or(&Map::new()), &["score"])
        .or_else(|| read::first_present(object, &["score"]))
        .and_then(parse_score);
    let submit_content = submit_info
        .and_then(|info| info.get("submitContent"))
        .and_then(read::value_as_string);
    let teacher_comment = submit_info
        .and_then(|info| info.get("comment"))
        .and_then(read::value_as_string);

    Ok(Detail {
        assignment_id: assignment_id.to_owned(),
        title: first_truthy_string(object, &["assignmentTitle", "title"]).unwrap_or_default(),
        content_text: text::html_to_text(&content_html),
        attachments: attachment_entries(
            object,
            &["attachmentList", "attachments", "assignmentResource"],
        )
        .iter()
        .enumerate()
        .map(|(index, entry)| normalize_attachment(entry, index))
        .collect(),
        submitted: submit_info
            .and_then(|info| info.get("isSubmit"))
            .is_some_and(read::is_truthy)
            || object.get("isSubmit").is_some_and(read::is_truthy)
            || object
                .get("assignmentStatus")
                .is_some_and(assignment_status_submitted),
        submit_content,
        submit_attachments: submit_info
            .and_then(|info| info.get("attachmentList"))
            .filter(|value| read::is_truthy(value))
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .enumerate()
                    .map(|(index, entry)| normalize_attachment(entry, index))
                    .collect()
            })
            .unwrap_or_default(),
        score,
        teacher_comment,
        raw: object.clone(),
    })
}

pub(super) fn fetch_submit_view(
    transport: &mut dyn Transport,
    session: &Session,
    assignment_id: &str,
) -> CliResult<Value> {
    let url = read::query_url(SUBMIT_VIEW_PATH, &[("assignmentId", assignment_id)]);
    let payload = read::send_payload(transport, session, "UCloud submit-view", "GET", url, None)?;
    Ok(payload
        .get("data")
        .and_then(Value::as_object)
        .map(|object| Value::Object(object.clone()))
        .unwrap_or_else(|| Value::Object(Map::new())))
}

pub(super) fn submit_view_has_submission(submit_view: &Value) -> bool {
    let Some(object) = submit_view.as_object() else {
        return false;
    };
    if let Some(assignment_id) = object.get("assignmentId") {
        let sentinel = match assignment_id {
            Value::Null => true,
            Value::String(text) => text.is_empty() || text == "-1",
            Value::Number(number) => number.as_f64() == Some(-1.0),
            _ => false,
        };
        if !sentinel {
            return true;
        }
    }
    object
        .get("attachmentIds")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items.iter().any(|item| {
                !item.is_null()
                    && read::value_as_string(item).is_some_and(|value| !value.trim().is_empty())
            })
        })
}

pub(super) fn submit_view_ids(submit_view: &Value) -> Vec<String> {
    submit_view
        .get("attachmentIds")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|item| !item.is_null())
                .filter_map(read::value_as_string)
                .filter(|id| !id.trim().is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Build submitted-attachment entries from the submit-view resource ids.
pub(super) fn submit_view_attachments(submit_view: &Value) -> Vec<Attachment> {
    submit_view_ids(submit_view)
        .into_iter()
        .enumerate()
        .map(|(index, resource_id)| Attachment {
            title: format!("attachment-{}", index + 1),
            url: None,
            resource_id: Some(resource_id),
            size: Value::Null,
        })
        .collect()
}

/// Best-effort preview URL resolution. A failed preview lookup leaves the URL
/// null; it must never sink the show/verify read.
pub(super) fn resolve_attachment_urls(
    transport: &mut dyn Transport,
    session: &Session,
    attachments: &mut [Attachment],
    derive_titles: bool,
) {
    for attachment in attachments.iter_mut() {
        if attachment.url.is_some() {
            continue;
        }
        let Some(resource_id) = attachment.resource_id.as_deref() else {
            continue;
        };
        let url = read::query_url(PREVIEW_PATH, &[("resourceId", resource_id)]);
        let Ok(data) = read::get_data(transport, session, "UCloud preview URL", url) else {
            continue;
        };
        let preview = match data {
            Value::Object(object) => object
                .get("previewUrl")
                .or_else(|| object.get("url"))
                .and_then(read::value_as_string)
                .filter(|url| !url.is_empty()),
            Value::String(url) if !url.is_empty() => Some(url),
            _ => None,
        };
        if preview.is_some() {
            attachment.url = preview;
        }
    }
    if derive_titles {
        for (index, attachment) in attachments.iter_mut().enumerate() {
            if let Some(title) = attachment
                .url
                .as_deref()
                .and_then(preview_filename)
                .filter(|title| !title.is_empty())
            {
                attachment.title = title;
            } else if attachment.title.is_empty() {
                attachment.title = format!("attachment-{}", index + 1);
            }
        }
    }
}

pub(super) fn target_site_name(detail: &Detail) -> Option<String> {
    first_truthy_string(&detail.raw, &["className", "siteName", "site_name"])
}

pub(super) fn show_course_name(detail: &Detail) -> Option<String> {
    first_truthy_string(&detail.raw, &["siteName", "site_name", "className"])
}

/// Read one course page and narrow candidate sites by the detail's course
/// name, falling back to every non-empty site id just like Python
/// `_find_list_record`.
pub(super) fn course_site_ids(
    transport: &mut dyn Transport,
    session: &Session,
    target_name: Option<&str>,
) -> CliResult<Vec<String>> {
    let (courses, _) = read::fetch_course_page(transport, session, 1)?;
    let mut matching = Vec::new();
    if let Some(target_name) = target_name.filter(|name| !name.is_empty()) {
        matching = courses
            .iter()
            .filter(|course| !course.site_id.is_empty() && course.site_name == target_name)
            .map(|course| course.site_id.clone())
            .collect();
    }
    if !matching.is_empty() {
        return Ok(matching);
    }
    Ok(courses
        .into_iter()
        .filter(|course| !course.site_id.is_empty())
        .map(|course| course.site_id)
        .collect())
}

pub(super) fn find_list_record(
    transport: &mut dyn Transport,
    session: &Session,
    assignment_id: &str,
    site_ids: &[String],
) -> CliResult<Option<ListRecord>> {
    if session.user_id.is_empty() {
        return Err(CliError::new(
            3,
            "auth",
            "UCloud token is missing user_id",
            "Run `bupt login --force`, then retry.",
        ));
    }
    let target = assignment_id.to_owned();
    for site_id in site_ids {
        let site = site_id.trim();
        if site.is_empty() {
            continue;
        }
        let mut fetched_records = 0usize;
        let mut seen_pages = HashSet::new();
        let mut finished = false;
        for page in 1..=MAX_PAGINATION_PAGES {
            let body = json!({
                "siteId": site,
                "userId": session.user_id,
                "current": page,
                "size": LIST_PAGE_SIZE,
            });
            let data = read::post_data(
                transport,
                session,
                "UCloud homework list",
                format!("{UCLOUD_API}{LIST_PATH}"),
                &body,
            )?;
            let (records, record_count, total, signature) = parse_list_page(&data)?;
            if record_count > 0 && !seen_pages.insert(signature) {
                return Err(CliError::new(
                    5,
                    "parse",
                    format!("UCloud homework pagination repeated a page for site '{site}'"),
                    "",
                ));
            }
            if let Some(record) = records
                .into_iter()
                .find(|record| record.activity_id == target || record.raw_id == target)
            {
                return Ok(Some(record));
            }
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
                format!("UCloud homework pagination exceeded the safety limit for site '{site}'"),
                "",
            ));
        }
    }
    Ok(None)
}

fn parse_list_page(data: &Value) -> CliResult<(Vec<ListRecord>, usize, Option<u64>, String)> {
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
    let mut parsed = Vec::with_capacity(records.len());
    for record in records {
        let record = record.as_object().ok_or_else(|| {
            CliError::new(
                5,
                "parse",
                "UCloud homework list returned malformed records",
                "",
            )
        })?;
        let submit_time = record
            .get("submitTime")
            .filter(|value| read::is_truthy(value))
            .and_then(read::value_as_string);
        let status = read::first_present(record, &["assignmentStatus", "status"]);
        let submitted = status.is_some_and(|value| {
            assignment_status_submitted(value)
                || matches!(value, Value::String(text) if text == "SUBMITTED")
        }) || submit_time.is_some();
        parsed.push(ListRecord {
            activity_id: first_truthy_string(record, &["activityId", "id"]).unwrap_or_default(),
            raw_id: record
                .get("id")
                .filter(|value| read::is_truthy(value))
                .and_then(read::value_as_string)
                .unwrap_or_default(),
            submit_time,
            submitted,
        });
    }
    let total = read::pagination_total(object, "UCloud homework list")?;
    let signature = serde_json::to_string(records).unwrap_or_default();
    Ok((parsed, records.len(), total, signature))
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

fn attachment_entries(object: &Map<String, Value>, keys: &[&str]) -> Vec<Value> {
    for key in keys {
        let Some(value) = object.get(*key).filter(|value| read::is_truthy(value)) else {
            continue;
        };
        return match value {
            Value::Array(items) => items.clone(),
            other => vec![other.clone()],
        };
    }
    Vec::new()
}

fn normalize_attachment(entry: &Value, index: usize) -> Attachment {
    if let Value::String(value) = entry {
        let title = value.rsplit('/').next().unwrap_or_default();
        return Attachment {
            title: if title.is_empty() {
                format!("attachment-{}", index + 1)
            } else {
                title.to_owned()
            },
            url: (!value.is_empty()).then(|| value.clone()),
            resource_id: None,
            size: Value::Null,
        };
    }
    let Some(object) = entry.as_object() else {
        return Attachment {
            title: format!("attachment-{}", index + 1),
            url: None,
            resource_id: None,
            size: Value::Null,
        };
    };
    Attachment {
        title: first_truthy_string(
            object,
            &[
                "title",
                "name",
                "resourceName",
                "fileName",
                "originalName",
                "originalFileName",
            ],
        )
        .unwrap_or_else(|| format!("attachment-{}", index + 1)),
        url: first_truthy_string(object, &["url", "downloadUrl", "previewUrl", "link"]),
        resource_id: first_truthy_string(object, &["resourceId", "storageId", "fileId", "id"]),
        size: ["size", "fileSize"]
            .iter()
            .find_map(|key| {
                object
                    .get(*key)
                    .filter(|value| read::is_truthy(value))
                    .cloned()
            })
            .unwrap_or(Value::Null),
    }
}

fn first_truthy_string(object: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = object.get(*key).filter(|value| read::is_truthy(value)) {
            return read::value_as_string(value).filter(|value| !value.is_empty());
        }
    }
    None
}

fn assignment_status_submitted(value: &Value) -> bool {
    match value {
        Value::Bool(_) => false,
        Value::Number(number) => number.as_f64().is_some_and(|number| number == 0.0),
        Value::String(text) => text == "0",
        _ => false,
    }
}

fn parse_score(value: &Value) -> Option<f64> {
    match value {
        Value::Bool(_) | Value::Null | Value::Array(_) | Value::Object(_) => None,
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
    }
}

/// Extract the filename from UCloud's encoded `response-content-disposition`.
pub(super) fn preview_filename(url: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    let value = query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        key.eq_ignore_ascii_case("response-content-disposition")
            .then_some(value)
    })?;
    let decoded = percent_decode(value);
    let mut star = None;
    let mut plain = None;
    for part in decoded.split(';') {
        let part = part.trim();
        let lower = part.to_ascii_lowercase();
        if let Some(value) = lower
            .strip_prefix("filename*=")
            .map(|_| &part["filename*=".len()..])
        {
            let raw = value.trim().trim_matches('"');
            star = Some(
                raw.split_once("''")
                    .map_or(raw, |(_, encoded)| encoded)
                    .to_owned(),
            );
        } else if let Some(value) = lower
            .strip_prefix("filename=")
            .map(|_| &part["filename=".len()..])
        {
            plain = Some(value.trim().trim_matches('"').to_owned());
        }
    }
    let name = star.or(plain).unwrap_or_default();
    let name = percent_decode(&name);
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let high = hex(bytes[index + 1]);
            let low = hex(bytes[index + 2]);
            if let (Some(high), Some(low)) = (high, low) {
                decoded.push(high * 16 + low);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_mapping_missing_status_is_not_submitted_and_missing_attachments_stay_empty() {
        let detail = parse_detail(
            "1",
            json!({
                "assignmentTitle": "合成作业",
                "assignmentContent": "<p>正文</p>",
            }),
        )
        .expect("detail");
        assert!(!detail.submitted);
        assert!(detail.attachments.is_empty());
        assert!(detail.submit_attachments.is_empty());
        assert_eq!(detail.content_text, "正文");
    }

    #[test]
    fn detail_status_zero_and_string_zero_are_submitted_but_false_is_not() {
        for status in [json!(0), json!("0")] {
            let detail =
                parse_detail("1", json!({"assignmentStatus": status})).expect("detail status");
            assert!(detail.submitted);
        }
        for status in [
            json!(false),
            json!(99),
            json!("99"),
            json!("SUBMITTED"),
            Value::Null,
        ] {
            let detail =
                parse_detail("1", json!({"assignmentStatus": status})).expect("detail status");
            assert!(!detail.submitted);
        }
    }

    #[test]
    fn list_status_accepts_only_the_documented_submitted_values() {
        let page = json!({
            "records": [
                {"activityId": "a", "assignmentStatus": 0},
                {"activityId": "b", "assignmentStatus": "SUBMITTED"},
                {"activityId": "c", "assignmentStatus": 99},
                {"activityId": "d", "submitTime": "2026-11-18 11:00"}
            ],
            "total": 4
        });
        let (records, count, total, _) = parse_list_page(&page).expect("list page");
        assert_eq!(count, 4);
        assert_eq!(total, Some(4));
        assert!(records[0].submitted);
        assert!(records[1].submitted);
        assert!(!records[2].submitted);
        assert!(records[3].submitted);
    }

    #[test]
    fn submit_view_sentinels_and_attachment_lists_are_not_fabricated() {
        assert!(!submit_view_has_submission(&json!({})));
        assert!(!submit_view_has_submission(&json!({"assignmentId": -1})));
        assert!(!submit_view_has_submission(&json!({"assignmentId": "-1"})));
        assert!(!submit_view_has_submission(&json!({"assignmentId": ""})));
        assert!(!submit_view_has_submission(&json!({"attachmentIds": []})));
        assert!(!submit_view_has_submission(
            &json!({"attachmentIds": [null, "  "]})
        ));
        assert!(submit_view_has_submission(&json!({"assignmentId": 0})));
        assert!(submit_view_has_submission(
            &json!({"attachmentIds": ["visible-rid"]})
        ));
    }

    #[test]
    fn preview_filename_handles_encoded_and_quoted_dispositions() {
        assert_eq!(
            preview_filename(
                "https://x.invalid/a?response-content-disposition=attachment%3Bfilename%3D%E7%AD%94%E6%A1%88.pdf"
            )
            .as_deref(),
            Some("答案.pdf")
        );
        assert_eq!(
            preview_filename(
                "https://x.invalid/a?response-content-disposition=attachment%3B%20filename%2A%3DUTF-8%27%27name.txt"
            )
            .as_deref(),
            Some("name.txt")
        );
        assert_eq!(preview_filename("https://x.invalid/a?x=1"), None);
    }

    #[test]
    fn attachment_normalization_keeps_explicit_values_and_placeholders() {
        let attachment = normalize_attachment(
            &json!({
                "resourceId": "rid",
                "resourceName": "作业.pdf",
                "size": 0,
                "fileSize": 12,
            }),
            0,
        );
        assert_eq!(attachment.title, "作业.pdf");
        assert_eq!(attachment.resource_id.as_deref(), Some("rid"));
        assert_eq!(attachment.size, 12, "zero size falls through like Python");
        let placeholder = normalize_attachment(&json!({}), 2);
        assert_eq!(placeholder.title, "attachment-3");
        assert_eq!(placeholder.url, None);
        assert_eq!(placeholder.resource_id, None);
    }
}
