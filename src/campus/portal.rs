//! Information-portal notices and the campus-card widget.
//!
//! The portal lives at the campus-internal host `my.bupt.edu.cn`. This module
//! owns the argument semantics, request shape, response classification, and
//! the Python-compatible output mapping. It receives an already-sourced
//! [`Transport`]: cookies, TLS, proxying, and the one allowed session renewal
//! belong to the adapter that supplies it. No request leaves this module
//! before its destination has been validated, and no credential is attached
//! here.
//!
//! HTML is scanned by hand because the Rust package keeps no HTML parser
//! dependency. The scanner understands quoted attributes, comments, void
//! elements, raw-text elements, and nested elements of the same tag; it does
//! not implement HTML error recovery, implied end tags, or foreign content.

use crate::cli::{CliError, CliResult};
use crate::transport::{Request, Response, Transport};
use serde_json::{Value, json};
use url::Url;

const PORTAL_BASE: &str = "http://my.bupt.edu.cn";
const PORTAL_ROOT: &str = "http://my.bupt.edu.cn/";
const PORTAL_HOST: &str = "my.bupt.edu.cn";

pub(super) const CARD_URL: &str = "http://my.bupt.edu.cn/system/resource/app/cuser/getwxtsA.jsp";
pub(super) const CARD_REFERER: &str =
    "http://my.bupt.edu.cn/xs_index.jsp?urltype=tree.TreeTempUrl&wbtreeid=1541";

const LOGIN_PATH_MARKERS: [&str; 2] = ["/system/resource/code/auth/clogin.jsp", "authserver/login"];
const SYSTEM_ERROR_TITLES: [&str; 5] = ["系统提示", "错误提示", "系统错误", "访问出错", "页面错误"];
const AUTHOR_MARKERS: [&str; 2] = ["发布部门", "作者"];
const METADATA_MARKERS: [&str; 12] = [
    "发布时间",
    "发布日期",
    "截止日期",
    "截止时间",
    "浏览次数",
    "点击次数",
    "来源",
    "责任编辑",
    "编辑",
    "审核",
    "作者",
    "发布部门",
];
const DEFAULT_TYPES: [NoticeType; 2] = [NoticeType::Notice, NoticeType::News];
const EMPTY_SORT_TIME: &str = "0000-00-00";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NoticeType {
    Notice,
    News,
    Rules,
    Ways,
}

impl NoticeType {
    fn parse(key: &str) -> Option<Self> {
        match key {
            "notice" => Some(Self::Notice),
            "news" => Some(Self::News),
            "rules" => Some(Self::Rules),
            "ways" => Some(Self::Ways),
            _ => None,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Notice => "notice",
            Self::News => "news",
            Self::Rules => "rules",
            Self::Ways => "ways",
        }
    }

    fn type_id(self) -> u16 {
        match self {
            Self::Notice => 1154,
            Self::News => 1221,
            Self::Rules => 1536,
            Self::Ways => 1524,
        }
    }

    fn type_name(self) -> &'static str {
        match self {
            Self::Notice => "通知公告",
            Self::News => "校园新闻",
            Self::Rules => "规章制度",
            Self::Ways => "办事指南",
        }
    }
}

/// Resolve `--type` exactly like the Python CLI: an absent or empty value means
/// the default categories, unknown keys are usage errors, and repeats are kept.
pub(super) fn resolve_types(value: Option<&str>) -> CliResult<Option<Vec<NoticeType>>> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let mut resolved = Vec::new();
    for raw in value.split(',') {
        let key = raw.trim().to_ascii_lowercase();
        if key.is_empty() {
            continue;
        }
        let Some(kind) = NoticeType::parse(&key) else {
            return Err(CliError::new(
                2,
                "usage",
                format!("unknown notice type '{key}'"),
                "choose from notice, news, rules, ways",
            ));
        };
        resolved.push(kind);
    }
    Ok((!resolved.is_empty()).then_some(resolved))
}

/// Accept only an absolute HTTP(S) notice page on the portal host itself.
///
/// The URL comes from the user, so it is checked before every request, and the
/// canonical parsed form is what the request uses. The portal uses plain HTTP
/// at its campus-internal address; which scheme the session adapter actually
/// uses, and whether it follows a redirect, stay with that adapter (the
/// transport here never follows redirects itself).
pub(super) fn validate_notice_url(raw: &str) -> CliResult<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(notice_url_error("notice URL must not be empty"));
    }
    let Ok(parsed) = Url::parse(trimmed) else {
        return Err(notice_url_error(
            "notice URL must be an absolute HTTP URL on my.bupt.edu.cn",
        ));
    };
    let allowed = matches!(parsed.scheme(), "http" | "https")
        && parsed.host_str() == Some(PORTAL_HOST)
        && parsed.port().is_none()
        && parsed.username().is_empty()
        && parsed.password().is_none();
    if !allowed {
        return Err(notice_url_error("notice URL must point at my.bupt.edu.cn"));
    }
    Ok(parsed.to_string())
}

fn notice_url_error(message: &str) -> CliError {
    CliError::new(2, "usage", message, "pass a URL printed by `bupt notices`")
}

/// Fetch the requested categories, merge them, sort by publication date
/// descending (stable, empty dates last), then cut to `count`.
#[allow(dead_code)] // exercised offline; wired when the portal session adapter lands
pub(super) fn fetch_notices(
    transport: &mut dyn Transport,
    types: Option<&[NoticeType]>,
    count: u64,
) -> CliResult<Value> {
    let target_types = types.unwrap_or(&DEFAULT_TYPES);
    let mut notices = Vec::new();
    if count > 0 {
        for kind in target_types {
            let url = list_url(*kind);
            let page = get_page(transport, &url, "notice list")?;
            notices.extend(parse_notice_list(&page, *kind)?);
        }
        notices.sort_by(|left, right| notice_sort_time(right).cmp(notice_sort_time(left)));
        notices.truncate(usize::try_from(count).unwrap_or(usize::MAX));
    }
    Ok(json!({
        "count": notices.len(),
        "types": types.map(|types| types.iter().map(|kind| kind.key()).collect::<Vec<_>>()),
        "notices": notices,
    }))
}

