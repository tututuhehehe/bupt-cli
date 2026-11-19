//! Isolated Chrome/CDP page source for the graduate system.
//!
//! Only the graduate path uses a browser: yjxt sits behind a script challenge
//! that plain HTTP cannot satisfy. The browser is launched with a fresh
//! temporary profile, navigates only known school origins, and is killed on
//! drop. No CAPTCHA challenge is solved or bypassed here.
//!
//! Authentication reuses the CAS cookies that the identity lane already
//! persisted in `session.json`; this module never uses or logs the password
//! value.
//! The small `IdentitySessionAuth` type is the scoped session interface. It can
//! be replaced by an identity-owned accessor without changing page parsing or
//! command wiring.

use super::{GradPage, GradPageSource, GradPages};
use crate::cli::{CliError, CliResult};
use crate::identity::{self, Role};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tempfile::{Builder as TempBuilder, TempDir};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const CHROME: &str = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const CHROME_CANDIDATES: [&str; 3] = [
    CHROME,
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
];
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
    AppleWebKit/537.36 (KHTML, like Gecko) Chrome/157.0.0.0 Safari/537.36";
const LOGIN_URL: &str =
    "https://auth.bupt.edu.cn/authserver/login?service=http%3a%2f%2fyjxt.bupt.edu.cn%2fULogin.aspx";
const YJXT_BASE: &str = "https://yjxt.bupt.edu.cn";
const MENU_URL: &str = "https://yjxt.bupt.edu.cn/Gstudent/LeftMenu.aspx?menu=00";
const SESSION_FILENAME: &str = "session.json";
const STALE_PROFILE_SECONDS: u64 = 6 * 3600;
const WEBSOCKET_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

const CHROME_MISSING_HINT: &str = "研究生系统需要真实浏览器（瑞数防护无法用纯 HTTP 绕过）。请安装 Google Chrome：https://www.google.com/chrome/";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::academics) struct SessionCookie {
    name: String,
    value: String,
    domain: String,
    path: String,
    secure: bool,
    http_only: bool,
    expires: Option<i64>,
    same_site: Option<String>,
}

/// Narrow session interface: only CAS cookies for the saved account and role.
pub(in crate::academics) struct IdentitySessionAuth {
    config_dir: PathBuf,
    cache_dir: PathBuf,
}

impl IdentitySessionAuth {
    pub(super) fn new(config_dir: &Path, cache_dir: &Path) -> Self {
        Self {
            config_dir: config_dir.to_path_buf(),
            cache_dir: cache_dir.to_path_buf(),
        }
    }

    pub(super) fn cookies(
        &self,
        expected_username: &str,
        expected_role: Role,
    ) -> CliResult<Vec<SessionCookie>> {
        let account = identity::require_authenticated_profile(&self.config_dir)?;
        if account.username() != expected_username || account.role() != Some(expected_role) {
            return Err(auth_error(
                "保存的会话与当前学号或身份不一致",
                "重新执行 `bupt login --role graduate` 建立当前账号的会话。",
            ));
        }
        let path = self.cache_dir.join(SESSION_FILENAME);
        if let Ok(metadata) = fs::symlink_metadata(&path)
            && (!metadata.file_type().is_file() || metadata.file_type().is_symlink())
        {
            return Err(auth_error(
                "本地 CAS 会话不是普通文件，拒绝读取",
                "重新执行 `bupt login --role graduate`。",
            ));
        }
        let bytes = fs::read(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                auth_error(
                    "研究生系统需要先建立可用的 CAS 会话",
                    "运行 `bupt login --role graduate`，再重试本命令。",
                )
            } else {
                CliError::new(
                    3,
                    "auth",
                    format!("无法读取本地 CAS 会话：{error}"),
                    "重新执行 `bupt login --role graduate`。",
                )
            }
        })?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
            auth_error(
                "本地 CAS 会话不是可用的 JSON",
                "重新执行 `bupt login --role graduate`。",
            )
        })?;
        let object = value.as_object().ok_or_else(|| {
            auth_error(
                "本地 CAS 会话格式无法识别",
                "重新执行 `bupt login --role graduate`。",
            )
        })?;
        if object.get("version").and_then(Value::as_u64) != Some(2) {
            return Err(auth_error(
                "本地 CAS 会话版本不受支持",
                "重新执行 `bupt login --role graduate`。",
            ));
        }
        if object.get("username").and_then(Value::as_str) != Some(expected_username)
            || object
                .get("role")
                .and_then(Value::as_str)
                .and_then(|role| match role {
                    "graduate" => Some(Role::Graduate),
                    "undergraduate" => Some(Role::Undergraduate),
                    _ => None,
                })
                != Some(expected_role)
        {
            return Err(auth_error(
                "本地 CAS 会话不属于当前学号或身份",
                "重新执行 `bupt login --role graduate`。",
            ));
        }
        let cookies = object
            .get("cookies")
            .and_then(Value::as_array)
            .map(|cookies| {
                cookies
                    .iter()
                    .filter_map(session_cookie)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if cookies.is_empty() {
            return Err(auth_error(
                "本地 CAS 会话没有可用的认证 Cookie",
                "重新执行 `bupt login --role graduate`。",
            ));
        }
        Ok(cookies)
    }
}

