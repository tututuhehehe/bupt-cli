"""Local timetable cache shared by undergraduate and graduate accounts.

Both academic systems answer the same question — "which classes, on which
dates" — from different data:

* graduate (`yjxt`) hands over the whole semester grid plus an authoritative
  week→date table, so every date is known up front;
* undergraduate (mobile JWGL) hands over numeric weeks, weekday and clock
  times per course, but no semester anchor, so week 1 has to be derived from
  the current teaching week.

The cache normalises both into one shape, which lets `bupt schedule --today`
answer from disk without touching a browser or the network.

File: ``~/.bupt-cli/timetable.json``. It is a cache, so a corrupt or outdated
file is simply rebuilt rather than treated as an error.
"""

from __future__ import annotations

import datetime as dt
import json
import os
import time
from pathlib import Path
from typing import Any, Optional

CACHE_FILENAME = "timetable.json"
CACHE_VERSION = 1
WEEKDAYS = ["星期一", "星期二", "星期三", "星期四", "星期五", "星期六", "星期日"]


class TimetableError(Exception):
    """The cache cannot be used and must be refreshed."""

    def __init__(self, message: str, hint: str = "") -> None:
        super().__init__(message)
        self.hint = hint


# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------


def cache_dir() -> Path:
    from bupt_cli.session import cache_dir as session_cache_dir

    return session_cache_dir()


def cache_path() -> Path:
    return cache_dir() / CACHE_FILENAME


# ---------------------------------------------------------------------------
# Load / save
# ---------------------------------------------------------------------------


def load(*, path: Optional[Path] = None) -> Optional[dict[str, Any]]:
    """Return the cached timetable, or None when absent/unusable."""
    target = path or cache_path()
    try:
        with target.open("r", encoding="utf-8") as handle:
            data = json.load(handle)
    except (OSError, ValueError):
        return None
    if not isinstance(data, dict):
        return None
    if data.get("version") != CACHE_VERSION:
        return None
    if not isinstance(data.get("courses"), list):
        return None
    if not isinstance(data.get("calendar"), dict):
        return None
    return data


def save(entry: dict[str, Any], *, path: Optional[Path] = None) -> Path:
    """Write the timetable atomically with mode 0600."""
    target = path or cache_path()
    target.parent.mkdir(parents=True, exist_ok=True)
    payload = dict(entry)
    payload["version"] = CACHE_VERSION
    payload.setdefault("fetched_at", dt.datetime.now(dt.timezone.utc).isoformat())
    tmp = target.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(payload, ensure_ascii=False, indent=2), encoding="utf-8")
    try:
        tmp.chmod(0o600)
    except OSError:
        pass
    os.replace(tmp, target)
    return target


def fetched_at(entry: dict[str, Any]) -> Optional[dt.datetime]:
    raw = entry.get("fetched_at")
    if not raw:
        return None
    try:
        return dt.datetime.fromisoformat(str(raw))
    except ValueError:
        return None


# ---------------------------------------------------------------------------
# Queries
# ---------------------------------------------------------------------------


def _all_dates(entry: dict[str, Any]) -> list[str]:
    dates: list[str] = []
    for week_dates in (entry.get("calendar") or {}).values():
        if isinstance(week_dates, list):
            dates.extend(str(d) for d in week_dates if d)
    return dates


def covers(entry: dict[str, Any], day: dt.date) -> bool:
    """True when ``day`` falls inside the cached teaching weeks."""
    return day.isoformat() in set(_all_dates(entry))


def is_stale(entry: Optional[dict[str, Any]], *, today: Optional[dt.date] = None) -> bool:
    """A cache is stale once ``today`` leaves its teaching weeks."""
    if not entry:
        return True
    return not covers(entry, today or dt.date.today())


