//! Transport-injected CAS and UCloud protocol slice.
//!
//! URL normalization and cookie rules use maintained implementations. The
//! module remains test-only until the scoped HTTP transport, credential store,
//! and service session wiring are reviewed together.
//!
//! It proves the request shape, redirect policy and response classification
//! with synthetic `Scripted` transports. A successful `cas_login` is the only
//! way to obtain a `CasLogin`, and it must include a service ticket issued by
//! the ordinary CAS verification request. Future account persistence must take
//! that value so a failed login cannot write local state.

use crate::cli::{CliError, CliResult};
use crate::transport::{Request, Response, Transport};
use cookie_store::{Cookie, CookieDomain, CookieStore};
use serde_json::{Map, Value, json};
use std::fmt;
use url::Url;

const CAS_LOGIN_URL: &str = "https://auth.bupt.edu.cn/authserver/login";
const CAS_HOST: &str = "auth.bupt.edu.cn";
const UCLOUD_HOST: &str = "ucloud.bupt.edu.cn";
const UCLOUD_API: &str = "https://apiucloud.bupt.edu.cn";
const UCLOUD_API_HOST: &str = "apiucloud.bupt.edu.cn";
const UCLOUD_TOKEN_URL: &str = "https://apiucloud.bupt.edu.cn/ykt-basics/oauth/token";
const UCLOUD_BASIC_AUTH: &str = "Basic c3dvcmQ6c3dvcmRfc2VjcmV0";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const MAX_CAS_REDIRECTS: usize = 5;
const FORM_CONTENT_TYPE: &str = "application/x-www-form-urlencoded";

/// A verified CAS login with a service ticket issued by CAS. Fields are
/// private so only a completed login can construct this value.
pub(super) struct CasLogin {
    cookies: CookieJar,
    ticket: Option<String>,
    ucloud: Option<UCloudToken>,
}

/// UCloud token metadata in the shape persisted by the existing session cache.
pub(super) struct UCloudToken {
    access_token: String,
    refresh_token: String,
    user_id: Option<Value>,
    user_name: Option<String>,
    real_name: Option<String>,
    expires_at: Option<f64>,
    jwt_payload: Map<String, Value>,
}

impl fmt::Debug for CasLogin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CasLogin")
            .field("ticket", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for UCloudToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UCloudToken")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

impl CasLogin {
    /// Fetch a UCloud token only when a caller needs one. The service ticket
    /// obtained during login is consumed first; later needs request a new CAS
    /// service ticket. No OAuth token request happens before this call.
    pub(super) fn ucloud_token(
        &mut self,
        transport: &mut dyn Transport,
    ) -> CliResult<&UCloudToken> {
        let cached = self
            .ucloud
            .as_ref()
            .is_some_and(|token| !token.is_expired());
        if !cached {
            let ticket = match self.ticket.take() {
                Some(ticket) => ticket,
                None => request_cas_service_ticket(transport, &mut self.cookies)?,
            };
            let token = exchange_ucloud_token(transport, &ticket)?;
            self.ucloud = Some(token);
        }
        Ok(self.ucloud.as_ref().expect("UCloud token was just set"))
    }

    /// Perform one authenticated UCloud read with the lazily obtained token.
    /// The path is a fixed API path supplied by a future service adapter; it is
    /// not CLI input in this migration slice.
    #[allow(dead_code)]
    pub(super) fn ucloud_read(
        &mut self,
        transport: &mut dyn Transport,
        path: &str,
    ) -> CliResult<Value> {
        let url = ucloud_read_url(path)?;
        let access_token = self.ucloud_token(transport)?.access_token.clone();
        let response = send_without_cookies(
            transport,
            Request {
                method: "GET",
                url,
                headers: ucloud_headers(access_token),
                body: Vec::new(),
            },
        )?;
        ensure_http_ok(&response, "UCloud authenticated read")?;
        parse_json_object(&response.body, "UCloud authenticated read")
    }
}

impl UCloudToken {
    #[allow(dead_code)]
    pub(super) fn access_token(&self) -> &str {
        &self.access_token
    }

    /// Existing session-cache metadata for a future persistence step.
    #[allow(dead_code)]
    pub(super) fn as_session_json(&self) -> Value {
        json!({
            "access_token": self.access_token,
            "refresh_token": self.refresh_token,
            "user_id": self.user_id.clone().unwrap_or(Value::Null),
            "user_name": self.user_name.clone().map(Value::String).unwrap_or(Value::Null),
            "real_name": self.real_name.clone().map(Value::String).unwrap_or(Value::Null),
            "jwt_payload": Value::Object(self.jwt_payload.clone()),
            "expires_at": self.expires_at.map(Value::from).unwrap_or(Value::Null),
        })
    }

    fn is_expired(&self) -> bool {
        self.expires_at.is_some_and(|expires_at| {
            expires_at <= super::unix_now() + super::UCLOUD_EXPIRY_BUFFER_SECONDS
        })
    }
}

/// Perform the ordinary CAS form login once.
pub(super) fn cas_login(
    transport: &mut dyn Transport,
    username: &str,
    password: &str,
) -> CliResult<CasLogin> {
    if username.trim().is_empty() || password.is_empty() {
        return Err(auth_error("BUPT CAS credentials are not configured"));
    }

    let mut cookies = CookieJar::default();
    let page = send_get(transport, &mut cookies, CAS_LOGIN_URL)?;
    let (page, _) = follow_get_redirects(transport, &mut cookies, page, CAS_LOGIN_URL.to_owned())?;
    ensure_http_ok(&page, "CAS login page")?;
    let page_text = String::from_utf8_lossy(&page.body);
    let execution = execution_token(&page_text)
        .ok_or_else(|| parse_error("CAS login page did not contain an execution token"))?;

    let body = form_body(&[
        ("username", username),
        ("password", password),
        ("submit", "登录"),
        ("type", "username_password"),
        ("execution", &execution),
        ("_eventId", "submit"),
    ]);
    let post = send(
        transport,
        &mut cookies,
        Request {
            method: "POST",
            url: CAS_LOGIN_URL.to_owned(),
            headers: vec![("Content-Type".into(), FORM_CONTENT_TYPE.into())],
            body,
        },
    )?;
    if matches!(post.status, 307 | 308) {
        return Err(auth_error(
            "refusing a CAS redirect that would replay the credential POST",
        ));
    }
    let (post, final_url) =
        follow_get_redirects(transport, &mut cookies, post, CAS_LOGIN_URL.to_owned())?;
    ensure_http_ok(&post, "CAS login")?;

    let final_text = String::from_utf8_lossy(&post.body);
    if is_login_page(&final_url, &final_text) {
        return Err(auth_error(
            "BUPT CAS login failed: invalid username or password",
        ));
    }

    let ticket = request_cas_service_ticket(transport, &mut cookies)?;
    Ok(CasLogin {
        cookies,
        ticket: Some(ticket),
        ucloud: None,
    })
}

/// Ask CAS for a service ticket and require an explicit redirect carrying a
/// ticket. A 2xx page, even with a Location header, is not proof of success.
fn request_cas_service_ticket(
    transport: &mut dyn Transport,
    cookies: &mut CookieJar,
) -> CliResult<String> {
    let response = send_get(transport, cookies, super::CAS_SERVICE_URL)?;
    match response.status {
        401 | 403 => Err(auth_error(format!(
            "CAS service ticket rejected authentication (HTTP {})",
            response.status
        ))),
        301 | 302 | 303 | 307 | 308 => {
            let location = header(&response.headers, "location").ok_or_else(|| {
                parse_error("CAS service redirect response has no Location header")
            })?;
            ticket_from_location(location)
        }
        200..=299 => {
            if is_login_page_body(&response.body) {
                Err(auth_error(
                    "BUPT CAS session is no longer valid; no service ticket was issued",
                ))
            } else {
                Err(auth_error(
                    "BUPT CAS login did not produce a service ticket",
                ))
            }
        }
        status => Err(upstream_http_error("CAS service ticket request", status)),
    }
}

fn exchange_ucloud_token(transport: &mut dyn Transport, ticket: &str) -> CliResult<UCloudToken> {
    let body = form_body(&[("grant_type", "third"), ("ticket", ticket)]);
    let response = send_without_cookies(
        transport,
        Request {
            method: "POST",
            url: UCLOUD_TOKEN_URL.to_owned(),
            headers: vec![
                ("Content-Type".into(), FORM_CONTENT_TYPE.into()),
                ("Authorization".into(), UCLOUD_BASIC_AUTH.into()),
            ],
            body,
        },
    )?;
    ensure_http_ok(&response, "UCloud token exchange")?;
    let value = parse_json_object(&response.body, "UCloud token response")?;
    let object = value
        .as_object()
        .ok_or_else(|| parse_error("UCloud token response must be a JSON object"))?;
    let access_token = object
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty()
                && !value
                    .chars()
                    .any(|ch| ch.is_control() || ch.is_whitespace())
        })
        .ok_or_else(|| auth_error("UCloud token response missing access_token"))?
        .to_owned();

    let jwt_payload = super::jwt_payload(&access_token).unwrap_or_default();
    let expires_at = jwt_payload
        .get("exp")
        .and_then(super::number_value)
        .filter(|expires_at| expires_at.is_finite());
    let user_id = jwt_payload
        .get("user_id")
        .filter(|value| !value.is_null())
        .cloned()
        .or_else(|| {
            object
                .get("user_id")
                .filter(|value| !value.is_null())
                .cloned()
        });
    let user_name = script_or_none(jwt_payload.get("user_name"));
    let real_name = script_or_none(jwt_payload.get("real_name"));
    let refresh_token = object
        .get("refresh_token")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();

    Ok(UCloudToken {
        access_token,
        refresh_token,
        user_id,
        user_name,
        real_name,
        expires_at,
        jwt_payload,
    })
}

