use crate::cli::{CliError, CliResult};
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const OPAC_PREFIX: &str = "https://opac.bupt.edu.cn/";
const OPAC_REFERER: &str = "https://opac.bupt.edu.cn/search-full-result.html";
const SEARCH_PATH: &str = "search-full-result.json";
const EBOOK_PATH: &str = "dianzi.json";
const HOLDINGS_PREFIX: &str = "bookInfo_";
const HOLDINGS_SUFFIX: &str = ".html";

const MAX_URL_BYTES: usize = 2048;
const MAX_REQUEST_BODY_BYTES: usize = 64 * 1024;
// Public-library responses are small; cap wall time and bytes regardless of a
// larger global --timeout or page size.
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const STATUS_MARKER: &str = "__BUPT_HTTP_STATUS__";
const MAX_STDOUT_BYTES: usize = MAX_RESPONSE_BYTES + STATUS_MARKER.len() + 3;
const MAX_HEADER_BYTES: usize = 1024;
const MAX_TIMEOUT_SECONDS: u64 = 120;
const CONNECT_TIMEOUT_SECONDS: u64 = 10;

/// Exchange exactly one request. Retry, authentication, and redirect decisions
/// belong to the service calling this trait, not the network adapter.
pub(crate) trait Transport {
    fn send(&mut self, request: Request) -> CliResult<Response>;
}

pub(crate) struct Request {
    pub method: &'static str,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

pub(crate) struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

pub(crate) struct Offline;

impl Transport for Offline {
    fn send(&mut self, _: Request) -> CliResult<Response> {
        Err(CliError::new(
            4,
            "network",
            "HTTP transport is not connected in this migration build",
            "",
        ))
    }
}

/// Anonymous OPAC transport backed by the system `curl` executable.
///
/// Only the three documented public-library routes are accepted. Bodies go to
/// curl on stdin, curl configuration files and proxy environment settings are
/// disabled, and redirects are never followed. Request and response internals
/// are deliberately kept out of every error message.
pub(crate) struct Curl {
    timeout: Duration,
    exec: Box<dyn CurlExec>,
}

impl Curl {
    pub(crate) fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            exec: Box::new(SystemCurl {
                program: system_curl_program(),
            }),
        }
    }
}

impl Transport for Curl {
    fn send(&mut self, request: Request) -> CliResult<Response> {
        let args = curl_arguments(&request, self.timeout)?;
        let output = self.exec.exec(&args, &request.body)?;
        if !output.success || output.stdout.len() > MAX_STDOUT_BYTES {
            return Err(transport_failure());
        }
        parse_curl_output(&output.stdout)
    }
}

trait CurlExec {
    fn exec(&mut self, args: &[OsString], body: &[u8]) -> CliResult<CurlOutput>;
}

struct CurlOutput {
    stdout: Vec<u8>,
    success: bool,
}

struct SystemCurl {
    program: PathBuf,
}

impl CurlExec for SystemCurl {
    fn exec(&mut self, args: &[OsString], body: &[u8]) -> CliResult<CurlOutput> {
        self.exec_bounded(args, body, MAX_STDOUT_BYTES)
    }
}

impl SystemCurl {
    fn exec_bounded(
        &mut self,
        args: &[OsString],
        body: &[u8],
        max_stdout: usize,
    ) -> CliResult<CurlOutput> {
        let mut child = Command::new(&self.program)
            .args(args)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| transport_failure())?;
        let Some(mut stdin) = child.stdin.take() else {
            terminate(&mut child);
            return Err(transport_failure());
        };
        let body = body.to_vec();
        let writer = match std::thread::Builder::new().spawn(move || {
            let result = stdin.write_all(&body);
            drop(stdin);
            result
        }) {
            Ok(writer) => writer,
            Err(_) => {
                terminate(&mut child);
                return Err(transport_failure());
            }
        };
        let Some(mut stdout) = child.stdout.take() else {
            terminate(&mut child);
            let _ = writer.join();
            return Err(transport_failure());
        };
        let bytes = match read_bounded(&mut stdout, max_stdout) {
            Ok(Some(bytes)) => bytes,
            Ok(None) | Err(_) => {
                terminate(&mut child);
                let _ = writer.join();
                return Err(transport_failure());
            }
        };
        // An early HTTP response can close curl's stdin before the writer
        // finishes; curl's exit status and the status marker are authoritative.
        let _ = writer.join();
        let status = match child.wait() {
            Ok(status) => status,
            Err(_) => {
                terminate(&mut child);
                return Err(transport_failure());
            }
        };
        Ok(CurlOutput {
            stdout: bytes,
            success: status.success(),
        })
    }
}

fn terminate(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn read_bounded(reader: &mut impl Read, limit: usize) -> io::Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    reader.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    Ok((bytes.len() <= limit).then_some(bytes))
}

