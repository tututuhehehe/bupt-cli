"""mobile_jwgl.py - BUPT mobile JWGL (jwglweixin.bupt.edu.cn/bjyddx) JSON services.

Unlike the DOM-scraped Qiangzhi JWGL in :mod:`bupt_cli._vendor.campus_api.bupt.schedule`, this
backend answers pure JSON: it logs in with the student's JWGL password (the
one for jwgl.bupt.edu.cn, NOT the CAS password) and guards subsequent calls
with a lowercase ``token`` header. Response envelope:
``{"code": "1", "Msg": ..., "data": ...}`` where code 1 (number or string)
means success.

Server-side permission boundary (verified 2026-08 with a live student
token): scores, score rankings, student records and teacher timetables
answer ``{"code": "0", "Msg": "未授权访问：<path>"}`` for this login realm.
Re-login cannot fix that, so those endpoints are intentionally not exposed.

The curriculum endpoint always describes the *current* week only; queries
for other weeks go through the DOM fallback in
:mod:`bupt_cli._vendor.campus_api.bupt.schedule`.
"""

import asyncio
import datetime as dt
import re
import time
from dataclasses import asdict, dataclass, field
from typing import Any, Optional

import httpx

from bupt_cli._vendor.campus_api.bupt.auth import decode_jwt_payload
from bupt_cli._vendor.campus_api.bupt.models import WEEKDAYS, CourseItem, ScheduleOverview
from bupt_cli._vendor.campus_api.exceptions import (
    CampusAuthError,
    CampusNetworkError,
    CampusParseError,
)

MOBILE_JWGL_BASE = "https://jwglweixin.bupt.edu.cn"
MOBILE_JWGL_LOGIN_URL = f"{MOBILE_JWGL_BASE}/bjyddx/login"
MOBILE_JWGL_HOME_URL = f"{MOBILE_JWGL_BASE}/sjd/#/new/Home_10013"
MOBILE_JWGL_UA = (
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 "
    "(KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"
)

_CURRICULUM_PATH = "/bjyddx/student/curriculum"
_SCHEDULE_MODES_PATH = "/bjyddx/Get_sjkbms"
_EXAM_WEEKS_PATH = "/bjyddx/kaowu/get_kszc"
_EXAM_SCHEDULE_PATH = "/bjyddx/kaowu/getKsapcx"
_EXAM_COURSE_SCHEDULE_PATH = "/bjyddx/kaowu/findKcksapcx"
_NOTICES_PATH = "/bjyddx/noticeList"
_TODAY_CLASSROOMS_PATH = "/bjyddx/todayClassrooms"
_TERM_GPA_PATH = "/bjyddx/student/termGPA"

CLASSROOM_CAMPUSES = {"01": "西土城路校区", "04": "沙河校区"}

# Messages that a fresh login can fix. The permanent permission boundary
# ("未授权访问：<path>") is deliberately not among them.
_TOKEN_EXPIRY_MARKERS = ("失效", "请登录", "重新登录", "过期", "token")
_TOKEN_REFRESH_BUFFER_SECONDS = 120.0


# ---------------------------------------------------------------------------
# Envelope and encoding helpers.
# ---------------------------------------------------------------------------


def _text(value: Any) -> str:
    """Coerce an optional JSON scalar to a trimmed string."""
    if value is None:
        return ""
    return value.strip() if isinstance(value, str) else str(value).strip()


def _code_is_success(payload: Any) -> bool:
    """True when the envelope reports success (code 1 as number or string)."""
    if not isinstance(payload, dict):
        return False
    return payload.get("code") in (1, "1")


def _envelope_message(payload: dict[str, Any]) -> str:
    """Best-effort human-readable message from an envelope."""
    message = payload.get("Msg", payload.get("msg"))
    return _text(message) or "(no message)"


def expand_week_numbers(week_text: str) -> list[int]:
    """Expand a week spec like ``"1-17单,2-18双"`` into explicit week numbers.

    A ``单``/``双`` marker keeps odd/even weeks only. A marker attached to a
    range applies to that range; ranges without their own marker inherit the
    first global marker of the text.
    """
    raw = week_text.replace("，", ",").replace(" ", "").replace("周", "")
    global_odd = "单" in raw
    global_even = "双" in raw
    raw = re.sub(r"\[.*?\]|\(.*?\)", "", raw)

    weeks: list[int] = []
    for item in raw.split(","):
        if not item:
            continue
        item_odd = "单" in item
        item_even = "双" in item
        clean = item.replace("单", "").replace("双", "")
        expanded: list[int] = []
        left, separator, right = clean.partition("-")
        try:
            if separator:
                expanded = list(range(int(left), int(right) + 1))
            else:
                expanded = [int(clean)]
        except ValueError:
            continue
        if item_odd or (not item_even and global_odd):
            expanded = [week for week in expanded if week % 2 == 1]
        elif item_even or (not item_odd and global_even):
            expanded = [week for week in expanded if week % 2 == 0]
        weeks.extend(expanded)
    return sorted(set(weeks))


