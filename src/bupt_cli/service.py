"""Pure helpers shared by CLI commands (no network side effects)."""

from __future__ import annotations

from pathlib import Path

import json
import os
import re
import time
from datetime import datetime
from typing import Any, Iterable, Optional
from urllib.parse import unquote

import httpx

from bupt_cli._vendor.campus_api.bupt.models import (
    CourseSite,
    HomeworkDetail,
    HomeworkItem,
)
from bupt_cli.errors import UsageError
from bupt_cli.session import seen_state_path

# Raw UCloud payload keys that may carry a publish/created/start timestamp,
# in preference order. `assignmentBeginTime` is the "available from" field
# observed on the student homework list.
PUBLISH_TIME_KEYS = (
    "assignmentBeginTime",
    "beginTime",
    "publishTime",
    "releaseTime",
    "createTime",
    "createdTime",
    "gmtCreate",
    "createDate",
    "startTime",
)

UCLOUD_API = "https://apiucloud.bupt.edu.cn"


# ---------------------------------------------------------------------------
# Time parsing.
# ---------------------------------------------------------------------------


def parse_datetime(value: Any) -> Optional[datetime]:
    """Parse upstream/ISO timestamps into a naive local datetime.

    Accepts ``YYYY-MM-DD``, ``YYYY-MM-DD HH:MM[:SS[.fff]]`` and ISO-8601
    with ``T``/``Z``/offsets. Returns None when the value is not a timestamp.
    """
    if value is None or isinstance(value, bool):
        return None
    if isinstance(value, (int, float)):
        # Some upstream fields use epoch milliseconds.
        try:
            seconds = float(value) / 1000.0 if value > 1e11 else float(value)
            return datetime.fromtimestamp(seconds)
        except (ValueError, OSError, OverflowError):
            return None
    text = str(value).strip()
    if not text or text in {"0", "-", "null", "None"}:
        return None
    text = text.replace("/", "-")
    if text.endswith("Z"):
        text = text[:-1] + "+00:00"
    try:
        parsed = datetime.fromisoformat(text)
    except ValueError:
        # Trim trailing fractional seconds that fromisoformat dislikes.
        head = text.split(".")[0]
        try:
            parsed = datetime.fromisoformat(head)
        except ValueError:
            return None
    if parsed.tzinfo is not None:
        parsed = parsed.astimezone().replace(tzinfo=None)
    return parsed


def publish_time(item: HomeworkItem) -> Optional[datetime]:
    """Best available publish/start time for an assignment."""
    for key in PUBLISH_TIME_KEYS:
        if key in item.raw:
            parsed = parse_datetime(item.raw.get(key))
            if parsed is not None:
                return parsed
    return parse_datetime(item.begin_time)


def deadline(item: HomeworkItem) -> Optional[datetime]:
    return item.parsed_end_time


def status_of(item: HomeworkItem) -> str:
    if item.is_submitted:
        return "submitted"
    if item.is_overdue:
        return "overdue"
    return "pending"


def status_for(submitted: bool, end_time: Optional[str]) -> str:
    """Same vocabulary as :func:`status_of`, for payloads without a HomeworkItem.

    ``homework show`` only knows the submission flag and the deadline string,
    so the tri-state is rebuilt here to keep ``list`` and ``show`` consistent.
    """
    if submitted:
        return "submitted"
    parsed_end = parse_datetime(end_time)
    if parsed_end is not None:
        now = datetime.now(parsed_end.tzinfo) if parsed_end.tzinfo else datetime.now()
        if parsed_end < now:
            return "overdue"
    return "pending"


# ---------------------------------------------------------------------------
# Homework serialization / filtering.
# ---------------------------------------------------------------------------