fn parse_curl_output(stdout: &[u8]) -> CliResult<Response> {
    let marker = STATUS_MARKER.as_bytes();
    let Some(index) = stdout
        .windows(marker.len())
        .rposition(|window| window == marker)
    else {
        return Err(transport_failure());
    };
    let body = &stdout[..index];
    let status_text = &stdout[index + marker.len()..];
    if status_text.len() != 3 || !status_text.iter().all(|byte| byte.is_ascii_digit()) {
        return Err(transport_failure());
    }
    let status = u16::from(status_text[0] - b'0') * 100
        + u16::from(status_text[1] - b'0') * 10
        + u16::from(status_text[2] - b'0');
    if !(100..=599).contains(&status) {
        return Err(transport_failure());
    }
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(transport_failure());
    }
    Ok(Response {
        status,
        headers: Vec::new(),
        body: body.to_vec(),
    })
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum LibraryRoute {
    Search,
    Holdings,
    Ebook,
}

fn library_route(url: &str) -> Option<LibraryRoute> {
    if url.len() > MAX_URL_BYTES || !url.is_ascii() {
        return None;
    }
    if !url.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
        return None;
    }
    let path = url.strip_prefix(OPAC_PREFIX)?;
    if path == SEARCH_PATH {
        return Some(LibraryRoute::Search);
    }
    if path == EBOOK_PATH {
        return Some(LibraryRoute::Ebook);
    }
    let segment = path
        .strip_prefix(HOLDINGS_PREFIX)?
        .strip_suffix(HOLDINGS_SUFFIX)?;
    valid_path_segment(segment).then_some(LibraryRoute::Holdings)
}

fn valid_path_segment(segment: &str) -> bool {
    if segment.is_empty() || segment.len() > 256 {
        return false;
    }
    let bytes = segment.as_bytes();
    let mut decoded = Vec::with_capacity(segment.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            decoded.push(byte);
            index += 1;
        } else if byte == b'%'
            && index + 2 < bytes.len()
            && bytes[index + 1].is_ascii_hexdigit()
            && bytes[index + 2].is_ascii_hexdigit()
        {
            let Ok(value) = u8::from_str_radix(&segment[index + 1..index + 3], 16) else {
                return false;
            };
            // Encoded ASCII bytes could hide a path separator or a delimiter
            // from the route check. Only non-ASCII UTF-8 bytes are accepted in
            // a holdings id, and the decoded segment must be valid UTF-8.
            if value < 0x80 {
                return false;
            }
            decoded.push(value);
            index += 3;
        } else {
            return false;
        }
    }
    std::str::from_utf8(&decoded).is_ok()
}

fn allowed_referer(value: &str) -> bool {
    value == OPAC_REFERER || matches!(library_route(value), Some(LibraryRoute::Holdings))
}

fn valid_headers(headers: &[(String, String)]) -> bool {
    let mut seen = [false; 4];
    for (name, value) in headers {
        if name.is_empty() || name.len() > 64 || value.len() > MAX_HEADER_BYTES {
            return false;
        }
        if !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return false;
        }
        if !value.bytes().all(|byte| (0x20..=0x7e).contains(&byte)) {
            return false;
        }
        let index = match name.to_ascii_lowercase().as_str() {
            "user-agent" => 0,
            "referer" => 1,
            "x-requested-with" => 2,
            "content-type" => 3,
            _ => return false,
        };
        if seen[index] {
            return false;
        }
        seen[index] = true;
        match index {
            1 if !allowed_referer(value) => return false,
            2 if value != "XMLHttpRequest" && !value.is_empty() => return false,
            3 if value != "application/x-www-form-urlencoded; charset=UTF-8" => return false,
            _ => {}
        }
    }
    true
}

fn validate_request(request: &Request) -> CliResult<()> {
    let Some(route) = library_route(&request.url) else {
        return Err(request_rejected());
    };
    match (route, request.method) {
        (LibraryRoute::Holdings, "GET") if request.body.is_empty() => {}
        (LibraryRoute::Search | LibraryRoute::Ebook, "POST") => {}
        _ => return Err(request_rejected()),
    }
    if request.body.len() > MAX_REQUEST_BODY_BYTES || !valid_headers(&request.headers) {
        return Err(request_rejected());
    }
    Ok(())
}

fn base_curl_arguments(timeout: Duration) -> Vec<OsString> {
    let timeout = timeout
        .min(Duration::from_secs(MAX_TIMEOUT_SECONDS))
        .max(Duration::from_millis(1));
    let connect_timeout = timeout.min(Duration::from_secs(CONNECT_TIMEOUT_SECONDS));
    vec![
        "--disable".into(),
        "--silent".into(),
        "--show-error".into(),
        "--globoff".into(),
        "--proto".into(),
        "=https".into(),
        "--proxy".into(),
        String::new().into(),
        "--max-time".into(),
        curl_seconds(timeout).into(),
        "--connect-timeout".into(),
        curl_seconds(connect_timeout).into(),
        "--max-filesize".into(),
        MAX_RESPONSE_BYTES.to_string().into(),
        "--max-redirs".into(),
        "0".into(),
    ]
}

