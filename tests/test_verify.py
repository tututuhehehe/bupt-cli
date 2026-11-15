"""Offline tests for post-submit verification.

Every upstream call is faked: these tests never touch apiucloud.bupt.edu.cn and
never call a write endpoint.
"""

import json
import time

import httpx
import pytest

from bupt_cli import cli, commands
from bupt_cli._vendor.campus_api.bupt.models import CourseSite, HomeworkDetail, HomeworkItem
from bupt_cli._vendor.campus_api.exceptions import CampusNetworkError
from bupt_cli.verify import verify_submission

ASSIGNMENT_ID = "1000000000000000001"
SUBMIT_TIME = "2026-10-22 16:26"


def _view(**overrides):
    payload = {
        "assignmentId": ASSIGNMENT_ID,
        "attachmentIds": ["res-1"],
        "assignmentContent": "<p>hello    world</p>",
        "assignmentStatus": 0,
    }
    payload.update(overrides)
    return payload


def _record(**overrides):
    values = {
        "activity_id": ASSIGNMENT_ID,
        "activity_name": "作业一",
        "site_id": "site-1",
        "site_name": "课程一",
        "end_time": "2026-10-26 23:59",
        "submit_time": SUBMIT_TIME,
        "is_submitted": True,
    }
    values.update(overrides)
    return HomeworkItem(**values)


def _call(*, views, records, expected_ids=("res-1",), text="hello world", wait=5.0):
    """Run verify_submission with sequences of views/records and fake sleep."""
    sleeps = []
    view_calls = []
    record_calls = []
    view_items = list(views)
    record_items = list(records)

    def fetch_view():
        call = view_calls.__len__()
        view_calls.append(call)
        item = view_items[min(call, len(view_items) - 1)]
        if isinstance(item, BaseException):
            raise item
        return item

    def fetch_record():
        call = record_calls.__len__()
        record_calls.append(call)
        item = record_items[min(call, len(record_items) - 1)]
        if isinstance(item, BaseException):
            raise item
        return item

    result = verify_submission(
        fetch_submit_view=fetch_view,
        fetch_list_record=fetch_record,
        assignment_id=ASSIGNMENT_ID,
        expected_attachment_ids=list(expected_ids),
        expected_text=text,
        wait=wait,
        sleep=sleeps.append,
    )
    return result, sleeps, view_calls, record_calls


# ---------------------------------------------------------------------------
# Unit tests: verify_submission.
# ---------------------------------------------------------------------------


def test_passes_on_first_attempt_and_sleeps_once():
    result, sleeps, views, records = _call(views=[_view()], records=[_record()])

    assert result["status"] == "passed"
    assert result["attempts"] == 1
    assert result["wait_seconds"] == 5.0
    assert sleeps == []
    assert len(views) == len(records) == 1
    assert result["checks"]["submit_view"]["status"] == "passed"
    assert result["checks"]["submit_view"]["content_checked"] is True
    assert result["checks"]["list"]["status"] == "passed"
    assert result["checks"]["list"]["submitted_at"] == SUBMIT_TIME
    assert "failed_checks" not in result
    assert "hint" not in result


def test_passes_on_second_attempt_after_stale_attachment_ids():
    result, sleeps, views, records = _call(
        views=[_view(attachmentIds=["stale"]), _view()],
        records=[_record(), _record()],
    )

    assert result["status"] == "passed"
    assert result["attempts"] == 2
    assert sleeps == [5.0]
    assert len(views) == len(records) == 2
    assert result["checks"]["submit_view"]["observed_attachment_ids"] == ["res-1"]


def test_mismatch_on_both_attempts_is_failed():
    result, sleeps, views, records = _call(
        views=[_view(attachmentIds=["other"])],
        records=[None],
    )

    assert result["status"] == "failed"
    assert result["attempts"] == 2
    assert sleeps == [5.0]
    assert len(views) == len(records) == 2
    assert result["failed_checks"] == ["submit_view", "list"]
    assert result["checks"]["submit_view"]["observed_attachment_ids"] == ["other"]
    assert result["checks"]["submit_view"]["expected_attachment_ids"] == ["res-1"]
    assert result["checks"]["list"]["found"] is False
    assert result["checks"]["list"]["submitted"] is False
    assert "upstream reported success" in result["hint"]


