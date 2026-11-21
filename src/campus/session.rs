//! Narrow authenticated-session boundary for portal and electricity requests.
//!
//! The campus modules build and validate each request, then hand it to a
//! [`SessionDriver`] with a [`Route`] tag. The driver owns the campus session:
//! it attaches cookies at its transport layer and re-authenticates once when a
//! confirmed auth failure is reported. The route fixes the destination; a
//! scheme/WebVPN change needs a reviewed route change and must not be rewritten
//! silently. No credential material is ever put into a [`Request`] by this
//! module.
//!
//! The electricity driver uses the fixed HTTPS landing and query routes;
//! the portal driver is in the sibling `portal_session` module.

use super::electricity;
use super::portal::{self, NoticeType};
use crate::cli::{CliError, CliResult, Context};
use crate::identity::{self, OwnedCasSession};
use crate::transport::electricity_scope::{self, SignIn};
use crate::transport::{Request, Response, Transport};
use cookie_store::{Cookie, CookieDomain, CookieStore};
use url::Url;

/// The service scope for session establishment. Portal and electricity use
/// different Python session-priming paths even when both share CAS cookies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Service {
    Portal,
    Electricity,
}

#[derive(Clone, Copy)]
pub(crate) struct Route(RouteKind);

#[derive(Clone, Copy)]
enum RouteKind {
    PortalNoticeList(NoticeType),
    PortalNoticeSearch,
    PortalNoticeDetail,
    PortalNoticeDownload,
    PortalCard,
    ElectricityBuildings,
    ElectricityFloors,
    ElectricityRooms,
    ElectricityBalance,
}

impl Route {
    pub(super) fn portal_list(kind: NoticeType) -> Self {
        Self(RouteKind::PortalNoticeList(kind))
    }

    pub(super) fn portal_search() -> Self {
        Self(RouteKind::PortalNoticeSearch)
    }

    pub(super) fn portal_detail() -> Self {
        Self(RouteKind::PortalNoticeDetail)
    }

    pub(super) fn portal_download() -> Self {
        Self(RouteKind::PortalNoticeDownload)
    }

    pub(super) fn portal_card() -> Self {
        Self(RouteKind::PortalCard)
    }

    pub(super) fn electricity_buildings() -> Self {
        Self(RouteKind::ElectricityBuildings)
    }

    pub(super) fn electricity_floors() -> Self {
        Self(RouteKind::ElectricityFloors)
    }

    pub(super) fn electricity_rooms() -> Self {
        Self(RouteKind::ElectricityRooms)
    }

    pub(super) fn electricity_balance() -> Self {
        Self(RouteKind::ElectricityBalance)
    }

    /// Check that a request stays on this route's exact method, destination,
    /// and caller-supplied header set. A driver should call this before adding
    /// cookies or following anything, then re-check after any redirect.
    pub(crate) fn accepts(self, request: &Request) -> bool {
        match self.0 {
            RouteKind::PortalNoticeList(kind) => {
                request.method == "GET"
                    && request.url == portal::list_url(kind)
                    && request.headers.is_empty()
                    && request.body.is_empty()
            }
            RouteKind::PortalNoticeSearch => {
                request.method == "GET"
                    && request.headers.is_empty()
                    && request.body.is_empty()
                    && crate::transport::portal_scope::portal_url(&request.url, false)
                        .is_some_and(|url| url.path() == "/fz_ssjg.jsp")
            }
            RouteKind::PortalNoticeDetail => {
                request.method == "GET"
                    && portal::validate_notice_url(&request.url).is_ok()
                    && request.headers.is_empty()
                    && request.body.is_empty()
            }
            RouteKind::PortalNoticeDownload => {
                request.method == "GET"
                    && portal::validate_attachment_url(&request.url).is_ok()
                    && request.body.is_empty()
                    && request.headers.len() == 1
                    && request.headers.iter().all(|(name, value)| {
                        name == "Referer" && portal::is_attachment_referer(value)
                    })
            }
            RouteKind::PortalCard => {
                request.method == "GET"
                    && request.url == portal::CARD_URL
                    && headers_are(
                        request,
                        &[
                            ("Referer", portal::CARD_REFERER),
                            ("X-Requested-With", "XMLHttpRequest"),
                        ],
                    )
                    && request.body.is_empty()
            }
            RouteKind::ElectricityBuildings => electricity_route(request, "/part"),
            RouteKind::ElectricityFloors => electricity_route(request, "/floor"),
            RouteKind::ElectricityRooms => electricity_route(request, "/drom"),
            RouteKind::ElectricityBalance => electricity_route(request, "/search"),
        }
    }

