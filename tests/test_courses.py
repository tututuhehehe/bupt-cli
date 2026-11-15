"""Tests for `bupt courses` pagination.

`--page`/`--size` were removed so that every enrolled course is returned
automatically. Pagination is now the only thing guaranteeing completeness, so
it needs tests that fail when the loop misbehaves — deleting any branch here
used to leave the whole suite green.
"""

from types import SimpleNamespace

import pytest

from bupt_cli import commands


class Course:
    def __init__(self, site_id, name="课程"):
        self.site_id = site_id
        self.site_name = name
        self.teacher_name = ""


class FakeClient:
    """Serves course pages from a dict and records the requested pages."""

    def __init__(self, pages):
        self.pages = pages
        self.requested: list[tuple[int, int]] = []

    def get_enrolled_courses(self, size=50, page=1):
        self.requested.append((size, page))
        return list(self.pages.get(page, []))


@pytest.fixture()
def run_courses(monkeypatch):
    def _run(client):
        monkeypatch.setattr(commands, "_session", lambda args: SimpleNamespace())
        monkeypatch.setattr(commands, "_client", lambda session: client)
        monkeypatch.setattr(commands, "_persist", lambda session: None)
        return commands.cmd_courses(SimpleNamespace())

    return _run


def test_courses_concatenates_pages(run_courses):
    client = FakeClient(
        {
            1: [Course(f"c{i}") for i in range(50)],
            2: [Course(f"c{i}") for i in range(50, 60)],
        }
    )
    payload = run_courses(client)

    assert payload["count"] == 60
    # Page order is preserved.
    assert [c["id"] for c in payload["courses"]][:2] == ["c0", "c1"]
    assert payload["courses"][-1]["id"] == "c59"


def test_courses_deduplicates_across_pages(run_courses):
    client = FakeClient(
        {
            1: [Course(f"c{i}") for i in range(50)],
            # Two ids repeat from page 1, plus one that is new.
            2: [Course("c0"), Course("c1"), Course("new")],
        }
    )
    payload = run_courses(client)

    ids = [c["id"] for c in payload["courses"]]
    assert ids.count("c0") == 1
    assert ids.count("c1") == 1
    assert "new" in ids
    assert payload["count"] == 51


def test_courses_keeps_every_empty_site_id(run_courses):
    """Courses without a site_id cannot be deduplicated without losing them."""
    client = FakeClient({1: [Course(""), Course(""), Course("")]})
    payload = run_courses(client)

    assert payload["count"] == 3


def test_courses_stops_on_a_short_page(run_courses):
    client = FakeClient({1: [Course("a"), Course("b")], 2: [Course("c")]})
    payload = run_courses(client)

    assert payload["count"] == 2
    # A page shorter than the requested size means the end; no second request.
    assert client.requested == [(50, 1)]


def test_courses_stops_on_an_empty_page(run_courses):
    client = FakeClient({1: [Course(f"c{i}") for i in range(50)]})
    payload = run_courses(client)

    assert payload["count"] == 50
    assert [page for _, page in client.requested] == [1, 2]


def test_courses_respects_the_page_limit(run_courses, monkeypatch):
    """A pathological upstream that always returns a full page must not loop."""
    monkeypatch.setattr(commands, "COURSE_PAGE_LIMIT", 3)

    class EndlessClient(FakeClient):
        def get_enrolled_courses(self, size=50, page=1):
            self.requested.append((size, page))
            return [Course(f"p{page}-c{i}") for i in range(50)]

    client = EndlessClient({})
    payload = run_courses(client)

    assert len(client.requested) == 3
    assert payload["count"] == 150


def test_courses_returns_empty_without_error(run_courses):
    payload = run_courses(FakeClient({}))
    assert payload == {"count": 0, "courses": []}


# ---------------------------------------------------------------------------
# `bupt schedule` reads the cache instead of launching a browser
# ---------------------------------------------------------------------------


class _CacheHarness:
    def __init__(self):
        self.refresh_calls = 0


@pytest.fixture()
def cache_harness(monkeypatch):
    """Patch timetable.load / is_stale / _refresh_timetable."""
    from bupt_cli import timetable

    harness = _CacheHarness()
    state = {"cached": None, "stale": True}

    monkeypatch.setattr(timetable, "load", lambda path=None: state["cached"])
    monkeypatch.setattr(
        timetable, "is_stale", lambda entry, today=None: state["stale"]
    )

    def fake_refresh(args):
        harness.refresh_calls += 1
        return {"term": "fresh", "courses": [], "calendar": {}}

    monkeypatch.setattr(commands, "_refresh_timetable", fake_refresh)
    harness.state = state
    return harness


def test_fresh_cache_is_used_without_a_fetch(cache_harness):
    cache_harness.state["cached"] = {"term": "cached"}
    cache_harness.state["stale"] = False

    entry, from_cache = commands._timetable_entry(SimpleNamespace())

    assert from_cache is True
    assert entry["term"] == "cached"
    assert cache_harness.refresh_calls == 0


def test_stale_cache_triggers_a_fetch(cache_harness):
    cache_harness.state["cached"] = {"term": "old"}
    cache_harness.state["stale"] = True

    entry, from_cache = commands._timetable_entry(SimpleNamespace())

    assert from_cache is False
    assert entry["term"] == "fresh"
    assert cache_harness.refresh_calls == 1


def test_missing_cache_triggers_a_fetch(cache_harness):
    cache_harness.state["cached"] = None
    cache_harness.state["stale"] = True

    entry, from_cache = commands._timetable_entry(SimpleNamespace())

    assert from_cache is False
    assert cache_harness.refresh_calls == 1