def test_content_mismatch_when_text_expected():
    result, _, _, _ = _call(
        views=[_view(assignmentContent="<div>goodbye</div>")],
        records=[_record()],
    )

    assert result["status"] == "failed"
    assert result["failed_checks"] == ["submit_view"]
    check = result["checks"]["submit_view"]
    assert check["content_checked"] is True
    assert check["expected_text"] == "hello world"
    assert check["observed_text"] == "goodbye"


def test_content_ignored_when_text_not_given():
    result, _, _, _ = _call(
        views=[_view(assignmentContent="<div>goodbye</div>")],
        records=[_record()],
        text=None,
    )

    assert result["status"] == "passed"
    check = result["checks"]["submit_view"]
    assert check["content_checked"] is False
    assert "expected_text" not in check
    assert "observed_text" not in check


def test_blank_expected_text_skips_content_check():
    result, _, _, _ = _call(
        views=[_view(assignmentContent="<div>goodbye</div>")],
        records=[_record()],
        text="   ",
    )

    assert result["status"] == "passed"
    check = result["checks"]["submit_view"]
    assert check["content_checked"] is False
    assert "expected_text" not in check
    assert "observed_text" not in check


def test_sentinel_assignment_id_without_attachments_fails():
    result, _, _, _ = _call(
        views=[_view(assignmentId=-1, attachmentIds=[])],
        records=[_record()],
        expected_ids=(),
        text=None,
    )

    assert result["status"] == "failed"
    assert result["failed_checks"] == ["submit_view"]
    check = result["checks"]["submit_view"]
    assert check["has_submission"] is False
    assert check["observed_attachment_ids"] == []


def test_expected_no_attachments_passes_when_none_submitted():
    result, _, _, _ = _call(
        views=[_view(attachmentIds=[])],
        records=[_record()],
        expected_ids=(),
        text=None,
    )

    assert result["status"] == "passed"
    assert result["checks"]["submit_view"]["observed_attachment_ids"] == []


@pytest.mark.parametrize(
    "exc",
    [
        CampusNetworkError("submit-view down"),
        httpx.ConnectError("connection refused"),
    ],
)
def test_network_error_on_last_attempt_is_error(exc):
    result, sleeps, views, records = _call(
        views=[_view(attachmentIds=["stale"]), exc],
        records=[_record(), _record()],
    )

    assert result["status"] == "error"
    assert result["attempts"] == 2
    assert sleeps == [5.0]
    assert len(views) == len(records) == 2
    assert result["checks"]["submit_view"]["status"] == "error"
    assert result["checks"]["submit_view"]["error"] == str(exc)
    assert "hint" in result


def test_error_message_is_redacted_and_bounded():
    secret = "secret-token-value"
    exc = CampusNetworkError("boom " + secret + " " + "x" * 500)
    result = verify_submission(
        fetch_submit_view=lambda: (_ for _ in ()).throw(exc),
        fetch_list_record=lambda: _record(),
        assignment_id=ASSIGNMENT_ID,
        expected_attachment_ids=[],
        expected_text=None,
        wait=0,
        sleep=lambda _wait: None,
        secrets=[secret],
    )

    message = result["checks"]["submit_view"]["error"]
    assert secret not in message
    assert "[REDACTED]" in message
    assert len(message) <= 200


def test_unexpected_check_exception_is_error_not_crash():
    result = verify_submission(
        fetch_submit_view=lambda: (_ for _ in ()).throw(RuntimeError("shape drift")),
        fetch_list_record=lambda: _record(),
        assignment_id=ASSIGNMENT_ID,
        expected_attachment_ids=[],
        expected_text=None,
        wait=0,
        sleep=lambda _wait: None,
    )

    assert result["status"] == "error"
    assert result["checks"]["submit_view"]["error"] == "shape drift"


def test_observed_and_expected_text_are_truncated():
    long_text = "a" * 300
    result = verify_submission(
        fetch_submit_view=lambda: _view(assignmentContent=long_text),
        fetch_list_record=lambda: _record(),
        assignment_id=ASSIGNMENT_ID,
        expected_attachment_ids=["res-1"],
        expected_text=long_text,
        wait=0,
        sleep=lambda _wait: None,
    )

    assert result["status"] == "passed"
    check = result["checks"]["submit_view"]
    assert len(check["expected_text"]) == 200
    assert len(check["observed_text"]) == 200