    pub(super) fn is_electricity(self) -> bool {
        matches!(
            self.0,
            RouteKind::ElectricityBuildings
                | RouteKind::ElectricityFloors
                | RouteKind::ElectricityRooms
                | RouteKind::ElectricityBalance
        )
    }

    pub(crate) fn describe(self) -> &'static str {
        match self.0 {
            RouteKind::PortalNoticeList(_) => "portal notice list",
            RouteKind::PortalNoticeSearch => "portal notice search",
            RouteKind::PortalNoticeDetail => "portal notice detail",
            RouteKind::PortalNoticeDownload => "portal notice attachment",
            RouteKind::PortalCard => "portal campus card",
            RouteKind::ElectricityBuildings => "electricity buildings",
            RouteKind::ElectricityFloors => "electricity floors",
            RouteKind::ElectricityRooms => "electricity rooms",
            RouteKind::ElectricityBalance => "electricity balance",
        }
    }
}

fn headers_are(request: &Request, expected: &[(&str, &str)]) -> bool {
    request.headers.len() == expected.len()
        && expected.iter().all(|(name, value)| {
            request
                .headers
                .iter()
                .any(|(actual_name, actual_value)| actual_name == name && actual_value == value)
        })
}

fn electricity_route(request: &Request, path: &str) -> bool {
    request.method == "POST"
        && request.url == electricity::request_url(path)
        && headers_are(
            request,
            &[
                ("Referer", electricity::REFERER),
                ("X-Requested-With", "XMLHttpRequest"),
                ("Content-Type", "application/x-www-form-urlencoded"),
            ],
        )
        && !request.body.is_empty()
}

/// The parent-owned authenticated session interface.
///
/// `ensure` establishes or loads the session before a command touches the
/// network. `send` performs exactly one HTTP exchange for an already-validated
/// route; it must return any HTTP status as a [`Response`] (the campus layer
/// classifies 401/403 and body auth markers), must not silently retry, and must
/// not follow a redirect. `renew` is called only after the campus layer has
/// confirmed an auth failure and must refresh the session for the given service
/// once without changing the route or attaching credentials to an unapproved
/// destination.
pub(crate) trait SessionDriver {
    fn ensure(&mut self, service: Service) -> CliResult<()>;
    fn send(&mut self, route: Route, request: Request) -> CliResult<Response>;
    fn renew(&mut self, service: Service) -> CliResult<()>;
}

/// Per-invocation app cookies; never load or persist a portal session here.
pub(super) struct ElectricitySession<'a, 't> {
    context: &'a mut Context<'t>,
    cas: Option<OwnedCasSession>,
    cookies: CookieStore,
    ready: bool,
    renewed: bool,
}

impl<'a, 't> ElectricitySession<'a, 't> {
    pub(super) fn new(context: &'a mut Context<'t>) -> Self {
        Self {
            context,
            cas: None,
            cookies: CookieStore::default(),
            ready: false,
            renewed: false,
        }
    }

    fn attach_cookies(&self, request: &mut Request) {
        let Some(url) = Url::parse(&request.url).ok() else {
            return;
        };
        let app: Vec<_> = self.cookies.get_request_values(&url).collect();
        let extra = self
            .cas
            .as_ref()
            .and_then(|cas| cas.electricity_cookie(&request.url));
        let mut pairs: Vec<String> = app
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        if let Some(extra) = &extra {
            pairs.extend(
                extra
                    .split("; ")
                    .filter(|entry| {
                        entry
                            .split_once('=')
                            .is_some_and(|(name, _)| !app.iter().any(|(key, _)| *key == name))
                    })
                    .map(str::to_owned),
            );
        }
        if !pairs.is_empty() {
            request.headers.push(("Cookie".into(), pairs.join("; ")));
        }
    }

    fn exchange(&mut self, mut request: Request) -> CliResult<Response> {
        self.attach_cookies(&mut request);
        // This wire layer disables redirects independently of these checks.
        let url = Url::parse(&request.url).map_err(|_| campus_scope_error())?;
        let response = self.context.transport.send(request)?;
        retain_app_cookies(&mut self.cookies, &url, &response);
        Ok(response)
    }

    fn get(&mut self, url: &str) -> CliResult<Response> {
        self.exchange(Request {
            method: "GET",
            url: url.into(),
            headers: vec![],
            body: vec![],
        })
    }

