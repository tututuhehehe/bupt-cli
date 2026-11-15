"""Parsers for the graduate student system (yjxt.bupt.edu.cn).

This module performs **no I/O**: it turns yjxt HTML into JSON-ready dicts.
Fetching (CAS login, the Riversafe challenge, the headless browser) lives in
:mod:`bupt_cli.grad_browser`; keeping them apart means the parsers can be
tested offline against saved HTML.

What the pages look like (verified 2026-11):

* The timetable is an 8-column grid (节次 + 星期一..星期日). A course spanning
  several periods uses ``rowspan``, so the grid must be expanded before column
  positions can be trusted.
* The teaching calendar (``TermCalender.aspx``) is the authoritative
  week -> date mapping; do not derive dates from a week number.
* Cells carry the term label (``学期：2026-2027秋学期``), which is how the
  academic year is discovered instead of being hardcoded.
"""

from __future__ import annotations

import re
from typing import Any, Optional

# Only parsing lives here; fetching pages (CAS login, Riversafe, the headless
# browser) is in `grad_browser`. Keeping the regexes together makes it clear
# that this module never performs I/O.
_SELECT_RE = re.compile(r"<select[^>]*>(.*?)</select>", re.S | re.I)
_OPTION_RE = re.compile(r'<option[^>]*value="([^"]*)"[^>]*>([^<]*)</option>', re.I)
_ROW_RE = re.compile(r"<tr[^>]*>(.*?)</tr>", re.S | re.I)
_CELL_RE = re.compile(r"<t[dh]([^>]*)>(.*?)</t[dh]>", re.S | re.I)
_TAG_RE = re.compile(r"<[^>]+>")
_SPAN_RE = re.compile(r'rowspan="(\d+)"', re.I)
_WS_RE = re.compile(r"\s+")

# Official period table from the timetable page (45-minute periods).
PERIOD_TIMES = {
    1: ("08:00", "08:45"), 2: ("08:50", "09:35"), 3: ("09:50", "10:35"),
    4: ("10:40", "11:25"), 5: ("11:30", "12:15"), 6: ("13:00", "13:45"),
    7: ("13:50", "14:35"), 8: ("14:45", "15:30"), 9: ("15:40", "16:25"),
    10: ("16:35", "17:20"), 11: ("17:25", "18:10"), 12: ("18:30", "19:15"),
    13: ("19:20", "20:05"), 14: ("20:10", "20:55"), 15: ("21:00", "21:45"),
}

WEEKDAYS = ["星期一", "星期二", "星期三", "星期四", "星期五", "星期六", "星期日"]


class GradError(Exception):
    """Graduate-system failure (auth, layout drift, missing page)."""

    def __init__(self, message: str, hint: str = "") -> None:
        super().__init__(message)
        self.hint = hint


def clean_text(value: Any) -> str:
    if value is None:
        return ""
    return _WS_RE.sub(" ", _TAG_RE.sub("", str(value))).strip()


def term_options(html: str) -> list[tuple[str, str]]:
    """(value, label) pairs of the semester dropdown."""
    for block in _SELECT_RE.findall(html):
        options = _OPTION_RE.findall(block)
        if any("学期" in label or "暑期" in label for _, label in options):
            return [(v, clean_text(t)) for v, t in options]
    return []


# ---------------------------------------------------------------------------
# Course cell parsing
# ---------------------------------------------------------------------------

_COURSE_RE = re.compile(r"^\[([^\]]+)\](.+?)[｛{](.+)[｝}]$", re.S)
# Inside the braces: `4周[教师:王宏祥,地点:3-105]` segments, or `[教师:王宏祥]`
# without a room. Split on `[` so teacher/room never bleed into each other.
_SEGMENT_RE = re.compile(
    r"([\d,\-]+)周\s*\[([^\]]*)\]",
)
_TEACHER_RE = re.compile(r"教师[:：]\s*([^,\]]+)")
_ROOM_RE = re.compile(r"地点[:：]\s*([^,\]]+)")


