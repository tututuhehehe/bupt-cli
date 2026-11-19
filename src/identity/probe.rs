//! Executable `diagnose` orchestration.
//!
//! Public reachability checks are anonymous. Authenticated checks only use an
//! owned, revision-bound native CAS session; they never submit a password and
//! never write token, cookie or session state back to disk. The transport is
//! injected by the parent dispatcher and every URL below is a fixed campus
//! endpoint.

use super::*;
use crate::transport::{Request, Response, Transport};
use serde_json::{Map, Value, json};
use std::time::Instant;
use url::Url;

const CAS_LOGIN_PAGE_URL: &str = "https://auth.bupt.edu.cn/authserver/login";
const UC_STATUS_URL: &str = "https://uc.bupt.edu.cn/api/uc/status";
const UCLOUD_TOKEN_URL: &str = "https://apiucloud.bupt.edu.cn/ykt-basics/oauth/token";
const UCLOUD_API: &str = "https://apiucloud.bupt.edu.cn";
const UCLOUD_BASIC_AUTH: &str = "Basic c3dvcmQ6c3dvcmRfc2VjcmV0";
const PORTAL_NOTICE_URL: &str =
    "http://my.bupt.edu.cn/list.jsp?totalpage=30&PAGENUM=0&urltype=tree.TreeTempUrl&wbtreeid=1154";
const PORTAL_CARD_URL: &str = "http://my.bupt.edu.cn/system/resource/app/cuser/getwxtsA.jsp";
const PORTAL_CARD_REFERER: &str =
    "http://my.bupt.edu.cn/xs_index.jsp?urltype=tree.TreeTempUrl&wbtreeid=1541";
const JWGL_DOM_SCHEDULE: &str = "https://jwgl.bupt.edu.cn/jsxsd/framework/xsMain_bjyddx.jsp";
const JWGL_DOM_PERSON: &str = "https://jwgl.bupt.edu.cn/jsxsd/framework/xsdPerson.jsp";
const JWXT_LOGIN: &str = "https://jwxt.bupt.edu.cn/caslogin.jsp";
const WIN_NOTICE: &str = "https://win.bupt.edu.cn/note.do";
const ELECTRICITY_LANDING: &str = "https://app.bupt.edu.cn/buptdf/wap/default/chong";
const ELECTRICITY_API: &str = "https://app.bupt.edu.cn/buptdf/wap/default";
const MOBILE_JWGL_BASE: &str = "https://jwglweixin.bupt.edu.cn";
const MOBILE_JWGL_LOGIN: &str = "https://jwglweixin.bupt.edu.cn/bjyddx/login";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

const MOBILE_PATHS: &[(&str, &str, &str)] = &[
    ("curriculum", "GET", "/bjyddx/student/curriculum"),
    ("schedule modes", "GET", "/bjyddx/Get_sjkbms"),
    ("teaching week", "GET", "/bjyddx/teachingWeek"),
    ("exam weeks", "GET", "/bjyddx/kaowu/get_kszc"),
    ("exam schedule", "GET", "/bjyddx/kaowu/getKsapcx"),
    (
        "exam course arrangements",
        "GET",
        "/bjyddx/kaowu/findKcksapcx",
    ),
    (
        "examination arrangement",
        "GET",
        "/bjyddx/student/examinationArrangement",
    ),
    ("notices", "GET", "/bjyddx/noticeList"),
    (
        "empty classrooms",
        "GET",
        "/bjyddx/todayClassrooms?campusId=04",
    ),
    ("current term", "GET", "/bjyddx/currentTerm"),
    ("score report (termGPA)", "POST", "/bjyddx/student/termGPA"),
];

struct ProbeRun<'a> {
    groups: &'a [String],
    rows: Vec<Value>,
    session: Option<super::OwnedCasSession>,
    cached_cas_cookies: bool,
    cas_ticket: Option<String>,
    ucloud: Option<(String, String)>,
    ucloud_refreshed: bool,
    mobile_token_active: bool,
    role: Option<Role>,
    started: Instant,
}

pub(super) fn run(matches: &ArgMatches, context: &mut Context<'_>) -> CliResult<Outcome> {
    let groups = super::requested_groups(matches)?;
    // Validate credentials before any network. A malformed credentials file is
    // an auth error, matching the existing contract.
    let profile = super::load_profile(&context.config_dir)?;
    let role = profile.as_ref().and_then(LocalProfile::role);
    let session = super::load_owned_cas_session(context).ok();
    let cached_cas_cookies = session
        .as_ref()
        .is_some_and(super::OwnedCasSession::has_usable_cookies);
    let mobile_token_active = role != Some(Role::Graduate)
        && session.as_ref().is_some_and(|session| {
            session
                .mobile_token()
                .is_some_and(|(_, expires_at)| expires_at.is_none_or(|expiry| expiry > unix_now()))
        });
    let cached_ucloud = session
        .as_ref()
        .and_then(super::OwnedCasSession::cached_ucloud_credentials);
    let mut probe = ProbeRun {
        groups: &groups,
        rows: Vec::new(),
        session,
        cached_cas_cookies,
        cas_ticket: None,
        ucloud: cached_ucloud,
        ucloud_refreshed: false,
        mobile_token_active,
        role,
        started: Instant::now(),
    };
    probe.execute(context.transport);
    Ok(Outcome {
        data: probe.report(context),
        code: 0,
        diagnostic: None,
    })
}