def parse_course_week_numbers(
    class_week: str, class_week_details: str = ""
) -> list[int]:
    """Week numbers a course runs in: ``classWeek`` first, details fallback."""
    weeks = expand_week_numbers(class_week)
    if not weeks:
        weeks = expand_week_numbers(class_week_details)
    if not weeks:
        weeks = [int(match) for match in re.findall(r"\d+", class_week_details)]
    return sorted(set(weeks))


def parse_course_slots(class_time: str) -> Optional[tuple[int, int]]:
    """Decode a ``classTime`` code into 1-based inclusive (first, last) sections.

    The mobile backend encodes sections as ``<course-count><node><node>...``:
    ``"1030405"`` is one course spanning nodes 03, 04 and 05, so the leading
    count digit is skipped and the remaining digits form two-digit nodes.
    """
    digits = class_time.strip()
    tail = digits[1:]
    if not digits.isdigit() or len(tail) < 2 or len(tail) % 2 != 0:
        return None
    nodes = [int(tail[index : index + 2]) for index in range(0, len(tail), 2)]
    if any(node < 1 or node > 30 for node in nodes):
        return None
    return min(nodes), max(nodes)


def _mobile_token_expiry(token: str) -> float:
    """Epoch seconds when a login token stops being trusted.

    This backend issues JWTs with a *millisecond* ``exp`` (about 4 hours).
    Missing claims fall back to a conservative half hour.
    """
    exp = decode_jwt_payload(token).get("exp")
    if isinstance(exp, (int, float)):
        seconds = exp / 1000.0 if exp > 1e12 else float(exp)
        return seconds - _TOKEN_REFRESH_BUFFER_SECONDS
    return time.time() + 1800.0


# ---------------------------------------------------------------------------
# Response models.
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class SjdDay:
    """One calendar day of the curriculum week grid."""

    xqmc: str = ""  # Weekday name, e.g. "一"
    mxrq: str = ""  # Full date, e.g. "2026-08-24"
    zc: str = ""  # Week number, e.g. "2"
    xqid: str = ""  # Weekday id 1=Monday .. 7=Sunday
    rq: str = ""  # Day of month, e.g. "24"

    @classmethod
    def from_api(cls, data: dict[str, Any]) -> "SjdDay":
        return cls(
            xqmc=_text(data.get("xqmc")),
            mxrq=_text(data.get("mxrq")),
            zc=_text(data.get("zc")),
            xqid=_text(data.get("xqid")),
            rq=_text(data.get("rq")),
        )


@dataclass(frozen=True)
class SjdNode:
    """One class period (节次) definition."""

    node_name: str = ""  # e.g. "第一节"
    node_number: str = ""  # e.g. "01"

    @classmethod
    def from_api(cls, data: dict[str, Any]) -> "SjdNode":
        return cls(
            node_name=_text(data.get("nodeName")),
            node_number=_text(data.get("nodeNumber")),
        )


@dataclass(frozen=True)
class SjdCourse:
    """One course session as returned by the curriculum endpoint."""

    class_week: str = ""  # e.g. "3-18"
    class_week_details: str = ""  # e.g. ",3,4,5,"
    class_time: str = ""  # Section code, e.g. "1030405" for sections 3-5
    week_day: str = ""  # "1"=Monday .. "7"=Sunday
    course_name: str = ""
    teacher_name: str = ""
    building_name: str = ""
    classroom_name: str = ""
    start_time: str = ""  # e.g. "09:50"
    end_time: str = ""  # Backend typo "endTIme", alias "endTime"
    jx0408id: str = ""  # Stable course session id

    @classmethod
    def from_api(cls, data: dict[str, Any]) -> "SjdCourse":
        return cls(
            class_week=_text(data.get("classWeek")),
            class_week_details=_text(data.get("classWeekDetails")),
            class_time=_text(data.get("classTime")),
            week_day=_text(data.get("weekDay")),
            course_name=_text(data.get("courseName")),
            teacher_name=_text(data.get("teacherName")),
            building_name=_text(data.get("buildingName")),
            classroom_name=_text(data.get("classroomName")),
            start_time=_text(data.get("startTime")),
            end_time=_text(data.get("endTIme") or data.get("endTime")),
            jx0408id=_text(data.get("jx0408id")),
        )

    def slots(self) -> Optional[tuple[int, int]]:
        """1-based inclusive (first, last) sections, or None if undecodable."""
        return parse_course_slots(self.class_time)

    def week_numbers(self) -> list[int]:
        """Explicit week numbers the course runs in."""
        return parse_course_week_numbers(self.class_week, self.class_week_details)

    def location(self) -> str:
        """Human-readable place, e.g. "教一楼-101"."""
        if self.building_name and self.classroom_name:
            if self.classroom_name.startswith(self.building_name):
                return self.classroom_name
            return f"{self.building_name}-{self.classroom_name}"
        return self.building_name or self.classroom_name