def parse_course_cell(text: str) -> Optional[dict[str, Any]]:
    """Split one grid cell like `[西土城]光纤通信原理｛4周[教师:王,地点:3-105]…｝`.

    Returns ``None`` for empty cells and for unrecognised layouts, so a layout
    change degrades to "unknown" instead of inventing wrong data.
    """
    text = clean_text(text)
    if not text:
        return None
    match = _COURSE_RE.search(text)
    if not match:
        return {"campus": "", "name": text, "weeks": [], "teachers": [], "rooms": []}

    campus, name, body = match.group(1), match.group(2), match.group(3)
    weeks: list[int] = []
    teachers: list[str] = []
    rooms: list[str] = []
    for seg in _SEGMENT_RE.finditer(body):
        weeks.extend(_expand_weeks(seg.group(1)))
        detail = seg.group(2) or ""
        who = _TEACHER_RE.search(detail)
        if who:
            for person in who.group(1).split(","):
                person = person.strip()
                if person and person not in teachers:
                    teachers.append(person)
        room = _ROOM_RE.search(detail)
        if room and room.group(1).strip() not in rooms:
            rooms.append(room.group(1).strip())

    return {
        "campus": campus.strip(),
        "name": name.strip(),
        "weeks": sorted(set(weeks)),
        "teachers": teachers,
        "rooms": rooms,
    }


def _expand_weeks(spec: str) -> list[int]:
    """`4`, `5-11`, `1,3,5` → concrete week numbers (bounded 1..30)."""
    out: list[int] = []
    for part in spec.replace("周", "").split(","):
        part = part.strip()
        if not part:
            continue
        if "-" in part:
            lo, _, hi = part.partition("-")
            try:
                a, b = int(lo), int(hi)
            except ValueError:
                continue
            out.extend(range(max(1, a), min(31, b) + 1))
        else:
            try:
                n = int(part)
            except ValueError:
                continue
            if 1 <= n <= 30:
                out.append(n)
    return out


# ---------------------------------------------------------------------------
# Timetable grid
# ---------------------------------------------------------------------------


def parse_timetable(html: str) -> list[dict[str, Any]]:
    """Expand the rowspan grid into one entry per (course, weekday, period).

    Returns a flat list sorted by weekday then period, each entry carrying
    ``weekday`` (0=Monday), ``period``, ``time_start``/``time_end`` and the
    parsed course fields.
    """
    grid: dict[tuple[int, int], str] = {}
    table = None
    for candidate in re.findall(r"<table[^>]*>(.*?)</table>", html, re.S | re.I):
        if "星期一" in candidate:
            table = candidate
            break
    if table is None:
        raise GradError("课表页面里找不到星期表头（页面结构可能已变）")

    rows = _ROW_RE.findall(table)
    # Read the weekday names from the header instead of assuming their order.
    header_cells = [clean_text(c) for _, c in _CELL_RE.findall(rows[0])] if rows else []
    weekday_names = [
        name for name in header_cells if name in WEEKDAYS
    ] or list(WEEKDAYS)
    first_weekday_col = next(
        (i for i, name in enumerate(header_cells) if name in WEEKDAYS), 1
    )

    for ri, row in enumerate(rows):
        if ri == 0:
            continue  # header
        cells = _CELL_RE.findall(row)
        col = 0
        for attr, content in cells:
            # Columns already filled by a rowspan from an earlier row are
            # absent from this <tr>, so advance past them before placing.
            while (ri, col) in grid:
                col += 1
            if col > 7:
                break
            span_match = _SPAN_RE.search(attr)
            span = int(span_match.group(1)) if span_match else 1
            for k in range(span):
                grid[(ri + k, col)] = content
            col += 1

    entries: list[dict[str, Any]] = []
    seen: set[tuple[int, int, str]] = set()
    row_count = len(rows)
    for ri in range(1, row_count):
        period_label = clean_text(grid.get((ri, 0), ""))
        if not period_label.isdigit():
            continue
        period = int(period_label)
        for col in range(1, 8):
            course = parse_course_cell(grid.get((ri, col), ""))
            if not course:
                continue
            key = (col, period, course["name"])
            if key in seen:
                continue  # same cell repeated through rowspan
            seen.add(key)
            start, end = PERIOD_TIMES.get(period, ("", ""))
            weekday_index = col - first_weekday_col
            if not 0 <= weekday_index < 7:
                continue
            entries.append(
                {
                    "weekday": weekday_names[weekday_index],
                    "weekday_index": weekday_index,
                    "period": period,
                    "time_start": start,
                    "time_end": end,
                    **course,
                }
            )
    entries.sort(key=lambda e: (e["weekday_index"], e["period"], e["name"]))
    return entries


