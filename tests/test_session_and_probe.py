import json
import time

import httpx
import pytest

from bupt_cli.credentials import Credentials, load_credentials
from bupt_cli.errors import AuthError
from bupt_cli.probe import (
    _Collector,
    _short,
    _ticket_from_response,
    _ucloud_headers,
    run_probe,
)
from bupt_cli.session import (
    BuptSession,
    delete_session,
    save_session_file,
    session_path,
)


def _fake_session_data(now=None):
    now = now or time.time()
    return {
        "version": 1,
        "username": "u1",
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:00Z",
        "cookies": [
            {
                "name": "SID",
                "value": "COOKIE-SECRET-VALUE",
                "domain": "auth.bupt.edu.cn",
                "path": "/",
                "secure": True,
                "expires": now + 3600,
                "domain_specified": True,
                "domain_initial_dot": False,
                "rest": {},
            }
        ],
        "ucloud": {
            "access_token": "ACCESS-TOKEN-SECRET-VALUE",
            "refresh_token": "REFRESH-TOKEN-SECRET-VALUE",
            "user_id": "42",
            "user_name": "u1",
            "real_name": "Tester",
            "jwt_payload": {"exp": now + 3600, "user_id": "42"},
            "expires_at": now + 3600,
        },
        "mobile_jwgl": {
            "token": "MOBILE-TOKEN-SECRET-VALUE",
            "expires_at": now + 3600,
        },
        "cas_logins_total": 2,
    }


def test_session_file_permissions_and_status_redaction(monkeypatch, tmp_path):
    monkeypatch.setenv("BUPT_CLI_CACHE_DIR", str(tmp_path))
    save_session_file(_fake_session_data())
    path = session_path()
    assert path.exists()
    assert (path.stat().st_mode & 0o777) == 0o600

    credentials = Credentials("u1", "CAS-PASSWORD")
    session = BuptSession(credentials)
    status = session.status()
    text = json.dumps(status, ensure_ascii=False)
    for secret in (
        "COOKIE-SECRET-VALUE",
        "ACCESS-TOKEN-SECRET-VALUE",
        "REFRESH-TOKEN-SECRET-VALUE",
        "MOBILE-TOKEN-SECRET-VALUE",
        "CAS-PASSWORD",
        "JWGL-PASSWORD",
    ):
        assert secret not in text
    assert status["cookie_count"] == 1
    assert status["ucloud"]["present"] is True
    assert status["mobile_jwgl"]["present"] is True
    assert status["locally_valid"] is True


def test_session_cache_username_mismatch(monkeypatch, tmp_path):
    monkeypatch.setenv("BUPT_CLI_CACHE_DIR", str(tmp_path))
    save_session_file(_fake_session_data())
    session = BuptSession(Credentials("other", "pw"))
    assert session.cache_mismatch is True
    assert session.has_cached_session is False
    assert session.status()["exists"] is False


def test_delete_session(monkeypatch, tmp_path):
    monkeypatch.setenv("BUPT_CLI_CACHE_DIR", str(tmp_path))
    save_session_file(_fake_session_data())
    assert delete_session() is True
    assert not session_path().exists()
    assert delete_session() is False


def test_credentials_env_overrides_config(monkeypatch, tmp_path):
    monkeypatch.setenv("BUPT_CLI_CONFIG_DIR", str(tmp_path))
    (tmp_path / "credentials.json").write_text(
        json.dumps({"username": "cfg-user", "password": "cfg-pass"}),
        encoding="utf-8",
    )
    monkeypatch.setenv("BUPT_USERNAME", "env-user")
    monkeypatch.setenv("BUPT_PASSWORD", "env-pass")
    credentials = load_credentials()
    assert credentials.username == "env-user"
    assert credentials.password == "env-pass"
    # One account: the academic systems share the CAS credentials.
    assert credentials.password == "env-pass"


def test_credentials_missing_raises_without_values(monkeypatch, tmp_path):
    monkeypatch.setenv("BUPT_CLI_CONFIG_DIR", str(tmp_path))
    monkeypatch.delenv("BUPT_USERNAME", raising=False)
    monkeypatch.delenv("BUPT_PASSWORD", raising=False)
    with pytest.raises(AuthError) as excinfo:
        load_credentials()
    assert "not configured" in str(excinfo.value)


def test_ticket_extraction_from_location_header():
    response = httpx.Response(
        302,
        headers={"location": "https://ucloud.bupt.edu.cn/uclass?ticket=ST-123"},
        request=httpx.Request("GET", "https://auth.bupt.edu.cn/authserver/login"),
    )
    assert _ticket_from_response(response) == "ST-123"
    assert _ticket_from_response(None) == ""