impl ProbeRun<'_> {
    fn execute(&mut self, transport: &mut dyn Transport) {
        // This sequence mirrors probe.py. Rows are sorted at the end, while
        // request order remains deterministic for the injected transport.
        self.run_public_checks(transport);
        self.run_cas(transport);
        self.prepare_ucloud(transport);
        self.run_portal(transport);
        self.run_electricity(transport);
        self.run_ucloud_rows(transport);
        self.run_mobile(transport);
    }

    fn wants(&self, group: &str) -> bool {
        self.groups.iter().any(|value| value == group)
    }

    fn run_public_checks(&mut self, transport: &mut dyn Transport) {
        // Public reachability, including rows that belong to other groups.
        if self.wants("public") {
            self.public_get(
                transport,
                "public",
                "CAS login page",
                CAS_LOGIN_PAGE_URL,
                &[200],
                "",
            );
        }
        if self.wants("other") {
            self.public_get(
                transport,
                "other",
                "UC status API",
                UC_STATUS_URL,
                &[200],
                "",
            );
        }
        if self.wants("public") {
            self.public_get(
                transport,
                "public",
                "UCloud OAuth token endpoint",
                UCLOUD_TOKEN_URL,
                &[401, 405],
                "GET without credentials should be rejected (401/405)",
            );
            self.public_get(
                transport,
                "public",
                "electricity landing page",
                ELECTRICITY_LANDING,
                &[200, 302],
                "302 redirects to CAS without a session",
            );
        }
        if self.wants("portal") {
            self.public_get(
                transport,
                "portal",
                "portal notice list (unauthed)",
                PORTAL_NOTICE_URL,
                &[],
                "my.bupt.edu.cn is expected to redirect to CAS",
            );
        }
        if self.wants("other") {
            self.public_get(
                transport,
                "other",
                "innovation platform notices",
                WIN_NOTICE,
                &[200, 302, 412],
                "412 is anti-bot protection; reachability only",
            );
        }
        if self.wants("jwgl_dom") {
            self.public_get(
                transport,
                "jwgl_dom",
                "DOM JWGL login page",
                JWGL_DOM_SCHEDULE,
                &[200, 302],
                "",
            );
            self.public_get(
                transport,
                "jwgl_dom",
                "DOM JWGL personal page",
                JWGL_DOM_PERSON,
                &[200, 302],
                "",
            );
            self.public_get(
                transport,
                "jwgl_dom",
                "legacy jwxt login page",
                JWXT_LOGIN,
                &[200, 302],
                "",
            );
        }
        if self.wants("mobile_jwgl") {
            self.public_get(
                transport,
                "mobile_jwgl",
                "mobile JWGL login endpoint",
                MOBILE_JWGL_LOGIN,
                &[200, 302, 400, 401, 405, 500],
                "POST-only in practice; GET 500 still proves reachability. Reachability only: this is the undergraduate academic system, so a graduate-student account has no data here even when the endpoint answers.",
            );
        }
    }

    fn public_get(
        &mut self,
        transport: &mut dyn Transport,
        group: &str,
        name: &str,
        url: &str,
        expected: &[u16],
        note: &str,
    ) {
        let request = Request {
            method: "GET",
            url: url.to_owned(),
            headers: base_headers(),
            body: Vec::new(),
        };
        let (result, latency) = timed_send(transport, request);
        match result {
            Err(error) => self.add(
                group,
                name,
                "GET",
                url,
                "fail",
                None,
                Some(latency),
                "",
                &transport_error(&error),
                note,
            ),
            Ok(response) => {
                let ok = if expected.is_empty() {
                    response.status < 400
                } else {
                    expected.contains(&response.status)
                };
                let evidence = format!("HTTP {}", response.status);
                let error = if ok {
                    String::new()
                } else {
                    status_error(response.status)
                };
                self.add(
                    group,
                    name,
                    "GET",
                    url,
                    if ok { "ok" } else { "fail" },
                    Some(response.status),
                    Some(latency),
                    &evidence,
                    &error,
                    note,
                );
            }
        }
    }

    fn run_cas(&mut self, transport: &mut dyn Transport) {
        if !self.wants("cas") {
            return;
        }
        if !self.cached_cas_cookies {
            self.add(
                "cas",
                "CAS service ticket",
                "GET",
                CAS_SERVICE_URL,
                "skipped",
                None,
                None,
                NO_SESSION,
                "",
                "",
            );
            return;
        }
        let Some(session) = self.session.as_mut() else {
            self.add(
                "cas",
                "CAS service ticket",
                "GET",
                CAS_SERVICE_URL,
                "skipped",
                None,
                None,
                NO_SESSION,
                "",
                "",
            );
            return;
        };
        match session.ticket_for(transport, super::CasService::UCloud) {
            Ok(ticket) => {
                self.cas_ticket = Some(ticket);
                self.add(
                    "cas",
                    "CAS service ticket",
                    "GET",
                    CAS_SERVICE_URL,
                    "ok",
                    None,
                    None,
                    "ticket issued",
                    "",
                    "ticket issued; session cookies are valid",
                );
            }
            Err(error) => self.add(
                "cas",
                "CAS service ticket",
                "GET",
                CAS_SERVICE_URL,
                "fail",
                None,
                None,
                "",
                &transport_error(&error),
                "ticket issuance does not authenticate again",
            ),
        }
    }

    fn prepare_ucloud(&mut self, transport: &mut dyn Transport) {
        if !self.wants("ucloud") {
            return;
        }
        let cached = self
            .session
            .as_ref()
            .and_then(super::OwnedCasSession::cached_ucloud_credentials);

        // A fresh token is already a verified, bound read credential. Reuse it
        // instead of spending a short-lived CAS ticket; only a missing or
        // expired token triggers the lazy exchange below.
        if let Some(pair) = cached.clone() {
            self.ucloud = Some(pair);
            self.add(
                "ucloud",
                "UCloud OAuth token exchange",
                "POST",
                UCLOUD_TOKEN_URL,
                "skipped",
                None,
                None,
                "cached UCloud token is fresh; not exchanged",
                "",
                "",
            );
            return;
        }

        if !self.cached_cas_cookies {
            self.ucloud = cached;
            self.add(
                "ucloud",
                "UCloud OAuth token exchange",
                "POST",
                UCLOUD_TOKEN_URL,
                "skipped",
                None,
                None,
                "no cached CAS session; run `bupt login`",
                "",
                "",
            );
            return;
        }

        let ticket = match self.cas_ticket.take() {
            Some(ticket) => ticket,
            None => {
                let Some(session) = self.session.as_mut() else {
                    self.ucloud = cached;
                    self.add(
                        "ucloud",
                        "UCloud OAuth token exchange",
                        "POST",
                        UCLOUD_TOKEN_URL,
                        "skipped",
                        None,
                        None,
                        NO_SESSION,
                        "",
                        "",
                    );
                    return;
                };
                match session.ticket_for(transport, super::CasService::UCloud) {
                    Ok(ticket) => ticket,
                    Err(_error) => {
                        // This is a local diagnostic fallback, not a login
                        // attempt. Keep the row skipped, matching probe.py: no
                        // ticket means no token exchange happened.
                        self.ucloud = cached;
                        self.add(
                            "ucloud",
                            "UCloud OAuth token exchange",
                            "POST",
                            UCLOUD_TOKEN_URL,
                            "skipped",
                            None,
                            None,
                            "CAS session did not issue a ticket",
                            "",
                            "",
                        );
                        return;
                    }
                }
            }
        };

        let Some(session) = self.session.as_mut() else {
            self.ucloud = cached;
            self.add(
                "ucloud",
                "UCloud OAuth token exchange",
                "POST",
                UCLOUD_TOKEN_URL,
                "fail",
                None,
                None,
                "",
                "auth: no owned CAS session",
                "",
            );
            return;
        };
        match session.exchange_ucloud_ticket(transport, &ticket) {
            Ok(()) => {
                let pair = self
                    .session
                    .as_ref()
                    .and_then(super::OwnedCasSession::cached_ucloud_credentials);
                if let Some(pair) = pair {
                    self.ucloud = Some(pair);
                    self.ucloud_refreshed = true;
                    self.add(
                        "ucloud",
                        "UCloud OAuth token exchange",
                        "POST",
                        UCLOUD_TOKEN_URL,
                        "ok",
                        Some(200),
                        None,
                        "HTTP 200",
                        "",
                        "",
                    );
                } else {
                    self.ucloud = cached;
                    self.add(
                        "ucloud",
                        "UCloud OAuth token exchange",
                        "POST",
                        UCLOUD_TOKEN_URL,
                        "fail",
                        Some(200),
                        None,
                        "",
                        "auth: UCloud token did not identify a user",
                        "",
                    );
                }
            }
            Err(error) => {
                self.ucloud = cached;
                self.add(
                    "ucloud",
                    "UCloud OAuth token exchange",
                    "POST",
                    UCLOUD_TOKEN_URL,
                    "fail",
                    None,
                    None,
                    "",
                    &transport_error(&error),
                    "",
                );
            }
        }
    }

    fn run_portal(&mut self, transport: &mut dyn Transport) {
        if !self.wants("portal") {
            return;
        }
        if !self.cached_cas_cookies {
            for (name, url) in [
                ("portal notice list", PORTAL_NOTICE_URL),
                ("portal campus-card widget", PORTAL_CARD_URL),
            ] {
                self.add(
                    "portal", name, "GET", url, "skipped", None, None, NO_SESSION, "", "",
                );
            }
            return;
        }
        self.authenticated_portal(transport, "portal notice list", PORTAL_NOTICE_URL, &[]);
        self.authenticated_portal(
            transport,
            "portal campus-card widget",
            PORTAL_CARD_URL,
            &[
                ("Referer", PORTAL_CARD_REFERER),
                ("X-Requested-With", "XMLHttpRequest"),
            ],
        );
    }

    fn authenticated_portal(
        &mut self,
        transport: &mut dyn Transport,
        name: &str,
        url: &str,
        extra: &[(&str, &str)],
    ) {
        let cookie = self
            .session
            .as_ref()
            .and_then(|session| session.cookie_header_for(url));
        let Some(cookie) = cookie else {
            self.add(
                "portal",
                name,
                "GET",
                url,
                "skipped",
                None,
                None,
                "no usable CAS cookies for this endpoint; run `bupt login`",
                "",
                "",
            );
            return;
        };
        let mut headers = base_headers();
        headers.push(("Cookie".into(), cookie));
        for (name, value) in extra {
            headers.push(((*name).into(), (*value).into()));
        }
        let request = Request {
            method: "GET",
            url: url.to_owned(),
            headers,
            body: Vec::new(),
        };
        let (result, latency) = timed_send(transport, request);
        match result {
            Err(error) => self.add(
                "portal",
                name,
                "GET",
                url,
                "fail",
                None,
                Some(latency),
                "",
                &transport_error(&error),
                "",
            ),
            Ok(response) => {
                let login_page = is_login_page(&response);
                if login_page || matches!(response.status, 401 | 403) {
                    self.add(
                        "portal",
                        name,
                        "GET",
                        url,
                        "fail",
                        Some(response.status),
                        Some(latency),
                        &format!("HTTP {}", response.status),
                        "auth: portal returned a login page or rejected the session",
                        "",
                    );
                } else if response.status == 200 {
                    self.add(
                        "portal",
                        name,
                        "GET",
                        url,
                        "ok",
                        Some(response.status),
                        Some(latency),
                        "HTTP 200",
                        "",
                        "",
                    );
                } else {
                    self.add(
                        "portal",
                        name,
                        "GET",
                        url,
                        "fail",
                        Some(response.status),
                        Some(latency),
                        &format!("HTTP {}", response.status),
                        &status_error(response.status),
                        "",
                    );
                }
            }
        }
    }

    fn run_electricity(&mut self, transport: &mut dyn Transport) {
        if !self.wants("electricity") {
            return;
        }
        if !self.cached_cas_cookies {
            self.skip_electricity("no cached CAS session; run `bupt login`");
            return;
        }

        let landing_cookie = self
            .session
            .as_ref()
            .and_then(|session| session.cookie_header_for(ELECTRICITY_LANDING));
        let Some(landing_cookie) = landing_cookie else {
            self.skip_electricity("no usable CAS cookies for this endpoint; run `bupt login`");
            return;
        };
        let request = Request {
            method: "GET",
            url: ELECTRICITY_LANDING.to_owned(),
            headers: headers_with_cookie(landing_cookie),
            body: Vec::new(),
        };
        let (result, latency) = timed_send(transport, request);
        match result {
            Err(error) => self.add(
                "electricity",
                "electricity session prime",
                "GET",
                ELECTRICITY_LANDING,
                "fail",
                None,
                Some(latency),
                "",
                &transport_error(&error),
                "GET /chong establishes app.bupt.edu.cn cookies",
            ),
            Ok(response) => {
                let ok = matches!(response.status, 200 | 302);
                let error = if ok {
                    String::new()
                } else {
                    status_error(response.status)
                };
                self.add(
                    "electricity",
                    "electricity session prime",
                    "GET",
                    ELECTRICITY_LANDING,
                    if ok { "ok" } else { "fail" },
                    Some(response.status),
                    Some(latency),
                    &format!("HTTP {}", response.status),
                    &error,
                    "GET /chong establishes app.bupt.edu.cn cookies",
                );
            }
        }

        let Some(cookie) = self
            .session
            .as_ref()
            .and_then(|session| session.cookie_header_for(ELECTRICITY_LANDING))
        else {
            self.skip_electricity("no usable CAS cookies for this endpoint; run `bupt login`");
            return;
        };
        let mut partment_id = String::new();
        let Some(part) = self.electricity_post(
            transport,
            "electricity buildings (/part)",
            "/part",
            &[("areaid", "2".to_owned())],
            &cookie,
        ) else {
            self.skip_electricity_dependents(
                &[
                    ("electricity floors (/floor)", "/floor"),
                    ("electricity rooms (/drom)", "/drom"),
                    ("electricity search (/search)", "/search"),
                ],
                "/part failed",
            );
            return;
        };
        let part_data = part
            .get("d")
            .and_then(|d| d.get("data"))
            .and_then(Value::as_array);
        if let Some(first) = part_data
            .and_then(|data| data.first())
            .and_then(Value::as_object)
        {
            partment_id = value_string(first.get("partmentId").or_else(|| first.get("partmentid")))
                .unwrap_or_default();
        }
        if part_data.is_none() || partment_id.is_empty() {
            let reason = if part_data.is_none() {
                "/part did not return building data"
            } else {
                "no partmentId in /part response"
            };
            self.mark_electricity_failure(
                "electricity buildings (/part)",
                reason,
                "upstream: electricity /part did not return building data",
            );
            self.skip_electricity_dependents(
                &[
                    ("electricity floors (/floor)", "/floor"),
                    ("electricity rooms (/drom)", "/drom"),
                    ("electricity search (/search)", "/search"),
                ],
                reason,
            );
            return;
        }

        let Some(floor) = self.electricity_post(
            transport,
            "electricity floors (/floor)",
            "/floor",
            &[
                ("partmentId", partment_id.clone()),
                ("areaid", "2".to_owned()),
            ],
            &cookie,
        ) else {
            self.skip_electricity_dependents(
                &[
                    ("electricity rooms (/drom)", "/drom"),
                    ("electricity search (/search)", "/search"),
                ],
                "/floor failed",
            );
            return;
        };
        let floor_data = floor
            .get("d")
            .and_then(|d| d.get("data"))
            .and_then(Value::as_array);
        let floor_id = floor_data
            .and_then(|data| data.first())
            .and_then(Value::as_object)
            .and_then(|first| value_string(first.get("floorId")))
            .unwrap_or_default();
        if floor_data.is_none() || floor_id.is_empty() {
            let reason = if floor_data.is_none() {
                "/floor did not return floor data"
            } else {
                "no floorId in /floor response"
            };
            self.mark_electricity_failure(
                "electricity floors (/floor)",
                reason,
                "upstream: electricity /floor did not return floor data",
            );
            self.skip_electricity_dependents(
                &[
                    ("electricity rooms (/drom)", "/drom"),
                    ("electricity search (/search)", "/search"),
                ],
                reason,
            );
            return;
        }

        let Some(drom) = self.electricity_post(
            transport,
            "electricity rooms (/drom)",
            "/drom",
            &[
                ("partmentId", partment_id.clone()),
                ("floorId", floor_id.clone()),
                ("areaid", "2".to_owned()),
            ],
            &cookie,
        ) else {
            self.skip_electricity_dependents(
                &[("electricity search (/search)", "/search")],
                "/drom failed",
            );
            return;
        };
        let drom_data = drom
            .get("d")
            .and_then(|d| d.get("data"))
            .and_then(Value::as_array);
        if drom_data.is_none() {
            self.mark_electricity_failure(
                "electricity rooms (/drom)",
                "/drom did not return room data",
                "upstream: electricity /drom did not return room data",
            );
            self.skip_electricity_dependents(
                &[("electricity search (/search)", "/search")],
                "/drom did not return room data",
            );
            return;
        }
        let room_number = drom_data
            .and_then(|data| data.first())
            .and_then(Value::as_object)
            .and_then(|first| value_string(first.get("dromNum").or_else(|| first.get("dromName"))))
            .unwrap_or_default();

        let _ = self.electricity_post(
            transport,
            "electricity search (/search)",
            "/search",
            &[
                ("areaid", "2".to_owned()),
                ("partmentId", partment_id),
                ("floorId", floor_id),
                ("dromNumber", room_number),
            ],
            &cookie,
        );
    }

    fn electricity_post(
        &mut self,
        transport: &mut dyn Transport,
        name: &str,
        path: &str,
        fields: &[(&str, String)],
        cookie: &str,
    ) -> Option<Value> {
        let url = format!("{ELECTRICITY_API}{path}");
        let body = form_body(fields);
        let mut headers = headers_with_cookie(cookie.to_owned());
        headers.push((
            "Content-Type".into(),
            "application/x-www-form-urlencoded".into(),
        ));
        headers.push(("Referer".into(), ELECTRICITY_LANDING.into()));
        headers.push(("X-Requested-With".into(), "XMLHttpRequest".into()));
        let request = Request {
            method: "POST",
            url: url.clone(),
            headers,
            body,
        };
        let (result, latency) = timed_send(transport, request);
        match result {
            Err(error) => {
                self.add(
                    "electricity",
                    name,
                    "POST",
                    &url,
                    "fail",
                    None,
                    Some(latency),
                    "",
                    &transport_error(&error),
                    "",
                );
                None
            }
            Ok(response) => {
                if matches!(response.status, 401 | 403) {
                    self.add(
                        "electricity",
                        name,
                        "POST",
                        &url,
                        "fail",
                        Some(response.status),
                        Some(latency),
                        &format!("HTTP {}", response.status),
                        "auth: electricity endpoint rejected the session",
                        "",
                    );
                    return None;
                }
                if !(200..300).contains(&response.status) {
                    self.add(
                        "electricity",
                        name,
                        "POST",
                        &url,
                        "fail",
                        Some(response.status),
                        Some(latency),
                        &format!("HTTP {}", response.status),
                        &status_error(response.status),
                        "",
                    );
                    return None;
                }
                let Ok(payload) = serde_json::from_slice::<Value>(&response.body) else {
                    self.add(
                        "electricity",
                        name,
                        "POST",
                        &url,
                        "fail",
                        Some(response.status),
                        Some(latency),
                        &format!("HTTP {}", response.status),
                        "upstream: electricity response was not valid JSON",
                        "",
                    );
                    return None;
                };
                let code = payload.get("e").and_then(number_or_string);
                if code.as_deref() != Some("0") {
                    let error = if matches!(code.as_deref(), Some("10013" | "401" | "403")) {
                        "auth: electricity session is no longer valid"
                    } else {
                        "upstream: electricity API rejected the request"
                    };
                    self.add(
                        "electricity",
                        name,
                        "POST",
                        &url,
                        "fail",
                        Some(response.status),
                        Some(latency),
                        &format!(
                            "HTTP {}, api_code={}",
                            response.status,
                            code.as_deref().unwrap_or("?")
                        ),
                        error,
                        "",
                    );
                    return None;
                }
                self.add(
                    "electricity",
                    name,
                    "POST",
                    &url,
                    "ok",
                    Some(response.status),
                    Some(latency),
                    &format!("HTTP {}", response.status),
                    "",
                    "",
                );
                Some(payload)
            }
        }
    }

    fn mark_electricity_failure(&mut self, name: &str, evidence: &str, error: &str) {
        for row in self.rows.iter_mut().rev() {
            if row.get("group").and_then(Value::as_str) != Some("electricity")
                || row.get("name").and_then(Value::as_str) != Some(name)
            {
                continue;
            }
            if let Some(object) = row.as_object_mut() {
                object.insert("status".into(), json!("fail"));
                object.insert("evidence".into(), json!(evidence));
                object.insert("error".into(), json!(error));
                return;
            }
        }
    }

    fn skip_electricity_dependents(&mut self, rows: &[(&str, &str)], reason: &str) {
        for (name, path) in rows {
            let url = format!("{ELECTRICITY_API}{path}");
            self.add(
                "electricity",
                name,
                "POST",
                &url,
                "skipped",
                None,
                None,
                reason,
                "",
                "",
            );
        }
    }

    fn skip_electricity(&mut self, reason: &str) {
        for (name, path) in [
            ("electricity buildings (/part)", "/part"),
            ("electricity floors (/floor)", "/floor"),
            ("electricity rooms (/drom)", "/drom"),
            ("electricity search (/search)", "/search"),
        ] {
            let url = format!("{ELECTRICITY_API}{path}");
            self.add(
                "electricity",
                name,
                "POST",
                &url,
                "skipped",
                None,
                None,
                reason,
                "",
                "",
            );
        }
    }

    fn run_ucloud_rows(&mut self, transport: &mut dyn Transport) {
        if !self.wants("ucloud") {
            return;
        }
        let Some((token, user_id)) = self.ucloud.clone() else {
            self.skip_ucloud("no usable UCloud token; run `bupt login`");
            return;
        };

        let undone = self.ucloud_get(
            transport,
            "undone homework",
            &format!("{UCLOUD_API}/ykt-site/site/student/undone"),
            &[("userId", user_id.clone())],
            &token,
            "",
        );
        let mut assignment_id = undone
            .as_ref()
            .and_then(|undone| undone.get("data"))
            .and_then(|data| data.get("undoneList"))
            .and_then(Value::as_array)
            .and_then(|records| records.first())
            .and_then(Value::as_object)
            .and_then(|first| value_string(first.get("activityId").or_else(|| first.get("id"))))
            .unwrap_or_default();

        let courses = self.ucloud_get(
            transport,
            "current course sites",
            &format!("{UCLOUD_API}/ykt-site/site/list/student/current"),
            &[("size", "20".to_owned()), ("current", "1".to_owned())],
            &token,
            "",
        );
        let (site_id, activity_id) = courses
            .as_ref()
            .and_then(|courses| courses.get("data"))
            .and_then(|data| data.get("records"))
            .and_then(Value::as_array)
            .and_then(|records| records.first())
            .and_then(Value::as_object)
            .map(|first| {
                (
                    value_string(first.get("siteId").or_else(|| first.get("id")))
                        .unwrap_or_default(),
                    value_string(first.get("activityId")).unwrap_or_default(),
                )
            })
            .unwrap_or_default();
        if assignment_id.is_empty() {
            assignment_id = activity_id;
        }

        if site_id.is_empty() {
            self.skip_ucloud_row(
                "course homework list",
                "POST",
                &format!("{UCLOUD_API}/ykt-site/work/student/list"),
                "no course id available",
            );
        } else {
            let body = serde_json::to_vec(&json!({
                "siteId": site_id,
                "userId": user_id,
                "current": 1,
                "size": 20,
            }))
            .unwrap_or_default();
            self.ucloud_post(
                transport,
                "course homework list",
                &format!("{UCLOUD_API}/ykt-site/work/student/list"),
                &token,
                body,
                "",
            );
        }
        if assignment_id.is_empty() {
            self.skip_ucloud_row(
                "homework detail",
                "GET",
                &format!("{UCLOUD_API}/ykt-site/work/detail"),
                "no assignment id available",
            );
        } else {
            self.ucloud_get(
                transport,
                "homework detail",
                &format!("{UCLOUD_API}/ykt-site/work/detail"),
                &[("assignmentId", assignment_id)],
                &token,
                "",
            );
        }
        self.ucloud_get(
            transport,
            "user info",
            &format!("{UCLOUD_API}/ykt-basics/info"),
            &[],
            &token,
            "",
        );
        self.ucloud_get(
            transport,
            "current term",
            &format!("{UCLOUD_API}/ykt-site/base-term/current"),
            &[],
            &token,
            "",
        );
        self.ucloud_get(
            transport,
            "user role/domain/department",
            &format!("{UCLOUD_API}/ykt-basics/userroledomaindept/listByUserId"),
            &[("userId", user_id.clone())],
            &token,
            "",
        );
        if site_id.is_empty() {
            self.skip_ucloud_row(
                "course examinations",
                "GET",
                &format!("{UCLOUD_API}/ykt-site/examination/list-stu"),
                "no course id available",
            );
            self.skip_ucloud_row(
                "course surveys/todos",
                "GET",
                &format!("{UCLOUD_API}/ykt-activity/survey/page/todo"),
                "no course id available",
            );
        } else {
            self.ucloud_get(
                transport,
                "course examinations",
                &format!("{UCLOUD_API}/ykt-site/examination/list-stu"),
                &[
                    ("current", "1".to_owned()),
                    ("size", "1".to_owned()),
                    ("status", "-1".to_owned()),
                    ("siteId", site_id.clone()),
                    ("state", "-1".to_owned()),
                ],
                &token,
                "",
            );
            self.ucloud_get(
                transport,
                "course surveys/todos",
                &format!("{UCLOUD_API}/ykt-activity/survey/page/todo"),
                &[
                    ("level", "1".to_owned()),
                    ("size", "1".to_owned()),
                    ("userId", user_id),
                    ("siteId", site_id),
                ],
                &token,
                "",
            );
        }
    }

    fn ucloud_get(
        &mut self,
        transport: &mut dyn Transport,
        name: &str,
        base: &str,
        query: &[(&str, String)],
        token: &str,
        skip: &str,
    ) -> Option<Value> {
        self.ucloud_request(transport, name, "GET", base, query, token, None, skip)
    }

    fn ucloud_post(
        &mut self,
        transport: &mut dyn Transport,
        name: &str,
        base: &str,
        token: &str,
        body: Vec<u8>,
        skip: &str,
    ) {
        let _ = self.ucloud_request(transport, name, "POST", base, &[], token, Some(body), skip);
    }

    fn ucloud_request(
        &mut self,
        transport: &mut dyn Transport,
        name: &str,
        method: &'static str,
        base: &str,
        query: &[(&str, String)],
        token: &str,
        body: Option<Vec<u8>>,
        skip: &str,
    ) -> Option<Value> {
        if !skip.is_empty() {
            self.skip_ucloud_row(name, method, base, skip);
            return None;
        }
        let url = query_url(base, query);
        let mut headers = ucloud_headers(token);
        if body.is_some() {
            headers.push((
                "Content-Type".into(),
                "application/json;charset=UTF-8".into(),
            ));
        }
        let request = Request {
            method,
            url: url.clone(),
            headers,
            body: body.unwrap_or_default(),
        };
        let (result, latency) = timed_send(transport, request);
        match result {
            Err(error) => {
                self.add(
                    "ucloud",
                    name,
                    method,
                    base,
                    "fail",
                    None,
                    Some(latency),
                    "",
                    &transport_error(&error),
                    "",
                );
                None
            }
            Ok(response) => {
                let payload = serde_json::from_slice::<Value>(&response.body).ok();
                if let Some(payload) = payload {
                    if ucloud_api_ok(&payload) {
                        let mut evidence = format!("HTTP {}", response.status);
                        if let Some(code) = payload.get("code") {
                            evidence.push_str(&format!(", api_code={}", scalar_text(code)));
                        }
                        if let Some(data) = payload.get("data").and_then(Value::as_array) {
                            evidence.push_str(&format!(", items={}", data.len()));
                        }
                        self.add(
                            "ucloud",
                            name,
                            method,
                            base,
                            "ok",
                            Some(response.status),
                            Some(latency),
                            &evidence,
                            "",
                            "",
                        );
                        return Some(payload);
                    }
                    let code = payload.get("code");
                    let error = if matches!(
                        code.and_then(number_or_string).as_deref(),
                        Some("401" | "403")
                    ) || matches!(response.status, 401 | 403)
                    {
                        "auth: UCloud API rejected the token"
                    } else {
                        "upstream: UCloud API rejected the request"
                    };
                    self.add(
                        "ucloud",
                        name,
                        method,
                        base,
                        "fail",
                        Some(response.status),
                        Some(latency),
                        &format!(
                            "HTTP {}{}",
                            response.status,
                            code.map(|value| format!(", api_code={}", scalar_text(value)))
                                .unwrap_or_default()
                        ),
                        error,
                        "",
                    );
                    None
                } else {
                    self.add(
                        "ucloud",
                        name,
                        method,
                        base,
                        "fail",
                        Some(response.status),
                        Some(latency),
                        &format!("HTTP {}", response.status),
                        if matches!(response.status, 401 | 403) {
                            "auth: UCloud API rejected the token"
                        } else {
                            "upstream: UCloud response was not valid JSON"
                        },
                        "",
                    );
                    None
                }
            }
        }
    }

    fn skip_ucloud(&mut self, reason: &str) {
        for (name, method, path) in [
            ("undone homework", "GET", "/ykt-site/site/student/undone"),
            (
                "current course sites",
                "GET",
                "/ykt-site/site/list/student/current",
            ),
            (
                "course homework list",
                "POST",
                "/ykt-site/work/student/list",
            ),
            ("homework detail", "GET", "/ykt-site/work/detail"),
            ("user info", "GET", "/ykt-basics/info"),
            ("current term", "GET", "/ykt-site/base-term/current"),
            (
                "user role/domain/department",
                "GET",
                "/ykt-basics/userroledomaindept/listByUserId",
            ),
            (
                "course examinations",
                "GET",
                "/ykt-site/examination/list-stu",
            ),
            (
                "course surveys/todos",
                "GET",
                "/ykt-activity/survey/page/todo",
            ),
        ] {
            self.skip_ucloud_row(name, method, &format!("{UCLOUD_API}{path}"), reason);
        }
    }

    fn skip_ucloud_row(&mut self, name: &str, method: &str, url: &str, reason: &str) {
        self.add(
            "ucloud", name, method, url, "skipped", None, None, reason, "", "",
        );
    }

    fn run_mobile(&mut self, transport: &mut dyn Transport) {
        if !self.wants("mobile_jwgl") {
            return;
        }
        let reason = if self.role == Some(Role::Graduate) {
            "graduate account has no undergraduate mobile JWGL data"
        } else if self.session.is_none() {
            "no usable owned session; run `bupt login`"
        } else if !self.mobile_token_active {
            "no cached mobile JWGL token; diagnose never submits JWGL credentials"
        } else {
            ""
        };
        let token = if reason.is_empty() {
            self.session
                .as_ref()
                .and_then(super::OwnedCasSession::mobile_token)
                .map(|(token, _)| token)
        } else {
            None
        };
        for (name, method, path) in MOBILE_PATHS {
            let url = format!("{MOBILE_JWGL_BASE}{path}");
            if reason.is_empty() {
                if let Some(token) = token.as_deref() {
                    self.mobile_request(transport, name, method, &url, token);
                    continue;
                }
            }
            self.add(
                "mobile_jwgl",
                name,
                method,
                &url,
                "skipped",
                None,
                None,
                reason,
                "",
                "",
            );
        }
    }

    fn mobile_request(
        &mut self,
        transport: &mut dyn Transport,
        name: &str,
        method: &'static str,
        url: &str,
        token: &str,
    ) {
        let headers = vec![
            ("User-Agent".into(), USER_AGENT.into()),
            ("token".into(), token.into()),
            ("Accept".into(), "application/json, text/plain, */*".into()),
        ];
        let request = Request {
            method,
            url: url.to_owned(),
            headers,
            body: Vec::new(),
        };
        let (result, latency) = timed_send(transport, request);
        match result {
            Err(error) => self.add(
                "mobile_jwgl",
                name,
                method,
                url,
                "fail",
                None,
                Some(latency),
                "",
                &transport_error(&error),
                "",
            ),
            Ok(response) => {
                let payload = serde_json::from_slice::<Value>(&response.body).ok();
                if payload.as_ref().is_some_and(mobile_api_ok) {
                    self.add(
                        "mobile_jwgl",
                        name,
                        method,
                        url,
                        "ok",
                        Some(response.status),
                        Some(latency),
                        &format!("HTTP {}", response.status),
                        "",
                        "",
                    );
                } else {
                    let error = if matches!(response.status, 401 | 403)
                        || payload
                            .as_ref()
                            .and_then(|value| value.get("code"))
                            .and_then(number_or_string)
                            .as_deref()
                            == Some("401")
                    {
                        "auth: mobile JWGL rejected the cached token"
                    } else {
                        "upstream: mobile JWGL response was not accepted"
                    };
                    self.add(
                        "mobile_jwgl",
                        name,
                        method,
                        url,
                        "fail",
                        Some(response.status),
                        Some(latency),
                        &format!("HTTP {}", response.status),
                        error,
                        "",
                    );
                }
            }
        }
    }

    fn add(
        &mut self,
        group: &str,
        name: &str,
        method: &str,
        url: &str,
        status: &str,
        http_code: Option<u16>,
        latency_ms: Option<u64>,
        evidence: &str,
        error: &str,
        notes: &str,
    ) {
        let mut row = Map::new();
        row.insert("group".into(), json!(group));
        row.insert("name".into(), json!(name));
        row.insert("method".into(), json!(method));
        row.insert("url".into(), json!(url));
        row.insert("status".into(), json!(status));
        if let Some(http_code) = http_code {
            row.insert("http_code".into(), json!(http_code));
        }
        if let Some(latency_ms) = latency_ms {
            row.insert("latency_ms".into(), json!(latency_ms));
        }
        if !evidence.is_empty() {
            row.insert("evidence".into(), json!(evidence));
        }
        if !error.is_empty() {
            row.insert("error".into(), json!(error));
        }
        if !notes.is_empty() {
            row.insert("notes".into(), json!(notes));
        }
        self.rows.push(Value::Object(row));
    }

    fn report(mut self, context: &Context<'_>) -> Value {
        self.rows.sort_by(|left, right| {
            let left_group = left.get("group").and_then(Value::as_str).unwrap_or("");
            let right_group = right.get("group").and_then(Value::as_str).unwrap_or("");
            let left_index = DIAGNOSE_GROUPS
                .iter()
                .position(|group| *group == left_group)
                .unwrap_or(DIAGNOSE_GROUPS.len());
            let right_index = DIAGNOSE_GROUPS
                .iter()
                .position(|group| *group == right_group)
                .unwrap_or(DIAGNOSE_GROUPS.len());
            left_index.cmp(&right_index).then_with(|| {
                left.get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .cmp(right.get("name").and_then(Value::as_str).unwrap_or(""))
            })
        });
        let mut ok = 0;
        let mut fail = 0;
        let mut skipped = 0;
        for row in &self.rows {
            match row.get("status").and_then(Value::as_str) {
                Some("ok") => ok += 1,
                Some("fail") => fail += 1,
                Some("skipped") => skipped += 1,
                _ => {}
            }
        }
        let duration = (self.started.elapsed().as_secs_f64() * 100.0).round() / 100.0;
        json!({
            "generated_at": iso_epoch(unix_now()).unwrap_or_default(),
            "duration_s": duration,
            "session_path": path_string(&context.cache_dir.join(SESSION_FILENAME)),
            "auth": {
                "cached_cas_cookies": self.cached_cas_cookies,
                "ucloud_token": self.ucloud.is_some(),
                "ucloud_token_refreshed": self.ucloud_refreshed,
                "mobile_jwgl_token": self.mobile_token_active,
            },
            "summary": {
                "ok": ok,
                "fail": fail,
                "skipped": skipped,
                "total": self.rows.len(),
            },
            "endpoints": self.rows,
        })
    }
}