def assignment_dict(item: HomeworkItem) -> dict[str, Any]:
    """Stable JSON shape for assignments (no raw upstream payload)."""
    pub = publish_time(item)
    return {
        "id": item.activity_id,
        "title": item.activity_name,
        "course": item.site_name or None,
        "course_id": item.site_id,
        "published_at": pub.isoformat(sep=" ") if pub else None,
        "deadline": item.end_time,
        "begin_time": item.begin_time,
        "status": status_of(item),
        "submitted": item.is_submitted,
        "submitted_at": item.submit_time,
        "score": item.score,
        "hours_remaining": item.hours_remaining,
        "overdue": item.is_overdue,
    }


def filter_assignments(
    items: Iterable[HomeworkItem],
    *,
    course_id: Optional[str] = None,
    undone_only: bool = False,
) -> list[HomeworkItem]:
    selected: list[HomeworkItem] = []
    for item in items:
        if course_id and item.site_id != course_id:
            continue
        if undone_only and item.is_submitted:
            continue
        selected.append(item)

    def sort_key(item: HomeworkItem):
        due = deadline(item)
        return (due is None, due or datetime.max, item.activity_name)

    selected.sort(key=sort_key)
    return selected


def course_dict(course: CourseSite) -> dict[str, Any]:
    return {
        "id": course.site_id,
        "name": course.site_name,
        "teacher": course.teacher_name or None,
    }


def normalize_attachment(entry: Any, index: int = 0) -> dict[str, Any]:
    """Normalize an UCloud attachment entry to {title,url,resource_id,size}."""
    if isinstance(entry, str):
        return {
            "title": entry.rsplit("/", 1)[-1] or f"attachment-{index + 1}",
            "url": entry or None,
            "resource_id": None,
            "size": None,
        }
    if not isinstance(entry, dict):
        return {
            "title": f"attachment-{index + 1}",
            "url": None,
            "resource_id": None,
            "size": None,
        }

    title = (
        entry.get("title")
        or entry.get("name")
        or entry.get("resourceName")
        or entry.get("fileName")
        or entry.get("originalName")
        or entry.get("originalFileName")
        or f"attachment-{index + 1}"
    )
    url = (
        entry.get("url")
        or entry.get("downloadUrl")
        or entry.get("previewUrl")
        or entry.get("link")
    )
    resource_id = (
        entry.get("resourceId")
        or entry.get("storageId")
        or entry.get("fileId")
        or entry.get("id")
    )
    size = entry.get("size") or entry.get("fileSize")
    return {
        "title": str(title),
        "url": str(url) if url else None,
        "resource_id": str(resource_id) if resource_id else None,
        "size": size,
    }


def preview_filename(url: Any) -> Optional[str]:
    """Extract a filename from a preview URL's ``response-content-disposition``.

    UCloud preview URLs look like
    ``.../file.pdf?response-content-disposition=attachment%3Bfilename%3Dname.pdf``.
    Handles URL-encoded values, quoted names and RFC 5987 ``filename*=UTF-8''``.
    Returns None when the URL carries no usable disposition filename.
    """
    if not url or not isinstance(url, str):
        return None
    match = re.search(
        r"[?&]response-content-disposition=([^&]*)", url, re.IGNORECASE
    )
    if not match:
        return None

    # Only treat `;`/`%3B` as a parameter separator when it is followed by a
    # known disposition parameter. A semicolon inside a filename (encoded or
    # literal) must stay part of the value.
    separator = re.compile(
        r"(?:;|%3[bB])(?=(?:\s|%20|\+)*(?:attachment|filename\*?|size|"
        r"creation-date|modification-date|read-date)(?:\s|%20|\+)*"
        r"(?:=|%3[dD]))",
        re.IGNORECASE,
    )
    star_name: Optional[str] = None
    plain_name: Optional[str] = None
    for segment in separator.split(match.group(1)):
        part = unquote(segment).strip()
        lowered = part.lower()
        if lowered.startswith("filename*="):
            raw = part.split("=", 1)[1].strip().strip('"')
            if "''" in raw:
                raw = raw.split("''", 1)[1]
            star_name = unquote(raw)
        elif lowered.startswith("filename="):
            plain_name = unquote(part.split("=", 1)[1].strip().strip('"'))
    name = (star_name or plain_name or "").strip()
    return name or None


