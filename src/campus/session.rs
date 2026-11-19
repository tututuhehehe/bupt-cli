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
//! The parent-owned CLI/session layer attaches a driver through
//! [`super::run_with_session`]. Until then the default driver reports the same
//! "not connected" upstream error as the migration build without touching the
//! network or local state.

use super::electricity;
use super::portal::{self, NoticeType};
use crate::cli::{CliError, CliResult};
use crate::transport::{Request, Response, Transport};

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
    PortalNoticeDetail,
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

    pub(super) fn portal_detail() -> Self {
        Self(RouteKind::PortalNoticeDetail)
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
            RouteKind::PortalNoticeDetail => {
                request.method == "GET"
                    && portal::validate_notice_url(&request.url).is_ok()
                    && request.headers.is_empty()
                    && request.body.is_empty()
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

    pub(crate) fn describe(self) -> &'static str {
        match self.0 {
            RouteKind::PortalNoticeList(_) => "portal notice list",
            RouteKind::PortalNoticeDetail => "portal notice detail",
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

fn pending() -> CliError {
    CliError::new(
        5,
        "upstream",
        "campus session driver is not connected in this migration build",
        "",
    )
}

/// Default driver used by the public command dispatcher until the parent
/// session layer attaches a real implementation.
pub(super) struct PendingSession;

impl SessionDriver for PendingSession {
    fn ensure(&mut self, _: Service) -> CliResult<()> {
        Err(pending())
    }

    fn send(&mut self, _: Route, _: Request) -> CliResult<Response> {
        Err(pending())
    }

    fn renew(&mut self, _: Service) -> CliResult<()> {
        Err(pending())
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

        let mut detail = portal_list_request(NoticeType::Notice);
        detail.url = "https://my.bupt.edu.cn/2026/a/page.htm".into();
        assert!(Route::portal_detail().accepts(&detail));
        detail.url = "https://example.invalid/2026/a/page.htm".into();
        assert!(!Route::portal_detail().accepts(&detail));
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
