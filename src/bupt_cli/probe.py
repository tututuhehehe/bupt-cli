"""Live endpoint health matrix for ``bupt probe``.

Rules for lockout safety:
  * public endpoints are checked without credentials;
  * CAS-authenticated checks reuse the cached cookie jar and never submit a
    username/password;
  * a UCloud token may be refreshed with a CAS service ticket (not a login);
  * mobile JWGL endpoints are only checked with a cached token; probe never
    posts any credential.

No cookie, ticket or token value is ever placed in the report.
"""

from __future__ import annotations

import datetime as dt
import re
import time
from typing import Any, Optional

import httpx

from bupt_cli._vendor.campus_api.bupt.auth import (
    LOGIN_URL,
    UCLOUD_API,
    UCLOUD_BASIC_AUTH,
    UCLOUD_SERVICE,
    decode_jwt_payload,
    is_token_expired,
)
from bupt_cli._vendor.campus_api.bupt.mobile_jwgl import MOBILE_JWGL_BASE
from bupt_cli.credentials import load_credentials
from bupt_cli.errors import redact
from bupt_cli.session import (
    BuptSession,
    cookie_snapshot,
    save_session_file,
    session_path,
)

PORTAL_BASE = "http://my.bupt.edu.cn"
PORTAL_NOTICE_URL = (
    f"{PORTAL_BASE}/list.jsp?totalpage=30&PAGENUM=0&urltype=tree.TreeTempUrl"
    "&wbtreeid=1154"
)
PORTAL_CARD_URL = f"{PORTAL_BASE}/system/resource/app/cuser/getwxtsA.jsp"
PORTAL_CARD_REFERER = (
    f"{PORTAL_BASE}/xs_index.jsp?urltype=tree.TreeTempUrl&wbtreeid=1541"
)
JWGL_DOM_BASE = "https://jwgl.bupt.edu.cn"
JWGL_DOM_SCHEDULE = f"{JWGL_DOM_BASE}/jsxsd/framework/xsMain_bjyddx.jsp"
JWGL_DOM_PERSON = f"{JWGL_DOM_BASE}/jsxsd/framework/xsdPerson.jsp"
JWXT_LOGIN = "https://jwxt.bupt.edu.cn/caslogin.jsp"
UC_STATUS = "https://uc.bupt.edu.cn/api/uc/status"
WIN_NOTICE = "https://win.bupt.edu.cn/note.do"
ELECTRICITY_LANDING = "https://app.bupt.edu.cn/buptdf/wap/default/chong"
ELECTRICITY_API = "https://app.bupt.edu.cn/buptdf/wap/default"
ELECTRICITY_HEADERS = {
    "Referer": "https://app.bupt.edu.cn/buptdf/wap/default/chong",
    "X-Requested-With": "XMLHttpRequest",
}
UCLOUD_HEADERS_BASIC = "c3dvcmQ6c3dvcmRfc2VjcmV0"
MOBILE_PATHS = [
    ("curriculum", "GET", "/bjyddx/student/curriculum"),
    ("schedule modes", "GET", "/bjyddx/Get_sjkbms"),
    ("teaching week", "GET", "/bjyddx/teachingWeek"),
    ("exam weeks", "GET", "/bjyddx/kaowu/get_kszc"),
    ("exam schedule", "GET", "/bjyddx/kaowu/getKsapcx"),
    ("exam course arrangements", "GET", "/bjyddx/kaowu/findKcksapcx"),
    ("examination arrangement", "GET", "/bjyddx/student/examinationArrangement"),
    ("notices", "GET", "/bjyddx/noticeList"),
    ("empty classrooms", "GET", "/bjyddx/todayClassrooms?campusId=04"),
    ("current term", "GET", "/bjyddx/currentTerm"),
    ("score report (termGPA)", "POST", "/bjyddx/student/termGPA"),
]
GROUPS = (
    "public",
    "cas",
    "electricity",
    "portal",
    "ucloud",
    "mobile_jwgl",
    "jwgl_dom",
    "other",
)


