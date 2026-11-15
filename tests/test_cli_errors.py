import json

import pytest

from bupt_cli import cli
from bupt_cli._vendor.campus_api.exceptions import CampusNetworkError
from bupt_cli.errors import (
    AuthError,
    NetworkError,
    ParseError,
    UsageError,
    from_campus_error,
)


def _run(monkeypatch, exc, capsys, argv=None):
    def boom(_args):
        raise exc

    monkeypatch.setattr(cli, "_dispatch", boom)
    code = cli.main(argv or ["courses"])
    captured = capsys.readouterr()
    assert captured.out == ""
    payload = json.loads(captured.err)
    assert payload["ok"] is False
    return code, payload


@pytest.mark.parametrize(
    "exc, expected_code, expected_type",
    [
        (UsageError("bad usage"), 2, "usage"),
        (AuthError("bad auth"), 3, "auth"),
        (NetworkError("bad network"), 4, "network"),
        (ParseError("bad upstream"), 5, "parse"),
    ],
)
def test_error_envelope_exit_codes(monkeypatch, capsys, exc, expected_code, expected_type):
    code, payload = _run(monkeypatch, exc, capsys)
    assert code == expected_code
    assert payload["error"]["type"] == expected_type


def test_success_envelope_has_ok(monkeypatch, capsys):
    monkeypatch.setattr(cli, "_dispatch", lambda _args: {"count": 1})
    code = cli.main(["courses"])
    captured = capsys.readouterr()
    assert code == 0
    payload = json.loads(captured.out)
    assert payload == {"ok": True, "count": 1}
    assert captured.err == ""


def test_pretty_flag_after_subcommand(monkeypatch, capsys):
    monkeypatch.setattr(cli, "_dispatch", lambda _args: {"count": 0})
    code = cli.main(["courses", "--pretty"])
    assert code == 0
    out = capsys.readouterr().out
    assert "\n  " in out


def test_usage_error_for_missing_command(monkeypatch, capsys, tmp_path):
    monkeypatch.setenv("BUPT_CLI_CACHE_DIR", str(tmp_path))
    code = cli.main([])
    payload = json.loads(capsys.readouterr().err)
    assert code == 2
    assert payload["error"]["type"] == "usage"


@pytest.mark.parametrize(
    "argv, expected_in_output",
    [
        (["help"], "commands:"),
        (["help", "homework"], "homework commands:"),
        (["help", "homework", "list"], "--undone"),
    ],
)
def test_help_subcommand_prints_usage(capsys, argv, expected_in_output):
    with pytest.raises(SystemExit) as excinfo:
        cli.main(argv)
    assert excinfo.value.code == 0
    assert expected_in_output in capsys.readouterr().out


def test_help_subcommand_rejects_unknown_topic(monkeypatch, capsys, tmp_path):
    monkeypatch.setenv("BUPT_CLI_CACHE_DIR", str(tmp_path))
    code = cli.main(["help", "nope"])
    payload = json.loads(capsys.readouterr().err)
    assert code == 2
    assert payload["error"]["type"] == "usage"


def test_unknown_option_is_json_usage_error(capsys, tmp_path):
    monkeypatch_env = {"BUPT_CLI_CACHE_DIR": str(tmp_path)}
    import os

    old = {key: os.environ.get(key) for key in monkeypatch_env}
    try:
        os.environ.update(monkeypatch_env)
        code = cli.main(["courses", "--definitely-not-an-option"])
    finally:
        for key, value in old.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value
    payload = json.loads(capsys.readouterr().err)
    assert code == 2
    assert payload["error"]["type"] == "usage"


def test_http_status_network_error_maps_upstream():
    error = from_campus_error(
        CampusNetworkError(
            "UCloud API /ykt-site/work/detail failed: Client error '400 Bad Request' "
            "for url 'https://apiucloud.bupt.edu.cn/ykt-site/work/detail'"
        )
    )
    assert error.exit_code == 5
    assert error.error_type == "upstream"