fn curl_arguments(request: &Request, timeout: Duration) -> CliResult<Vec<OsString>> {
    validate_request(request)?;
    if timeout.is_zero() {
        return Err(request_rejected());
    }
    let mut args = base_curl_arguments(timeout);
    args.extend([
        "--request".into(),
        request.method.into(),
        "--url".into(),
        request.url.clone().into(),
    ]);
    for (name, value) in &request.headers {
        args.push("--header".into());
        args.push(format!("{name}: {value}").into());
    }
    if request.method == "POST" {
        args.push("--data-binary".into());
        args.push("@-".into());
    }
    args.push("--write-out".into());
    // An ASCII marker is a single safe argv value on macOS and Windows. The
    // body may contain the same bytes; only the final marker is the status.
    args.push(format!("{STATUS_MARKER}%{{http_code}}").into());
    Ok(args)
}

fn curl_seconds(duration: Duration) -> String {
    format!("{:.3}", duration.as_secs_f64().max(0.001))
}

fn system_curl_program() -> PathBuf {
    #[cfg(target_os = "macos")]
    let program = PathBuf::from("/usr/bin/curl");
    #[cfg(target_os = "windows")]
    // Windows 10+ ships curl.exe in System32; fall back to PATH only when the
    // standard system directory cannot be derived.
    let program = std::env::var_os("SystemRoot")
        .or_else(|| std::env::var_os("SYSTEMROOT"))
        .map(PathBuf::from)
        .map(|root| root.join("System32").join("curl.exe"))
        .unwrap_or_else(|| PathBuf::from("curl.exe"));
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let program = PathBuf::from("curl");
    program
}

fn request_rejected() -> CliError {
    CliError::new(5, "upstream", "public library request rejected", "")
}

fn transport_failure() -> CliError {
    CliError::new(
        4,
        "network",
        "public library request failed",
        "check network connectivity to the public library and retry",
    )
}

mod cas_wire;
pub(crate) mod electricity_scope;
pub(crate) mod portal_scope;

/// Public release metadata and, for installs only, fixed release archives.
/// Redirects are returned to the lifecycle handler, never followed by curl.
pub(crate) struct ReleaseCurl {
    timeout: Duration,
    allow_assets: bool,
}

impl ReleaseCurl {
    pub(crate) fn new(timeout: Duration, allow_assets: bool) -> Self {
        Self {
            timeout,
            allow_assets,
        }
    }
}

const MAX_RELEASE_DOWNLOAD: usize = 64 * 1024 * 1024;

impl Transport for ReleaseCurl {
    fn send(&mut self, request: Request) -> CliResult<Response> {
        let mut exec = SystemCurl {
            program: system_curl_program(),
        };
        let network = || {
            CliError::new(
                4,
                "network",
                "GitHub release request failed",
                "Check the connection and retry `bupt update --check`.",
            )
        };
        let output = match release_metadata_arguments(&request, self.timeout) {
            Ok(args) => exec.exec(&args, &[]),
            Err(_) if self.allow_assets => {
                let config = release_asset_config(&request, self.timeout)?;
                let mut args = base_curl_arguments(self.timeout);
                let cap = args
                    .iter()
                    .position(|arg| arg == "--max-filesize")
                    .expect("curl size limit is configured");
                args[cap + 1] = MAX_RELEASE_DOWNLOAD.to_string().into();
                args.extend(["--config".into(), "-".into()]);
                exec.exec_bounded(&args, &config, MAX_RELEASE_DOWNLOAD + 64 * 1024)
            }
            Err(error) => return Err(error),
        }
        .map_err(|_| network())?;
        if !output.success {
            return Err(network());
        }
        cas_wire::parse_response(&output.stdout)
            .map_err(|_| CliError::new(5, "upstream", "GitHub returned malformed HTTP headers", ""))
    }
}

fn release_metadata_arguments(request: &Request, timeout: Duration) -> CliResult<Vec<OsString>> {
    let rejected = || {
        CliError::new(
            5,
            "upstream",
            "request is outside the release metadata policy",
            "",
        )
    };
    let valid_segment = |part: &str| {
        !part.is_empty()
            && !matches!(part, "." | "..")
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    };
    let parts: Vec<_> = request
        .url
        .strip_prefix("https://github.com/")
        .ok_or_else(rejected)?
        .split('/')
        .collect();
    if request.url.len() > MAX_URL_BYTES
        || request.method != "GET"
        || !request.body.is_empty()
        || timeout.is_zero()
        || !matches!(parts.as_slice(), [owner, repo, "releases", "latest"] if valid_segment(owner) && valid_segment(repo))
        || !matches!(request.headers.as_slice(), [(name, value)] if name.eq_ignore_ascii_case("user-agent") && value == "bupt-cli")
    {
        return Err(rejected());
    }
    let mut args = base_curl_arguments(timeout);
    args.extend([
        "--include".into(),
        "--request".into(),
        "GET".into(),
        "--user-agent".into(),
        "bupt-cli".into(),
        "--url".into(),
        request.url.clone().into(),
    ]);
    Ok(args)
}