fn send_get(
    transport: &mut dyn Transport,
    cookies: &mut CookieJar,
    url: &str,
) -> CliResult<Response> {
    send(
        transport,
        cookies,
        Request {
            method: "GET",
            url: url.to_owned(),
            headers: Vec::new(),
            body: Vec::new(),
        },
    )
}

fn send_without_cookies(transport: &mut dyn Transport, request: Request) -> CliResult<Response> {
    let mut headers = vec![("User-Agent".into(), USER_AGENT.into())];
    headers.extend(request.headers);
    transport.send(Request {
        method: request.method,
        url: request.url,
        headers,
        body: request.body,
    })
}

fn send(
    transport: &mut dyn Transport,
    cookies: &mut CookieJar,
    request: Request,
) -> CliResult<Response> {
    let mut headers = vec![("User-Agent".into(), USER_AGENT.into())];
    if let Some(cookie) = cookies.header_for(&request.url) {
        headers.push(("Cookie".into(), cookie));
    }
    headers.extend(request.headers);
    let response = transport.send(Request {
        method: request.method,
        url: request.url.clone(),
        headers,
        body: request.body,
    })?;
    cookies.store_response(&request.url, &response.headers);
    Ok(response)
}

fn follow_get_redirects(
    transport: &mut dyn Transport,
    cookies: &mut CookieJar,
    mut response: Response,
    mut url: String,
) -> CliResult<(Response, String)> {
    for followed in 0..=MAX_CAS_REDIRECTS {
        if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
            if followed == MAX_CAS_REDIRECTS {
                break;
            }
            let location = header(&response.headers, "location")
                .ok_or_else(|| parse_error("CAS redirect response has no Location header"))?;
            let target = allowed_cas_redirect(location)?;
            response = send_get(transport, cookies, &target)?;
            url = target;
        } else {
            return Ok((response, url));
        }
    }
    Err(parse_error("too many CAS redirects"))
}

fn allowed_cas_redirect(raw: &str) -> CliResult<String> {
    let raw = raw.trim();
    let parsed = parse_https_url(raw).ok_or_else(redirect_error)?;
    let path = parsed.path();
    let allowed_path = match parsed.host_str() {
        Some(CAS_HOST) => path == "/authserver" || path.starts_with("/authserver/"),
        Some(UCLOUD_HOST) => path == "/uclass" || path.starts_with("/uclass/"),
        _ => false,
    };
    if !allowed_path {
        return Err(redirect_error());
    }
    Ok(parsed.into())
}

