//! A browserless HTTPS page source for the graduate system (yjxt).
//!
//! The verified sign-in is a pure HTTPS chain: CAS issues a ticket for the
//! HTTPS callback, the site then asks for a second ticket using its legacy
//! HTTP service name, and both observed same-site landing pages must be
//! visited before the menu or any business page returns real data. Every
//! request below is HTTPS-only; advertised HTTP destinations are upgraded, and
//! anything off the two known hosts is refused. No site script is executed.

use super::{GradPage, GradPageSource, GradPages};
use crate::cli::{CliError, CliResult, Context};
use crate::identity::{self, CasCookie, Role};
use cookie_store::{Cookie, CookieDomain, CookieStore};
use std::collections::BTreeMap;
use url::Url;

const CAS: &str = "auth.bupt.edu.cn";
const APP: &str = "yjxt.bupt.edu.cn";
const CAS_URL: &str = "https://auth.bupt.edu.cn/authserver/login";
const TLS_SERVICE: &str = "https://yjxt.bupt.edu.cn/ULogin.aspx";
const LEGACY_SERVICE: &str = "http://yjxt.bupt.edu.cn/ULogin.aspx";
const MENU: &str = "https://yjxt.bupt.edu.cn/Gstudent/LeftMenu.aspx?menu=00";

fn scope_error(hint: &str) -> CliError {
    CliError::new(
        5,
        "upstream",
        "研究生教务的跳转超出了已验证的 HTTPS 范围",
        hint,
    )
}

fn auth_error(message: impl Into<String>, hint: impl Into<String>) -> CliError {
    CliError::new(3, "auth", message, hint)
}

fn network_error() -> CliError {
    CliError::new(
        4,
        "network",
        "研究生教务 HTTPS 请求失败",
        "检查校园网络后重试；该请求没有被自动重试。",
    )
}

fn body_text(response: &crate::transport::Response) -> String {
    String::from_utf8_lossy(&response.body).to_string()
}

fn signed_url(raw: &str, host: &str) -> Option<Url> {
    if raw.len() > 8192 || !raw.bytes().all(|b| b.is_ascii_graphic()) || raw.contains('\\') {
        return None;
    }
    let url = Url::parse(raw).ok()?;
    let scheme = url.scheme();
    if !matches!(scheme, "http" | "https") {
        return None;
    }
    let prefix = format!("{scheme}://{host}/");
    if url.as_str() != raw
        || url.fragment().is_some()
        || url.host_str() != Some(host)
        || url.port().is_some()
        || !url.username().is_empty()
        || !url.password().is_none_or(|p| p.is_empty())
        || !raw.starts_with(&prefix)
    {
        return None;
    }
    Some(url)
}

fn upgrade_https(url: &Url) -> Option<Url> {
    let mut url = url.clone();
    if url.scheme() == "http" {
        url.set_scheme("https").ok()?;
    }
    (url.scheme() == "https").then_some(url)
}

fn single_ticket(url: &Url) -> bool {
    url.query_pairs().count() == 1
        && url.query_pairs().any(|(key, value)| {
            key == "ticket"
                && value.starts_with("ST-")
                && (4..2048).contains(&value.len())
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        })
}

fn install_saved(cookie: &CasCookie, url: &Url, store: &mut CookieStore) {
    let raw = format!(
        "{}={}; Path={}; Secure{}",
        cookie.name(),
        cookie.value(),
        cookie.path(),
        if cookie.http_only() { "; HttpOnly" } else { "" }
    );
    if let Ok(cookie) = Cookie::parse(raw, url) {
        let _ = store.insert(cookie, url);
    }
}

/// Sign in over verified HTTPS and read pages through the scoped transport.
pub(in crate::academics) struct HttpsPageSource<'a, 't> {
    context: &'a mut Context<'t>,
    cookies: CookieStore,
    menu: Option<String>,
}