@dataclass(frozen=True)
class SjdCurriculumWeek:
    """One week of the curriculum: calendar days, class nodes and courses."""

    date: list[SjdDay] = field(default_factory=list)
    courses: list[SjdCourse] = field(default_factory=list)
    nodes_lst: list[SjdNode] = field(default_factory=list)
    nodes: dict[str, list[str]] = field(default_factory=dict)  # sw/xw/ws section groups
    top_info: list[dict[str, Any]] = field(default_factory=list)
    semester_id: str = ""  # e.g. "2026-2027-1"
    item: Any = None  # Nested `[[[course]]]` grid used by some deployments
    week: str = ""
    weekday: str = ""
    remark: str = ""  # "bz": free-text course note shown under the grid

    @classmethod
    def from_api(cls, data: dict[str, Any]) -> "SjdCurriculumWeek":
        top_info = data.get("topInfo")
        if isinstance(top_info, dict):
            top_info = [top_info]
        return cls(
            date=[
                SjdDay.from_api(day)
                for day in data.get("date") or []
                if isinstance(day, dict)
            ],
            courses=[
                SjdCourse.from_api(course)
                for course in data.get("courses") or []
                if isinstance(course, dict)
            ],
            nodes_lst=[
                SjdNode.from_api(node)
                for node in data.get("nodesLst") or []
                if isinstance(node, dict)
            ],
            nodes={
                key: [str(number) for number in value]
                for key, value in (data.get("nodes") or {}).items()
                if isinstance(value, list)
            },
            top_info=[info for info in top_info or [] if isinstance(info, dict)],
            semester_id=_text(data.get("semesterId") or data.get("xnxq01id")),
            item=data.get("item"),
            week=_text(data.get("week")),
            weekday=_text(data.get("weekday")),
            remark=_text(data.get("bz")),
        )

    def all_courses(self) -> list[SjdCourse]:
        """Courses from both the flat array and the nested grid, deduplicated."""
        seen: set[tuple[str, str, str, str]] = set()
        found: list[SjdCourse] = []
        for course in [*self.courses, *self._item_courses()]:
            key = (
                course.jx0408id,
                course.course_name,
                course.class_time,
                course.week_day,
            )
            if key in seen:
                continue
            seen.add(key)
            found.append(course)
        return found

    def _item_courses(self) -> list[SjdCourse]:
        courses: list[SjdCourse] = []

        def collect(node: Any, depth: int = 0) -> None:
            if depth > 64:
                return
            if isinstance(node, list):
                for child in node:
                    collect(child, depth + 1)
            elif isinstance(node, dict):
                if "courseName" in node or "jx0408id" in node:
                    courses.append(SjdCourse.from_api(node))
                    return
                for child in node.values():
                    collect(child, depth + 1)

        collect(self.item)
        return courses

    def current_week(self) -> Optional[int]:
        """Week number the grid describes (grid days, then week, then topInfo)."""
        for day in self.date:
            if day.zc.isdigit():
                return int(day.zc)
        if self.week.isdigit():
            return int(self.week)
        for info in self.top_info:
            candidate = _text(info.get("week"))
            if candidate.isdigit():
                return int(candidate)
        return None

    def term_id(self) -> str:
        """Semester id like "2026-2027-1" (root, then topInfo)."""
        if self.semester_id:
            return self.semester_id
        for info in self.top_info:
            candidate = _text(info.get("semesterId") or info.get("xnxq01id"))
            if candidate:
                return candidate
        return ""

    def term_start_date(self) -> Optional[dt.date]:
        """Monday of week 1, inferred from a dated grid day and its week number."""
        week = self.current_week()
        if week is None or week < 0:
            return None
        dated = next((day for day in self.date if day.mxrq and day.zc != "all"), None)
        if dated is None:
            return None
        try:
            day = dt.datetime.strptime(dated.mxrq, "%Y-%m-%d").date()
        except ValueError:
            return None
        try:
            weekday = int(dated.xqid)
        except ValueError:
            weekday = 0
        if weekday == 0:
            weekday = 7
        if not 1 <= weekday <= 7:
            weekday = day.isoweekday()
        monday = day - dt.timedelta(days=weekday - 1)
        return monday - dt.timedelta(weeks=week - 1)


