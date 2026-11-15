"""Offline tests for the timetable cache.

The cache is what makes `bupt schedule --today` a file read instead of a
browser launch, so its date arithmetic is worth pinning down.
"""

import datetime as dt
import json

import pytest

from bupt_cli import timetable

ENTRY = {
    "version": timetable.CACHE_VERSION,
    "role": "graduate",
    "term": "2026-2027秋学期",
    "source": "yjxt",
    "fetched_at": "2026-11-15T00:00:00+00:00",
    "courses": [
        {
            "name": "光纤通信原理",
            "weekday_index": 2,
            "weeks": [4, 5],
            "period_start": 1,
            "period_end": 2,
            "time_start": "08:00",
            "time_end": "09:35",
            "teachers": ["王宏祥"],
            "rooms": ["3-105"],
            "campus": "西土城",
        }
    ],
    "calendar": {
        "4": ["2026-09-21", "2026-09-22", "2026-09-23", "2026-09-24",
              "2026-09-25", "2026-09-26", "2026-09-27"],
        "5": ["2026-09-28", "2026-09-29", "2026-09-30", "2026-10-01",
              "2026-10-02", "2026-10-03", "2026-10-04"],
    },
}


def test_occurrences_resolve_weekday_and_date():
    got = timetable.occurrences(
        ENTRY, dt.date(2026, 9, 20), dt.date(2026, 10, 5)
    )
    assert [s["date"] for s in got] == ["2026-09-23", "2026-09-30"]
    assert got[0]["time_start"] == "08:00"
    assert got[0]["week"] == 4
    assert got[0]["weekday"] == "星期三"


def test_occurrences_respect_the_range():
    got = timetable.occurrences(
        ENTRY, dt.date(2026, 9, 24), dt.date(2026, 9, 29)
    )
    assert got == []  # the only classes are on 09-23 and 09-30


def test_week_of_and_day_range():
    day = dt.date(2026, 9, 23)
    assert timetable.week_of(ENTRY, day) == 4
    monday, sunday = timetable.day_range(ENTRY, day)
    assert (monday, sunday) == (dt.date(2026, 9, 21), dt.date(2026, 9, 27))


def test_day_range_falls_back_outside_the_term():
    outside = dt.date(2027, 6, 1)  # a Tuesday
    monday, sunday = timetable.day_range(ENTRY, outside)
    assert monday == dt.date(2027, 5, 31)
    assert sunday == dt.date(2027, 6, 6)


def test_covers_and_is_stale():
    assert timetable.covers(ENTRY, dt.date(2026, 9, 23)) is True
    assert timetable.covers(ENTRY, dt.date(2027, 6, 1)) is False
    assert timetable.is_stale(ENTRY, today=dt.date(2026, 9, 23)) is False
    assert timetable.is_stale(ENTRY, today=dt.date(2027, 6, 1)) is True
    assert timetable.is_stale(None) is True


def test_save_and_load_round_trip(tmp_path):
    path = tmp_path / "timetable.json"
    timetable.save(ENTRY, path=path)
    assert path.stat().st_mode & 0o777 == 0o600

    loaded = timetable.load(path=path)
    assert loaded is not None
    assert loaded["term"] == "2026-2027秋学期"
    assert loaded["courses"][0]["name"] == "光纤通信原理"


def test_load_rejects_corrupt_or_wrong_version(tmp_path):
    path = tmp_path / "timetable.json"
    path.write_text("{not json", encoding="utf-8")
    assert timetable.load(path=path) is None

    payload = dict(ENTRY, version=999)
    path.write_text(json.dumps(payload), encoding="utf-8")
    assert timetable.load(path=path) is None

    path.write_text(json.dumps({"version": 1, "courses": "nope"}), encoding="utf-8")
    assert timetable.load(path=path) is None


def test_build_calendar_is_monday_anchored():
    calendar = timetable.build_calendar(dt.date(2026, 8, 31), 2)
    assert calendar["1"][0] == "2026-08-31"
    assert calendar["1"][-1] == "2026-09-06"
    assert calendar["2"][0] == "2026-09-07"
    assert len(calendar["2"]) == 7