fn ticket_from_location(location: &str) -> CliResult<String> {
    let parsed = parse_https_url(location.trim()).ok_or_else(redirect_error)?;
    if parsed.host_str() != Some(UCLOUD_HOST) || parsed.path() != "/uclass" {
        return Err(redirect_error());
    }
    let mut tickets = parsed
        .query()
        .unwrap_or_default()
        .split('&')
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key == "ticket").then_some(value)
        });
    let raw = tickets
        .next()
        .ok_or_else(|| auth_error("No CAS ticket issued for UCloud (login may have expired)"))?;
    if tickets.next().is_some() {
        return Err(auth_error("CAS issued an ambiguous UCloud ticket"));
    }
    let ticket = percent_decode(raw)
        .filter(|ticket| {
            !ticket.is_empty()
                && ticket.len() <= 2048
                && !ticket
                    .chars()
                    .any(|ch| ch.is_control() || ch.is_whitespace())
        })
        .ok_or_else(|| auth_error("CAS issued an invalid UCloud ticket"))?;
    Ok(ticket)
}

fn ucloud_read_url(path: &str) -> CliResult<String> {
    if !path.starts_with('/')
        || path.starts_with("//")
        || path.contains('#')
        || path.contains('\\')
        || path.chars().any(|ch| ch.is_control() || ch.is_whitespace())
        || path.split('/').any(|segment| matches!(segment, "." | ".."))
    {
        return Err(parse_error("invalid UCloud read path"));
    }
    let url = format!("{UCLOUD_API}{path}");
    let parsed = parse_https_url(&url).ok_or_else(|| parse_error("invalid UCloud read URL"))?;
    if parsed.host_str() != Some(UCLOUD_API_HOST) {
        return Err(parse_error("invalid UCloud read host"));
    }
    Ok(parsed.into())
}

fn ucloud_headers(access_token: String) -> Vec<(String, String)> {
    vec![
        ("Blade-Auth".into(), access_token),
        ("Tenant-Id".into(), "000000".into()),
        ("Authorization".into(), UCLOUD_BASIC_AUTH.into()),
    ]
}

fn ensure_http_ok(response: &Response, operation: &str) -> CliResult<()> {
    match response.status {
        200..=299 => Ok(()),
        401 | 403 => Err(auth_error(format!(
            "{operation} rejected authentication (HTTP {})",
            response.status
        ))),
        status => Err(upstream_http_error(operation, status)),
    }
}

fn upstream_http_error(operation: &str, status: u16) -> CliError {
    let hint = if status == 429 {
        "The campus service is rate limiting requests; wait before retrying."
    } else {
        "The campus service returned an unexpected response."
    };
    CliError::new(
        5,
        "upstream",
        format!("{operation} failed with HTTP {status}"),
        hint,
    )
}

fn parse_json_object(bytes: &[u8], operation: &str) -> CliResult<Value> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| parse_error(format!("{operation} response was not valid JSON")))?;
    if !value.is_object() {
        return Err(parse_error(format!(
            "{operation} response must be a JSON object"
        )));
    }
    Ok(value)
}

fn script_or_none(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn auth_error(message: impl Into<String>) -> CliError {
    CliError::new(
        3,
        "auth",
        message,
        "Check the student id and password, then run `bupt login` again.",
    )
}

fn redirect_error() -> CliError {
    CliError::new(
        3,
        "auth",
        "refusing a CAS redirect outside the approved HTTPS hosts",
        "Expected auth.bupt.edu.cn or the UCloud uclass service over HTTPS.",
    )
}

fn parse_error(message: impl Into<String>) -> CliError {
    CliError::new(
        5,
        "upstream",
        message,
        "The campus service returned an unexpected response.",
    )
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn is_login_page(url: &str, body: &str) -> bool {
    parse_https_url(url).is_some_and(|parsed| {
        parsed.host_str() == Some(CAS_HOST) && parsed.path() == "/authserver/login"
    }) || is_login_page_body(body.as_bytes())
}

fn is_login_page_body(body: &[u8]) -> bool {
    execution_token(&String::from_utf8_lossy(body)).is_some()
}

fn execution_token(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let mut offset = 0;
    while let Some(found) = lower[offset..].find("<input") {
        let start = offset + found;
        let after = start + "<input".len();
        let Some(next) = lower.as_bytes().get(after) else {
            break;
        };
        if !next.is_ascii_whitespace() && *next != b'>' && *next != b'/' {
            offset = after;
            continue;
        }
        let Some(end) = lower[after..].find('>') else {
            break;
        };
        let tag = &html[after..after + end];
        let attributes = tag_attributes(tag);
        let is_execution = attributes.iter().any(|(key, value)| {
            key.eq_ignore_ascii_case("name") && value.eq_ignore_ascii_case("execution")
        });
        if is_execution {
            let value = attributes
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("value"))
                .map(|(_, value)| value.clone())
                .filter(|value| !value.is_empty());
            if value.is_some() {
                return value;
            }
        }
        offset = after + end + 1;
    }
    None
}

fn tag_attributes(tag: &str) -> Vec<(String, String)> {
    let bytes = tag.as_bytes();
    let mut attributes = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        while index < bytes.len() && (bytes[index].is_ascii_whitespace() || bytes[index] == b'/') {
            index += 1;
        }
        if index >= bytes.len() {
            break;
        }
        let name_start = index;
        while index < bytes.len()
            && !bytes[index].is_ascii_whitespace()
            && bytes[index] != b'='
            && bytes[index] != b'/'
        {
            index += 1;
        }
        if name_start == index {
            index += 1;
            continue;
        }
        let name = tag[name_start..index].to_ascii_lowercase();
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        let mut value = String::new();
        if index < bytes.len() && bytes[index] == b'=' {
            index += 1;
            while index < bytes.len() && bytes[index].is_ascii_whitespace() {
                index += 1;
            }
            if index < bytes.len() && matches!(bytes[index], b'"' | b'\'') {
                let quote = bytes[index];
                index += 1;
                let value_start = index;
                while index < bytes.len() && bytes[index] != quote {
                    index += 1;
                }
                value = tag[value_start..index].to_owned();
                if index < bytes.len() {
                    index += 1;
                }
            } else {
                let value_start = index;
                while index < bytes.len() && !bytes[index].is_ascii_whitespace() {
                    index += 1;
                }
                value = tag[value_start..index].to_owned();
            }
        }
        attributes.push((name, decode_html_entities(&value)));
    }
    attributes
}

