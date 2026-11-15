"""schedule.py - BUPT Qiangzhi educational administration schedule service."""

import datetime as dt
import re
from typing import Optional

import httpx
from lxml import etree

from bupt_cli._vendor.campus_api.bupt.auth import AsyncJwglSessionManager, JwglSessionManager
from bupt_cli._vendor.campus_api.bupt.models import WEEKDAYS, CourseItem, ScheduleOverview
from bupt_cli._vendor.campus_api.exceptions import (
    CampusAuthError,
    CampusNetworkError,
    CampusParseError,
)

JWGL_BASE = "https://jwgl.bupt.edu.cn"
SCHEDULE_URL = f"{JWGL_BASE}/jsxsd/framework/xsdPerson.jsp"
DEFAULT_KBJCMSID = "9475847A3F3033D1E05377B5030AA94D"
LOGIN_PAGE_MARKERS = ("LoginToXk", "用户登录", "统一身份认证")
ERROR_PAGE_MARKERS = ("系统错误", "错误页面", "访问出错", "Internal Server Error")


def calculate_current_week(anchor_date: Optional[dt.date] = None) -> int:
    """Calculate the current academic week number based on semester anchor date.

    Default anchor is 2026-03-02 for Spring 2026 term, or 2026-08-31 for Autumn 2026 term.
    """
    today = dt.date.today()
    if anchor_date is None:
        # If in or after August, use autumn anchor, else spring anchor
        if today.month >= 8:
            anchor_date = dt.date(today.year, 8, 31)
        else:
            anchor_date = dt.date(today.year, 3, 2)

    days_diff = (today - anchor_date).days
    week = max(1, days_diff // 7 + 1)
    return min(week, 25)


def parse_schedule_html(html_text: str, week: int) -> ScheduleOverview:
    """Parse Qiangzhi schedule HTML into structured ScheduleOverview."""
    if _is_login_page(html_text):
        raise CampusAuthError("JWGL session expired or login required")

    try:
        root = etree.HTML(html_text)
    except Exception as e:
        raise CampusParseError(f"Failed to parse schedule HTML: {e}") from e
    if root is None:
        raise CampusParseError("Failed to parse schedule HTML")

    table_nodes = root.xpath(
        "//*[contains(concat(' ', normalize-space(@class), ' '), ' table-content ')]"
    )
    if not table_nodes:
        if any(marker.lower() in html_text.lower() for marker in ERROR_PAGE_MARKERS):
            raise CampusParseError("JWGL returned an error page instead of a schedule")
        raise CampusParseError("Schedule page structure missing expected table layout")

    # 1. Parse header dates
    header_lis = root.xpath("//ul[contains(@class,'table-header')]/li")
    dates_by_day: dict[int, str] = {}
    sequential_dates: list[str] = []
    for li in header_lis:
        text = " ".join(part.strip() for part in li.itertext() if part.strip())
        date_match = re.search(r"(?<!\d)(\d{2}-\d{2})(?!\d)", text)
        if not date_match:
            continue
        date_str = date_match.group(1)
        sequential_dates.append(date_str)
        for day, day_name in enumerate(WEEKDAYS):
            if day_name in text:
                dates_by_day[day] = date_str
                break

    if not dates_by_day:
        dates_by_day = {day: value for day, value in enumerate(sequential_dates[:7])}
    dates = _complete_week_dates(dates_by_day)

    # 2. Parse course blocks
    blocks = root.xpath(
        "//div[contains(@class,'table-class') and contains(@class,'div-context')]"
    )
    courses: list[CourseItem] = []

    for blk in blocks:
        cls_str = " ".join(blk.get("class", "").split())
        day_match = re.search(r"\bday(\d)\b", cls_str)
        if not day_match:
            continue
        day = int(day_match.group(1))
        if day < 0 or day > 6:
            continue

        # Course name
        h4_nodes = blk.xpath("./h4/text()")
        name = h4_nodes[0].strip() if h4_nodes else ""

        # Key-value details
        li_nodes = blk.xpath("./ul/li")
        details: dict[str, str] = {}
        for li in li_nodes:
            line = "".join(li.itertext()).strip()
            parts = re.split(r"[:：]", line, maxsplit=1)
            if len(parts) == 2:
                details[parts[0].strip()] = parts[1].strip()

        if not name and not details:
            continue

        time_str = details.get("上课时间", "")
        style = blk.get("style", "")
        top_k = _try_extract_style_multiplier(style, "top")
        height_h = _try_extract_style_multiplier(style, "height")
        time_sections = _extract_time_sections(time_str)
        section_start = (
            top_k + 1
            if top_k is not None
            else (time_sections[0] if time_sections else 1)
        )
        section_span = (
            max(1, height_h)
            if height_h is not None
            else (time_sections[1] - time_sections[0] + 1 if time_sections else 1)
        )

        credit_val = details.get("课程学分")
        credit_match = re.search(r"\d+(?:\.\d+)?", credit_val or "")
        credit = float(credit_match.group()) if credit_match else None

        sections_display = (
            f"第{section_start}节"
            if section_span <= 1
            else f"第{section_start}-{section_start + section_span - 1}节"
        )

        weeks_display = ""
        w_match = re.search(r"第\s*([\d,，\-]+)\s*周", time_str)
        if w_match:
            weeks_display = f"{w_match.group(1)}周"

        courses.append(
            CourseItem(
                name=name,
                day=day,
                day_name=WEEKDAYS[day],
                section_start=section_start,
                section_span=section_span,
                sections_display=sections_display,
                time_str=time_str,
                credit=credit,
                attr=details.get("课程属性", ""),
                classes=details.get("上课班级", ""),
                campus=details.get("上课校区", ""),
                weeks_display=weeks_display,
                location=details.get("上课地点") or details.get("上课教室", ""),
                raw={"style": style, "class": cls_str, "details": details},
            )
        )

    # 3. Deduplicate (server renders duplicate course blocks for multiple classes/weeks)
    seen: set[tuple[int, str, int, int, str]] = set()
    unique_courses: list[CourseItem] = []
    for c in courses:
        key = (c.day, c.name, c.section_start, c.section_span, c.location)
        if key in seen:
            continue
        seen.add(key)
        unique_courses.append(c)

    # Sort courses by weekday, then start section
    unique_courses.sort(key=lambda c: (c.day, c.section_start))

    return ScheduleOverview(
        week=week,
        dates=dates,
        courses=unique_courses,
        raw_html=html_text,
    )


def _extract_style_multiplier(style: str, key: str) -> int:
    """Extract multiplier N from `calc((N * 95px) + 2px)` or `calc(N * 95px - 4px)`."""
    return _try_extract_style_multiplier(style, key) or 0


def _try_extract_style_multiplier(style: str, key: str) -> Optional[int]:
    pattern = re.escape(key) + r"\s*:\s*calc\s*\(\s*\(?\s*(\d+)"
    match = re.search(pattern, style)
    return int(match.group(1)) if match else None


def _extract_time_sections(time_str: str) -> Optional[tuple[int, int]]:
    match = re.search(r"\[(\d{1,2})\s*[-~—–至]\s*(\d{1,2})\]\s*节", time_str)
    if not match:
        match = re.search(r"第?\s*(\d{1,2})\s*(?:[-~—–至]\s*(\d{1,2}))?\s*节", time_str)
    if not match:
        return None
    start = int(match.group(1))
    end = int(match.group(2) or start)
    if start < 1 or end < start:
        return None
    return start, end


def _complete_week_dates(dates_by_day: dict[int, str]) -> list[str]:
    """Return seven MM-DD values, inferring missing days from any valid header date."""
    dates = [""] * 7
    valid_dates: dict[int, dt.date] = {}
    for day, value in dates_by_day.items():
        if not 0 <= day < 7:
            continue
        try:
            parsed = dt.datetime.strptime(f"2000-{value}", "%Y-%m-%d").date()
        except ValueError:
            continue
        dates[day] = value
        valid_dates[day] = parsed

    if valid_dates:
        anchor_day, anchor_date = next(iter(valid_dates.items()))
        for day in range(7):
            if not dates[day]:
                dates[day] = (
                    anchor_date + dt.timedelta(days=day - anchor_day)
                ).strftime("%m-%d")
    return dates


def _is_login_page(html_text: str) -> bool:
    lowered = html_text.lower()
    return any(marker.lower() in lowered for marker in LOGIN_PAGE_MARKERS)


class ScheduleService:
    """Synchronous service for querying BUPT educational course schedule."""

    def __init__(self, session_manager: JwglSessionManager):
        self._session_mgr = session_manager

    def get_schedule(
        self, week: Optional[int] = None, retry_on_auth_fail: bool = True
    ) -> ScheduleOverview:
        """Fetch weekly course schedule.

        Args:
            week (Optional[int]): Week number (1-25). Computed from semester anchor if omitted.

        Returns:
            ScheduleOverview: Parsed weekly schedule with deduplicated courses.
        """
        target_week = week if week is not None else calculate_current_week()
        cli = self._session_mgr.ensure_login()

        try:
            resp = cli.get(
                SCHEDULE_URL,
                params={"kbjcmsid": DEFAULT_KBJCMSID, "xkzc": target_week},
            )
            if (
                "LoginToXk" in str(resp.url)
                or resp.status_code == 401
                or resp.status_code == 302
            ):
                if retry_on_auth_fail:
                    self._session_mgr.invalidate()
                    return self.get_schedule(week=week, retry_on_auth_fail=False)
                raise CampusAuthError("JWGL session expired or login required")

            resp.raise_for_status()
            if _is_login_page(resp.text):
                if retry_on_auth_fail:
                    self._session_mgr.invalidate()
                    return self.get_schedule(week=week, retry_on_auth_fail=False)
                raise CampusAuthError("JWGL session expired or login required")
            return parse_schedule_html(resp.text, week=target_week)

        except (CampusAuthError, CampusParseError):
            raise
        except httpx.HTTPError as e:
            raise CampusNetworkError(f"JWGL schedule fetch failed: {e}") from e

    def get_today_schedule(
        self,
        week: Optional[int] = None,
        target_date: Optional[dt.date] = None,
    ) -> list[CourseItem]:
        """Fetch courses scheduled for today (or specified date)."""
        overview = self.get_schedule(week=week)
        return overview.get_today_courses(target_date=target_date)


class AsyncScheduleService:
    """Asynchronous service for querying BUPT educational course schedule."""

    def __init__(self, session_manager: AsyncJwglSessionManager):
        self._session_mgr = session_manager

    async def get_schedule(
        self, week: Optional[int] = None, retry_on_auth_fail: bool = True
    ) -> ScheduleOverview:
        """Fetch weekly course schedule asynchronously."""
        target_week = week if week is not None else calculate_current_week()
        cli = await self._session_mgr.ensure_login()

        try:
            resp = await cli.get(
                SCHEDULE_URL,
                params={"kbjcmsid": DEFAULT_KBJCMSID, "xkzc": target_week},
            )
            if (
                "LoginToXk" in str(resp.url)
                or resp.status_code == 401
                or resp.status_code == 302
            ):
                if retry_on_auth_fail:
                    self._session_mgr.invalidate()
                    return await self.get_schedule(week=week, retry_on_auth_fail=False)
                raise CampusAuthError("JWGL session expired or login required")

            resp.raise_for_status()
            if _is_login_page(resp.text):
                if retry_on_auth_fail:
                    self._session_mgr.invalidate()
                    return await self.get_schedule(week=week, retry_on_auth_fail=False)
                raise CampusAuthError("JWGL session expired or login required")
            return parse_schedule_html(resp.text, week=target_week)

        except (CampusAuthError, CampusParseError):
            raise
        except httpx.HTTPError as e:
            raise CampusNetworkError(f"JWGL schedule fetch failed: {e}") from e

    async def get_today_schedule(
        self,
        week: Optional[int] = None,
        target_date: Optional[dt.date] = None,
    ) -> list[CourseItem]:
        """Fetch courses scheduled for today (or specified date) asynchronously."""
        overview = await self.get_schedule(week=week)
        return overview.get_today_courses(target_date=target_date)
