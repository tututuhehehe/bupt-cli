//! Scoped campus requests for a one-exchange system curl executor. All request
//! strings are written as curl options to stdin, never to argv or a file.

use super::graduate_scope;
use super::{CurlExec, Request, Response, base_curl_arguments};
use crate::cli::{CliError, CliResult};
use std::fmt::Write;
use std::time::Duration;
use url::Url;

const MAX_REQUEST: usize = 64 * 1024;
const MAX_HEADERS: usize = 64 * 1024;
const MAX_UPLOAD_BYTES: usize = 8 * 1024 * 1024 + 4096;
const MAX_SUBMIT_BYTES: usize = 512 * 1024;
use super::electricity_scope::{self, LANDING as ELECTRICITY_LANDING};
const ANONYMOUS_PORTAL_PROBE: &str =
    "http://my.bupt.edu.cn/list.jsp?totalpage=30&PAGENUM=0&urltype=tree.TreeTempUrl&wbtreeid=1154";
const UCLOUD_TOKEN_URL: &str = "https://apiucloud.bupt.edu.cn/ykt-basics/oauth/token";

#[derive(Clone, Copy)]
enum Route {
    CasRead,
    CasForm,
    UcloudLanding,
    UcloudToken,
    UcloudRead,
    UcloudHomeworkList,
    UcloudAttachment,
    UcloudUpload,
    UcloudSubmit,
    ElectricityLanding,
    ElectricitySignIn,
    ElectricityRead(&'static [&'static str]),
    GraduateRead,
    PortalRead { card: bool, download: bool },
    PublicProbe,
    MobileProbe,
    MobileLogin,
}

pub(super) fn exchange(
    exec: &mut dyn CurlExec,
    timeout: Duration,
    request: &Request,
) -> CliResult<Response> {
    exchange_with_policy(exec, timeout, request, false)
}

pub(super) fn exchange_probe(
    exec: &mut dyn CurlExec,
    timeout: Duration,
    request: &Request,
) -> CliResult<Response> {
    exchange_with_policy(exec, timeout, request, true)
}

fn exchange_with_policy(
    exec: &mut dyn CurlExec,
    timeout: Duration,
    request: &Request,
    probe: bool,
) -> CliResult<Response> {
    if timeout.is_zero() {
        return Err(rejected());
    }
    let input = if probe {
        curl_probe_config(request)?
    } else {
        curl_config(request)?
    };
    let mut args = base_curl_arguments(timeout);
    // A homework upload body is binary multipart: it is staged to a private
    // temporary file and passed as `--data-binary @path`, never through
    // argv content or the curl configuration text.
    let mut upload_body = None;
    if !probe && route(request).is_some_and(|tag| matches!(tag, Route::UcloudUpload)) {
        let staged = stage_upload_body(&request.body)?;
        args.extend([
            "--data-binary".into(),
            format!("@{}", staged.path().display()).into(),
        ]);
        upload_body = Some(staged);
    }
    // The information portal is HTTP-only. Every other campus route stays HTTPS.
    if super::portal_scope::portal_url(&request.url, true).is_some() {
        let proto = args
            .iter()
            .position(|arg| arg == "--proto")
            .expect("protocol is specified");
        args[proto + 1] = "=http".into();
    }
    args.extend(["--config".into(), "-".into()]);
    let output = exec.exec(&args, &input).map_err(|_| network_failure())?;
    drop(upload_body);
    if !output.success {
        return Err(network_failure());
    }
    parse_response(&output.stdout)
}

fn network_failure() -> CliError {
    CliError::new(
        4,
        "network",
        "campus request failed",
        "Check campus network connectivity. The request was not retried.",
    )
}

fn curl_config(request: &Request) -> CliResult<Vec<u8>> {
    config_for_route(request, route(request).ok_or_else(rejected)?, false)
}

fn curl_probe_config(request: &Request) -> CliResult<Vec<u8>> {
    config_for_route(request, probe_route(request).ok_or_else(rejected)?, true)
}

fn config_for_route(request: &Request, route: Route, probe: bool) -> CliResult<Vec<u8>> {
    if matches!(route, Route::UcloudUpload) {
        return upload_config(request);
    }
    let requires_body = matches!(
        route,
        Route::CasForm
            | Route::UcloudToken
            | Route::UcloudSubmit
            | Route::UcloudHomeworkList
            | Route::ElectricityRead(_)
            | Route::MobileLogin
    );
    // Bodies reaching this point are printable ASCII (the JSON submission
    // body is validated separately and never contains control characters).
    let body = match std::str::from_utf8(&request.body) {
        Ok(body) if !matches!(route, Route::UcloudSubmit) => body,
        _ => "",
    };
    if !matches!(route, Route::UcloudSubmit)
        && (body.len() > MAX_REQUEST
            || body.is_empty() == requires_body
            || !body.bytes().all(|b| matches!(b, 0x21..=0x7e)))
    {
        return Err(rejected());
    }
    if matches!(route, Route::UcloudSubmit)
        && (request.body.len() > MAX_SUBMIT_BYTES || !valid_submit_body(&request.body))
    {
        return Err(rejected());
    }
    let mut options = String::from("include\n");
    option(&mut options, "url", &request.url);
    option(&mut options, "request", request.method);
    let mut seen = Vec::new();
    for (name, value) in &request.headers {
        let key = name.to_ascii_lowercase();
        if !allowed_header(route, &key)
            || seen.contains(&key)
            || !value.bytes().all(|byte| matches!(byte, 0x20..=0x7e))
        {
            return Err(rejected());
        }
        seen.push(key);
        option(&mut options, "header", &format!("{name}: {value}"));
        if options.len() > MAX_HEADERS {
            return Err(rejected());
        }
    }
    if matches!(route, Route::UcloudHomeworkList)
        && (!valid_homework_list_body(body, if probe { 20 } else { 50 })
            || !has_header(
                request,
                "content-type",
                if probe {
                    "application/json;charset=UTF-8"
                } else {
                    "application/json"
                },
            ))
    {
        return Err(rejected());
    }
    if matches!(route, Route::UcloudSubmit)
        && (!has_header(request, "content-type", "application/json")
            || request.body.iter().any(|byte| byte.is_ascii_control()))
    {
        // The submit body is written into the curl configuration text, so raw
        // control bytes would end the directive line. Reject them here.
        return Err(rejected());
    }
    if probe && matches!(route, Route::UcloudToken) && !valid_ticket_exchange(body) {
        return Err(rejected());
    }
    if matches!(route, Route::MobileLogin)
        && (!has_header(request, "content-type", "application/x-www-form-urlencoded")
            || !valid_mobile_login_body(body))
    {
        return Err(rejected());
    }
    if let Route::ElectricityRead(keys) = route {
        if !valid_electricity_body(body, keys)
            || !has_header(request, "referer", ELECTRICITY_LANDING)
            || !has_header(request, "x-requested-with", "XMLHttpRequest")
            || !has_header(request, "content-type", "application/x-www-form-urlencoded")
        {
            return Err(rejected());
        }
    }
    if matches!(route, Route::PortalRead { card: true, .. })
        && request.headers.iter().any(|(key, value)| {
            (key.eq_ignore_ascii_case("referer") && value != super::portal_scope::CARD_REFERER)
                || (key.eq_ignore_ascii_case("x-requested-with") && value != "XMLHttpRequest")
        })
    {
        return Err(rejected());
    }
    // An attachment is served only to a request that names a portal page as its
    // referer. The notice commands pass the page they read the link from; the
    // exact referer is not pinned because the portal accepts any portal page.
    // This is the transport's own backstop; the route pins the stricter rule
    // (a page, never another attachment).
    if matches!(route, Route::PortalRead { download: true, .. })
        && (!request
            .headers
            .iter()
            .any(|(key, _)| key.eq_ignore_ascii_case("referer"))
            || request.headers.iter().any(|(key, value)| {
                key.eq_ignore_ascii_case("referer")
                    && !super::portal_scope::portal_url(value, false)
                        .is_some_and(|url| url.path() != super::portal_scope::DOWNLOAD)
            }))
    {
        return Err(rejected());
    }
    if requires_body {
        option(&mut options, "header", "Expect:");
        if matches!(route, Route::UcloudSubmit) {
            let text = std::str::from_utf8(&request.body).map_err(|_| rejected())?;
            option(&mut options, "data-raw", text);
        } else {
            option(&mut options, "data-raw", body);
        }
    }
    Ok(options.into_bytes())
}

