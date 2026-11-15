"""Offline tests for `homework show` submission state and vendor detail parsing.

All HTTP is stubbed: these tests never touch apiucloud.bupt.edu.cn and never
call a write endpoint.
"""

import asyncio
import json
from urllib.parse import quote

from bupt_cli import cli, commands
from bupt_cli._vendor.campus_api.bupt.homework import (
    AsyncHomeworkService,
    HomeworkService,
)
from bupt_cli._vendor.campus_api.bupt.models import (
    CourseSite,
    HomeworkDetail,
    HomeworkItem,
)
from bupt_cli.service import preview_filename


class FakeSessionManager:
    def get_ucloud_token(self):
        return {"access_token": "test-token", "user_id": "user-1"}

    def invalidate(self):
        pass


class FakeAsyncSessionManager:
    async def get_ucloud_token(self):
        return {"access_token": "test-token", "user_id": "user-1"}

    def invalidate(self):
        pass


DETAIL_SUBMITTED = {
    "id": "1000000000000000001",
    "assignmentTitle": "作业一",
    "className": "课程一",
    "assignmentBeginTime": "2026-10-21 00:00",
    "assignmentEndTime": "2026-10-26 23:59",
    "status": 2,
    "assignmentStatus": 0,
    "score": -1,
    "fullMark": 100,
    "assignmentResource": [
        {
            "resourceId": "teacher-attachment",
            "resourceName": "作业要求.pdf",
            "resourceType": "pdf",
        }
    ],
}

DETAIL_NOT_SUBMITTED = {
    **DETAIL_SUBMITTED,
    "id": "1000000000000000002",
    "assignmentStatus": 99,
}


# ---------------------------------------------------------------------------
# Vendor get_detail: assignmentStatus is the submission flag.
# ---------------------------------------------------------------------------


def _patch_sync_request(monkeypatch, payloads):
    def fake_request(self, *args, **kwargs):
        return payloads.pop(0)

    monkeypatch.setattr(HomeworkService, "_request", fake_request)


def test_get_detail_assignment_status_zero_means_submitted(monkeypatch):
    _patch_sync_request(monkeypatch, [DETAIL_SUBMITTED])
    detail = HomeworkService(FakeSessionManager()).get_detail(
        "1000000000000000001"
    )
    assert detail.is_submitted is True


def test_get_detail_assignment_status_99_means_not_submitted(monkeypatch):
    _patch_sync_request(
        monkeypatch,
        [{**DETAIL_SUBMITTED, "assignmentStatus": 99}, {"id": "x"}],
    )
    service = HomeworkService(FakeSessionManager())
    assert service.get_detail("x").is_submitted is False
    # Missing assignmentStatus must not be treated as submitted.
    assert service.get_detail("x").is_submitted is False


def test_get_detail_string_zero_and_submit_info_compatibility(monkeypatch):
    _patch_sync_request(
        monkeypatch,
        [
            {**DETAIL_SUBMITTED, "assignmentStatus": "0"},
            {"id": "x", "submitInfo": {"isSubmit": True}},
            {"id": "x", "submitInfo": {"isSubmit": False}, "assignmentStatus": 99},
        ],
    )
    service = HomeworkService(FakeSessionManager())
    assert service.get_detail("x").is_submitted is True
    assert service.get_detail("x").is_submitted is True
    assert service.get_detail("x").is_submitted is False


def test_async_get_detail_assignment_status(monkeypatch):
    payloads = [DETAIL_SUBMITTED, DETAIL_NOT_SUBMITTED]

    async def fake_request(self, *args, **kwargs):
        return payloads.pop(0)

    monkeypatch.setattr(AsyncHomeworkService, "_request", fake_request)
    service = AsyncHomeworkService(FakeAsyncSessionManager())
    submitted = asyncio.run(service.get_detail("1000000000000000001"))
    missing = asyncio.run(service.get_detail("1000000000000000002"))
    assert submitted.is_submitted is True
    assert missing.is_submitted is False


