"""Session cache and authenticated-client lifecycle for the ``bupt`` CLI.

The cache lives at ``~/.bupt-cli/session.json`` (mode 0600). It stores
CAS cookies, the UCloud BladeX token and the mobile-JWGL JWT so follow-up
invocations do not need to authenticate again. Passwords are never written.

The CLI deliberately treats the cache as an optimisation: expired sessions
are refreshed automatically, and a corrupt cache is ignored (``bupt login``
rewrites it).
"""

from __future__ import annotations

import http.cookiejar
import json
import os
import re
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Optional

import httpx

from bupt_cli._vendor.campus_api.bupt.auth import (
    LOGIN_URL,
    UCLOUD_API,
    UCLOUD_SERVICE,
    BuptSessionManager,
    decode_jwt_payload,
    is_token_expired,
)
from bupt_cli._vendor.campus_api.bupt.mobile_jwgl import (
    MobileJwglSessionManager,
    _mobile_token_expiry,
)
from bupt_cli._vendor.campus_api.exceptions import CampusAuthError, CampusError
from bupt_cli.credentials import Credentials
from bupt_cli.errors import AuthError

CACHE_DIR_ENV = "BUPT_CLI_CACHE_DIR"
SESSION_VERSION = 1
MOBILE_LOGIN_BLOCK_SECONDS = 6 * 3600
DEFAULT_MAX_LOGINS_PER_RUN = 5
UCLOUD_INFO_PATH = "/ykt-basics/info"


# ---------------------------------------------------------------------------
# Paths and file I/O.
# ---------------------------------------------------------------------------


def cache_dir() -> Path:
    """Where session.json / homework-seen.json live.

    BUPT_CLI_CACHE_DIR wins; otherwise everything lives in ~/.bupt-cli next to
    the credentials, so one directory holds all state.
    """
    override = os.environ.get(CACHE_DIR_ENV)
    if override:
        return Path(override).expanduser()
    return Path("~/.bupt-cli").expanduser()


def session_path() -> Path:
    return cache_dir() / "session.json"


def seen_state_path() -> Path:
    return cache_dir() / "homework-seen.json"


def _utc_iso(epoch: Optional[float]) -> Optional[str]:
    if epoch is None:
        return None
    try:
        return (
            datetime.fromtimestamp(float(epoch), tz=timezone.utc)
            .isoformat()
            .replace("+00:00", "Z")
        )
    except (ValueError, OSError, OverflowError):
        return None


def _ensure_private_dir(path: Path) -> None:
    path.mkdir(parents=True, exist_ok=True)
    try:
        os.chmod(path, 0o700)
    except OSError:
        pass


def load_session_file() -> tuple[dict[str, Any], str]:
    """Load the cache; return (data, error message). Corrupt caches are ignored."""
    path = session_path()
    if not path.exists():
        return {}, ""
    try:
        with path.open("r", encoding="utf-8") as handle:
            data = json.load(handle)
    except (OSError, ValueError) as exc:
        return {}, f"could not read session cache: {exc.__class__.__name__}"
    if not isinstance(data, dict):
        return {}, "session cache is not a JSON object"
    return data, ""


def save_session_file(data: dict[str, Any]) -> None:
    """Atomically write the cache with 0600 permissions."""
    directory = cache_dir()
    _ensure_private_dir(directory)
    target = session_path()
    tmp = target.with_name(target.name + ".tmp")
    payload = json.dumps(data, ensure_ascii=False, indent=2) + "\n"
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(tmp, target)
        os.chmod(target, 0o600)
    finally:
        try:
            if tmp.exists():
                tmp.unlink()
        except OSError:
            pass


def delete_session_file() -> bool:
    path = session_path()
    try:
        if path.exists():
            path.unlink()
            return True
    except OSError as exc:
        raise AuthError(f"could not delete session cache: {exc}") from exc
    return False


# ---------------------------------------------------------------------------
# Cookie (de)serialization.
# ---------------------------------------------------------------------------


