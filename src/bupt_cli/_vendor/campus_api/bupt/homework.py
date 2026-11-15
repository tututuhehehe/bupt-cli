"""homework.py - BUPT UCloud homework and course assignment service."""

from typing import Any, Iterable, Optional

import httpx

from bupt_cli._vendor.campus_api.bupt.auth import AsyncBuptSessionManager, BuptSessionManager
from bupt_cli._vendor.campus_api.bupt.models import (
    CourseSite,
    HomeworkDetail,
    HomeworkItem,
    HomeworkOverview,
)
from bupt_cli._vendor.campus_api.exceptions import (
    CampusAuthError,
    CampusNetworkError,
    CampusParseError,
    CampusResourceNotFoundError,
)

UCLOUD_API = "https://apiucloud.bupt.edu.cn"
MAX_PAGINATION_PAGES = 1000


def _build_ucloud_headers(token: str) -> dict[str, str]:
    return {
        "Blade-Auth": token,
        "Tenant-Id": "000000",
        "Authorization": "Basic c3dvcmQ6c3dvcmRfc2VjcmV0",
    }


def _format_teacher_name(val: Any) -> str:
    """Format teacherName or teachers field into a clean string."""
    if isinstance(val, list):
        names = []
        for item in val:
            if isinstance(item, dict):
                n = item.get("realName") or item.get("name")
                if n:
                    names.append(str(n))
            elif item:
                names.append(str(item))
        return ", ".join(names)
    elif isinstance(val, dict):
        return str(val.get("realName") or val.get("name") or "")
    return str(val) if val else ""


def _first_present(data: dict[str, Any], *keys: str) -> Any:
    """Return the first present, non-empty value without dropping zero or False."""
    for key in keys:
        if key in data and data[key] is not None and data[key] != "":
            return data[key]
    return None


def _assignment_status_submitted(value: Any) -> bool:
    """Return True when an upstream ``assignmentStatus`` means submitted.

    Only ``0``/``"0"`` are known to mean submitted; ``99`` is not submitted.
    Unknown or missing values must not be treated as submitted.
    """
    if isinstance(value, bool):
        return False
    return value in (0, "0")


def _parse_score(value: Any) -> Optional[float]:
    if value is None or value == "":
        return None
    try:
        return float(value)
    except (ValueError, TypeError):
        return None


def _require_dict(data: Any, context: str) -> dict[str, Any]:
    if not isinstance(data, dict):
        raise CampusParseError(
            f"{context} returned malformed data (expected an object)"
        )
    return data


def _page_records(
    data: Any, context: str
) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    page_data = _require_dict(data, context)
    records = page_data.get("records")
    if not isinstance(records, list) or not all(
        isinstance(record, dict) for record in records
    ):
        raise CampusParseError(f"{context} returned malformed records")
    return page_data, records


def _pagination_total(data: dict[str, Any], context: str) -> Optional[int]:
    for key in ("total", "totalCount", "totalElements"):
        if key not in data or data[key] is None or data[key] == "":
            continue
        value = data[key]
        if isinstance(value, bool):
            raise CampusParseError(f"{context} returned an invalid pagination total")
        try:
            total = int(value)
        except (ValueError, TypeError) as e:
            raise CampusParseError(
                f"{context} returned an invalid pagination total"
            ) from e
        if total < 0:
            raise CampusParseError(f"{context} returned an invalid pagination total")
        return total
    return None


def _page_signature(records: list[dict[str, Any]]) -> tuple[str, ...]:
    return tuple(repr(record) for record in records)


def _pagination_finished(
    record_count: int,
    requested_size: int,
    fetched_records: int,
    total: Optional[int],
) -> bool:
    if record_count == 0:
        return True
    if total is not None:
        return fetched_records >= total
    return record_count < requested_size


def _parse_course_page(
    data: Any,
) -> tuple[list[CourseSite], int, Optional[int], tuple[str, ...]]:
    page_data, records = _page_records(data, "UCloud course list")
    courses = [
        CourseSite(
            site_id=str(_first_present(record, "siteId", "id") or ""),
            site_name=str(_first_present(record, "siteName", "className") or ""),
            teacher_name=_format_teacher_name(
                _first_present(record, "teacherName", "teachers")
            ),
            raw=record,
        )
        for record in records
    ]
    return (
        courses,
        len(records),
        _pagination_total(page_data, "UCloud course list"),
        _page_signature(records),
    )