fn auth_error(message: impl Into<String>, hint: impl Into<String>) -> CliError {
    CliError::new(3, "auth", message, hint)
}

fn session_cookie(value: &Value) -> Option<SessionCookie> {
    let object = value.as_object()?;
    let raw = object.get("raw_cookie")?.as_str()?.trim();
    let mut segments = raw.split(';');
    let first = segments.next()?.trim();
    let (name, value) = first.split_once('=')?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let value = unquote(value.trim()).to_string();
    let attributes = segments
        .map(|segment| {
            let segment = segment.trim();
            let (key, value) = segment.split_once('=').unwrap_or((segment, ""));
            (key.trim().to_ascii_lowercase(), value.trim().to_string())
        })
        .collect::<BTreeMap<_, _>>();
    let domain = object
        .get("domain")
        .and_then(Value::as_object)
        .and_then(|domain| {
            domain
                .get("HostOnly")
                .or_else(|| domain.get("Suffix"))
                .and_then(Value::as_str)
        })
        .or_else(|| attributes.get("domain").map(String::as_str))
        .map(str::trim)
        .and_then(|domain| {
            let domain = domain.trim_start_matches('.');
            (domain.eq_ignore_ascii_case("auth.bupt.edu.cn")
                || domain.eq_ignore_ascii_case("bupt.edu.cn"))
            .then_some("auth.bupt.edu.cn".to_string())
        })?;
    let path = object
        .get("path")
        .and_then(Value::as_array)
        .and_then(|path| path.first())
        .and_then(Value::as_str)
        .or_else(|| attributes.get("path").map(String::as_str))
        .filter(|path| path.starts_with('/'))
        .unwrap_or("/")
        .to_string();
    let expires = cookie_expiry(object.get("expires"))?;
    let secure = attributes.contains_key("secure");
    let http_only = attributes.contains_key("httponly");
    let same_site =
        attributes
            .get("samesite")
            .and_then(|value| match value.to_ascii_lowercase().as_str() {
                "strict" => Some("Strict".to_string()),
                "lax" => Some("Lax".to_string()),
                "none" => Some("None".to_string()),
                _ => None,
            });
    Some(SessionCookie {
        name: name.to_string(),
        value,
        domain,
        path,
        secure,
        http_only,
        expires,
        same_site,
    })
}

/// `None` means the cookie is absent, expired, or unparseable; expired cookies
/// must never be injected into a fresh browser session.
fn cookie_expiry(value: Option<&Value>) -> Option<Option<i64>> {
    match value {
        None | Some(Value::Null) => Some(None),
        Some(Value::String(value)) if value == "SessionEnd" => Some(None),
        Some(Value::Object(object)) => {
            let raw = object.get("AtUtc")?.as_str()?;
            let expires = OffsetDateTime::parse(raw, &Rfc3339).ok()?.unix_timestamp();
            (expires > OffsetDateTime::now_utc().unix_timestamp()).then_some(Some(expires))
        }
        _ => None,
    }
}

fn unquote(value: &str) -> &str {
    if value.len() >= 2 {
        let first = value.as_bytes()[0];
        let last = value.as_bytes()[value.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return &value[1..value.len() - 1];
        }
    }
    value
}

pub(in crate::academics) struct ChromePageSource {
    config_dir: PathBuf,
    cache_dir: PathBuf,
    timeout: Duration,
}

impl ChromePageSource {
    pub(in crate::academics) fn new(
        config_dir: &Path,
        cache_dir: &Path,
        timeout: Duration,
    ) -> Self {
        Self {
            config_dir: config_dir.to_path_buf(),
            cache_dir: cache_dir.to_path_buf(),
            timeout,
        }
    }
}