    fn sign_in(&mut self) -> CliResult<()> {
        let landing = self.get(electricity::REFERER)?;
        let Some(login) = electricity_redirect(&landing)? else {
            return check_electricity_landing(landing.status);
        };
        let login = electricity_scope::app_redirect(login)
            .filter(|url| electricity_scope::sign_in(url) == Some(SignIn::Login))
            .ok_or_else(campus_scope_error)?;
        let response = self.get(&login)?;
        let service = electricity_redirect(&response)?
            .and_then(electricity_scope::app_redirect)
            .filter(|url| electricity_scope::sign_in(url) == Some(SignIn::Service))
            .ok_or_else(campus_scope_error)?;
        let response = self.get(&service)?;
        let cas_url = electricity_redirect(&response)?.ok_or_else(campus_scope_error)?;
        let callback_service =
            electricity_scope::cas_service(cas_url).ok_or_else(campus_scope_error)?;
        let issued_for = Url::parse(&service).map_err(|_| campus_scope_error())?;
        if callback_service != issued_for {
            return Err(campus_scope_error());
        }
        let cas = self.cas.as_mut().ok_or_else(campus_scope_error)?;
        let ticket_url = cas.electricity_ticket(self.context.transport, cas_url)?;
        let response = self.get(&ticket_url)?;
        if let Some(next) = electricity_redirect(&response)? {
            if electricity_scope::app_redirect(next).as_deref() != Some(electricity::REFERER) {
                return Err(campus_scope_error());
            }
        } else {
            check_electricity_landing(response.status)?;
        }
        let landing = self.get(electricity::REFERER)?;
        if let Some(next) = electricity_redirect(&landing)?
            && electricity_scope::app_redirect(next)
                .is_some_and(|url| electricity_scope::sign_in(&url) == Some(SignIn::Login))
        {
            return Err(CliError::new(
                3,
                "auth",
                "electricity service did not accept the CAS ticket",
                "No password was sent.",
            ));
        }
        check_electricity_landing(landing.status)
    }
}

fn electricity_redirect(response: &Response) -> CliResult<Option<&str>> {
    if !matches!(response.status, 301 | 302 | 303 | 307 | 308) {
        return Ok(None);
    }
    let mut locations = response
        .headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("location"));
    let (_, value) = locations.next().ok_or_else(campus_scope_error)?;
    if locations.next().is_some() || value.is_empty() {
        return Err(campus_scope_error());
    }
    Ok(Some(value))
}

impl SessionDriver for ElectricitySession<'_, '_> {
    fn ensure(&mut self, service: Service) -> CliResult<()> {
        if service != Service::Electricity {
            return Err(campus_scope_error());
        }
        if self.ready {
            return Ok(());
        }
        if self.cas.is_none() {
            let cas = identity::load_owned_cas_session(self.context)?;
            if !cas.has_usable_cookies() {
                return Err(CliError::new(
                    3,
                    "auth",
                    "the saved CAS session has no usable cookies",
                    "Run `bupt login`, then try again.",
                ));
            }
            self.cas = Some(cas);
        }
        self.sign_in()?;
        self.ready = true;
        Ok(())
    }

    fn send(&mut self, route: Route, request: Request) -> CliResult<Response> {
        if !self.ready || !route.is_electricity() || !route.accepts(&request) {
            return Err(campus_scope_error());
        }
        self.exchange(request)
    }

    fn renew(&mut self, service: Service) -> CliResult<()> {
        if service != Service::Electricity {
            return Err(campus_scope_error());
        }
        if self.renewed {
            return Err(CliError::new(
                3,
                "auth",
                "the electricity session could not be renewed",
                "No password was sent; verify the saved campus session.",
            ));
        }
        self.renewed = true;
        self.ready = false;
        self.cookies.clear();
        self.ensure(service)
    }
}

fn campus_scope_error() -> CliError {
    CliError::new(
        5,
        "upstream",
        "request is outside the electricity session scope",
        "",
    )
}

fn check_electricity_landing(status: u16) -> CliResult<()> {
    match status {
        200..=299 => Ok(()),
        401 | 403 => Err(CliError::new(
            3,
            "auth",
            "electricity landing rejected the saved session",
            "No password was sent.",
        )),
        301 | 302 | 303 | 307 | 308 => Err(CliError::new(
            5,
            "upstream",
            "electricity landing returned an unverified redirect",
            "Confirm the app's campus-network sign-in flow; no redirect was followed.",
        )),
        _ => Err(CliError::new(
            5,
            "upstream",
            format!("electricity landing returned HTTP {status}"),
            "",
        )),
    }
}