/// Fetch one user-supplied notice page and map it to the CLI document.
#[allow(dead_code)] // exercised offline; wired when the portal session adapter lands
pub(super) fn fetch_notice_detail(
    transport: &mut dyn Transport,
    url: &str,
    max_chars: usize,
) -> CliResult<Value> {
    let url = validate_notice_url(url)?;
    let page = get_page(transport, &url, "notice detail")?;
    let detail = parse_notice_detail(&page, &url)?;
    let (content_text, content_truncated) = truncate_chars(detail.content_text, max_chars);
    Ok(json!({
        "notice": {
            "title": detail.title,
            "url": url,
            "author": detail.author,
            "time": detail.time,
            "content_text": content_text,
            "content_truncated": content_truncated,
            "attachments": detail.attachments,
            "images": detail.images,
        }
    }))
}

fn truncate_chars(content: String, max_chars: usize) -> (String, bool) {
    if max_chars == 0 || content.chars().count() <= max_chars {
        return (content, false);
    }
    (content.chars().take(max_chars).collect(), true)
}

/// Fetch the balance widget. The referer and AJAX header are required by the
/// upstream widget and are the only headers this module sets.
#[allow(dead_code)] // exercised offline; wired when the portal session adapter lands
pub(super) fn fetch_card(transport: &mut dyn Transport) -> CliResult<Value> {
    let response = send(
        transport,
        Request {
            method: "GET",
            url: CARD_URL.into(),
            headers: vec![
                ("Referer".into(), CARD_REFERER.into()),
                ("X-Requested-With".into(), "XMLHttpRequest".into()),
            ],
            body: Vec::new(),
        },
        "campus card balance",
    )?;
    check_status(&response, "campus card balance")?;
    let text = String::from_utf8_lossy(&response.body).into_owned();
    if let Some(error) = page_problem(CARD_URL, &text, "campus card balance") {
        return Err(error);
    }
    parse_card(&text)
}

pub(super) fn list_url(kind: NoticeType) -> String {
    format!(
        "{PORTAL_BASE}/list.jsp?totalpage=30&PAGENUM=0&urltype=tree.TreeTempUrl&wbtreeid={}",
        kind.type_id()
    )
}

fn notice_sort_time(notice: &Value) -> &str {
    notice
        .get("time")
        .and_then(Value::as_str)
        .filter(|time| !time.is_empty())
        .unwrap_or(EMPTY_SORT_TIME)
}

fn send(transport: &mut dyn Transport, request: Request, context: &str) -> CliResult<Response> {
    transport.send(request).map_err(|_| {
        CliError::new(
            4,
            "network",
            format!("portal {context} request failed"),
            "my.bupt.edu.cn must be reachable on the campus network; try `bupt diagnose`",
        )
    })
}

/// Read one portal HTML page: status classification first, then the empty-page
/// and login/system-error page checks the Python client performs on HTTP 200.
fn get_page(transport: &mut dyn Transport, url: &str, context: &str) -> CliResult<String> {
    let response = send(
        transport,
        Request {
            method: "GET",
            url: url.into(),
            headers: Vec::new(),
            body: Vec::new(),
        },
        context,
    )?;
    check_status(&response, context)?;
    let text = String::from_utf8_lossy(&response.body).into_owned();
    if text.trim().is_empty() {
        return Err(parse_error(context, "returned an empty page"));
    }
    if let Some(error) = page_problem(url, &text, context) {
        return Err(error);
    }
    Ok(text)
}

fn check_status(response: &Response, context: &str) -> CliResult<()> {
    // A redirect is never followed here. The portal redirects an expired
    // session to CAS, which the Python client treats as an auth problem. A
    // future redirect-following adapter must re-check the destination.
    if response.status == 401 || response.status == 403 || (300..400).contains(&response.status) {
        return Err(auth_error(context));
    }
    if (200..300).contains(&response.status) {
        return Ok(());
    }
    Err(CliError::new(
        5,
        "upstream",
        format!("portal {context} failed: HTTP {}", response.status),
        "retry later; if it persists, run `bupt diagnose`",
    ))
}

fn page_problem(url: &str, html: &str, context: &str) -> Option<CliError> {
    let lower_url = url.to_ascii_lowercase();
    let lower_html = html.to_ascii_lowercase();
    let title = page_title(html);
    let login_url = LOGIN_PATH_MARKERS
        .iter()
        .any(|marker| lower_url.contains(marker));
    let login_html = LOGIN_PATH_MARKERS
        .iter()
        .any(|marker| lower_html.contains(marker))
        || has_attribute_value(html, &["name"], "execution")
        || (has_attribute_value(html, &["name", "type"], "password")
            && (title.contains("登录") || title.contains("login")));
    if login_url || login_html {
        return Some(auth_error(context));
    }
    if lower_url.contains("/system/resource/code/error")
        || SYSTEM_ERROR_TITLES
            .iter()
            .any(|marker| title.contains(marker))
    {
        return Some(parse_error(context, "returned a system error page"));
    }
    None
}

fn page_title(html: &str) -> String {
    let title = elements(html, "title", |_| true)
        .into_iter()
        .next()
        .map(|title| joined_text(&html[title.content_start..title.content_end]))
        .unwrap_or_default();
    collapse_whitespace(&title).to_lowercase()
}

fn auth_error(context: &str) -> CliError {
    CliError::new(
        3,
        "auth",
        format!("portal session expired while fetching {context}"),
        "run `bupt login`",
    )
}

fn parse_error(context: &str, detail: &str) -> CliError {
    CliError::new(
        5,
        "parse",
        format!("portal {context} {detail}"),
        "the portal response format may have changed; retry or run `bupt diagnose`",
    )
}

fn parse_notice_list(html: &str, kind: NoticeType) -> CliResult<Vec<Value>> {
    let lists = elements(html, "ul", |attrs| has_class_token(attrs, "newslist"));
    if lists.is_empty() {
        return Err(parse_error(
            "notice list",
            "is missing the expected newslist element",
        ));
    }

    let mut item_count = 0;
    let mut notices = Vec::new();
    for list in &lists {
        let fragment = &html[list.content_start..list.content_end];
        for item in elements(fragment, "li", |_| true) {
            item_count += 1;
            let body = &fragment[item.content_start..item.content_end];
            let Some(anchor) = elements(body, "a", |_| true).into_iter().next() else {
                continue;
            };
            let Some(href) = attr_value(anchor.attrs, "href") else {
                continue;
            };
            let href = decode_entities(href);
            let href = href.trim();
            if href.is_empty() {
                continue;
            }
            let title = match attr_value(anchor.attrs, "title") {
                Some(title) if !title.is_empty() => decode_entities(title),
                _ => joined_text(&body[anchor.content_start..anchor.content_end]),
            };
            let title = title.trim();
            if title.is_empty() {
                continue;
            }
            let Some(url) = portal_join(PORTAL_ROOT, href) else {
                continue;
            };
            notices.push(json!({
                "title": title,
                "url": url,
                "author": first_span_text(body, "author"),
                "time": first_span_text(body, "time"),
                "type": kind.key(),
                "type_name": kind.type_name(),
            }));
        }
    }

    // The Python client returns an empty list when no <li> exists at all, but
    // rejects a non-empty list whose entries all lack a usable link.
    if item_count == 0 {
        return Ok(Vec::new());
    }
    if notices.is_empty() {
        return Err(parse_error("notice list", "contains no valid notice items"));
    }
    Ok(notices)
}