impl GradPageSource for ChromePageSource {
    fn collect(&mut self, wanted: &[GradPage]) -> CliResult<GradPages> {
        let account = identity::require_authenticated_profile(&self.config_dir)?;
        let role = account.role().ok_or_else(|| {
            CliError::new(
                2,
                "usage",
                "账号身份未登记或不受支持",
                "运行 `bupt login --role graduate` 重新登记身份。",
            )
        })?;
        if role != Role::Graduate {
            return Err(CliError::new(
                2,
                "usage",
                "研究生数据源只用于研究生账号",
                "本科生请使用 `bupt schedule` / `bupt scores` / `bupt classrooms`。",
            ));
        }
        let cookies = IdentitySessionAuth::new(&self.config_dir, &self.cache_dir)
            .cookies(account.username(), role)?;
        let mut browser = Browser::launch(&cookies, self.timeout)?;
        browser.login()?;
        let menu = browser.menu()?;
        let mut pages = GradPages::default();

        if wanted.contains(&GradPage::Schedule) || wanted.contains(&GradPage::Calendar) {
            let url = menu_url(&menu, "学期课表信息查询")?;
            let mut last_error = None;
            for _ in 0..3 {
                match browser.goto(&url) {
                    Ok(()) => {
                        if let Some(html) =
                            browser.wait_for_html(|html| html.contains("星期一"), 12)
                        {
                            pages.schedule = Some(html);
                            last_error = None;
                            break;
                        }
                    }
                    Err(error) => last_error = Some(error),
                }
            }
            if pages.schedule.is_none() {
                return Err(last_error.unwrap_or_else(|| {
                    CliError::new(
                        5,
                        "upstream",
                        "课表页没有渲染出表格",
                        "重试一次；若持续失败，页面结构可能已变。",
                    )
                }));
            }
        }

        if wanted.contains(&GradPage::Calendar) {
            let source = pages.schedule.clone().unwrap_or_default();
            let mut eid = extract_eid(&source);
            if eid.is_none() {
                let url = menu_url(&menu, "学期课表信息查询")?;
                browser.goto(&url)?;
                if let Some(html) =
                    browser.wait_for_html(|html| html.contains("TermCalender.aspx?EID="), 12)
                {
                    eid = extract_eid(&html);
                }
            }
            let eid = eid.ok_or_else(|| {
                CliError::new(
                    5,
                    "upstream",
                    "课表页里找不到教学日历入口",
                    "重试一次；页面结构可能已变。",
                )
            })?;
            let url = format!("{YJXT_BASE}/PublicPage/TermCalender.aspx?EID={eid}&UID=");
            browser.goto(&url)?;
            let calendar = browser
                .wait_for_html(|html| html.contains("dgData") && html.contains("二十"), 25)
                .ok_or_else(|| {
                    CliError::new(
                        5,
                        "upstream",
                        "教学日历页面没有渲染出完整周次",
                        "重试一次；不会把半渲染的日历当成完整学期。",
                    )
                })?;
            pages.calendar = Some(calendar);
        }

        if wanted.contains(&GradPage::Exams) {
            let url = menu_url(&menu, "学期考核信息查询")?;
            let mut last_error = None;
            for _ in 0..3 {
                match browser.goto(&url) {
                    Ok(()) => {
                        if let Some(html) =
                            browser.wait_for_html(|html| html.contains("考试课程"), 12)
                        {
                            pages.exams = Some(html);
                            last_error = None;
                            break;
                        }
                    }
                    Err(error) => last_error = Some(error),
                }
            }
            if pages.exams.is_none() {
                return Err(last_error.unwrap_or_else(|| {
                    CliError::new(
                        5,
                        "upstream",
                        "考试安排页没有渲染出来",
                        "重试一次；页面结构可能已变。",
                    )
                }));
            }
        }

        Ok(pages)
    }
}

fn menu_url(menu: &BTreeMap<String, String>, wanted: &str) -> CliResult<String> {
    let (_, url) = menu
        .iter()
        .find(|(name, _)| name.contains(wanted))
        .ok_or_else(|| {
            CliError::new(
                5,
                "upstream",
                format!("研究生菜单里找不到「{wanted}」"),
                "页面结构可能已变；重试一次并反馈菜单结构。",
            )
        })?;
    let url = url.trim();
    if url.is_empty()
        || url.starts_with("http:")
        || url.starts_with("https:")
        || url.starts_with("//")
        || url.contains("..")
        || url.contains('\\')
    {
        return Err(CliError::new(
            5,
            "upstream",
            format!("研究生菜单里的「{wanted}」地址不受支持"),
            "只接受研究生系统内的相对页面地址。",
        ));
    }
    let path = url.trim_start_matches('/');
    if path.starts_with("Gstudent/") || path.starts_with("PublicPage/") {
        Ok(format!("{YJXT_BASE}/{path}"))
    } else {
        Ok(format!("{YJXT_BASE}/Gstudent/{path}"))
    }
}

fn extract_eid(html: &str) -> Option<String> {
    let marker = "TermCalender.aspx?EID=";
    let start = html.find(marker)? + marker.len();
    let eid = html[start..]
        .chars()
        .take_while(|character| {
            *character != '&'
                && *character != '\''
                && *character != '"'
                && !character.is_whitespace()
        })
        .collect::<String>();
    if eid.is_empty()
        || !eid.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        })
    {
        return None;
    }
    Some(eid)
}

fn parse_menu(html: &str) -> BTreeMap<String, String> {
    let mut menu = BTreeMap::new();
    let mut position = 0;
    while let Some(relative) = html[position..].find("name:") {
        let start = position + relative + "name:".len();
        let Some((name, after_name)) = quoted_value(html, start) else {
            position = start;
            continue;
        };
        let Some(url_relative) = html[after_name..].find("url:") else {
            break;
        };
        let url_start = after_name + url_relative + "url:".len();
        let Some((url, after_url)) = quoted_value(html, url_start) else {
            position = url_start;
            continue;
        };
        if !name.is_empty() && !url.is_empty() {
            menu.insert(name, url);
        }
        position = after_url;
    }
    menu
}

fn quoted_value(text: &str, start: usize) -> Option<(String, usize)> {
    let raw = text.get(start..)?;
    let rest = raw.trim_start();
    let leading = raw.len() - rest.len();
    let quote = rest.chars().next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    let tail = &rest[quote.len_utf8()..];
    let end = tail.find(quote)?;
    let value = tail[..end].to_string();
    Some((
        value,
        start + leading + quote.len_utf8() + end + quote.len_utf8(),
    ))
}

// ---------------------------------------------------------------------------
// Browser / CDP.
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct BrowserError {
    code: u8,
    message: String,
    hint: String,
}