# ---------------------------------------------------------------------------
# Command-level tests: cmd_homework_submit with a fully faked environment.
# ---------------------------------------------------------------------------


class FakeHomework:
    def __init__(self, record, calls):
        self.record = record
        self.calls = calls

    def find_list_record(self, assignment_id, site_ids):
        self.calls.append(("find_list_record", str(assignment_id), list(site_ids)))
        return self.record


class FakeCasManager:
    def get_ucloud_token(self):
        return {"access_token": "test-token", "user_id": "user-1"}


class FakeClient:
    def __init__(self, detail, record, calls):
        self.detail = detail
        self.calls = calls
        self.homework = FakeHomework(record, calls)
        self._cas_session_mgr = FakeCasManager()

    def get_homework_detail(self, activity_id):
        self.calls.append(("detail", str(activity_id)))
        return self.detail

    def get_enrolled_courses(self):
        self.calls.append(("courses",))
        return [CourseSite(site_id="site-1", site_name="课程一")]


def _detail():
    payload = {
        "id": ASSIGNMENT_ID,
        "assignmentTitle": "作业一",
        "className": "课程一",
        "assignmentStatus": 0,
    }
    return HomeworkDetail(
        activity_id=ASSIGNMENT_ID,
        title="作业一",
        content_html="",
        attachments=[],
        is_submitted=True,
        raw=payload,
    )


def _install(monkeypatch, client):
    monkeypatch.setattr(commands, "_session", lambda _args, **_: object())
    monkeypatch.setattr(commands, "_client", lambda _session, **_: client)
    monkeypatch.setattr(commands, "_persist", lambda _session: None)


def _install_view_sequence(monkeypatch, sequence):
    """Patch get_submit_view; the last item repeats, exceptions raise."""
    calls = {"count": 0}
    items = list(sequence)

    def fake_view(token, assignment_id, timeout=0):
        index = calls["count"]
        calls["count"] += 1
        item = items[min(index, len(items) - 1)]
        if isinstance(item, BaseException):
            raise item
        return item

    monkeypatch.setattr(commands, "get_submit_view", fake_view)
    return calls


def _install_submit(monkeypatch):
    state = {"calls": 0}

    def fake_submit(*args, **kwargs):
        state["calls"] += 1
        return {"success": True, "code": 200, "msg": "ok"}

    monkeypatch.setattr(commands, "submit_homework", fake_submit)
    return state


@pytest.fixture
def sleeps(monkeypatch):
    recorded = []
    monkeypatch.setattr(time, "sleep", lambda seconds: recorded.append(seconds))
    return recorded


def _run_submit(capsys, *extra):
    code = cli.main(
        [
            "homework",
            "submit",
            ASSIGNMENT_ID,
            "--text",
            "hello world",
            "--i-confirm-submit",
            *extra,
        ]
    )
    captured = capsys.readouterr()
    return code, captured


def _run_verify(capsys, *extra):
    code = cli.main(
        ["homework", "verify", ASSIGNMENT_ID, *extra]
    )
    captured = capsys.readouterr()
    return code, captured


def test_submit_verified_pass_exits_zero(monkeypatch, capsys, sleeps):
    calls = []
    client = FakeClient(_detail(), _record(), calls)
    _install(monkeypatch, client)
    _install_view_sequence(monkeypatch, [_view(attachmentIds=[]), _view(attachmentIds=[])])
    submit_state = _install_submit(monkeypatch)

    code, captured = _run_submit(capsys)

    assert code == 0
    assert captured.err == ""
    payload = json.loads(captured.out)
    assert payload["ok"] is True
    assert payload["submitted"] is True
    assert payload["verified"] is True
    assert payload["submitted_at"] == SUBMIT_TIME
    assert payload["verification"]["status"] == "passed"
    assert "_exit_code" not in payload
    assert submit_state["calls"] == 1
    assert sleeps == []