def _parse_homework_page(
    data: Any,
    site_id: str,
) -> tuple[list[HomeworkItem], int, Optional[int], tuple[str, ...]]:
    page_data, records = _page_records(data, "UCloud homework list")
    items: list[HomeworkItem] = []
    for record in records:
        status_val = _first_present(record, "assignmentStatus", "status")
        is_submitted = (
            not isinstance(status_val, bool)
            and status_val in (0, "0", "SUBMITTED")
        ) or bool(record.get("submitTime"))
        items.append(
            HomeworkItem(
                activity_id=str(_first_present(record, "activityId", "id") or ""),
                activity_name=str(
                    _first_present(record, "assignmentTitle", "activityName") or ""
                ),
                site_id=str(site_id),
                site_name=str(record.get("siteName") or ""),
                begin_time=_first_present(record, "assignmentBeginTime", "beginTime"),
                end_time=_first_present(record, "assignmentEndTime", "endTime"),
                submit_time=record.get("submitTime"),
                is_submitted=is_submitted,
                score=_parse_score(record.get("score")),
                raw=record,
            )
        )
    return (
        items,
        len(records),
        _pagination_total(page_data, "UCloud homework list"),
        _page_signature(records),
    )


def _validate_page_args(size: int, page: int) -> None:
    if size < 1:
        raise ValueError("page size must be at least 1")
    if page < 1:
        raise ValueError("page must be at least 1")