impl BrowserError {
    fn network(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            code: 4,
            message: message.into(),
            hint: hint.into(),
        }
    }

    fn upstream(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            code: 5,
            message: message.into(),
            hint: hint.into(),
        }
    }
}

impl From<BrowserError> for CliError {
    fn from(error: BrowserError) -> Self {
        let kind = if error.code == 5 {
            "upstream"
        } else {
            "network"
        };
        Self::new(error.code, kind, error.message, error.hint)
    }
}

struct Browser {
    ws: Ws,
    child: Child,
    profile: TempDir,
    timeout: Duration,
}

impl Browser {
    fn launch(cookies: &[SessionCookie], timeout: Duration) -> CliResult<Self> {
        sweep_stale_profiles();
        let profile = TempBuilder::new()
            .prefix("bupt-grad-")
            .tempdir()
            .map_err(|error| {
                CliError::new(
                    4,
                    "network",
                    format!("无法创建研究生浏览器的临时 profile：{error}"),
                    "检查临时目录权限后重试。",
                )
            })?;
        let chrome = find_chrome().ok_or_else(|| {
            CliError::new(4, "network", "没有找到 Google Chrome", CHROME_MISSING_HINT)
        })?;
        let mut command = ProcessCommand::new(chrome);
        command
            .arg("--headless=new")
            .arg("--remote-debugging-port=0")
            .arg(format!("--user-data-dir={}", profile.path().display()))
            .arg(format!("--user-agent={USER_AGENT}"))
            .arg("--window-size=1512,982")
            .arg("--disable-extensions")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|error| {
            CliError::new(
                4,
                "network",
                format!("无法启动 Google Chrome：{error}"),
                CHROME_MISSING_HINT,
            )
        })?;
        let stderr = child.stderr.take().expect("piped stderr");
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                if let Some(url) = line.strip_prefix("DevTools listening on ") {
                    let _ = sender.send(url.trim().to_string());
                }
            }
        });
        let port = match wait_for_port(&receiver, timeout) {
            Ok(port) => port,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.into());
            }
        };
        let page = match wait_for_page(port, timeout) {
            Ok(page) => page,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.into());
            }
        };
        let ws = match Ws::connect(&page, timeout) {
            Ok(ws) => ws,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(BrowserError::network(error, "Chrome 调试连接失败；重试一次。").into());
            }
        };
        let mut browser = Self {
            ws,
            child,
            profile,
            timeout,
        };
        browser.configure(cookies)?;
        Ok(browser)
    }

    fn configure(&mut self, cookies: &[SessionCookie]) -> CliResult<()> {
        self.ws.call("Page.enable", json!({}), self.timeout)?;
        self.ws.call("Runtime.enable", json!({}), self.timeout)?;
        self.ws.call("Network.enable", json!({}), self.timeout)?;
        self.ws.call(
            "Network.setUserAgentOverride",
            json!({"userAgent": USER_AGENT, "platform": "MacIntel"}),
            self.timeout,
        )?;
        for cookie in cookies {
            let mut params = Map::new();
            params.insert("name".to_string(), json!(cookie.name));
            params.insert("value".to_string(), json!(cookie.value));
            params.insert("domain".to_string(), json!(cookie.domain));
            params.insert("path".to_string(), json!(cookie.path));
            params.insert("secure".to_string(), json!(cookie.secure));
            params.insert("httpOnly".to_string(), json!(cookie.http_only));
            if let Some(expires) = cookie.expires {
                params.insert("expires".to_string(), json!(expires));
            }
            if let Some(same_site) = &cookie.same_site {
                params.insert("sameSite".to_string(), json!(same_site));
            }
            let result = self
                .ws
                .call("Network.setCookie", Value::Object(params), self.timeout)?;
            if result.get("success").and_then(Value::as_bool) != Some(true) {
                return Err(auth_error(
                    "无法把已保存的 CAS 会话注入浏览器",
                    "重新执行 `bupt login --role graduate`。",
                ));
            }
        }
        Ok(())
    }

    fn login(&mut self) -> CliResult<()> {
        self.goto(LOGIN_URL)?;
        let deadline = Instant::now() + self.timeout.max(Duration::from_secs(10));
        loop {
            if let Ok(url) = self.url()
                && url.contains("yjxt.bupt.edu.cn")
            {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(auth_error(
                    "保存的 CAS 会话没有进入研究生系统",
                    "会话可能已过期：重新执行 `bupt login --role graduate`，不会尝试破解验证码。",
                ));
            }
            thread::sleep(Duration::from_millis(120));
        }
    }

    fn menu(&mut self) -> CliResult<BTreeMap<String, String>> {
        self.goto(MENU_URL)?;
        let html = self
            .wait_for_html(|html| !parse_menu(html).is_empty(), 20)
            .ok_or_else(|| {
                BrowserError::upstream(
                    "研究生菜单页没有返回内容",
                    "重新执行本命令；页面结构可能已变。",
                )
            })?;
        Ok(parse_menu(&html))
    }

    fn goto(&mut self, url: &str) -> CliResult<()> {
        // Mark the current document before navigating. A fresh document loses
        // the marker, so a still-complete old page cannot make `wait_ready`
        // return before Chrome has actually committed the navigation.
        let _ = self
            .ws
            .eval("window.__bupt_cli_navigating = true; true", self.timeout);
        self.ws
            .call("Page.navigate", json!({"url": url}), self.timeout)?;
        self.wait_ready()
    }

    fn wait_ready(&mut self) -> CliResult<()> {
        let deadline = Instant::now() + self.timeout;
        while Instant::now() < deadline {
            if let Ok(Value::Object(state)) = self.ws.eval(
                "({ready:document.readyState,old:window.__bupt_cli_navigating===true})",
                self.timeout,
            ) && state.get("old").and_then(Value::as_bool) != Some(true)
                && state.get("ready").and_then(Value::as_str) == Some("complete")
            {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(50));
        }
        Err(BrowserError::network("Chrome 页面没有在限定时间内完成加载", "检查网络后重试。").into())
    }

    fn wait_for_html(&mut self, predicate: impl Fn(&str) -> bool, seconds: u64) -> Option<String> {
        let deadline = Instant::now() + Duration::from_secs(seconds);
        while Instant::now() < deadline {
            if let Ok(Value::String(html)) = self
                .ws
                .eval("document.documentElement.outerHTML", self.timeout)
                && predicate(&html)
            {
                return Some(html);
            }
            thread::sleep(Duration::from_millis(200));
        }
        None
    }

    fn url(&mut self) -> Result<String, String> {
        match self.ws.eval("location.href", self.timeout)? {
            Value::String(url) => Ok(url),
            _ => Ok(String::new()),
        }
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // `profile` is dropped after the process is gone.
        let _ = &self.profile;
    }
}

