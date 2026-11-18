//! The CAS/UCloud slice for a one-exchange system curl executor. All request
//! strings are written as curl options to stdin, never to argv or a file.

use super::{CurlExec, Request, Response, base_curl_arguments};
use crate::cli::{CliError, CliResult};
use std::fmt::Write;
use std::time::Duration;
use url::Url;

const MAX_REQUEST: usize = 64 * 1024;
const MAX_HEADERS: usize = 64 * 1024;

#[derive(Clone, Copy)]
enum Route {
    CasRead,
    CasForm,
    UcloudLanding,
    UcloudToken,
    UcloudRead,
}

pub(super) fn exchange(
    exec: &mut dyn CurlExec,
    timeout: Duration,
    request: &Request,
) -> CliResult<Response> {
    if timeout.is_zero() {
        return Err(rejected());
    }
    let input = curl_config(request)?;
    let mut args = base_curl_arguments(timeout);
    args.extend(["--config".into(), "-".into()]);
    let output = exec.exec(&args, &input).map_err(|_| network_failure())?;
    if !output.success {
        return Err(network_failure());
    }
    parse_response(&output.stdout)
}

fn network_failure() -> CliError {
    CliError::new(
        4,
        "network",
        "campus HTTPS request failed",
        "Check campus network connectivity. The request was not retried.",
    )
}

fn curl_config(request: &Request) -> CliResult<Vec<u8>> {
    let route = route(request).ok_or_else(rejected)?;
    let body = std::str::from_utf8(&request.body).map_err(|_| rejected())?;
    if body.len() > MAX_REQUEST
        || (!body.is_empty() && !matches!(route, Route::CasForm | Route::UcloudToken))
        || !body.bytes().all(|b| matches!(b, 0x21..=0x7e))
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
    if matches!(route, Route::CasForm | Route::UcloudToken) {
        if body.is_empty() {
            return Err(rejected());
        }
        option(&mut options, "header", "Expect:");
        option(&mut options, "data-raw", body);
    }
    Ok(options.into_bytes())
}

fn route(request: &Request) -> Option<Route> {
    let raw = &request.url;
    if raw.len() > 8192 || !raw.bytes().all(|b| matches!(b, 0x21..=0x7e)) || raw.contains('\\') {
        return None;
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
            "/ykt-site/site/list/student/current" | "/ykt-site/site/student/undone",
        ) => Some(Route::UcloudRead),
        _ => None,
    }
}

fn allowed_header(route: Route, name: &str) -> bool {
    name == "user-agent"
        || match route {
            Route::CasRead => name == "cookie",
            Route::CasForm => matches!(name, "cookie" | "content-type"),
            Route::UcloudLanding => false,
            Route::UcloudToken => matches!(name, "authorization" | "content-type"),
            Route::UcloudRead => matches!(
                name,
                "authorization" | "tenant-id" | "blade-auth" | "accept"
            ),
        }
}

fn option(config: &mut String, name: &'static str, value: &str) {
    let _ = write!(config, "{name} = \"");
    for ch in value.chars() {
        if matches!(ch, '"' | '\\') {
            config.push('\\');
        }
        config.push(ch);
    }
    config.push_str("\"\n");
}

fn parse_response(mut stdout: &[u8]) -> CliResult<Response> {
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
        "request is outside the CAS/UCloud transport policy",
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