def test_find_list_record_stops_at_first_site_with_match(monkeypatch):
    calls = []

    def fake_request(self, method, path, params=None, json_body=None, **kwargs):
        calls.append(dict(json_body))
        return {
            "records": [
                {
                    "id": "other",
                    "assignmentStatus": 99,
                    "submitTime": "",
                },
                {
                    "id": "1000000000000000001",
                    "activityId": "different-activity",
                    "assignmentStatus": 0,
                    "submitTime": "2026-10-22 16:26",
                },
            ],
            "total": 2,
        }

    monkeypatch.setattr(HomeworkService, "_request", fake_request)
    record = HomeworkService(FakeSessionManager()).find_list_record(
        "1000000000000000001", ["site-1", "site-2"]
    )
    assert record is not None
    assert record.submit_time == "2026-10-22 16:26"
    assert [call["siteId"] for call in calls] == ["site-1"]


def test_async_find_list_record_matches(monkeypatch):
    async def fake_request(self, method, path, params=None, json_body=None, **kwargs):
        return {
            "records": [
                {
                    "id": "1000000000000000001",
                    "activityId": "different-activity",
                    "assignmentStatus": 0,
                    "submitTime": "2026-10-22 16:26",
                }
            ],
            "total": 1,
        }

    monkeypatch.setattr(AsyncHomeworkService, "_request", fake_request)
    record = asyncio.run(
        AsyncHomeworkService(FakeAsyncSessionManager()).find_list_record(
            "1000000000000000001", ["site-1"]
        )
    )
    assert record is not None
    assert record.submit_time == "2026-10-22 16:26"


# ---------------------------------------------------------------------------
# Command-level tests: fake client, fake submit-view, fake preview HTTP.
# ---------------------------------------------------------------------------


class FakeResponse:
    def __init__(self, payload):
        self._payload = payload

    def json(self):
        return self._payload


class FakeHomework:
    def __init__(self, calls, record):
        self.calls = calls
        self.record = record

    def find_list_record(self, assignment_id, site_ids):
        self.calls.append(("find_list_record", str(assignment_id), list(site_ids)))
        return self.record


class FakeCaseManager:
    def get_ucloud_token(self):
        return {"access_token": "test-token", "user_id": "user-1"}


class FakeClient:
    def __init__(self, detail, courses=(), record=None):
        self.detail = detail
        self.courses = list(courses)
        self.calls = []
        self.homework = FakeHomework(self.calls, record)
        self._cas_session_mgr = FakeCaseManager()

    def get_homework_detail(self, activity_id):
        self.calls.append(("detail", str(activity_id)))
        return self.detail

    def get_enrolled_courses(self):
        self.calls.append(("courses",))
        return self.courses


def _install_fake_client(monkeypatch, client):
    monkeypatch.setattr(commands, "_session", lambda _args, **_: object())
    monkeypatch.setattr(commands, "_client", lambda _session, **_: client)
    monkeypatch.setattr(commands, "_persist", lambda _session: None)


def _install_preview_http(monkeypatch, previews):
    class FakePreviewClient:
        def __init__(self, *args, **kwargs):
            pass

        def __enter__(self):
            return self

        def __exit__(self, *exc_info):
            return False

        def get(self, url, params=None, headers=None):
            resource_id = (params or {}).get("resourceId")
            return FakeResponse(
                {"data": {"previewUrl": previews.get(resource_id, "")}}
            )

    monkeypatch.setattr("bupt_cli.service.httpx.Client", FakePreviewClient)


def _args(**overrides):
    values = {
        "id": "1000000000000000001",
        "max_chars": 0,
        "timeout": 5.0,
        "course": None,
    }
    values.update(overrides)
    return type("Args", (), values)()


def _detail(payload, **overrides):
    base = dict(
        activity_id=str(payload.get("id") or ""),
        title=payload.get("assignmentTitle") or "",
        content_html="",
        attachments=payload.get("assignmentResource") or [],
        is_submitted=payload.get("assignmentStatus") in (0, "0"),
        raw=payload,
    )
    base.update(overrides)
    return HomeworkDetail(**base)


def _courses(*pairs):
    return [
        CourseSite(site_id=site_id, site_name=name) for site_id, name in pairs
    ]


