"""Tests for `homework list --new` and the submit confirmation gate.

Both are the parts most worth pinning down:

* `--new` decides what counts as "recently assigned". If it silently drops
  assignments it could not date, "nothing new" becomes indistinguishable from
  "I could not tell" — the same failure mode `--since` was removed for.
* `--i-confirm-submit` is the only thing standing between a command and an
  irreversible submission, so it needs a test that actually reaches it.
"""

import datetime as dt
import json
from types import SimpleNamespace

import pytest

from bupt_cli import cli, commands
from bupt_cli._vendor.campus_api.bupt.models import HomeworkItem


class FakeDetail:
    def __init__(self, publish_iso: str | None):
        self.raw = {"assignmentBeginTime": publish_iso} if publish_iso else {}


def make_item(
    index: int,
    *,
    published_days_ago: float | None,
    deadline_days: int,
    submitted: bool = False,
    site_id: str = "site-1",
) -> HomeworkItem:
    deadline = dt.datetime(2026, 11, 15) + dt.timedelta(days=deadline_days)
    return HomeworkItem(
        activity_id=f"hw{index}",
        activity_name=f"作业 {index}",
        site_id=site_id,
        site_name="测试课程",
        end_time=deadline.strftime("%Y-%m-%d %H:%M:%S"),
        is_submitted=submitted,
    )


class FakeClient:
    """Returns a publish time per assignment, and records every detail call."""

    def __init__(self, publish_days_ago: dict[str, float | None]):
        self.publish_days_ago = publish_days_ago
        self.detail_calls: list[str] = []
        self.fail_ids: set[str] = set()

    def get_homework_detail(self, activity_id: str):
        self.detail_calls.append(activity_id)
        if activity_id in self.fail_ids:
            raise RuntimeError("boom")
        days = self.publish_days_ago.get(activity_id)
        if days is None:
            return FakeDetail(None)
        stamp = dt.datetime.now() - dt.timedelta(days=days)
        return FakeDetail(stamp.strftime("%Y-%m-%d %H:%M:%S"))


def run_list(monkeypatch, items, client, **kwargs):
    monkeypatch.setattr(commands, "_session", lambda args: SimpleNamespace())
    monkeypatch.setattr(commands, "_client", lambda session: client)
    monkeypatch.setattr(commands, "_persist", lambda session: None)
    monkeypatch.setattr(
        commands, "_load_assignments", lambda client, args: (items, "all")
    )
    monkeypatch.setattr(
        commands, "_enrich_undone_courses", lambda client, items, args: None
    )

    args = SimpleNamespace(
        course=kwargs.get("course"),
        undone=kwargs.get("undone", False),
        new=kwargs.get("new"),
        limit=kwargs.get("limit", 0),
    )
    return commands.cmd_homework_list(args)


def test_new_keeps_only_recent_assignments(monkeypatch):
    items = [
        make_item(1, published_days_ago=90, deadline_days=-60),
        make_item(2, published_days_ago=1, deadline_days=5),
    ]
    client = FakeClient({"hw1": 90, "hw2": 1})

    payload = run_list(monkeypatch, items, client, new=7)

    assert [a["id"] for a in payload["assignments"]] == ["hw2"]
    assert payload["assignments"][0]["published_at"].startswith(
        (dt.datetime.now() - dt.timedelta(days=1)).strftime("%Y-%m-%d")
    )
    assert payload["publish_time"]["unknown"] == 0


def test_new_sorts_newest_publication_first(monkeypatch):
    items = [
        make_item(1, published_days_ago=5, deadline_days=10),
        make_item(2, published_days_ago=1, deadline_days=2),
    ]
    client = FakeClient({"hw1": 5, "hw2": 1})

    payload = run_list(monkeypatch, items, client, new=7)
    assert [a["id"] for a in payload["assignments"]] == ["hw2", "hw1"]


def test_new_discloses_assignments_it_could_not_date(monkeypatch):
    """Regression: beyond the lookup cap, undated assignments were dropped and
    the payload still claimed a clean publish-time filter, so a "0 results"
    answer could mean "I could not tell"."""
    # 40 assignments, all with deadlines far newer than... the recent ones are
    # at the end of the list, exactly like course-order aggregation produces.
    items = [
        make_item(i, published_days_ago=None, deadline_days=i) for i in range(40)
    ]
    # Only the first PUBLISH_LOOKUP_LIMIT get a publish time; they are old.
    publish = {f"hw{i}": 90 for i in range(40)}
    client = FakeClient(publish)

    payload = run_list(monkeypatch, items, client, new=7)

    assert payload["count"] == 0
    # Every assignment got dated thanks to newest-deadline-first ordering, so
    # nothing is unknown here; the point is the field exists and is explicit.
    assert payload["publish_time"]["lookup_limit"] == commands.PUBLISH_LOOKUP_LIMIT
    assert "unknown" in payload["publish_time"]


def test_new_reports_unknown_count_when_lookup_budget_runs_out(monkeypatch):
    """When the budget is exhausted, leftovers must be reported, not hidden."""
    items = [
        make_item(i, published_days_ago=None, deadline_days=i) for i in range(40)
    ]
    # Undatable assignments: the detail endpoint returns nothing for all of them.
    client = FakeClient({})

    payload = run_list(monkeypatch, items, client, new=7)

    assert payload["count"] == 0
    assert payload["publish_time"]["unknown"] == 40
    assert len(payload["unknown_publish_time"]) == 40
    assert "拿不到发布时间" in payload["note"]
    # The budget is respected.
    assert len(client.detail_calls) <= commands.PUBLISH_LOOKUP_LIMIT