class _Collector:
    def __init__(self, enabled_groups: Optional[set[str]], timeout: float, secrets: list[str]):
        self.rows: list[dict[str, Any]] = []
        self.enabled = enabled_groups
        self.timeout = timeout
        self.secrets = secrets

    def wants(self, group: str) -> bool:
        return self.enabled is None or group in self.enabled

    def add(
        self,
        group: str,
        name: str,
        method: str,
        url: str,
        *,
        status: str,
        http_code: Optional[int] = None,
        latency_ms: Optional[int] = None,
        evidence: str = "",
        error: str = "",
        notes: str = "",
    ) -> None:
        if not self.wants(group):
            return
        row: dict[str, Any] = {
            "group": group,
            "name": name,
            "method": method,
            "url": url,
            "status": status,
        }
        if http_code is not None:
            row["http_code"] = http_code
        if latency_ms is not None:
            row["latency_ms"] = latency_ms
        if evidence:
            row["evidence"] = redact(evidence, self.secrets)
        if error:
            row["error"] = redact(error, self.secrets)
        if notes:
            row["notes"] = redact(notes, self.secrets)
        self.rows.append(row)

    def skipped(self, group: str, name: str, method: str, url: str, reason: str) -> None:
        self.add(group, name, method, url, status="skipped", evidence=reason)


def _short(value: Any, limit: int = 160) -> str:
    text = str(value).replace("\n", " ").strip()
    return text[:limit] + ("…" if len(text) > limit else "")


def _request(
    collector: _Collector,
    group: str,
    name: str,
    method: str,
    url: str,
    client: httpx.Client,
    *,
    expected: Optional[set[int]] = None,
    headers: Optional[dict[str, str]] = None,
    params: Optional[dict[str, Any]] = None,
    json_body: Optional[dict[str, Any]] = None,
    data: Optional[dict[str, Any]] = None,
    success_codes: Optional[set[Any]] = None,
    note: str = "",
    skip_reason: str = "",
    follow_redirects: Optional[bool] = None,
    detect_login_page: bool = False,
) -> tuple[Optional[httpx.Response], Any]:
    """Run one HTTP check and append a row; returns (response, parsed JSON)."""
    if not collector.wants(group):
        return None, None
    if skip_reason:
        collector.skipped(group, name, method, url, skip_reason)
        return None, None

    started = time.monotonic()
    request_kwargs: dict[str, Any] = {
        "headers": headers,
        "params": params,
        "json": json_body,
        "data": data,
        "timeout": collector.timeout,
    }
    if follow_redirects is not None:
        request_kwargs["follow_redirects"] = follow_redirects
    try:
        response = client.request(method, url, **request_kwargs)
    except httpx.HTTPError as exc:
        latency = int((time.monotonic() - started) * 1000)
        collector.add(
            group,
            name,
            method,
            url,
            status="fail",
            latency_ms=latency,
            error=f"{exc.__class__.__name__}: {_short(exc)}",
        )
        return None, None

    latency = int((time.monotonic() - started) * 1000)
    payload: Any = None
    if response.content:
        try:
            payload = response.json()
        except ValueError:
            payload = None

    if success_codes is not None:
        code = payload.get("code") if isinstance(payload, dict) else None
        success = payload.get("success") if isinstance(payload, dict) else None
        if code in success_codes or success is True:
            evidence = f"HTTP {response.status_code}"
            if code is not None:
                evidence += f", api_code={code}"
            if isinstance(payload, dict) and isinstance(payload.get("data"), list):
                evidence += f", items={len(payload['data'])}"
            collector.add(
                group,
                name,
                method,
                url,
                status="ok",
                http_code=response.status_code,
                latency_ms=latency,
                evidence=evidence,
                notes=note,
            )
        else:
            message = ""
            if isinstance(payload, dict):
                message = payload.get("msg") or payload.get("Msg") or ""
            collector.add(
                group,
                name,
                method,
                url,
                status="fail",
                http_code=response.status_code,
                latency_ms=latency,
                evidence=f"HTTP {response.status_code}"
                + (f", api_code={code}" if code is not None else "")
                + (f", msg={_short(message)}" if message else ""),
                error=f"upstream API rejected request (HTTP {response.status_code})",
                notes=note,
            )
        return response, payload

    ok = False
    if expected is not None:
        ok = response.status_code in expected
    else:
        ok = response.status_code < 400
    login_page = False
    if detect_login_page and response.status_code < 400:
        haystack = (str(response.url) + "\n" + response.text[:30000]).lower()
        login_page = any(
            marker in haystack
            for marker in (
                "authserver/login",
                "clogin.jsp",
                'name="execution"',
                "name='execution'",
                "logintoxk",
                "统一身份认证",
            )
        )
        if login_page:
            ok = False
    collector.add(
        group,
        name,
        method,
        url,
        status="ok" if ok else "fail",
        http_code=response.status_code,
        latency_ms=latency,
        evidence=f"HTTP {response.status_code}"
        + (", returned login page (session expired)" if login_page else ""),
        error=(
            "portal/JWGL returned a login page instead of data (session expired)"
            if login_page
            else ("" if ok else f"unexpected HTTP status {response.status_code}")
        ),
        notes=note,
    )
    return response, payload


