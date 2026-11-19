//! A per-command HTTPS portal session. CAS cookies never go to the portal,
//! and portal cookies and tickets never go to a plaintext callback.
use super::{Route, Service, SessionDriver};
use crate::cli::{CliError, CliResult, Context};
use crate::identity::{self, OwnedCasSession};
use crate::transport::{Request, Response, portal_scope};
use cookie_store::{Cookie, CookieDomain, CookieStore};
use url::Url;

pub(super) struct PortalSession<'a, 't> {
    context: &'a mut Context<'t>,
    cas: Option<OwnedCasSession>,
    cookies: CookieStore,
    renewed: bool,
}

impl<'a, 't> PortalSession<'a, 't> {
    pub(super) fn new(context: &'a mut Context<'t>) -> Self {
        Self {
            context,
            cas: None,
            cookies: CookieStore::default(),
            renewed: false,
        }
    }

    fn portal_get(&mut self, url: &str, mut headers: Vec<(String, String)>) -> CliResult<Response> {
        portal_scope::portal_url(url, true).ok_or_else(scope_error)?;
        let target = Url::parse(url).map_err(|_| scope_error())?;
        let cookies = self
            .cookies
            .get_request_values(&target)
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>();
        if !cookies.is_empty() {
            headers.push(("Cookie".into(), cookies.join("; ")));
        }
        let response = self.context.transport.send(Request {
            method: "GET",
            url: url.into(),
            headers,
            body: vec![],
        })?;
        for (name, value) in &response.headers {
            if name.eq_ignore_ascii_case("set-cookie")
                && let Ok(cookie) = Cookie::parse(value.clone(), &target)
                && (matches!(&cookie.domain, CookieDomain::HostOnly(host) if host == "my.bupt.edu.cn")
                    || matches!(&cookie.domain, CookieDomain::Suffix(host) if host == "my.bupt.edu.cn"))
            {
                let _ = self.cookies.insert(cookie, &target);
            }
        }
        Ok(response)
    }

    fn load_cas(&mut self) -> CliResult<&mut OwnedCasSession> {
        if self.cas.is_none() {
            self.cas = Some(identity::load_owned_cas_session(self.context)?);
        }
        Ok(self.cas.as_mut().expect("loaded CAS"))
    }
}

impl SessionDriver for PortalSession<'_, '_> {
    fn ensure(&mut self, service: Service) -> CliResult<()> {
        if service != Service::Portal {
            return Err(scope_error());
        }
        // Check the local binding only; a public notice or --count 0 does
        // not spend a service ticket or otherwise touch the network here.
        self.load_cas().map(|_| ())
    }

    fn send(&mut self, route: Route, request: Request) -> CliResult<Response> {
        if route.is_electricity() || !route.accepts(&request) {
            return Err(scope_error());
        }
        let initial = portal_scope::to_tls(&request.url).ok_or_else(scope_error)?;
        let mut headers = request.headers;
        for (key, value) in &mut headers {
            if key.eq_ignore_ascii_case("referer") {
                *value = portal_scope::CARD_REFERER.into();
            }
        }
        let mut url = initial.to_string();
        let mut used_ticket = false;
        let mut replayed = false;
        for _ in 0..6 {
            let response = self.portal_get(
                &url,
                if url == initial.as_str() {
                    headers.clone()
                } else {
                    vec![]
                },
            )?;
            let Some(location) = redirect(&response)? else {
                // A callback may render a generic landing page rather than
                // the original resource. Re-read the original route once.
                if used_ticket
                    && !replayed
                    && url != initial.as_str()
                    && (200..300).contains(&response.status)
                {
                    replayed = true;
                    url = initial.to_string();
                    continue;
                }
                return Ok(response);
            };
            if let Some(service) = portal_scope::service_from_cas(location) {
                if used_ticket {
                    return Err(CliError::new(
                        3,
                        "auth",
                        "portal rejected the saved CAS session",
                        "Run `bupt login` if the session has expired.",
                    ));
                }
                if !portal_scope::service_matches(&service, &initial) {
                    return Err(scope_error());
                }
                let cas = self.cas.as_mut().ok_or_else(scope_error)?;
                let callback = cas.portal_ticket(self.context.transport, &service)?;
                used_ticket = true;
                url = callback;
                continue;
            }
            let base = Url::parse(&url).map_err(|_| scope_error())?;
            let next = base.join(location).map_err(|_| scope_error())?;
            url = portal_scope::portal_url(next.as_str(), false)
                .ok_or_else(scope_error)?
                .into();
        }
        Err(CliError::new(
            5,
            "upstream",
            "too many portal redirects",
            "No further portal request was made.",
        ))
    }