def test_show_submitted_uses_submit_view_and_matching_site(monkeypatch):
    detail = _detail(DETAIL_SUBMITTED)
    record = HomeworkItem(
        activity_id="1000000000000000001",
        activity_name="作业一",
        site_id="site-1",
        site_name="课程一",
        end_time="2026-10-26 23:59",
        submit_time="2026-10-22 16:26",
        is_submitted=True,
    )
    client = FakeClient(
        detail,
        courses=_courses(
            ("site-1", "课程一"), ("site-2", "课程一"), ("site-3", "其他课程")
        ),
        record=record,
    )
    _install_fake_client(monkeypatch, client)

    filename = "答案.pdf"
    preview_url = (
        "https://fileucloud.bupt.edu.cn/ucloud/document/hash.pdf"
        "?response-content-disposition=attachment%3Bfilename%3D"
        + quote(filename)
    )
    _install_preview_http(
        monkeypatch,
        {"teacher-attachment": "", "student-attachment": preview_url},
    )
    monkeypatch.setattr(
        commands,
        "get_submit_view",
        lambda token, assignment_id, timeout=0: {
            "assignmentId": "1000000000000000001",
            "attachmentIds": ["student-attachment"],
            "assignmentContent": "<p>见附件</p>",
            "score": -1,
            "evaluation": "做得不错",
            "assignmentStatus": 0,
        },
    )

    payload = commands.cmd_homework_show(_args())["assignment"]

    assert payload["submitted"] is True
    assert payload["submit_content"] == "见附件"
    assert payload["submit_attachments"] == [
        {
            "title": filename,
            "url": preview_url,
            "resource_id": "student-attachment",
            "size": None,
        }
    ]
    assert payload["submitted_at"] == "2026-10-22 16:26"
    assert payload["teacher_comment"] == "做得不错"
    assert payload["score"] is None  # submit-view -1 is not a grade
    # Teacher attachments keep their own names.
    assert payload["attachments"][0]["title"] == "作业要求.pdf"

    lookups = [call for call in client.calls if call[0] == "find_list_record"]
    assert lookups == [
        (
            "find_list_record",
            "1000000000000000001",
            ["site-1", "site-2"],
        )
    ]


def test_show_not_submitted_skips_list_lookup(monkeypatch):
    detail = _detail(DETAIL_NOT_SUBMITTED)
    client = FakeClient(detail, courses=_courses(("site-1", "课程一")))
    _install_fake_client(monkeypatch, client)
    _install_preview_http(monkeypatch, {})
    monkeypatch.setattr(
        commands,
        "get_submit_view",
        lambda token, assignment_id, timeout=0: {
            "assignmentId": -1,
            "attachmentIds": [],
            "assignmentContent": "",
            "score": -1,
            "evaluation": "",
            "assignmentStatus": 99,
        },
    )

    payload = commands.cmd_homework_show(_args(id="1000000000000000002"))[
        "assignment"
    ]

    assert payload["submitted"] is False
    assert payload["submit_content"] is None
    assert payload["submit_attachments"] == []
    assert payload["submitted_at"] is None
    assert not [call for call in client.calls if call[0] == "courses"]
    assert not [call for call in client.calls if call[0] == "find_list_record"]


def test_show_succeeds_when_submit_view_raises(monkeypatch):
    detail = _detail(
        DETAIL_SUBMITTED,
        submit_content="detail content",
        submit_attachments=[],
    )
    client = FakeClient(detail, courses=_courses(("site-1", "课程一")))
    _install_fake_client(monkeypatch, client)
    _install_preview_http(monkeypatch, {})

    def boom(token, assignment_id, timeout=0):
        raise RuntimeError(
            "submit-view exploded with token test-token at https://example.invalid"
        )

    monkeypatch.setattr(commands, "get_submit_view", boom)

    payload = commands.cmd_homework_show(_args())["assignment"]

    assert payload["submitted"] is True  # detail-derived value kept
    assert payload["submit_content"] == "detail content"
    assert "submit-view exploded" in payload["submit_view_error"]
    assert "test-token" not in payload["submit_view_error"]
    assert len(payload["submit_view_error"]) <= 200


def test_show_empty_submit_view_keeps_detail_values(monkeypatch):
    detail = _detail(DETAIL_SUBMITTED, submit_content="detail content")
    client = FakeClient(detail, courses=_courses(("site-1", "课程一")))
    _install_fake_client(monkeypatch, client)
    _install_preview_http(monkeypatch, {})
    monkeypatch.setattr(
        commands, "get_submit_view", lambda token, assignment_id, timeout=0: {}
    )

    payload = commands.cmd_homework_show(_args())["assignment"]

    assert payload["submitted"] is True
    assert payload["submit_content"] == "detail content"
    assert payload["submit_view_error"] == "submit-view returned no data"
    assert payload["submitted_at"] is None


