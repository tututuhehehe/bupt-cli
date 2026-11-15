"""auth.py - BUPT CAS, UCloud BladeX, and JWGL authentication management using httpx."""

import asyncio
import base64
import json
import re
import time
from typing import Any, Optional

import httpx
from lxml import etree

from bupt_cli._vendor.campus_api.exceptions import (
    CampusAuthError,
    CampusNetworkError,
    CampusParseError,
)

LOGIN_URL = "https://auth.bupt.edu.cn/authserver/login"
APP_BASE = "https://app.bupt.edu.cn"
UCLOUD_API = "https://apiucloud.bupt.edu.cn"
UCLOUD_SERVICE = "https%3A%2F%2Fucloud.bupt.edu.cn%2Fuclass"
UCLOUD_BASIC_AUTH = "Basic c3dvcmQ6c3dvcmRfc2VjcmV0"  # sword:sword_secret

JWGL_BASE = "https://jwgl.bupt.edu.cn"
JWGL_LOGIN_PAGE = f"{JWGL_BASE}/jsxsd/framework/xsMain_bjyddx.jsp"
JWGL_LOGIN_POST = f"{JWGL_BASE}/jsxsd/xk/LoginToXk"

DEFAULT_UA = (
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 "
    "(KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"
)


def _raise_for_status(response: httpx.Response, context: str) -> None:
    """Classify authentication HTTP statuses before normal network errors."""
    try:
        response.raise_for_status()
    except httpx.HTTPStatusError as exc:
        if exc.response.status_code in (401, 403):
            raise CampusAuthError(
                f"{context} rejected authentication (HTTP {exc.response.status_code})"
            ) from exc
        raise


def decode_jwt_payload(token: str) -> dict[str, Any]:
    """Decode JWT payload without verifying signature."""
    try:
        parts = token.split(".")
        if len(parts) < 2:
            return {}
        payload = parts[1]
        payload += "=" * (-len(payload) % 4)
        decoded = base64.urlsafe_b64decode(payload.encode("utf-8"))
        return json.loads(decoded)
    except Exception:
        return {}


def is_token_expired(payload: dict[str, Any], buffer_seconds: int = 60) -> bool:
    """Check if token's exp field indicates it is expired or expiring soon."""
    exp = payload.get("exp")
    if not exp or not isinstance(exp, (int, float)):
        return False
    return (time.time() + buffer_seconds) >= exp