@dataclass(frozen=True)
class SjdKbMode:
    """Schedule time-mode entry (``Get_sjkbms``); ``mode_id`` feeds DOM queries."""

    is_default: str = ""  # "mrms": "1" for the default mode
    mode_id: str = ""  # "kbjcmsid"
    mode_name: str = ""  # "kbjcmsmc"

    @classmethod
    def from_api(cls, data: dict[str, Any]) -> "SjdKbMode":
        return cls(
            is_default=_text(data.get("mrms")),
            mode_id=_text(data.get("kbjcmsid")),
            mode_name=_text(data.get("kbjcmsmc")),
        )


@dataclass(frozen=True)
class SjdKeyedValue:
    """Generic ``{"key": ..., "value": ...}`` entry (e.g. exam week options)."""

    key: str = ""
    value: str = ""

    @classmethod
    def from_api(cls, data: dict[str, Any]) -> "SjdKeyedValue":
        return cls(key=_text(data.get("key")), value=_text(data.get("value")))


def _optional_float(value: Any) -> Optional[float]:
    """Parse a JSON scalar (number or numeric string) into an Optional float."""
    if value is None or isinstance(value, bool):
        return None
    if isinstance(value, (int, float)):
        return float(value)
    try:
        return float(str(value).strip())
    except ValueError:
        return None


@dataclass(frozen=True)
class SjdCourseScore:
    """One course grade row of the ``termGPA`` transcript report."""

    course_name: str = ""
    course_code: str = ""  # "kcbh"
    credit: Optional[float] = None
    curriculum_attributes: str = ""  # "curriculumAttributes": 必修/选修
    course_nature: str = ""  # "courseNature": e.g. 思想政治理论课
    examination_nature: str = ""  # "examinationNature": e.g. 正常考试
    fraction: str = ""  # 成绩, e.g. "85" (kept as string: may hold markers)
    score_flag: str = ""  # "cjbs": 成绩标识
    semester_name: str = ""  # "curSemesterName": e.g. 2025-2026-2
    score_id: str = ""  # "cj0708id": stable grade record id

    @classmethod
    def from_api(cls, data: dict[str, Any]) -> "SjdCourseScore":
        return cls(
            course_name=_text(data.get("courseName")),
            course_code=_text(data.get("kcbh")),
            credit=_optional_float(data.get("credit")),
            curriculum_attributes=_text(data.get("curriculumAttributes")),
            course_nature=_text(data.get("courseNature")),
            examination_nature=_text(data.get("examinationNature")),
            fraction=_text(data.get("fraction")),
            score_flag=_text(data.get("cjbs")),
            semester_name=_text(data.get("curSemesterName")),
            score_id=_text(data.get("cj0708id")),
        )


@dataclass(frozen=True)
class SjdScoreReport:
    """Transcript report from ``termGPA``: per-course grades plus GPA summary."""

    student_id: str = ""  # "studentID"
    average_score: Optional[float] = None  # "pjcj": 平均成绩
    term_gpas: list[dict[str, Any]] = field(
        default_factory=list
    )  # "xqgpa": raw entries (no observed schema yet)
    courses: list[SjdCourseScore] = field(default_factory=list)  # "achievement"

    @classmethod
    def from_api(cls, data: dict[str, Any]) -> "SjdScoreReport":
        return cls(
            student_id=_text(data.get("studentID")),
            average_score=_optional_float(data.get("pjcj")),
            term_gpas=[
                entry for entry in data.get("xqgpa") or [] if isinstance(entry, dict)
            ],
            courses=[
                SjdCourseScore.from_api(course)
                for course in data.get("achievement") or []
                if isinstance(course, dict)
            ],
        )

    def courses_in_semester(self, semester_name: str) -> list[SjdCourseScore]:
        """Grades of one semester, e.g. "2025-2026-2"."""
        return [
            course for course in self.courses if course.semester_name == semester_name
        ]