impl<'a, 't> HttpsPageSource<'a, 't> {
    pub(in crate::academics) fn new(context: &'a mut Context<'t>) -> CliResult<Self> {
        let account = identity::require_authenticated_profile(&context.config_dir)?;
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
        let saved = identity::load_cas_cookies_for_auth(&context.config_dir, &context.cache_dir)
            .map_err(|error| {
                auth_error(
                    format!("研究生系统需要可用的本地 CAS 会话：{}", error.message),
                    "运行 `bupt login --role graduate`，再重试本命令。",
                )
            })?;
        let mut cookies = CookieStore::default();
        let mut cas = Url::parse(CAS_URL).expect("static CAS URL");
        cas.query_pairs_mut().append_pair("service", TLS_SERVICE);
        for cookie in &saved {
            install_saved(cookie, &cas, &mut cookies);
        }
        let mut source = Self {
            context,
            cookies,
            menu: None,
        };
        source.sign_in()?;
        Ok(source)
    }

    fn exchange(&mut self, url: &Url) -> CliResult<crate::transport::Response> {
        let host = url.host_str().unwrap_or_default();
        if url.scheme() != "https" || !matches!(host, CAS | APP) {
            return Err(scope_error("所有研究生教务请求必须使用 HTTPS。"));
        }
        let cookie = self
            .cookies
            .get_request_values(url)
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ");
        let headers = if cookie.is_empty() {
            Vec::new()
        } else {
            vec![("Cookie".into(), cookie)]
        };
        let response = self
            .context
            .transport
            .send(crate::transport::Request {
                method: "GET",
                url: url.as_str().to_owned(),
                headers,
                body: Vec::new(),
            })
            .map_err(|_| network_error())?;
        for (name, value) in &response.headers {
            if name.eq_ignore_ascii_case("set-cookie")
                && let Ok(cookie) = Cookie::parse(value.clone(), url)
                && (matches!(&cookie.domain, CookieDomain::HostOnly(d) if d == host)
                    || matches!(&cookie.domain, CookieDomain::Suffix(d) if d == host))
            {
                let _ = self.cookies.insert(cookie, url);
            }
        }
        Ok(response)
    }