fn find_chrome() -> Option<&'static str> {
    CHROME_CANDIDATES
        .into_iter()
        .find(|candidate| Path::new(candidate).is_file())
}

fn sweep_stale_profiles() {
    let Ok(entries) = fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(STALE_PROFILE_SECONDS))
        .unwrap_or(UNIX_EPOCH);
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("bupt-grad-") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata
                .modified()
                .map_or(true, |modified| modified > cutoff)
        {
            continue;
        }
        let _ = fs::remove_dir_all(entry.path());
    }
}

fn wait_for_port(
    receiver: &mpsc::Receiver<String>,
    timeout: Duration,
) -> Result<u16, BrowserError> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match receiver.recv_timeout(remaining.min(Duration::from_millis(100))) {
            Ok(url) => {
                if let Some(port) = port_from_browser_url(&url) {
                    return Ok(port);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    Err(BrowserError::network(
        "Chrome 调试端口没有起来",
        "确认已安装 Google Chrome，且有权限创建临时 profile。",
    ))
}

fn port_from_browser_url(url: &str) -> Option<u16> {
    let rest = url.strip_prefix("ws://")?;
    let authority = rest.split('/').next()?;
    let (_, port) = authority.rsplit_once(':')?;
    port.parse().ok()
}

fn wait_for_page(port: u16, timeout: Duration) -> Result<String, BrowserError> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(body) = http_get(port, "/json/list", Duration::from_secs(1))
            && let Ok(value) = serde_json::from_str::<Value>(&body)
            && let Some(tabs) = value.as_array()
            && let Some(page) = tabs
                .iter()
                .find(|tab| tab.get("type").and_then(Value::as_str) == Some("page"))
            && let Some(url) = page.get("webSocketDebuggerUrl").and_then(Value::as_str)
        {
            return Ok(url.to_string());
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err(BrowserError::network(
        "Chrome 没有可用的页面 target",
        "确认 Chrome 能正常启动后重试。",
    ))
}

fn http_get(port: u16, path: &str, timeout: Duration) -> Result<String, String> {
    let address = format!("127.0.0.1:{port}")
        .to_socket_addrs()
        .map_err(|error| error.to_string())?
        .next()
        .ok_or_else(|| "no local address".to_string())?;
    let mut stream = TcpStream::connect_timeout(&address, timeout).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .map_err(|e| e.to_string())?;
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|e| e.to_string())?;
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| "malformed HTTP response".to_string())?;
    let status = String::from_utf8_lossy(&response[..split]);
    if !status
        .lines()
        .next()
        .is_some_and(|line| line.contains(" 200 "))
    {
        return Err("Chrome debug endpoint returned a non-200 status".to_string());
    }
    Ok(String::from_utf8_lossy(&response[split + 4..]).to_string())
}

struct Ws {
    stream: TcpStream,
    pending: Vec<u8>,
    next_id: u64,
}