def occurrences(
    entry: dict[str, Any],
    start: dt.date,
    end: dt.date,
) -> list[dict[str, Any]]:
    """Every class session between ``start`` and ``end`` inclusive, sorted.

    A course that spans several periods is a single session; its clock times
    come from the source system, so no period table is needed here.
    """
    calendar = entry.get("calendar") or {}
    out: list[dict[str, Any]] = []
    for course in entry.get("courses") or []:
        if not isinstance(course, dict):
            continue
        try:
            weekday = int(course.get("weekday_index"))
        except (TypeError, ValueError):
            continue
        # The cache file is user-editable JSON, so a stray -1 would otherwise
        # wrap around and place the class on Sunday; 7+ would raise IndexError.
        if not 0 <= weekday <= 6:
            continue
        weeks = course.get("weeks") or []
        if not isinstance(weeks, list):
            continue
        for week in weeks:
            if not isinstance(week, int):
                continue
            week_dates = calendar.get(str(week))
            if not isinstance(week_dates, list) or weekday >= len(week_dates):
                continue
            raw_date = week_dates[weekday]
            if not raw_date:
                continue
            try:
                day = dt.date.fromisoformat(str(raw_date))
            except ValueError:
                continue
            if not (start <= day <= end):
                continue
            out.append(
                {
                    "date": day.isoformat(),
                    "weekday": WEEKDAYS[weekday],
                    "week": week,
                    "time_start": course.get("time_start") or "",
                    "time_end": course.get("time_end") or "",
                    "name": course.get("name") or "",
                    "teachers": course.get("teachers") or [],
                    "rooms": course.get("rooms") or [],
                    "campus": course.get("campus") or "",
                    "period_start": course.get("period_start"),
                    "period_end": course.get("period_end"),
                }
            )
    out.sort(key=lambda x: (x["date"], x["time_start"], x["name"]))
    return out


def day_range(entry: dict[str, Any], day: dt.date) -> tuple[dt.date, dt.date]:
    """Monday..Sunday of the teaching week containing ``day``."""
    for week_dates in (entry.get("calendar") or {}).values():
        if not isinstance(week_dates, list) or not week_dates:
            continue
        if day.isoformat() in [str(d) for d in week_dates]:
            try:
                monday = dt.date.fromisoformat(str(week_dates[0]))
                sunday = dt.date.fromisoformat(str(week_dates[-1]))
            except (ValueError, IndexError):
                break
            return monday, sunday
    # Outside the term: fall back to the calendar week so the output is sane.
    monday = day - dt.timedelta(days=day.weekday())
    return monday, monday + dt.timedelta(days=6)


def week_of(entry: dict[str, Any], day: dt.date) -> Optional[int]:
    """Teaching week number containing ``day``, or None outside the term."""
    for key, week_dates in (entry.get("calendar") or {}).items():
        if isinstance(week_dates, list) and day.isoformat() in [str(d) for d in week_dates]:
            try:
                return int(key)
            except (TypeError, ValueError):
                return None
    return None


def week_bounds(entry: dict[str, Any]) -> Optional[tuple[dt.date, dt.date]]:
    """First and last date covered by the cache."""
    dates = sorted(set(_all_dates(entry)))
    if not dates:
        return None
    try:
        return dt.date.fromisoformat(dates[0]), dt.date.fromisoformat(dates[-1])
    except ValueError:
        return None


# ---------------------------------------------------------------------------
# Normalising source data into the cache shape
# ---------------------------------------------------------------------------


def build_calendar(monday_of_week_one: dt.date, week_count: int) -> dict[str, list[str]]:
    """Monday-anchored week→dates table.

    Used for the undergraduate system, which only reports the *current* week;
    every other week is plain arithmetic from its Monday.
    """
    calendar: dict[str, list[str]] = {}
    for week in range(1, week_count + 1):
        start = monday_of_week_one + dt.timedelta(days=7 * (week - 1))
        calendar[str(week)] = [
            (start + dt.timedelta(days=i)).isoformat() for i in range(7)
        ]
    return calendar


def make_entry(
    *,
    role: str,
    term: str,
    courses: list[dict[str, Any]],
    calendar: dict[str, list[str]],
    source: str,
) -> dict[str, Any]:
    return {
        "version": CACHE_VERSION,
        "role": role,
        "term": term,
        "source": source,
        "fetched_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "courses": courses,
        "calendar": calendar,
    }


# ---------------------------------------------------------------------------
# Graduate source: browser-backed (Riversafe)
# ---------------------------------------------------------------------------