fn release_asset_config(request: &Request, timeout: Duration) -> CliResult<Vec<u8>> {
    let error = || {
        CliError::new(
            5,
            "upstream",
            "request is outside the release asset policy",
            "",
        )
    };
    let raw = request.url.as_str();
    if raw.len() > 8192
        || raw.contains(['\\', '#'])
        || !raw.bytes().all(|byte| matches!(byte, 0x21..=0x7e))
        || timeout.is_zero()
        || request.method != "GET"
        || !request.body.is_empty()
        || !matches!(request.headers.as_slice(), [(name, value)] if name.eq_ignore_ascii_case("user-agent") && value == "bupt-cli")
    {
        return Err(error());
    }
    let url = url::Url::parse(raw).map_err(|_| error())?;
    let host = url.host_str().ok_or_else(error)?;
    if raw
        .strip_prefix("https://")
        .and_then(|rest| rest.split('/').next())
        != Some(host)
        || url.as_str() != raw
        || url.path().contains("//")
        || url.path().to_ascii_lowercase().contains("%2f")
        || url.path().to_ascii_lowercase().contains("%5c")
    {
        return Err(error());
    }
    let valid = match host {
        "github.com" => {
            let parts: Vec<_> = url.path().split('/').skip(1).collect();
            let segment = |value: &str| {
                !value.is_empty()
                    && !matches!(value, "." | "..")
                    && value.bytes().all(|b| {
                        b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'+')
                    })
            };
            url.query().is_none()
                && matches!(parts.as_slice(), [owner, repo, "releases", "download", tag, asset]
                if [*owner, *repo, *tag].into_iter().all(segment)
                    && matches!(*asset, "bupt-macos-arm64.zip" | "bupt-windows-x64.zip"))
        }
        "release-assets.githubusercontent.com"
        | "objects.githubusercontent.com"
        | "github-releases.githubusercontent.com" => url.path() != "/",
        _ => false,
    };
    if !valid {
        return Err(error());
    }
    let mut config = String::from("include\n");
    cas_wire::option(&mut config, "url", raw);
    cas_wire::option(&mut config, "request", "GET");
    cas_wire::option(&mut config, "user-agent", "bupt-cli");
    Ok(config.into_bytes())
}

/// Separate from public-library transport: CAS cookies and UCloud credentials
/// are supplied to the trusted executable only through stdin.
pub(crate) struct CampusCurl {
    timeout: Duration,
    diagnostic: bool,
}

impl CampusCurl {
    pub(crate) fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            diagnostic: false,
        }
    }

    pub(crate) fn for_diagnose(timeout: Duration) -> Self {
        Self {
            timeout,
            diagnostic: true,
        }
    }
}