@dataclass(frozen=True)
class SjdClassroomEntry:
    """One class period of ``todayClassrooms``: comma-separated room(capacity)."""

    classrooms: str = ""

    @classmethod
    def from_api(cls, data: dict[str, Any]) -> "SjdClassroomEntry":
        return cls(classrooms=_text(data.get("CLASSROOMS") or data.get("classrooms")))

    def parse_classrooms(self) -> list[tuple[str, Optional[int]]]:
        """Parse ``room(capacity)`` pairs; rooms without a capacity yield None."""
        parsed: list[tuple[str, Optional[int]]] = []
        for room in self.classrooms.split(","):
            room = room.strip()
            if not room:
                continue
            name, separator, tail = room.rpartition("(")
            if separator and tail.endswith(")"):
                capacity = tail[:-1].strip()
                parsed.append(
                    (name.strip(), int(capacity) if capacity.isdigit() else None)
                )
            else:
                parsed.append((room, None))
        return parsed


# ---------------------------------------------------------------------------
# Normalization into the shared ScheduleOverview model.
# ---------------------------------------------------------------------------


def curriculum_to_overview(
    weeks: list[SjdCurriculumWeek],
    fallback_week: Optional[int] = None,
) -> ScheduleOverview:
    """Normalize the current-week mobile curriculum into a ScheduleOverview."""
    root = weeks[0] if weeks else None
    if root is None:
        raise CampusParseError("Mobile curriculum returned no week grid")

    courses: list[CourseItem] = []
    seen: set[tuple[int, str, int, int, str]] = set()
    for course in root.all_courses():
        slots = course.slots()
        if slots is None:
            continue
        try:
            day = int(course.week_day) - 1
        except ValueError:
            continue
        if not 0 <= day <= 6:
            continue
        first, last = slots
        item = CourseItem(
            name=course.course_name or "未命名课程",
            day=day,
            day_name=WEEKDAYS[day],
            section_start=first,
            section_span=last - first + 1,
            sections_display=f"第{first}节" if first == last else f"第{first}-{last}节",
            time_str=f"{course.start_time}-{course.end_time}"
            if course.start_time and course.end_time
            else course.class_time,
            weeks_display=course.class_week or course.class_week_details,
            location=course.location(),
            raw=asdict(course),
        )
        key = (
            item.day,
            item.name,
            item.section_start,
            item.section_span,
            item.location,
        )
        if key in seen:
            continue
        seen.add(key)
        courses.append(item)
    courses.sort(key=lambda course: (course.day, course.section_start))

    dates = [day.mxrq[-5:] for day in root.date][:7]
    dates += [""] * (7 - len(dates))
    week = root.current_week() or fallback_week or 1
    return ScheduleOverview(
        week=week,
        dates=dates,
        courses=courses,
        raw_html="",
        source="mobile",
    )


# ---------------------------------------------------------------------------
# Session managers.
# ---------------------------------------------------------------------------


class MobileJwglSessionManager:
    """Synchronous session manager holding the mobile JWGL token.

    The backend checks the JWGL password (the one for jwgl.bupt.edu.cn), not
    the CAS password. Login tokens are JWTs whose ``exp`` counts milliseconds.
    """

    def __init__(self, username: str = "", password: str = "", timeout: float = 15.0):
        self.username = username
        self.password = password
        self.timeout = timeout
        self._client: Optional[httpx.Client] = None
        self._token: Optional[str] = None
        self._token_expires_at = 0.0

    @property
    def client(self) -> httpx.Client:
        """Get or initialize the underlying httpx client."""
        if self._client is None or self._client.is_closed:
            self._client = httpx.Client(
                headers={
                    "User-Agent": MOBILE_JWGL_UA,
                    "Accept": "application/json, text/plain, */*",
                },
                timeout=self.timeout,
            )
            self._reset_token()
        return self._client

    def ensure_login(self, force: bool = False) -> httpx.Client:
        """Log in (or reuse the cached token) and return the authed client."""
        if self._token and not force and time.time() < self._token_expires_at:
            return self.client

        if not self.username or not self.password:
            raise CampusAuthError(
                "Mobile JWGL credentials not provided (jwgl_username/jwgl_password required)"
            )
        if force:
            self._reset_token()

        cli = self.client
        try:
            resp = cli.post(
                MOBILE_JWGL_LOGIN_URL,
                data={"userNo": self.username, "pwd": self.password},
            )
            resp.raise_for_status()
            try:
                payload = resp.json()
            except ValueError as e:
                raise CampusParseError(
                    f"Mobile JWGL login returned invalid JSON: {e}"
                ) from e
        except httpx.HTTPStatusError as e:
            raise CampusAuthError(
                f"Mobile JWGL login rejected (HTTP {e.response.status_code})"
            ) from e
        except httpx.HTTPError as e:
            raise CampusNetworkError(f"Mobile JWGL login network error: {e}") from e

        if not _code_is_success(payload):
            self._reset_token()
            raise CampusAuthError(
                f"Mobile JWGL login failed: {_envelope_message(payload)} "
                "(the mobile backend checks the JWGL password, not the CAS password)"
            )

        data = payload.get("data")
        token = _text(data.get("token")) if isinstance(data, dict) else ""
        if not token:
            raise CampusAuthError("Mobile JWGL login succeeded but returned no token")

        self._token = token
        self._token_expires_at = _mobile_token_expiry(token)
        cli.headers["Token"] = token
        cli.headers["token"] = token
        return cli

    def invalidate(self):
        """Drop the cached token so the next call logs in again."""
        self._reset_token()

    def close(self):
        """Close the underlying HTTP client session."""
        if self._client and not self._client.is_closed:
            self._client.close()
        self._client = None
        self._reset_token()

    def _reset_token(self):
        self._token = None
        self._token_expires_at = 0.0
        if self._client is not None:
            for header in ("Token", "token"):
                self._client.headers.pop(header, None)