def test_http_401_network_error_maps_auth():
    error = from_campus_error(
        CampusNetworkError("Client error '401 Unauthorized' for url 'https://x'")
    )
    assert error.exit_code == 3
    assert error.error_type == "auth"


@pytest.mark.parametrize(
    "argv",
    [
        ["--timeout", "0", "courses"],
        ["homework", "list", "--limit", "-1"],
        ["homework", "list", "--new", "-1"],
        ["notices", "--count", "-1"],
        ["probe", "--request-timeout", "0"],
    ],
)
def test_invalid_numeric_arguments_are_usage_errors(monkeypatch, capsys, tmp_path, argv):
    monkeypatch.setenv("BUPT_CLI_CACHE_DIR", str(tmp_path / "cache"))
    monkeypatch.setenv("BUPT_CLI_CONFIG_DIR", str(tmp_path / "config"))
    monkeypatch.delenv("BUPT_USERNAME", raising=False)
    monkeypatch.delenv("BUPT_PASSWORD", raising=False)

    code = cli.main(argv)

    captured = capsys.readouterr()
    assert captured.out == ""
    payload = json.loads(captured.err)
    assert code == 2
    assert payload["ok"] is False
    assert payload["error"]["type"] == "usage"


def test_cli_error_messages_are_redacted(monkeypatch, capsys, tmp_path):
    monkeypatch.setenv("BUPT_CLI_CONFIG_DIR", str(tmp_path / "config"))
    monkeypatch.setenv("BUPT_USERNAME", "u1")
    monkeypatch.setenv("BUPT_PASSWORD", "s3cr3t-pw")
    monkeypatch.setattr(
        cli,
        "_dispatch",
        lambda _args: (_ for _ in ()).throw(AuthError("upstream echoed s3cr3t-pw")),
    )

    code = cli.main(["courses"])

    captured = capsys.readouterr()
    assert code == 3
    assert "s3cr3t-pw" not in captured.err
    assert "[REDACTED]" in captured.err


def test_jwgl_auth_failure_mentions_undergraduate_only(capsys, tmp_path):
    """JWGL auth errors must not read as 'your password is wrong'.

    Graduate-student ids do not exist in the JWGL backends, so the hint has to
    say so and warn against retrying (which can lock the account).
    """
    from bupt_cli._vendor.campus_api.exceptions import CampusAuthError

    message = "JWGL login failed: invalid username or password"
    assert cli._is_jwgl_auth_failure(message) is True

    cli_exc = cli.from_campus_error(CampusAuthError(message))
    assert isinstance(cli_exc, AuthError)
    hint = cli.JWGL_AUTH_HINT
    assert "undergraduate" in hint
    assert "graduate" in hint
    assert "lock" in hint


def test_jwgl_chinese_rejection_is_recognised():
    """The mobile backend answers in Chinese; that path must match too."""
    message = "Mobile JWGL login failed: 该帐号不存在或密码错误！"
    assert cli._is_jwgl_auth_failure(message) is True


def test_non_jwgl_auth_failure_keeps_generic_hint():
    """A CAS failure must not be blamed on the JWGL/undergraduate situation."""
    assert cli._is_jwgl_auth_failure("BUPT CAS login failed: invalid username") is False


def test_help_covers_every_registered_command():
    """`bupt help <cmd>` must accept every command the parser knows.

    Regression: the topic check used a hardcoded list, so a newly added
    command (library) was rejected even though it existed.
    """
    names = cli._command_names()
    assert "library" in names
    for name in names:
        assert cli._rewrite_help(["help", name]) == [name, "--help"]