fn decode_html_entities(text: &str) -> String {
    let mut output = String::new();
    let mut rest = text;
    while let Some(position) = rest.find('&') {
        output.push_str(&rest[..position]);
        let after = &rest[position + 1..];
        let Some(end) = after.find(';').filter(|end| *end <= 12) else {
            output.push('&');
            rest = after;
            continue;
        };
        let entity = &after[..end];
        if let Some(character) = decode_entity(entity) {
            output.push(character);
            rest = &after[end + 1..];
        } else {
            output.push('&');
            rest = after;
        }
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
        _ => {
            if let Some(hex) = entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
            {
                u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
            } else if let Some(decimal) = entity.strip_prefix('#') {
                decimal.parse::<u32>().ok().and_then(char::from_u32)
            } else {
                None
            }
        }
    }
}

fn form_body(fields: &[(&str, &str)]) -> Vec<u8> {
    fields
        .iter()
        .map(|(key, value)| format!("{}={}", form_encode(key), form_encode(value)))
        .collect::<Vec<_>>()
        .join("&")
        .into_bytes()
}

fn form_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else if byte == b' ' {
            encoded.push('+');
        } else {
            encoded.push('%');
            encoded.push(hex_digit(byte >> 4));
            encoded.push(hex_digit(byte & 0x0f));
        }
    }
    encoded
}

fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => char::from(b'0' + value),
        10..=15 => char::from(b'A' + value - 10),
        _ => unreachable!("nibble is always below 16"),
    }
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = *bytes.get(index + 1)?;
            let low = *bytes.get(index + 2)?;
            output.push(hex_value(high)? << 4 | hex_value(low)?);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[derive(Default)]
struct CookieJar(CookieStore);

impl CookieJar {
    fn store_response(&mut self, request_url: &str, headers: &[(String, String)]) {
        let Some(url) = parse_https_url(request_url).filter(|url| url.host_str() == Some(CAS_HOST))
        else {
            return;
        };
        for (name, value) in headers {
            if !name.eq_ignore_ascii_case("set-cookie") {
                continue;
            }
            let Ok(cookie) = Cookie::parse(value.clone(), &url) else {
                continue;
            };
            // No public-suffix database is needed: accept only CAS and its campus parent.
            if matches!(&cookie.domain,
                CookieDomain::HostOnly(host) if host == CAS_HOST)
                || matches!(&cookie.domain,
                    CookieDomain::Suffix(domain) if matches!(domain.as_str(), CAS_HOST | "bupt.edu.cn"))
            {
                let _ = self.0.insert(cookie, &url);
            }
        }
    }

    fn header_for(&self, raw: &str) -> Option<String> {
        let url = parse_https_url(raw).filter(|url| url.host_str() == Some(CAS_HOST))?;
        let values: Vec<_> = self
            .0
            .get_request_values(&url)
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        (!values.is_empty()).then(|| values.join("; "))
    }
}