def test_show_lookup_failure_leaves_submitted_at_null(monkeypatch):
    detail = _detail(DETAIL_SUBMITTED)
    client = FakeClient(detail, courses=_courses(("site-1", "课程一")))

    def boom(assignment_id, site_ids):
        raise RuntimeError("student list down")

    client.homework.find_list_record = boom
    _install_fake_client(monkeypatch, client)
    _install_preview_http(monkeypatch, {})
    monkeypatch.setattr(
        commands,
        "get_submit_view",
        lambda token, assignment_id, timeout=0: {
            "assignmentId": "1000000000000000001",
            "attachmentIds": [],
            "assignmentContent": "",
            "score": -1,
            "evaluation": "",
        },
    )

    payload = commands.cmd_homework_show(_args())["assignment"]

    assert payload["submitted"] is True
    assert payload["submitted_at"] is None


def test_show_course_flag_restricts_lookup(monkeypatch):
    detail = _detail(DETAIL_SUBMITTED)
    record = HomeworkItem(
        activity_id="1000000000000000001",
        activity_name="作业一",
        site_id="site-9",
        site_name="课程一",
        end_time=None,
        submit_time="2026-10-22 16:26",
        is_submitted=True,
    )
    client = FakeClient(
        detail, courses=_courses(("site-1", "课程一"), ("site-2", "课程一")), record=record
    )
    _install_fake_client(monkeypatch, client)
    _install_preview_http(monkeypatch, {})
    monkeypatch.setattr(
        commands,
        "get_submit_view",
        lambda token, assignment_id, timeout=0: {
            "assignmentId": "1000000000000000001",
            "attachmentIds": [],
            "assignmentContent": "",
            "score": -1,
            "evaluation": "",
        },
    )

    payload = commands.cmd_homework_show(_args(course="site-9"))["assignment"]

    assert payload["submitted"] is True
    assert payload["submitted_at"] == "2026-10-22 16:26"
    assert not [call for call in client.calls if call[0] == "courses"]
    lookups = [call for call in client.calls if call[0] == "find_list_record"]
    assert lookups == [
        ("find_list_record", "1000000000000000001", ["site-9"])
    ]


# ---------------------------------------------------------------------------
# Preview URL filename parsing.
# ---------------------------------------------------------------------------


def test_preview_filename_plain_and_rfc5987():
    plain = (
        "https://x/ucloud/document/a.pdf"
        "?response-content-disposition=attachment%3Bfilename%3D"
        + quote("作业2026.pdf")
    )
    star = (
        "https://x/ucloud/document/a.pdf"
        "?response-content-disposition=attachment%3Bfilename*%3DUTF-8''"
        + quote("答案 1.pdf")
    )
    quoted = (
        "https://x/ucloud/document/a.pdf"
        "?response-content-disposition=attachment%3Bfilename%3D%22report.pdf%22"
    )
    semicolon = (
        "https://x/ucloud/document/a.pdf"
        "?response-content-disposition=attachment%3Bfilename%3Da%3Bb.pdf"
    )
    literal_semicolon = (
        "https://x/ucloud/document/a.pdf"
        "?response-content-disposition=attachment;filename=a;b.pdf"
    )
    assert preview_filename(plain) == "作业2026.pdf"
    assert preview_filename(star) == "答案 1.pdf"
    assert preview_filename(quoted) == "report.pdf"
    assert preview_filename(semicolon) == "a;b.pdf"
    assert preview_filename(literal_semicolon) == "a;b.pdf"
    assert preview_filename("https://x/file.pdf") is None
    assert preview_filename(None) is None


def test_show_course_flag_requires_numeric_id(monkeypatch, capsys, tmp_path):
    monkeypatch.setenv("BUPT_CLI_CACHE_DIR", str(tmp_path))
    monkeypatch.setenv("BUPT_CLI_CONFIG_DIR", str(tmp_path / "config"))
    for name in (
        "BUPT_USERNAME",
        "BUPT_PASSWORD",
        "BUPT_JWGL_USERNAME",
        "BUPT_JWGL_PASSWORD",
    ):
        monkeypatch.delenv(name, raising=False)
    code = cli.main(["homework", "show", "1000000000000000001", "--course", "abc"])
    payload = json.loads(capsys.readouterr().err)
    assert code == 2
    assert payload["error"]["type"] == "usage"