class BuptSessionManager:
    """Manages authenticated synchronous httpx sessions for BUPT CAS and UCloud."""

    def __init__(self, username: str = "", password: str = "", timeout: float = 15.0):
        self.username = username
        self.password = password
        self.timeout = timeout
        self._client: Optional[httpx.Client] = None
        self._cas_authenticated = False
        self._electricity_primed = False
        self._ucloud_token_data: Optional[dict[str, Any]] = None

    @property
    def client(self) -> httpx.Client:
        """Get or initialize the underlying httpx client."""
        if self._client is None or self._client.is_closed:
            self._client = httpx.Client(
                follow_redirects=True,
                headers={"User-Agent": DEFAULT_UA},
                timeout=self.timeout,
            )
            self._cas_authenticated = False
            self._electricity_primed = False
            self._ucloud_token_data = None
        return self._client

    def ensure_cas_login(self, force: bool = False) -> httpx.Client:
        """Authenticate with BUPT CAS (auth.bupt.edu.cn)."""
        if self._cas_authenticated and not force:
            return self.client

        if not self.username or not self.password:
            raise CampusAuthError(
                "BUPT CAS credentials not provided (username/password required)"
            )

        if force:
            self.invalidate()

        cli = self.client
        try:
            resp = cli.get(LOGIN_URL)
            _raise_for_status(resp, "CAS login page")

            try:
                html = etree.HTML(resp.text)
                execution_nodes = html.xpath("//input[@name='execution']/@value")
            except Exception as e:
                raise CampusParseError(
                    f"Failed to parse CAS login page HTML: {e}"
                ) from e

            if not execution_nodes:
                raise CampusParseError(
                    "Failed to extract execution token from CAS login page"
                )

            execution = execution_nodes[0]

            post_data = {
                "username": self.username,
                "password": self.password,
                "submit": "登录",
                "type": "username_password",
                "execution": execution,
                "_eventId": "submit",
            }
            resp = cli.post(LOGIN_URL, data=post_data)
            _raise_for_status(resp, "CAS login")

            if "authserver" in str(resp.url) and "/login" in str(resp.url):
                raise CampusAuthError(
                    "BUPT CAS login failed: invalid username or password"
                )

            self._cas_authenticated = True
            return cli

        except (CampusAuthError, CampusParseError):
            raise
        except httpx.HTTPError as e:
            raise CampusNetworkError(f"CAS login network error: {e}") from e
        except Exception as e:
            raise CampusAuthError(f"CAS login failed unexpectedly: {e}") from e

    def prime_electricity_session(self, force: bool = False) -> httpx.Client:
        """Prime the session for app.bupt.edu.cn (exchanges CAS ticket for cookies)."""
        if self._electricity_primed and self._cas_authenticated and not force:
            return self.client

        if force:
            self._electricity_primed = False

        cli = self.ensure_cas_login(force=force)
        target_page = f"{APP_BASE}/buptdf/wap/default/chong"
        try:
            resp = cli.get(target_page)
            _raise_for_status(resp, "Electricity session exchange")
            self._electricity_primed = True
            return cli
        except CampusAuthError:
            self._electricity_primed = False
            raise
        except httpx.HTTPError as e:
            self._electricity_primed = False
            raise CampusNetworkError(f"Failed to prime electricity session: {e}") from e

    def get_ucloud_token(self, force: bool = False) -> dict[str, Any]:
        """Exchange CAS ticket for a BladeX UCloud OAuth token."""
        if (
            self._ucloud_token_data
            and not force
            and not is_token_expired(self._ucloud_token_data.get("_jwt_payload", {}))
        ):
            return self._ucloud_token_data

        if force:
            self._ucloud_token_data = None

        cli = self.ensure_cas_login(force=force)
        ticket_url = f"{LOGIN_URL}?service={UCLOUD_SERVICE}"
        try:
            # Do not follow the redirect into ucloud.bupt.edu.cn: the ticket
            # lives in the Location header and must be extracted locally.
            resp = cli.get(ticket_url, follow_redirects=False)
            if resp.status_code >= 400:
                _raise_for_status(resp, "UCloud CAS ticket exchange")
        except CampusAuthError:
            raise
        except httpx.HTTPError as e:
            raise CampusNetworkError(f"UCloud ticket exchange failed: {e}") from e

        match = re.search(r"ticket=([^&\s]+)", str(resp.url)) or re.search(
            r"ticket=([^&\s]+)", resp.headers.get("location", "")
        )
        if not match:
            raise CampusAuthError(
                "No CAS ticket issued for UCloud (login may have expired)"
            )

        ticket = match.group(1)
        try:
            resp = cli.post(
                f"{UCLOUD_API}/ykt-basics/oauth/token",
                data={"grant_type": "third", "ticket": ticket},
                headers={"Authorization": UCLOUD_BASIC_AUTH},
            )
            _raise_for_status(resp, "UCloud token exchange")
            token_json = resp.json()
        except CampusAuthError:
            raise
        except httpx.HTTPError as e:
            raise CampusNetworkError(f"UCloud token request failed: {e}") from e
        except Exception as e:
            raise CampusParseError(f"UCloud token response invalid JSON: {e}") from e

        if not isinstance(token_json, dict):
            raise CampusParseError("UCloud token response must be a JSON object")

        access_token = token_json.get("access_token")
        if not access_token:
            raise CampusAuthError(
                "UCloud token response missing access_token"
            )

        jwt_payload = decode_jwt_payload(access_token)
        self._ucloud_token_data = {
            "access_token": access_token,
            "refresh_token": token_json.get("refresh_token", ""),
            "user_id": jwt_payload.get("user_id") or token_json.get("user_id"),
            "user_name": jwt_payload.get("user_name"),
            "real_name": jwt_payload.get("real_name"),
            "_jwt_payload": jwt_payload,
        }
        return self._ucloud_token_data

    def invalidate(self):
        """Mark session as expired to force re-login on next request."""
        self._cas_authenticated = False
        self._electricity_primed = False
        self._ucloud_token_data = None

    def close(self):
        """Close the underlying HTTP client session."""
        if self._client and not self._client.is_closed:
            self._client.close()
            self._client = None
            self._cas_authenticated = False
            self._electricity_primed = False
            self._ucloud_token_data = None