def cookie_snapshot(client: httpx.Client) -> list[dict[str, Any]]:
    cookies: list[dict[str, Any]] = []
    for cookie in client.cookies.jar:
        cookies.append(
            {
                "name": cookie.name,
                "value": cookie.value,
                "domain": cookie.domain,
                "path": cookie.path,
                "secure": bool(cookie.secure),
                "expires": int(cookie.expires) if cookie.expires else None,
                "domain_specified": bool(getattr(cookie, "domain_specified", False)),
                "domain_initial_dot": bool(
                    getattr(cookie, "domain_initial_dot", False)
                ),
                "rest": {str(k): str(v) for k, v in (cookie._rest or {}).items()},
            }
        )
    return cookies


def restore_cookies(client: httpx.Client, cookies: list[dict[str, Any]]) -> int:
    restored = 0
    for entry in cookies or []:
        if not isinstance(entry, dict):
            continue
        name = str(entry.get("name") or "")
        value = str(entry.get("value") or "")
        domain = str(entry.get("domain") or "")
        if not name or not domain:
            continue
        expires = entry.get("expires")
        try:
            expires = int(expires) if expires not in (None, "") else None
        except (TypeError, ValueError):
            expires = None
        cookie = http.cookiejar.Cookie(
            version=0,
            name=name,
            value=value,
            port=None,
            port_specified=False,
            domain=domain,
            domain_specified=bool(entry.get("domain_specified", True)),
            domain_initial_dot=bool(entry.get("domain_initial_dot", False)),
            path=str(entry.get("path") or "/"),
            path_specified=True,
            secure=bool(entry.get("secure", False)),
            expires=expires,
            discard=expires is None,
            comment=None,
            comment_url=None,
            rest={str(k): str(v) for k, v in (entry.get("rest") or {}).items()},
            rfc2109=False,
        )
        client.cookies.jar.set_cookie(cookie)
        restored += 1
    return restored


# ---------------------------------------------------------------------------
# Managers with login accounting.
# ---------------------------------------------------------------------------


def _max_logins_per_run() -> int:
    try:
        return max(1, int(os.environ.get("BUPT_MAX_CAS_LOGINS", DEFAULT_MAX_LOGINS_PER_RUN)))
    except ValueError:
        return DEFAULT_MAX_LOGINS_PER_RUN


class CliSessionManager(BuptSessionManager):
    """BuptSessionManager that counts real CAS credential submissions."""

    def __init__(self, *args: Any, on_login=None, **kwargs: Any):
        super().__init__(*args, **kwargs)
        self.on_login = on_login
        self.cas_login_count = 0

    def ensure_cas_login(self, force: bool = False) -> httpx.Client:
        already = self._cas_authenticated and not force
        if not already and self.cas_login_count >= _max_logins_per_run():
            raise CampusAuthError(
                "refusing to perform more than "
                f"{_max_logins_per_run()} CAS logins in one invocation"
            )
        client = super().ensure_cas_login(force=force)
        if not already:
            self.cas_login_count += 1
            if self.on_login is not None:
                self.on_login()
        return client

    def get_ucloud_token(self, force: bool = False) -> dict[str, Any]:
        """Return a valid UCloud token, refreshing an expired cache safely.

        Cached CAS cookies can expire while a previously cached UCloud token
        is still (or no longer) usable. If the cached cookie jar cannot issue
        a ticket, retry once with a real CAS login. This is the only automatic
        CAS credential-submission path in the CLI, and it obeys the
        per-process ``BUPT_MAX_CAS_LOGINS`` cap.
        """
        try:
            return super().get_ucloud_token(force=force)
        except CampusAuthError:
            if force or self.cas_login_count >= _max_logins_per_run():
                raise
            self.ensure_cas_login(force=True)
            return super().get_ucloud_token(force=False)


class CliMobileSessionManager(MobileJwglSessionManager):
    """Mobile JWGL manager that refuses repeated known-bad logins."""

    def __init__(
        self,
        *args: Any,
        disabled_reason: str = "",
        on_login=None,
        **kwargs: Any,
    ):
        super().__init__(*args, **kwargs)
        self.disabled_reason = disabled_reason
        self.on_login = on_login
        self.mobile_login_count = 0

    def ensure_login(self, force: bool = False) -> httpx.Client:
        already = bool(self._token) and not force and time.time() < self._token_expires_at
        if not already and self.disabled_reason:
            raise CampusAuthError(self.disabled_reason)
        client = super().ensure_login(force=force)
        if not already:
            self.mobile_login_count += 1
            if self.on_login is not None:
                self.on_login()
        return client