def _ticket_from_response(response: Optional[httpx.Response]) -> str:
    if response is None:
        return ""
    match = re.search(r"[?&]ticket=([^&\s]+)", str(response.url)) or re.search(
        r"[?&]ticket=([^&\s]+)", response.headers.get("location", "")
    )
    return match.group(1) if match else ""


def _mobile_headers(token: str) -> dict[str, str]:
    return {"Token": token, "token": token, "Accept": "application/json, text/plain, */*"}


def _ucloud_headers(token: str) -> dict[str, str]:
    return {
        "Blade-Auth": token,
        "Tenant-Id": "000000",
        "Authorization": f"Basic {UCLOUD_HEADERS_BASIC}",
    }


def _save_probe_state(session: BuptSession, cookie_client: httpx.Client, token: dict[str, Any]) -> None:
    session.data["version"] = 1
    session.data["username"] = session.credentials.username or session.data.get("username") or ""
    session.data["cookies"] = cookie_snapshot(cookie_client)
    session.data["updated_at"] = (
        dt.datetime.now(dt.timezone.utc).isoformat().replace("+00:00", "Z")
    )
    payload = decode_jwt_payload(token.get("access_token", ""))
    session.data["ucloud"] = {
        "access_token": token.get("access_token"),
        "refresh_token": token.get("refresh_token", ""),
        "user_id": payload.get("user_id") or token.get("user_id"),
        "user_name": payload.get("user_name"),
        "real_name": payload.get("real_name"),
        "jwt_payload": payload,
        "expires_at": payload.get("exp"),
    }
    try:
        save_session_file(session.data)
    except OSError:
        pass