fn parse_https_url(raw: &str) -> Option<Url> {
    // Keep the established allowlist syntax strict before canonicalizing. In
    // particular, Url drops explicit default ports, so check the raw authority.
    let rest = raw.strip_prefix("https://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty()
        || !authority
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.'))
        || raw.contains(['\\', '#'])
        || raw.chars().any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return None;
    }
    let path = rest.strip_prefix(authority)?.split('?').next()?;
    if path.contains("//")
        || path.split('/').any(|segment| {
            let path_segment = segment.to_ascii_lowercase();
            matches!(
                path_segment.as_str(),
                "." | ".." | "%2e" | ".%2e" | "%2e." | "%2e%2e"
            ) || path_segment.contains("%2f")
                || path_segment.contains("%5c")
        })
    {
        return None;
    }
    Url::parse(raw)
        .ok()
        .filter(|url| url.scheme() == "https" && url.host_str().is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::testing::Scripted;

    const USERNAME: &str = "2026000001";
    const PASSWORD: &str = "TEST-ONLY-CAS-PASSWORD 7f3a&x";
    const EXECUTION: &str = "TEST-ONLY-EXECUTION-1";
    const COOKIE_SECRET: &str = "TEST-ONLY-CAS_SESSION-a1b2";
    const ACCESS_SECRET: &str = "eyJhbGciOiJub25lIn0.eyJ1c2VyX2lkIjoiNDIiLCJ1c2VyX25hbWUiOiIyMDI2MDAwMDAxIiwicmVhbF9uYW1lIjoiU3ludGhldGljIFRlc3RlciIsImV4cCI6NDEwMjQ0NDgwMH0.synthetic-signature";
    const EXPIRED_ACCESS_SECRET: &str =
        "eyJhbGciOiJub25lIn0.eyJ1c2VyX2lkIjoiNDIiLCJleHAiOjF9.synthetic-signature";
    const TICKET_SECRET: &str = "ST-TEST-ONLY-TICKET-c3d4";
    const TICKET_URL: &str = "https://ucloud.bupt.edu.cn/uclass?ticket=ST-TEST-ONLY-TICKET-c3d4";
    const FINAL_BODY: &str = "<html><body>synthetic ucloud workspace</body></html>";

    fn response(status: u16, headers: &[(&str, &str)], body: &str) -> Response {
        Response {
            status,
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
            body: body.as_bytes().to_vec(),
        }
    }

    fn enqueue(
        transport: &mut Scripted,
        method: &'static str,
        url: &str,
        status: u16,
        headers: &[(&str, &str)],
        body: &str,
    ) {
        transport
            .expected
            .push_back((method, url.to_owned(), Ok(response(status, headers, body))));
    }

    fn enqueue_transport_error(transport: &mut Scripted, method: &'static str, url: &str) {
        transport.expected.push_back((
            method,
            url.to_owned(),
            Err(CliError::new(
                4,
                "network",
                "synthetic connection failure",
                "Check the network connection and retry.",
            )),
        ));
    }

    fn login_page() -> Response {
        response(
            200,
            &[(
                "Set-Cookie",
                "CAS_SESSION=TEST-ONLY-CAS_SESSION-a1b2; Path=/authserver; Secure; HttpOnly",
            )],
            &format!(
                "<html><form><input value='{EXECUTION}' type='hidden' name='execution'></form></html>"
            ),
        )
    }

    fn cas_script_before_ticket_with_final_body(final_body: &str) -> Scripted {
        let mut transport = Scripted::default();
        transport
            .expected
            .push_back(("GET", CAS_LOGIN_URL.into(), Ok(login_page())));
        enqueue(
            &mut transport,
            "POST",
            CAS_LOGIN_URL,
            302,
            &[
                ("Location", TICKET_URL),
                (
                    "Set-Cookie",
                    "CASTGC=TEST-ONLY-CASTGC-e5f6; Path=/authserver; Secure; HttpOnly",
                ),
            ],
            "",
        );
        enqueue(&mut transport, "GET", TICKET_URL, 200, &[], final_body);
        transport
    }

    fn cas_script_before_ticket() -> Scripted {
        cas_script_before_ticket_with_final_body(FINAL_BODY)
    }

    fn cas_script_with_ticket_response(
        status: u16,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Scripted {
        let mut transport = cas_script_before_ticket();
        enqueue(
            &mut transport,
            "GET",
            super::super::CAS_SERVICE_URL,
            status,
            headers,
            body,
        );
        transport
    }

    fn successful_cas_script() -> Scripted {
        cas_script_with_ticket_response(302, &[("Location", TICKET_URL)], "")
    }

    fn assert_no_secret(error: &CliError) {
        for secret in [PASSWORD, COOKIE_SECRET, ACCESS_SECRET, TICKET_SECRET] {
            assert!(!error.message.contains(secret));
            assert!(!error.hint.contains(secret));
        }
        assert!(!error.hint.to_ascii_lowercase().contains("password"));
    }

    fn assert_auth_error(error: CliError) {
        assert_eq!(error.code, 3, "expected auth exit: {error:?}");
        assert_eq!(error.kind, "auth");
        for secret in [PASSWORD, COOKIE_SECRET, ACCESS_SECRET, TICKET_SECRET] {
            assert!(!error.message.contains(secret));
            assert!(!error.hint.contains(secret));
        }
    }

    fn assert_upstream_error(error: CliError) {
        assert_eq!(error.code, 5, "expected upstream exit: {error:?}");
        assert_eq!(error.kind, "upstream");
        assert_no_secret(&error);
    }

    fn assert_network_error(error: CliError) {
        assert_eq!(error.code, 4, "expected network exit: {error:?}");
        assert_eq!(error.kind, "network");
        for secret in [PASSWORD, COOKIE_SECRET, ACCESS_SECRET, TICKET_SECRET] {
            assert!(!error.message.contains(secret));
            assert!(!error.hint.contains(secret));
        }
    }

    fn assert_no_token_request(transport: &Scripted) {
        assert!(
            transport
                .calls
                .iter()
                .all(|call| call.url != UCLOUD_TOKEN_URL),
            "login must not request a UCloud OAuth token"
        );
    }

    fn assert_single_password_post(transport: &Scripted) {
        assert_eq!(
            transport
                .calls
                .iter()
                .filter(|call| call.method == "POST" && call.url == CAS_LOGIN_URL)
                .count(),
            1,
            "the password must be submitted exactly once"
        );
    }

    fn request_header<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
        request
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn cas_cookie_jar_honors_scope_defaults_and_expiration() {
        let mut jar = CookieJar::default();
        jar.store_response(
            CAS_LOGIN_URL,
            &[
                (
                    "Set-Cookie".into(),
                    "local=TEST-ONLY; Secure; HttpOnly".into(),
                ),
                (
                    "Set-Cookie".into(),
                    "campus=TEST-ONLY; Domain=.bupt.edu.cn; Path=/; Secure".into(),
                ),
                (
                    "Set-Cookie".into(),
                    "public=TEST-ONLY; Domain=.edu.cn; Path=/".into(),
                ),
                (
                    "Set-Cookie".into(),
                    "expired=TEST-ONLY; Expires=Wed, 01 Jan 2003 00:00:00 GMT".into(),
                ),
            ],
        );
        let login = jar.header_for(CAS_LOGIN_URL).unwrap();
        assert!(login.contains("local=TEST-ONLY") && login.contains("campus=TEST-ONLY"));
        assert!(!login.contains("public=") && !login.contains("expired="));
        assert_eq!(
            jar.header_for("https://auth.bupt.edu.cn/other").as_deref(),
            Some("campus=TEST-ONLY")
        );
        assert!(
            jar.header_for("https://ucloud.bupt.edu.cn/uclass")
                .is_none()
        );
        assert!(
            jar.header_for("https://apiucloud.bupt.edu.cn/ykt-site/")
                .is_none()
        );
        jar.store_response(
            "https://ucloud.bupt.edu.cn/uclass",
            &[(
                "Set-Cookie".into(),
                "campus=INJECTED; Domain=.bupt.edu.cn; Path=/".into(),
            )],
        );
        assert!(
            jar.header_for(CAS_LOGIN_URL)
                .unwrap()
                .contains("campus=TEST-ONLY")
        );
        jar.store_response(
            CAS_LOGIN_URL,
            &[("Set-Cookie".into(), "local=; Max-Age=0; Secure".into())],
        );
        assert_eq!(
            jar.header_for(CAS_LOGIN_URL).as_deref(),
            Some("campus=TEST-ONLY")
        );
    }

    #[test]
    fn canonical_url_must_stay_inside_the_original_campus_scope() {
        assert_eq!(
            allowed_cas_redirect("https://AUTH.BUPT.EDU.CN/authserver/a").unwrap(),
            "https://auth.bupt.edu.cn/authserver/a"
        );
        for unsafe_location in [
            "https://auth.bupt.edu.cn:443/authserver/a",
            "https://auth.bupt.edu.cn@outside.example/authserver/a",
            "https://auth.bupt.edu.cn/authserver/%2e%2E/elsewhere",
            "https://auth.bupt.edu.cn/authserver/%2Felsewhere",
            "https://auth.bupt.edu.cn/authserver/%5celsewhere",
            "https://auth.bupt.edu.cn./authserver/a",
            "//auth.bupt.edu.cn/authserver/a",
        ] {
            assert!(allowed_cas_redirect(unsafe_location).is_err());
        }
        assert!(
            ticket_from_location("https://ucloud.bupt.edu.cn/uclass/other?ticket=ST-TEST-ONLY")
                .is_err()
        );
        assert!(
            ticket_from_location(
                "https://ucloud.bupt.edu.cn/uclass?ticket=ST-TEST-ONLY&ticket=ST-TEST-ONLY"
            )
            .is_err()
        );
    }

    #[test]
    fn cas_login_posts_the_form_once_and_accepts_a_verified_ticket() {
        let mut transport = successful_cas_script();
        let login = cas_login(&mut transport, USERNAME, PASSWORD).expect("CAS login succeeds");
        let debug = format!("{login:?}");
        assert!(
            !debug.contains("TEST-ONLY"),
            "CAS login Debug must not leak cookies or tickets"
        );

        assert_eq!(login.ticket.as_deref(), Some(TICKET_SECRET));
        assert_eq!(transport.calls.len(), 4);
        let methods: Vec<&str> = transport.calls.iter().map(|call| call.method).collect();
        assert_eq!(methods, ["GET", "POST", "GET", "GET"]);
        let post = &transport.calls[1];
        assert_eq!(post.url, CAS_LOGIN_URL);
        let body = String::from_utf8_lossy(&post.body);
        assert!(body.contains("username=2026000001"));
        assert!(body.contains("password=TEST-ONLY-CAS-PASSWORD+7f3a%26x"));
        assert!(body.contains("submit=%E7%99%BB%E5%BD%95"));
        assert!(body.contains("type=username_password"));
        assert!(body.contains("execution=TEST-ONLY-EXECUTION-1"));
        assert!(body.contains("_eventId=submit"));
        assert_eq!(
            request_header(post, "content-type"),
            Some(FORM_CONTENT_TYPE),
            "the CAS form POST must be declared as a form"
        );
        assert!(
            request_header(post, "cookie")
                .is_some_and(|cookie| cookie.contains("CAS_SESSION=TEST-ONLY-CAS_SESSION-a1b2")),
            "the CAS page cookie must be sent with the form POST"
        );
        assert!(
            request_header(&transport.calls[2], "cookie").is_none(),
            "host-scoped CAS cookies must not cross into ucloud.bupt.edu.cn"
        );
        let ticket_get = &transport.calls[3];
        assert_eq!(ticket_get.method, "GET");
        assert_eq!(ticket_get.url, super::super::CAS_SERVICE_URL);
        assert!(
            request_header(ticket_get, "cookie").is_some(),
            "the CAS cookie jar must authenticate the service-ticket request"
        );
        assert_single_password_post(&transport);
        assert_no_token_request(&transport);
    }

    #[test]
    fn cas_login_rejects_a_200_login_page() {
        let mut transport = Scripted::default();
        transport
            .expected
            .push_back(("GET", CAS_LOGIN_URL.into(), Ok(login_page())));
        enqueue(
            &mut transport,
            "POST",
            CAS_LOGIN_URL,
            200,
            &[],
            &format!("<html><input name=\"execution\" value=\"{EXECUTION}\"></html>"),
        );

        let error = cas_login(&mut transport, USERNAME, PASSWORD).expect_err("login must fail");
        assert_auth_error(error);
        assert_eq!(transport.calls.len(), 2);
        assert_single_password_post(&transport);
        assert_no_token_request(&transport);
    }

    #[test]
    fn cas_login_rejects_method_preserving_redirects_without_retrying() {
        for status in [307, 308] {
            let mut transport = Scripted::default();
            transport
                .expected
                .push_back(("GET", CAS_LOGIN_URL.into(), Ok(login_page())));
            enqueue(
                &mut transport,
                "POST",
                CAS_LOGIN_URL,
                status,
                &[("Location", CAS_LOGIN_URL)],
                "",
            );

            let error = cas_login(&mut transport, USERNAME, PASSWORD)
                .expect_err("method-preserving redirect must fail");
            assert_auth_error(error);
            assert_eq!(transport.calls.len(), 2);
            assert_single_password_post(&transport);
            assert_no_token_request(&transport);
        }
    }

    #[test]
    fn cas_login_rejects_non_tls_and_foreign_redirects() {
        for location in [
            "http://ucloud.bupt.edu.cn/uclass?ticket=ST-x",
            "https://evil.example/uclass?ticket=ST-x",
            "https://ucloud.bupt.edu.cn.evil/uclass?ticket=ST-x",
            "https://ucloud.bupt.edu.cn@evil.example/uclass?ticket=ST-x",
            "https://ucloud.bupt.edu.cn:444/uclass?ticket=ST-x",
        ] {
            let mut transport = Scripted::default();
            transport
                .expected
                .push_back(("GET", CAS_LOGIN_URL.into(), Ok(login_page())));
            enqueue(
                &mut transport,
                "POST",
                CAS_LOGIN_URL,
                302,
                &[("Location", location)],
                "",
            );

            let error = cas_login(&mut transport, USERNAME, PASSWORD)
                .expect_err("foreign redirect must fail");
            assert_auth_error(error);
            assert_eq!(
                transport.calls.len(),
                2,
                "redirect target must not be fetched"
            );
            assert_single_password_post(&transport);
            assert_no_token_request(&transport);
        }
    }

    #[test]
    fn cas_login_classifies_http_failures_without_retrying() {
        for (status, auth) in [
            (401, true),
            (403, true),
            (400, false),
            (429, false),
            (500, false),
        ] {
            let mut transport = Scripted::default();
            transport
                .expected
                .push_back(("GET", CAS_LOGIN_URL.into(), Ok(login_page())));
            enqueue(&mut transport, "POST", CAS_LOGIN_URL, status, &[], "");

            let error = cas_login(&mut transport, USERNAME, PASSWORD)
                .expect_err("HTTP failure must not produce a session");
            if auth {
                assert_auth_error(error);
            } else {
                assert_upstream_error(error);
            }
            assert_eq!(transport.calls.len(), 2);
            assert_single_password_post(&transport);
            assert_no_token_request(&transport);
        }
    }

    #[test]
    fn connection_failure_is_network_without_retry() {
        let mut transport = Scripted::default();
        enqueue_transport_error(&mut transport, "GET", CAS_LOGIN_URL);
        let error = cas_login(&mut transport, USERNAME, PASSWORD)
            .expect_err("page connection failure must fail");
        assert_network_error(error);
        assert_eq!(transport.calls.len(), 1);
        assert_no_token_request(&transport);

        let mut transport = cas_script_before_ticket();
        enqueue_transport_error(&mut transport, "GET", super::super::CAS_SERVICE_URL);
        let error = cas_login(&mut transport, USERNAME, PASSWORD)
            .expect_err("ticket connection failure must fail");
        assert_network_error(error);
        assert_eq!(transport.calls.len(), 4);
        assert_single_password_post(&transport);
        assert_no_token_request(&transport);
    }

    #[test]
    fn cas_login_requires_an_execution_token() {
        let mut transport = Scripted::default();
        enqueue(
            &mut transport,
            "GET",
            CAS_LOGIN_URL,
            200,
            &[],
            "<html><body>synthetic page without a login form</body></html>",
        );

        let error = cas_login(&mut transport, USERNAME, PASSWORD).expect_err("must fail");
        assert_upstream_error(error);
        assert_eq!(transport.calls.len(), 1);
        assert_eq!(transport.calls[0].method, "GET");
        assert_no_token_request(&transport);
    }

    #[test]
    fn allowed_redirect_to_unrelated_200_without_ticket_fails_closed() {
        for final_body in ["", "<html><body>synthetic interstitial</body></html>"] {
            let mut transport = cas_script_before_ticket_with_final_body(final_body);
            enqueue(
                &mut transport,
                "GET",
                super::super::CAS_SERVICE_URL,
                200,
                &[],
                "",
            );

            let error = cas_login(&mut transport, USERNAME, PASSWORD)
                .expect_err("a 200 page without a ticket must not prove login");
            assert_auth_error(error);
            assert_eq!(
                transport.calls.len(),
                4,
                "the ticket check must fail closed after the final 200 page"
            );
            assert_single_password_post(&transport);
            assert_no_token_request(&transport);
        }
    }

    #[test]
    fn cas_login_requires_a_ticket_from_an_explicit_redirect() {
        for (status, location, body, code) in [
            (401, None, "", 3),
            (403, None, "", 3),
            (429, None, "", 5),
            (500, None, "", 5),
            (304, Some(TICKET_URL), "", 5),
            (302, None, "", 5),
            (302, Some("https://ucloud.bupt.edu.cn/uclass"), "", 3),
            (
                302,
                Some("https://ucloud.bupt.edu.cn/uclass?ticket="),
                "",
                3,
            ),
            (302, Some("https://evil.example/uclass?ticket=ST-x"), "", 3),
            (200, Some(TICKET_URL), "", 3),
            (200, None, "", 3),
            (
                200,
                None,
                "<html><input name=\"execution\" value=\"TEST-ONLY-EXPIRED\"></html>",
                3,
            ),
        ] {
            let mut transport = cas_script_before_ticket();
            let headers: Vec<(&str, &str)> = location
                .into_iter()
                .map(|value| ("Location", value))
                .collect();
            enqueue(
                &mut transport,
                "GET",
                super::super::CAS_SERVICE_URL,
                status,
                &headers,
                body,
            );

            let error = cas_login(&mut transport, USERNAME, PASSWORD)
                .expect_err("ticket verification must fail closed");
            assert_eq!(error.code, code, "wrong class for status {status}");
            if code == 3 {
                assert_auth_error(error);
            } else {
                assert_upstream_error(error);
            }
            assert_eq!(transport.calls.len(), 4);
            assert_single_password_post(&transport);
            assert_no_token_request(&transport);
        }
    }

    #[test]
    fn cas_login_accepts_explicit_redirect_statuses_for_service_tickets() {
        for status in [301, 302, 303, 307, 308] {
            let mut transport =
                cas_script_with_ticket_response(status, &[("Location", TICKET_URL)], "");
            let login = cas_login(&mut transport, USERNAME, PASSWORD)
                .expect("an explicit redirect with a ticket proves login");
            assert_eq!(login.ticket.as_deref(), Some(TICKET_SECRET));
            assert_eq!(transport.calls.len(), 4);
            assert_single_password_post(&transport);
            assert_no_token_request(&transport);
        }
    }

    #[test]
    fn ucloud_token_is_lazy_and_uses_the_login_ticket_once() {
        let mut transport = successful_cas_script();
        let mut login = cas_login(&mut transport, USERNAME, PASSWORD).expect("CAS login succeeds");
        assert_eq!(transport.calls.len(), 4, "login must not prewarm UCloud");
        assert_no_token_request(&transport);

        enqueue(
            &mut transport,
            "POST",
            UCLOUD_TOKEN_URL,
            200,
            &[],
            &format!(
                "{{\"access_token\":\"{ACCESS_SECRET}\",\"refresh_token\":\"TEST-ONLY-REFRESH\",\"user_id\":\"42\"}}"
            ),
        );

        let token = login
            .ucloud_token(&mut transport)
            .expect("token exchange succeeds");
        let debug = format!("{token:?}");
        assert!(
            !debug.contains(ACCESS_SECRET),
            "token Debug must redact access tokens"
        );
        assert!(
            !debug.contains("TEST-ONLY-REFRESH"),
            "token Debug must redact refresh tokens"
        );
        assert_eq!(token.access_token(), ACCESS_SECRET);
        let metadata = token.as_session_json();
        assert_eq!(metadata["user_id"], json!("42"));
        assert_eq!(metadata["user_name"], json!(USERNAME));
        assert_eq!(metadata["real_name"], json!("Synthetic Tester"));
        assert_eq!(metadata["expires_at"], json!(4102444800.0));
        assert_eq!(transport.calls.len(), 5);
        let ticket_get = &transport.calls[3];
        assert_eq!(ticket_get.url, super::super::CAS_SERVICE_URL);
        assert!(
            request_header(ticket_get, "cookie").is_some(),
            "the CAS cookie jar must authenticate the service-ticket request"
        );
        let token_post = &transport.calls[4];
        assert_eq!(token_post.url, UCLOUD_TOKEN_URL);
        assert!(
            request_header(token_post, "authorization")
                .is_some_and(|value| value == UCLOUD_BASIC_AUTH)
        );
        let body = String::from_utf8_lossy(&token_post.body);
        assert!(body.contains("grant_type=third"));
        assert!(body.contains("ticket=ST-TEST-ONLY-TICKET-c3d4"));
        assert!(
            !body.to_ascii_lowercase().contains("password"),
            "the UCloud token request must not carry the CAS password"
        );
        assert!(
            request_header(token_post, "cookie").is_none(),
            "CAS cookies must not leak to the UCloud API host"
        );

        let again = login
            .ucloud_token(&mut transport)
            .expect("cached token is reused");
        assert_eq!(again.access_token(), ACCESS_SECRET);
        assert_eq!(transport.calls.len(), 5, "cached token must not refetch");

        enqueue(
            &mut transport,
            "GET",
            "https://apiucloud.bupt.edu.cn/ykt-site/work/submit-view?assignmentId=1000000000000000001",
            200,
            &[],
            "{\"code\":200,\"data\":{\"submitted\":false}}",
        );
        let read = login
            .ucloud_read(
                &mut transport,
                "/ykt-site/work/submit-view?assignmentId=1000000000000000001",
            )
            .expect("authenticated read succeeds");
        assert_eq!(read["data"]["submitted"], json!(false));
        assert_eq!(transport.calls.len(), 6);
        let read = &transport.calls[5];
        assert_eq!(
            request_header(read, "blade-auth"),
            Some(ACCESS_SECRET),
            "the read must use the fetched token"
        );
        assert_eq!(request_header(read, "tenant-id"), Some("000000"));
        assert!(
            request_header(read, "cookie").is_none(),
            "the UCloud API must authenticate with Blade-Auth, not CAS cookies"
        );
    }

    #[test]
    fn ucloud_read_rejects_relative_or_foreign_paths() {
        for path in [
            "",
            "ykt-site/work",
            "//evil.example/ykt-site/work",
            "/../ykt-site/work",
            "/ykt-site/../work",
            "/ykt-site/work#fragment",
            "/ykt-site/work\\callback",
            "/ykt-site/work path",
        ] {
            let error = ucloud_read_url(path).expect_err("path must be rejected");
            assert_eq!(error.code, 5, "unexpected class for {path:?}");
            assert_eq!(error.kind, "upstream");
            assert_no_secret(&error);
        }

        let mut transport = successful_cas_script();
        let mut login = cas_login(&mut transport, USERNAME, PASSWORD).expect("CAS login succeeds");
        let error = login
            .ucloud_read(&mut transport, "/../ykt-site/work")
            .expect_err("invalid path must fail before token lookup");
        assert_upstream_error(error);
        assert_eq!(
            transport.calls.len(),
            4,
            "an invalid read path must not trigger a UCloud token request"
        );
        assert_no_token_request(&transport);
    }

    #[test]
    fn expired_ucloud_token_is_not_reused() {
        let mut transport = successful_cas_script();
        let mut login = cas_login(&mut transport, USERNAME, PASSWORD).expect("CAS login succeeds");
        enqueue(
            &mut transport,
            "POST",
            UCLOUD_TOKEN_URL,
            200,
            &[],
            &format!("{{\"access_token\":\"{EXPIRED_ACCESS_SECRET}\"}}"),
        );

        let token = login
            .ucloud_token(&mut transport)
            .expect("expired token exchange succeeds");
        assert_eq!(token.as_session_json()["expires_at"], json!(1.0));
        assert_eq!(transport.calls.len(), 5);

        enqueue(
            &mut transport,
            "GET",
            super::super::CAS_SERVICE_URL,
            302,
            &[("Location", TICKET_URL)],
            "",
        );
        enqueue(
            &mut transport,
            "POST",
            UCLOUD_TOKEN_URL,
            200,
            &[],
            &format!("{{\"access_token\":\"{ACCESS_SECRET}\"}}"),
        );
        let token = login
            .ucloud_token(&mut transport)
            .expect("expired token is exchanged again");
        assert_eq!(token.access_token(), ACCESS_SECRET);
        assert_eq!(
            transport.calls.len(),
            7,
            "an expired token must not be served from cache"
        );
    }

    #[test]
    fn ucloud_ticket_redirect_is_validated_before_token_exchange() {
        for location in [
            "https://evil.example/uclass?ticket=ST-x",
            "http://ucloud.bupt.edu.cn/uclass?ticket=ST-x",
        ] {
            let mut transport = successful_cas_script();
            let mut login =
                cas_login(&mut transport, USERNAME, PASSWORD).expect("CAS login succeeds");
            enqueue(
                &mut transport,
                "POST",
                UCLOUD_TOKEN_URL,
                200,
                &[],
                &format!("{{\"access_token\":\"{EXPIRED_ACCESS_SECRET}\"}}"),
            );
            login
                .ucloud_token(&mut transport)
                .expect("the login ticket is consumed");
            let baseline = transport.calls.len();
            enqueue(
                &mut transport,
                "GET",
                super::super::CAS_SERVICE_URL,
                302,
                &[("Location", location)],
                "",
            );

            let error = login
                .ucloud_token(&mut transport)
                .expect_err("foreign ticket redirect must fail");
            assert_auth_error(error);
            assert_eq!(transport.calls.len(), baseline + 1);
            assert!(
                transport.calls[baseline..]
                    .iter()
                    .all(|call| call.url != UCLOUD_TOKEN_URL),
                "invalid ticket redirect must not reach the token endpoint"
            );
        }
    }

    #[test]
    fn ucloud_token_response_requires_explicit_success() {
        for (status, body, code) in [
            (401, "{\"error\":\"unauthorized\"}", 3),
            (403, "{\"error\":\"forbidden\"}", 3),
            (429, "{\"error\":\"slow down\"}", 5),
            (500, "{}", 5),
            (200, "not json", 5),
            (200, "[]", 5),
            (200, "{\"refresh_token\":\"missing-access\"}", 3),
            (200, "{\"access_token\":\" \"}", 3),
        ] {
            let mut transport = successful_cas_script();
            let mut login =
                cas_login(&mut transport, USERNAME, PASSWORD).expect("CAS login succeeds");
            enqueue(&mut transport, "POST", UCLOUD_TOKEN_URL, status, &[], body);

            let error = login
                .ucloud_token(&mut transport)
                .expect_err("invalid token response must fail");
            assert_eq!(error.code, code, "wrong error class for {status} {body}");
            if code == 3 {
                assert_auth_error(error);
            } else {
                assert_upstream_error(error);
            }
            assert_eq!(transport.calls.len(), 5);
        }
    }
}