def resolve_attachment_urls(
    attachments: list[dict[str, Any]],
    token: str,
    timeout: float = 15.0,
    *,
    derive_titles: bool = False,
) -> list[dict[str, Any]]:
    """Best-effort preview-URL resolution for attachments without a URL.

    The upstream preview endpoint is public but is still called with the
    Blade-Auth header. Failures leave ``url`` as null instead of failing the
    whole `homework show` command. With ``derive_titles`` the attachment
    title is taken from the preview URL's ``content-disposition`` filename
    (used for student submissions, which have no filename field).
    """
    pending = [
        att
        for att in attachments
        if not att.get("url") and att.get("resource_id")
    ]
    if pending:
        headers = {
            "Blade-Auth": token,
            "Tenant-Id": "000000",
            "Authorization": "Basic c3dvcmQ6c3dvcmRfc2VjcmV0",
        }
        try:
            with httpx.Client(timeout=timeout, follow_redirects=True) as client:
                for att in pending:
                    try:
                        resp = client.get(
                            f"{UCLOUD_API}/blade-source/resource/preview-url",
                            params={"resourceId": att["resource_id"]},
                            headers=headers,
                        )
                        payload = resp.json()
                    except (httpx.HTTPError, ValueError):
                        continue
                    if not isinstance(payload, dict):
                        continue
                    data = payload.get("data")
                    preview = ""
                    if isinstance(data, dict):
                        preview = data.get("previewUrl") or data.get("url") or ""
                    elif isinstance(data, str):
                        preview = data
                    if preview:
                        att["url"] = preview
        except httpx.HTTPError:
            pass
    if derive_titles:
        for index, att in enumerate(attachments):
            title = preview_filename(att.get("url"))
            if title:
                att["title"] = title
            elif not att.get("title"):
                att["title"] = f"attachment-{index + 1}"
    return attachments


def detail_dict(detail: HomeworkDetail) -> dict[str, Any]:
    # Assignment descriptions are short; a long one would be an attachment, so
    # the content is always returned in full.
    content = detail.content_text
    truncated = False
    attachments = [
        normalize_attachment(entry, index)
        for index, entry in enumerate(detail.attachments or [])
    ]
    submitted_attachments = [
        normalize_attachment(entry, index)
        for index, entry in enumerate(detail.submit_attachments or [])
    ]
    return {
        "id": detail.activity_id,
        "title": detail.title,
        "content_text": content,
        "content_truncated": truncated,
        "attachments": attachments,
        "submitted": detail.is_submitted,
        "submit_content": detail.submit_content,
        "submit_attachments": submitted_attachments,
        "score": detail.score,
        "teacher_comment": detail.teacher_comment,
    }


# ---------------------------------------------------------------------------
# "new homework" seen-state (fallback when upstream has no publish time).
# ---------------------------------------------------------------------------


def ucloud_auth_headers(token: str) -> dict[str, str]:
    return {
        "Blade-Auth": token,
        "Tenant-Id": "000000",
        "Authorization": "Basic c3dvcmQ6c3dvcmRfc2VjcmV0",
    }


def get_submit_view(
    token: str, assignment_id: str, *, timeout: float = 20.0
) -> dict[str, Any]:
    """GET /ykt-site/work/submit-view — current student submission view (read-only)."""
    with httpx.Client(timeout=timeout, follow_redirects=True) as client:
        resp = client.get(
            f"{UCLOUD_API}/ykt-site/work/submit-view",
            params={"assignmentId": assignment_id},
            headers=ucloud_auth_headers(token),
        )
        resp.raise_for_status()
        payload = resp.json()
    if not isinstance(payload, dict):
        raise ValueError("submit-view returned non-object JSON")
    data = payload.get("data")
    return data if isinstance(data, dict) else {}