impl Transport for CampusCurl {
    fn send(&mut self, request: Request) -> CliResult<Response> {
        // Do not hand a private request to the PATH fallback. A trusted
        // Windows system-curl path and its execution remain to be verified.
        if !cfg!(target_os = "macos") {
            return Err(CliError::new(
                5,
                "upstream",
                "authenticated HTTPS is not yet enabled on this platform",
                "",
            ));
        }
        let mut exec = SystemCurl {
            program: PathBuf::from("/usr/bin/curl"),
        };
        if self.diagnostic {
            cas_wire::exchange_probe(&mut exec, self.timeout, &request)
        } else {
            cas_wire::exchange(&mut exec, self.timeout, &request)
        }
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::collections::VecDeque;

    /// Records every attempt before matching a response. No real fallback.
    #[derive(Default)]
    pub(crate) struct Scripted {
        pub expected: VecDeque<(&'static str, String, CliResult<Response>)>,
        pub calls: Vec<Request>,
    }

    impl Transport for Scripted {
        fn send(&mut self, request: Request) -> CliResult<Response> {
            self.calls.push(request);
            let Some((method, url, response)) = self.expected.pop_front() else {
                return Err(unexpected());
            };
            let request = self.calls.last().expect("recorded request");
            if request.method != method || request.url != url {
                return Err(unexpected());
            }
            response
        }
    }

    fn unexpected() -> CliError {
        CliError::new(5, "upstream", "unexpected request in offline test", "")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::io::Cursor;
    use std::rc::Rc;

    #[test]
    fn release_assets_allow_only_the_named_platform_archives_and_trusted_https_hosts() {
        let timeout = Duration::from_secs(3);
        let mut request = Request {
            method: "GET",
            url: "https://github.com/owner/project/releases/download/v1.0.0/bupt-macos-arm64.zip"
                .into(),
            headers: vec![("User-Agent".into(), "bupt-cli".into())],
            body: vec![],
        };
        assert!(release_metadata_arguments(&request, timeout).is_err());
        assert!(release_asset_config(&request, timeout).is_ok());
        request.url = "https://release-assets.githubusercontent.com/example?signature=TEST".into();
        let config = String::from_utf8(release_asset_config(&request, timeout).unwrap()).unwrap();
        assert!(config.contains("signature=TEST"));
        assert!(!config.contains("location"));
        for invalid in [
            "https://github.com/owner/project/releases/download/v1.0.0/source.zip",
            "https://release-assets.githubusercontent.com@outside.example/example",
            "https://github.com/owner/project/releases/download/v1.0.0/%2Fbupt-macos-arm64.zip",
            "http://release-assets.githubusercontent.com/example",
        ] {
            request.url = invalid.into();
            assert!(release_asset_config(&request, timeout).is_err());
        }
        request.url = "https://release-assets.githubusercontent.com/example".into();
        request
            .headers
            .push(("Authorization".into(), "TEST-ONLY".into()));
        assert!(release_asset_config(&request, timeout).is_err());
    }

    #[test]
    fn release_metadata_requests_are_read_only_and_never_follow_redirects() {
        let mut request = Request {
            method: "GET",
            url: "https://github.com/owner/project/releases/latest".into(),
            headers: vec![("User-Agent".into(), "bupt-cli".into())],
            body: vec![],
        };
        let args = release_metadata_arguments(&request, Duration::from_secs(3)).unwrap();
        assert_eq!(args.first().unwrap(), "--disable");
        assert!(args.iter().any(|arg| arg == "--include"));
        assert!(!args.iter().any(|arg| arg == "--location"));
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "--max-redirs" && pair[1] == "0")
        );
        for invalid in [
            "http://github.com/owner/project/releases/latest",
            "https://github.com@outside.example/owner/project/releases/latest",
            "https://github.com/../project/releases/latest",
            "https://github.com/owner/project/releases/latest?foo=1",
            "https://github.com/owner/project/releases/download/v1/bupt.zip",
        ] {
            request.url = invalid.into();
            assert!(release_metadata_arguments(&request, Duration::from_secs(3)).is_err());
        }
        request.url = "https://github.com/owner/project/releases/latest".into();
        request
            .headers
            .push(("Cookie".into(), "TEST-ONLY-COOKIE".into()));
        assert!(release_metadata_arguments(&request, Duration::from_secs(3)).is_err());
    }

    #[derive(Default)]
    struct Journal {
        calls: Vec<(Vec<OsString>, Vec<u8>)>,
    }

    struct FakeCurl {
        journal: Rc<RefCell<Journal>>,
        outputs: VecDeque<CliResult<CurlOutput>>,
    }

    impl CurlExec for FakeCurl {
        fn exec(&mut self, args: &[OsString], body: &[u8]) -> CliResult<CurlOutput> {
            self.journal
                .borrow_mut()
                .calls
                .push((args.to_vec(), body.to_vec()));
            self.outputs
                .pop_front()
                .unwrap_or_else(|| Err(transport_failure()))
        }
    }

    fn fake_curl(outputs: Vec<CliResult<CurlOutput>>) -> (Curl, Rc<RefCell<Journal>>) {
        let journal = Rc::new(RefCell::new(Journal::default()));
        let exec = FakeCurl {
            journal: Rc::clone(&journal),
            outputs: outputs.into(),
        };
        (
            Curl {
                timeout: Duration::from_secs(20),
                exec: Box::new(exec),
            },
            journal,
        )
    }

    fn search_request() -> Request {
        Request {
            method: "POST",
            url: format!("{OPAC_PREFIX}{SEARCH_PATH}"),
            headers: vec![
                ("User-Agent".into(), "Mozilla/5.0 synthetic".into()),
                ("Referer".into(), OPAC_REFERER.into()),
                ("X-Requested-With".into(), "XMLHttpRequest".into()),
                (
                    "Content-Type".into(),
                    "application/x-www-form-urlencoded; charset=UTF-8".into(),
                ),
            ],
            body: b"keyword=synthetic&pageNo=1".to_vec(),
        }
    }

    fn holdings_request() -> Request {
        Request {
            method: "GET",
            url: format!("{OPAC_PREFIX}{HOLDINGS_PREFIX}fixture-1{HOLDINGS_SUFFIX}"),
            headers: vec![
                ("User-Agent".into(), "Mozilla/5.0 synthetic".into()),
                ("Referer".into(), OPAC_REFERER.into()),
                ("X-Requested-With".into(), String::new()),
                (
                    "Content-Type".into(),
                    "application/x-www-form-urlencoded; charset=UTF-8".into(),
                ),
            ],
            body: Vec::new(),
        }
    }

    fn ebook_request() -> Request {
        Request {
            method: "POST",
            url: format!("{OPAC_PREFIX}{EBOOK_PATH}"),
            headers: vec![
                ("User-Agent".into(), "Mozilla/5.0 synthetic".into()),
                (
                    "Referer".into(),
                    format!("{OPAC_PREFIX}{HOLDINGS_PREFIX}fixture-1{HOLDINGS_SUFFIX}"),
                ),
                ("X-Requested-With".into(), "XMLHttpRequest".into()),
                (
                    "Content-Type".into(),
                    "application/x-www-form-urlencoded; charset=UTF-8".into(),
                ),
            ],
            body: b"sid=fixture-1".to_vec(),
        }
    }

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    fn assert_pair(args: &[String], flag: &str, value: &str) {
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == flag && pair[1] == value),
            "missing {flag} {value:?} in {args:?}"
        );
    }

    fn send_error(curl: &mut Curl, request: Request) -> CliError {
        match curl.send(request) {
            Ok(_) => panic!("expected an error"),
            Err(error) => error,
        }
    }

    #[test]
    fn builds_a_bounded_argument_vector_for_library_search() {
        let args = curl_arguments(&search_request(), Duration::from_secs(20)).unwrap();
        let text = strings(&args);
        assert_eq!(text[0], "--disable");
        assert_pair(&text, "--proto", "=https");
        assert_pair(&text, "--proxy", "");
        assert_pair(&text, "--max-time", "20.000");
        assert_pair(&text, "--connect-timeout", "10.000");
        assert_pair(&text, "--max-filesize", &MAX_RESPONSE_BYTES.to_string());
        assert_pair(&text, "--max-redirs", "0");
        assert_pair(&text, "--request", "POST");
        assert_pair(&text, "--url", &format!("{OPAC_PREFIX}{SEARCH_PATH}"));
        assert_pair(&text, "--data-binary", "@-");
        assert_pair(
            &text,
            "--write-out",
            &format!("{STATUS_MARKER}%{{http_code}}"),
        );

        let header_values: Vec<&str> = text
            .iter()
            .enumerate()
            .filter_map(|(index, value)| {
                (index > 0 && text[index - 1] == "--header").then_some(value.as_str())
            })
            .collect();
        assert_eq!(
            header_values,
            [
                "User-Agent: Mozilla/5.0 synthetic",
                "Referer: https://opac.bupt.edu.cn/search-full-result.html",
                "X-Requested-With: XMLHttpRequest",
                "Content-Type: application/x-www-form-urlencoded; charset=UTF-8",
            ]
        );
        for forbidden in [
            "--location",
            "--retry",
            "--user",
            "--cookie",
            "--netrc",
            "--config",
        ] {
            assert!(
                !text.iter().any(|arg| arg == forbidden),
                "found {forbidden}"
            );
        }
        assert!(!text.iter().any(|arg| arg.contains("keyword=synthetic")));
    }

    #[test]
    fn caps_the_request_timeout_and_connect_timeout() {
        let args = curl_arguments(&search_request(), Duration::from_secs(500)).unwrap();
        let text = strings(&args);
        assert_pair(&text, "--max-time", "120.000");
        assert_pair(&text, "--connect-timeout", "10.000");

        let args = curl_arguments(&search_request(), Duration::from_millis(500)).unwrap();
        let text = strings(&args);
        assert_pair(&text, "--max-time", "0.500");
        assert_pair(&text, "--connect-timeout", "0.500");
        assert!(curl_arguments(&search_request(), Duration::ZERO).is_err());
    }

    #[test]
    fn accepts_only_documented_methods_for_exact_library_routes() {
        for request in [search_request(), holdings_request(), ebook_request()] {
            assert!(
                curl_arguments(&request, Duration::from_secs(20)).is_ok(),
                "rejected {} {}",
                request.method,
                request.url
            );
        }

        let mut get_search = search_request();
        get_search.method = "GET";
        get_search.body.clear();
        assert!(curl_arguments(&get_search, Duration::from_secs(20)).is_err());

        let mut post_holdings = holdings_request();
        post_holdings.method = "POST";
        post_holdings.body = b"x".to_vec();
        assert!(curl_arguments(&post_holdings, Duration::from_secs(20)).is_err());

        let mut get_with_body = holdings_request();
        get_with_body.body = b"x".to_vec();
        assert!(curl_arguments(&get_with_body, Duration::from_secs(20)).is_err());

        let mut delete = search_request();
        delete.method = "DELETE";
        assert!(curl_arguments(&delete, Duration::from_secs(20)).is_err());

        let mut oversized_body = search_request();
        oversized_body.body = vec![b'x'; MAX_REQUEST_BODY_BYTES + 1];
        assert!(curl_arguments(&oversized_body, Duration::from_secs(20)).is_err());
    }

    #[test]
    fn rejects_urls_outside_the_exact_library_routes() {
        let invalid = [
            "http://opac.bupt.edu.cn/search-full-result.json",
            "https://opac.bupt.edu.cn.evil.test/search-full-result.json",
            "https://opac.bupt.edu.cn:443/search-full-result.json",
            "https://user@opac.bupt.edu.cn/search-full-result.json",
            "https://opac.bupt.edu.cn/search-full-result.json?x=1",
            "https://opac.bupt.edu.cn/search-full-result.json#fragment",
            "https://opac.bupt.edu.cn/other.json",
            "https://opac.bupt.edu.cn/bookInfo_.html",
            "https://opac.bupt.edu.cn/bookInfo_a/b.html",
            "https://opac.bupt.edu.cn/bookInfo_a%2Fb.html",
            "https://opac.bupt.edu.cn/bookInfo_a%zz.html",
            "https://opac.bupt.edu.cn/bookInfo_a%C0%AFb.html",
            "https://opac.bupt.edu.cn/bookInfo_a b.html",
            "ftp://opac.bupt.edu.cn/search-full-result.json",
        ];
        for url in invalid {
            let mut request = search_request();
            request.url = url.into();
            assert!(
                curl_arguments(&request, Duration::from_secs(20)).is_err(),
                "accepted {url}"
            );
        }

        let mut encoded = holdings_request();
        encoded.url =
            format!("{OPAC_PREFIX}{HOLDINGS_PREFIX}%E8%AE%B0%E5%BD%95%E4%B8%80{HOLDINGS_SUFFIX}");
        assert!(curl_arguments(&encoded, Duration::from_secs(20)).is_ok());
    }

    #[test]
    fn rejects_credentials_unknown_headers_and_header_injection() {
        for (name, value) in [
            ("Cookie", "session=synthetic"),
            ("Authorization", "Bearer synthetic"),
            ("Proxy-Authorization", "Basic synthetic"),
            ("Accept", "*/*"),
            ("X-Api-Key", "synthetic"),
        ] {
            let mut request = search_request();
            request.headers.push((name.into(), value.into()));
            assert!(
                curl_arguments(&request, Duration::from_secs(20)).is_err(),
                "accepted {name}"
            );
        }

        let mut duplicate = search_request();
        duplicate
            .headers
            .push(("user-agent".into(), "Mozilla/5.0 second".into()));
        assert!(curl_arguments(&duplicate, Duration::from_secs(20)).is_err());

        let mut evil_referer = search_request();
        evil_referer.headers[1].1 = "https://evil.invalid/search-full-result.html".into();
        assert!(curl_arguments(&evil_referer, Duration::from_secs(20)).is_err());

        let mut fetch = search_request();
        fetch.headers[2].1 = "fetch".into();
        assert!(curl_arguments(&fetch, Duration::from_secs(20)).is_err());

        let mut wrong_content_type = search_request();
        wrong_content_type.headers[3].1 = "application/json".into();
        assert!(curl_arguments(&wrong_content_type, Duration::from_secs(20)).is_err());

        let mut injection = search_request();
        injection.headers[0].1 = "synthetic\r\nCookie: synthetic".into();
        assert!(curl_arguments(&injection, Duration::from_secs(20)).is_err());

        let mut lowercase = search_request();
        for (name, _) in &mut lowercase.headers {
            *name = name.to_ascii_lowercase();
        }
        assert!(curl_arguments(&lowercase, Duration::from_secs(20)).is_ok());
    }

    #[test]
    fn parses_the_status_from_the_final_write_out_marker() {
        let response = parse_curl_output(b"hello__BUPT_HTTP_STATUS__200").unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body.as_slice(), b"hello");
        assert!(response.headers.is_empty());

        let response = parse_curl_output(b"__BUPT_HTTP_STATUS__404").unwrap();
        assert_eq!(response.status, 404);
        assert!(response.body.is_empty());

        let response = parse_curl_output(b"line\n200__BUPT_HTTP_STATUS__201").unwrap();
        assert_eq!(response.status, 201);
        assert_eq!(response.body.as_slice(), b"line\n200");

        let response =
            parse_curl_output(b"payload__BUPT_HTTP_STATUS__999__BUPT_HTTP_STATUS__200").unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body.as_slice(), b"payload__BUPT_HTTP_STATUS__999");

        let malformed: &[&[u8]] = &[
            b"no status",
            b"body__BUPT_HTTP_STATUS__20",
            b"body__BUPT_HTTP_STATUS__abc",
            b"body__BUPT_HTTP_STATUS__999",
            b"body__BUPT_HTTP_STATUS__2000",
        ];
        for stdout in malformed {
            assert!(parse_curl_output(stdout).is_err(), "{stdout:?}");
        }

        let mut oversized = vec![b'x'; MAX_RESPONSE_BYTES + 1];
        oversized.extend_from_slice(b"__BUPT_HTTP_STATUS__200");
        assert!(parse_curl_output(&oversized).is_err());

        let mut maximum = vec![b'x'; MAX_RESPONSE_BYTES];
        maximum.extend_from_slice(b"__BUPT_HTTP_STATUS__200");
        assert_eq!(
            parse_curl_output(&maximum).unwrap().body.len(),
            MAX_RESPONSE_BYTES
        );
    }

    #[test]
    fn reads_at_most_the_bounded_number_of_response_bytes() {
        let mut exact = Cursor::new(vec![7; 10]);
        assert_eq!(
            read_bounded(&mut exact, 10).unwrap().unwrap().len(),
            10,
            "the limit is inclusive"
        );
        let mut over = Cursor::new(vec![7; 11]);
        assert!(read_bounded(&mut over, 10).unwrap().is_none());
    }

    #[test]
    fn send_preserves_the_http_status_and_calls_curl_once() {
        let (mut curl, journal) = fake_curl(vec![
            Ok(CurlOutput {
                stdout: b"upstream error__BUPT_HTTP_STATUS__503".to_vec(),
                success: true,
            }),
            Ok(CurlOutput {
                stdout: b"unexpected second response__BUPT_HTTP_STATUS__200".to_vec(),
                success: true,
            }),
        ]);
        let response = curl.send(search_request()).unwrap();
        assert_eq!(response.status, 503);
        assert_eq!(journal.borrow().calls.len(), 1);
    }

    #[test]
    fn send_does_not_retry_a_transport_failure() {
        let (mut curl, journal) = fake_curl(vec![
            Err(transport_failure()),
            Ok(CurlOutput {
                stdout: b"second response__BUPT_HTTP_STATUS__200".to_vec(),
                success: true,
            }),
        ]);
        let error = send_error(&mut curl, search_request());
        assert_eq!(error.code, 4);
        assert_eq!(error.kind, "network");
        assert_eq!(journal.borrow().calls.len(), 1);
    }

    #[test]
    fn nonzero_curl_exit_is_a_transport_failure_even_with_a_status_line() {
        let (mut curl, _) = fake_curl(vec![Ok(CurlOutput {
            stdout: b"partial response__BUPT_HTTP_STATUS__200".to_vec(),
            success: false,
        })]);
        let error = send_error(&mut curl, search_request());
        assert_eq!(error.code, 4);
        assert_eq!(error.kind, "network");
        assert!(!error.message.contains("partial response"));
    }

    #[test]
    fn invalid_requests_do_not_reach_the_curl_process() {
        let (mut curl, journal) = fake_curl(Vec::new());
        let mut request = search_request();
        request.url = "https://evil.invalid/search-full-result.json".into();
        let error = send_error(&mut curl, request);
        assert_eq!(error.code, 5);
        assert_eq!(error.kind, "upstream");
        assert!(journal.borrow().calls.is_empty());
    }

    #[test]
    fn malformed_or_oversized_output_is_a_transport_failure() {
        let (mut curl, _) = fake_curl(vec![Ok(CurlOutput {
            stdout: b"no status".to_vec(),
            success: true,
        })]);
        assert_eq!(send_error(&mut curl, search_request()).code, 4);

        let mut oversized = vec![b'x'; MAX_RESPONSE_BYTES + 1];
        oversized.extend_from_slice(b"__BUPT_HTTP_STATUS__200");
        let (mut curl, _) = fake_curl(vec![Ok(CurlOutput {
            stdout: oversized,
            success: true,
        })]);
        let error = send_error(&mut curl, search_request());
        assert_eq!(error.code, 4);
        assert!(!error.message.contains(&MAX_RESPONSE_BYTES.to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn system_curl_receives_separate_arguments_and_the_body_on_stdin() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("create fake curl directory");
        let program = directory.path().join("fake-curl");
        std::fs::write(
            &program,
            "#!/bin/sh\nprintf 'argc=%s\\0' \"$#\"\nfor argument in \"$@\"; do printf '%s\\0' \"$argument\"; done\nprintf 'BODY\\0'\ncat\nprintf '__BUPT_HTTP_STATUS__200'\n",
        )
        .expect("write fake curl");
        let mut permissions = std::fs::metadata(&program)
            .expect("stat fake curl")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&program, permissions).expect("make fake curl executable");

        let mut request = search_request();
        request.body = b"keyword=x; echo injected".to_vec();
        request.headers[0].1 = "Mozilla/5.0; echo injected".into();
        let mut curl = Curl {
            timeout: Duration::from_secs(5),
            exec: Box::new(SystemCurl { program }),
        };
        let response = curl.send(request).unwrap();
        assert_eq!(response.status, 200);

        let text = String::from_utf8(response.body).expect("fake curl emitted text");
        let mut fields = text.split('\0');
        let argc = fields
            .next()
            .and_then(|field| field.strip_prefix("argc="))
            .and_then(|value| value.parse::<usize>().ok())
            .expect("fake curl reported argc");
        let arguments: Vec<&str> = fields.by_ref().take(argc).collect();
        assert_eq!(arguments.first().copied(), Some("--disable"));
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair == ["--url", "https://opac.bupt.edu.cn/search-full-result.json"])
        );
        assert!(
            arguments.contains(&"User-Agent: Mozilla/5.0; echo injected"),
            "header value was not a single argument: {arguments:?}"
        );
        assert_eq!(fields.next(), Some("BODY"));
        assert_eq!(
            fields.collect::<Vec<_>>().join("\0"),
            "keyword=x; echo injected"
        );
        assert!(!arguments.iter().any(|arg| arg.contains("keyword=x")));
    }
}