/// Build the curl configuration for one homework upload. The multipart body
/// is binary, so it is staged to a private temporary file and passed with
/// `--data-binary @path` instead of ever entering argv or the config text.
fn upload_config(request: &Request) -> CliResult<Vec<u8>> {
    let boundary = super::HOMEWORK_MULTIPART_BOUNDARY;
    let opening = format!("--{boundary}\r\n");
    let closing = format!("\r\n--{boundary}--\r\n");
    let body = &request.body;
    let opening_bytes = opening.as_bytes();
    let openings = (0..=body.len().saturating_sub(opening_bytes.len()))
        .filter(|&at| &body[at..at + opening_bytes.len()] == opening_bytes)
        .count();
    if body.len() > MAX_UPLOAD_BYTES
        || openings != 3
        || !body.starts_with(opening.as_bytes())
        || !body.ends_with(closing.as_bytes())
        || !body
            .windows(b"name=\"file\"".len())
            .any(|window| window == b"name=\"file\"")
    {
        return Err(rejected());
    }
    let mut options = String::from("include\n");
    option(&mut options, "url", &request.url);
    option(&mut options, "request", request.method);
    let mut seen = Vec::new();
    for (name, value) in &request.headers {
        let key = name.to_ascii_lowercase();
        if !allowed_header(Route::UcloudUpload, &key)
            || seen.contains(&key)
            || !value.bytes().all(|byte| matches!(byte, 0x20..=0x7e))
        {
            return Err(rejected());
        }
        seen.push(key);
        option(&mut options, "header", &format!("{name}: {value}"));
    }
    if !has_header(
        request,
        "content-type",
        &format!("multipart/form-data; boundary={boundary}"),
    ) || seen.len() != 4
    {
        return Err(rejected());
    }
    Ok(options.into_bytes())
}

/// Stage the upload body in a private temporary file; it is removed as soon
/// as curl has consumed it. Never returns the path through error text.
fn stage_upload_body(body: &[u8]) -> CliResult<tempfile::NamedTempFile> {
    use std::io::Write;
    let mut file = tempfile::Builder::new()
        .prefix("bupt-upload-")
        .rand_bytes(6)
        .tempfile_in(std::env::temp_dir())
        .map_err(|_| rejected())?;
    file.write_all(body).map_err(|_| rejected())?;
    file.flush().map_err(|_| rejected())?;
    Ok(file)
}

/// The fixed submission payload shape; mirrors `homework::submit`.
fn valid_submit_body(body: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    let Some(object) = value.as_object() else {
        return false;
    };
    let string = |key: &str| object.get(key).and_then(serde_json::Value::as_str);
    object.len() == 7
        && object
            .get("assignmentType")
            .and_then(serde_json::Value::as_i64)
            == Some(0)
        && string("assignmentId").is_some_and(|text| !text.is_empty() && text.len() <= 64)
        && string("assignmentContent").is_some_and(|text| text.chars().count() <= 100_000)
        && string("userId").is_some_and(|text| !text.is_empty() && text.len() <= 64)
        && string("groupId") == Some("")
        && string("commitId") == Some("")
        && object.get("attachmentIds").is_some_and(|ids| {
            ids.as_array().is_some_and(|items| {
                items.len() <= 128
                    && items.iter().all(|id| {
                        id.as_str()
                            .is_some_and(|text| !text.is_empty() && text.len() <= 256)
                    })
            })
        })
}