class AsyncMobileJwglSessionManager:
    """Asynchronous twin of :class:`MobileJwglSessionManager`."""

    def __init__(self, username: str = "", password: str = "", timeout: float = 15.0):
        self.username = username
        self.password = password
        self.timeout = timeout
        self._client: Optional[httpx.AsyncClient] = None
        self._token: Optional[str] = None
        self._token_expires_at = 0.0
        self._login_lock = asyncio.Lock()

    @property
    def client(self) -> httpx.AsyncClient:
        """Get or initialize the underlying httpx async client."""
        if self._client is None or self._client.is_closed:
            self._client = httpx.AsyncClient(
                headers={
                    "User-Agent": MOBILE_JWGL_UA,
                    "Accept": "application/json, text/plain, */*",
                },
                timeout=self.timeout,
            )
            self._reset_token()
        return self._client

    async def ensure_login(self, force: bool = False) -> httpx.AsyncClient:
        """Log in (or reuse the cached token) and return the authed client."""
        if self._token and not force and time.time() < self._token_expires_at:
            return self.client

        async with self._login_lock:
            if self._token and not force and time.time() < self._token_expires_at:
                return self.client

            if not self.username or not self.password:
                raise CampusAuthError(
                    "Mobile JWGL credentials not provided "
                    "(jwgl_username/jwgl_password required)"
                )
            if force:
                self._reset_token()

            cli = self.client
            try:
                resp = await cli.post(
                    MOBILE_JWGL_LOGIN_URL,
                    data={"userNo": self.username, "pwd": self.password},
                )
                resp.raise_for_status()
                try:
                    payload = resp.json()
                except ValueError as e:
                    raise CampusParseError(
                        f"Mobile JWGL login returned invalid JSON: {e}"
                    ) from e
            except httpx.HTTPStatusError as e:
                raise CampusAuthError(
                    f"Mobile JWGL login rejected (HTTP {e.response.status_code})"
                ) from e
            except httpx.HTTPError as e:
                raise CampusNetworkError(f"Mobile JWGL login network error: {e}") from e

            if not _code_is_success(payload):
                self._reset_token()
                raise CampusAuthError(
                    f"Mobile JWGL login failed: {_envelope_message(payload)} "
                    "(the mobile backend checks the JWGL password, not the CAS password)"
                )

            data = payload.get("data")
            token = _text(data.get("token")) if isinstance(data, dict) else ""
            if not token:
                raise CampusAuthError(
                    "Mobile JWGL login succeeded but returned no token"
                )

            self._token = token
            self._token_expires_at = _mobile_token_expiry(token)
            cli.headers["Token"] = token
            cli.headers["token"] = token
            return cli

    def invalidate(self):
        """Drop the cached token so the next call logs in again."""
        self._reset_token()

    async def aclose(self):
        """Close the underlying async HTTP client session."""
        if self._client and not self._client.is_closed:
            await self._client.aclose()
        self._client = None
        self._reset_token()

    def _reset_token(self):
        self._token = None
        self._token_expires_at = 0.0
        if self._client is not None:
            for header in ("Token", "token"):
                self._client.headers.pop(header, None)


# ---------------------------------------------------------------------------
# Services.
# ---------------------------------------------------------------------------


def _raw_list(data: Any) -> list[dict[str, Any]]:
    """Coerce an envelope payload into a list of objects (or empty list)."""
    if isinstance(data, list):
        return [item for item in data if isinstance(item, dict)]
    if isinstance(data, dict):
        return [data]
    return []