def test_new_looks_up_only_the_filtered_assignments(monkeypatch):
    """--undone must not spend the lookup budget on already-submitted rows."""
    items = [
        make_item(i, published_days_ago=90, deadline_days=i, submitted=True)
        for i in range(30)
    ] + [
        make_item(100, published_days_ago=1, deadline_days=40),
    ]
    client = FakeClient({**{f"hw{i}": 90 for i in range(30)}, "hw100": 1})

    payload = run_list(monkeypatch, items, client, new=7, undone=True)

    assert [a["id"] for a in payload["assignments"]] == ["hw100"]
    # Only the undone assignment should have been fetched.
    assert client.detail_calls == ["hw100"]


def test_new_reports_detail_failures(monkeypatch):
    items = [make_item(1, published_days_ago=None, deadline_days=5)]
    client = FakeClient({"hw1": 1})
    client.fail_ids = {"hw1"}

    payload = run_list(monkeypatch, items, client, new=7)

    assert payload["count"] == 0
    assert payload["publish_time"]["failed"] == 1
    assert "详情请求失败" in payload["note"]


def test_new_uses_list_publish_time_without_a_detail_call(monkeypatch):
    """A row that already carries assignmentBeginTime needs no extra request."""
    item = make_item(1, published_days_ago=None, deadline_days=5)
    item.raw["assignmentBeginTime"] = dt.datetime.now().strftime("%Y-%m-%d %H:%M:%S")
    client = FakeClient({})

    payload = run_list(monkeypatch, [item], client, new=7)

    assert payload["count"] == 1
    assert client.detail_calls == []


def test_new_with_empty_list_says_so(monkeypatch):
    client = FakeClient({})
    payload = run_list(monkeypatch, [], client, new=7)
    assert payload["count"] == 0
    assert "没有作业" in payload["note"]


def test_list_without_new_never_calls_the_detail_endpoint(monkeypatch):
    items = [make_item(1, published_days_ago=90, deadline_days=5)]
    client = FakeClient({})
    payload = run_list(monkeypatch, items, client)
    assert payload["count"] == 1
    assert client.detail_calls == []
    assert "note" not in payload


# ---------------------------------------------------------------------------
# The confirmation gate
# ---------------------------------------------------------------------------


class _GateHarness:
    def __init__(self):
        self.uploads: list[str] = []
        self.submits: list[str] = []
        self.detail_calls = 0


@pytest.fixture()
def gate(monkeypatch):
    harness = _GateHarness()
    # Verification sleeps between retries; keep the suite instant.
    import time as _time

    monkeypatch.setattr(_time, "sleep", lambda seconds: None)

    class FakeClient:
        def get_homework_detail(self, activity_id):
            harness.detail_calls += 1
            return SimpleNamespace(
                title="作业",
                is_submitted=False,
                raw={},
            )

        class _Mgr:
            @staticmethod
            def get_ucloud_token():
                return {"access_token": "t", "user_id": "u"}

        _cas_session_mgr = _Mgr()

    monkeypatch.setattr(commands, "_session", lambda args: SimpleNamespace())
    monkeypatch.setattr(commands, "_client", lambda session: FakeClient())
    monkeypatch.setattr(commands, "_persist", lambda session: None)
    monkeypatch.setattr(commands, "get_submit_view", lambda *a, **k: {})
    monkeypatch.setattr(
        commands,
        "upload_homework_file",
        lambda token, uid, path, timeout=0: (
            harness.uploads.append(str(path))
            or {"resource_id": "r1", "name": "f"}
        ),
    )
    monkeypatch.setattr(
        commands,
        "submit_homework",
        lambda *a, **k: harness.submits.append("s") or {"ok": True},
    )
    return harness


def _submit_args(tmp_path, **overrides):
    path = tmp_path / "answer.pdf"
    path.write_bytes(b"x")
    base = dict(
        id="hw1",
        file=[str(path)],
        resource_id=[],
        text="",
        dry_run=False,
        i_confirm_submit=False,
        no_verify=False,
    )
    base.update(overrides)
    return SimpleNamespace(**base)


def test_submit_without_confirm_uploads_nothing(gate, tmp_path):
    """Regression: the gate used to run after the upload loop, so a plain
    `submit ID --file x` uploaded the file and only then refused."""
    from bupt_cli.errors import UsageError

    args = _submit_args(tmp_path)

    with pytest.raises(UsageError) as excinfo:
        commands.cmd_homework_submit(args)

    assert "i-confirm-submit" in str(excinfo.value)
    assert gate.uploads == []
    assert gate.submits == []


def test_submit_with_confirm_uploads_then_submits(gate, tmp_path):
    args = _submit_args(tmp_path, i_confirm_submit=True)
    commands.cmd_homework_submit(args)

    assert len(gate.uploads) == 1
    assert len(gate.submits) == 1


def test_dry_run_uploads_and_submits_nothing(gate, tmp_path):
    args = _submit_args(tmp_path, dry_run=True)
    payload = commands.cmd_homework_submit(args)

    assert payload["dry_run"] is True
    assert gate.uploads == []
    assert gate.submits == []


def test_dry_run_and_confirm_are_mutually_exclusive(gate, tmp_path):
    from bupt_cli.errors import UsageError

    with pytest.raises(UsageError):
        commands.cmd_homework_submit(
            _submit_args(tmp_path, dry_run=True, i_confirm_submit=True)
        )
    assert gate.uploads == []