class HomeworkService:
    """Synchronous service for querying BUPT UCloud homework and courses."""

    def __init__(self, session_manager: BuptSessionManager):
        self._session_mgr = session_manager

    def _request(
        self,
        method: str,
        path: str,
        params: Optional[dict[str, Any]] = None,
        json_body: Optional[dict[str, Any]] = None,
        retry_on_auth_fail: bool = True,
    ) -> Any:
        token_data = self._session_mgr.get_ucloud_token()
        cli = self._session_mgr.client
        url = UCLOUD_API + path
        headers = _build_ucloud_headers(token_data["access_token"])

        try:
            resp = cli.request(
                method, url, params=params, json=json_body, headers=headers
            )
        except httpx.HTTPError as e:
            raise CampusNetworkError(f"UCloud API {path} failed: {e}") from e

        if resp.status_code == 401:
            if retry_on_auth_fail:
                self._session_mgr.invalidate()
                return self._request(
                    method, path, params, json_body, retry_on_auth_fail=False
                )
            raise CampusAuthError("UCloud request unauthorized (token expired)")

        try:
            resp.raise_for_status()
        except httpx.HTTPError as e:
            raise CampusNetworkError(f"UCloud API {path} failed: {e}") from e

        try:
            payload = resp.json()
        except Exception as e:
            raise CampusParseError(f"UCloud API {path} invalid JSON: {e}") from e

        if not isinstance(payload, dict):
            raise CampusParseError(f"UCloud API {path} returned malformed payload")

        code = payload.get("code")
        if code != 200 and code != 0 and code is not None:
            msg = str(payload.get("msg") or payload)
            if "token" in msg.lower() or "auth" in msg.lower() or "登录" in msg:
                if retry_on_auth_fail:
                    self._session_mgr.invalidate()
                    return self._request(
                        method, path, params, json_body, retry_on_auth_fail=False
                    )
                raise CampusAuthError(f"UCloud authentication error: {msg}")
            raise CampusNetworkError(f"UCloud API {path} error: {msg}")

        if "data" not in payload:
            raise CampusParseError(f"UCloud API {path} response missing data")
        return payload["data"]

    def get_undone(self) -> HomeworkOverview:
        """Fetch pending (unfinished) assignments across all enrolled courses."""
        token_data = self._session_mgr.get_ucloud_token()
        user_id = token_data.get("user_id")
        if not user_id:
            raise CampusAuthError("UCloud token missing user_id")

        data = self._request(
            "GET", "/ykt-site/site/student/undone", params={"userId": user_id}
        )
        overview_data = _require_dict(data, "UCloud undone homework")
        undone_records = overview_data.get("undoneList")
        if not isinstance(undone_records, list) or not all(
            isinstance(item, dict) for item in undone_records
        ):
            raise CampusParseError("UCloud undone homework returned malformed records")

        undone_items: list[HomeworkItem] = []
        for item in undone_records:
            undone_items.append(
                HomeworkItem(
                    activity_id=str(item.get("activityId") or item.get("id") or ""),
                    activity_name=item.get("activityName")
                    or item.get("assignmentTitle")
                    or "",
                    site_id=str(item.get("siteId")) if item.get("siteId") else None,
                    site_name=item.get("siteName") or "",
                    end_time=item.get("endTime"),
                    begin_time=item.get("beginTime"),
                    is_submitted=False,
                    raw=item,
                )
            )

        return HomeworkOverview(
            total_sites=overview_data.get("siteNum", 0),
            undone_count=overview_data.get("undoneNum", len(undone_items)),
            undone_list=undone_items,
            raw=overview_data,
        )

    def find_list_record(
        self,
        assignment_id: str,
        site_ids: Iterable[str],
        page_size: int = 50,
    ) -> Optional[HomeworkItem]:
        """Find one assignment in the student lists of ``site_ids``.

        Walks each site's ``/ykt-site/work/student/list`` (following pages)
        and returns the first matching record. Returns None when no site
        lists the assignment. Read-only; uses the same request/pagination
        helpers as the other list methods.
        """
        target = str(assignment_id)
        _validate_page_args(page_size, 1)
        token_data = self._session_mgr.get_ucloud_token()
        user_id = token_data.get("user_id")
        if not user_id:
            raise CampusAuthError("UCloud token missing user_id")

        for site_id in site_ids:
            site = str(site_id or "").strip()
            if not site:
                continue
            fetched_records = 0
            seen_pages: set[tuple[str, ...]] = set()
            for page in range(1, MAX_PAGINATION_PAGES + 1):
                data = self._request(
                    "POST",
                    "/ykt-site/work/student/list",
                    json_body={
                        "siteId": site,
                        "userId": user_id,
                        "current": page,
                        "size": page_size,
                    },
                )
                works, record_count, total, signature = _parse_homework_page(
                    data, site
                )
                if record_count and signature in seen_pages:
                    raise CampusParseError(
                        f"UCloud homework pagination repeated a page for site '{site}'"
                    )
                seen_pages.add(signature)

                for work in works:
                    if work.activity_id == target or str(
                        work.raw.get("id") or ""
                    ) == target:
                        return work
                fetched_records += record_count
                if _pagination_finished(
                    record_count, page_size, fetched_records, total
                ):
                    break
            else:
                raise CampusParseError(
                    f"UCloud homework pagination exceeded the safety limit for site '{site}'"
                )
        return None

    def get_courses(self, size: int = 50, page: int = 1) -> list[CourseSite]:
        """Fetch currently enrolled course sites."""
        _validate_page_args(size, page)
        data = self._request(
            "GET",
            "/ykt-site/site/list/student/current",
            params={"size": size, "current": page},
        )
        return _parse_course_page(data)[0]

    def get_course_homework(
        self,
        site_id: str,
        page_size: int = 50,
        page: int = 1,
    ) -> list[HomeworkItem]:
        """Fetch homework assignments for a specific course site."""
        _validate_page_args(page_size, page)
        token_data = self._session_mgr.get_ucloud_token()
        user_id = token_data.get("user_id")
        if not user_id:
            raise CampusAuthError("UCloud token missing user_id")

        body = {
            "siteId": site_id,
            "userId": user_id,
            "current": page,
            "size": page_size,
        }
        data = self._request("POST", "/ykt-site/work/student/list", json_body=body)
        return _parse_homework_page(data, site_id)[0]

    def get_all(self, site_limit: int = 30, page_size: int = 50) -> list[HomeworkItem]:
        """Fetch all homework for at most ``site_limit`` courses, following every result page."""
        if site_limit < 1:
            return []
        _validate_page_args(page_size, 1)

        course_page_size = min(site_limit, page_size)
        courses: list[CourseSite] = []
        fetched_course_records = 0
        seen_course_pages: set[tuple[str, ...]] = set()
        for page in range(1, MAX_PAGINATION_PAGES + 1):
            data = self._request(
                "GET",
                "/ykt-site/site/list/student/current",
                params={"size": course_page_size, "current": page},
            )
            page_courses, record_count, total, signature = _parse_course_page(data)
            if record_count and signature in seen_course_pages:
                raise CampusParseError("UCloud course pagination repeated a page")
            seen_course_pages.add(signature)

            courses.extend(page_courses[: site_limit - len(courses)])
            fetched_course_records += record_count
            if len(courses) >= site_limit or _pagination_finished(
                record_count,
                course_page_size,
                fetched_course_records,
                total,
            ):
                break
        else:
            raise CampusParseError("UCloud course pagination exceeded the safety limit")

        all_homework: list[HomeworkItem] = []
        for course in courses:
            if not course.site_id:
                continue
            fetched_homework_records = 0
            seen_homework_pages: set[tuple[str, ...]] = set()
            for page in range(1, MAX_PAGINATION_PAGES + 1):
                token_data = self._session_mgr.get_ucloud_token()
                user_id = token_data.get("user_id")
                if not user_id:
                    raise CampusAuthError("UCloud token missing user_id")
                data = self._request(
                    "POST",
                    "/ykt-site/work/student/list",
                    json_body={
                        "siteId": course.site_id,
                        "userId": user_id,
                        "current": page,
                        "size": page_size,
                    },
                )
                works, record_count, total, signature = _parse_homework_page(
                    data, course.site_id
                )
                if record_count and signature in seen_homework_pages:
                    raise CampusParseError(
                        f"UCloud homework pagination repeated a page for site '{course.site_id}'"
                    )
                seen_homework_pages.add(signature)

                for work in works:
                    if not work.site_name:
                        object.__setattr__(work, "site_name", course.site_name)
                    all_homework.append(work)
                fetched_homework_records += record_count
                if _pagination_finished(
                    record_count,
                    page_size,
                    fetched_homework_records,
                    total,
                ):
                    break
            else:
                raise CampusParseError(
                    f"UCloud homework pagination exceeded the safety limit for site '{course.site_id}'"
                )
        return all_homework

    def get_detail(self, activity_id: str) -> HomeworkDetail:
        """Fetch detailed information and attachments for a specific assignment."""
        data = self._request(
            "GET", "/ykt-site/work/detail", params={"assignmentId": activity_id}
        )
        if data is None or data == {}:
            raise CampusResourceNotFoundError(
                f"Assignment detail not found for ID '{activity_id}'"
            )
        if not isinstance(data, dict):
            raise CampusParseError("UCloud assignment detail returned malformed data")

        submit_info_raw = data.get("submitInfo")
        if submit_info_raw is not None and not isinstance(submit_info_raw, dict):
            raise CampusParseError(
                "UCloud assignment detail returned malformed submitInfo"
            )
        submit_info = submit_info_raw or {}
        score_val = _first_present(submit_info, "score")
        if score_val is None:
            score_val = _first_present(data, "score")
        score = _parse_score(score_val)

        return HomeworkDetail(
            activity_id=activity_id,
            title=data.get("assignmentTitle") or data.get("title") or "",
            content_html=data.get("assignmentContent") or data.get("content") or "",
            attachments=data.get("attachmentList")
            or data.get("attachments")
            or data.get("assignmentResource")
            or [],
            is_submitted=bool(
                submit_info.get("isSubmit") or data.get("isSubmit")
            )
            or _assignment_status_submitted(data.get("assignmentStatus")),
            submit_content=submit_info.get("submitContent"),
            submit_attachments=submit_info.get("attachmentList") or [],
            score=score,
            teacher_comment=submit_info.get("comment"),
            raw=data,
        )