def upload_homework_file(
    token: str,
    user_id: str,
    path: Path,
    *,
    timeout: float = 60.0,
    filename: Optional[str] = None,
    mime_type: Optional[str] = None,
) -> dict[str, Any]:
    """POST /blade-source/resource/upload/biz — upload one local file.

    Returns ``{resource_id, filename, size, mime_type}``. Does **not** attach
    the file to any assignment; that only happens on ``submit_homework``.
    """
    import mimetypes

    path = Path(path)
    if not path.is_file():
        raise FileNotFoundError(str(path))
    name = filename or path.name
    mime = mime_type or mimetypes.guess_type(name)[0] or "application/octet-stream"
    data_bytes = path.read_bytes()
    headers = ucloud_auth_headers(token)
    with httpx.Client(timeout=timeout, follow_redirects=True) as client:
        resp = client.post(
            f"{UCLOUD_API}/blade-source/resource/upload/biz",
            headers=headers,
            files={"file": (name, data_bytes, mime)},
            data={"userId": str(user_id), "bizType": "3"},
        )
        resp.raise_for_status()
        payload = resp.json()
    if not isinstance(payload, dict) or not payload.get("success"):
        msg = ""
        if isinstance(payload, dict):
            msg = str(payload.get("msg") or payload)
        raise RuntimeError(f"upload failed: {msg or resp.status_code}")
    resource_id = payload.get("data")
    if not resource_id:
        raise RuntimeError("upload succeeded but returned no resourceId")
    return {
        "resource_id": str(resource_id),
        "filename": name,
        "size": len(data_bytes),
        "mime_type": mime,
    }


def submit_homework(
    token: str,
    user_id: str,
    assignment_id: str,
    *,
    content: str = "",
    attachment_ids: Optional[list[str]] = None,
    timeout: float = 30.0,
) -> dict[str, Any]:
    """POST /ykt-site/work/submit — submit an assignment.

    This is a real write. Callers must gate it behind an explicit confirm flag.
    """
    body = {
        "assignmentId": str(assignment_id),
        "assignmentType": 0,
        "assignmentContent": (content or "").rstrip(),
        "attachmentIds": list(attachment_ids or []),
        "userId": str(user_id),
        "groupId": "",
        "commitId": "",
    }
    headers = ucloud_auth_headers(token)
    headers["Content-Type"] = "application/json"
    with httpx.Client(timeout=timeout, follow_redirects=True) as client:
        resp = client.post(
            f"{UCLOUD_API}/ykt-site/work/submit",
            headers=headers,
            json=body,
        )
        resp.raise_for_status()
        payload = resp.json()
    if not isinstance(payload, dict):
        raise RuntimeError("submit returned non-object JSON")
    if not payload.get("success") and payload.get("code") not in (0, 200):
        raise RuntimeError(str(payload.get("msg") or payload))
    return {
        "success": bool(payload.get("success", True)),
        "code": payload.get("code"),
        "msg": payload.get("msg"),
        "payload": body,
    }


def download_url_to_path(
    url: str, dest: Path, *, timeout: float = 60.0, token: Optional[str] = None
) -> dict[str, Any]:
    """Download ``url`` to ``dest``. Returns size/path metadata.

    ``fileucloud.bupt.edu.cn`` CDN URLs reject Blade-Auth (HTTP 400); auth
    headers are only attached for non-CDN hosts when ``token`` is set.
    """
    dest = Path(dest)
    dest.parent.mkdir(parents=True, exist_ok=True)
    headers: dict[str, str] = {
        "User-Agent": "Mozilla/5.0 (compatible; bupt-cli/1.0)",
    }
    host = url.split("/")[2] if "://" in url else ""
    if token and "fileucloud.bupt.edu.cn" not in host:
        headers.update(ucloud_auth_headers(token))
    with httpx.Client(timeout=timeout, follow_redirects=True) as client:
        with client.stream("GET", url, headers=headers) as resp:
            resp.raise_for_status()
            written = 0
            with dest.open("wb") as handle:
                for chunk in resp.iter_bytes():
                    handle.write(chunk)
                    written += len(chunk)
    return {"path": str(dest), "size": written, "url": url}