def test_uninstall_never_targets_source_files(tmp_path, monkeypatch):
    """Uninstall must only consider executables that look like bupt.

    Regression: the candidate scan matched any file containing "bupt_cli",
    which included src/bupt_cli/__main__.py — deleting source code.
    """
    from bupt_cli import commands

    fake = tmp_path / "__main__.py"
    fake.write_text("from bupt_cli.cli import main\n", encoding="utf-8")

    launcher = tmp_path / "bupt"
    launcher.write_text("#!/bin/sh\nexec python -m bupt_cli \"$@\"\n", encoding="utf-8")
    launcher.chmod(0o755)

    # The helper is defined inside cmd_uninstall, so assert the invariant via a
    # real dry run in an isolated HOME/PATH.
    monkeypatch.setenv("PATH", str(tmp_path))
    monkeypatch.setenv("HOME", str(tmp_path))
    import argparse

    args = argparse.Namespace(dry_run=True, keep_data=True)
    result = commands.cmd_uninstall(args)

    removed = [str(x) for x in result.get("removed", [])]
    assert str(fake) not in removed
    assert all(not p.endswith("__main__.py") for p in removed)


def test_looks_like_bupt_recognises_onefile_binary(tmp_path):
    """Regression: only the first 8 KB was inspected, so PyInstaller's archive
    (which lives at the end, behind the MEI cookie) was never found and
    `bupt uninstall` silently deleted nothing."""
    from bupt_cli import commands

    # Fake a PyInstaller artifact: Mach-O magic + our name in the appendix.
    # Larger than 64 KB so the tail-read path (seek(-65536)) is exercised.
    blob = b"\xcf\xfa\xed\xfe" + b"\x00" * 70000
    blob += b"MEI\x0c\x0b\x0a\x0b\x0e" + b"bupt_cli.commands\x00"
    fake = tmp_path / "bupt"
    fake.write_bytes(blob)
    fake.chmod(0o755)

    import argparse

    # keep_data avoids touching ~/.bupt-cli; PATH/HOME point at tmp_path.
    monkey = pytest.MonkeyPatch()
    try:
        monkey.setenv("PATH", str(tmp_path))
        monkey.setenv("HOME", str(tmp_path))
        args = argparse.Namespace(dry_run=True, keep_data=True)
        result = commands.cmd_uninstall(args)
    finally:
        monkey.undo()

    assert str(fake) in result["removed"]


def test_looks_like_bupt_rejects_random_binary(tmp_path):
    """A Mach-O without our marker must not be deleted."""
    from bupt_cli import commands

    other = tmp_path / "bupt"
    other.write_bytes(b"\xcf\xfa\xed\xfe" + b"\x00" * 20000)
    other.chmod(0o755)

    import argparse

    monkey = pytest.MonkeyPatch()
    try:
        monkey.setenv("PATH", str(tmp_path))
        monkey.setenv("HOME", str(tmp_path))
        args = argparse.Namespace(dry_run=True, keep_data=True)
        result = commands.cmd_uninstall(args)
    finally:
        monkey.undo()

    assert str(other) not in result["removed"]


def test_update_uses_release_redirect_not_the_api(monkeypatch):
    """`update` must not depend on api.github.com.

    Anonymous API access is capped at 60 requests/hour; relying on it would
    force users to configure a GITHUB_TOKEN just to check for updates. The
    version comes from the /releases/latest redirect instead.
    """
    from bupt_cli import commands

    monkeypatch.setattr(commands, "_release_tag", lambda repo, **k: "v9.9.9")
    import argparse

    result = commands.cmd_update(argparse.Namespace(check=True, repo="owner/repo"))
    assert result["ok"] is True
    assert result["latest"] == "9.9.9"
    assert result["update_available"] is True


def test_update_reports_missing_release(monkeypatch):
    from bupt_cli import commands

    monkeypatch.setattr(commands, "_release_tag", lambda repo, **k: None)
    import argparse

    result = commands.cmd_update(argparse.Namespace(check=True, repo="o/r"))
    assert result["ok"] is True
    assert result["latest"] is None
    assert result["update_available"] is False
    assert "没有可用的 release" in result["note"]