fn route(request: &Request) -> Option<Route> {
    let raw = &request.url;
    if raw.len() > 8192 || !raw.bytes().all(|b| matches!(b, 0x21..=0x7e)) || raw.contains('\\') {
        return None;
    }
    // The portal is HTTP-only, and `exchange_with_policy` derives its protocol
    // policy from this same predicate. Accepting the `https` spelling here would
    // hand curl an https URL while the policy says `--proto =http`, which curl
    // refuses and which would then be reported as a network failure. The driver
    // already canonicalises portal URLs to http before they reach this layer.
    if request.method == "GET"
        && !raw.starts_with("https://")
        && super::portal_scope::portal_url(raw, true).is_some()
    {
        let path = Url::parse(raw).ok()?.path().to_owned();
        return Some(Route::PortalRead {
            card: path == super::portal_scope::CARD,
            download: path == super::portal_scope::DOWNLOAD,
        });
    }
    let authority = raw
        .strip_prefix("https://")?
        .split(['/', '?', '#'])
        .next()?;
    let parsed = Url::parse(raw).ok()?;
    let host = parsed.host_str()?;
    // Require canonical, already-validated input, including the exact authority.
    if authority != host || parsed.as_str() != raw || parsed.fragment().is_some() {
        return None;
    }
    let path = parsed.path();
    let lower = path.to_ascii_lowercase();
    if lower.contains("%2f") || lower.contains("%5c") || path.contains("//") {
        return None;
    }
    match (host, request.method, path) {
        ("auth.bupt.edu.cn", "POST", "/authserver/login") if parsed.query().is_none() => {
            Some(Route::CasForm)
        }
        ("auth.bupt.edu.cn", "GET", p) if p == "/authserver" || p.starts_with("/authserver/") => {
            Some(Route::CasRead)
        }
        ("yjxt.bupt.edu.cn", "GET", "/ULogin.aspx")
            if graduate_scope::ticketed_callback(raw).is_some() =>
        {
            Some(Route::GraduateRead)
        }
        ("yjxt.bupt.edu.cn", "GET", _p) if graduate_scope::approved_page(raw).is_some() => {
            Some(Route::GraduateRead)
        }
        ("ucloud.bupt.edu.cn", "GET", p) if p == "/uclass" || p.starts_with("/uclass/") => {
            Some(Route::UcloudLanding)
        }
        ("apiucloud.bupt.edu.cn", "POST", "/ykt-basics/oauth/token")
            if parsed.query().is_none() =>
        {
            Some(Route::UcloudToken)
        }
        (
            "apiucloud.bupt.edu.cn",
            "GET",
            "/ykt-site/site/list/student/current"
            | "/ykt-site/site/student/undone"
            | "/ykt-site/work/detail"
            | "/ykt-site/work/submit-view"
            | "/blade-source/resource/preview-url",
        ) => Some(Route::UcloudRead),
        ("apiucloud.bupt.edu.cn", "POST", "/blade-source/resource/upload/biz")
            if parsed.query().is_none() =>
        {
            Some(Route::UcloudUpload)
        }
        ("apiucloud.bupt.edu.cn", "POST", "/ykt-site/work/submit") if parsed.query().is_none() => {
            Some(Route::UcloudSubmit)
        }
        ("apiucloud.bupt.edu.cn", "POST", "/ykt-site/work/student/list")
            if parsed.query().is_none() =>
        {
            Some(Route::UcloudHomeworkList)
        }
        ("fileucloud.bupt.edu.cn", "GET", _)
        | ("apiucloud.bupt.edu.cn", "GET", "/blade-source/resource/download") => {
            Some(Route::UcloudAttachment)
        }
        ("jwglweixin.bupt.edu.cn", "POST", "/bjyddx/login") if parsed.query().is_none() => {
            Some(Route::MobileLogin)
        }
        (
            "jwglweixin.bupt.edu.cn",
            "GET",
            "/bjyddx/student/curriculum"
            | "/bjyddx/kaowu/get_kszc"
            | "/bjyddx/kaowu/getKsapcx"
            | "/bjyddx/kaowu/findKcksapcx",
        ) if parsed.query().is_none() => Some(Route::MobileProbe),
        ("jwglweixin.bupt.edu.cn", "POST", "/bjyddx/student/termGPA")
            if parsed.query().is_none() =>
        {
            Some(Route::MobileProbe)
        }
        ("jwglweixin.bupt.edu.cn", "GET", "/bjyddx/todayClassrooms")
            if matches!(parsed.query(), Some("campusId=01" | "campusId=04")) =>
        {
            Some(Route::MobileProbe)
        }
        ("app.bupt.edu.cn", "GET", "/buptdf/wap/default/chong") if parsed.query().is_none() => {
            Some(Route::ElectricityLanding)
        }
        ("app.bupt.edu.cn", "GET", _) if electricity_scope::sign_in(raw).is_some() => {
            Some(Route::ElectricitySignIn)
        }
        ("app.bupt.edu.cn", "POST", path) if parsed.query().is_none() => {
            let keys: &'static [&'static str] = match path {
                "/buptdf/wap/default/part" => &["areaid"],
                "/buptdf/wap/default/floor" => &["areaid", "partmentId"],
                "/buptdf/wap/default/drom" => &["areaid", "partmentId", "floorId"],
                "/buptdf/wap/default/search" => &["areaid", "partmentId", "floorId", "dromNumber"],
                _ => return None,
            };
            Some(Route::ElectricityRead(keys))
        }
        _ => None,
    }
}

fn probe_route(request: &Request) -> Option<Route> {
    // Diagnostic mode never posts a CAS form, even if a caller constructs one.
    const PUBLIC: &[&str] = &[
        ANONYMOUS_PORTAL_PROBE,
        UCLOUD_TOKEN_URL,
        "https://auth.bupt.edu.cn/authserver/login",
        "https://uc.bupt.edu.cn/api/uc/status",
        "https://win.bupt.edu.cn/note.do",
        "https://jwgl.bupt.edu.cn/jsxsd/framework/xsMain_bjyddx.jsp",
        "https://jwgl.bupt.edu.cn/jsxsd/framework/xsdPerson.jsp",
        "https://jwxt.bupt.edu.cn/caslogin.jsp",
        "https://jwglweixin.bupt.edu.cn/bjyddx/login",
    ];
    if request.method == "GET" && PUBLIC.contains(&request.url.as_str()) {
        return Some(Route::PublicProbe);
    }
    if let Some(route) = route(request) {
        return match route {
            Route::CasForm
            | Route::UcloudLanding
            | Route::UcloudAttachment
            | Route::MobileLogin
            | Route::ElectricitySignIn
            | Route::PortalRead { .. } => None,
            Route::CasRead
                if !matches!(
                    request.url.as_str(),
                    "https://auth.bupt.edu.cn/authserver/login"
                        | "https://auth.bupt.edu.cn/authserver/login?service=https%3A%2F%2Fucloud.bupt.edu.cn%2Fuclass"
                ) =>
            {
                None
            }
            Route::UcloudUpload | Route::UcloudSubmit | Route::GraduateRead => None,
            _ => Some(route),
        };
    }
    let raw = &request.url;
    if raw.len() > 8192 || !raw.bytes().all(|b| matches!(b, 0x21..=0x7e)) || raw.contains('\\') {
        return None;
    }
    let url = Url::parse(raw).ok()?;
    let host = url.host_str()?;
    if raw.strip_prefix("https://")?.split(['/', '?', '#']).next() != Some(host)
        || url.as_str() != raw
        || url.fragment().is_some()
    {
        return None;
    }
    match (host, request.method, url.path()) {
        (
            "apiucloud.bupt.edu.cn",
            "GET",
            "/ykt-basics/info"
            | "/ykt-site/base-term/current"
            | "/ykt-basics/userroledomaindept/listByUserId"
            | "/ykt-site/examination/list-stu"
            | "/ykt-activity/survey/page/todo",
        ) => Some(Route::UcloudRead),
        (
            "jwglweixin.bupt.edu.cn",
            "GET",
            "/bjyddx/student/curriculum"
            | "/bjyddx/Get_sjkbms"
            | "/bjyddx/teachingWeek"
            | "/bjyddx/kaowu/get_kszc"
            | "/bjyddx/kaowu/getKsapcx"
            | "/bjyddx/kaowu/findKcksapcx"
            | "/bjyddx/student/examinationArrangement"
            | "/bjyddx/noticeList"
            | "/bjyddx/currentTerm",
        ) if url.query().is_none() => Some(Route::MobileProbe),
        ("jwglweixin.bupt.edu.cn", "GET", "/bjyddx/todayClassrooms")
            if url.query() == Some("campusId=04") =>
        {
            Some(Route::MobileProbe)
        }
        ("jwglweixin.bupt.edu.cn", "POST", "/bjyddx/student/termGPA") if url.query().is_none() => {
            Some(Route::MobileProbe)
        }
        _ => None,
    }
}