fn first_span_text(fragment: &str, class_marker: &str) -> String {
    for span in elements(fragment, "span", |attrs| {
        attr_value(attrs, "class")
            .is_some_and(|class| decode_entities(class).contains(class_marker))
    }) {
        let nodes = text_nodes(&fragment[span.content_start..span.content_end]);
        if let Some(first) = nodes.first() {
            return first.trim().to_string();
        }
    }
    String::new()
}

struct NoticeDetail {
    title: String,
    author: String,
    time: String,
    content_text: String,
    attachments: Vec<Value>,
    images: Vec<String>,
}

fn parse_notice_detail(html: &str, page_url: &str) -> CliResult<NoticeDetail> {
    let title = detail_title(html)?;
    let info = info_text(html);
    let author = extract_author(&info);
    let time = extract_time(&info);
    let content = elements(html, "div", content_element)
        .into_iter()
        .next()
        .ok_or_else(|| parse_error("notice detail", "is missing the expected content element"))?;
    let content_text = strip_tags_to_text(&html[content.start..content.end]);
    let body = &html[content.content_start..content.content_end];
    Ok(NoticeDetail {
        title,
        author,
        time,
        content_text,
        attachments: detail_attachments(html),
        images: content_images(body, page_url),
    })
}

fn detail_title(html: &str) -> CliResult<String> {
    let heading = elements(html, "h1", |_| true)
        .into_iter()
        .next()
        .map(|heading| {
            text_nodes(&html[heading.content_start..heading.content_end])
                .iter()
                .map(|node| node.trim())
                .filter(|node| !node.is_empty())
                .collect::<String>()
        })
        .unwrap_or_default();
    if !heading.is_empty() {
        return Ok(heading);
    }

    let page = elements(html, "title", |_| true)
        .into_iter()
        .next()
        .map(|title| joined_text(&html[title.content_start..title.content_end]))
        .unwrap_or_default();
    let page = page.trim();
    let fallback = page.split('-').next().unwrap_or("").trim().to_string();
    if fallback.is_empty() {
        return Err(parse_error("notice detail", "is missing a title"));
    }
    Ok(fallback)
}

fn content_element(attrs: &str) -> bool {
    attr_value(attrs, "id").is_some_and(|id| decode_entities(id).trim() == "vsb_content")
        || attr_value(attrs, "class").is_some_and(|class| {
            let class = decode_entities(class);
            class.contains("v_news_content")
                || class.contains("article-content")
                || class == "content"
        })
}

fn info_text(html: &str) -> String {
    let mut parts = Vec::new();
    for div in elements(html, "div", |attrs| {
        has_class_token(attrs, "article-info") || has_class_token(attrs, "info")
    }) {
        for node in text_nodes(&html[div.content_start..div.content_end]) {
            parts.push(node.trim().to_string());
        }
    }
    collapse_whitespace(&parts.join(" "))
}

fn extract_author(info: &str) -> String {
    let Some(start) = marker_end(info, &AUTHOR_MARKERS) else {
        return String::new();
    };
    let remainder = &info[start..];
    let mut boundary = remainder.len();
    if let Some((date_start, _)) = first_date(remainder) {
        boundary = boundary.min(date_start);
    }
    if let Some(metadata_start) = marker_start(remainder, &METADATA_MARKERS) {
        boundary = boundary.min(metadata_start);
    }
    let author = collapse_whitespace(&remainder[..boundary]);
    author
        .trim_matches(|character| {
            matches!(
                character,
                ' ' | '\t' | '\r' | '\n' | '|' | '｜' | ',' | '，' | ';' | '；'
            )
        })
        .to_string()
}

fn extract_time(info: &str) -> String {
    if let Some(date) = labeled_date(info) {
        return date;
    }
    first_date(info)
        .map(|(_, date)| date.replace('/', "-"))
        .unwrap_or_default()
}

fn labeled_date(info: &str) -> Option<String> {
    for marker in ["发布时间", "发布日期"] {
        let mut from = 0;
        while let Some(relative) = info[from..].find(marker) {
            let after = from + relative + marker.len();
            let mut cursor = skip_whitespace(info, after);
            if let Some(character) = info[cursor..].chars().next()
                && matches!(character, ':' | '：')
            {
                cursor = skip_whitespace(info, cursor + character.len_utf8());
            }
            if let Some(date) = date_at(info, cursor) {
                return Some(date);
            }
            from = after;
        }
    }
    None
}

fn marker_end(text: &str, markers: &[&str]) -> Option<usize> {
    find_marker(text, markers).map(|(_, end)| end)
}

fn marker_start(text: &str, markers: &[&str]) -> Option<usize> {
    find_marker(text, markers).map(|(start, _)| start)
}

/// Find the leftmost `marker` followed by optional whitespace and a colon,
/// returning its start and the index just after the colon.
fn find_marker(text: &str, markers: &[&str]) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    for marker in markers {
        let mut from = 0;
        while let Some(relative) = text[from..].find(marker) {
            let start = from + relative;
            let after = start + marker.len();
            let cursor = skip_whitespace(text, after);
            if let Some(character) = text[cursor..].chars().next()
                && matches!(character, ':' | '：')
            {
                let end = cursor + character.len_utf8();
                if best.is_none_or(|(best_start, _)| start < best_start) {
                    best = Some((start, end));
                }
                break;
            }
            from = after;
        }
    }
    best
}

fn skip_whitespace(text: &str, mut cursor: usize) -> usize {
    while let Some(character) = text[cursor..].chars().next() {
        if !character.is_whitespace() {
            break;
        }
        cursor += character.len_utf8();
    }
    cursor
}

fn first_date(text: &str) -> Option<(usize, String)> {
    (0..text.len()).find_map(|index| date_at(text, index).map(|date| (index, date)))
}

fn date_at(text: &str, index: usize) -> Option<String> {
    let bytes = text.as_bytes();
    if index + 10 > bytes.len()
        || !(0..4).all(|offset| bytes[index + offset].is_ascii_digit())
        || !matches!(bytes[index + 4], b'-' | b'/')
        || !(5..7).all(|offset| bytes[index + offset].is_ascii_digit())
        || !matches!(bytes[index + 7], b'-' | b'/')
        || !(8..10).all(|offset| bytes[index + offset].is_ascii_digit())
    {
        return None;
    }
    Some(text[index..index + 10].replace('/', "-"))
}

