from datetime import datetime, timedelta

import httpx
import pytest

from bupt_cli._vendor.campus_api.bupt.models import HomeworkDetail, HomeworkItem
from bupt_cli.errors import UsageError
from bupt_cli.service import (
    assignment_dict,
    detail_dict,
    filter_assignments,
    normalize_attachment,
    parse_datetime,
    publish_time,
)


def make_item(**overrides):
    base = dict(
        activity_id="a1",
        activity_name="作业一",
        site_id="s1",
        site_name="课程一",
        begin_time=None,
        end_time="2026-11-09 23:59",
        is_submitted=False,
        raw={},
    )
    base.update(overrides)
    return HomeworkItem(**base)


def test_parse_datetime_formats():
    assert parse_datetime("2026-08-01") == datetime(2026, 8, 1)
    assert parse_datetime("2026-08-01 12:30:45") == datetime(2026, 8, 1, 12, 30, 45)
    assert parse_datetime("2026-08-01T12:30:45Z") is not None
    assert parse_datetime("2026-08-01 12:30:45.123") == datetime(
        2026, 8, 1, 12, 30, 45, 123000
    )
    assert parse_datetime("") is None
    assert parse_datetime("not-a-date") is None
    assert parse_datetime(None) is None


def test_publish_time_prefers_assignment_begin_time():
    item = make_item(raw={"assignmentBeginTime": "2026-10-19 00:00"})
    assert publish_time(item) == datetime(2026, 10, 19)
    item2 = make_item(begin_time="2026-10-18 08:00", raw={})
    assert publish_time(item2) == datetime(2026, 10, 18, 8)


def test_assignment_dict_status_and_fields():
    # Deadlines are relative to "now" so the test cannot rot as the calendar
    # moves past a hard-coded date.
    now = datetime.now()
    published = now - timedelta(days=3)
    deadline = now + timedelta(days=5)
    item = make_item(
        end_time=deadline.strftime("%Y-%m-%d %H:%M"),
        raw={"assignmentBeginTime": published.strftime("%Y-%m-%d %H:%M")},
    )
    payload = assignment_dict(item)
    assert payload["id"] == "a1"
    assert payload["course"] == "课程一"
    assert payload["status"] == "pending"
    assert payload["published_at"] == published.strftime("%Y-%m-%d %H:%M:00")
    assert payload["deadline"] == deadline.strftime("%Y-%m-%d %H:%M")

    submitted = make_item(is_submitted=True, submit_time="2026-10-20 10:00")
    assert assignment_dict(submitted)["status"] == "submitted"

    overdue = make_item(end_time="2020-01-01 00:00")
    assert assignment_dict(overdue)["status"] == "overdue"


def test_filter_assignments_course_and_undone_semantics():
    items = [
        make_item(activity_id="a1", site_id="s1", end_time="2026-11-01 00:00"),
        make_item(activity_id="a2", site_id="s2", end_time="2026-12-01 00:00"),
        make_item(activity_id="a3", site_id="s1", end_time="2026-10-01 00:00"),
    ]
    by_course = filter_assignments(items, course_id="s1")
    assert [item.activity_id for item in by_course] == ["a3", "a1"]
    submitted_as_undone = filter_assignments(
        [make_item(is_submitted=True)], undone_only=True
    )
    assert submitted_as_undone == []


def test_normalize_attachment_handles_assignment_resource():
    attachment = normalize_attachment(
        {
            "resourceId": "r1",
            "resourceName": "作业要求.pdf",
            "resourceType": "pdf",
        }
    )
    assert attachment["title"] == "作业要求.pdf"
    assert attachment["resource_id"] == "r1"
    assert attachment["url"] is None


def test_detail_dict_truncates_content():
    detail = HomeworkDetail(
        activity_id="a1",
        title="t",
        content_html="<p>hello world</p>",
        attachments=[{"resourceId": "r1", "resourceName": "f.pdf"}],
    )
    full = detail_dict(detail)
    assert full["content_text"] == "hello world"
    assert full["content_truncated"] is False
    assert full["attachments"][0]["resource_id"] == "r1"


def test_cookie_snapshot_and_restore():
    from bupt_cli.session import cookie_snapshot, restore_cookies

    original = httpx.Client()
    try:
        original.cookies.set("SESSION", "secret-cookie", domain="auth.bupt.edu.cn")
        snapshot = cookie_snapshot(original)
    finally:
        original.close()

    assert snapshot and snapshot[0]["name"] == "SESSION"
    restored_client = httpx.Client()
    try:
        count = restore_cookies(restored_client, snapshot)
        assert count == 1
        assert restored_client.cookies.get("SESSION") == "secret-cookie"
    finally:
        restored_client.close()