fn valid_mobile_login_body(body: &str) -> bool {
    let fields: Vec<_> = url::form_urlencoded::parse(body.as_bytes()).collect();
    fields.len() == 2
        && fields
            .iter()
            .any(|(key, value)| key == "userNo" && !value.trim().is_empty())
        && fields
            .iter()
            .any(|(key, value)| key == "pwd" && !value.is_empty())
}

fn valid_ticket_exchange(body: &str) -> bool {
    let fields: Vec<_> = url::form_urlencoded::parse(body.as_bytes()).collect();
    fields.len() == 2
        && fields
            .iter()
            .any(|(key, value)| key == "grant_type" && value == "third")
        && fields.iter().any(|(key, value)| {
            key == "ticket"
                && !value.is_empty()
                && !value.chars().any(|c| c.is_control() || c.is_whitespace())
        })
}

fn allowed_header(route: Route, name: &str) -> bool {
    name == "user-agent"
        || match route {
            Route::CasRead | Route::ElectricityLanding | Route::ElectricitySignIn => {
                name == "cookie"
            }
            Route::GraduateRead => name == "cookie",
            Route::ElectricityRead(_) => matches!(
                name,
                "cookie" | "referer" | "x-requested-with" | "content-type"
            ),
            Route::CasForm => matches!(name, "cookie" | "content-type"),
            Route::UcloudLanding | Route::UcloudAttachment | Route::PublicProbe => false,
            Route::PortalRead { card, download } => {
                name == "cookie"
                    || (card && matches!(name, "referer" | "x-requested-with"))
                    || (download && name == "referer")
            }
            Route::MobileProbe => matches!(name, "token" | "accept"),
            Route::MobileLogin => matches!(name, "accept" | "content-type"),
            Route::UcloudToken => matches!(name, "authorization" | "content-type"),
            Route::UcloudRead | Route::UcloudHomeworkList => {
                matches!(
                    name,
                    "authorization" | "tenant-id" | "blade-auth" | "accept"
                ) || (matches!(route, Route::UcloudHomeworkList) && name == "content-type")
            }
            Route::UcloudUpload | Route::UcloudSubmit => matches!(
                name,
                "blade-auth" | "tenant-id" | "authorization" | "content-type"
            ),
        }
}

fn has_header(request: &Request, name: &str, value: &str) -> bool {
    request
        .headers
        .iter()
        .any(|(key, actual)| key.eq_ignore_ascii_case(name) && actual == value)
}

fn valid_electricity_body(body: &str, expected_keys: &[&str]) -> bool {
    let mut keys = Vec::with_capacity(expected_keys.len());
    for (key, value) in url::form_urlencoded::parse(body.as_bytes()) {
        if !expected_keys.contains(&key.as_ref())
            || keys.contains(&key)
            || value.trim().is_empty()
            || value.chars().any(char::is_control)
            || (key == "areaid" && !matches!(value.as_ref(), "1" | "2"))
        {
            return false;
        }
        keys.push(key);
    }
    keys.len() == expected_keys.len()
}

fn valid_homework_list_body(body: &str, page_size: u64) -> bool {
    let Ok(serde_json::Value::Object(object)) = serde_json::from_str(body) else {
        return false;
    };
    object.len() == 4
        && ["siteId", "userId"].into_iter().all(|key| {
            object
                .get(key)
                .and_then(serde_json::Value::as_str)
                .is_some_and(|s| !s.trim().is_empty())
        })
        && object
            .get("current")
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|page| page > 0)
        && object.get("size").and_then(serde_json::Value::as_u64) == Some(page_size)
}

pub(super) fn option(config: &mut String, name: &'static str, value: &str) {
    let _ = write!(config, "{name} = \"");
    for ch in value.chars() {
        if matches!(ch, '"' | '\\') {
            config.push('\\');
        }
        config.push(ch);
    }
    config.push_str("\"\n");
}

pub(super) fn parse_response(mut stdout: &[u8]) -> CliResult<Response> {
    for _ in 0..=3 {
        let end = stdout[..stdout.len().min(MAX_HEADERS)]
            .windows(4)
            .position(|chunk| chunk == b"\r\n\r\n")
            .ok_or_else(malformed_response)?;
        let mut lines = stdout[..end].split(|&b| b == b'\n');
        let status = lines
            .next()
            .and_then(http_status)
            .ok_or_else(malformed_response)?;
        let mut headers = Vec::new();
        for line in lines {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let colon = line
                .iter()
                .position(|&byte| byte == b':')
                .ok_or_else(malformed_response)?;
            let (name, value) = line.split_at(colon);
            if name.eq_ignore_ascii_case(b"location") || name.eq_ignore_ascii_case(b"set-cookie") {
                let value = std::str::from_utf8(&value[1..])
                    .map_err(|_| malformed_response())?
                    .trim();
                if value.bytes().any(|byte| byte.is_ascii_control())
                    || (name.eq_ignore_ascii_case(b"location")
                        && headers.iter().any(|(key, _)| key == "location"))
                {
                    return Err(malformed_response());
                }
                headers.push((
                    String::from_utf8_lossy(name).to_ascii_lowercase(),
                    value.to_owned(),
                ));
            }
        }
        stdout = &stdout[end + 4..];
        if (200..=599).contains(&status) {
            return Ok(Response {
                status,
                headers,
                body: stdout.to_vec(),
            });
        }
        if status == 101 {
            break;
        }
    }
    Err(malformed_response())
}