def test_submit_unverified_mismatch_exits_six(monkeypatch, capsys, sleeps):
    calls = []
    client = FakeClient(_detail(), _record(), calls)
    _install(monkeypatch, client)
    _install_view_sequence(monkeypatch, [_view(), _view(attachmentIds=["wrong"])])
    submit_state = _install_submit(monkeypatch)

    code, captured = _run_submit(capsys)

    assert code == 6
    payload = json.loads(captured.out)
    assert payload["verified"] is False
    assert payload["verification"]["status"] == "failed"
    assert payload["verification"]["failed_checks"] == ["submit_view"]
    assert "upstream reported success" in captured.err
    assert submit_state["calls"] == 1
    assert sleeps == [5.0]


def test_submit_verification_error_exits_seven(monkeypatch, capsys, sleeps):
    calls = []
    client = FakeClient(_detail(), _record(), calls)
    _install(monkeypatch, client)
    _install_view_sequence(
        monkeypatch, [_view(), CampusNetworkError("down with test-token")]
    )
    submit_state = _install_submit(monkeypatch)

    code, captured = _run_submit(capsys)

    assert code == 7
    payload = json.loads(captured.out)
    assert payload["verified"] is None
    assert payload["verification"]["status"] == "error"
    assert "warning" in payload
    assert "verification could not run" in payload["warning"]
    assert "test-token" not in payload["warning"]
    assert captured.err.strip()
    assert submit_state["calls"] == 1


def test_submit_no_verify_skips_all_readonly_checks(monkeypatch, capsys, sleeps):
    calls = []
    client = FakeClient(_detail(), _record(), calls)
    _install(monkeypatch, client)
    view_calls = {"count": 0}

    def fake_view(token, assignment_id, timeout=0):
        view_calls["count"] += 1
        if view_calls["count"] > 1:
            raise AssertionError("verification must not call submit-view")
        return _view()

    monkeypatch.setattr(commands, "get_submit_view", fake_view)
    submit_state = _install_submit(monkeypatch)

    code, captured = _run_submit(capsys, "--no-verify")

    assert code == 0
    payload = json.loads(captured.out)
    assert payload["verified"] is None
    assert payload["submitted_at"] is None
    assert payload["verification"] == {"status": "skipped"}
    assert submit_state["calls"] == 1
    assert view_calls["count"] == 1  # only the pre-submit planning view
    assert calls == [("detail", ASSIGNMENT_ID)]
    assert sleeps == []


def test_submit_uploaded_file_is_the_expected_attachment_set(
    monkeypatch, capsys, sleeps, tmp_path
):
    calls = []
    client = FakeClient(_detail(), _record(), calls)
    _install(monkeypatch, client)
    _install_view_sequence(
        monkeypatch,
        [_view(attachmentIds=[]), _view(attachmentIds=["uploaded-rid"])],
    )
    submit_state = _install_submit(monkeypatch)
    uploads = []

    def fake_upload(token, user_id, path, timeout=0, **kwargs):
        uploads.append((token, user_id, str(path)))
        return {
            "resource_id": "uploaded-rid",
            "filename": path.name,
            "size": path.stat().st_size,
            "mime_type": "text/plain",
        }

    monkeypatch.setattr(commands, "upload_homework_file", fake_upload)
    path = tmp_path / "ans.txt"
    path.write_text("answer", encoding="utf-8")

    code = cli.main(
        [
            "homework",
            "submit",
            ASSIGNMENT_ID,
            "--file",
            str(path),
            "--i-confirm-submit",
        ]
    )
    captured = capsys.readouterr()

    assert code == 0
    payload = json.loads(captured.out)
    assert payload["attachment_ids"] == ["uploaded-rid"]
    check = payload["verification"]["checks"]["submit_view"]
    assert check["expected_attachment_ids"] == ["uploaded-rid"]
    assert check["observed_attachment_ids"] == ["uploaded-rid"]
    assert len(uploads) == 1
    assert submit_state["calls"] == 1