def test_update_install_path_from_source_is_honest(monkeypatch):
    """From source there is no binary to replace; say so instead of claiming
    success. Runs the real zip-extraction path."""
    import io
    import zipfile

    from bupt_cli import commands

    monkeypatch.setattr(commands, "_release_tag", lambda repo, **k: "v9.9.9")

    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as zf:
        zf.writestr("bupt", b"#!/bin/sh\necho fake\n")
    payload = buf.getvalue()

    class BinResp:
        def __enter__(self):
            return self

        def __exit__(self, *exc):
            return False

        def read(self):
            return payload

    import urllib.request as urlreq

    seen = {}

    def fake_urlopen(request, *a, **k):
        seen["url"] = request.full_url if hasattr(request, "full_url") else str(request)
        return BinResp()

    monkeypatch.setattr(urlreq, "urlopen", fake_urlopen)

    import argparse

    result = commands.cmd_update(argparse.Namespace(check=False, repo="o/r"))
    assert result["ok"] is True
    assert result["installed_to"] is None
    assert "未自动安装" in result["note"]
    # The asset URL follows the release tag, not an API call.
    assert "/releases/download/v9.9.9/" in seen["url"]
    assert commands.ASSET_NAME in seen["url"]


def test_release_tag_reads_the_redirect(monkeypatch):
    """`_release_tag` extracts the tag from the Location header."""
    from bupt_cli import commands

    class FakeOpener:
        def open(self, request, timeout=None):
            class Resp:
                headers = {
                    "Location": "https://github.com/o/r/releases/tag/v1.2.3"
                }
                url = ""

                def __enter__(self):
                    return self

                def __exit__(self, *exc):
                    return False

            return Resp()

    import urllib.request as urlreq

    monkeypatch.setattr(urlreq, "build_opener", lambda *a, **k: FakeOpener())
    assert commands._release_tag("o/r") == "v1.2.3"


def test_timetable_calendar_uses_the_page_term(monkeypatch):
    """The academic year must come from the page, not a hardcoded constant.

    Regression: commands.py used to call parse_calendar(..., 2026), so every
    date would be a year off once the term rolled over.
    """
    from bupt_cli import timetable

    calendar_html = (
        '<td class="tb_line">学期：2030-2031秋学期</td>'
        '<table id="dgData"><tr><td>一</td>'
        + "".join(f"<td>{d}&nbsp;(9月)</td>" for d in range(2, 9))
        + "</tr></table>"
    )
    schedule_html = "<table><tr><td>节次</td><td>星期一</td></tr></table>"

    from bupt_cli import grad_browser

    monkeypatch.setattr(
        grad_browser,
        "collect",
        lambda user, pwd, want=("schedule", "calendar"): {
            "schedule": schedule_html,
            "calendar": calendar_html,
        },
    )

    entry = timetable.fetch_graduate("u", "p")
    assert entry["term"] == "2030-2031秋学期"
    assert entry["calendar"]["1"][0].startswith("2030-")


def test_timetable_calendar_without_term_raises(monkeypatch):
    """An unknown year must be an error, never a silent fallback."""
    from bupt_cli import grad_browser, timetable

    calendar_html = (
        '<table id="dgData"><tr><td>一</td>'
        + "".join(f"<td>{d}&nbsp;(9月)</td>" for d in range(2, 9))
        + "</tr></table>"
    )
    monkeypatch.setattr(
        grad_browser,
        "collect",
        lambda user, pwd, want=("schedule", "calendar"): {
            "schedule": (
                "<table><tr><th>节次</th><th>星期一</th></tr>"
                "<tr><td>1</td><td></td></tr></table>"
            ),
            "calendar": calendar_html,
        },
    )

    with pytest.raises(timetable.TimetableError):
        timetable.fetch_graduate("u", "p")