def merge_periods(entries: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Collapse consecutive periods of the same course on the same weekday.

    The grid repeats a multi-period course on every row it spans; a single
    ``光纤通信原理`` entry becomes ``period_start=1, period_end=2`` with the
    matching clock times.
    """
    merged: list[dict[str, Any]] = []
    index: dict[tuple[int, str], dict[str, Any]] = {}
    for entry in entries:
        key = (entry["weekday_index"], entry["name"])
        current = index.get(key)
        if current is not None and entry["period"] == current["period_end"] + 1:
            current["period_end"] = entry["period"]
            current["time_end"] = entry["time_end"]
            continue
        item = dict(entry)
        item["period_start"] = entry["period"]
        item["period_end"] = entry["period"]
        index[key] = item
        merged.append(item)
    for item in merged:
        item["periods"] = list(range(item["period_start"], item["period_end"] + 1))
        item.pop("period", None)
    merged.sort(key=lambda e: (e["weekday_index"], e["period_start"], e["name"]))
    return merged


# ---------------------------------------------------------------------------
# Teaching calendar (the authoritative week -> date mapping)
# ---------------------------------------------------------------------------

_CAL_TABLE_RE = re.compile(r'<table[^>]*dgData[^>]*>(.*?)</table>', re.S | re.I)
_CN_WEEK = {"一": 1, "二": 2, "三": 3, "四": 4, "五": 5,
            "六": 6, "七": 7, "八": 8, "九": 9, "十": 10}


def _cn_number(text: str) -> Optional[int]:
    """一..二十一 → 1..21."""
    text = text.strip()
    if not text:
        return None
    if text == "十":
        return 10
    if text.startswith("十"):
        return 10 + _CN_WEEK.get(text[1:], 0)
    if text.endswith("十"):
        return _CN_WEEK.get(text[0], 0) * 10
    if "十" in text:
        head, _, tail = text.partition("十")
        return _CN_WEEK.get(head, 0) * 10 + _CN_WEEK.get(tail, 0)
    return _CN_WEEK.get(text)


def parse_calendar(html: str, academic_year_start: int) -> dict[int, list[str]]:
    """Teach week number -> seven ISO dates (Mon..Sun).

    ``academic_year_start`` disambiguates the month-only cells: a cell showing
    ``(8月)`` after the new year belongs to the previous calendar year.
    """
    table = _CAL_TABLE_RE.search(html)
    if not table:
        raise GradError("教学日历页面结构已变（找不到 dgData 表格）")

    weeks: dict[int, list[str]] = {}
    for row in _ROW_RE.findall(table.group(1)):
        cells = re.findall(r"<t[dh][^>]*>(.*?)</t[dh]>", row, re.S | re.I)
        if len(cells) < 8:
            continue
        head = _WS_RE.sub("", _TAG_RE.sub("", cells[0]))
        week_no = _cn_number(head)
        if week_no is None:
            continue

        dates: list[str] = []
        for cell in cells[1:8]:
            plain = _WS_RE.sub("", _TAG_RE.sub(" ", cell).replace("&nbsp;", " "))
            m = re.match(r"(\d{1,2})\((\d{1,2})月\)", plain)
            if not m:
                dates.append("")
                continue
            day, month = int(m.group(1)), int(m.group(2))
            # The academic year starts in autumn: Aug-Dec belong to the start
            # year, Jan-Jul to the year after. A single rule handles the
            # December->January week correctly; no extra month-wraparound
            # correction is needed (it used to add a second, wrong year).
            year = academic_year_start if month >= 8 else academic_year_start + 1
            dates.append(f"{year:04d}-{month:02d}-{day:02d}")
        if len(dates) == 7:
            weeks[week_no] = dates
    if not weeks:
        raise GradError("教学日历里没有解析出任何周次")
    return weeks


# A term label looks like 2026-2027秋学期 or 2026暑期学校. `&nbsp;` and other
# entities may appear between 学期 and the colon, so the label is matched on its
# own shape instead of relying on that prefix.
_TERM_RE = re.compile(r"\d{4}(?:-\d{4})?\s*[^\s<>（）()]{0,8}(?:学期|暑期学校)")


def term_from_html(html: str) -> str:
    """Extract `2026-2027秋学期` from a yjxt page, or "" when absent.

    The calendar page shows the label next to its table, but a page can also
    mention earlier terms (news items, links), so the match closest to the
    ``dgData`` table wins. Without a table, the last match is used: the current
    term is rendered nearest the content being viewed.
    """
    text = html or ""
    matches = list(_TERM_RE.finditer(text))
    if not matches:
        return ""
    anchor = text.find("dgData")
    if anchor > 0:
        best = min(matches, key=lambda m: abs(m.start() - anchor))
        return best.group(0).strip()
    return matches[-1].group(0).strip()


def academic_year_start(term_label: str) -> Optional[int]:
    """`2026-2027秋学期` -> 2026; None when the label carries no year.

    Returning None (rather than a guessed year) makes the caller report the
    problem instead of silently dating every event a year off.
    """
    match = re.search(r"(\d{4})", term_label or "")
    return int(match.group(1)) if match else None


# ---------------------------------------------------------------------------
# iCalendar export
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
    """RFC 5545 §3.1: lines longer than 75 octets are folded."""
    if len(line.encode("utf-8")) <= 75:
        return line
    out, current = [], b""
    for ch in line:
        raw = ch.encode("utf-8")
        if len(current) + len(raw) > 75:
            out.append(current.decode("utf-8"))
            current = b" "
        current += raw
    if current:
        out.append(current.decode("utf-8"))
    return "\r\n".join(out)


def build_ics(
    courses: list[dict[str, Any]],
    calendar: dict[int, list[str]],
    *,
    term: str,
    now: Optional[str] = None,
) -> str:
    """Build an iCalendar document with one VEVENT per class occurrence.

    Each merged course block is expanded into concrete dates: for every week
    listed on the course, the calendar supplies the real date for that
    weekday, and ``PERIOD_TIMES`` supplies the clock time.
    """
    import datetime as _dt

    stamp = now or _dt.datetime.now(_dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    lines = [
        "BEGIN:VCALENDAR",
        "VERSION:2.0",
        f"PRODID:-//bupt-cli//{_ics_escape(term)}//CN",
        "CALSCALE:GREGORIAN",
        "METHOD:PUBLISH",
        f"X-WR-CALNAME:{_ics_escape(term)} 课表",
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
    for course in courses:
        start_clock, _ = PERIOD_TIMES.get(course["period_start"], ("", ""))
        _, end_clock = PERIOD_TIMES.get(course["period_end"], ("", ""))
        if not start_clock or not end_clock:
            continue
        weekday = course["weekday_index"]
        summary = course["name"]
        location = " / ".join(course.get("rooms") or [])
        if course.get("campus"):
            location = f"[{course['campus']}] {location}".strip()
        description_parts = []
        if course.get("teachers"):
            description_parts.append("教师: " + ", ".join(course["teachers"]))
        if course.get("weeks"):
            description_parts.append("周次: " + _format_weeks(course["weeks"]))

        for week in course["weeks"]:
            dates = calendar.get(week)
            if not dates or not dates[weekday]:
                continue
            day = dates[weekday]
            lines.append("BEGIN:VEVENT")
            lines.append(f"UID:bupt-{day.replace('-', '')}-{weekday}-{course['period_start']}-{emitted}@bupt-cli")
            lines.append(f"DTSTAMP:{stamp}")
            lines.append(f"DTSTART;TZID=Asia/Shanghai:{day.replace('-', '')}T{start_clock.replace(':', '')}00")
            lines.append(f"DTEND;TZID=Asia/Shanghai:{day.replace('-', '')}T{end_clock.replace(':', '')}00")
            lines.append(_fold(f"SUMMARY:{_ics_escape(summary)}"))
            if location:
                lines.append(_fold(f"LOCATION:{_ics_escape(location)}"))
            if description_parts:
                lines.append(_fold("DESCRIPTION:" + _ics_escape("; ".join(description_parts))))
            lines.append("END:VEVENT")
            emitted += 1

    lines.append("END:VCALENDAR")
    return "\r\n".join(lines) + "\r\n"


def _format_weeks(weeks: list[int]) -> str:
    """[4,5,6,10,11] -> `4-6,10-11`."""
    if not weeks:
        return ""
    parts: list[str] = []
    start = prev = weeks[0]
    for w in weeks[1:]:
        if w == prev + 1:
            prev = w
            continue
        parts.append(str(start) if start == prev else f"{start}-{prev}")
        start = prev = w
    parts.append(str(start) if start == prev else f"{start}-{prev}")
    return ",".join(parts)


def parse_exams(html: str) -> dict[str, Any]:
    """Parse 学期考核信息查询. The header is always present; rows may be empty."""
    entries: list[dict[str, str]] = []
    header: list[str] = []
    for table in re.findall(r"<table[^>]*>(.*?)</table>", html, re.S | re.I):
        for row in _ROW_RE.findall(table):
            cells = [clean_text(c) for _, c in _CELL_RE.findall(row)]
            if not cells:
                continue
            if "考试课程" in cells[0] and not header:
                header = cells
                continue
            if header and len(cells) >= 6 and cells[0] != "没有信息数据":
                entries.append(
                    {
                        "course": cells[0],
                        "mode": cells[1] if len(cells) > 1 else "",
                        "date": cells[2] if len(cells) > 2 else "",
                        "time": cells[3] if len(cells) > 3 else "",
                        "room": cells[4] if len(cells) > 4 else "",
                        "note": cells[6] if len(cells) > 6 else "",
                    }
                )
    return {
        "columns": header,
        "count": len(entries),
        "exams": entries,
        "note": "" if entries else "本学期暂无考试安排（研究生课程多为论文/大作业考核）",
    }