class MobileJwglService:
    """Synchronous queries against the mobile JWGL JSON backend.

    Exam and notice payloads carry no stable schema outside semesters (the
    endpoints answer with empty data), so they surface as raw dictionaries
    until real fixtures exist.
    """

    def __init__(self, session_manager: MobileJwglSessionManager):
        self._session_mgr = session_manager

    def get_curriculum(self) -> list[SjdCurriculumWeek]:
        """Fetch the current week's curriculum grid."""
        data = self._request_json("GET", _CURRICULUM_PATH)
        weeks = data if isinstance(data, list) else []
        return [
            SjdCurriculumWeek.from_api(week) for week in weeks if isinstance(week, dict)
        ]

    def get_schedule_modes(self) -> list[SjdKbMode]:
        """Fetch available schedule time modes (节次模式)."""
        return [
            SjdKbMode.from_api(mode)
            for mode in _raw_list(self._request_json("GET", _SCHEDULE_MODES_PATH))
        ]

    def get_exam_weeks(self) -> list[SjdKeyedValue]:
        """Fetch exam week options (考试周次)."""
        return [
            SjdKeyedValue.from_api(week)
            for week in _raw_list(self._request_json("GET", _EXAM_WEEKS_PATH))
        ]

    def get_exam_schedule(self) -> list[dict[str, Any]]:
        """Fetch the exam schedule (raw dicts; empty between semesters)."""
        return _raw_list(self._request_json("GET", _EXAM_SCHEDULE_PATH))

    def get_exam_course_schedule(self) -> list[dict[str, Any]]:
        """Fetch exam course arrangements (raw dicts; empty between semesters)."""
        return _raw_list(self._request_json("GET", _EXAM_COURSE_SCHEDULE_PATH))

    def get_jwgl_notices(self) -> list[dict[str, Any]]:
        """Fetch JWGL notices (raw dicts; the endpoint may answer without data)."""
        return _raw_list(self._request_json("GET", _NOTICES_PATH))

    def get_score_report(self) -> SjdScoreReport:
        """Fetch the transcript report (``termGPA``): per-course grades and GPA.

        The report covers all recorded semesters; use
        :meth:`SjdScoreReport.courses_in_semester` to filter.
        """
        data = self._request_json("POST", _TERM_GPA_PATH)
        items = data if isinstance(data, list) else []
        reports = [
            SjdScoreReport.from_api(item) for item in items if isinstance(item, dict)
        ]
        if not reports:
            raise CampusParseError("Mobile JWGL termGPA returned no score report")
        return reports[0]

    def get_today_classrooms(
        self, campus_id: Optional[str] = None
    ) -> list[SjdClassroomEntry]:
        """Fetch real-time empty classrooms, one entry per class period.

        Args:
            campus_id (Optional[str]): "01" 西土城路校区, "04" 沙河校区.
                None uses the backend default.
        """
        params = {"campusId": campus_id} if campus_id else None
        return [
            SjdClassroomEntry.from_api(entry)
            for entry in _raw_list(
                self._request_json("GET", _TODAY_CLASSROOMS_PATH, params=params)
            )
        ]

    def _request_json(
        self,
        method: str,
        path: str,
        params: Optional[dict[str, str]] = None,
        retry_on_auth_fail: bool = True,
    ) -> Any:
        """Call an endpoint and return the envelope's ``data`` member."""
        cli = self._session_mgr.ensure_login()
        url = f"{MOBILE_JWGL_BASE}{path}"
        try:
            if method == "POST":
                resp = cli.post(url, params=params)
            else:
                resp = cli.get(url, params=params)
        except httpx.HTTPError as e:
            raise CampusNetworkError(f"Mobile JWGL {path} network error: {e}") from e

        # An empty or HTML body means the session was rejected server-side;
        # re-login once before giving up.
        body = resp.text.strip()
        if not body or body.startswith("<"):
            if retry_on_auth_fail:
                self._session_mgr.invalidate()
                return self._request_json(
                    method, path, params=params, retry_on_auth_fail=False
                )
            if not body:
                return None

        try:
            payload = resp.json()
        except ValueError as e:
            raise CampusParseError(
                f"Mobile JWGL {path} returned invalid JSON: {e}"
            ) from e

        if not _code_is_success(payload):
            message = _envelope_message(payload)
            if message.startswith("未授权访问"):
                raise CampusParseError(
                    f"Mobile JWGL {path} is outside the student token "
                    f"permission realm: {message}"
                )
            expired = resp.status_code == 401 or any(
                marker in message for marker in _TOKEN_EXPIRY_MARKERS
            )
            if retry_on_auth_fail and expired:
                self._session_mgr.invalidate()
                return self._request_json(
                    method, path, params=params, retry_on_auth_fail=False
                )
            raise CampusParseError(f"Mobile JWGL {path} failed: {message}")

        return payload.get("data")