    fn location(response: &crate::transport::Response) -> CliResult<Option<String>> {
        let mut locations = response
            .headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("location"));
        let Some((_, value)) = locations.next() else {
            return Ok(None);
        };
        if locations.next().is_some() || value.is_empty() {
            return Err(scope_error("教务响应的跳转头不受支持。"));
        }
        Ok(Some(value.clone()))
    }

    fn page(&mut self, url: &Url) -> CliResult<String> {
        let response = self.exchange(url)?;
        if !(200..300).contains(&response.status) {
            return Err(network_error());
        }
        Ok(body_text(&response))
    }

    fn sign_in(&mut self) -> CliResult<()> {
        let mut cas = Url::parse(CAS_URL).expect("static CAS URL");
        cas.query_pairs_mut().append_pair("service", TLS_SERVICE);
        let first = self.ticketed_callback(cas, TLS_SERVICE)?;
        let second_cas = self.legacy_cas(first)?;
        let second = self.ticketed_callback(second_cas, LEGACY_SERVICE)?;
        let mut current = second;
        for path in ["/Gstudent/Default.aspx", "/Gstudent/DefaultN.aspx"] {
            current = self.landing(current, path)?;
        }
        let menu = self.page(&Url::parse(MENU).expect("static menu URL"))?;
        if parse_menu(&menu).is_empty() {
            return Err(CliError::new(
                5,
                "upstream",
                "研究生菜单页没有返回内容",
                "页面结构可能已变；重试一次。",
            ));
        }
        self.menu = Some(menu);
        Ok(())
    }

    /// Exchange one CAS URL and follow its single redirect to the graduate
    /// callback, or validate a same-host CAS service URL.
    fn ticketed_callback(&mut self, cas: Url, _service: &str) -> CliResult<Url> {
        let response = self.exchange(&cas)?;
        let location = Self::location(&response)?.ok_or_else(|| {
            auth_error(
                "CAS 没有签发研究生教务票据",
                "重新执行 `bupt login --role graduate`。",
            )
        })?;
        let url = signed_url(&location, APP)
            .ok_or_else(|| scope_error("教务票据回调必须是校内教务主机的绝对 HTTPS 地址。"))?;
        if url.path() != "/ULogin.aspx" || !single_ticket(&url) {
            return Err(scope_error("教务回调必须携带且仅携带一张 CAS 票据。"));
        }
        upgrade_https(&url).ok_or_else(|| scope_error("教务回调无法升级为 HTTPS。"))
    }

    fn legacy_cas(&mut self, callback: Url) -> CliResult<Url> {
        let response = self.exchange(&callback)?;
        let location = Self::location(&response)?
            .ok_or_else(|| scope_error("教务回调之后没有回到统一认证的入口。"))?;
        let url = signed_url(&location, CAS)
            .ok_or_else(|| scope_error("第二轮 CAS 入口不在统一的认证 HTTPS 范围内。"))?;
        if url.path() != "/authserver/login"
            || url.query_pairs().count() != 1
            || !url
                .query_pairs()
                .any(|(key, value)| key == "service" && value == LEGACY_SERVICE)
        {
            return Err(scope_error("第二轮 CAS service 不是已验证的教务回调。"));
        }
        upgrade_https(&url).ok_or_else(|| scope_error("第二轮 CAS 入口无法升级为 HTTPS。"))
    }

    fn landing(&mut self, current: Url, path: &str) -> CliResult<Url> {
        let response = self.exchange(&current)?;
        let location = Self::location(&response)?.ok_or_else(|| {
            CliError::new(
                5,
                "upstream",
                "研究生教务落地页没有跳转",
                "页面结构可能已变；重试一次。",
            )
        })?;
        let url = signed_url(&location, APP)
            .filter(|url| url.path() == path)
            .ok_or_else(|| scope_error("教务落地页地址不受支持。"))?;
        upgrade_https(&url).ok_or_else(|| scope_error("教务落地页无法升级为 HTTPS。"))
    }
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
    Some((
        tail[..end].to_string(),
        start + leading + quote.len_utf8() + end + 1,
    ))
}

fn menu_page_url(menu: &BTreeMap<String, String>, wanted: &str) -> CliResult<Url> {
    let (_, relative) = menu
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
    let relative = relative.trim();
    if relative.is_empty()
        || relative.starts_with("http:")
        || relative.starts_with("https:")
        || relative.starts_with("//")
        || relative.contains("..")
        || relative.contains('\\')
    {
        return Err(scope_error("研究生菜单里的地址不受支持。"));
    }
    let relative = relative.trim_start_matches('/');
    let (path, query) = match relative.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (relative, None),
    };
    let path = if path.starts_with("Gstudent/") || path.starts_with("PublicPage/") {
        format!("/{path}")
    } else {
        format!("/Gstudent/{path}")
    };
    // Keep the menu's already-escaped query exactly as the page emitted it.
    let mut raw = format!("https://{APP}{path}");
    if let Some(query) = query {
        raw.push('?');
        raw.push_str(query);
    }
    crate::transport::graduate_scope::approved_page(&raw)
        .ok_or_else(|| scope_error("研究生菜单地址超出已验证的页面范围。"))
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
        || eid.len() > 2048
        || !eid.bytes().any(|b| b.is_ascii_alphanumeric())
        || !eid.bytes().all(|b| b.is_ascii_graphic() && b != b'%')
    {
        return None;
    }
    Some(eid)
}

