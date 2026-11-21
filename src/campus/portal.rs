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
//! HTML is scanned by the shared hand-written scanner in [`crate::html`] because
//! the Rust package keeps no HTML parser dependency.

use super::session::{Route, ScopedTransport, Service, SessionDriver};
use crate::cli::{CliError, CliResult};
use crate::files;
use crate::html::{
    attr_value, collapse_whitespace, decode_entities, elements, has_attribute_value,
    has_class_token, joined_text, strip_tags, text_nodes,
};
use crate::transport::{Request, Response, Transport};
use serde_json::{Value, json};
use std::path::Path;
use url::Url;

const PORTAL_BASE: &str = "http://my.bupt.edu.cn";
const PORTAL_ROOT: &str = "http://my.bupt.edu.cn/";
const PORTAL_HOST: &str = "my.bupt.edu.cn";
// One source of truth: the transport layer pins these paths too, and a second
// literal here could drift into a policy mismatch that only shows up at runtime.
const ATTACHMENT_PATH: &str = crate::transport::portal_scope::DOWNLOAD;
const CALLBACK_PATH: &str = crate::transport::portal_scope::CALLBACK;

pub(super) const CARD_URL: &str = "http://my.bupt.edu.cn/system/resource/app/cuser/getwxtsA.jsp";
pub(super) const CARD_REFERER: &str = crate::transport::portal_scope::CARD_REFERER;

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
    // The page must also be one the transport will fetch. `portal_url` is the
    // same predicate the wire layer applies, so a URL accepted here is never
    // refused one layer down as a policy violation (which would surface as an
    // upstream failure for what is really a user input mistake). The parsed form
    // is used because it is already lower-cased and port-normalised, which is
    // exactly what the transport will send.
    let canonical = parsed.to_string();
    if crate::transport::portal_scope::portal_url(&canonical, false).is_none() {
        return Err(notice_url_error(
            "notice URL must be a notice page on my.bupt.edu.cn",
        ));
    }
    // An attachment link is a file, not a page. Refuse it here instead of
    // fetching a binary and looking for notice markup inside it.
    if parsed.path() == ATTACHMENT_PATH {
        return Err(CliError::new(
            2,
            "usage",
            "that is a notice attachment link, not a notice page",
            "pass the notice page URL; `bupt notices download` saves its attachments",
        ));
    }
    // The CAS callback signs the session in. It is not a page to read, and it
    // must not be fetched with portal cookies on a user-supplied URL.
    if parsed.path() == CALLBACK_PATH {
        return Err(CliError::new(
            2,
            "usage",
            "that is the portal login callback, not a notice page",
            "pass the notice page URL from `bupt notices` or `bupt notices search`",
        ));
    }
    Ok(canonical)
}

fn notice_url_error(message: &str) -> CliError {
    CliError::new(2, "usage", message, "pass a URL printed by `bupt notices`")
}

/// Accept only the attachment link the portal itself renders under a notice.
/// The path and its three fields are pinned by the portal scope, so a user
/// cannot turn this into a fetch of an arbitrary portal file.
pub(super) fn validate_attachment_url(raw: &str) -> CliResult<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(attachment_url_error("attachment URL must not be empty"));
    }
    let url = crate::transport::portal_scope::portal_url(trimmed, false)
        .filter(|url| url.path() == ATTACHMENT_PATH)
        .ok_or_else(|| {
            attachment_url_error("attachment URL must be a my.bupt.edu.cn notice attachment link")
        })?;
    Ok(url.to_string())
}

/// The `Referer` a download must carry: a portal page the session can read.
/// The portal serves the file only when some portal page is named; it accepts
/// any of them, so this is not pinned to the page the link came from.
pub(super) fn is_attachment_referer(value: &str) -> bool {
    crate::transport::portal_scope::portal_url(value, false).is_some()
        && !value.contains(ATTACHMENT_PATH)
}

fn attachment_url_error(message: &str) -> CliError {
    CliError::new(
        2,
        "usage",
        message,
        "pass the notice page URL from `bupt notices` or `bupt notices search`; its attachments are saved together",
    )
}

/// Fetch the requested categories, merge them, sort by publication date
/// descending (stable, empty dates last), then cut to `count`.
#[cfg(test)]
pub(super) fn fetch_notices(
    transport: &mut dyn Transport,
    types: Option<&[NoticeType]>,
    count: u64,
) -> CliResult<Value> {
    collect_notices(types, count, |kind| {
        get_page(transport, &list_url(kind), "notice list")
    })
}