    fn renew(&mut self, service: Service) -> CliResult<()> {
        if service != Service::Portal {
            return Err(scope_error());
        }
        if self.renewed {
            return Err(CliError::new(
                3,
                "auth",
                "portal session could not be renewed",
                "No password was sent.",
            ));
        }
        self.renewed = true;
        self.cookies.clear();
        Ok(())
    }
}

fn redirect(response: &Response) -> CliResult<Option<&str>> {
    if !matches!(response.status, 301 | 302 | 303 | 307 | 308) {
        return Ok(None);
    }
    response
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("location"))
        .map(|(_, value)| Some(value.as_str()))
        .ok_or_else(scope_error)
}

fn scope_error() -> CliError {
    CliError::new(
        5,
        "upstream",
        "portal request or redirect is outside the verified HTTPS scope",
        "Confirm the campus portal HTTPS address. No credentials were sent to an HTTP or unapproved destination.",
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::transport::testing::Scripted;
    use std::time::Duration;

    #[test]
    fn card_reuses_bound_cas_cookies_only_at_cas_and_honors_portal_cookie_scope() {
        const CAS: &str = "https://auth.bupt.edu.cn/authserver/login";
        const VERIFY: &str = "https://auth.bupt.edu.cn/authserver/login?service=https%3A%2F%2Fucloud.bupt.edu.cn%2Fuclass";
        const CARD: &str = "https://my.bupt.edu.cn/system/resource/app/cuser/getwxtsA.jsp";
        const CALLBACK: &str =
            "https://my.bupt.edu.cn/system/resource/code/auth/clogin.jsp?ticket=ST-TEST-PORTAL";
        let callback =
            Url::parse("https://my.bupt.edu.cn/system/resource/code/auth/clogin.jsp").unwrap();
        let login_redirect = portal_scope::cas_url(&callback);
        let home = tempfile::tempdir().unwrap();
        let mut tr = Scripted::default();
        let mut queue = |method, url: &str, status, fields: &[(&str, &str)], body: &str| {
            tr.expected.push_back((
                method,
                url.into(),
                Ok(Response {
                    status,
                    headers: fields
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .collect(),
                    body: body.as_bytes().to_vec(),
                }),
            ));
        };
        queue(
            "GET",
            CAS,
            200,
            &[],
            "<input name='execution' value='TEST-ONLY'>",
        );
        queue(
            "POST",
            CAS,
            302,
            &[
                ("Location", "https://uc.bupt.edu.cn/"),
                ("Set-Cookie", "CASTGC=TEST-ONLY; Secure; Path=/authserver"),
            ],
            "",
        );
        queue(
            "GET",
            VERIFY,
            302,
            &[(
                "Location",
                "https://ucloud.bupt.edu.cn/uclass?ticket=ST-TEST-UCLOUD",
            )],
            "",
        );
        queue("GET", CARD, 302, &[("Location", &login_redirect)], "");
        queue("GET", &login_redirect, 302, &[("Location", CALLBACK)], "");
        queue(
            "GET",
            CALLBACK,
            200,
            &[("Set-Cookie", "portal=TEST-ONLY; Secure; Path=/")],
            "landing",
        );
        queue(
            "GET",
            CARD,
            200,
            &[],
            r#"{"qkje":"12.00","oddfare":"3.00"}"#,
        );
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
        let card = super::super::commands()
            .into_iter()
            .find(|cmd| cmd.get_name() == "card")
            .unwrap()
            .get_matches_from(["card"]);
        assert!(super::super::run("card", &card, &mut ctx).is_ok());
        assert_eq!(
            std::fs::read(ctx.cache_dir.join("session.json")).unwrap(),
            cached
        );
        assert!(tr.expected.is_empty());
        assert_eq!(
            tr.calls.iter().filter(|call| call.method == "POST").count(),
            1
        );
        for (request, expected) in tr.calls[3..].iter().zip([
            None,
            Some("CASTGC=TEST-ONLY"),
            None,
            Some("portal=TEST-ONLY"),
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
}
