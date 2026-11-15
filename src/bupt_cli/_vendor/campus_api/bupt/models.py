"""models.py - Data models for BUPT campus services."""

import html
import re
from dataclasses import dataclass, field
from datetime import date, datetime
from typing import Any, Optional


def html_to_text(value: str) -> str:
    """Strip HTML tags and unescape entities into plain text."""
    if not value:
        return ""
    clean = re.sub(r"<[^>]+>", " ", value)
    return html.unescape(clean).strip()


@dataclass(frozen=True)
class ElectricityInfo:
    """Represents dormitory electricity meter status and balance."""

    surplus: float
    price: float
    total_consumed_kwh: Optional[float]
    time: Optional[str]
    building: str
    floor: str
    room: str
    meter_number: Optional[str] = None
    raw: dict[str, Any] = field(default_factory=dict, repr=False)

    @property
    def v_total(self) -> Optional[float]:
        """Backward-compatible alias for total_consumed_kwh."""
        return self.total_consumed_kwh

    @property
    def remaining_kwh(self) -> float:
        """Estimated remaining electricity in kilowatt-hours (度)."""
        if self.price <= 0:
            return 0.0
        return round(self.surplus / self.price, 2)

    @property
    def parsed_time(self) -> Optional[datetime]:
        """Parsed datetime object of the meter reading timestamp, if available."""
        if not self.time:
            return None
        clean_time = self.time.split(".")[0].strip()
        try:
            return datetime.strptime(clean_time, "%Y-%m-%d %H:%M:%S")
        except ValueError:
            return None

    def is_low_balance(self, threshold: float = 10.0) -> bool:
        """Check if the remaining surplus balance is below the specified threshold."""
        return self.surplus < threshold

    @classmethod
    def from_api_response(
        cls,
        data: dict[str, Any],
        building: str,
        floor: str,
        room: str,
        meter_number: Optional[str] = None,
    ) -> "ElectricityInfo":
        """Construct an ElectricityInfo instance from raw upstream API dictionary."""

        def _to_float(val: Any) -> Optional[float]:
            if val is None or val == "":
                return None
            try:
                return float(val)
            except (ValueError, TypeError):
                return None

        surplus = _to_float(data.get("surplus")) or 0.0
        price = _to_float(data.get("price")) or 0.48
        total_consumed = _to_float(data.get("vTotal") or data.get("freeEnd"))
        time_str = data.get("time")

        return cls(
            surplus=surplus,
            price=price,
            total_consumed_kwh=total_consumed,
            time=time_str,
            building=data.get("parName") or building,
            floor=data.get("floorName") or floor,
            room=room,
            meter_number=meter_number,
            raw=data,
        )


@dataclass(frozen=True)
class CourseSite:
    """Represents an enrolled course site in UCloud."""

    site_id: str
    site_name: str
    teacher_name: str = ""
    raw: dict[str, Any] = field(default_factory=dict, repr=False)


@dataclass(frozen=True)
class HomeworkItem:
    """Represents an individual homework assignment in UCloud."""

    activity_id: str
    activity_name: str
    site_id: Optional[str]
    site_name: str
    end_time: Optional[str]
    begin_time: Optional[str] = None
    submit_time: Optional[str] = None
    is_submitted: bool = False
    score: Optional[float] = None
    raw: dict[str, Any] = field(default_factory=dict, repr=False)

    @property
    def parsed_end_time(self) -> Optional[datetime]:
        """Parsed datetime object for assignment deadline."""
        if not self.end_time:
            return None
        clean = self.end_time.split(".")[0].strip()
        for fmt in ("%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M", "%Y-%m-%d"):
            try:
                return datetime.strptime(clean, fmt)
            except ValueError:
                continue
        return None

    @property
    def is_overdue(self) -> bool:
        """Check if the deadline has passed."""
        parsed = self.parsed_end_time
        if not parsed:
            return False
        return datetime.now() > parsed

    @property
    def hours_remaining(self) -> Optional[float]:
        """Remaining hours before the deadline. Negative if overdue."""
        parsed = self.parsed_end_time
        if not parsed:
            return None
        delta = parsed - datetime.now()
        return round(delta.total_seconds() / 3600.0, 2)


@dataclass(frozen=True)
class HomeworkOverview:
    """Summary overview of pending (undone) assignments."""

    total_sites: int
    undone_count: int
    undone_list: list[HomeworkItem]
    raw: dict[str, Any] = field(default_factory=dict, repr=False)