class AsyncBuptSessionManager:
    """Manages authenticated asynchronous httpx sessions for BUPT CAS and UCloud."""

    def __init__(self, username: str = "", password: str = "", timeout: float = 15.0):
        self.username = username
        self.password = password
        self.timeout = timeout
        self._client: Optional[httpx.AsyncClient] = None
        self._cas_authenticated = False
        self._electricity_primed = False
        self._ucloud_token_data: Optional[dict[str, Any]] = None
        self._cas_lock = asyncio.Lock()
        self._electricity_lock = asyncio.Lock()
        self._ucloud_token_lock = asyncio.Lock()

    @property
    def client(self) -> httpx.AsyncClient:
        """Get or initialize the underlying httpx AsyncClient."""
        if self._client is None or self._client.is_closed:
            self._client = httpx.AsyncClient(
                follow_redirects=True,
                headers={"User-Agent": DEFAULT_UA},
                timeout=self.timeout,
            )
            self._cas_authenticated = False
            self._electricity_primed = False
            self._ucloud_token_data = None
        return self._client

    async def ensure_cas_login(self, force: bool = False) -> httpx.AsyncClient:
        """Authenticate with BUPT CAS asynchronously."""
        if self._cas_authenticated and not force:
            return self.client

        if not self.username or not self.password:
            raise CampusAuthError(
                "BUPT CAS credentials not provided (username/password required)"
            )

        async with self._cas_lock:
            if self._cas_authenticated and not force:
                return self.client

            if force:
                self.invalidate()

            cli = self.client
            try:
                resp = await cli.get(LOGIN_URL)
                _raise_for_status(resp, "CAS login page")

                try:
                    html = etree.HTML(resp.text)
                    execution_nodes = html.xpath("//input[@name='execution']/@value")
                except Exception as e:
                    raise CampusParseError(
                        f"Failed to parse CAS login page HTML: {e}"
                    ) from e

                if not execution_nodes:
                    raise CampusParseError(
                        "Failed to extract execution token from CAS login page"
                    )

                execution = execution_nodes[0]

                post_data = {
                    "username": self.username,
                    "password": self.password,
                    "submit": "登录",
                    "type": "username_password",
                    "execution": execution,
                    "_eventId": "submit",
                }
                resp = await cli.post(LOGIN_URL, data=post_data)
                _raise_for_status(resp, "CAS login")

                if "authserver" in str(resp.url) and "/login" in str(resp.url):
                    raise CampusAuthError(
                        "BUPT CAS login failed: invalid username or password"
                    )

                self._cas_authenticated = True
                return cli

            except (CampusAuthError, CampusParseError):
                raise
            except httpx.HTTPError as e:
                raise CampusNetworkError(f"CAS login network error: {e}") from e
            except Exception as e:
                raise CampusAuthError(f"CAS login failed unexpectedly: {e}") from e

    async def prime_electricity_session(self, force: bool = False) -> httpx.AsyncClient:
        """Prime the async session for app.bupt.edu.cn."""
        if self._electricity_primed and self._cas_authenticated and not force:
            return self.client

        async with self._electricity_lock:
            if self._electricity_primed and self._cas_authenticated and not force:
                return self.client

            if force:
                self._electricity_primed = False

            cli = await self.ensure_cas_login(force=force)
            target_page = f"{APP_BASE}/buptdf/wap/default/chong"
            try:
                resp = await cli.get(target_page)
                _raise_for_status(resp, "Electricity session exchange")
                self._electricity_primed = True
                return cli
            except CampusAuthError:
                self._electricity_primed = False
                raise
            except httpx.HTTPError as e:
                self._electricity_primed = False
                raise CampusNetworkError(
                    f"Failed to prime electricity session: {e}"
                ) from e

    async def get_ucloud_token(self, force: bool = False) -> dict[str, Any]:
        """Exchange CAS ticket for a BladeX UCloud OAuth token asynchronously."""
        if (
            self._ucloud_token_data
            and not force
            and not is_token_expired(self._ucloud_token_data.get("_jwt_payload", {}))
        ):
            return self._ucloud_token_data

        async with self._ucloud_token_lock:
            if (
                self._ucloud_token_data
                and not force
                and not is_token_expired(
                    self._ucloud_token_data.get("_jwt_payload", {})
                )
            ):
                return self._ucloud_token_data

            if force:
                self._ucloud_token_data = None

            cli = await self.ensure_cas_login(force=force)
            ticket_url = f"{LOGIN_URL}?service={UCLOUD_SERVICE}"
            try:
                # See the sync manager: keep the CAS redirect local.
                resp = await cli.get(ticket_url, follow_redirects=False)
                if resp.status_code >= 400:
                    _raise_for_status(resp, "UCloud CAS ticket exchange")
            except CampusAuthError:
                raise
            except httpx.HTTPError as e:
                raise CampusNetworkError(f"UCloud ticket exchange failed: {e}") from e

            match = re.search(r"ticket=([^&\s]+)", str(resp.url)) or re.search(
                r"ticket=([^&\s]+)", resp.headers.get("location", "")
            )
            if not match:
                raise CampusAuthError(
                    "No CAS ticket issued for UCloud (login may have expired)"
                )

            ticket = match.group(1)
            try:
                resp = await cli.post(
                    f"{UCLOUD_API}/ykt-basics/oauth/token",
                    data={"grant_type": "third", "ticket": ticket},
                    headers={"Authorization": UCLOUD_BASIC_AUTH},
                )
                _raise_for_status(resp, "UCloud token exchange")
                token_json = resp.json()
            except CampusAuthError:
                raise
            except httpx.HTTPError as e:
                raise CampusNetworkError(f"UCloud token request failed: {e}") from e
            except Exception as e:
                raise CampusParseError(
                    f"UCloud token response invalid JSON: {e}"
                ) from e

            if not isinstance(token_json, dict):
                raise CampusParseError("UCloud token response must be a JSON object")

            access_token = token_json.get("access_token")
            if not access_token:
                raise CampusAuthError(
                    "UCloud token response missing access_token"
                )

            jwt_payload = decode_jwt_payload(access_token)
            self._ucloud_token_data = {
                "access_token": access_token,
                "refresh_token": token_json.get("refresh_token", ""),
                "user_id": jwt_payload.get("user_id") or token_json.get("user_id"),
                "user_name": jwt_payload.get("user_name"),
                "real_name": jwt_payload.get("real_name"),
                "_jwt_payload": jwt_payload,
            }
            return self._ucloud_token_data

    def invalidate(self):
        """Mark session as expired to force re-login on next request."""
        self._cas_authenticated = False
        self._electricity_primed = False
        self._ucloud_token_data = None

    async def aclose(self):
        """Close the underlying async HTTP client session."""
        if self._client and not self._client.is_closed:
            await self._client.aclose()
            self._client = None
            self._cas_authenticated = False
            self._electricity_primed = False
            self._ucloud_token_data = None