class AsyncMobileJwglService:
    """Asynchronous twin of :class:`MobileJwglService`."""

    def __init__(self, session_manager: AsyncMobileJwglSessionManager):
        self._session_mgr = session_manager

    async def get_curriculum(self) -> list[SjdCurriculumWeek]:
        """Fetch the current week's curriculum grid asynchronously."""
        data = await self._request_json("GET", _CURRICULUM_PATH)
        weeks = data if isinstance(data, list) else []
        return [
            SjdCurriculumWeek.from_api(week) for week in weeks if isinstance(week, dict)
        ]

    async def get_schedule_modes(self) -> list[SjdKbMode]:
        """Fetch available schedule time modes asynchronously."""
        return [
            SjdKbMode.from_api(mode)
            for mode in _raw_list(await self._request_json("GET", _SCHEDULE_MODES_PATH))
        ]

    async def get_exam_weeks(self) -> list[SjdKeyedValue]:
        """Fetch exam week options asynchronously."""
        return [
            SjdKeyedValue.from_api(week)
            for week in _raw_list(await self._request_json("GET", _EXAM_WEEKS_PATH))
        ]

    async def get_exam_schedule(self) -> list[dict[str, Any]]:
        """Fetch the exam schedule asynchronously (raw dicts)."""
        return _raw_list(await self._request_json("GET", _EXAM_SCHEDULE_PATH))

    async def get_exam_course_schedule(self) -> list[dict[str, Any]]:
        """Fetch exam course arrangements asynchronously (raw dicts)."""
        return _raw_list(await self._request_json("GET", _EXAM_COURSE_SCHEDULE_PATH))

    async def get_jwgl_notices(self) -> list[dict[str, Any]]:
        """Fetch JWGL notices asynchronously (raw dicts)."""
        return _raw_list(await self._request_json("GET", _NOTICES_PATH))

    async def get_score_report(self) -> SjdScoreReport:
        """Fetch the transcript report (``termGPA``) asynchronously."""
        data = await self._request_json("POST", _TERM_GPA_PATH)
        items = data if isinstance(data, list) else []
        reports = [
            SjdScoreReport.from_api(item) for item in items if isinstance(item, dict)
        ]
        if not reports:
            raise CampusParseError("Mobile JWGL termGPA returned no score report")
        return reports[0]

    async def get_today_classrooms(
        self, campus_id: Optional[str] = None
    ) -> list[SjdClassroomEntry]:
        """Fetch real-time empty classrooms asynchronously."""
        params = {"campusId": campus_id} if campus_id else None
        return [
            SjdClassroomEntry.from_api(entry)
            for entry in _raw_list(
                await self._request_json("GET", _TODAY_CLASSROOMS_PATH, params=params)
            )
        ]

    async def _request_json(
        self,
        method: str,
        path: str,
        params: Optional[dict[str, str]] = None,
        retry_on_auth_fail: bool = True,
    ) -> Any:
        """Call an endpoint and return the envelope's ``data`` member."""
        cli = await self._session_mgr.ensure_login()
        url = f"{MOBILE_JWGL_BASE}{path}"
        try:
            if method == "POST":
                resp = await cli.post(url, params=params)
            else:
                resp = await cli.get(url, params=params)
        except httpx.HTTPError as e:
            raise CampusNetworkError(f"Mobile JWGL {path} network error: {e}") from e

        # An empty or HTML body means the session was rejected server-side;
        # re-login once before giving up.
        body = resp.text.strip()
        if not body or body.startswith("<"):
            if retry_on_auth_fail:
                self._session_mgr.invalidate()
                return await self._request_json(
                    method, path, params=params, retry_on_auth_fail=False
                )
            if not body:
                return None

        try:
            payload = resp.json()
        except ValueError as e:
            raise CampusParseError(
                f"Mobile JWGL {path} returned invalid JSON: {e}"
            ) from e

        if not _code_is_success(payload):
            message = _envelope_message(payload)
            if message.startswith("未授权访问"):
                raise CampusParseError(
                    f"Mobile JWGL {path} is outside the student token "
                    f"permission realm: {message}"
                )
            expired = resp.status_code == 401 or any(
                marker in message for marker in _TOKEN_EXPIRY_MARKERS
            )
            if retry_on_auth_fail and expired:
                self._session_mgr.invalidate()
                return await self._request_json(
                    method, path, params=params, retry_on_auth_fail=False
                )
            raise CampusParseError(f"Mobile JWGL {path} failed: {message}")

        return payload.get("data")