# ---------------------------------------------------------------------------
# Session facade.
# ---------------------------------------------------------------------------


class BuptSession:
    """Cached CAS + UCloud + mobile-JWGL session used by commands.

    Normal use:
        session = BuptSession(credentials, timeout=20)
        client = session.client()          # authenticated facade
        ...                                 # run commands
        session.persist()                  # in a finally block
    """

    def __init__(self, credentials: Credentials, timeout: float = 20.0):
        self.credentials = credentials
        self.timeout = timeout
        self.data, self.cache_error = load_session_file()
        self.cache_mismatch = bool(
            credentials.username
            and self.data.get("username")
            and self.data.get("username") != credentials.username
        )
        if self.cache_mismatch:
            self.data = {}
        self._cas_mgr: Optional[CliSessionManager] = None
        self._mobile_mgr: Optional[CliMobileSessionManager] = None
        self._new_cas_logins = 0
        self._mobile_login_attempted = False
        self._mobile_login_error = ""

    # -- state ---------------------------------------------------------

    @property
    def has_cached_session(self) -> bool:
        return bool(self.data.get("cookies"))

    def _record_cas_login(self) -> None:
        self._new_cas_logins += 1
        self.data["cas_logins_total"] = int(self.data.get("cas_logins_total") or 0) + 1
        self.data["last_cas_login_at"] = _utc_iso(time.time())

    def _record_mobile_login(self) -> None:
        self._mobile_login_attempted = True

    def _mobile_blocked_reason(self) -> str:
        blocked_until = self.data.get("mobile_jwgl", {}).get("blocked_until")
        try:
            blocked = float(blocked_until) if blocked_until else 0.0
        except (TypeError, ValueError):
            blocked = 0.0
        if blocked > time.time():
            error = self.data.get("mobile_jwgl", {}).get("last_error") or "login failed"
            return (
                "mobile JWGL login is temporarily disabled after a failed attempt "
                f"({error}); retry with `bupt login --jwgl --force` after checking "
                "why the JWGL login was rejected"
            )
        return ""

    # -- managers / clients -------------------------------------------

    def cas_manager(self) -> CliSessionManager:
        if self._cas_mgr is not None:
            return self._cas_mgr
        manager = CliSessionManager(
            username=self.credentials.username,
            password=self.credentials.password,
            timeout=self.timeout,
            on_login=self._record_cas_login,
        )
        client = manager.client
        if self.has_cached_session:
            restore_cookies(client, self.data.get("cookies", []))
            manager._cas_authenticated = True
        token = self.data.get("ucloud") or {}
        if token.get("access_token"):
            payload = token.get("jwt_payload")
            if not isinstance(payload, dict):
                payload = decode_jwt_payload(str(token.get("access_token")))
            manager._ucloud_token_data = {
                "access_token": token.get("access_token"),
                "refresh_token": token.get("refresh_token", ""),
                "user_id": token.get("user_id"),
                "user_name": token.get("user_name"),
                "real_name": token.get("real_name"),
                "_jwt_payload": payload or {},
            }
        self._cas_mgr = manager
        return manager

    def raw_client(self) -> httpx.Client:
        """Cookie-restored httpx client that never logs in automatically."""
        client = httpx.Client(
            follow_redirects=True,
            headers={
                "User-Agent": (
                    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) "
                    "AppleWebKit/537.36 (KHTML, like Gecko) "
                    "Chrome/120.0.0.0 Safari/537.36"
                )
            },
            timeout=self.timeout,
        )
        if self.has_cached_session:
            restore_cookies(client, self.data.get("cookies", []))
        return client

    def client(self, allow_mobile_login: bool = False):
        """Authenticated BuptClient backed by the cached session.

        Mobile JWGL is only allowed to submit a password when
        ``allow_mobile_login`` is true (used by `bupt login --jwgl`); other
        commands use a cached token or fail fast.
        """
        from bupt_cli._vendor.campus_api.bupt.client import BuptClient

        manager = self.cas_manager()
        client = BuptClient(
            username=self.credentials.username,
            password=self.credentials.password,
            jwgl_username=self.credentials.username,
            jwgl_password=self.credentials.password,
            timeout=self.timeout,
        )
        # Reuse the already-prepared managers so cookies/token/accounting stay
        # in one place.
        client._cas_session_mgr = manager
        mobile_manager = self.mobile_manager()
        mobile = self.data.get("mobile_jwgl") or {}
        if not mobile.get("token") and not allow_mobile_login:
            mobile_manager.disabled_reason = (
                "no cached mobile JWGL session; run `bupt login --jwgl`"
            )
        client._mobile_jwgl_session_mgr = mobile_manager
        return client

    def mobile_manager(self) -> CliMobileSessionManager:
        if self._mobile_mgr is not None:
            return self._mobile_mgr
        manager = CliMobileSessionManager(
            username=self.credentials.username,
            password=self.credentials.password,
            timeout=self.timeout,
            disabled_reason=self._mobile_blocked_reason(),
            on_login=self._record_mobile_login,
        )
        client = manager.client
        mobile = self.data.get("mobile_jwgl") or {}
        token = str(mobile.get("token") or "")
        if token:
            expires = mobile.get("expires_at")
            try:
                expires_at = float(expires) if expires else _mobile_token_expiry(token)
            except (TypeError, ValueError):
                expires_at = _mobile_token_expiry(token)
            manager._token = token
            manager._token_expires_at = expires_at
            client.headers["Token"] = token
            client.headers["token"] = token
        self._mobile_mgr = manager
        return manager

    def mobile_service(self, allow_login: bool = False):
        """Return a MobileJwglService or raise when no session is available.

        With ``allow_login=False`` the manager will only use a cached/valid
        token; a missing token raises CampusAuthError instead of silently
        attempting a password login (lockout safety). Expired tokens from a
        previously successful login may be refreshed automatically.
        """
        from bupt_cli._vendor.campus_api.bupt.mobile_jwgl import MobileJwglService

        mobile = self.data.get("mobile_jwgl") or {}
        token = str(mobile.get("token") or "")
        if not token and not allow_login:
            raise CampusAuthError(
                "no cached mobile JWGL session; run `bupt login --jwgl` "
                "(mobile JWGL is the undergraduate system; graduate ids are not present)"
            )
        manager = self.mobile_manager()
        if not allow_login and not token:
            manager.disabled_reason = (
                "no cached mobile JWGL session; run `bupt login --jwgl`"
            )
        return MobileJwglService(manager)

    # -- login / persist ----------------------------------------------

    def login(self, force: bool = False, try_mobile: bool = False) -> dict[str, Any]:
        manager = self.cas_manager()
        manager.ensure_cas_login(force=force)
        # Persist the CAS cookies immediately: if the token exchange has a
        # transient failure we must not waste the (lockout-sensitive) login.
        self.data["version"] = SESSION_VERSION
        self.data["username"] = self.credentials.username
        self.data.setdefault("created_at", _utc_iso(time.time()))
        self.data["updated_at"] = _utc_iso(time.time())
        self.persist()
        # Note: get_ucloud_token(force=True) would perform a *second* CAS
        # credential login; clearing the token and using the freshly
        # authenticated session avoids that.
        if force:
            manager._ucloud_token_data = None
        # CliSessionManager.get_ucloud_token() automatically retries once with
        # a fresh CAS login when cached cookies can no longer issue a ticket.
        token = manager.get_ucloud_token(force=False)
        user = self.data.setdefault("user", {})
        if token.get("user_id") is not None:
            user["user_id"] = token.get("user_id")
        if token.get("user_name"):
            user["user_name"] = token.get("user_name")
        if token.get("real_name"):
            user["real_name"] = token.get("real_name")
        self.persist()

        mobile_status: dict[str, Any] = {"attempted": False, "ok": False}
        if try_mobile:
            mobile_status = self.try_mobile_login(force=force)
        return {
            "user": {
                "username": self.credentials.username,
                "user_id": user.get("user_id"),
                "user_name": user.get("user_name"),
                "real_name": user.get("real_name"),
            },
            "ucloud_token": True,
            "mobile_jwgl": mobile_status,
            "cas_logins_this_run": self._new_cas_logins,
        }

    def try_mobile_login(self, force: bool = False) -> dict[str, Any]:
        mobile = self.data.setdefault("mobile_jwgl", {})
        if not self.credentials.username or not self.credentials.password:
            return {"attempted": False, "ok": False, "reason": "no credentials"}
        blocked = self._mobile_blocked_reason()
        if blocked and not force:
            return {"attempted": False, "ok": False, "reason": blocked}
        if force:
            mobile.pop("blocked_until", None)
            mobile.pop("last_error", None)

        manager = self.mobile_manager()
        # A forced login must actually re-authenticate.
        manager.disabled_reason = ""
        self._mobile_login_attempted = True
        try:
            manager.ensure_login(force=force)
        except CampusError as exc:
            message = str(exc)
            self._mobile_login_error = message
            mobile["last_error"] = message
            mobile["last_attempt_at"] = _utc_iso(time.time())
            if isinstance(exc, CampusAuthError):
                mobile["blocked_until"] = time.time() + MOBILE_LOGIN_BLOCK_SECONDS
            self.persist()
            return {
                "attempted": True,
                "ok": False,
                "error": message,
                "hint": (
                    "mobile JWGL is the undergraduate academic system; a "
                    "graduate account does not exist there. Undergraduate "
                    "accounts use the same 统一身份认证 password as CAS."
                ),
            }
        mobile.pop("last_error", None)
        mobile.pop("blocked_until", None)
        mobile.pop("last_attempt_at", None)
        self.persist()
        return {"attempted": True, "ok": True}

    def persist(self) -> None:
        """Write cookies/tokens (never passwords) to the cache."""
        if self._cas_mgr is not None:
            self.data["cookies"] = cookie_snapshot(self._cas_mgr.client)
            token = self._cas_mgr._ucloud_token_data
            if token and token.get("access_token"):
                payload = token.get("_jwt_payload") or {}
                expires_at = payload.get("exp") if isinstance(payload, dict) else None
                self.data["ucloud"] = {
                    "access_token": token.get("access_token"),
                    "refresh_token": token.get("refresh_token", ""),
                    "user_id": token.get("user_id"),
                    "user_name": token.get("user_name"),
                    "real_name": token.get("real_name"),
                    "jwt_payload": payload,
                    "expires_at": expires_at,
                }
        if self._mobile_mgr is not None and self._mobile_mgr._token:
            self.data["mobile_jwgl"] = {
                **(self.data.get("mobile_jwgl") or {}),
                "token": self._mobile_mgr._token,
                "expires_at": self._mobile_mgr._token_expires_at,
            }

        if not self.data.get("username") and not self.data.get("cookies"):
            return
        self.data["version"] = SESSION_VERSION
        self.data["username"] = self.credentials.username
        self.data["updated_at"] = _utc_iso(time.time())
        save_session_file(self.data)

    # -- status -------------------------------------------------------

    def status(self) -> dict[str, Any]:
        data = self.data
        now = time.time()
        cookies = data.get("cookies") or []
        expiries: list[float] = []
        for entry in cookies:
            if not isinstance(entry, dict):
                continue
            raw_expiry = entry.get("expires")
            if not raw_expiry:
                continue
            try:
                expiries.append(float(raw_expiry))
            except (TypeError, ValueError):
                continue
        cookie_expiry = min(expiries) if expiries else None

        ucloud = data.get("ucloud") or {}
        ucloud_token = ucloud.get("access_token")
        ucloud_exp = ucloud.get("expires_at")
        try:
            ucloud_exp = float(ucloud_exp) if ucloud_exp else None
        except (TypeError, ValueError):
            ucloud_exp = None
        ucloud_expired = bool(ucloud_exp and ucloud_exp <= now + 60)
        if not ucloud_exp and ucloud_token:
            ucloud_expired = is_token_expired(decode_jwt_payload(ucloud_token))

        mobile = data.get("mobile_jwgl") or {}
        mobile_token = mobile.get("token")
        mobile_exp = mobile.get("expires_at")
        try:
            mobile_exp = float(mobile_exp) if mobile_exp else None
        except (TypeError, ValueError):
            mobile_exp = None
        mobile_expired = bool(mobile_exp and mobile_exp <= now)

        return {
            "exists": bool(data),
            "path": str(session_path()),
            "username": data.get("username"),
            "user": data.get("user") or {},
            "created_at": data.get("created_at"),
            "updated_at": data.get("updated_at"),
            "cache_error": self.cache_error,
            "cache_username_mismatch": self.cache_mismatch,
            "cookie_count": len(cookies),
            "cookies_expire_at": _utc_iso(cookie_expiry),
            "ucloud": {
                "present": bool(ucloud_token),
                "expired": ucloud_expired if ucloud_token else None,
                "expires_at": _utc_iso(ucloud_exp),
                "user_id": ucloud.get("user_id"),
                "user_name": ucloud.get("user_name"),
            },
            "mobile_jwgl": {
                "present": bool(mobile_token),
                "expired": mobile_expired if mobile_token else None,
                "expires_at": _utc_iso(mobile_exp),
                "last_error": mobile.get("last_error"),
                "blocked_until": _utc_iso(mobile.get("blocked_until")),
            },
            "cas_logins_total": data.get("cas_logins_total", 0),
            "locally_valid": bool(cookies)
            and bool(ucloud_token)
            and not ucloud_expired,
            "note": "locally_valid is based on cached expiries; use --check for a live check",
        }

    def live_check(self) -> dict[str, Any]:
        """Read-only live validation (never submits credentials)."""
        now = time.time()
        result: dict[str, Any] = {
            "cas_session": {"checked": False},
            "ucloud_token": {"checked": False},
            "mobile_jwgl_token": {"checked": False},
        }
        deps = self.data.get("cookies") or []
        if not deps:
            result["cas_session"] = {
                "checked": False,
                "reason": "no cached cookies",
            }
            result["ucloud_token"] = {
                "checked": False,
                "reason": "no cached cookies",
            }
            result["mobile_jwgl_token"] = {
                "checked": False,
                "reason": "no cached cookies",
            }
            return result

        with self.raw_client() as client:
            # Asking CAS for a service ticket does not authenticate again; a
            # valid cookie yields a redirect containing a ticket.
            try:
                # Keep the CAS redirect local: the ticket is in Location.
                resp = client.get(
                    f"{LOGIN_URL}?service={UCLOUD_SERVICE}",
                    follow_redirects=False,
                )
                issued = bool(
                    re.search(r"[?&]ticket=", str(resp.url))
                    or re.search(
                        r"[?&]ticket=", resp.headers.get("location", "")
                    )
                )
                result["cas_session"] = {
                    "checked": True,
                    "ok": issued,
                    "http_code": resp.status_code,
                    "detail": "ticket issued" if issued else "no ticket (session expired?)",
                }
            except httpx.HTTPError as exc:
                result["cas_session"] = {
                    "checked": True,
                    "ok": False,
                    "error": f"{exc.__class__.__name__}: {exc}",
                }

            ucloud = self.data.get("ucloud") or {}
            token = ucloud.get("access_token")
            if not token:
                result["ucloud_token"] = {"checked": False, "reason": "no cached token"}
            else:
                try:
                    resp = client.get(
                        f"{UCLOUD_API}{UCLOUD_INFO_PATH}",
                        headers={
                            "Blade-Auth": token,
                            "Tenant-Id": "000000",
                            "Authorization": "Basic c3dvcmQ6c3dvcmRfc2VjcmV0",
                        },
                    )
                    payload: Any
                    try:
                        payload = resp.json()
                    except ValueError:
                        payload = {}
                    code = payload.get("code") if isinstance(payload, dict) else None
                    result["ucloud_token"] = {
                        "checked": True,
                        "ok": resp.status_code == 200 and code in (200, 0),
                        "http_code": resp.status_code,
                        "api_code": code,
                    }
                except httpx.HTTPError as exc:
                    result["ucloud_token"] = {
                        "checked": True,
                        "ok": False,
                        "error": f"{exc.__class__.__name__}: {exc}",
                    }

        mobile = self.data.get("mobile_jwgl") or {}
        token = mobile.get("token")
        expires = mobile.get("expires_at")
        try:
            expires_at = float(expires) if expires else None
        except (TypeError, ValueError):
            expires_at = None
        if not token:
            result["mobile_jwgl_token"] = {"checked": False, "reason": "no cached token"}
        else:
            result["mobile_jwgl_token"] = {
                "checked": True,
                "ok": bool(expires_at and expires_at > now),
                "expires_at": _utc_iso(expires_at),
                "detail": "cached JWT expiry (no network request)",
            }
        result["checked_at"] = _utc_iso(time.time())
        return result


def delete_session() -> bool:
    return delete_session_file()