fn collect_notices(
    types: Option<&[NoticeType]>,
    count: u64,
    mut page: impl FnMut(NoticeType) -> CliResult<String>,
) -> CliResult<Value> {
    let target_types = types.unwrap_or(&DEFAULT_TYPES);
    let mut notices = Vec::new();
    if count > 0 {
        for kind in target_types {
            let html = page(*kind)?;
            notices.extend(parse_notice_list(&html, *kind)?);
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

/// Fetch notices through the authenticated session driver. A confirmed auth
/// failure restarts the whole list once after `renew`, matching Python's
/// invalidate-and-retry recursion.
pub(super) fn fetch_notices_with_session(
    driver: &mut dyn SessionDriver,
    types: Option<&[NoticeType]>,
    count: u64,
) -> CliResult<Value> {
    let mut renewed = false;
    loop {
        let result = collect_notices(types, count, |kind| {
            let mut transport = ScopedTransport::new(driver, Route::portal_list(kind));
            get_page(&mut transport, &list_url(kind), "notice list")
        });
        match result {
            Err(error) if !renewed && error.kind == "auth" => {
                driver.renew(Service::Portal)?;
                renewed = true;
            }
            result => return result,
        }
    }
}

/// Reject a blank, huge, or control-character keyword before any session use.
pub(super) fn validate_keyword(raw: &str) -> CliResult<String> {
    let keyword = raw.trim();
    if keyword.is_empty() || keyword.chars().count() > 80 || keyword.chars().any(char::is_control) {
        return Err(CliError::new(
            2,
            "usage",
            "notice search keyword must be 1-80 characters",
            "try `bupt notices search \u{5956}\u{5b66}\u{91d1}`",
        ));
    }
    Ok(keyword.to_string())
}

/// Search notice titles through the portal's own notice-tree form.
///
/// The form posts `INTEXT2` as standard base64 and scopes the query with the
/// `wbtreeids` hidden field from the notice list. Only the first result page
/// is read; `count` truncates that page and `total` is the portal's own count.
pub(super) fn fetch_notice_search_with_session(
    driver: &mut dyn SessionDriver,
    keyword: &str,
    count: u64,
) -> CliResult<Value> {
    if count == 0 {
        return Ok(json!({
            "keyword": keyword,
            "total": null,
            "count": 0,
            "notices": [],
        }));
    }
    let mut renewed = false;
    loop {
        let result = search_once(driver, keyword, count);
        match result {
            Err(error) if !renewed && error.kind == "auth" => {
                driver.renew(Service::Portal)?;
                renewed = true;
            }
            result => return result,
        }
    }
}

fn search_once(driver: &mut dyn SessionDriver, keyword: &str, count: u64) -> CliResult<Value> {
    let list = {
        let mut transport = ScopedTransport::new(driver, Route::portal_list(NoticeType::Notice));
        get_page(&mut transport, &list_url(NoticeType::Notice), "notice list")?
    };
    let tree_ids = notice_search_tree_ids(&list)
        .ok_or_else(|| parse_error("notice search", "is missing the notice-tree scope"))?;
    let url = search_url(keyword, &tree_ids)?;
    let page = {
        let mut transport = ScopedTransport::new(driver, Route::portal_search());
        get_page(&mut transport, &url, "notice search")?
    };
    parse_notice_search(&page, keyword, count)
}

pub(super) fn search_url(keyword: &str, tree_ids: &str) -> CliResult<String> {
    let mut url =
        Url::parse("http://my.bupt.edu.cn/fz_ssjg.jsp").expect("static notice search URL");
    url.query_pairs_mut()
        .append_pair("a218493p", "1")
        .append_pair("a218493c", "10")
        .append_pair("wbtreeid", "1154")
        .append_pair("wbnewsfield", "wbtitle")
        .append_pair("Find", "find")
        .append_pair("entrymode", "2")
        .append_pair("INTEXT", "")
        .append_pair("INTEXT2", &base64_encode(keyword.as_bytes()))
        .append_pair("news_search_code", "")
        .append_pair("wbtreeids", tree_ids);
    let url = url.to_string();
    if crate::transport::portal_scope::portal_url(&url, false).is_none() {
        return Err(parse_error(
            "notice search",
            "could not build an approved search request",
        ));
    }
    Ok(url)
}

fn notice_search_tree_ids(html: &str) -> Option<String> {
    let start = html
        .find("name='wbtreeids'")
        .or_else(|| html.find("name=\"wbtreeids\""))?;
    let rest = html.get(start..)?;
    let rest = rest.get(rest.find("value=")? + "value=".len()..)?;
    let value = if let Some(rest) = rest.strip_prefix('"') {
        rest.split_once('"')?.0
    } else if let Some(rest) = rest.strip_prefix('\'') {
        rest.split_once('\'')?.0
    } else {
        rest.split(|c: char| c.is_whitespace() || c == '>').next()?
    };
    let value = value
        .split(',')
        .filter(|id| !id.is_empty())
        .collect::<Vec<_>>()
        .join(",");
    (!value.is_empty()).then_some(value)
}

fn parse_notice_search(html: &str, keyword: &str, count: u64) -> CliResult<Value> {
    let total = search_total(html)
        .ok_or_else(|| parse_error("notice search", "is missing the result count"))?;
    let mut notices = Vec::new();
    if total > 0 {
        for anchor in elements(html, "a", |_| true) {
            if notices.len() >= usize::try_from(count).unwrap_or(usize::MAX) {
                break;
            }
            let Some(href) = attr_value(anchor.attrs, "href") else {
                continue;
            };
            let href = decode_entities(href);
            if !href.contains("wbnewsid=") {
                continue;
            }
            let title = joined_text(&html[anchor.content_start..anchor.content_end]);
            let title = title.trim();
            if title.is_empty() {
                continue;
            }
            let Some(url) = portal_join(PORTAL_ROOT, href.trim()) else {
                continue;
            };
            let window = html.get(anchor.end..).unwrap_or("");
            let time = first_date(window)
                .map(|(_, date)| date.replace('/', "-"))
                .unwrap_or_default();
            notices.push(json!({
                "title": title,
                "url": url,
                "author": "",
                "time": time,
                "type": NoticeType::Notice.key(),
                "type_name": NoticeType::Notice.type_name(),
            }));
        }
    }
    Ok(json!({
        "keyword": keyword,
        "total": total,
        "count": notices.len(),
        "notices": notices,
    }))
}

fn search_total(html: &str) -> Option<u64> {
    let marker = "共";
    let mut rest = html;
    while let Some(at) = rest.find(marker) {
        let after = rest[at + marker.len()..].trim_start();
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            let tail = after[digits.len()..].trim_start();
            if tail.starts_with('条') {
                return digits.parse().ok();
            }
        }
        rest = &rest[at + marker.len()..];
    }
    None
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let (chunks, rest) = input.as_chunks::<3>();
    for chunk in chunks {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(chunk[1]) << 8) | u32::from(chunk[2]);
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 6) & 0x3f) as usize] as char);
        out.push(TABLE[(n & 0x3f) as usize] as char);
    }
    if rest.len() == 1 {
        let n = u32::from(rest[0]) << 16;
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        out.push('=');
        out.push('=');
    } else if rest.len() == 2 {
        let n = (u32::from(rest[0]) << 16) | (u32::from(rest[1]) << 8);
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 6) & 0x3f) as usize] as char);
        out.push('=');
    }
    out
}