impl GradPageSource for HttpsPageSource<'_, '_> {
    fn collect(&mut self, wanted: &[GradPage]) -> CliResult<GradPages> {
        let menu = self
            .menu
            .as_deref()
            .ok_or_else(|| CliError::new(5, "upstream", "研究生菜单尚未就绪", "重试一次。"))?;
        let parsed = parse_menu(menu);
        let mut pages = GradPages::default();
        if wanted.contains(&GradPage::Schedule) || wanted.contains(&GradPage::Calendar) {
            let url = menu_page_url(&parsed, "学期课表信息查询")?;
            pages.schedule = Some(self.page(&url)?);
        }
        if wanted.contains(&GradPage::Calendar) {
            let eid = pages
                .schedule
                .as_deref()
                .and_then(extract_eid)
                .ok_or_else(|| {
                    CliError::new(
                        5,
                        "upstream",
                        "课表页里找不到教学日历入口",
                        "重试一次；页面结构可能已变。",
                    )
                })?;
            let mut url =
                Url::parse(&format!("https://{APP}/PublicPage/TermCalender.aspx")).unwrap();
            url.query_pairs_mut().append_pair("EID", &eid);
            pages.calendar = Some(self.page(&url)?);
        }
        if wanted.contains(&GradPage::Exams) {
            let url = menu_page_url(&parsed, "学期考核信息查询")?;
            pages.exams = Some(self.page(&url)?);
        }
        Ok(pages)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graduate_menu_pages_stay_on_verified_aspx_paths() {
        let mut menu = BTreeMap::new();
        menu.insert(
            "学期课表信息查询".to_string(),
            "Gstudent/xskbcx.aspx".into(),
        );
        menu.insert("学期考核信息查询".to_string(), "xskhcx.aspx".into());
        let schedule = menu_page_url(&menu, "学期课表信息查询").unwrap();
        let exams = menu_page_url(&menu, "学期考核信息查询").unwrap();
        assert_eq!(schedule.path(), "/Gstudent/xskbcx.aspx");
        assert_eq!(schedule.host_str(), Some(APP));
        assert_eq!(exams.path(), "/Gstudent/xskhcx.aspx");
        assert!(exams.query().is_none());
        let mut hostile = BTreeMap::new();
        hostile.insert(
            "学期课表信息查询".to_string(),
            "https://yjxt.bupt.edu.cn/evil".into(),
        );
        assert!(menu_page_url(&hostile, "学期课表信息查询").is_err());
        hostile.insert(
            "学期课表信息查询".to_string(),
            "../PublicPage/xskbcx.aspx".into(),
        );
        assert!(menu_page_url(&hostile, "学期课表信息查询").is_err());
    }

    #[test]
    fn graduate_eid_extraction_reuses_the_browser_semantics() {
        assert_eq!(
            extract_eid("TermCalender.aspx?EID=abc-123_XYZ&amp;UID="),
            Some("abc-123_XYZ".to_string())
        );
        assert_eq!(extract_eid("TermCalender.aspx?EID=abc%2Fdef"), None);
        assert_eq!(extract_eid("TermCalender.aspx?EID="), None);
    }

    #[test]
    fn advertised_http_destinations_are_upgraded_and_foreign_hosts_are_refused() {
        let http = signed_url("http://yjxt.bupt.edu.cn/ULogin.aspx?ticket=ST-a", APP).unwrap();
        assert_eq!(
            upgrade_https(&http).unwrap().as_str(),
            "https://yjxt.bupt.edu.cn/ULogin.aspx?ticket=ST-a"
        );
        assert!(signed_url("http://yjxt.bupt.edu.cn.evil.example/ULogin.aspx", APP).is_none());
        assert!(signed_url("https://yjxt.bupt.edu.cn:8443/ULogin.aspx", APP).is_none());
        assert!(signed_url("https://yjxt.bupt.edu.cn/ULogin.aspx#x", APP).is_none());
        let ticketed = upgrade_https(&http).unwrap();
        assert!(single_ticket(&ticketed));
        let clean = signed_url("https://yjxt.bupt.edu.cn/ULogin.aspx", APP).unwrap();
        assert!(!single_ticket(&clean));
    }

    #[test]
    fn graduate_menu_parsing_matches_the_browser_menu_shape() {
        let menu = parse_menu("name:'学期课表信息查询', t:'x', url:'xskbcx.aspx'");
        assert_eq!(
            menu.get("学期课表信息查询").map(String::as_str),
            Some("xskbcx.aspx")
        );
        assert!(parse_menu("no menu here").is_empty());
    }
}