def test_probe_collector_group_filter_and_redaction():
    collector = _Collector({"public"}, timeout=1.0, secrets=["SECRET"])
    collector.add(
        "public",
        "x",
        "GET",
        "https://example.invalid",
        status="ok",
        http_code=200,
        evidence="token SECRET",
    )
    collector.add(
        "ucloud",
        "y",
        "GET",
        "https://example.invalid",
        status="ok",
    )
    assert len(collector.rows) == 1
    assert "SECRET" not in collector.rows[0]["evidence"]
    assert _short("x" * 200).endswith("…")


def test_ucloud_headers_have_single_basic_prefix():
    headers = _ucloud_headers("token-value")
    assert headers["Authorization"].startswith("Basic ")
    assert "Basic Basic" not in headers["Authorization"]


def test_probe_unknown_group_is_usage_error(monkeypatch, tmp_path):
    from bupt_cli.errors import UsageError

    monkeypatch.setenv("BUPT_CLI_CACHE_DIR", str(tmp_path))
    monkeypatch.setenv("BUPT_CLI_CONFIG_DIR", str(tmp_path))
    monkeypatch.delenv("BUPT_USERNAME", raising=False)
    monkeypatch.delenv("BUPT_PASSWORD", raising=False)

    class Args:
        only = "nope"
        probe_timeout = 1.0

    with pytest.raises(UsageError):
        run_probe(Args())


def test_cli_session_manager_relogins_when_cached_cookies_cannot_issue_ticket(
    monkeypatch,
):
    from bupt_cli._vendor.campus_api.bupt.auth import BuptSessionManager
    from bupt_cli._vendor.campus_api.exceptions import CampusAuthError
    from bupt_cli.session import CliSessionManager

    calls = {"token": 0, "login": 0}

    def fake_get_ucloud_token(self, force=False):
        calls["token"] += 1
        if calls["token"] == 1:
            raise CampusAuthError("No CAS ticket issued for UCloud (login may have expired)")
        return {"access_token": "fresh-token", "user_id": "42"}

    def fake_ensure_cas_login(self, force=False):
        calls["login"] += 1
        self.cas_login_count += 1
        return self.client

    monkeypatch.setattr(BuptSessionManager, "get_ucloud_token", fake_get_ucloud_token)
    monkeypatch.setattr(CliSessionManager, "ensure_cas_login", fake_ensure_cas_login)

    manager = CliSessionManager(username="u1", password="pw", timeout=1)
    token = manager.get_ucloud_token()

    assert token["access_token"] == "fresh-token"
    assert calls == {"token": 2, "login": 1}
    assert manager.cas_login_count == 1


def test_cli_session_manager_does_not_relogin_when_login_budget_is_exhausted(
    monkeypatch,
):
    from bupt_cli._vendor.campus_api.bupt.auth import BuptSessionManager
    from bupt_cli._vendor.campus_api.exceptions import CampusAuthError
    from bupt_cli.session import CliSessionManager, _max_logins_per_run

    def fake_get_ucloud_token(self, force=False):
        raise CampusAuthError("No CAS ticket issued for UCloud (login may have expired)")

    def fail_ensure_cas_login(self, force=False):
        raise AssertionError("must not log in past the configured budget")

    monkeypatch.setattr(BuptSessionManager, "get_ucloud_token", fake_get_ucloud_token)
    monkeypatch.setattr(CliSessionManager, "ensure_cas_login", fail_ensure_cas_login)

    manager = CliSessionManager(username="u1", password="pw", timeout=1)
    manager.cas_login_count = _max_logins_per_run()
    with pytest.raises(CampusAuthError):
        manager.get_ucloud_token()


def test_status_tolerates_non_numeric_cookie_expiry(monkeypatch, tmp_path):
    monkeypatch.setenv("BUPT_CLI_CACHE_DIR", str(tmp_path))
    data = _fake_session_data()
    data["cookies"][0]["expires"] = "not-a-number"
    data["cookies"].append(
        {
            "name": "SID2",
            "value": "another-secret",
            "domain": "auth.bupt.edu.cn",
            "path": "/",
            "secure": True,
            "expires": "1900000000",
            "domain_specified": True,
            "domain_initial_dot": False,
            "rest": {},
        }
    )
    save_session_file(data)

    status = BuptSession(Credentials("u1", "pw")).status()

    assert status["cookie_count"] == 2
    assert status["cookies_expire_at"] is not None


def test_save_credentials_preserves_unknown_keys(tmp_path, monkeypatch):
    """Re-login must merge, not rebuild: hand-added keys have to survive."""
    import json

    from bupt_cli.credentials import save_credentials

    monkeypatch.setenv("BUPT_CLI_CONFIG_DIR", str(tmp_path))
    (tmp_path / "credentials.json").write_text(
        json.dumps({"username": "u", "password": "old", "custom_note": "keep me"}),
        encoding="utf-8",
    )

    save_credentials("u", "new", "graduate")

    data = json.loads((tmp_path / "credentials.json").read_text(encoding="utf-8"))
    assert data["password"] == "new"
    assert data["role"] == "graduate"
    assert data["custom_note"] == "keep me"