def test_uninstall_never_targets_source_files(tmp_path, monkeypatch):
    """Uninstall must only consider executables that look like bupt.

    Regression: the candidate scan matched any file containing "bupt_cli",
    which included src/bupt_cli/__main__.py — deleting source code.
    """
    from bupt_cli import commands

    fake = tmp_path / "__main__.py"
    fake.write_text("from bupt_cli.cli import main\n", encoding="utf-8")

    launcher = tmp_path / "bupt"
    launcher.write_text("#!/bin/sh\nexec python -m bupt_cli \"$@\"\n", encoding="utf-8")
    launcher.chmod(0o755)

    # The helper is defined inside cmd_uninstall, so assert the invariant via a
    # real dry run in an isolated HOME/PATH.
    monkeypatch.setenv("PATH", str(tmp_path))
    monkeypatch.setenv("HOME", str(tmp_path))
    import argparse

    args = argparse.Namespace(dry_run=True, keep_data=True)
    result = commands.cmd_uninstall(args)

    removed = [str(x) for x in result.get("removed", [])]
    assert str(fake) not in removed
    assert all(not p.endswith("__main__.py") for p in removed)


def test_looks_like_bupt_recognises_onefile_binary(tmp_path):
    """Regression: only the first 8 KB was inspected, so PyInstaller's archive
    (which lives at the end, behind the MEI cookie) was never found and
    `bupt uninstall` silently deleted nothing."""
    from bupt_cli import commands

    # Fake a PyInstaller artifact: Mach-O magic + our name in the appendix.
    # Larger than 64 KB so the tail-read path (seek(-65536)) is exercised.
    blob = b"\xcf\xfa\xed\xfe" + b"\x00" * 70000
    blob += b"MEI\x0c\x0b\x0a\x0b\x0e" + b"bupt_cli.commands\x00"
    fake = tmp_path / "bupt"
    fake.write_bytes(blob)
    fake.chmod(0o755)

    import argparse

    # keep_data avoids touching ~/.bupt-cli; PATH/HOME point at tmp_path.
    monkey = pytest.MonkeyPatch()
    try:
        monkey.setenv("PATH", str(tmp_path))
        monkey.setenv("HOME", str(tmp_path))
        args = argparse.Namespace(dry_run=True, keep_data=True)
        result = commands.cmd_uninstall(args)
    finally:
        monkey.undo()

    assert str(fake) in result["removed"]


def test_looks_like_bupt_rejects_random_binary(tmp_path):
    """A Mach-O without our marker must not be deleted."""
    from bupt_cli import commands

    other = tmp_path / "bupt"
    other.write_bytes(b"\xcf\xfa\xed\xfe" + b"\x00" * 20000)
    other.chmod(0o755)

    import argparse

    monkey = pytest.MonkeyPatch()
    try:
        monkey.setenv("PATH", str(tmp_path))
        monkey.setenv("HOME", str(tmp_path))
        args = argparse.Namespace(dry_run=True, keep_data=True)
        result = commands.cmd_uninstall(args)
    finally:
        monkey.undo()

    assert str(other) not in result["removed"]


def test_update_uses_release_redirect_not_the_api(monkeypatch):
    """`update` must not depend on api.github.com.

    Anonymous API access is capped at 60 requests/hour; relying on it would
    force users to configure a GITHUB_TOKEN just to check for updates. The
    version comes from the /releases/latest redirect instead.
    """
    from bupt_cli import commands

    monkeypatch.setattr(commands, "_release_tag", lambda repo, **k: "v9.9.9")
    import argparse

    result = commands.cmd_update(argparse.Namespace(check=True, repo="owner/repo"))
    assert result["ok"] is True
    assert result["latest"] == "9.9.9"
    assert result["update_available"] is True


def test_update_reports_missing_release(monkeypatch):
    from bupt_cli import commands

    monkeypatch.setattr(commands, "_release_tag", lambda repo, **k: None)
    import argparse

    result = commands.cmd_update(argparse.Namespace(check=True, repo="o/r"))
    assert result["ok"] is True
    assert result["latest"] is None
    assert result["update_available"] is False
    assert "没有可用的 release" in result["note"]