const NO_SESSION: &str = "no cached CAS session; run `bupt login`";

fn timed_send(transport: &mut dyn Transport, request: Request) -> (CliResult<Response>, u64) {
    let started = Instant::now();
    let result = transport.send(request);
    (result, started.elapsed().as_millis() as u64)
}

fn base_headers() -> Vec<(String, String)> {
    vec![("User-Agent".into(), USER_AGENT.into())]
}

fn headers_with_cookie(cookie: String) -> Vec<(String, String)> {
    let mut headers = base_headers();
    headers.push(("Cookie".into(), cookie));
    headers
}

fn ucloud_headers(token: &str) -> Vec<(String, String)> {
    vec![
        ("User-Agent".into(), USER_AGENT.into()),
        ("Blade-Auth".into(), token.into()),
        ("Tenant-Id".into(), "000000".into()),
        ("Authorization".into(), UCLOUD_BASIC_AUTH.into()),
    ]
}

fn transport_error(error: &CliError) -> String {
    let category = match error.kind {
        "auth" => "auth",
        "network" => "unreachable",
        _ => "upstream",
    };
    format!("{category}: request failed")
}

fn status_error(status: u16) -> String {
    if matches!(status, 401 | 403) {
        "auth: request rejected authentication".to_owned()
    } else {
        "upstream: unexpected HTTP status".to_owned()
    }
}