fn detail_attachments(html: &str) -> Vec<Value> {
    let mut attachments = Vec::new();
    for anchor in elements(html, "a", |_| true) {
        let Some(href) = attr_value(anchor.attrs, "href") else {
            continue;
        };
        let href = decode_entities(href);
        if href.is_empty() || href.contains("logout") {
            continue;
        }
        if !(href.contains("download.jsp")
            || href.contains("DownloadAttachUrl")
            || href.contains("dfiles"))
        {
            continue;
        }
        let title = joined_text(&html[anchor.content_start..anchor.content_end])
            .trim()
            .to_string();
        let title = if title.is_empty() {
            "附件".to_string()
        } else {
            title
        };
        let Some(url) = portal_join(PORTAL_ROOT, &href) else {
            continue;
        };
        attachments.push(json!({"title": title, "url": url}));
    }
    attachments
}

fn content_images(content: &str, page_url: &str) -> Vec<String> {
    let base = if page_url.trim().is_empty() {
        PORTAL_ROOT
    } else {
        page_url.trim()
    };
    let mut images: Vec<String> = Vec::new();
    for image in elements(content, "img", |_| true) {
        let Some(src) = attr_value(image.attrs, "src") else {
            continue;
        };
        let src = decode_entities(src);
        let src = src.trim();
        if src.is_empty() || is_decoration_image(src) {
            continue;
        }
        let Some(url) = portal_join(base, src) else {
            continue;
        };
        if !images.contains(&url) {
            images.push(url);
        }
    }
    images
}

fn is_decoration_image(src: &str) -> bool {
    let lower = src.to_ascii_lowercase();
    if lower.ends_with(".gif") && (lower.contains("arrow") || lower.contains("dot")) {
        return true;
    }
    [
        "topback",
        "drop-shadow",
        "shadow",
        "logo",
        "icon",
        "button",
        "spacer",
        "banner",
        "nav_",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn portal_join(base: &str, reference: &str) -> Option<String> {
    Url::parse(base)
        .ok()?
        .join(reference)
        .ok()
        .map(|url| url.to_string())
}

fn parse_card(text: &str) -> CliResult<Value> {
    let payload: Value = serde_json::from_str(text)
        .map_err(|_| parse_error("campus card balance", "returned invalid JSON"))?;
    let Some(object) = payload.as_object() else {
        return Err(parse_error(
            "campus card balance",
            "payload must be a JSON object",
        ));
    };
    Ok(json!({
        "student_id": raw_value(object.get("a")).unwrap_or_default(),
        "main_wallet": to_float(raw_value(object.get("qkje"))),
        "subsidy": to_float(raw_value(object.get("oddfare"))),
        "unread_mail": to_int(raw_value(object.get("mail"))),
    }))
}

/// Mirror Python's `"" if value is None else str(value)` for the raw widget.
fn raw_value(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Null => Some(String::new()),
        other => Some(python_str(other)),
    }
}

fn python_str(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn to_float(value: Option<String>) -> Option<f64> {
    let value = value?;
    value
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
}

fn to_int(value: Option<String>) -> Option<i64> {
    to_float(value).map(|value| value as i64)
}

fn has_class_token(attrs: &str, token: &str) -> bool {
    attr_value(attrs, "class").is_some_and(|class| {
        decode_entities(class)
            .split_whitespace()
            .any(|value| value == token)
    })
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn joined_text(fragment: &str) -> String {
    text_nodes(fragment).concat()
}

fn text_nodes(fragment: &str) -> Vec<String> {
    let mut nodes = Vec::new();
    for token in Tokenizer::new(fragment) {
        if let Token::Text(text) = token {
            nodes.push(decode_entities(text));
        }
    }
    nodes
}

/// Python's `html_to_text`: every tag becomes a space, entities are unescaped,
/// and the result is trimmed. Inner whitespace is preserved.
fn strip_tags_to_text(html: &str) -> String {
    let mut output = String::with_capacity(html.len());
    for token in Tokenizer::new(html) {
        match token {
            Token::Text(text) => output.push_str(&decode_entities(text)),
            Token::Tag(_) | Token::Other => output.push(' '),
        }
    }
    output.trim().to_string()
}

fn decode_entities(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find('&') {
        output.push_str(&rest[..index]);
        let tail = &rest[index..];
        if let Some(end) = tail.find(';') {
            let entity = &tail[1..end];
            if let Some(character) = decode_entity(entity) {
                output.push(character);
                rest = &tail[end + 1..];
                continue;
            }
        }
        output.push('&');
        rest = &tail[1..];
    }
    output.push_str(rest);
    output
}

fn decode_entity(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some('\u{a0}'),
        hex if hex.starts_with("#x") || hex.starts_with("#X") => {
            char::from_u32(u32::from_str_radix(&hex[2..], 16).ok()?)
        }
        decimal if decimal.starts_with('#') => char::from_u32(decimal[1..].parse::<u32>().ok()?),
        _ => None,
    }
}

/// Detect `attribute="value"` / `attribute='value'` anywhere in the markup,
/// which is what the Python page-context regexes effectively check.
fn has_attribute_value(html: &str, names: &[&str], value: &str) -> bool {
    let bytes = html.as_bytes();
    for index in 0..bytes.len() {
        for name in names {
            if !starts_with_ignore_ascii_case(bytes, index, name.as_bytes()) {
                continue;
            }
            let mut cursor = index + name.len();
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            if cursor >= bytes.len() || bytes[cursor] != b'=' {
                continue;
            }
            cursor += 1;
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            if cursor >= bytes.len() || (bytes[cursor] != b'"' && bytes[cursor] != b'\'') {
                continue;
            }
            let quote = bytes[cursor];
            cursor += 1;
            if starts_with_ignore_ascii_case(bytes, cursor, value.as_bytes()) {
                let end = cursor + value.len();
                if end < bytes.len() && bytes[end] == quote {
                    return true;
                }
            }
        }
    }
    false
}

fn starts_with_ignore_ascii_case(bytes: &[u8], index: usize, needle: &[u8]) -> bool {
    index + needle.len() <= bytes.len()
        && bytes[index..index + needle.len()]
            .iter()
            .zip(needle)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
}

struct Element<'a> {
    attrs: &'a str,
    start: usize,
    content_start: usize,
    content_end: usize,
    end: usize,
}

/// Return the outermost elements named `name` that satisfy `accept`, in
/// document order. Descendants of a match are not reported separately.
fn elements<'a>(
    text: &'a str,
    name: &str,
    mut accept: impl FnMut(&str) -> bool,
) -> Vec<Element<'a>> {
    let mut found = Vec::new();
    let mut tokenizer = Tokenizer::new(text);
    while let Some(token) = tokenizer.next() {
        let Token::Tag(tag) = token else {
            continue;
        };
        if tag.closing || !tag.name.eq_ignore_ascii_case(name) || !accept(tag.attrs) {
            continue;
        }
        if tag.self_closing || is_void(tag.name) {
            found.push(Element {
                attrs: tag.attrs,
                start: tag.start,
                content_start: tag.end,
                content_end: tag.end,
                end: tag.end,
            });
            continue;
        }
        match matching_close(text, tag.end, tag.name) {
            Some((close_start, close_end)) => {
                found.push(Element {
                    attrs: tag.attrs,
                    start: tag.start,
                    content_start: tag.end,
                    content_end: close_start,
                    end: close_end,
                });
                tokenizer.seek(close_end);
            }
            None => {
                found.push(Element {
                    attrs: tag.attrs,
                    start: tag.start,
                    content_start: tag.end,
                    content_end: text.len(),
                    end: text.len(),
                });
                tokenizer.seek(text.len());
            }
        }
    }
    found
}