fn http_status(line: &[u8]) -> Option<u16> {
    let mut parts = line
        .strip_suffix(b"\r")
        .unwrap_or(line)
        .split(|&b| b == b' ');
    let version = parts.next()?;
    let code = parts.next()?;
    if !matches!(version, b"HTTP/1.0" | b"HTTP/1.1" | b"HTTP/2" | b"HTTP/3")
        || code.len() != 3
        || !code.iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    let code: u16 = std::str::from_utf8(code).ok()?.parse().ok()?;
    (100..=599).contains(&code).then_some(code)
}

fn rejected() -> CliError {
    CliError::new(
        5,
        "upstream",
        "request is outside the scoped campus transport policy",
        "",
    )
}

fn malformed_response() -> CliError {
    CliError::new(
        5,
        "upstream",
        "campus service returned malformed HTTP headers",
        "",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn curl_receives_no_request_secrets_in_arguments() {
        struct FakeCurl;
        impl CurlExec for FakeCurl {
            fn exec(
                &mut self,
                args: &[std::ffi::OsString],
                stdin: &[u8],
            ) -> CliResult<super::super::CurlOutput> {
                assert_eq!(args.first().unwrap(), "--disable");
                assert!(args.ends_with(&["--config".into(), "-".into()]));
                assert!(
                    args.iter()
                        .all(|arg| !arg.to_string_lossy().contains("TEST-ONLY"))
                );
                assert!(
                    std::str::from_utf8(stdin)
                        .unwrap()
                        .contains("password=TEST-ONLY")
                );
                Ok(super::super::CurlOutput {
                    stdout: b"HTTP/2 429\r\n\r\n".to_vec(),
                    success: true,
                })
            }
        }
        let response = exchange(
            &mut FakeCurl,
            Duration::from_secs(1),
            &Request {
                method: "POST",
                url: "https://auth.bupt.edu.cn/authserver/login".into(),
                headers: vec![],
                body: b"password=TEST-ONLY".to_vec(),
            },
        )
        .unwrap();
        assert_eq!(response.status, 429);
    }

    #[test]
    fn portal_requests_downgrade_the_protocol_policy_and_others_do_not() {
        // The live failure this guards: the portal answers plain HTTP only, so
        // the argv policy must say `=http` for a portal URL and stay `=https`
        // everywhere else. Reading only the config text would miss it, because
        // the scheme sits in argv and the URL in stdin.
        struct Record(Mutex<Vec<(Vec<std::ffi::OsString>, Vec<u8>)>>);
        impl CurlExec for Record {
            fn exec(
                &mut self,
                args: &[std::ffi::OsString],
                stdin: &[u8],
            ) -> CliResult<super::super::CurlOutput> {
                self.0.lock().unwrap().push((args.to_vec(), stdin.to_vec()));
                Ok(super::super::CurlOutput {
                    stdout: b"HTTP/1.1 200 OK\r\n\r\n".to_vec(),
                    success: true,
                })
            }
        }
        fn proto(args: &[std::ffi::OsString]) -> String {
            let at = args
                .iter()
                .position(|arg| arg == "--proto")
                .expect("--proto is always passed");
            args[at + 1].to_string_lossy().into_owned()
        }

        let exec = Record(Mutex::new(Vec::new()));
        let mut exec_ref = exec;
        let capture = |exec_ref: &mut Record| {
            let recorded = exec_ref.0.lock().unwrap().clone();
            recorded.last().cloned().expect("one recorded call")
        };

        let portal = Request {
            method: "GET",
            url: "http://my.bupt.edu.cn/2026/a/page.htm".into(),
            headers: vec![],
            body: vec![],
        };
        exchange(&mut exec_ref, Duration::from_secs(5), &portal).unwrap();
        let (args, stdin) = capture(&mut exec_ref);
        assert_eq!(proto(&args), "=http", "portal reads use plain HTTP");
        assert!(
            String::from_utf8_lossy(&stdin).contains("url = \"http://my.bupt.edu.cn"),
            "the config carries the same URL the policy describes"
        );

        let cas = Request {
            method: "GET",
            url: "https://auth.bupt.edu.cn/authserver/login".into(),
            headers: vec![],
            body: vec![],
        };
        exchange(&mut exec_ref, Duration::from_secs(5), &cas).unwrap();
        let (args, _) = capture(&mut exec_ref);
        assert_eq!(proto(&args), "=https", "CAS stays on HTTPS");

        // An `https` spelling of the portal URL is refused before curl: the
        // policy would contradict the URL and curl would fail as if the network
        // were down.
        let https_portal = Request {
            method: "GET",
            url: "https://my.bupt.edu.cn/2026/a/page.htm".into(),
            headers: vec![],
            body: vec![],
        };
        let error = match exchange(&mut exec_ref, Duration::from_secs(5), &https_portal) {
            Err(error) => error,
            Ok(_) => panic!("an https portal URL must be refused before curl"),
        };
        assert_eq!((error.code, error.kind), (5, "upstream"));
        assert_eq!(
            exec_ref.0.lock().unwrap().len(),
            2,
            "no third curl call was made"
        );
    }

    #[test]
    fn portal_download_reaches_curl_only_with_a_portal_referer() {
        const PAGE: &str = "http://my.bupt.edu.cn/xntz_content.jsp?urltype=news.NewsContentUrl&wbtreeid=2149&wbnewsid=144119";
        let mut request = Request {
            method: "GET",
            url: "http://my.bupt.edu.cn/system/_content/download.jsp?urltype=news.DownloadAttachUrl&owner=1664271694&wbfileid=3251628".into(),
            headers: vec![
                ("Cookie".into(), "JSESSIONID=TEST-ONLY".into()),
                ("Referer".into(), PAGE.into()),
            ],
            body: Vec::new(),
        };
        let config = std::str::from_utf8(&curl_config(&request).expect("download config"))
            .unwrap()
            .to_owned();
        assert!(config.contains("download.jsp"), "{config}");
        assert!(
            config.contains(&format!("header = \"Referer: {PAGE}\"")),
            "{config}"
        );

        // Same destination, no referer: this is the request the portal
        // answers with a captcha page, so it must not reach the network.
        request.headers.retain(|(name, _)| name != "Referer");
        assert!(curl_config(&request).is_err());
        // A referer that is not an approved portal page is refused too.
        request.headers.push((
            "Referer".into(),
            "http://my.bupt.edu.cn/system/_content/download.jsp".into(),
        ));
        assert!(curl_config(&request).is_err());
    }

    #[test]
    fn attachment_sources_receive_neither_account_headers_nor_request_bodies() {
        let mut request = Request {
            method: "GET",
            url: "https://fileucloud.bupt.edu.cn/attachment.bin?signature=TEST".into(),
            headers: vec![("User-Agent".into(), "bupt-test".into())],
            body: Vec::new(),
        };
        assert!(curl_config(&request).is_ok());
        assert!(curl_probe_config(&request).is_err());
        request.url =
            "https://apiucloud.bupt.edu.cn/blade-source/resource/download?resourceId=TEST".into();
        assert!(curl_config(&request).is_ok());
        for secret_header in ["Cookie", "Authorization", "Blade-Auth", "Tenant-Id"] {
            request
                .headers
                .push((secret_header.into(), "TEST-ONLY".into()));
            assert!(curl_config(&request).is_err());
            request.headers.pop();
        }
        request.body.push(b'x');
        assert!(curl_config(&request).is_err());
        request.body.clear();
        request.method = "POST";
        assert!(curl_config(&request).is_err());
        request.method = "GET";
        request.url = "https://fileucloud.bupt.edu.cn.example/attachment.bin".into();
        assert!(curl_config(&request).is_err());
    }

    #[test]
    fn mobile_academic_data_uses_fixed_read_endpoints() {
        let base = "https://jwglweixin.bupt.edu.cn";
        let mut request = Request {
            method: "GET",
            url: String::new(),
            headers: vec![("token".into(), "TEST-ONLY".into())],
            body: vec![],
        };
        for path in [
            "/bjyddx/student/curriculum",
            "/bjyddx/kaowu/get_kszc",
            "/bjyddx/kaowu/getKsapcx",
            "/bjyddx/kaowu/findKcksapcx",
            "/bjyddx/todayClassrooms?campusId=01",
            "/bjyddx/todayClassrooms?campusId=04",
        ] {
            request.url = format!("{base}{path}");
            assert!(curl_config(&request).is_ok(), "{path}");
        }
        request.url = format!("{base}/bjyddx/todayClassrooms?campusId=01&pwd=TEST");
        assert!(curl_config(&request).is_err());
        request.url = format!("{base}/bjyddx/student/termGPA");
        request.method = "POST";
        assert!(curl_config(&request).is_ok());
        request.body = b"pwd=TEST".to_vec();
        assert!(curl_config(&request).is_err());
    }

    #[test]
    fn mobile_login_is_only_available_on_one_tls_form_route() {
        let endpoint = "https://jwglweixin.bupt.edu.cn/bjyddx/login";
        let mut request = Request {
            method: "POST",
            url: endpoint.into(),
            headers: vec![(
                "Content-Type".into(),
                "application/x-www-form-urlencoded".into(),
            )],
            body: b"userNo=TEST-ONLY&pwd=TEST%2BONLY".to_vec(),
        };
        assert!(curl_config(&request).is_ok());
        assert!(curl_probe_config(&request).is_err());
        request.headers.push(("Cookie".into(), "TEST=ONLY".into()));
        assert!(curl_config(&request).is_err());
        request.headers.pop();
        request.url = endpoint.replace("https:", "http:");
        assert!(curl_config(&request).is_err());
        request.url = endpoint.into();
        for invalid in [
            "userNo=TEST&pwd=",
            "userNo=TEST&userNo=ONLY",
            "userNo=TEST&pwd=ONLY&extra=1",
        ] {
            request.body = invalid.as_bytes().to_vec();
            assert!(curl_config(&request).is_err());
        }
    }

    #[test]
    fn diagnostic_transport_does_not_send_passwords_or_plaintext_cookies() {
        let mut request = Request {
            method: "GET",
            url: ANONYMOUS_PORTAL_PROBE.into(),
            headers: vec![("User-Agent".into(), "bupt-test".into())],
            body: Vec::new(),
        };
        assert!(curl_probe_config(&request).is_ok());
        assert!(
            curl_config(&request).is_ok(),
            "the portal list is an approved HTTP read"
        );
        request.headers.push(("Cookie".into(), "TEST=ONLY".into()));
        assert!(curl_probe_config(&request).is_err());
        assert!(
            curl_config(&request).is_ok(),
            "a portal session cookie may ride on the portal list"
        );
        request.url = "https://auth.bupt.edu.cn/authserver/login".into();
        assert!(
            curl_probe_config(&request).is_err(),
            "public CAS GET is anonymous"
        );
        request.method = "POST";
        request.body = b"password=TEST-ONLY".to_vec();
        assert!(
            curl_probe_config(&request).is_err(),
            "diagnose cannot post a CAS form"
        );
        request.url = UCLOUD_TOKEN_URL.into();
        request.headers.clear();
        assert!(
            curl_probe_config(&request).is_err(),
            "token exchange cannot post a password"
        );
        request.body = b"grant_type=third&ticket=ST-TEST-ONLY".to_vec();
        assert!(curl_probe_config(&request).is_ok());
        request.body.clear();
        request.method = "GET";
        request.url = "https://jwglweixin.bupt.edu.cn/bjyddx/student/curriculum".into();
        request.headers.push(("token".into(), "TEST-ONLY".into()));
        assert!(curl_probe_config(&request).is_ok());
        request.url = "https://jwglweixin.bupt.edu.cn/bjyddx/login".into();
        assert!(
            curl_probe_config(&request).is_err(),
            "public mobile login GET is anonymous"
        );
    }

    #[test]
    fn homework_upload_is_staged_to_a_private_file_and_only_our_shape_is_accepted() {
        use std::sync::Mutex;
        const UPLOAD: &str = "https://apiucloud.bupt.edu.cn/blade-source/resource/upload/biz";
        let boundary = super::super::HOMEWORK_MULTIPART_BOUNDARY;
        let body: Vec<u8> = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"userId\"\r\n\r\nu1\r\n\
             --{boundary}\r\nContent-Disposition: form-data; name=\"bizType\"\r\n\r\n3\r\n\
             --{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.bin\"\r\n\
             Content-Type: application/octet-stream\r\n\r\nPAYLOAD\r\n--{boundary}--\r\n"
        )
        .into_bytes();
        fn upload_request(url: &str, headers: Vec<(String, String)>, body: &[u8]) -> Request {
            Request {
                method: "POST",
                url: url.into(),
                headers,
                body: body.to_vec(),
            }
        }
        let headers = || {
            vec![
                ("Blade-Auth".into(), "TEST-ONLY".into()),
                ("Tenant-Id".into(), "000000".into()),
                (
                    "Authorization".into(),
                    "Basic c3dvcmQ6c3dvcmRfc2VjcmV0".into(),
                ),
                (
                    "Content-Type".into(),
                    format!("multipart/form-data; boundary={boundary}"),
                ),
            ]
        };
        let request = upload_request(UPLOAD, headers(), &body);
        assert!(curl_config(&request).is_ok());
        assert!(curl_probe_config(&request).is_err());

        struct Journal(Mutex<Vec<(Vec<std::ffi::OsString>, Vec<u8>)>>);
        impl CurlExec for Journal {
            fn exec(
                &mut self,
                args: &[std::ffi::OsString],
                stdin: &[u8],
            ) -> CliResult<super::super::CurlOutput> {
                let position = args
                    .iter()
                    .position(|arg| arg == "--data-binary")
                    .expect("upload uses --data-binary");
                let raw = args[position + 1].to_string_lossy().into_owned();
                let path = std::path::PathBuf::from(raw.strip_prefix('@').expect("path arg"));
                let staged = std::fs::read(&path).expect("the staged body exists during exec");
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
                    assert_eq!(mode & 0o777, 0o600, "the staged body is user-private");
                }
                self.0.lock().unwrap().push((args.to_vec(), staged));
                let config = String::from_utf8_lossy(stdin);
                assert!(!config.contains("data-"));
                assert!(!config.contains("PAYLOAD"));
                assert!(
                    !args
                        .iter()
                        .any(|arg| arg.to_string_lossy().contains("PAYLOAD"))
                );
                Ok(super::super::CurlOutput {
                    stdout: b"HTTP/2 200\r\n\r\n{\"success\":true}".to_vec(),
                    success: true,
                })
            }
        }
        let mut journal = Journal(Mutex::new(Vec::new()));
        let response = exchange(&mut journal, Duration::from_secs(1), &request).unwrap();
        assert_eq!(response.status, 200);
        let journal = journal.0.into_inner().unwrap();
        let (args, staged) = &journal[0];
        assert_eq!(staged, &body);
        let position = args
            .iter()
            .position(|arg| arg == "--data-binary")
            .expect("data-binary arg");
        let path = std::path::PathBuf::from(
            args[position + 1]
                .to_string_lossy()
                .strip_prefix('@')
                .unwrap()
                .to_string(),
        );
        assert!(
            !path.exists(),
            "the staged body is removed after the exchange"
        );

        for (url, headers, body) in [
            (UPLOAD, headers(), &body[..body.len() - 2]),
            (
                UPLOAD,
                {
                    let mut other = headers();
                    other[3].1 = "multipart/form-data; boundary=other".into();
                    other
                },
                &body[..],
            ),
            (
                UPLOAD,
                vec![("Blade-Auth".into(), "TEST-ONLY".into())],
                &body[..],
            ),
            (&format!("{UPLOAD}?extra=1"), headers(), &body[..]),
        ] {
            let invalid = upload_request(url, headers, body);
            assert!(curl_config(&invalid).is_err(), "accepted an invalid upload");
        }
    }

    #[test]
    fn homework_submit_body_is_validated_and_never_probed() {
        const SUBMIT: &str = "https://apiucloud.bupt.edu.cn/ykt-site/work/submit";
        let headers = vec![
            ("Blade-Auth".into(), "TEST-ONLY".into()),
            ("Tenant-Id".into(), "000000".into()),
            (
                "Authorization".into(),
                "Basic c3dvcmQ6c3dvcmRfc2VjcmV0".into(),
            ),
            ("Content-Type".into(), "application/json".into()),
        ];
        let body = r#"{"assignmentId":"1","assignmentType":0,"assignmentContent":"作业内容","attachmentIds":["r1"],"userId":"u1","groupId":"","commitId":""}"#.as_bytes().to_vec();
        fn submit_request(url: &str, headers: Vec<(String, String)>, body: &[u8]) -> Request {
            Request {
                method: "POST",
                url: url.into(),
                headers,
                body: body.to_vec(),
            }
        }
        let request = submit_request(SUBMIT, headers.clone(), &body);
        assert!(curl_config(&request).is_ok());
        assert!(curl_probe_config(&request).is_err());
        let config = String::from_utf8(curl_config(&request).unwrap()).unwrap();
        assert!(config.contains("data-raw"));

        // Pretty-printed JSON with a raw newline between tokens parses fine
        // but must never reach the curl configuration text.
        let pretty = b"{\n  \"assignmentId\": \"1\",\"assignmentType\":0,\"assignmentContent\":\"\",\"attachmentIds\":[],\"userId\":\"u1\",\"groupId\":\"\",\"commitId\":\"\"\n}".to_vec();
        assert!(curl_config(&submit_request(SUBMIT, headers.clone(), &pretty)).is_err());
        for invalid in [
            br#"{"assignmentId":"1"}"#.to_vec(),
            br#"{"assignmentId":"1","assignmentType":1,"assignmentContent":"","attachmentIds":[],"userId":"u1","groupId":"","commitId":""}"#.to_vec(),
            br#"{"assignmentId":"","assignmentType":0,"assignmentContent":"","attachmentIds":[],"userId":"u1","groupId":"","commitId":""}"#.to_vec(),
            br#"{"assignmentId":"1","assignmentType":0,"assignmentContent":"","attachmentIds":[],"userId":"u1","groupId":"g","commitId":""}"#.to_vec(),
            br#"{"assignmentId":"1","assignmentType":0,"assignmentContent":"","attachmentIds":[""],"userId":"u1","groupId":"","commitId":""}"#.to_vec(),
            br#"not json"#.to_vec(),
        ] {
            let bad = submit_request(SUBMIT, headers.clone(), &invalid);
            assert!(curl_config(&bad).is_err(), "accepted an invalid submit body");
        }
        let oversized = format!(
            "{{\"assignmentId\":\"1\",\"assignmentType\":0,\"assignmentContent\":\"{}\",\"attachmentIds\":[],\"userId\":\"u1\",\"groupId\":\"\",\"commitId\":\"\"}}",
            "x".repeat(200_000)
        );
        assert!(
            curl_config(&submit_request(
                SUBMIT,
                headers.clone(),
                oversized.as_bytes()
            ))
            .is_err()
        );
        let wrong_type = submit_request(
            SUBMIT,
            vec![
                ("Blade-Auth".into(), "TEST-ONLY".into()),
                ("Tenant-Id".into(), "000000".into()),
                (
                    "Authorization".into(),
                    "Basic c3dvcmQ6c3dvcmRfc2VjcmV0".into(),
                ),
                ("Content-Type".into(), "text/plain".into()),
            ],
            &body,
        );
        assert!(curl_config(&wrong_type).is_err());
    }

    #[test]
    fn electricity_transport_accepts_only_known_tls_reads() {
        let mut request = Request {
            method: "GET",
            url: ELECTRICITY_LANDING.into(),
            headers: vec![],
            body: vec![],
        };
        assert!(curl_config(&request).is_ok());
        request.method = "POST";
        request.headers = vec![
            ("Referer".into(), ELECTRICITY_LANDING.into()),
            ("X-Requested-With".into(), "XMLHttpRequest".into()),
            (
                "Content-Type".into(),
                "application/x-www-form-urlencoded".into(),
            ),
            ("Cookie".into(), "TEST=ONLY".into()),
        ];
        let base = "https://app.bupt.edu.cn/buptdf/wap/default";
        for (path, form) in [
            ("part", "areaid=1"),
            ("floor", "partmentId=test&areaid=2"),
            ("drom", "areaid=1&partmentId=test&floorId=1"),
            (
                "search",
                "areaid=1&partmentId=test&floorId=1&dromNumber=TEST",
            ),
        ] {
            request.url = format!("{base}/{path}");
            request.body = form.as_bytes().to_vec();
            assert!(curl_config(&request).is_ok());
            request.url = format!("{base}/{path}?ticket=TEST");
            assert!(curl_config(&request).is_err());
        }
        for url in [
            format!("{base}/recharge"),
            format!("{base}/chong"),
            "http://app.bupt.edu.cn/buptdf/wap/default/search".into(),
            "https://app.bupt.edu.cn.example.invalid/buptdf/wap/default/search".into(),
        ] {
            request.url = url;
            assert!(curl_config(&request).is_err());
        }
        request.url = format!("{base}/part");
        for form in [
            "areaid=1&areaid=2",
            "areaid=3",
            "areaid=1&amount=2",
            "areaid=%00",
        ] {
            request.body = form.as_bytes().to_vec();
            assert!(curl_config(&request).is_err());
        }
        request.body = b"areaid=1".to_vec();
        request.headers[0].1 = "https://outside.example".into();
        assert!(curl_config(&request).is_err());
    }

    #[test]
    fn homework_transport_allows_only_the_expected_read_routes_and_json_shape() {
        let base = "https://apiucloud.bupt.edu.cn";
        let mut request = Request {
            method: "POST",
            url: format!("{base}/ykt-site/work/student/list"),
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: br#"{"siteId":"site-1","userId":"TEST-ONLY-USER","current":1,"size":50}"#
                .to_vec(),
        };
        assert!(curl_config(&request).is_ok());
        let valid_body = request.body.clone();
        for invalid in [
            "{}",
            "{\"password\":\"TEST-ONLY\"}",
            "{\"siteId\":\"site-1\",\"userId\":\"TEST-ONLY-USER\",\"current\":0,\"size\":50}",
        ] {
            request.body = invalid.as_bytes().to_vec();
            assert!(curl_config(&request).is_err());
        }
        request.body = valid_body;
        request.headers.clear();
        assert!(curl_config(&request).is_err());
        request
            .headers
            .push(("Content-Type".into(), "application/json".into()));
        for path in [
            "/ykt-site/work/student/list?test=1",
            "/ykt-site/work/student/submit",
            "/blade-source/resource/upload",
            "/ykt-site/work/detail",
        ] {
            request.url = format!("{base}{path}");
            assert!(curl_config(&request).is_err());
        }
        request.headers = vec![("Blade-Auth".into(), "TEST-ONLY-TOKEN".into())];
        request.body.clear();
        request.method = "GET";
        for path in [
            "/ykt-site/work/detail?assignmentId=1",
            "/ykt-site/work/submit-view?assignmentId=1",
            "/blade-source/resource/preview-url?resourceId=1",
        ] {
            request.url = format!("{base}{path}");
            assert!(curl_config(&request).is_ok());
        }
        request.url = "https://outside.example/ykt-site/work/detail?assignmentId=1".into();
        assert!(curl_config(&request).is_err());
    }

    #[test]
    fn request_scope_and_curl_option_boundaries() {
        let mut request = Request {
            method: "POST",
            url: "https://auth.bupt.edu.cn/authserver/login".into(),
            headers: vec![("Cookie".into(), "test=\"a\\b\"".into())],
            body: b"password=TEST-ONLY%22%0A%5C".to_vec(),
        };
        let options = String::from_utf8(curl_config(&request).unwrap()).unwrap();
        assert!(options.contains("header = \"Cookie: test=\\\"a\\\\b\\\"\"\n"));
        assert!(options.ends_with("data-raw = \"password=TEST-ONLY%22%0A%5C\"\n"));
        for url in [
            "https://auth.bupt.edu.cn:443/authserver/login",
            "https://auth.bupt.edu.cn@outside.example/authserver/login",
            "https://auth.bupt.edu.cn/authserver/login?extra=1",
            "https://auth.bupt.edu.cn/authserver/other",
            "https://ucloud.bupt.edu.cn/uclass",
            "https://apiucloud.bupt.edu.cn/ykt-basics/oauth/token",
        ] {
            request.url = url.into();
            assert!(curl_config(&request).is_err());
        }
        request.url = "https://auth.bupt.edu.cn/authserver/login".into();
        request.headers[0].1 = "cookie=TEST\nlocation = \"https://outside.example/\"".into();
        assert!(curl_config(&request).is_err());
    }

    #[test]
    fn response_preserves_explicit_status_and_repeated_cookies() {
        let raw = b"HTTP/1.1 103 Early Hints\r\nLink: </asset>\r\n\r\nHTTP/2 302\r\nSet-Cookie: a=TEST\r\nset-cookie: b=TEST\r\nlocation: https://ucloud.bupt.edu.cn/uclass\r\n\r\n<response>";
        let response = parse_response(raw).unwrap();
        assert_eq!(response.status, 302);
        assert_eq!(response.headers.len(), 3);
        assert_eq!(response.headers[1], ("set-cookie".into(), "b=TEST".into()));
        assert_eq!(response.body, b"<response>");
        assert!(parse_response(b"HTTP/2 302\r\nLocation: one\r\nLocation: two\r\n\r\n").is_err());
        assert!(parse_response(b"HTTP/2 101\r\n\r\n").is_err());
    }
}