impl Ws {
    fn connect(url: &str, timeout: Duration) -> Result<Self, String> {
        let rest = url
            .strip_prefix("ws://")
            .ok_or_else(|| "unsupported websocket scheme".to_string())?;
        let (authority, path) = rest
            .split_once('/')
            .map(|(authority, path)| (authority, format!("/{path}")))
            .unwrap_or((rest, "/".to_string()));
        let address = authority
            .to_socket_addrs()
            .map_err(|error| error.to_string())?
            .next()
            .ok_or_else(|| "websocket address did not resolve".to_string())?;
        let mut stream =
            TcpStream::connect_timeout(&address, timeout).map_err(|e| e.to_string())?;
        stream
            .set_read_timeout(Some(timeout))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(timeout))
            .map_err(|e| e.to_string())?;
        stream.set_nodelay(true).map_err(|e| e.to_string())?;
        let key = base64_encode(&random_bytes(16));
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: {authority}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )
        .map_err(|e| e.to_string())?;
        let mut buffer = Vec::new();
        let header_end = loop {
            let mut chunk = [0u8; 4096];
            let read = stream.read(&mut chunk).map_err(|e| e.to_string())?;
            if read == 0 {
                return Err("websocket handshake closed".to_string());
            }
            buffer.extend_from_slice(&chunk[..read]);
            if let Some(position) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                break position;
            }
            if buffer.len() > 64 * 1024 {
                return Err("websocket handshake response too large".to_string());
            }
        };
        let header = String::from_utf8_lossy(&buffer[..header_end]).to_string();
        if !header
            .lines()
            .next()
            .is_some_and(|line| line.contains(" 101"))
        {
            return Err("websocket handshake rejected".to_string());
        }
        let expected = base64_encode(&sha1(format!("{key}{WEBSOCKET_GUID}").as_bytes()));
        if !header.to_ascii_lowercase().contains(&format!(
            "sec-websocket-accept: {}",
            expected.to_ascii_lowercase()
        )) {
            return Err("websocket handshake accept mismatch".to_string());
        }
        Ok(Self {
            stream,
            pending: buffer[header_end + 4..].to_vec(),
            next_id: 0,
        })
    }

    fn call(&mut self, method: &str, params: Value, timeout: Duration) -> CliResult<Value> {
        self.next_id += 1;
        let id = self.next_id;
        let payload = json!({"id": id, "method": method, "params": params});
        let payload = serde_json::to_string(&payload)
            .map_err(|_| BrowserError::network("无法编码 CDP 请求", "重试一次。"))?;
        self.write_frame(0x1, payload.as_bytes()).map_err(|error| {
            BrowserError::network(format!("CDP 发送失败：{error}"), "重试一次。")
        })?;
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(BrowserError::network(
                    format!("CDP command timed out: {method}"),
                    "检查 Chrome 与网络后重试。",
                )
                .into());
            }
            self.stream
                .set_read_timeout(Some(remaining))
                .map_err(|error| BrowserError::network(error.to_string(), "重试一次。"))?;
            let text = self.read_text().map_err(|error| {
                BrowserError::network(format!("CDP 连接错误：{error}"), "重试一次。")
            })?;
            let message: Value = serde_json::from_str(&text).map_err(|_| {
                BrowserError::network(
                    "CDP 返回了非 JSON 帧",
                    "Chrome 版本或调试连接异常；重试一次。",
                )
            })?;
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                return Err(BrowserError::upstream(
                    format!("CDP 命令失败：{error}"),
                    "页面或 Chrome 调试协议异常；重试一次。",
                )
                .into());
            }
            return Ok(message.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    fn eval(&mut self, expression: &str, timeout: Duration) -> Result<Value, String> {
        let result = self
            .call(
                "Runtime.evaluate",
                json!({
                    "expression": expression,
                    "returnByValue": true,
                    "awaitPromise": true,
                }),
                timeout,
            )
            .map_err(|error| error.message)?;
        if result.get("exceptionDetails").is_some() {
            return Err("page evaluation failed".to_string());
        }
        Ok(result
            .get("result")
            .and_then(|result| result.get("value"))
            .cloned()
            .unwrap_or(Value::Null))
    }

    fn write_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<(), String> {
        let mut frame = Vec::with_capacity(payload.len() + 14);
        frame.push(0x80 | opcode);
        let length = payload.len();
        if length < 126 {
            frame.push(0x80 | length as u8);
        } else if length <= u16::MAX as usize {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(length as u16).to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(length as u64).to_be_bytes());
        }
        let mask = random_bytes(4);
        frame.extend_from_slice(&mask);
        for (index, byte) in payload.iter().enumerate() {
            frame.push(byte ^ mask[index % 4]);
        }
        self.stream.write_all(&frame).map_err(|e| e.to_string())
    }

    fn read_text(&mut self) -> Result<String, String> {
        let mut parts = Vec::new();
        loop {
            let (fin, opcode, payload) = self.read_frame()?;
            match opcode {
                0x1 | 0x0 => {
                    parts.extend_from_slice(&payload);
                    if fin {
                        return Ok(String::from_utf8_lossy(&parts).to_string());
                    }
                }
                0x8 => return Err("websocket closed by peer".to_string()),
                0x9 => {
                    self.write_frame(0xA, &payload)?;
                }
                0xA | 0x2 => {}
                _ => {}
            }
        }
    }

    fn read_frame(&mut self) -> Result<(bool, u8, Vec<u8>), String> {
        let header = self.read_exact(2)?;
        let fin = header[0] & 0x80 != 0;
        let opcode = header[0] & 0x0F;
        let masked = header[1] & 0x80 != 0;
        let mut length = u64::from(header[1] & 0x7F);
        if length == 126 {
            let bytes = self.read_exact(2)?;
            length = u64::from(u16::from_be_bytes([bytes[0], bytes[1]]));
        } else if length == 127 {
            let bytes = self.read_exact(8)?;
            length = u64::from_be_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]);
        }
        if length > 64 * 1024 * 1024 {
            return Err("websocket frame too large".to_string());
        }
        let mask = if masked {
            Some(self.read_exact(4)?)
        } else {
            None
        };
        let mut payload = self.read_exact(length as usize)?;
        if let Some(mask) = mask {
            for (index, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[index % 4];
            }
        }
        Ok((fin, opcode, payload))
    }

    fn read_exact(&mut self, count: usize) -> Result<Vec<u8>, String> {
        let mut output = Vec::with_capacity(count);
        if !self.pending.is_empty() {
            let take = count.min(self.pending.len());
            output.extend(self.pending.drain(..take));
        }
        if output.len() < count {
            let mut rest = vec![0u8; count - output.len()];
            self.stream
                .read_exact(&mut rest)
                .map_err(|e| e.to_string())?;
            output.extend_from_slice(&rest);
        }
        Ok(output)
    }
}