/// Fetch one user-supplied notice page and map it to the CLI document.
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

/// Fetch one notice through the authenticated session driver, renewing once
/// only after a confirmed auth failure.
pub(super) fn fetch_notice_detail_with_session(
    driver: &mut dyn SessionDriver,
    url: &str,
    max_chars: usize,
) -> CliResult<Value> {
    let mut renewed = false;
    loop {
        let result = {
            let mut transport = ScopedTransport::new(driver, Route::portal_detail());
            fetch_notice_detail(&mut transport, url, max_chars)
        };
        match result {
            Err(error) if !renewed && error.kind == "auth" => {
                driver.renew(Service::Portal)?;
                renewed = true;
            }
            result => return result,
        }
    }
}

/// Save every attachment of one notice page.
///
/// The portal serves an attachment only to a request that names a portal page
/// as its `Referer`; a bare request gets a captcha page instead. That is why
/// the command takes the notice page and not an attachment link: the page is
/// both the referer and the list of files, so every saved file is tied to the
/// page that rendered it.
pub(super) fn fetch_notice_download_with_session(
    driver: &mut dyn SessionDriver,
    notice_url: &str,
    dir: &Path,
    force: bool,
) -> CliResult<Value> {
    let notice_url = validate_notice_url(notice_url)?;
    let dir = files::prepare_dir(dir)?;
    // At most one renewal for the whole command, shared by the page and by every
    // attachment. Renewal happens per request, so a retry never re-saves a file
    // this run has already written and never downloads it twice.
    let mut renewed = false;
    let detail = retry_once_after_renewal(driver, &mut renewed, |driver| {
        let mut transport = ScopedTransport::new(driver, Route::portal_detail());
        let page = get_page(&mut transport, &notice_url, "notice detail")?;
        parse_notice_detail(&page, &notice_url)
    })?;
    let mut saved = Vec::new();
    let mut skipped = Vec::new();
    for (index, attachment) in detail.attachments.iter().enumerate() {
        let title = attachment
            .get("title")
            .and_then(Value::as_str)
            .filter(|title| !title.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("attachment-{}", index + 1));
        let Some(raw_url) = attachment.get("url").and_then(Value::as_str) else {
            skipped.push(skipped_json(&title, None, "no_url"));
            continue;
        };
        // A notice may also link a file the download scope does not approve;
        // report it instead of failing the whole page.
        let Ok(url) = validate_attachment_url(raw_url) else {
            skipped.push(skipped_json(&title, None, "unsupported_link"));
            continue;
        };
        let filename = files::safe_filename(&title, &format!("attachment-{}", index + 1));
        let dest = dir.join(&filename);
        // Ask the filesystem before spending the transfer, the same way
        // `homework download` does.
        if !force && dest.exists() {
            skipped.push(skipped_json(&title, Some(&dest), "exists"));
            continue;
        }
        let body = retry_once_after_renewal(driver, &mut renewed, |driver| {
            let mut transport = ScopedTransport::new(driver, Route::portal_download());
            download_attachment(&mut transport, &url, &notice_url)
        })?;
        match files::write_atomic(&dir, &dest, &body, force) {
            Ok(files::WriteOutcome::Written(size)) => saved.push(json!({
                "title": title,
                "path": dest.to_string_lossy(),
                "size": size,
                "url": url,
            })),
            Ok(files::WriteOutcome::Exists) => {
                skipped.push(skipped_json(&title, Some(&dest), "exists"));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(json!({
        "title": detail.title,
        "url": notice_url,
        "dir": dir.to_string_lossy(),
        "saved": saved,
        "skipped": skipped,
        "count_saved": saved.len(),
        "count_skipped": skipped.len(),
    }))
}

/// Run one request, renewing the portal session at most once per command.
/// The contract allows a renewal only when the service actually reported an
/// expired session, so every other failure is returned untouched.
fn retry_once_after_renewal<T>(
    driver: &mut dyn SessionDriver,
    renewed: &mut bool,
    mut attempt: impl FnMut(&mut dyn SessionDriver) -> CliResult<T>,
) -> CliResult<T> {
    match attempt(&mut *driver) {
        Err(error) if !*renewed && error.kind == "auth" => {
            driver.renew(Service::Portal)?;
            *renewed = true;
            attempt(driver)
        }
        result => result,
    }
}

fn skipped_json(title: &str, path: Option<&Path>, reason: &str) -> Value {
    json!({
        "title": title,
        "path": path.map(|path| path.to_string_lossy().into_owned()),
        "reason": reason,
    })
}

/// Fetch one attachment. The portal answers a request that does not name a
/// portal page with a captcha form and HTTP 200, so a page is never written to
/// disk as if it were the file.
///
/// The saved name comes from the link text on the notice page. The portal does
/// send a `Content-Disposition`, but the campus transport surfaces only the
/// headers the routes act on (`location`, `set-cookie`), and the link text is
/// the same file name the page shows the reader.
fn download_attachment(
    transport: &mut dyn Transport,
    url: &str,
    referer: &str,
) -> CliResult<Vec<u8>> {
    let response = send(
        transport,
        Request {
            method: "GET",
            url: url.into(),
            headers: vec![("Referer".into(), referer.into())],
            body: Vec::new(),
        },
        "notice attachment",
    )
    .map_err(|error| {
        if error.kind != "network" {
            return error;
        }
        // The transport cannot tell a dead connection from a file over its
        // 8 MiB cap, so both causes are named here.
        CliError::new(
            4,
            "network",
            error.message,
            "my.bupt.edu.cn must be reachable on the campus network, and one attachment is limited to 8 MiB; try `bupt diagnose`",
        )
    })?;
    check_status(&response, "notice attachment")?;
    if response.body.is_empty() {
        return Err(parse_error("notice attachment", "returned an empty file"));
    }
    if looks_like_html(&response.body) {
        return Err(attachment_page_error(&String::from_utf8_lossy(
            &response.body,
        )));
    }
    Ok(response.body)
}

fn looks_like_html(body: &[u8]) -> bool {
    // The captcha form is HTML. A captcha page written to disk would look like a
    // successful download, so this errs towards calling a response a page: after
    // an optional UTF-8 BOM and whitespace, a leading `<` is a page. No file
    // format the portal serves starts that way (xlsx/docx start with `PK`, pdf
    // with `%PDF`).
    let body = body.strip_prefix(b"\xef\xbb\xbf").unwrap_or(body);
    let head = &body[..body.len().min(512)];
    head.iter()
        .find(|byte| !byte.is_ascii_whitespace())
        .is_some_and(|byte| *byte == b'<')
}

fn attachment_page_error(page: &str) -> CliError {
    // A login page must win over the captcha text that a sign-in form can carry
    // as well: it is the case the caller can renew.
    let mut page_error = page_problem(ATTACHMENT_PATH, page, "notice attachment");
    if page_error
        .as_ref()
        .is_some_and(|error| error.kind == "auth")
    {
        return page_error.take().expect("checked as a login page");
    }
    if page.contains("验证码") {
        return CliError::new(
            5,
            "upstream",
            "the portal answered with a captcha page instead of the file",
            "No file was written. The portal serves an attachment only to a session that is still signed in; run `bupt login` if it expired, then rerun `bupt notices download` with the notice page URL.",
        );
    }
    if let Some(error) = page_error {
        return error;
    }
    CliError::new(
        5,
        "upstream",
        "the portal answered with a page instead of the file",
        "No file was written. The attachment link may have expired; re-read the notice with `bupt notices show`.",
    )
}

fn truncate_chars(content: String, max_chars: usize) -> (String, bool) {
    if max_chars == 0 || content.chars().count() <= max_chars {
        return (content, false);
    }
    (content.chars().take(max_chars).collect(), true)
}

/// Fetch the balance widget. The referer and AJAX header are required by the
/// upstream widget and are the only headers this module sets.
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

/// Fetch the card widget through the authenticated session driver, renewing
/// once only after a confirmed auth failure.
pub(super) fn fetch_card_with_session(driver: &mut dyn SessionDriver) -> CliResult<Value> {
    let mut renewed = false;
    loop {
        let result = {
            let mut transport = ScopedTransport::new(driver, Route::portal_card());
            fetch_card(&mut transport)
        };
        match result {
            Err(error) if !renewed && error.kind == "auth" => {
                driver.renew(Service::Portal)?;
                renewed = true;
            }
            result => return result,
        }
    }
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
    transport.send(request).map_err(|error| {
        if error.kind != "network" {
            return error;
        }
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
    // Python's `html_to_text`: every tag becomes a space, entities are
    // unescaped, and the result is trimmed.
    let content_text = strip_tags(&html[content.start..content.end]);
    let content_text = decode_entities(&content_text).trim().to_owned();
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
    fn search_with_zero_count_makes_no_request() {
        struct Never;
        impl SessionDriver for Never {
            fn ensure(&mut self, _: Service) -> CliResult<()> {
                panic!("a zero count must not touch the session")
            }
            fn renew(&mut self, _: Service) -> CliResult<()> {
                panic!("a zero count must not renew")
            }
            fn send(&mut self, _: Route, _: Request) -> CliResult<Response> {
                panic!("a zero count must not send")
            }
        }
        let data = fetch_notice_search_with_session(&mut Never, "奖学金", 0).unwrap();
        assert_eq!(data["count"], 0);
        assert_eq!(data["notices"], json!([]));
        assert_eq!(data["total"], Value::Null);
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
                "url": "http://my.bupt.edu.cn/system/_content/download.jsp?urltype=news.DownloadAttachUrl&owner=1664271694&wbfileid=3251628",
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
    fn an_html_answer_is_recognised_whatever_preamble_it_carries() {
        let bom = "\u{feff}";
        let captcha = r#"<!DOCTYPE html><html><head><title>请输入验证码</title></head></html>"#;
        for page in [
            captcha.to_string(),
            format!("{bom}{captcha}"),
            format!("\n\n  \t{captcha}"),
            format!("{bom}  {captcha}"),
            "<?xml version=\"1.0\"?><html><body>验证码</body></html>".into(),
            "<!-- saved from the portal --><html></html>".into(),
        ] {
            assert!(
                looks_like_html(page.as_bytes()),
                "{page:?} must be treated as a page, not a file"
            );
        }
        // Real attachment bytes are never pages.
        let xlsx = b"PK\x03\x04\x14\x00";
        let pdf = b"%PDF-1.7\n";
        let docx = b"PK\x03\x04";
        let zip = b"PK\x05\x06";
        for file in [xlsx.as_slice(), pdf, docx, zip, b"" as &[u8]] {
            assert!(!looks_like_html(file), "{file:?} is a file, not a page");
        }
    }

    #[test]
    fn an_attachment_page_error_prefers_the_actionable_cause() {
        // A sign-in form that also shows a captcha must stay an auth error: that
        // is the only case the caller can renew.
        let login = r#"<html><title>登录</title><form><input type="password"><input name="captcha">请输入验证码</form></html>"#;
        let error = attachment_page_error(login);
        assert_eq!((error.code, error.kind), (3, "auth"));

        // A captcha without a sign-in form explains itself.
        let error = attachment_page_error("<html><body>请输入验证码后下载附件</body></html>");
        assert_eq!((error.code, error.kind), (5, "upstream"));
        assert!(error.message.contains("captcha"), "{}", error.message);
        assert!(
            error.hint.contains("bupt login"),
            "the hint names the recovery step; hint={}",
            error.hint
        );

        // A generic page still reports that nothing was written.
        let error = attachment_page_error("<html><body>404</body></html>");
        assert_eq!((error.code, error.kind), (5, "upstream"));
        assert!(error.hint.contains("No file was written"));
    }

    #[test]
    fn notice_urls_must_be_a_page_the_transport_would_fetch() {
        for (url, expected) in [
            (
                "http://my.bupt.edu.cn/system/resource/code/auth/clogin.jsp?owner=1",
                "login callback",
            ),
            (
                "http://my.bupt.edu.cn/system/_content/download.jsp?urltype=news.DownloadAttachUrl&owner=1&wbfileid=2",
                "attachment link",
            ),
            ("http://my.bupt.edu.cn/foo.jsp", "notice page"),
        ] {
            let error = validate_notice_url(url).unwrap_err();
            assert_eq!((error.code, error.kind), (2, "usage"), "{url}");
            assert!(
                error.message.contains(expected),
                "{url}: expected {expected:?} in {:?}",
                error.message
            );
        }
        // A page the wire approves is accepted, including an upper-case host.
        assert_eq!(
            validate_notice_url("http://MY.BUPT.EDU.CN/2026/a/page.htm").unwrap(),
            "http://my.bupt.edu.cn/2026/a/page.htm"
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

    #[test]
    fn notice_search_encodes_the_keyword_and_reads_the_first_page() {
        assert_eq!(base64_encode("奖学金".as_bytes()), "5aWW5a2m6YeR");
        assert!(validate_keyword("  ").is_err());
        assert!(validate_keyword(&"奖".repeat(81)).is_err());

        let ids = notice_search_tree_ids("<input type='hidden' name='wbtreeids' value=1155,1158,>")
            .unwrap();
        assert_eq!(ids, "1155,1158");
        let url = search_url("奖学金", &ids).unwrap();
        assert!(url.contains("INTEXT2=5aWW5a2m6YeR"));
        assert!(url.contains("wbtreeids=1155%2C1158") || url.contains("wbtreeids=1155,1158"));

        let page = r#"
            <a href="xntz_content.jsp?urltype=news.NewsContentUrl&wbtreeid=2149&wbnewsid=143528">奖学金通知</a>
            <span>2026-11-20</span>
            <span class="p_t">共12条</span>
            <a href="xntz_content.jsp?urltype=news.NewsContentUrl&wbtreeid=1736&wbnewsid=142208">第二条</a>
            <span>2026-11-19</span>
        "#;
        let parsed = parse_notice_search(page, "奖学金", 1).unwrap();
        assert_eq!(parsed["total"], 12);
        assert_eq!(parsed["count"], 1);
        assert_eq!(parsed["notices"][0]["title"], "奖学金通知");
        assert_eq!(parsed["notices"][0]["type"], "notice");
        assert_eq!(parsed["notices"][0]["time"], "2026-11-20");
        assert!(
            parsed["notices"][0]["url"]
                .as_str()
                .unwrap()
                .contains("wbnewsid=143528")
        );
        assert!(parse_notice_search("<p>公共信息</p>", "奖学金", 1).is_err());
    }
}