def test_update_install_path_from_source_is_honest(monkeypatch):
    """From source there is no binary to replace; say so instead of claiming
    success. Runs the real zip-extraction path."""
    import io
    import zipfile

    from bupt_cli import commands

    monkeypatch.setattr(commands, "_release_tag", lambda repo, **k: "v9.9.9")

    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as zf:
        zf.writestr("bupt", b"#!/bin/sh\necho fake\n")
    payload = buf.getvalue()

    class BinResp:
        def __enter__(self):
            return self

        def __exit__(self, *exc):
            return False

        def read(self):
            return payload

    import urllib.request as urlreq

    seen = {}

    def fake_urlopen(request, *a, **k):
        seen["url"] = request.full_url if hasattr(request, "full_url") else str(request)
        return BinResp()

    monkeypatch.setattr(urlreq, "urlopen", fake_urlopen)

    import argparse

    result = commands.cmd_update(argparse.Namespace(check=False, repo="o/r"))
    assert result["ok"] is True
    assert result["installed_to"] is None
    assert "未自动安装" in result["note"]
    # The asset URL follows the release tag, not an API call.
    assert "/releases/download/v9.9.9/" in seen["url"]
    assert commands.ASSET_NAME in seen["url"]


def test_release_tag_reads_the_redirect(monkeypatch):
    """`_release_tag` extracts the tag from the Location header."""
    from bupt_cli import commands

    class FakeOpener:
        def open(self, request, timeout=None):
            class Resp:
                headers = {
                    "Location": "https://github.com/o/r/releases/tag/v1.2.3"
                }
                url = ""

                def __enter__(self):
                    return self

                def __exit__(self, *exc):
                    return False

            return Resp()

    import urllib.request as urlreq

    monkeypatch.setattr(urlreq, "build_opener", lambda *a, **k: FakeOpener())
    assert commands._release_tag("o/r") == "v1.2.3"


def test_uninstall_refuses_home_and_root(monkeypatch, tmp_path):
    """BUPT_CLI_CONFIG_DIR pointing at $HOME (or /) must never be rmtree'd."""
    import argparse

    from bupt_cli import commands

    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("PATH", "/usr/bin:/bin")

    for dangerous in (str(tmp_path), "/"):
        monkeypatch.setenv("BUPT_CLI_CONFIG_DIR", dangerous)
        monkeypatch.setenv("BUPT_CLI_CACHE_DIR", str(tmp_path / "cache"))
        result = commands.cmd_uninstall(
            argparse.Namespace(dry_run=True, keep_data=False)
        )
        assert dangerous not in result["removed"]
        assert any("跳过" in s for s in result["skipped"])


def test_uninstall_removes_a_real_data_dir(monkeypatch, tmp_path):
    """The guard must not block a genuine bupt data directory."""
    import argparse
    import json

    from bupt_cli import commands

    data = tmp_path / ".bupt-cli"
    data.mkdir()
    (data / "credentials.json").write_text(json.dumps({"username": "u"}), encoding="utf-8")

    monkeypatch.setenv("HOME", str(tmp_path / "home"))
    monkeypatch.setenv("PATH", "/usr/bin:/bin")
    monkeypatch.setenv("BUPT_CLI_CONFIG_DIR", str(data))
    monkeypatch.setenv("BUPT_CLI_CACHE_DIR", str(data))

    result = commands.cmd_uninstall(argparse.Namespace(dry_run=True, keep_data=False))
    assert str(data) in result["removed"]


def test_status_works_without_any_credentials(tmp_path, monkeypatch, capsys):
    """A fresh machine has no credentials; `status` must still answer.

    Regression: load_credentials(required=False) built Credentials with four
    positional arguments after the dataclass shrank to three fields, so both
    `bupt status` and `bupt probe` raised TypeError before doing anything.
    """
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("BUPT_CLI_CONFIG_DIR", str(tmp_path / "cfg"))
    monkeypatch.delenv("BUPT_USERNAME", raising=False)
    monkeypatch.delenv("BUPT_PASSWORD", raising=False)

    code = cli.main(["status"])
    payload = json.loads(capsys.readouterr().out)
    assert code == 0
    assert payload["ok"] is True
    assert payload["exists"] is False