fn random_bytes(count: usize) -> Vec<u8> {
    if count > 0
        && let Ok(mut source) = fs::File::open("/dev/urandom")
    {
        let mut bytes = vec![0u8; count];
        if source.read_exact(&mut bytes).is_ok() {
            return bytes;
        }
    }
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
        ^ u64::from(std::process::id());
    (0..count)
        .map(|index| (seed.rotate_left((index as u32 % 63) + 1) >> 24) as u8)
        .collect()
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::new();
    for chunk in bytes.chunks(3) {
        let first = chunk[0] as u32;
        let second = chunk.get(1).copied().unwrap_or(0) as u32;
        let third = chunk.get(2).copied().unwrap_or(0) as u32;
        let value = (first << 16) | (second << 8) | third;
        output.push(TABLE[((value >> 18) & 0x3F) as usize] as char);
        output.push(TABLE[((value >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            output.push(TABLE[((value >> 6) & 0x3F) as usize] as char);
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(TABLE[(value & 0x3F) as usize] as char);
        } else {
            output.push('=');
        }
    }
    output
}

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut message = data.to_vec();
    let bit_length = (message.len() as u64) * 8;
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_length.to_be_bytes());

    let mut h0 = 0x67452301_u32;
    let mut h1 = 0xEFCDAB89_u32;
    let mut h2 = 0x98BADCFE_u32;
    let mut h3 = 0x10325476_u32;
    let mut h4 = 0xC3D2E1F0_u32;

    for chunk in message.chunks(64) {
        let mut words = [0u32; 80];
        for (index, word) in words.iter_mut().take(16).enumerate() {
            let offset = index * 4;
            *word = u32::from_be_bytes([
                chunk[offset],
                chunk[offset + 1],
                chunk[offset + 2],
                chunk[offset + 3],
            ]);
        }
        for index in 16..80 {
            words[index] =
                (words[index - 3] ^ words[index - 8] ^ words[index - 14] ^ words[index - 16])
                    .rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h0, h1, h2, h3, h4);
        for (index, word) in words.iter().enumerate() {
            let (function, constant) = match index {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temperature = a
                .rotate_left(5)
                .wrapping_add(function)
                .wrapping_add(e)
                .wrapping_add(constant)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temperature;
        }
        h0 = h0.wrapping_add(a);
        h1 = h1.wrapping_add(b);
        h2 = h2.wrapping_add(c);
        h3 = h3.wrapping_add(d);
        h4 = h4.wrapping_add(e);
    }

    let mut digest = [0u8; 20];
    for (index, value) in [h0, h1, h2, h3, h4].iter().enumerate() {
        digest[index * 4..index * 4 + 4].copy_from_slice(&value.to_be_bytes());
    }
    digest
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const USER: &str = "20260001";

    fn write_account(config: &Path, username: &str, role: &str) {
        fs::create_dir_all(config).unwrap();
        fs::write(
            config.join("credentials.json"),
            serde_json::to_vec_pretty(&json!({
                "username": username,
                "password": "synthetic-test-password",
                "role": role,
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn write_session(cache: &Path, username: &str, role: &str, cookies: Value) {
        fs::create_dir_all(cache).unwrap();
        fs::write(
            cache.join(SESSION_FILENAME),
            serde_json::to_vec_pretty(&json!({
                "version": 2,
                "username": username,
                "role": role,
                "cookies": cookies,
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn synthetic_cookie(expiry: &str) -> Value {
        json!([{
            "raw_cookie": "CASTGC=synthetic-castgc; Path=/authserver; Secure; HttpOnly",
            "path": ["/authserver", true],
            "domain": {"HostOnly": "auth.bupt.edu.cn"},
            "expires": {"AtUtc": expiry},
        }])
    }

    fn synthetic_suffix_cookie() -> Value {
        json!([{
            "raw_cookie": "CASTGC=synthetic-castgc; Path=/authserver; Secure; HttpOnly",
            "path": ["/authserver", true],
            "domain": {"Suffix": "bupt.edu.cn"},
            "expires": {"AtUtc": "2100-01-01T00:00:00Z"},
        }])
    }

    #[test]
    fn missing_session_is_an_auth_error_before_chrome_launch() {
        let root = tempdir().unwrap();
        let config = root.path().join("config");
        write_account(&config, USER, "graduate");
        let error = IdentitySessionAuth::new(&config, root.path())
            .cookies(USER, Role::Graduate)
            .expect_err("missing session must not launch Chrome");
        assert_eq!(error.code, 3);
        assert!(error.message.contains("CAS 会话"));
    }

    #[test]
    fn foreign_or_role_mismatched_session_is_not_reused() {
        let root = tempdir().unwrap();
        let config = root.path().join("config");
        write_account(&config, USER, "graduate");
        write_session(
            root.path(),
            "20260002",
            "graduate",
            synthetic_cookie("2100-01-01T00:00:00Z"),
        );
        let error = IdentitySessionAuth::new(&config, root.path())
            .cookies(USER, Role::Graduate)
            .expect_err("foreign session must be refused");
        assert_eq!(error.code, 3);
        assert!(error.message.contains("不属于"));
    }

    #[test]
    fn expired_cas_cookies_are_not_injected() {
        let root = tempdir().unwrap();
        let config = root.path().join("config");
        write_account(&config, USER, "graduate");
        write_session(
            root.path(),
            USER,
            "graduate",
            synthetic_cookie("2000-01-01T00:00:00Z"),
        );
        let error = IdentitySessionAuth::new(&config, root.path())
            .cookies(USER, Role::Graduate)
            .expect_err("expired cookie must not become a browser session");
        assert_eq!(error.code, 3);
        assert!(error.message.contains("没有可用"));
    }

    #[test]
    fn live_cas_cookie_is_scoped_to_the_known_auth_host() {
        let root = tempdir().unwrap();
        let config = root.path().join("config");
        write_account(&config, USER, "graduate");
        write_session(
            root.path(),
            USER,
            "graduate",
            synthetic_cookie("2100-01-01T00:00:00Z"),
        );
        let cookies = IdentitySessionAuth::new(&config, root.path())
            .cookies(USER, Role::Graduate)
            .expect("live synthetic session");
        assert_eq!(cookies.len(), 1);
        assert_eq!(cookies[0].name, "CASTGC");
        assert_eq!(cookies[0].domain, "auth.bupt.edu.cn");
        assert_eq!(cookies[0].path, "/authserver");
        assert!(cookies[0].secure && cookies[0].http_only);
    }

    #[test]
    fn websocket_handshake_helpers_match_known_vectors() {
        assert_eq!(base64_encode(&sha1(b"abc")), "qZk+NkcGgWq6PiVxeFDCbJzQ2J0=");
        assert_eq!(base64_encode(b"abc"), "YWJj");
        assert_eq!(base64_encode(b"a"), "YQ==");
        assert_eq!(
            base64_encode(&sha1(
                b"dGhlIHNhbXBsZSBub25jZQ==258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
            )),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn broad_school_cookie_is_narrowed_to_the_cas_host() {
        let root = tempdir().unwrap();
        let config = root.path().join("config");
        write_account(&config, USER, "graduate");
        write_session(root.path(), USER, "graduate", synthetic_suffix_cookie());
        let cookies = IdentitySessionAuth::new(&config, root.path())
            .cookies(USER, Role::Graduate)
            .expect("suffix school cookie");
        assert_eq!(cookies[0].domain, "auth.bupt.edu.cn");
    }

    #[test]
    fn menu_urls_stay_on_known_graduate_pages() {
        let mut menu = BTreeMap::new();
        menu.insert("学期课表信息查询".to_string(), "Schedule.aspx".to_string());
        assert_eq!(
            menu_url(&menu, "学期课表信息查询").unwrap(),
            "https://yjxt.bupt.edu.cn/Gstudent/Schedule.aspx"
        );
        menu.insert(
            "学期课表信息查询".to_string(),
            "Gstudent/Schedule.aspx".to_string(),
        );
        assert_eq!(
            menu_url(&menu, "学期课表信息查询").unwrap(),
            "https://yjxt.bupt.edu.cn/Gstudent/Schedule.aspx"
        );
        for untrusted in [
            "https://attacker.example/",
            "//attacker.example/",
            "../x",
            "a\\b",
        ] {
            menu.insert("学期课表信息查询".to_string(), untrusted.to_string());
            let error = menu_url(&menu, "学期课表信息查询").expect_err("untrusted menu URL");
            assert_eq!(error.code, 5);
        }
    }

    #[test]
    fn menu_parser_and_eid_extraction_ignore_untrusted_urls() {
        let html = "name: '学期课表信息查询', t: 'x', url: 'Schedule.aspx' \
                    name: '学期考核信息查询', t: 'x', url: 'Exams.aspx'";
        let menu = parse_menu(html);
        assert_eq!(
            menu.get("学期课表信息查询").map(String::as_str),
            Some("Schedule.aspx")
        );
        assert_eq!(
            menu.get("学期考核信息查询").map(String::as_str),
            Some("Exams.aspx")
        );
        assert_eq!(
            extract_eid("TermCalender.aspx?EID=abc-123_XYZ&amp;x=1"),
            Some("abc-123_XYZ".to_string())
        );
        assert_eq!(extract_eid("TermCalender.aspx?EID=abc/def"), None);
        assert_eq!(extract_eid("TermCalender.aspx?EID="), None);
    }
}