def fetch_graduate(
    username: str, password: str, *, force_browser: bool = True
) -> dict[str, Any]:
    """Fetch the graduate timetable + teaching calendar via a headless browser."""
    from bupt_cli import grad, grad_browser

    pages = grad_browser.collect(username, password, want=("schedule", "calendar"))
    if "schedule" not in pages or "calendar" not in pages:
        raise TimetableError(
            "没能取到研究生课表或教学日历",
            hint="重试一次；若持续失败，检查 ~/.bupt-cli/credentials.json 的 role",
        )

    courses = grad.merge_periods(grad.parse_timetable(pages["schedule"]))
    term = grad.term_from_html(pages["calendar"]) or ""
    year = grad.academic_year_start(term)
    if year is None:
        raise TimetableError(
            "教学日历页里找不到学期标签，无法确定学年",
            hint="页面结构可能已变",
        )
    weeks = grad.parse_calendar(pages["calendar"], year)
    return make_entry(
        role="graduate",
        term=term,
        courses=courses,
        calendar={str(k): v for k, v in weeks.items()},
        source="yjxt",
    )


# ---------------------------------------------------------------------------
# Undergraduate source: mobile JWGL
# ---------------------------------------------------------------------------


def fetch_undergraduate(client: Any) -> dict[str, Any]:
    """Fetch the undergraduate timetable from mobile JWGL.

    ``get_curriculum()`` returns one entry per teaching week, and each entry
    carries ``date: list[SjdDay]`` where ``SjdDay`` has both ``zc`` (week
    number) and ``mxrq`` (the real date, e.g. ``2026-08-24``). That is the same
    kind of authoritative week→date data the graduate calendar page provides,
    so nothing is inferred from "today's week number".

    .. warning::
       **This path has not been run against a real undergraduate account**
       (none was available while it was written). The data model above is read
       straight from the vendored SDK, but two things are unverified: that the
       endpoint returns *every* week in one call, and that ``zc`` numbering
       starts at 1 for the first teaching week. Run `bupt schedule update` on an
       undergraduate account and check the cache before trusting it.
    """
    weeks = client.get_curriculum()
    if not weeks:
        raise TimetableError("移动教务没有返回课表数据")

    calendar: dict[str, list[str]] = {}
    term = ""
    for week in weeks:
        if not term:
            term = str(getattr(week, "semester_id", "") or "")
        day_dates: dict[int, str] = {}
        for day in getattr(week, "date", []) or []:
            week_no = str(getattr(day, "zc", "") or "").strip()
            iso = str(getattr(day, "mxrq", "") or "").strip()
            weekday = str(getattr(day, "xqid", "") or "").strip()
            if not week_no or not iso or not weekday:
                continue
            try:
                idx = int(weekday) - 1
            except ValueError:
                continue
            if not 0 <= idx <= 6:
                continue
            if week_no not in calendar:
                calendar[week_no] = [""] * 7
            calendar[week_no][idx] = iso

    if not calendar:
        raise TimetableError(
            "移动教务的课表里没有可用的日期信息",
            hint="该后端可能只返回当前周；请反馈以便适配",
        )

    courses: list[dict[str, Any]] = []
    for week in weeks:
        for row in getattr(week, "courses", []) or []:
            weekday = str(getattr(row, "week_day", "") or "").strip()
            try:
                weekday_index = int(weekday) - 1
            except ValueError:
                continue
            if not 0 <= weekday_index <= 6:
                continue
            course_weeks = (
                row.week_numbers() if hasattr(row, "week_numbers") else []
            )
            if not course_weeks:
                # Fall back to the week this row was returned in.
                zc = str(getattr(week, "week", "") or "").strip()
                try:
                    course_weeks = [int(zc)]
                except ValueError:
                    continue
            rooms = [
                part
                for part in (
                    getattr(row, "building_name", ""),
                    getattr(row, "classroom_name", ""),
                )
                if part
            ]
            teacher = getattr(row, "teacher_name", "") or ""
            courses.append(
                {
                    "name": getattr(row, "course_name", "") or "",
                    "weekday_index": weekday_index,
                    "weeks": sorted({int(w) for w in course_weeks if isinstance(w, int)}),
                    "period_start": None,
                    "period_end": None,
                    "time_start": getattr(row, "start_time", "") or "",
                    "time_end": getattr(row, "end_time", "") or "",
                    "teachers": [teacher] if teacher else [],
                    "rooms": rooms,
                    "campus": getattr(row, "campus", "") or "",
                }
            )

    if not courses:
        raise TimetableError("移动教务的课表数据里没有可解析的课程")

    # De-duplicate: the same row can appear in several week entries.
    seen: set[tuple] = set()
    unique: list[dict[str, Any]] = []
    for course in courses:
        key = (
            course["name"],
            course["weekday_index"],
            tuple(course["weeks"]),
            course["time_start"],
            tuple(course["rooms"]),
        )
        if key in seen:
            continue
        seen.add(key)
        unique.append(course)

    return make_entry(
        role="undergraduate",
        term=term or "本学期",
        courses=unique,
        calendar=calendar,
        source="mobile_jwgl",
    )