def run_probe(args) -> dict[str, Any]:
    requested = None
    if getattr(args, "only", None):
        requested = {part.strip() for part in args.only.split(",") if part.strip()}
        unknown = requested - set(GROUPS)
        if unknown:
            from bupt_cli.errors import UsageError

            raise UsageError(
                f"unknown probe group(s): {', '.join(sorted(unknown))}",
                hint=f"available groups: {', '.join(GROUPS)}",
            )

    timeout = float(getattr(args, "probe_timeout", 8.0))
    session = BuptSession(load_credentials(required=False), timeout=timeout)
    secrets = list(session.credentials.secret_values())
    collector = _Collector(requested, timeout, secrets)
    started_at = dt.datetime.now(dt.timezone.utc)

    with httpx.Client(follow_redirects=True, timeout=timeout) as public_client:
        # -- public reachability -------------------------------------
        _request(
            collector,
            "public",
            "CAS login page",
            "GET",
            LOGIN_URL,
            public_client,
            expected={200},
        )
        _request(
            collector,
            "other",
            "UC status API",
            "GET",
            UC_STATUS,
            public_client,
            expected={200},
        )
        _request(
            collector,
            "public",
            "UCloud OAuth token endpoint",
            "GET",
            f"{UCLOUD_API}/ykt-basics/oauth/token",
            public_client,
            expected={401, 405},
            note="GET without credentials should be rejected (401/405)",
        )
        _request(
            collector,
            "public",
            "electricity landing page",
            "GET",
            ELECTRICITY_LANDING,
            public_client,
            expected={200, 302},
            note="302 redirects to CAS without a session",
        )
        _request(
            collector,
            "portal",
            "portal notice list (unauthed)",
            "GET",
            PORTAL_NOTICE_URL,
            public_client,
            note="my.bupt.edu.cn is expected to redirect to CAS",
        )
        _request(
            collector,
            "other",
            "innovation platform notices",
            "GET",
            WIN_NOTICE,
            public_client,
            expected={200, 302, 412},
            note="412 is anti-bot protection; reachability only",
        )
        _request(
            collector,
            "jwgl_dom",
            "DOM JWGL login page",
            "GET",
            JWGL_DOM_SCHEDULE,
            public_client,
            expected={200, 302},
        )
        _request(
            collector,
            "jwgl_dom",
            "DOM JWGL personal page",
            "GET",
            JWGL_DOM_PERSON,
            public_client,
            expected={200, 302},
        )
        _request(
            collector,
            "jwgl_dom",
            "legacy jwxt login page",
            "GET",
            JWXT_LOGIN,
            public_client,
            expected={200, 302},
        )
        _request(
            collector,
            "mobile_jwgl",
            "mobile JWGL login endpoint",
            "GET",
            f"{MOBILE_JWGL_BASE}/bjyddx/login",
            public_client,
            expected={200, 302, 400, 401, 405, 500},
            note=(
                "POST-only in practice; GET 500 still proves reachability. "
                "Reachability only: this is the undergraduate academic system, "
                "so a graduate-student account has no data here even when the "
                "endpoint answers."
            ),
        )

        # -- CAS-authenticated raw checks ----------------------------
        cookie_client = session.raw_client()
        has_cookies = bool(session.data.get("cookies"))
        ticket = ""
        fresh_token = ""
        fresh_user_id = ""
        token_obtained = False
        if not has_cookies:
            no_session = "no cached CAS session; run `bupt login`"
            _request(
                collector,
                "cas",
                "CAS service ticket",
                "GET",
                f"{LOGIN_URL}?service={UCLOUD_SERVICE}",
                cookie_client,
                skip_reason=no_session,
            )
            for group, name, url in [
                ("portal", "portal notice list", PORTAL_NOTICE_URL),
                ("portal", "portal campus-card widget", PORTAL_CARD_URL),
            ]:
                _request(
                    collector, group, name, "GET", url, cookie_client, skip_reason=no_session
                )
            for name, path in [
                ("electricity buildings (/part)", "/part"),
                ("electricity floors (/floor)", "/floor"),
                ("electricity rooms (/drom)", "/drom"),
                ("electricity search (/search)", "/search"),
            ]:
                _request(
                    collector,
                    "electricity",
                    name,
                    "POST",
                    ELECTRICITY_API + path,
                    cookie_client,
                    skip_reason=no_session,
                )
        else:
            ticket_resp, _ = _request(
                collector,
                "cas",
                "CAS service ticket",
                "GET",
                f"{LOGIN_URL}?service={UCLOUD_SERVICE}",
                cookie_client,
                headers={"Accept": "text/html,application/xhtml+xml"},
                expected={200, 302},
                note="ticket issuance does not authenticate again",
                follow_redirects=False,
            )
            ticket = _ticket_from_response(ticket_resp)
            if ticket and collector.wants("cas"):
                row = collector.rows[-1]
                row["notes"] = "ticket issued; session cookies are valid"

            # Exchange the ticket immediately: CAS service tickets are
            # short-lived, and the portal/electricity checks below can take
            # tens of seconds on this network.
            if ticket and collector.wants("ucloud"):
                token_resp, token_payload = _request(
                    collector,
                    "ucloud",
                    "UCloud OAuth token exchange",
                    "POST",
                    f"{UCLOUD_API}/ykt-basics/oauth/token",
                    cookie_client,
                    data={"grant_type": "third", "ticket": ticket},
                    headers={"Authorization": UCLOUD_BASIC_AUTH},
                    expected={200},
                )
                if (
                    token_resp is not None
                    and isinstance(token_payload, dict)
                    and token_payload.get("access_token")
                ):
                    fresh_token = str(token_payload["access_token"])
                    payload = decode_jwt_payload(fresh_token)
                    fresh_user_id = (
                        payload.get("user_id") or token_payload.get("user_id") or ""
                    )
                    token_obtained = True
                    _save_probe_state(session, cookie_client, token_payload)

            _request(
                collector,
                "portal",
                "portal notice list",
                "GET",
                PORTAL_NOTICE_URL,
                cookie_client,
                expected={200},
                detect_login_page=True,
            )
            _request(
                collector,
                "portal",
                "portal campus-card widget",
                "GET",
                PORTAL_CARD_URL,
                cookie_client,
                headers={
                    "Referer": PORTAL_CARD_REFERER,
                    "X-Requested-With": "XMLHttpRequest",
                },
                expected={200},
                detect_login_page=True,
            )
            if collector.wants("electricity"):
                # Prime app.bupt.edu.cn cookies via /chong, then walk the
                # /part -> /floor -> /drom -> /search chain. HTTP 200 alone is
                # not enough: the app answers expired sessions with
                # {"e":10013,"m":"用户信息已失效..."} and HTTP 200.
                _request(
                    collector,
                    "electricity",
                    "electricity session prime",
                    "GET",
                    ELECTRICITY_LANDING,
                    cookie_client,
                    expected={200, 302},
                    note="GET /chong establishes app.bupt.edu.cn cookies",
                )

                def _electricity_data(resp):
                    if resp is None:
                        return None, "no response"
                    try:
                        payload = resp.json()
                    except ValueError:
                        return None, "invalid JSON"
                    if not isinstance(payload, dict):
                        return None, "unexpected payload"
                    if str(payload.get("e")) != "0":
                        return None, str(payload.get("m") or payload.get("e"))
                    data = (payload.get("d") or {}).get("data")
                    return data, ""

                def _electricity_skip(names_paths, reason):
                    for name, path in names_paths:
                        collector.skipped(
                            "electricity", name, "POST", ELECTRICITY_API + path, reason
                        )

                part_resp, _ = _request(
                    collector,
                    "electricity",
                    "electricity buildings (/part)",
                    "POST",
                    ELECTRICITY_API + "/part",
                    cookie_client,
                    data={"areaid": 2},
                    headers=ELECTRICITY_HEADERS,
                )
                part_data, part_error = _electricity_data(part_resp)
                if part_error or not isinstance(part_data, list) or not part_data:
                    if collector.wants("electricity") and collector.rows:
                        row = collector.rows[-1]
                        row["status"] = "fail"
                        row["evidence"] = f"HTTP {getattr(part_resp, 'status_code', '')}, api_error={part_error or 'empty building list'}"
                        row["error"] = "electricity /part did not return building data"
                    _electricity_skip(
                        [
                            ("electricity floors (/floor)", "/floor"),
                            ("electricity rooms (/drom)", "/drom"),
                            ("electricity search (/search)", "/search"),
                        ],
                        f"/part failed: {part_error or 'empty building list'}",
                    )
                else:
                    first = part_data[0] if isinstance(part_data[0], dict) else {}
                    partment_id = first.get("partmentId") or first.get("partmentid")
                    if not partment_id:
                        _electricity_skip(
                            [
                                ("electricity floors (/floor)", "/floor"),
                                ("electricity rooms (/drom)", "/drom"),
                                ("electricity search (/search)", "/search"),
                            ],
                            "no partmentId in /part response",
                        )
                    else:
                        floor_resp, _ = _request(
                            collector,
                            "electricity",
                            "electricity floors (/floor)",
                            "POST",
                            ELECTRICITY_API + "/floor",
                            cookie_client,
                            data={"partmentId": partment_id, "areaid": 2},
                            headers=ELECTRICITY_HEADERS,
                        )
                        floor_data, floor_error = _electricity_data(floor_resp)
                        if floor_error or not isinstance(floor_data, list) or not floor_data:
                            if collector.wants("electricity") and collector.rows:
                                row = collector.rows[-1]
                                row["status"] = "fail"
                                row["evidence"] = f"HTTP {getattr(floor_resp, 'status_code', '')}, api_error={floor_error or 'empty floor list'}"
                                row["error"] = "electricity /floor did not return floor data"
                            _electricity_skip(
                                [
                                    ("electricity rooms (/drom)", "/drom"),
                                    ("electricity search (/search)", "/search"),
                                ],
                                f"/floor failed: {floor_error or 'empty floor list'}",
                            )
                        else:
                            floor_first = floor_data[0] if isinstance(floor_data[0], dict) else {}
                            floor_id = floor_first.get("floorId") or ""
                            if not floor_id:
                                _electricity_skip(
                                    [
                                        ("electricity rooms (/drom)", "/drom"),
                                        ("electricity search (/search)", "/search"),
                                    ],
                                    "no floorId in /floor response",
                                )
                            else:
                                drom_resp, _ = _request(
                                    collector,
                                    "electricity",
                                    "electricity rooms (/drom)",
                                    "POST",
                                    ELECTRICITY_API + "/drom",
                                    cookie_client,
                                    data={
                                        "partmentId": partment_id,
                                        "floorId": floor_id,
                                        "areaid": 2,
                                    },
                                    headers=ELECTRICITY_HEADERS,
                                )
                                drom_data, drom_error = _electricity_data(drom_resp)
                                if drom_error or not isinstance(drom_data, list):
                                    if collector.wants("electricity") and collector.rows:
                                        row = collector.rows[-1]
                                        row["status"] = "fail"
                                        row["evidence"] = f"HTTP {getattr(drom_resp, 'status_code', '')}, api_error={drom_error}"
                                        row["error"] = "electricity /drom did not return room data"
                                    _electricity_skip(
                                        [("electricity search (/search)", "/search")],
                                        f"/drom failed: {drom_error}",
                                    )
                                else:
                                    room_number = ""
                                    if drom_data and isinstance(drom_data[0], dict):
                                        room_number = (
                                            drom_data[0].get("dromNum")
                                            or drom_data[0].get("dromName")
                                            or ""
                                        )
                                    search_resp, _ = _request(
                                        collector,
                                        "electricity",
                                        "electricity search (/search)",
                                        "POST",
                                        ELECTRICITY_API + "/search",
                                        cookie_client,
                                        data={
                                            "areaid": 2,
                                            "partmentId": partment_id,
                                            "floorId": floor_id,
                                            "dromNumber": room_number,
                                        },
                                        headers=ELECTRICITY_HEADERS,
                                    )
                                    search_data, search_error = _electricity_data(search_resp)
                                    if search_error and collector.wants("electricity") and collector.rows:
                                        row = collector.rows[-1]
                                        row["status"] = "fail"
                                        row["evidence"] = f"HTTP {getattr(search_resp, 'status_code', '')}, api_error={search_error}"
                                        row["error"] = "electricity /search failed"

        # -- UCloud ---------------------------------------------------
        cached = session.data.get("ucloud") or {}
        ucloud_payload = cached.get("jwt_payload") or {}
        ucloud_token = fresh_token or str(cached.get("access_token") or "")
        user_id = (
            fresh_user_id
            or cached.get("user_id")
            or (ucloud_payload.get("user_id") if isinstance(ucloud_payload, dict) else None)
            or ""
        )

        if not has_cookies:
            collector.skipped(
                "ucloud",
                "UCloud OAuth token exchange",
                "POST",
                f"{UCLOUD_API}/ykt-basics/oauth/token",
                "no cached CAS session; run `bupt login`",
            )
        elif not token_obtained and collector.wants("ucloud"):
            # Fallback for `--only ucloud` (or when the CAS group was not
            # collected): fetch a fresh ticket and exchange it now. Do not
            # touch CAS when the caller did not ask for UCloud checks.
            try:
                ticket_resp = cookie_client.get(
                    f"{LOGIN_URL}?service={UCLOUD_SERVICE}",
                    timeout=timeout,
                    follow_redirects=False,
                )
                ticket = _ticket_from_response(ticket_resp)
            except httpx.HTTPError:
                ticket = ""
            if not ticket:
                collector.skipped(
                    "ucloud",
                    "UCloud OAuth token exchange",
                    "POST",
                    f"{UCLOUD_API}/ykt-basics/oauth/token",
                    "CAS session did not issue a ticket",
                )
            else:
                token_resp, token_payload = _request(
                    collector,
                    "ucloud",
                    "UCloud OAuth token exchange",
                    "POST",
                    f"{UCLOUD_API}/ykt-basics/oauth/token",
                    cookie_client,
                    data={"grant_type": "third", "ticket": ticket},
                    headers={"Authorization": UCLOUD_BASIC_AUTH},
                    expected={200},
                )
                if (
                    token_resp is not None
                    and isinstance(token_payload, dict)
                    and token_payload.get("access_token")
                ):
                    ucloud_token = str(token_payload["access_token"])
                    payload = decode_jwt_payload(ucloud_token)
                    user_id = payload.get("user_id") or token_payload.get("user_id") or ""
                    token_obtained = True
                    _save_probe_state(session, cookie_client, token_payload)

        token_expired = bool(
            ucloud_token and is_token_expired(decode_jwt_payload(ucloud_token))
        )
        if not ucloud_token:
            ucloud_skip = "no cached/obtained UCloud token; run `bupt login`"
        elif token_expired:
            ucloud_skip = "cached UCloud token is expired; run `bupt login`"
        else:
            ucloud_skip = ""

        api_headers = _ucloud_headers(ucloud_token) if ucloud_token else {}
        ucloud_base = UCLOUD_API
        undone_resp, undone_payload = _request(
            collector,
            "ucloud",
            "undone homework",
            "GET",
            f"{ucloud_base}/ykt-site/site/student/undone",
            cookie_client,
            params={"userId": user_id},
            headers=api_headers,
            success_codes={200, 0, "200", "0"},
            skip_reason=ucloud_skip,
        )
        assignment_id = ""
        if isinstance(undone_payload, dict):
            data = undone_payload.get("data")
            if isinstance(data, dict):
                records = data.get("undoneList") or []
                if records and isinstance(records[0], dict):
                    assignment_id = str(
                        records[0].get("activityId") or records[0].get("id") or ""
                    )

        courses_resp, courses_payload = _request(
            collector,
            "ucloud",
            "current course sites",
            "GET",
            f"{ucloud_base}/ykt-site/site/list/student/current",
            cookie_client,
            params={"size": 20, "current": 1},
            headers=api_headers,
            success_codes={200, 0, "200", "0"},
            skip_reason=ucloud_skip,
        )
        site_id = ""
        if isinstance(courses_payload, dict):
            data = courses_payload.get("data")
            if isinstance(data, dict):
                records = data.get("records") or []
                if records and isinstance(records[0], dict):
                    site_id = str(records[0].get("siteId") or records[0].get("id") or "")
                    if not assignment_id:
                        assignment_id = str(records[0].get("activityId") or "")

        _request(
            collector,
            "ucloud",
            "course homework list",
            "POST",
            f"{ucloud_base}/ykt-site/work/student/list",
            cookie_client,
            json_body={
                "siteId": site_id,
                "userId": user_id,
                "current": 1,
                "size": 20,
            }
            if site_id
            else None,
            headers={**api_headers, "Content-Type": "application/json;charset=UTF-8"},
            success_codes={200, 0, "200", "0"},
            skip_reason=ucloud_skip or ("" if site_id else "no course id available"),
        )
        _request(
            collector,
            "ucloud",
            "homework detail",
            "GET",
            f"{ucloud_base}/ykt-site/work/detail",
            cookie_client,
            params={"assignmentId": assignment_id},
            headers=api_headers,
            success_codes={200, 0, "200", "0"},
            skip_reason=ucloud_skip
            or ("" if assignment_id else "no assignment id available"),
        )
        _request(
            collector,
            "ucloud",
            "user info",
            "GET",
            f"{ucloud_base}/ykt-basics/info",
            cookie_client,
            headers=api_headers,
            success_codes={200, 0, "200", "0"},
            skip_reason=ucloud_skip,
        )
        _request(
            collector,
            "ucloud",
            "current term",
            "GET",
            f"{ucloud_base}/ykt-site/base-term/current",
            cookie_client,
            headers=api_headers,
            success_codes={200, 0, "200", "0"},
            skip_reason=ucloud_skip,
        )
        _request(
            collector,
            "ucloud",
            "user role/domain/department",
            "GET",
            f"{ucloud_base}/ykt-basics/userroledomaindept/listByUserId",
            cookie_client,
            params={"userId": user_id},
            headers=api_headers,
            success_codes={200, 0, "200", "0"},
            skip_reason=ucloud_skip,
        )
        _request(
            collector,
            "ucloud",
            "course examinations",
            "GET",
            f"{ucloud_base}/ykt-site/examination/list-stu",
            cookie_client,
            params={
                "current": 1,
                "size": 1,
                "status": -1,
                "siteId": site_id,
                "state": -1,
            },
            headers=api_headers,
            success_codes={200, 0, "200", "0"},
            skip_reason=ucloud_skip or ("" if site_id else "no course id available"),
        )
        _request(
            collector,
            "ucloud",
            "course surveys/todos",
            "GET",
            f"{ucloud_base}/ykt-activity/survey/page/todo",
            cookie_client,
            params={"level": 1, "size": 1, "userId": user_id, "siteId": site_id},
            headers=api_headers,
            success_codes={200, 0, "200", "0"},
            skip_reason=ucloud_skip or ("" if site_id else "no course id available"),
        )

        cookie_client.close()

        # -- mobile JWGL (cached token only) --------------------------
        mobile = session.data.get("mobile_jwgl") or {}
        mobile_token = str(mobile.get("token") or "")
        try:
            mobile_expires = float(mobile.get("expires_at") or 0)
        except (TypeError, ValueError):
            mobile_expires = 0.0
        if not mobile_token:
            mobile_skip = (
                "no cached mobile JWGL token; probe never submits JWGL "
                "credentials (this is the undergraduate academic system — a "
                "graduate-student account has no data here)"
            )
        elif mobile_expires and mobile_expires <= time.time():
            mobile_skip = "cached mobile JWGL token is expired; run `bupt login --jwgl`"
        else:
            mobile_skip = ""
        mobile_client = httpx.Client(
            follow_redirects=True,
            timeout=timeout,
            headers={
                "User-Agent": (
                    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) "
                    "AppleWebKit/537.36 (KHTML, like Gecko) "
                    "Chrome/120.0.0.0 Safari/537.36"
                ),
                "Accept": "application/json, text/plain, */*",
            },
        )
        try:
            for name, method, path in MOBILE_PATHS:
                _request(
                    collector,
                    "mobile_jwgl",
                    name,
                    method,
                    f"{MOBILE_JWGL_BASE}{path}",
                    mobile_client,
                    headers=_mobile_headers(mobile_token) if mobile_token else None,
                    success_codes={1, "1"},
                    skip_reason=mobile_skip,
                    note=""
                    if not mobile_skip
                    else "",
                )
        finally:
            mobile_client.close()

    collector.rows.sort(key=lambda row: (GROUPS.index(row["group"]), row["name"]))
    summary = {
        "ok": sum(1 for row in collector.rows if row["status"] == "ok"),
        "fail": sum(1 for row in collector.rows if row["status"] == "fail"),
        "skipped": sum(1 for row in collector.rows if row["status"] == "skipped"),
        "total": len(collector.rows),
    }
    finished_at = dt.datetime.now(dt.timezone.utc)
    return {
        "generated_at": finished_at.isoformat().replace("+00:00", "Z"),
        "duration_s": round((finished_at - started_at).total_seconds(), 2),
        "session_path": str(session_path()),
        "auth": {
            "cached_cas_cookies": has_cookies,
            "ucloud_token": bool(ucloud_token) and not token_expired,
            "ucloud_token_refreshed": token_obtained,
            "mobile_jwgl_token": bool(mobile_token) and not mobile_skip,
        },
        "summary": summary,
        "endpoints": collector.rows,
    }