fn matching_close(text: &str, from: usize, name: &str) -> Option<(usize, usize)> {
    let mut depth = 1usize;
    let mut tokenizer = Tokenizer::new(text);
    tokenizer.seek(from);
    for token in tokenizer {
        let Token::Tag(tag) = token else {
            continue;
        };
        if !tag.name.eq_ignore_ascii_case(name) {
            continue;
        }
        if tag.closing {
            depth -= 1;
            if depth == 0 {
                return Some((tag.start, tag.end));
            }
        } else if !tag.self_closing && !is_void(tag.name) {
            depth += 1;
        }
    }
    None
}

fn is_void(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

fn is_raw_text(name: &str) -> bool {
    name.eq_ignore_ascii_case("script") || name.eq_ignore_ascii_case("style")
}

fn attr_value<'a>(attrs: &'a str, name: &str) -> Option<&'a str> {
    let bytes = attrs.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        while index < bytes.len() && (bytes[index].is_ascii_whitespace() || bytes[index] == b'/') {
            index += 1;
        }
        let key_start = index;
        while index < bytes.len()
            && !bytes[index].is_ascii_whitespace()
            && bytes[index] != b'='
            && bytes[index] != b'/'
            && bytes[index] != b'>'
        {
            index += 1;
        }
        let key = &attrs[key_start..index];
        if key.is_empty() {
            index += 1;
            continue;
        }
        let mut cursor = index;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() || bytes[cursor] != b'=' {
            if key.eq_ignore_ascii_case(name) {
                return Some("");
            }
            index = cursor.max(index + 1);
            continue;
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let (value, next) =
            if cursor < bytes.len() && (bytes[cursor] == b'"' || bytes[cursor] == b'\'') {
                let quote = bytes[cursor];
                let value_start = cursor + 1;
                let mut end = value_start;
                while end < bytes.len() && bytes[end] != quote {
                    end += 1;
                }
                (&attrs[value_start..end], (end + 1).min(bytes.len()))
            } else {
                let value_start = cursor;
                let mut end = cursor;
                while end < bytes.len() && !bytes[end].is_ascii_whitespace() {
                    end += 1;
                }
                (&attrs[value_start..end], end)
            };
        if key.eq_ignore_ascii_case(name) {
            return Some(value);
        }
        index = next.max(index + 1);
    }
    None
}

enum Token<'a> {
    Text(&'a str),
    Tag(Tag<'a>),
    Other,
}

struct Tag<'a> {
    name: &'a str,
    attrs: &'a str,
    closing: bool,
    self_closing: bool,
    start: usize,
    end: usize,
}

struct Tokenizer<'a> {
    text: &'a str,
    cursor: usize,
}

impl<'a> Tokenizer<'a> {
    fn new(text: &'a str) -> Self {
        Self { text, cursor: 0 }
    }

    fn seek(&mut self, position: usize) {
        self.cursor = position;
    }

    fn skip_raw_text(&mut self, name: &str, from: usize) {
        let rest = &self.text[from..];
        let lower = rest.to_ascii_lowercase();
        let needle = format!("</{}", name.to_ascii_lowercase());
        match lower.find(&needle) {
            Some(relative) => {
                let close_start = from + relative;
                let end = self.text[close_start..]
                    .find('>')
                    .map(|index| close_start + index + 1)
                    .unwrap_or(self.text.len());
                self.cursor = end;
            }
            None => self.cursor = self.text.len(),
        }
    }
}

impl<'a> Iterator for Tokenizer<'a> {
    type Item = Token<'a>;

    fn next(&mut self) -> Option<Token<'a>> {
        let bytes = self.text.as_bytes();
        if self.cursor >= bytes.len() {
            return None;
        }
        let Some(relative) = self.text[self.cursor..].find('<') else {
            let text = &self.text[self.cursor..];
            self.cursor = bytes.len();
            return Some(Token::Text(text));
        };
        let start = self.cursor + relative;
        if start > self.cursor {
            let text = &self.text[self.cursor..start];
            self.cursor = start;
            return Some(Token::Text(text));
        }

        let rest = &self.text[start..];
        let after = &rest[1..];
        if after.starts_with("!--") {
            self.cursor = rest
                .find("-->")
                .map(|index| start + index + 3)
                .unwrap_or(bytes.len());
            return Some(Token::Other);
        }
        if after.starts_with('!') || after.starts_with('?') {
            self.cursor = rest
                .find('>')
                .map(|index| start + index + 1)
                .unwrap_or(bytes.len());
            return Some(Token::Other);
        }

        let closing = after.starts_with('/');
        let name_start = 1 + usize::from(closing);
        if !matches!(
            rest.as_bytes().get(name_start),
            Some(byte) if byte.is_ascii_alphabetic()
        ) {
            self.cursor = start + 1;
            return Some(Token::Text("<"));
        }
        let mut name_end = name_start;
        while name_end < rest.len()
            && !rest.as_bytes()[name_end].is_ascii_whitespace()
            && rest.as_bytes()[name_end] != b'>'
            && rest.as_bytes()[name_end] != b'/'
        {
            name_end += 1;
        }
        let name = &rest[name_start..name_end];