@dataclass(frozen=True)
class HomeworkDetail:
    """Detailed information for a specific homework assignment."""

    activity_id: str
    title: str
    content_html: str
    attachments: list[dict[str, Any]] = field(default_factory=list)
    is_submitted: bool = False
    submit_content: Optional[str] = None
    submit_attachments: list[dict[str, Any]] = field(default_factory=list)
    score: Optional[float] = None
    teacher_comment: Optional[str] = None
    raw: dict[str, Any] = field(default_factory=dict, repr=False)

    @property
    def content_text(self) -> str:
        """Plaintext content with HTML tags stripped."""
        return html_to_text(self.content_html)


WEEKDAYS = ["周一", "周二", "周三", "周四", "周五", "周六", "周日"]


@dataclass(frozen=True)
class CourseItem:
    """Represents a scheduled course period in the educational timetable."""

    name: str
    day: int  # 0=Monday .. 6=Sunday
    day_name: str  # e.g. "周一"
    section_start: int  # 1-indexed start section (e.g. 1 for 1st section)
    section_span: int  # Number of sections (e.g. 2 for 2-period class)
    sections_display: str  # e.g. "第1-2节"
    time_str: str  # e.g. "第3-18周 1[01-02]节"
    credit: Optional[float] = None
    attr: str = ""  # e.g. "必修" / "选修"
    classes: str = ""  # e.g. "[2024211101]班"
    campus: str = ""  # e.g. "沙河校区"
    weeks_display: str = ""  # e.g. "3-18周"
    location: str = ""  # e.g. "教学实验综合楼-N108"
    raw: dict[str, Any] = field(default_factory=dict, repr=False)


@dataclass(frozen=True)
class ScheduleOverview:
    """Weekly course timetable overview."""

    week: int
    dates: list[str]  # 7 MM-DD strings for Mon..Sun
    courses: list[CourseItem]
    raw_html: str = field(default="", repr=False)
    source: str = "dom"  # "mobile" (JSON backend) or "dom" (DOM fallback)

    def get_day_courses(self, day: int) -> list[CourseItem]:
        """Get courses scheduled for a specific weekday (0=Monday .. 6=Sunday)."""
        matched = [c for c in self.courses if c.day == day]
        return sorted(matched, key=lambda c: c.section_start)

    def get_today_courses(self, target_date: Optional[date] = None) -> list[CourseItem]:
        """Get courses scheduled for today (or specified date)."""
        ref_date = target_date or date.today()
        day_index = ref_date.weekday()  # Monday is 0 and Sunday is 6
        return self.get_day_courses(day_index)


@dataclass(frozen=True)
class CampusCardBalance:
    """Campus card balances from the info portal homepage widget.

    ``main_wallet``/``subsidy`` map to the homepage's 主钱包余额/补助余额
    (upstream keys ``qkje``/``oddfare``); amounts are in CNY. ``raw`` keeps
    the full original payload — the remaining keys (``yjcs``/``cgtodo``/...)
    have no confirmed semantics yet.
    """

    main_wallet: Optional[float] = None
    subsidy: Optional[float] = None
    unread_mail: Optional[int] = None
    student_id: str = ""
    raw: dict[str, str] = field(default_factory=dict, repr=False)


@dataclass(frozen=True)
class NoticeItem:
    """Represents a campus notification or news item from the information portal."""

    title: str
    url: str
    author: str  # Publishing department / author
    time: str  # Date string, e.g. "2026-08-26"
    type: str  # Category key: "notice", "news", "rules", "ways"
    type_name: str = ""  # Chinese label: "通知公告", "校园新闻", etc.
    raw: dict[str, Any] = field(default_factory=dict, repr=False)

    @property
    def parsed_time(self) -> Optional[date]:
        """Parsed date object of the notice publication date."""
        if not self.time:
            return None
        clean = self.time.split(" ")[0].strip()
        try:
            return datetime.strptime(clean, "%Y-%m-%d").date()
        except ValueError:
            return None


@dataclass(frozen=True)
class ContentBlock:
    """One block of notice content flow: a text paragraph or an inline image."""

    kind: str  # "paragraph" or "image"
    text: str = ""  # Paragraph text (empty for images)
    url: str = ""  # Resolved image URL (empty for paragraphs)
    alt: str = ""  # Image alt attribute (empty for paragraphs)


@dataclass(frozen=True)
class NoticeDetail:
    """Detailed content and attachments of a portal notification."""

    title: str
    url: str
    author: str
    time: str
    content_html: str
    attachments: list[dict[str, str]] = field(
        default_factory=list
    )  # list of {"title": ..., "url": ...}
    raw_html: str = field(default="", repr=False)
    images: list[str] = field(
        default_factory=list
    )  # Resolved content image URLs, deduplicated in order
    blocks: list[ContentBlock] = field(
        default_factory=list
    )  # Paragraph/image flow of the content body

    @property
    def content_text(self) -> str:
        """Plaintext content with HTML tags stripped."""
        return html_to_text(self.content_html)