fn query_url(base: &str, query: &[(&str, String)]) -> String {
    let Ok(mut url) = Url::parse(base) else {
        return base.to_owned();
    };
    if !query.is_empty() {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in query {
            pairs.append_pair(key, value);
        }
    }
    url.into()
}

fn form_body(fields: &[(&str, String)]) -> Vec<u8> {
    let mut body = String::new();
    for (index, (key, value)) in fields.iter().enumerate() {
        if index > 0 {
            body.push('&');
        }
        body.push_str(&encode_form(key));
        body.push('=');
        body.push_str(&encode_form(value));
    }
    body.into_bytes()
}

fn encode_form(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn value_string(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Number(value) => value.to_string(),
        _ => "?".into(),
    }
}

fn number_or_string(value: &Value) -> Option<String> {
    match value {
        Value::Number(value) => Some(value.to_string()),
        Value::String(value) => Some(value.clone()),
        _ => None,
    }
}

fn ucloud_api_ok(payload: &Value) -> bool {
    if payload.get("success").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    matches!(
        payload.get("code").and_then(number_or_string).as_deref(),
        Some("0" | "200")
    )
}

fn mobile_api_ok(payload: &Value) -> bool {
    if payload.get("success").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    payload.get("code").and_then(number_or_string).as_deref() == Some("1")
}