class JwglSessionManager:
    """Manages authenticated synchronous httpx sessions for Qiangzhi JWGL system."""

    def __init__(self, username: str = "", password: str = "", timeout: float = 15.0):
        self.username = username
        self.password = password
        self.timeout = timeout
        self._client: Optional[httpx.Client] = None
        self._authenticated = False

    @property
    def client(self) -> httpx.Client:
        """Get or initialize the underlying httpx client."""
        if self._client is None or self._client.is_closed:
            self._client = httpx.Client(
                follow_redirects=True,
                headers={"User-Agent": DEFAULT_UA},
                timeout=self.timeout,
            )
            self._authenticated = False
        return self._client

    def ensure_login(self, force: bool = False) -> httpx.Client:
        """Authenticate with Qiangzhi JWGL system."""
        if self._authenticated and not force:
            return self.client

        if not self.username or not self.password:
            raise CampusAuthError(
                "JWGL credentials not provided (jwgl_username/jwgl_password required)"
            )

        if force:
            self._authenticated = False

        cli = self.client
        try:
            resp = cli.get(JWGL_LOGIN_PAGE)
            _raise_for_status(resp, "JWGL login page")

            enc_user = base64.b64encode(str(self.username).encode("utf-8")).decode(
                "utf-8"
            )
            enc_pass = base64.b64encode(str(self.password).encode("utf-8")).decode(
                "utf-8"
            )
            post_data = {
                "userAccount": str(self.username),
                "userPassword": "",
                "encoded": f"{enc_user}%%%{enc_pass}",
            }
            resp = cli.post(JWGL_LOGIN_POST, data=post_data)
            _raise_for_status(resp, "JWGL login")

            if "LoginToXk" in str(resp.url):
                raise CampusAuthError("JWGL login failed: invalid username or password")

            self._authenticated = True
            return cli

        except CampusAuthError:
            raise
        except httpx.HTTPError as e:
            raise CampusNetworkError(f"JWGL login network error: {e}") from e
        except Exception as e:
            raise CampusAuthError(f"JWGL login failed: {e}") from e

    def invalidate(self):
        """Mark session as expired."""
        self._authenticated = False

    def close(self):
        """Close the underlying HTTP client session."""
        if self._client and not self._client.is_closed:
            self._client.close()
            self._client = None
            self._authenticated = False