fn retain_app_cookies(store: &mut CookieStore, url: &Url, response: &Response) {
    const HOST: &str = "app.bupt.edu.cn";
    if url.scheme() != "https" || url.host_str() != Some(HOST) {
        return;
    }
    for (key, value) in &response.headers {
        if !key.eq_ignore_ascii_case("set-cookie") {
            continue;
        }
        if let Ok(cookie) = Cookie::parse(value.clone(), url)
            && (matches!(&cookie.domain, CookieDomain::HostOnly(host) if host == HOST)
                || matches!(&cookie.domain, CookieDomain::Suffix(domain) if domain == HOST))
        {
            let _ = store.insert(cookie, url);
        }
    }
}

fn scoped_request(
    driver: &mut dyn SessionDriver,
    route: Route,
    request: Request,
) -> CliResult<Response> {
    if !route.accepts(&request) {
        return Err(CliError::new(
            5,
            "upstream",
            format!("campus request left its scoped route: {}", route.describe()),
            "",
        ));
    }
    driver.send(route, request)
}

/// A `Transport` view over one route. Existing parsers keep their narrow
/// `Transport` seam, while every call is checked against the route first.
pub(super) struct ScopedTransport<'a> {
    driver: &'a mut dyn SessionDriver,
    route: Route,
}

impl<'a> ScopedTransport<'a> {
    pub(super) fn new(driver: &'a mut dyn SessionDriver, route: Route) -> Self {
        Self { driver, route }
    }
}