fn is_login_page(response: &Response) -> bool {
    let location = response
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("location"))
        .map(|(_, value)| value.to_ascii_lowercase())
        .unwrap_or_default();
    if location.contains("authserver/login")
        || location.contains("caslogin.jsp")
        || location.contains("/sso/")
    {
        return true;
    }
    let body = String::from_utf8_lossy(&response.body);
    let text = body
        .get(..body.len().min(30_000))
        .unwrap_or(&body)
        .to_ascii_lowercase();
    [
        "authserver/login",
        "clogin.jsp",
        "name=\"execution\"",
        "name='execution'",
        "logintoxk",
        "统一身份认证",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::testing::Scripted;
    use cookie_store::CookieStore;
    use serde_json::{Value, json};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::Duration;
    use tempfile::{TempDir, tempdir};

    const USER: &str = "2026000001";
    const PASSWORD: &str = "TEST-ONLY-PROBE-PASSWORD-7a1b";
    const REVISION: &str = "TEST-ONLY-PROBE-REVISION-2c3d";
    const COOKIE: &str = "TEST-ONLY-PROBE-CAS-COOKIE-4e5f";
    const ACCESS: &str = "TEST-ONLY-PROBE-ACCESS-TOKEN-6a7b";
    const TICKET_URL: &str =
        "https://ucloud.bupt.edu.cn/uclass?ticket=ST-TEST-ONLY-PROBE-TICKET-8c9d";

    struct Sandbox {
        root: TempDir,
    }

    impl Sandbox {
        fn new() -> Self {
            Self {
                root: tempdir().expect("create synthetic probe root"),
            }
        }

        fn config_dir(&self) -> PathBuf {
            self.root.path().join("config")
        }

        fn cache_dir(&self) -> PathBuf {
            self.root.path().join("cache")
        }

        fn context<'a>(&self, transport: &'a mut Scripted) -> Context<'a> {
            Context {
                home_dir: self.root.path().join("home"),
                config_dir: self.config_dir(),
                cache_dir: self.cache_dir(),
                transport,
            }
        }
    }

    fn matches(groups: &str) -> ArgMatches {
        super::super::commands()
            .into_iter()
            .find(|command| command.get_name() == "diagnose")
            .expect("diagnose command")
            .try_get_matches_from(["diagnose", "--only", groups])
            .expect("valid diagnose arguments")
    }

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

    fn enqueue(transport: &mut Scripted, method: &'static str, url: &str, response: Response) {
        transport
            .expected
            .push_back((method, url.to_owned(), Ok(response)));
    }

    fn cookies_json(entries: &[(&str, &str)]) -> Value {
        let mut store = CookieStore::default();
        for (origin, raw) in entries {
            let url = Url::parse(origin).expect("parse synthetic cookie origin");
            store
                .parse(raw, &url)
                .expect("insert synthetic cookie into maintained store");
        }
        let mut bytes = Vec::new();
        cookie_store::serde::json::save_incl_expired_and_nonpersistent(&store, &mut bytes)
            .expect("serialize synthetic cookie store");
        serde_json::from_slice(&bytes).expect("parse synthetic cookie store JSON")
    }

    fn native_cookie() -> Value {
        let raw = format!("CASTGC={COOKIE}; Path=/authserver; Secure; HttpOnly");
        cookies_json(&[("https://auth.bupt.edu.cn/authserver/login", raw.as_str())])
    }

    fn write_private(path: &Path, value: &Value) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create synthetic directory");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                    .expect("secure synthetic directory");
            }
        }
        let bytes = serde_json::to_vec_pretty(value).expect("serialize synthetic state");
        fs::write(path, bytes).expect("write synthetic state");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .expect("secure synthetic file");
        }
    }

    fn seed_native(sandbox: &Sandbox, ucloud: Option<Value>) {
        write_private(
            &sandbox.config_dir().join(CREDENTIALS_FILENAME),
            &json!({
                "username": USER,
                "password": PASSWORD,
                "role": "graduate",
                "session_revision": REVISION,
            }),
        );
        let mut session = json!({
            "version": 2,
            "username": USER,
            "role": "graduate",
            "session_revision": REVISION,
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-02T03:04:05Z",
            "cookies": native_cookie(),
            "cas_logins_total": 1,
        });
        if let Some(ucloud) = ucloud {
            session["ucloud"] = ucloud;
        }
        write_private(&sandbox.cache_dir().join(SESSION_FILENAME), &session);
    }

    fn bytes(path: &Path) -> Vec<u8> {
        fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
    }

    #[test]
    fn public_group_is_independent_and_uses_only_its_fixed_endpoints() {
        let sandbox = Sandbox::new();
        let mut transport = Scripted::default();
        enqueue(
            &mut transport,
            "GET",
            CAS_LOGIN_PAGE_URL,
            response(200, &[], "synthetic login page"),
        );
        enqueue(
            &mut transport,
            "GET",
            UCLOUD_TOKEN_URL,
            response(401, &[], ""),
        );
        enqueue(
            &mut transport,
            "GET",
            ELECTRICITY_LANDING,
            response(302, &[("Location", CAS_LOGIN_PAGE_URL)], ""),
        );

        let report = {
            let mut context = sandbox.context(&mut transport);
            super::run(&matches("public"), &mut context).expect("diagnose succeeds")
        };
        assert_eq!(report.code, 0);
        assert_eq!(transport.calls.len(), 3);
        assert_eq!(transport.calls[0].url, CAS_LOGIN_PAGE_URL);
        assert_eq!(transport.calls[1].url, UCLOUD_TOKEN_URL);
        assert_eq!(transport.calls[2].url, ELECTRICITY_LANDING);
        assert_eq!(report.data["summary"]["ok"], 3);
        assert_eq!(report.data["summary"]["fail"], 0);
        assert_eq!(report.data["auth"]["cached_cas_cookies"], false);
        assert!(!sandbox.config_dir().exists());
        assert!(!sandbox.cache_dir().exists());
    }

    #[test]
    fn missing_owned_session_skips_authenticated_rows_without_transport_calls() {
        let sandbox = Sandbox::new();
        let mut transport = Scripted::default();
        let report = {
            let mut context = sandbox.context(&mut transport);
            super::run(&matches("cas,electricity,ucloud"), &mut context).expect("diagnose succeeds")
        };
        assert!(transport.calls.is_empty());
        let rows = report.data["endpoints"].as_array().expect("endpoint rows");
        assert!(rows.iter().all(|row| row["status"] == "skipped"));
        assert_eq!(report.data["summary"]["skipped"], rows.len());
        assert!(!sandbox.config_dir().exists());
        assert!(!sandbox.cache_dir().exists());
    }

    #[test]
    fn cas_ticket_uses_owned_cookies_without_login_or_persistence() {
        let sandbox = Sandbox::new();
        seed_native(&sandbox, None);
        let credentials_path = sandbox.config_dir().join(CREDENTIALS_FILENAME);
        let session_path = sandbox.cache_dir().join(SESSION_FILENAME);
        let credentials_before = bytes(&credentials_path);
        let session_before = bytes(&session_path);

        let mut transport = Scripted::default();
        enqueue(
            &mut transport,
            "GET",
            CAS_SERVICE_URL,
            response(302, &[("Location", TICKET_URL)], ""),
        );
        let report = {
            let mut context = sandbox.context(&mut transport);
            super::run(&matches("cas"), &mut context).expect("diagnose succeeds")
        };

        assert_eq!(transport.calls.len(), 1);
        let call = &transport.calls[0];
        assert_eq!(call.method, "GET");
        assert_eq!(call.url, CAS_SERVICE_URL);
        assert!(
            call.headers
                .iter()
                .any(|(name, value)| name.eq_ignore_ascii_case("cookie") && value.contains(COOKIE))
        );
        assert!(
            !transport
                .calls
                .iter()
                .any(|call| call.method == "POST" && call.url == CAS_LOGIN_PAGE_URL)
        );
        assert_eq!(report.data["summary"]["ok"], 1);
        assert_eq!(report.data["auth"]["cached_cas_cookies"], true);
        assert_eq!(report.data["auth"]["ucloud_token"], false);
        assert_eq!(bytes(&credentials_path), credentials_before);
        assert_eq!(bytes(&session_path), session_before);
    }

    #[test]
    fn transport_failures_are_reported_as_auth_unreachable_or_upstream() {
        for (error, expected) in [
            (CliError::new(3, "auth", "synthetic", ""), "auth:"),
            (CliError::new(4, "network", "synthetic", ""), "unreachable:"),
            (CliError::new(5, "upstream", "synthetic", ""), "upstream:"),
        ] {
            let sandbox = Sandbox::new();
            seed_native(&sandbox, None);
            let credentials_path = sandbox.config_dir().join(CREDENTIALS_FILENAME);
            let session_path = sandbox.cache_dir().join(SESSION_FILENAME);
            let credentials_before = bytes(&credentials_path);
            let session_before = bytes(&session_path);
            let mut transport = Scripted::default();
            transport
                .expected
                .push_back(("GET", CAS_SERVICE_URL.to_owned(), Err(error)));

            let report = {
                let mut context = sandbox.context(&mut transport);
                super::run(&matches("cas"), &mut context).expect("diagnose succeeds")
            };
            let row = &report.data["endpoints"][0];
            assert_eq!(row["status"], "fail");
            assert!(
                row["error"]
                    .as_str()
                    .is_some_and(|value| value.starts_with(expected)),
                "unexpected error class: {row}"
            );
            assert_eq!(bytes(&credentials_path), credentials_before);
            assert_eq!(bytes(&session_path), session_before);
        }
    }

    fn broad_cookie() -> Value {
        cookies_json(&[
            (
                "https://auth.bupt.edu.cn/authserver/login",
                "CASTGC=TEST-ONLY-PROBE-BROAD-CAS; Path=/authserver; Secure; HttpOnly",
            ),
            (
                "https://auth.bupt.edu.cn/authserver/login",
                "PORTAL=TEST-ONLY-PROBE-PORTAL; Domain=bupt.edu.cn; Path=/",
            ),
        ])
    }

    #[test]
    fn cached_ucloud_token_is_reused_without_cas_or_disk_writes() {
        let sandbox = Sandbox::new();
        seed_native(
            &sandbox,
            Some(json!({
                "access_token": ACCESS,
                "refresh_token": "TEST-ONLY-PROBE-REFRESH",
                "user_id": "42",
                "expires_at": 4102444800.0,
            })),
        );
        let session_path = sandbox.cache_dir().join(SESSION_FILENAME);
        let mut session =
            serde_json::from_slice::<Value>(&bytes(&session_path)).expect("session JSON");
        session["cookies"] = json!([]);
        write_private(&session_path, &session);
        let session_before = bytes(&session_path);

        let undone = format!("{UCLOUD_API}/ykt-site/site/student/undone?userId=42");
        let courses = format!("{UCLOUD_API}/ykt-site/site/list/student/current?size=20&current=1");
        let courses_list = format!("{UCLOUD_API}/ykt-site/work/student/list");
        let detail = format!("{UCLOUD_API}/ykt-site/work/detail?assignmentId=a1");
        let info = format!("{UCLOUD_API}/ykt-basics/info");
        let term = format!("{UCLOUD_API}/ykt-site/base-term/current");
        let roles = format!("{UCLOUD_API}/ykt-basics/userroledomaindept/listByUserId?userId=42");
        let exams = format!(
            "{UCLOUD_API}/ykt-site/examination/list-stu?current=1&size=1&status=-1&siteId=s1&state=-1"
        );
        let surveys = format!(
            "{UCLOUD_API}/ykt-activity/survey/page/todo?level=1&size=1&userId=42&siteId=s1"
        );

        let mut transport = Scripted::default();
        enqueue(
            &mut transport,
            "GET",
            &undone,
            response(
                200,
                &[],
                "{\"code\":200,\"data\":{\"undoneList\":[{\"activityId\":\"a1\"}]}}",
            ),
        );
        enqueue(
            &mut transport,
            "GET",
            &courses,
            response(
                200,
                &[],
                "{\"code\":200,\"data\":{\"records\":[{\"siteId\":\"s1\"}]}}",
            ),
        );
        enqueue(
            &mut transport,
            "POST",
            &courses_list,
            response(200, &[], "{\"code\":200}"),
        );
        enqueue(
            &mut transport,
            "GET",
            &detail,
            response(200, &[], "{\"code\":200}"),
        );
        enqueue(
            &mut transport,
            "GET",
            &info,
            response(200, &[], "{\"code\":200}"),
        );
        enqueue(
            &mut transport,
            "GET",
            &term,
            response(200, &[], "{\"code\":200}"),
        );
        enqueue(
            &mut transport,
            "GET",
            &roles,
            response(200, &[], "{\"code\":200}"),
        );
        enqueue(
            &mut transport,
            "GET",
            &exams,
            response(200, &[], "{\"code\":200}"),
        );
        enqueue(
            &mut transport,
            "GET",
            &surveys,
            response(200, &[], "{\"code\":200}"),
        );

        let report = {
            let mut context = sandbox.context(&mut transport);
            super::run(&matches("ucloud"), &mut context).expect("diagnose succeeds")
        };
        assert_eq!(transport.calls.len(), 9);
        assert!(
            !transport
                .calls
                .iter()
                .any(|call| call.url == CAS_SERVICE_URL || call.url == UCLOUD_TOKEN_URL)
        );
        assert_eq!(report.data["auth"]["cached_cas_cookies"], false);
        assert_eq!(report.data["auth"]["ucloud_token"], true);
        assert_eq!(report.data["auth"]["ucloud_token_refreshed"], false);
        assert_eq!(report.data["summary"]["skipped"], 1);
        assert_eq!(report.data["summary"]["ok"], 9);
        assert_eq!(bytes(&session_path), session_before);
    }

    #[test]
    fn portal_and_electricity_use_only_their_owned_cookie_headers() {
        let sandbox = Sandbox::new();
        seed_native(&sandbox, None);
        let session_path = sandbox.cache_dir().join(SESSION_FILENAME);
        let mut session = serde_json::from_slice::<Value>(&bytes(&session_path)).expect("session");
        session["cookies"] = broad_cookie();
        write_private(&session_path, &session);
        let session_before = bytes(&session_path);

        let mut transport = Scripted::default();
        enqueue(
            &mut transport,
            "GET",
            PORTAL_NOTICE_URL,
            response(200, &[], "portal notice"),
        );
        enqueue(
            &mut transport,
            "GET",
            PORTAL_NOTICE_URL,
            response(200, &[], "portal notice"),
        );
        enqueue(
            &mut transport,
            "GET",
            PORTAL_CARD_URL,
            response(200, &[], "portal card"),
        );
        let report = {
            let mut context = sandbox.context(&mut transport);
            super::run(&matches("portal"), &mut context).expect("diagnose succeeds")
        };
        assert_eq!(transport.calls.len(), 3);
        assert!(
            transport.calls[0]
                .headers
                .iter()
                .all(|(name, _)| !name.eq_ignore_ascii_case("cookie"))
        );
        for call in &transport.calls[1..] {
            assert!(call.headers.iter().any(|(name, value)| {
                name.eq_ignore_ascii_case("cookie")
                    && value.contains("PORTAL=TEST-ONLY-PROBE-PORTAL")
                    && !value.contains("CASTGC=")
            }));
            assert!(!String::from_utf8_lossy(&call.body).contains(PASSWORD));
        }
        assert_eq!(report.data["summary"]["ok"], 3);
        assert_eq!(bytes(&session_path), session_before);

        let mut transport = Scripted::default();
        enqueue(
            &mut transport,
            "GET",
            ELECTRICITY_LANDING,
            response(200, &[], ""),
        );
        for (path, body) in [
            (
                "/part",
                "{\"e\":0,\"d\":{\"data\":[{\"partmentId\":\"p1\"}]}}",
            ),
            (
                "/floor",
                "{\"e\":0,\"d\":{\"data\":[{\"floorId\":\"f1\"}]}}",
            ),
            (
                "/drom",
                "{\"e\":0,\"d\":{\"data\":[{\"dromNum\":\"101\"}]}}",
            ),
            ("/search", "{\"e\":0,\"d\":{\"data\":[]}}"),
        ] {
            enqueue(
                &mut transport,
                "POST",
                &format!("{ELECTRICITY_API}{path}"),
                response(200, &[], body),
            );
        }
        let report = {
            let mut context = sandbox.context(&mut transport);
            super::run(&matches("electricity"), &mut context).expect("diagnose succeeds")
        };
        assert_eq!(transport.calls.len(), 5);
        for call in &transport.calls {
            assert!(call.headers.iter().any(|(name, value)| {
                name.eq_ignore_ascii_case("cookie")
                    && value.contains("PORTAL=TEST-ONLY-PROBE-PORTAL")
                    && !value.contains("CASTGC=")
            }));
            assert!(!String::from_utf8_lossy(&call.body).contains(PASSWORD));
        }
        assert_eq!(report.data["summary"]["ok"], 5);
        assert_eq!(bytes(&session_path), session_before);
    }

    #[test]
    fn mobile_diagnose_uses_a_cached_token_and_never_writes() {
        let sandbox = Sandbox::new();
        seed_native(&sandbox, None);
        let session_path = sandbox.cache_dir().join(SESSION_FILENAME);
        let mut session = serde_json::from_slice::<Value>(&bytes(&session_path)).expect("session");
        session["role"] = json!("undergraduate");
        session["cookies"] = json!([]);
        session["mobile_jwgl"] = json!({
            "token": "TEST-ONLY-PROBE-MOBILE-TOKEN",
            "expires_at": 4102444800.0,
        });
        write_private(&session_path, &session);
        let credentials_path = sandbox.config_dir().join(CREDENTIALS_FILENAME);
        let mut credentials =
            serde_json::from_slice::<Value>(&bytes(&credentials_path)).expect("credentials");
        credentials["role"] = json!("undergraduate");
        write_private(&credentials_path, &credentials);
        let credentials_before = bytes(&credentials_path);
        let session_before = bytes(&session_path);

        let mut transport = Scripted::default();
        enqueue(
            &mut transport,
            "GET",
            MOBILE_JWGL_LOGIN,
            response(200, &[], ""),
        );
        for (_, _, path) in MOBILE_PATHS {
            enqueue(
                &mut transport,
                "GET",
                &format!("{MOBILE_JWGL_BASE}{path}"),
                response(200, &[], "{\"code\":1}"),
            );
        }
        let report = {
            let mut context = sandbox.context(&mut transport);
            super::run(&matches("mobile_jwgl"), &mut context).expect("diagnose succeeds")
        };
        assert_eq!(transport.calls.len(), 1 + MOBILE_PATHS.len());
        assert_eq!(report.data["auth"]["mobile_jwgl_token"], true);
        for call in &transport.calls[1..] {
            assert!(
                call.headers
                    .iter()
                    .any(|(name, value)| name.eq_ignore_ascii_case("token")
                        && value == "TEST-ONLY-PROBE-MOBILE-TOKEN")
            );
            assert!(!String::from_utf8_lossy(&call.body).contains(PASSWORD));
        }
        assert_eq!(bytes(&credentials_path), credentials_before);
        assert_eq!(bytes(&session_path), session_before);
    }

    #[test]
    fn cas_http_statuses_are_layered_without_writing() {
        for (status, expected) in [
            (401, "auth:"),
            (403, "auth:"),
            (429, "upstream:"),
            (500, "upstream:"),
        ] {
            let sandbox = Sandbox::new();
            seed_native(&sandbox, None);
            let credentials_path = sandbox.config_dir().join(CREDENTIALS_FILENAME);
            let session_path = sandbox.cache_dir().join(SESSION_FILENAME);
            let credentials_before = bytes(&credentials_path);
            let session_before = bytes(&session_path);
            let mut transport = Scripted::default();
            enqueue(
                &mut transport,
                "GET",
                CAS_SERVICE_URL,
                response(status, &[], "synthetic failure"),
            );
            let report = {
                let mut context = sandbox.context(&mut transport);
                super::run(&matches("cas"), &mut context).expect("diagnose succeeds")
            };
            let row = &report.data["endpoints"][0];
            assert_eq!(row["status"], "fail");
            assert!(
                row["error"]
                    .as_str()
                    .is_some_and(|value| value.starts_with(expected)),
                "unexpected error class for HTTP {status}: {row}"
            );
            assert_eq!(bytes(&credentials_path), credentials_before);
            assert_eq!(bytes(&session_path), session_before);
        }
    }

    #[test]
    fn ucloud_refresh_is_in_memory_only_and_api_calls_do_not_send_cookies() {
        let sandbox = Sandbox::new();
        seed_native(&sandbox, None);
        let credentials_path = sandbox.config_dir().join(CREDENTIALS_FILENAME);
        let session_path = sandbox.cache_dir().join(SESSION_FILENAME);
        let credentials_before = bytes(&credentials_path);
        let session_before = bytes(&session_path);

        let undone = format!("{UCLOUD_API}/ykt-site/site/student/undone?userId=42");
        let courses = format!("{UCLOUD_API}/ykt-site/site/list/student/current?size=20&current=1");
        let courses_list = format!("{UCLOUD_API}/ykt-site/work/student/list");
        let detail = format!("{UCLOUD_API}/ykt-site/work/detail?assignmentId=a1");
        let info = format!("{UCLOUD_API}/ykt-basics/info");
        let term = format!("{UCLOUD_API}/ykt-site/base-term/current");
        let roles = format!("{UCLOUD_API}/ykt-basics/userroledomaindept/listByUserId?userId=42");
        let exams = format!(
            "{UCLOUD_API}/ykt-site/examination/list-stu?current=1&size=1&status=-1&siteId=s1&state=-1"
        );
        let surveys = format!(
            "{UCLOUD_API}/ykt-activity/survey/page/todo?level=1&size=1&userId=42&siteId=s1"
        );

        let mut transport = Scripted::default();
        enqueue(
            &mut transport,
            "GET",
            CAS_SERVICE_URL,
            response(302, &[("Location", TICKET_URL)], ""),
        );
        enqueue(
            &mut transport,
            "POST",
            UCLOUD_TOKEN_URL,
            response(
                200,
                &[],
                &format!("{{\"access_token\":\"{ACCESS}\",\"user_id\":\"42\"}}"),
            ),
        );
        enqueue(
            &mut transport,
            "GET",
            &undone,
            response(
                200,
                &[],
                "{\"code\":200,\"data\":{\"undoneList\":[{\"activityId\":\"a1\"}]}}",
            ),
        );
        enqueue(
            &mut transport,
            "GET",
            &courses,
            response(
                200,
                &[],
                "{\"code\":200,\"data\":{\"records\":[{\"siteId\":\"s1\"}]}}",
            ),
        );
        enqueue(
            &mut transport,
            "POST",
            &courses_list,
            response(200, &[], "{\"code\":200}"),
        );
        enqueue(
            &mut transport,
            "GET",
            &detail,
            response(200, &[], "{\"code\":200}"),
        );
        enqueue(
            &mut transport,
            "GET",
            &info,
            response(200, &[], "{\"code\":200}"),
        );
        enqueue(
            &mut transport,
            "GET",
            &term,
            response(200, &[], "{\"code\":200}"),
        );
        enqueue(
            &mut transport,
            "GET",
            &roles,
            response(200, &[], "{\"code\":200}"),
        );
        enqueue(
            &mut transport,
            "GET",
            &exams,
            response(200, &[], "{\"code\":200}"),
        );
        enqueue(
            &mut transport,
            "GET",
            &surveys,
            response(200, &[], "{\"code\":200}"),
        );

        let report = {
            let mut context = sandbox.context(&mut transport);
            super::run(&matches("ucloud"), &mut context).expect("diagnose succeeds")
        };

        assert_eq!(transport.calls.len(), 11);
        assert_eq!(transport.calls[0].url, CAS_SERVICE_URL);
        assert_eq!(transport.calls[1].url, UCLOUD_TOKEN_URL);
        assert_eq!(report.data["auth"]["ucloud_token"], true);
        assert_eq!(report.data["auth"]["ucloud_token_refreshed"], true);
        assert_eq!(report.data["summary"]["fail"], 0);
        assert_eq!(report.data["summary"]["ok"], 10);
        assert_eq!(bytes(&credentials_path), credentials_before);
        assert_eq!(bytes(&session_path), session_before);
        assert!(!String::from_utf8_lossy(&transport.calls[1].body).contains(PASSWORD));
        assert!(!String::from_utf8_lossy(&transport.calls[1].body).contains(ACCESS));
        for call in &transport.calls[2..] {
            assert!(
                call.headers
                    .iter()
                    .all(|(name, _)| !name.eq_ignore_ascii_case("cookie")),
                "UCloud API request leaked cookies"
            );
            assert!(call
                .headers
                .iter()
                .any(|(name, value)| name.eq_ignore_ascii_case("blade-auth") && value == ACCESS));
            assert!(!String::from_utf8_lossy(&call.body).contains(PASSWORD));
        }
    }
}