class AsyncHomeworkService:
    """Asynchronous service for querying BUPT UCloud homework and courses."""

    def __init__(self, session_manager: AsyncBuptSessionManager):
        self._session_mgr = session_manager

    async def _request(
        self,
        method: str,
        path: str,
        params: Optional[dict[str, Any]] = None,
        json_body: Optional[dict[str, Any]] = None,
        retry_on_auth_fail: bool = True,
    ) -> Any:
        token_data = await self._session_mgr.get_ucloud_token()
        cli = self._session_mgr.client
        url = UCLOUD_API + path
        headers = _build_ucloud_headers(token_data["access_token"])

        try:
            resp = await cli.request(
                method, url, params=params, json=json_body, headers=headers
            )
        except httpx.HTTPError as e:
            raise CampusNetworkError(f"UCloud API {path} failed: {e}") from e

        if resp.status_code == 401:
            if retry_on_auth_fail:
                self._session_mgr.invalidate()
                return await self._request(
                    method, path, params, json_body, retry_on_auth_fail=False
                )
            raise CampusAuthError("UCloud request unauthorized (token expired)")

        try:
            resp.raise_for_status()
        except httpx.HTTPError as e:
            raise CampusNetworkError(f"UCloud API {path} failed: {e}") from e

        try:
            payload = resp.json()
        except Exception as e:
            raise CampusParseError(f"UCloud API {path} invalid JSON: {e}") from e

        if not isinstance(payload, dict):
            raise CampusParseError(f"UCloud API {path} returned malformed payload")

        code = payload.get("code")
        if code != 200 and code != 0 and code is not None:
            msg = str(payload.get("msg") or payload)
            if "token" in msg.lower() or "auth" in msg.lower() or "登录" in msg:
                if retry_on_auth_fail:
                    self._session_mgr.invalidate()
                    return await self._request(
                        method, path, params, json_body, retry_on_auth_fail=False
                    )
                raise CampusAuthError(f"UCloud authentication error: {msg}")
            raise CampusNetworkError(f"UCloud API {path} error: {msg}")

        if "data" not in payload:
            raise CampusParseError(f"UCloud API {path} response missing data")
        return payload["data"]

    async def get_undone(self) -> HomeworkOverview:
        """Fetch pending (unfinished) assignments across all enrolled courses asynchronously."""
        token_data = await self._session_mgr.get_ucloud_token()
        user_id = token_data.get("user_id")
        if not user_id:
            raise CampusAuthError("UCloud token missing user_id")

        data = await self._request(
            "GET", "/ykt-site/site/student/undone", params={"userId": user_id}
        )
        overview_data = _require_dict(data, "UCloud undone homework")
        undone_records = overview_data.get("undoneList")
        if not isinstance(undone_records, list) or not all(
            isinstance(item, dict) for item in undone_records
        ):
            raise CampusParseError("UCloud undone homework returned malformed records")

        undone_items: list[HomeworkItem] = []
        for item in undone_records:
            undone_items.append(
                HomeworkItem(
                    activity_id=str(item.get("activityId") or item.get("id") or ""),
                    activity_name=item.get("activityName")
                    or item.get("assignmentTitle")
                    or "",
                    site_id=str(item.get("siteId")) if item.get("siteId") else None,
                    site_name=item.get("siteName") or "",
                    end_time=item.get("endTime"),
                    begin_time=item.get("beginTime"),
                    is_submitted=False,
                    raw=item,
                )
            )

        return HomeworkOverview(
            total_sites=overview_data.get("siteNum", 0),
            undone_count=overview_data.get("undoneNum", len(undone_items)),
            undone_list=undone_items,
            raw=overview_data,
        )

    async def find_list_record(
        self,
        assignment_id: str,
        site_ids: Iterable[str],
        page_size: int = 50,
    ) -> Optional[HomeworkItem]:
        """Find one assignment in the student lists of ``site_ids`` asynchronously."""
        target = str(assignment_id)
        _validate_page_args(page_size, 1)
        token_data = await self._session_mgr.get_ucloud_token()
        user_id = token_data.get("user_id")
        if not user_id:
            raise CampusAuthError("UCloud token missing user_id")

        for site_id in site_ids:
            site = str(site_id or "").strip()
            if not site:
                continue
            fetched_records = 0
            seen_pages: set[tuple[str, ...]] = set()
            for page in range(1, MAX_PAGINATION_PAGES + 1):
                data = await self._request(
                    "POST",
                    "/ykt-site/work/student/list",
                    json_body={
                        "siteId": site,
                        "userId": user_id,
                        "current": page,
                        "size": page_size,
                    },
                )
                works, record_count, total, signature = _parse_homework_page(
                    data, site
                )
                if record_count and signature in seen_pages:
                    raise CampusParseError(
                        f"UCloud homework pagination repeated a page for site '{site}'"
                    )
                seen_pages.add(signature)

                for work in works:
                    if work.activity_id == target or str(
                        work.raw.get("id") or ""
                    ) == target:
                        return work
                fetched_records += record_count
                if _pagination_finished(
                    record_count, page_size, fetched_records, total
                ):
                    break
            else:
                raise CampusParseError(
                    f"UCloud homework pagination exceeded the safety limit for site '{site}'"
                )
        return None

    async def get_courses(self, size: int = 50, page: int = 1) -> list[CourseSite]:
        """Fetch currently enrolled course sites asynchronously."""
        _validate_page_args(size, page)
        data = await self._request(
            "GET",
            "/ykt-site/site/list/student/current",
            params={"size": size, "current": page},
        )
        return _parse_course_page(data)[0]

    async def get_course_homework(
        self,
        site_id: str,
        page_size: int = 50,
        page: int = 1,
    ) -> list[HomeworkItem]:
        """Fetch homework assignments for a specific course site asynchronously."""
        _validate_page_args(page_size, page)
        token_data = await self._session_mgr.get_ucloud_token()
        user_id = token_data.get("user_id")
        if not user_id:
            raise CampusAuthError("UCloud token missing user_id")

        body = {
            "siteId": site_id,
            "userId": user_id,
            "current": page,
            "size": page_size,
        }
        data = await self._request(
            "POST", "/ykt-site/work/student/list", json_body=body
        )
        return _parse_homework_page(data, site_id)[0]

    async def get_all(
        self, site_limit: int = 30, page_size: int = 50
    ) -> list[HomeworkItem]:
        """Fetch all homework for at most ``site_limit`` courses, following every result page."""
        if site_limit < 1:
            return []
        _validate_page_args(page_size, 1)

        course_page_size = min(site_limit, page_size)
        courses: list[CourseSite] = []
        fetched_course_records = 0
        seen_course_pages: set[tuple[str, ...]] = set()
        for page in range(1, MAX_PAGINATION_PAGES + 1):
            data = await self._request(
                "GET",
                "/ykt-site/site/list/student/current",
                params={"size": course_page_size, "current": page},
            )
            page_courses, record_count, total, signature = _parse_course_page(data)
            if record_count and signature in seen_course_pages:
                raise CampusParseError("UCloud course pagination repeated a page")
            seen_course_pages.add(signature)

            courses.extend(page_courses[: site_limit - len(courses)])
            fetched_course_records += record_count
            if len(courses) >= site_limit or _pagination_finished(
                record_count,
                course_page_size,
                fetched_course_records,
                total,
            ):
                break
        else:
            raise CampusParseError("UCloud course pagination exceeded the safety limit")

        all_homework: list[HomeworkItem] = []
        for course in courses:
            if not course.site_id:
                continue
            fetched_homework_records = 0
            seen_homework_pages: set[tuple[str, ...]] = set()
            for page in range(1, MAX_PAGINATION_PAGES + 1):
                token_data = await self._session_mgr.get_ucloud_token()
                user_id = token_data.get("user_id")
                if not user_id:
                    raise CampusAuthError("UCloud token missing user_id")
                data = await self._request(
                    "POST",
                    "/ykt-site/work/student/list",
                    json_body={
                        "siteId": course.site_id,
                        "userId": user_id,
                        "current": page,
                        "size": page_size,
                    },
                )
                works, record_count, total, signature = _parse_homework_page(
                    data, course.site_id
                )
                if record_count and signature in seen_homework_pages:
                    raise CampusParseError(
                        f"UCloud homework pagination repeated a page for site '{course.site_id}'"
                    )
                seen_homework_pages.add(signature)

                for work in works:
                    if not work.site_name:
                        object.__setattr__(work, "site_name", course.site_name)
                    all_homework.append(work)
                fetched_homework_records += record_count
                if _pagination_finished(
                    record_count,
                    page_size,
                    fetched_homework_records,
                    total,
                ):
                    break
            else:
                raise CampusParseError(
                    f"UCloud homework pagination exceeded the safety limit for site '{course.site_id}'"
                )
        return all_homework

    async def get_detail(self, activity_id: str) -> HomeworkDetail:
        """Fetch detailed information and attachments for a specific assignment asynchronously."""
        data = await self._request(
            "GET", "/ykt-site/work/detail", params={"assignmentId": activity_id}
        )
        if data is None or data == {}:
            raise CampusResourceNotFoundError(
                f"Assignment detail not found for ID '{activity_id}'"
            )
        if not isinstance(data, dict):
            raise CampusParseError("UCloud assignment detail returned malformed data")

        submit_info_raw = data.get("submitInfo")
        if submit_info_raw is not None and not isinstance(submit_info_raw, dict):
            raise CampusParseError(
                "UCloud assignment detail returned malformed submitInfo"
            )
        submit_info = submit_info_raw or {}
        score_val = _first_present(submit_info, "score")
        if score_val is None:
            score_val = _first_present(data, "score")
        score = _parse_score(score_val)

        return HomeworkDetail(
            activity_id=activity_id,
            title=data.get("assignmentTitle") or data.get("title") or "",
            content_html=data.get("assignmentContent") or data.get("content") or "",
            attachments=data.get("attachmentList")
            or data.get("attachments")
            or data.get("assignmentResource")
            or [],
            is_submitted=bool(
                submit_info.get("isSubmit") or data.get("isSubmit")
            )
            or _assignment_status_submitted(data.get("assignmentStatus")),
            submit_content=submit_info.get("submitContent"),
            submit_attachments=submit_info.get("attachmentList") or [],
            score=score,
            teacher_comment=submit_info.get("comment"),
            raw=data,
        )