def test_build_ics_emits_one_event_per_session():
    ics = timetable.build_ics(ENTRY)
    assert ics.count("BEGIN:VEVENT") == 2
    assert "DTSTART;TZID=Asia/Shanghai:20260923T080000" in ics
    assert "DTEND;TZID=Asia/Shanghai:20260923T093500" in ics
    assert "SUMMARY:光纤通信原理" in ics
    assert "第 4 周" in ics


def test_build_ics_is_crlf_and_folded():
    entry = dict(ENTRY)
    entry["courses"] = [dict(ENTRY["courses"][0], name="很长的课程名称" * 8)]
    ics = timetable.build_ics(entry)
    assert "\r\n" in ics
    for line in ics.split("\r\n"):
        assert len(line.encode("utf-8")) <= 76


def test_fetch_undergraduate_builds_dates_from_sjd_day():
    """The undergraduate path reads real dates off SjdDay, not arithmetic."""

    class Day:
        def __init__(self, zc, mxrq, xqid):
            self.zc, self.mxrq, self.xqid = zc, mxrq, xqid

    class Course:
        week_day = "3"  # Wednesday
        course_name = "数字信号处理"
        teacher_name = "张老师"
        building_name = "教三"
        classroom_name = "101"
        start_time = "08:00"
        end_time = "09:35"
        campus = ""

        def week_numbers(self):
            return [1]

    class Week:
        semester_id = "2026-2027-1"
        week = "1"
        courses = [Course()]
        date = [
            Day("1", "2026-08-31", "1"),
            Day("1", "2026-09-02", "3"),
            Day("1", "2026-09-06", "7"),
        ]

    class Client:
        def get_curriculum(self):
            return [Week()]

    entry = timetable.fetch_undergraduate(Client())
    assert entry["role"] == "undergraduate"
    assert entry["calendar"]["1"][0] == "2026-08-31"
    assert entry["calendar"]["1"][2] == "2026-09-02"
    assert entry["calendar"]["1"][6] == "2026-09-06"

    got = timetable.occurrences(
        entry, dt.date(2026, 8, 31), dt.date(2026, 9, 6)
    )
    assert len(got) == 1
    assert got[0]["date"] == "2026-09-02"
    assert got[0]["time_start"] == "08:00"


def test_fetch_undergraduate_without_dates_raises():
    class Week:
        semester_id = ""
        week = "1"
        courses = []
        date = []

    class Client:
        def get_curriculum(self):
            return [Week()]

    with pytest.raises(timetable.TimetableError):
        timetable.fetch_undergraduate(Client())


def test_occurrences_range_is_inclusive_on_both_ends():
    """Lock the closed-interval semantics: `build_ics` relies on it.

    `week_bounds` hands over the exact first and last date of the term, so an
    open end would silently drop the last day's classes from the export. Both
    mutations (left-open, right-open) used to leave the suite green.
    """
    only_day = dt.date(2026, 9, 23)  # the class on week 4
    assert len(timetable.occurrences(ENTRY, only_day, only_day)) == 1

    # Left endpoint inclusive: 09-23 .. 09-29 must keep 09-23.
    left = timetable.occurrences(ENTRY, dt.date(2026, 9, 23), dt.date(2026, 9, 29))
    assert [s["date"] for s in left] == ["2026-09-23"]

    # Right endpoint inclusive: 09-24 .. 09-30 must keep 09-30.
    right = timetable.occurrences(ENTRY, dt.date(2026, 9, 24), dt.date(2026, 9, 30))
    assert [s["date"] for s in right] == ["2026-09-30"]


def test_occurrences_ignores_out_of_range_weekdays():
    """A hand-edited cache must not crash or wrap a class onto Sunday."""
    entry = dict(ENTRY)
    entry["courses"] = [
        dict(ENTRY["courses"][0], weekday_index=-1),
        dict(ENTRY["courses"][0], weekday_index=7),
        dict(ENTRY["courses"][0], weekday_index=99),
    ]
    got = timetable.occurrences(
        entry, dt.date(2026, 9, 20), dt.date(2026, 10, 5)
    )
    assert got == []


def test_week_of_returns_none_outside_the_term():
    assert timetable.week_of(ENTRY, dt.date(2027, 6, 1)) is None