def test_submit_dry_run_does_not_upload_or_submit(
    monkeypatch, capsys, sleeps
):
    calls = []
    client = FakeClient(_detail(), _record(), calls)
    _install(monkeypatch, client)
    _install_view_sequence(
        monkeypatch, [_view(), AssertionError("must not be used")]
    )
    submit_state = _install_submit(monkeypatch)

    code = cli.main(
        [
            "homework",
            "submit",
            ASSIGNMENT_ID,
            "--text",
            "hello",
            "--dry-run",
            "--no-verify",
        ]
    )
    captured = capsys.readouterr()

    assert code == 0
    payload = json.loads(captured.out)
    assert payload["dry_run"] is True
    assert "verification" not in payload
    assert submit_state["calls"] == 0
    assert calls == [("detail", ASSIGNMENT_ID)]


def test_verify_subcommand_pass(monkeypatch, capsys, sleeps):
    calls = []
    client = FakeClient(_detail(), _record(), calls)
    _install(monkeypatch, client)
    _install_view_sequence(monkeypatch, [_view()])
    submit_state = _install_submit(monkeypatch)

    code, captured = _run_verify(
        capsys,
        "--resource-id",
        "res-1",
        "--text",
        "hello world",
    )

    assert code == 0
    payload = json.loads(captured.out)
    assert payload["ok"] is True
    assert payload["verified"] is True
    assert payload["submitted_at"] == SUBMIT_TIME
    assert payload["verification"]["status"] == "passed"
    assert "submitted" not in payload
    assert submit_state["calls"] == 0
    assert sleeps == []
    lookups = [call for call in calls if call[0] == "find_list_record"]
    assert lookups == [("find_list_record", ASSIGNMENT_ID, ["site-1"])]


def test_verify_subcommand_failure_exits_six(monkeypatch, capsys, sleeps):
    calls = []
    client = FakeClient(_detail(), _record(), calls)
    _install(monkeypatch, client)
    _install_view_sequence(monkeypatch, [_view(attachmentIds=["wrong"])])
    submit_state = _install_submit(monkeypatch)

    code, captured = _run_verify(
        capsys, "--resource-id", "res-1"
    )

    assert code == 6
    payload = json.loads(captured.out)
    assert payload["verified"] is False
    assert payload["verification"]["status"] == "failed"
    assert payload["verification"]["failed_checks"] == ["submit_view"]
    assert "upstream reported success" not in payload["verification"]["hint"]
    assert "could not be confirmed" in captured.err
    assert submit_state["calls"] == 0
    assert sleeps == [5.0]


def test_verify_without_resource_id_accepts_visible_submission(
    monkeypatch, capsys, sleeps
):
    """A bare `verify` has nothing to compare against, so it must not fail.

    Regression: without --resource-id the expected id list is empty, and the
    old equality check reported every already-submitted assignment as
    unconfirmed (exit 6).
    """
    calls = []
    client = FakeClient(_detail(), _record(), calls)
    _install(monkeypatch, client)
    _install_view_sequence(monkeypatch, [_view(attachmentIds=["2121920224368910337"])])

    code, captured = _run_verify(capsys)

    assert code == 0
    payload = json.loads(captured.out)
    assert payload["verified"] is True
    check = payload["verification"]["checks"]["submit_view"]
    assert check["status"] == "passed"
    assert check["attachments_checked"] is False
    assert check["observed_attachment_ids"] == ["2121920224368910337"]


def test_verify_with_resource_id_still_compares_exactly(monkeypatch, capsys, sleeps):
    """Passing --resource-id keeps the strict comparison."""
    calls = []
    client = FakeClient(_detail(), _record(), calls)
    _install(monkeypatch, client)
    _install_view_sequence(monkeypatch, [_view(attachmentIds=["other-id"])])

    code, captured = _run_verify(
        capsys, "--resource-id", "res-1"
    )

    assert code == 6
    payload = json.loads(captured.out)
    check = payload["verification"]["checks"]["submit_view"]
    assert check["status"] == "failed"
    assert check["attachments_checked"] is True


def test_homework_show_reports_status(monkeypatch, capsys):
    """`show` must expose the same status vocabulary as `list`."""
    calls = []
    client = FakeClient(_detail(), _record(), calls)
    _install(monkeypatch, client)
    _install_view_sequence(monkeypatch, [_view()])

    code = cli.main(["homework", "show", ASSIGNMENT_ID])
    payload = json.loads(capsys.readouterr().out)

    assert code == 0
    assert payload["assignment"]["status"] == "submitted"