class AsyncJwglSessionManager:
    """Manages authenticated asynchronous httpx sessions for Qiangzhi JWGL system."""

    def __init__(self, username: str = "", password: str = "", timeout: float = 15.0):
        self.username = username
        self.password = password
        self.timeout = timeout
        self._client: Optional[httpx.AsyncClient] = None
        self._authenticated = False

    @property
    def client(self) -> httpx.AsyncClient:
        """Get or initialize the underlying httpx AsyncClient."""
        if self._client is None or self._client.is_closed:
            self._client = httpx.AsyncClient(
                follow_redirects=True,
                headers={"User-Agent": DEFAULT_UA},
                timeout=self.timeout,
            )
            self._authenticated = False
        return self._client

    async def ensure_login(self, force: bool = False) -> httpx.AsyncClient:
        """Authenticate with Qiangzhi JWGL system asynchronously."""
        if self._authenticated and not force:
            return self.client

        if not self.username or not self.password:
            raise CampusAuthError(
                "JWGL credentials not provided (jwgl_username/jwgl_password required)"
            )

        if force:
            self._authenticated = False

        cli = self.client
        try:
            resp = await cli.get(JWGL_LOGIN_PAGE)
            _raise_for_status(resp, "JWGL login page")

            enc_user = base64.b64encode(str(self.username).encode("utf-8")).decode(
                "utf-8"
            )
            enc_pass = base64.b64encode(str(self.password).encode("utf-8")).decode(
                "utf-8"
            )
            post_data = {
                "userAccount": str(self.username),
                "userPassword": "",
                "encoded": f"{enc_user}%%%{enc_pass}",
            }
            resp = await cli.post(JWGL_LOGIN_POST, data=post_data)
            _raise_for_status(resp, "JWGL login")

            if "LoginToXk" in str(resp.url):
                raise CampusAuthError("JWGL login failed: invalid username or password")

            self._authenticated = True
            return cli

        except CampusAuthError:
            raise
        except httpx.HTTPError as e:
            raise CampusNetworkError(f"JWGL login network error: {e}") from e
        except Exception as e:
            raise CampusAuthError(f"JWGL login failed: {e}") from e

    def invalidate(self):
        """Mark session as expired."""
        self._authenticated = False

    async def aclose(self):
        """Close the underlying async HTTP client session."""
        if self._client and not self._client.is_closed:
            await self._client.aclose()
            self._client = None
            self._authenticated = False