impl Transport for ScopedTransport<'_> {
    fn send(&mut self, request: Request) -> CliResult<Response> {
        scoped_request(self.driver, self.route, request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::testing::Scripted;

    struct TestDriver {
        transport: Scripted,
        routes: Vec<Route>,
    }

    impl SessionDriver for TestDriver {
        fn ensure(&mut self, _: Service) -> CliResult<()> {
            Ok(())
        }

        fn send(&mut self, route: Route, request: Request) -> CliResult<Response> {
            self.routes.push(route);
            self.transport.send(request)
        }

        fn renew(&mut self, _: Service) -> CliResult<()> {
            Ok(())
        }
    }

    fn portal_list_request(kind: NoticeType) -> Request {
        Request {
            method: "GET",
            url: portal::list_url(kind),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    const DETAIL_PAGE: &str = "http://my.bupt.edu.cn/xntz_content.jsp?urltype=news.NewsContentUrl&wbtreeid=2149&wbnewsid=144119";

    #[test]
    fn routes_accept_only_their_exact_shape() {
        let notice = Route::portal_list(NoticeType::Notice);
        assert!(notice.accepts(&portal_list_request(NoticeType::Notice)));
        assert!(!notice.accepts(&portal_list_request(NoticeType::News)));

        let mut card = portal_list_request(NoticeType::Notice);
        card.url = portal::CARD_URL.into();
        card.headers = vec![
            ("Referer".into(), portal::CARD_REFERER.into()),
            ("X-Requested-With".into(), "XMLHttpRequest".into()),
        ];
        assert!(!notice.accepts(&card));
        assert!(Route::portal_card().accepts(&card));
        card.headers
            .push(("Cookie".into(), "should-not-be-here".into()));
        assert!(!Route::portal_card().accepts(&card));

        let search = portal::search_url("奖学金", "1155").unwrap();
        let search_request = Request {
            method: "GET",
            url: search,
            headers: Vec::new(),
            body: Vec::new(),
        };
        assert!(Route::portal_search().accepts(&search_request));
        assert!(!Route::portal_search().accepts(&portal_list_request(NoticeType::Notice)));

        let mut detail = portal_list_request(NoticeType::Notice);
        detail.url = "https://my.bupt.edu.cn/2026/a/page.htm".into();
        assert!(Route::portal_detail().accepts(&detail));
        detail.url = "https://example.invalid/2026/a/page.htm".into();
        assert!(!Route::portal_detail().accepts(&detail));

        // An attachment is fetched only with the notice page as its referer.
        let attachment = "http://my.bupt.edu.cn/system/_content/download.jsp?urltype=news.DownloadAttachUrl&owner=1664271694&wbfileid=3251628";
        let mut download = Request {
            method: "GET",
            url: attachment.into(),
            headers: vec![("Referer".into(), DETAIL_PAGE.into())],
            body: Vec::new(),
        };
        assert!(Route::portal_download().accepts(&download));
        // The same request without its referer is what the portal answers with
        // a captcha page, so it must never be sent.
        download.headers.clear();
        assert!(!Route::portal_download().accepts(&download));
        download
            .headers
            .push(("Referer".into(), "https://example.invalid/page".into()));
        assert!(!Route::portal_download().accepts(&download));
        download.headers = vec![("Referer".into(), DETAIL_PAGE.into())];
        download
            .headers
            .push(("Cookie".into(), "should-not-be-here".into()));
        assert!(!Route::portal_download().accepts(&download));
        download.headers = vec![("Referer".into(), DETAIL_PAGE.into())];
        download.url = "http://my.bupt.edu.cn/system/_content/download.jsp?urltype=news.DownloadAttachUrl&owner=1".into();
        assert!(!Route::portal_download().accepts(&download));
        // A notice page is not an attachment, and the detail route is not a
        // download route: the two never overlap.
        assert!(!Route::portal_download().accepts(&portal_list_request(NoticeType::Notice)));
        assert!(!Route::portal_detail().accepts(&Request {
            method: "GET",
            url: attachment.into(),
            headers: Vec::new(),
            body: Vec::new(),
        }));
    }

    #[cfg(unix)]
    #[test]
    fn electricity_reuses_cas_only_at_cas_and_spends_one_ticket_on_the_app() {
        const ENTRY: &str = electricity::REFERER;
        const LOGIN: &str = "https://app.bupt.edu.cn/uc/wap/login?redirect=https%3A%2F%2Fapp.bupt.edu.cn%2Fbuptdf%2Fwap%2Fdefault%2Fchong";
        const SERVICE: &str = "https://app.bupt.edu.cn/a_bupt/api/sso/cas?redirect=https%3A%2F%2Fapp.bupt.edu.cn%2Fbuptdf%2Fwap%2Fdefault%2Fchong&from=wap";
        const CAS: &str = "https://auth.bupt.edu.cn/authserver/login";
        const VERIFY: &str = "https://auth.bupt.edu.cn/authserver/login?service=https%3A%2F%2Fucloud.bupt.edu.cn%2Fuclass";
        let ticket = format!("{SERVICE}&ticket=ST-TEST-APP");
        let mut cas = Url::parse(CAS).unwrap();
        cas.query_pairs_mut()
            .append_pair("noAutoRedirect", "1")
            .append_pair("service", SERVICE);
        let mut tr = Scripted::default();
        for (method, url, status, headers, body) in [
            (
                "GET",
                CAS,
                200,
                vec![],
                "<input name='execution' value='TEST-ONLY'>",
            ),
            (
                "POST",
                CAS,
                302,
                vec![
                    ("Location", "https://uc.bupt.edu.cn/"),
                    ("Set-Cookie", "CASTGC=TEST-ONLY; Secure; Path=/authserver"),
                ],
                "",
            ),
            (
                "GET",
                VERIFY,
                302,
                vec![(
                    "Location",
                    "https://ucloud.bupt.edu.cn/uclass?ticket=ST-TEST-UCLOUD",
                )],
                "",
            ),
            (
                "GET",
                ENTRY,
                302,
                vec![
                    ("Location", LOGIN),
                    ("Set-Cookie", "app=INITIAL; Secure; Path=/"),
                ],
                "",
            ),
            ("GET", LOGIN, 302, vec![("Location", SERVICE)], ""),
            ("GET", SERVICE, 302, vec![("Location", cas.as_str())], ""),
            (
                "GET",
                cas.as_str(),
                302,
                vec![("Location", ticket.as_str())],
                "",
            ),
            (
                "GET",
                ticket.as_str(),
                302,
                vec![
                    ("Location", "/buptdf/wap/default/chong"),
                    ("Set-Cookie", "app=READY; Secure; Path=/"),
                ],
                "",
            ),
            ("GET", ENTRY, 200, vec![], "electricity landing"),
        ] {
            tr.expected.push_back((
                method,
                url.into(),
                Ok(Response {
                    status,
                    headers: headers
                        .into_iter()
                        .map(|(k, v)| (k.into(), v.into()))
                        .collect(),
                    body: body.as_bytes().to_vec(),
                }),
            ));
        }
        let home = tempfile::tempdir().unwrap();
        let mut ctx = Context {
            home_dir: home.path().into(),
            config_dir: home.path().join("config"),
            cache_dir: home.path().join("cache"),
            transport: &mut tr,
        };
        let login = identity::commands()
            .into_iter()
            .find(|cmd| cmd.get_name() == "login")
            .unwrap()
            .try_get_matches_from([
                "login",
                "--user",
                "20260001",
                "--password",
                "TEST-ONLY",
                "--role",
                "graduate",
            ])
            .unwrap();
        identity::run("login", &login, &mut ctx).expect("scripted login");
        let cached = std::fs::read(ctx.cache_dir.join("session.json")).unwrap();
        let mut session = ElectricitySession::new(&mut ctx);
        session
            .ensure(Service::Electricity)
            .expect("scripted sign-in and ticket callback");
        session
            .ensure(Service::Electricity)
            .expect("in-memory session reuse");
        assert_eq!(
            std::fs::read(ctx.cache_dir.join("session.json")).unwrap(),
            cached
        );
        assert!(tr.expected.is_empty());
        assert_eq!(tr.calls.len(), 9);
        assert_eq!(tr.calls.iter().filter(|r| r.method == "POST").count(), 1);
        for (request, expected) in tr.calls[3..].iter().zip([
            None,
            Some("app=INITIAL"),
            Some("app=INITIAL"),
            Some("CASTGC=TEST-ONLY"),
            Some("app=INITIAL"),
            Some("app=READY"),
        ]) {
            assert_eq!(
                request
                    .headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("cookie"))
                    .map(|(_, v)| v.as_str()),
                expected
            );
        }
    }

    #[test]
    fn electricity_landing_cookies_are_app_only_and_keep_the_default_path() {
        let mut store = CookieStore::default();
        let landing = Url::parse(electricity::REFERER).unwrap();
        let response = Response {
            status: 200,
            headers: vec![
                (
                    "Set-Cookie".into(),
                    "app=TEST-ONLY; Secure; HttpOnly".into(),
                ),
                (
                    "Set-Cookie".into(),
                    "parent=TEST-ONLY; Domain=bupt.edu.cn; Path=/".into(),
                ),
                (
                    "Set-Cookie".into(),
                    "other=TEST-ONLY; Domain=auth.bupt.edu.cn; Path=/".into(),
                ),
            ],
            body: Vec::new(),
        };
        retain_app_cookies(&mut store, &landing, &response);
        let part = Url::parse(&electricity::request_url("/part")).unwrap();
        assert_eq!(
            store.get_request_values(&part).collect::<Vec<_>>(),
            [("app", "TEST-ONLY")]
        );
        for raw in [
            "https://auth.bupt.edu.cn/authserver/login",
            "https://app.bupt.edu.cn/other",
            "http://app.bupt.edu.cn/buptdf/wap/default/part",
        ] {
            assert!(
                store
                    .get_request_values(&Url::parse(raw).unwrap())
                    .next()
                    .is_none()
            );
        }
        let error = check_electricity_landing(302).unwrap_err();
        assert_eq!(error.code, 5);
        assert!(error.message.contains("redirect"));
        assert_eq!(check_electricity_landing(401).unwrap_err().code, 3);
    }

    #[test]
    fn electricity_route_rejects_credentials_and_other_destinations() {
        let mut request = Request {
            method: "POST",
            url: electricity::request_url("/part"),
            headers: vec![
                ("Referer".into(), electricity::REFERER.into()),
                ("X-Requested-With".into(), "XMLHttpRequest".into()),
                (
                    "Content-Type".into(),
                    "application/x-www-form-urlencoded".into(),
                ),
            ],
            body: b"areaid=2".to_vec(),
        };
        assert!(Route::electricity_buildings().accepts(&request));
        request.url = electricity::request_url("/search");
        assert!(!Route::electricity_buildings().accepts(&request));
        request.url = electricity::request_url("/part");
        request
            .headers
            .push(("Authorization".into(), "Bearer test".into()));
        assert!(!Route::electricity_buildings().accepts(&request));
    }

    #[test]
    fn scoped_transport_rejects_an_unapproved_request_before_the_driver() {
        let mut driver = TestDriver {
            transport: Scripted::default(),
            routes: Vec::new(),
        };
        let mut transport = ScopedTransport::new(&mut driver, Route::portal_card());
        let error = match transport.send(portal_list_request(NoticeType::Notice)) {
            Ok(_) => panic!("unapproved request must be rejected"),
            Err(error) => error,
        };
        assert_eq!((error.code, error.kind), (5, "upstream"));
        assert!(driver.routes.is_empty());
        assert!(driver.transport.calls.is_empty());
    }
}