def test_probe_runs_without_credentials(tmp_path, monkeypatch, capsys):
    """`probe` is advertised as credential-free; it must not need an account."""
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("BUPT_CLI_CONFIG_DIR", str(tmp_path / "cfg"))
    monkeypatch.delenv("BUPT_USERNAME", raising=False)
    monkeypatch.delenv("BUPT_PASSWORD", raising=False)

    code = cli.main(["probe", "--only", "cas"])
    out = capsys.readouterr().out
    assert code == 0
    assert '"ok":true' in out


def test_update_replaces_a_frozen_binary(monkeypatch, tmp_path):
    """The frozen path must swap the file, keep a backup and leave no .new."""
    import io
    import sys
    import zipfile

    from bupt_cli import commands

    target = tmp_path / "bupt"
    target.write_bytes(b"OLD-BINARY")
    target.chmod(0o755)

    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as zf:
        zf.writestr("bupt", b"NEW-BINARY")
    payload = buf.getvalue()

    class BinResp:
        def __enter__(self):
            return self

        def __exit__(self, *exc):
            return False

        def read(self):
            return payload

    import urllib.request as urlreq

    monkeypatch.setattr(urlreq, "urlopen", lambda *a, **k: BinResp())
    monkeypatch.setattr(commands, "_release_tag", lambda repo, **k: "v9.9.9")
    monkeypatch.setattr(sys, "frozen", True, raising=False)
    monkeypatch.setattr(sys, "executable", str(target))

    import argparse

    result = commands.cmd_update(argparse.Namespace(check=False, repo="o/r"))

    assert result["installed_to"] == str(target)
    assert target.read_bytes() == b"NEW-BINARY"
    assert target.stat().st_mode & 0o777 == 0o755
    assert (tmp_path / "bupt.old").read_bytes() == b"OLD-BINARY"
    assert not (tmp_path / "bupt.new").exists()
    assert "已更新到" in result["note"]


@pytest.mark.parametrize("command", ["exams", "scores", "classrooms"])
def test_undergraduate_jwgl_commands_refuse_graduate_accounts(
    command, tmp_path, monkeypatch, capsys
):
    """A graduate account must be refused before any login is attempted.

    Regression: the command used to try the undergraduate JWGL login, spend a
    real attempt and report "invalid username or password" — which reads like
    a wrong password and invites retries that lock the account.
    """
    import json

    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("BUPT_CLI_CONFIG_DIR", str(tmp_path))
    (tmp_path / "credentials.json").write_text(
        json.dumps({"username": "u", "password": "p", "role": "graduate"}),
        encoding="utf-8",
    )

    code = cli.main([command])
    captured = capsys.readouterr()
    payload = json.loads(captured.err)

    assert code == 2
    assert payload["error"]["type"] == "usage"
    assert "本科教务" in payload["error"]["message"]
    assert "bupt schedule" in payload["error"]["hint"]
    # Nothing should have reached the network layer.
    assert captured.out == ""


def test_undergraduate_role_is_not_blocked(tmp_path, monkeypatch):
    """The guard must only fire for graduate accounts."""
    import json

    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("BUPT_CLI_CONFIG_DIR", str(tmp_path))
    (tmp_path / "credentials.json").write_text(
        json.dumps({"username": "u", "password": "p", "role": "undergraduate"}),
        encoding="utf-8",
    )

    from bupt_cli import commands

    # No exception means it proceeded past the role check (the network call
    # will fail, but that is a different error type).
    try:
        commands._require_undergraduate("exams")
    except Exception as exc:  # pragma: no cover - should not happen
        pytest.fail(f"undergraduate role was refused: {exc}")