# ---------------------------------------------------------------------------
# iCalendar export straight from the cache
# ---------------------------------------------------------------------------


def _ics_escape(text: str) -> str:
    return (
        str(text)
        .replace("\\", "\\\\")
        .replace(";", "\\;")
        .replace(",", "\\,")
        .replace("\n", "\\n")
    )


def _fold(line: str) -> str:
    """RFC 5545 §3.1: fold lines longer than 75 octets."""
    if len(line.encode("utf-8")) <= 75:
        return line
    out: list[str] = []
    current = b""
    for ch in line:
        raw = ch.encode("utf-8")
        if len(current) + len(raw) > 75:
            out.append(current.decode("utf-8"))
            current = b" "
        current += raw
    if current:
        out.append(current.decode("utf-8"))
    return "\r\n".join(out)


def build_ics(entry: dict[str, Any], *, now: Optional[str] = None) -> str:
    """One VEVENT per class occurrence, using the cache's own clock times.

    ``time_start``/``time_end`` come from the source system, so this works for
    both accounts (the graduate grid has period numbers, the undergraduate API
    has clock times directly).
    """
    bounds = week_bounds(entry)
    if bounds is None:
        raise TimetableError("缓存里没有日期信息")
    sessions = occurrences(entry, bounds[0], bounds[1])

    stamp = now or dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    term = str(entry.get("term") or "课表")
    lines = [
        "BEGIN:VCALENDAR",
        "VERSION:2.0",
        f"PRODID:-//bupt-cli//{_ics_escape(term)}//CN",
        "CALSCALE:GREGORIAN",
        "METHOD:PUBLISH",
        f"X-WR-CALNAME:{_ics_escape(term)}",
        "X-WR-TIMEZONE:Asia/Shanghai",
        "BEGIN:VTIMEZONE",
        "TZID:Asia/Shanghai",
        "BEGIN:STANDARD",
        "DTSTART:19700101T000000",
        "TZOFFSETFROM:+0800",
        "TZOFFSETTO:+0800",
        "END:STANDARD",
        "END:VTIMEZONE",
    ]
    emitted = 0
    for session in sessions:
        start_clock = str(session.get("time_start") or "")
        end_clock = str(session.get("time_end") or "")
        if not start_clock or not end_clock:
            continue
        day = str(session["date"]).replace("-", "")
        location = " / ".join(session.get("rooms") or [])
        if session.get("campus"):
            location = f"[{session['campus']}] {location}".strip()
        details = []
        if session.get("teachers"):
            details.append("教师: " + ", ".join(session["teachers"]))
        details.append(f"第 {session['week']} 周")
        lines += [
            "BEGIN:VEVENT",
            f"UID:bupt-{day}-{session['week']}-{session.get('period_start') or ''}-{emitted}@bupt-cli",
            f"DTSTAMP:{stamp}",
            f"DTSTART;TZID=Asia/Shanghai:{day}T{start_clock.replace(':', '')}00",
            f"DTEND;TZID=Asia/Shanghai:{day}T{end_clock.replace(':', '')}00",
            _fold(f"SUMMARY:{_ics_escape(session['name'])}"),
        ]
        if location:
            lines.append(_fold(f"LOCATION:{_ics_escape(location)}"))
        lines.append(_fold("DESCRIPTION:" + _ics_escape("; ".join(details))))
        lines.append("END:VEVENT")
        emitted += 1
    lines.append("END:VCALENDAR")
    return "\r\n".join(lines) + "\r\n"