        let mut quote: Option<u8> = None;
        let mut index = name_end;
        while index < rest.len() {
            let byte = rest.as_bytes()[index];
            match quote {
                Some(open) if byte == open => quote = None,
                Some(_) => {}
                None if byte == b'"' || byte == b'\'' => quote = Some(byte),
                None if byte == b'>' => break,
                None => {}
            }
            index += 1;
        }
        let attrs = &rest[name_end..index];
        let end = (start + index + 1).min(bytes.len());
        let self_closing = attrs.trim_end().ends_with('/');
        self.cursor = end;
        if !closing && !self_closing && is_raw_text(name) {
            self.skip_raw_text(name, end);
        }
        Some(Token::Tag(Tag {
            name,
            attrs,
            closing,
            self_closing,
            start,
            end,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::testing::Scripted;
    use crate::transport::{Request, Response};
    use serde_json::json;
    use std::fs;
    use std::path::Path;

    const DETAIL_URL: &str = "http://my.bupt.edu.cn/2026/1101/c1154a000001/page.htm";

    fn fixture(name: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/contracts/campus")
            .join(name);
        fs::read_to_string(path).expect("read synthetic fixture")
    }

    fn response(status: u16, body: impl Into<String>) -> CliResult<Response> {
        Ok(Response {
            status,
            headers: Vec::new(),
            body: body.into().into_bytes(),
        })
    }

    fn news_list_html() -> String {
        r#"<!doctype html>
<html lang="zh-CN">
<head><meta charset="utf-8"><title>校园新闻</title></head>
<body>
  <ul class="nav newslist">
    <li>
      <a href="/2026/1102/c1221a000003/page.htm"><span>示例新闻一</span></a>
      <span class="list-author">示例新闻部门</span>
      <span class="list-time">2026-11-02</span>
    </li>
  </ul>
</body>
</html>"#
            .into()
    }

    fn empty_newslist_html() -> String {
        r#"<html><body><ul class="newslist"></ul></body></html>"#.into()
    }

    fn invalid_items_html() -> String {
        r#"<html><body><ul class="newslist"><li><a href="">没有链接</a></li></ul></body></html>"#
            .into()
    }

    fn notice_response(transport: &mut Scripted, status: u16, body: String) {
        transport
            .expected
            .push_back(("GET", list_url(NoticeType::Notice), response(status, body)));
    }

    #[test]
    fn notice_types_resolve_defaults_case_insensitively_and_reject_unknown() {
        assert!(resolve_types(None).unwrap().is_none());
        assert!(resolve_types(Some("")).unwrap().is_none());
        assert!(resolve_types(Some(", ,")).unwrap().is_none());
        assert_eq!(
            resolve_types(Some("NOTICE, News")).unwrap(),
            Some(vec![NoticeType::Notice, NoticeType::News])
        );
        assert_eq!(
            resolve_types(Some("rules,ways")).unwrap(),
            Some(vec![NoticeType::Rules, NoticeType::Ways])
        );

        let error = resolve_types(Some("notice,bogus")).unwrap_err();
        assert_eq!((error.code, error.kind), (2, "usage"));
        assert!(
            error.message.contains("bogus"),
            "unknown type must be named; message={}",
            error.message
        );
        assert!(
            error.hint.contains("notice") && error.hint.contains("rules"),
            "hint must list categories; hint={}",
            error.hint
        );
    }

    #[test]
    fn notices_fetch_default_types_and_sort_by_time_descending() {
        let mut transport = Scripted::default();
        notice_response(&mut transport, 200, fixture("portal_notices_notice.html"));
        transport.expected.push_back((
            "GET",
            list_url(NoticeType::News),
            response(200, news_list_html()),
        ));

        let data = fetch_notices(&mut transport, None, 10).unwrap();

        assert_eq!(data["count"], 3);
        assert!(data["types"].is_null());
        assert_eq!(data["notices"][0]["title"], "示例新闻一");
        assert_eq!(data["notices"][0]["type"], "news");
        assert_eq!(data["notices"][0]["type_name"], "校园新闻");
        assert_eq!(data["notices"][0]["author"], "示例新闻部门");
        assert_eq!(data["notices"][0]["time"], "2026-11-02");
        assert_eq!(
            data["notices"][0]["url"],
            "http://my.bupt.edu.cn/2026/1102/c1221a000003/page.htm"
        );
        assert_eq!(data["notices"][1]["title"], "示例通知一");
        assert_eq!(data["notices"][1]["type"], "notice");
        assert_eq!(data["notices"][1]["type_name"], "通知公告");
        assert_eq!(data["notices"][1]["author"], "示例发布部门");
        assert_eq!(data["notices"][1]["time"], "2026-11-01");
        assert_eq!(
            data["notices"][1]["url"],
            "http://my.bupt.edu.cn/2026/1101/c1154a000001/page.htm"
        );
        assert_eq!(data["notices"][2]["title"], "示例通知二");
        assert_eq!(data["notices"][2]["time"], "2026-10-31");

        assert_eq!(transport.calls.len(), 2);
        assert_eq!(transport.calls[0].method, "GET");
        assert_eq!(transport.calls[0].url, list_url(NoticeType::Notice));
        assert_eq!(transport.calls[1].url, list_url(NoticeType::News));
        assert!(transport.calls[0].headers.is_empty());
        assert!(transport.calls[0].body.is_empty());
    }

    #[test]
    fn notices_truncate_after_sorting_and_echo_requested_types() {
        let mut transport = Scripted::default();
        notice_response(&mut transport, 200, fixture("portal_notices_notice.html"));
        transport.expected.push_back((
            "GET",
            list_url(NoticeType::News),
            response(200, news_list_html()),
        ));

        let data = fetch_notices(&mut transport, None, 1).unwrap();

        assert_eq!(data["count"], 1);
        assert_eq!(data["notices"][0]["time"], "2026-11-02");
    }

    #[test]
    fn notices_zero_count_returns_empty_without_requests() {
        let mut transport = Scripted::default();
        let types = [NoticeType::Rules];

        let data = fetch_notices(&mut transport, Some(&types), 0).unwrap();

        assert_eq!(data["count"], 0);
        assert_eq!(data["types"], json!(["rules"]));
        assert_eq!(data["notices"], json!([]));
        assert!(transport.calls.is_empty());
    }

    #[test]
    fn notices_accepts_an_empty_newslist_but_rejects_listless_pages() {
        let mut transport = Scripted::default();
        notice_response(&mut transport, 200, empty_newslist_html());
        let data = fetch_notices(&mut transport, Some(&[NoticeType::Notice]), 5).unwrap();
        assert_eq!(data["count"], 0);
        assert_eq!(data["notices"], json!([]));

        let mut transport = Scripted::default();
        notice_response(
            &mut transport,
            200,
            "<html><body>no list</body></html>".into(),
        );
        let error = fetch_notices(&mut transport, Some(&[NoticeType::Notice]), 5).unwrap_err();
        assert_eq!((error.code, error.kind), (5, "parse"));
        assert!(
            error.message.contains("newslist"),
            "message must name the missing element; message={}",
            error.message
        );

        let mut transport = Scripted::default();
        notice_response(&mut transport, 200, invalid_items_html());
        let error = fetch_notices(&mut transport, Some(&[NoticeType::Notice]), 5).unwrap_err();
        assert_eq!((error.code, error.kind), (5, "parse"));
    }

    #[test]
    fn notice_list_classifies_login_and_system_error_pages() {
        let mut transport = Scripted::default();
        notice_response(&mut transport, 200, fixture("portal_login_page.html"));
        let error = fetch_notices(&mut transport, Some(&[NoticeType::Notice]), 5).unwrap_err();
        assert_eq!((error.code, error.kind), (3, "auth"));
        assert!(
            error.hint.contains("login"),
            "auth hint must point at login; hint={}",
            error.hint
        );

        let mut transport = Scripted::default();
        notice_response(
            &mut transport,
            200,
            "<html><head><title>页面错误</title></head><body></body></html>".into(),
        );
        let error = fetch_notices(&mut transport, Some(&[NoticeType::Notice]), 5).unwrap_err();
        assert_eq!((error.code, error.kind), (5, "parse"));
    }

    #[test]
    fn portal_status_codes_map_to_auth_upstream_and_network_without_diagnostics() {
        for status in [302, 401, 403] {
            let mut transport = Scripted::default();
            notice_response(&mut transport, status, "<html></html>".into());
            let error = fetch_notices(&mut transport, Some(&[NoticeType::Notice]), 5).unwrap_err();
            assert_eq!((error.code, error.kind), (3, "auth"), "status {status}");
        }

        for status in [404, 500] {
            let mut transport = Scripted::default();
            notice_response(&mut transport, status, "<html></html>".into());
            let error = fetch_notices(&mut transport, Some(&[NoticeType::Notice]), 5).unwrap_err();
            assert_eq!((error.code, error.kind), (5, "upstream"), "status {status}");
        }

        const INTERNAL: &str = "connect failed: 10.3.55.84:80 secret detail";
        let mut transport = Scripted::default();
        transport.expected.push_back((
            "GET",
            CARD_URL.into(),
            Err(CliError::new(4, "network", INTERNAL, "")),
        ));
        let error = fetch_card(&mut transport).unwrap_err();
        assert_eq!((error.code, error.kind), (4, "network"));
        assert!(!error.message.contains("10.3.55.84"));
        assert!(!error.message.contains(INTERNAL));
        assert!(
            error.hint.contains("diagnose"),
            "network hint must point at diagnose; hint={}",
            error.hint
        );

        let mut transport = Scripted::default();
        notice_response(&mut transport, 200, "".into());
        let error = fetch_notices(&mut transport, Some(&[NoticeType::Notice]), 5).unwrap_err();
        assert_eq!((error.code, error.kind), (5, "parse"));
    }

    #[test]
    fn notice_detail_maps_content_links_and_images() {
        let mut transport = Scripted::default();
        transport.expected.push_back((
            "GET",
            DETAIL_URL.into(),
            response(200, fixture("portal_notice_detail.html")),
        ));

        let data = fetch_notice_detail(&mut transport, DETAIL_URL, 0).unwrap();
        let notice = &data["notice"];

        assert_eq!(notice["title"], "示例通知一");
        assert_eq!(notice["url"], DETAIL_URL);
        assert_eq!(notice["author"], "示例发布部门");
        assert_eq!(notice["time"], "2026-11-01");
        assert_eq!(notice["content_truncated"], false);
        let content = notice["content_text"].as_str().expect("content_text");
        assert!(content.contains("第一段示例正文。"), "content={content}");
        assert!(content.contains("第二段示例正文。"), "content={content}");
        assert!(content.contains("装饰图标应被忽略。"), "content={content}");
        assert!(
            !content.contains('<'),
            "content must be plain text: {content}"
        );
        assert_eq!(
            notice["images"],
            json!(["http://my.bupt.edu.cn/2026/1101/c1154a000001/images/fixture-content.png"])
        );
        assert_eq!(
            notice["attachments"],
            json!([{
                "title": "示例附件.pdf",
                "url": "http://my.bupt.edu.cn/system/_content/download.jsp?urltype=news.DownloadAttachUrl&id=FIXTURE-ATTACH-1",
            }])
        );
        assert_eq!(transport.calls.len(), 1);
        assert_eq!(transport.calls[0].method, "GET");
        assert!(transport.calls[0].headers.is_empty());
    }

    #[test]
    fn notice_detail_truncates_content_by_characters() {
        let mut transport = Scripted::default();
        transport.expected.push_back((
            "GET",
            DETAIL_URL.into(),
            response(200, fixture("portal_notice_detail.html")),
        ));
        let full = fetch_notice_detail(&mut transport, DETAIL_URL, 0).unwrap();
        let full_chars = full["notice"]["content_text"]
            .as_str()
            .expect("content_text")
            .chars()
            .count();

        let mut transport = Scripted::default();
        transport.expected.push_back((
            "GET",
            DETAIL_URL.into(),
            response(200, fixture("portal_notice_detail.html")),
        ));
        let data = fetch_notice_detail(&mut transport, DETAIL_URL, 6).unwrap();
        let notice = &data["notice"];

        assert_eq!(notice["content_truncated"], true);
        assert_eq!(
            notice["content_text"]
                .as_str()
                .expect("content_text")
                .chars()
                .count(),
            6
        );
        assert_eq!(notice["content_text"], "第一段示例正");
        assert!(full_chars > 6);
    }

    #[test]
    fn notice_detail_falls_back_to_the_page_title_and_normalizes_slash_dates() {
        let mut transport = Scripted::default();
        let body = r#"<html><head><title>回退标题 - 信息门户</title></head><body>
  <div class="info">作者：示例作者&nbsp;|&nbsp;发布日期：2026/03/09&nbsp;|&nbsp;来源：示例来源</div>
  <div class="v_news_content">正文内容</div>
</body></html>"#;
        transport
            .expected
            .push_back(("GET", DETAIL_URL.into(), response(200, body)));

        let data = fetch_notice_detail(&mut transport, DETAIL_URL, 0).unwrap();
        let notice = &data["notice"];

        assert_eq!(notice["title"], "回退标题");
        assert_eq!(notice["author"], "示例作者");
        assert_eq!(notice["time"], "2026-03-09");
        assert_eq!(notice["content_text"], "正文内容");
        assert_eq!(notice["images"], json!([]));
        assert_eq!(notice["attachments"], json!([]));
    }

    #[test]
    fn notice_detail_ignores_script_bodies_in_content_and_images() {
        let mut transport = Scripted::default();
        let body = r#"<html><head><title>脚本体 - 信息门户</title></head><body>
  <div class="content">正文<img src="/images/body.png"><script>var marker = "<img src='/images/logo.png'>";</script></div>
</body></html>"#;
        transport
            .expected
            .push_back(("GET", DETAIL_URL.into(), response(200, body)));

        let data = fetch_notice_detail(&mut transport, DETAIL_URL, 0).unwrap();
        let notice = &data["notice"];

        assert_eq!(notice["title"], "脚本体");
        assert_eq!(notice["content_text"], "正文");
        assert_eq!(
            notice["images"],
            json!(["http://my.bupt.edu.cn/images/body.png"])
        );
    }

    #[test]
    fn notice_detail_requires_a_title_and_a_content_element() {
        let mut transport = Scripted::default();
        transport.expected.push_back((
            "GET",
            DETAIL_URL.into(),
            response(
                200,
                "<html><body><div id=\"vsb_content\">正文</div></body></html>",
            ),
        ));
        let error = fetch_notice_detail(&mut transport, DETAIL_URL, 0).unwrap_err();
        assert_eq!((error.code, error.kind), (5, "parse"));
        assert!(error.message.contains("title"), "message={}", error.message);

        let mut transport = Scripted::default();
        transport.expected.push_back((
            "GET",
            DETAIL_URL.into(),
            response(200, "<html><body><h1>只有标题</h1></body></html>"),
        ));
        let error = fetch_notice_detail(&mut transport, DETAIL_URL, 0).unwrap_err();
        assert_eq!((error.code, error.kind), (5, "parse"));
        assert!(
            error.message.contains("content"),
            "message={}",
            error.message
        );
    }

    #[test]
    fn notice_detail_validates_the_destination_before_any_request() {
        let mut transport = Scripted::default();
        for url in [
            "",
            "   ",
            "/2026/1101/c1154a000001/page.htm",
            "https://example.invalid/notice/1",
            "http://my.bupt.edu.cn.evil.invalid/notice/1",
            "ftp://my.bupt.edu.cn/notice/1",
            "http://my.bupt.edu.cn:8080/notice/1",
            "http://user:pass@my.bupt.edu.cn/notice/1",
        ] {
            let error = fetch_notice_detail(&mut transport, url, 0).unwrap_err();
            assert_eq!(
                (error.code, error.kind),
                (2, "usage"),
                "URL {url:?} must be rejected before a request"
            );
            assert!(
                error.hint.contains("bupt notices"),
                "hint must point at the listing command; hint={}",
                error.hint
            );
        }
        assert!(transport.calls.is_empty());

        assert_eq!(
            validate_notice_url("  https://my.bupt.edu.cn/2026/a/page.htm ").unwrap(),
            "https://my.bupt.edu.cn/2026/a/page.htm"
        );
        assert_eq!(
            validate_notice_url("http://MY.BUPT.EDU.CN/2026/a/page.htm").unwrap(),
            "http://my.bupt.edu.cn/2026/a/page.htm"
        );
    }

    #[test]
    fn card_maps_amounts_and_widget_fields() {
        let mut transport = Scripted::default();
        transport.expected.push_back((
            "GET",
            CARD_URL.into(),
            response(200, fixture("portal_card.json")),
        ));

        let data = fetch_card(&mut transport).unwrap();

        assert_eq!(data["student_id"], "fixture-student");
        assert_eq!(data["main_wallet"], json!(123.45));
        assert_eq!(data["subsidy"], json!(6.78));
        assert_eq!(data["unread_mail"], json!(3));
        assert_eq!(transport.calls.len(), 1);
        let request: &Request = &transport.calls[0];
        assert_eq!(request.method, "GET");
        assert_eq!(request.url, CARD_URL);
        assert!(
            request
                .headers
                .contains(&("Referer".into(), CARD_REFERER.into()))
        );
        assert!(
            request
                .headers
                .contains(&("X-Requested-With".into(), "XMLHttpRequest".into()))
        );
        assert!(request.body.is_empty());
    }

    #[test]
    fn card_keeps_missing_and_malformed_values_null() {
        let mut transport = Scripted::default();
        transport.expected.push_back((
            "GET",
            CARD_URL.into(),
            response(200, r#"{"qkje":"","oddfare":null,"mail":"oops","a":null}"#),
        ));
        let data = fetch_card(&mut transport).unwrap();
        assert!(data["main_wallet"].is_null());
        assert!(data["subsidy"].is_null());
        assert!(data["unread_mail"].is_null());
        assert_eq!(data["student_id"], "");

        let mut transport = Scripted::default();
        transport.expected.push_back((
            "GET",
            CARD_URL.into(),
            response(200, r#"{"qkje":12.5,"mail":"3.9","a":2026000001}"#),
        ));
        let data = fetch_card(&mut transport).unwrap();
        assert_eq!(data["main_wallet"], json!(12.5));
        assert!(data["subsidy"].is_null());
        assert_eq!(data["unread_mail"], json!(3));
        assert_eq!(data["student_id"], "2026000001");
    }

    #[test]
    fn card_rejects_non_json_payloads_as_parse_errors() {
        for body in [
            fixture("portal_card_invalid.txt"),
            "[]".into(),
            "\"text\"".into(),
        ] {
            let mut transport = Scripted::default();
            transport
                .expected
                .push_back(("GET", CARD_URL.into(), response(200, body)));
            let error = fetch_card(&mut transport).unwrap_err();
            assert_eq!((error.code, error.kind), (5, "parse"));
            assert!(!error.message.contains("qkje"));
        }
    }

    #[test]
    fn card_login_page_is_an_auth_error() {
        let mut transport = Scripted::default();
        transport.expected.push_back((
            "GET",
            CARD_URL.into(),
            response(200, fixture("portal_login_page.html")),
        ));
        let error = fetch_card(&mut transport).unwrap_err();
        assert_eq!((error.code, error.kind), (3, "auth"));
        assert!(error.hint.contains("login"));
    }
}
