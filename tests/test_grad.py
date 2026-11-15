"""Offline tests for the graduate-system parsers and the risky code paths.

No network, no browser: every HTML sample below is a trimmed copy of what
yjxt.bupt.edu.cn actually returned in 2026-11.
"""

import pytest

from bupt_cli import grad


# ---------------------------------------------------------------------------
# Timetable grid (rowspan expansion is the part that silently corrupts data)
# ---------------------------------------------------------------------------

_TIMETABLE = """
<table id="dgData">
<tr><th>节次</th><th>星期一</th><th>星期二</th><th>星期三</th>
    <th>星期四</th><th>星期五</th><th>星期六</th><th>星期日</th></tr>
<tr><td>1</td><td></td><td></td>
    <td rowspan="2">[西土城]光纤通信原理｛4周[教师:王宏祥,地点:3-105]｝</td>
    <td></td><td></td><td></td><td></td></tr>
<tr><td>2</td><td></td><td></td><td></td><td></td><td></td><td></td></tr>
<tr><td>3</td><td></td><td rowspan="2">[沙河]军事通信系统｛4-19周[教师:何元,地点:3-132]｝</td>
    <td></td><td></td><td></td><td></td><td></td></tr>
<tr><td>4</td><td></td><td></td><td></td><td></td><td></td><td></td></tr>
</table>
"""


def test_parse_timetable_expands_rowspan_into_weekdays():
    entries = grad.parse_timetable(_TIMETABLE)
    got = {(e["weekday"], e["period"], e["name"]) for e in entries}
    assert ("星期三", 1, "光纤通信原理") in got
    assert ("星期三", 2, "光纤通信原理") in got
    assert ("星期二", 3, "军事通信系统") in got
    assert ("星期二", 4, "军事通信系统") in got


def test_parse_timetable_sets_clock_times_from_period_table():
    entries = grad.parse_timetable(_TIMETABLE)
    first = next(e for e in entries if e["period"] == 1)
    assert (first["time_start"], first["time_end"]) == ("08:00", "08:45")


def test_merge_periods_collapses_consecutive_blocks():
    merged = grad.merge_periods(grad.parse_timetable(_TIMETABLE))
    light = next(m for m in merged if m["name"] == "光纤通信原理")
    assert light["period_start"] == 1
    assert light["period_end"] == 2
    assert light["periods"] == [1, 2]
    assert (light["time_start"], light["time_end"]) == ("08:00", "09:35")


def test_parse_course_cell_extracts_teachers_rooms_weeks():
    course = grad.parse_course_cell(
        "[西土城]光纤通信原理｛4周[教师:王宏祥,地点:3-105]、"
        "5-11周[教师:王宏祥,地点:3-211]｝"
    )
    assert course["campus"] == "西土城"
    assert course["name"] == "光纤通信原理"
    assert course["teachers"] == ["王宏祥"]
    assert course["rooms"] == ["3-105", "3-211"]
    assert course["weeks"] == [4] + list(range(5, 12))


def test_parse_course_cell_returns_none_for_blank():
    assert grad.parse_course_cell("   ") is None


# ---------------------------------------------------------------------------
# Teaching calendar: the week -> date mapping, including the year boundary
# ---------------------------------------------------------------------------

def _calendar_row(week_cn: str, cells: list[str]) -> str:
    tds = "".join(f"<td>{c}</td>" for c in cells)
    return f"<tr><td>{week_cn}</td>{tds}</tr>"


def test_parse_calendar_maps_week_one_to_real_dates():
    html = (
        '<table id="dgData">'
        + _calendar_row("一", [f"{d}&nbsp;(8月)" for d in range(31, 32)]
                        + [f"{d}&nbsp;(9月)" for d in range(1, 7)])
        + "</table>"
    )
    weeks = grad.parse_calendar(html, 2026)
    assert weeks[1] == [
        "2026-08-31", "2026-09-01", "2026-09-02", "2026-09-03",
        "2026-09-04", "2026-09-05", "2026-09-06",
    ]


def test_parse_calendar_handles_december_january_boundary():
    """Regression: the year used to jump to +2 for the 1 January cells."""
    html = (
        '<table id="dgData">'
        + _calendar_row(
            "十八",
            ["29&nbsp;(12月)", "30&nbsp;(12月)", "31&nbsp;(12月)",
             "1&nbsp;(1月)", "2&nbsp;(1月)", "3&nbsp;(1月)", "4&nbsp;(1月)"],
        )
        + "</table>"
    )
    weeks = grad.parse_calendar(html, 2026)
    assert weeks[18] == [
        "2026-12-29", "2026-12-30", "2026-12-31",
        "2027-01-01", "2027-01-02", "2027-01-03", "2027-01-04",
    ]


def test_parse_calendar_parses_chinese_week_numbers_up_to_twenty():
    rows = []
    for cn, num in [("一", 1), ("十", 10), ("十一", 11), ("十九", 19), ("二十", 20)]:
        rows.append(_calendar_row(cn, [f"{num}&nbsp;(9月)"] * 7))
    html = '<table id="dgData">' + "".join(rows) + "</table>"
    weeks = grad.parse_calendar(html, 2026)
    assert sorted(weeks) == [1, 10, 11, 19, 20]


def test_parse_calendar_rejects_missing_table():
    with pytest.raises(grad.GradError):
        grad.parse_calendar("<html><body>nothing</body></html>", 2026)


def test_term_from_html_and_academic_year_start():
    html = '<td class="tb_line"> 学期：2027-2028春学期 </td>'
    term = grad.term_from_html(html)
    assert term == "2027-2028春学期"
    assert grad.academic_year_start(term) == 2027


def test_term_from_html_returns_empty_when_absent():
    assert grad.term_from_html("<html></html>") == ""


# ---------------------------------------------------------------------------
# iCalendar export
# ---------------------------------------------------------------------------

def test_build_ics_emits_one_event_per_week_and_period():
    course = {
        "weekday_index": 2,
        "name": "光纤通信原理",
        "campus": "西土城",
        "teachers": ["王宏祥"],
        "rooms": ["3-105"],
        "weeks": [4, 5],
        "period_start": 1,
        "period_end": 2,
    }
    calendar = {4: ["2026-09-21"] * 7, 5: ["2026-09-28"] * 7}
    ics = grad.build_ics([course], calendar, term="2026-2027秋学期")
    assert ics.count("BEGIN:VEVENT") == 2
    assert "DTSTART;TZID=Asia/Shanghai:20260921T080000" in ics
    assert "DTEND;TZID=Asia/Shanghai:20260921T093500" in ics
    assert "SUMMARY:光纤通信原理" in ics


def test_build_ics_skips_weeks_absent_from_the_calendar():
    course = {
        "weekday_index": 0,
        "name": "X",
        "campus": "",
        "teachers": [],
        "rooms": [],
        "weeks": [4, 99],
        "period_start": 1,
        "period_end": 1,
    }
    ics = grad.build_ics([course], {4: ["2026-09-21"] * 7}, term="t")
    assert ics.count("BEGIN:VEVENT") == 1


def test_build_ics_is_crlf_and_folded():
    course = {
        "weekday_index": 0,
        "name": "很长的课程名称" * 8,
        "campus": "",
        "teachers": [],
        "rooms": [],
        "weeks": [1],
        "period_start": 1,
        "period_end": 1,
    }
    ics = grad.build_ics([course], {1: ["2026-08-31"] * 7}, term="t")
    assert "\r\n" in ics
    for line in ics.split("\r\n"):
        assert len(line.encode("utf-8")) <= 76


# ---------------------------------------------------------------------------
# Exam table
# ---------------------------------------------------------------------------

def test_parse_exams_returns_columns_even_when_empty():
    html = (
        "<table><tr><td>考试课程</td><td>考核方式</td><td>考核日期</td>"
        "<td>考试时间</td><td>考试地点</td><td>考场人数</td><td>备注</td></tr>"
        "<tr><td>没有信息数据</td></tr></table>"
    )
    result = grad.parse_exams(html)
    assert result["count"] == 0
    assert "考试课程" in result["columns"]
    assert result["exams"] == []


def test_parse_exams_parses_rows():
    html = (
        "<table>"
        "<tr><td>考试课程</td><td>考核方式</td><td>考核日期</td><td>考试时间</td>"
        "<td>考试地点</td><td>考场人数</td><td>备注</td></tr>"
        "<tr><td>通信网理论</td><td>闭卷</td><td>2026-01-05</td><td>09:00-11:00</td>"
        "<td>3-101</td><td>40</td><td>带计算器</td></tr>"
        "</table>"
    )
    result = grad.parse_exams(html)
    assert result["count"] == 1
    assert result["exams"][0]["course"] == "通信网理论"
    assert result["exams"][0]["room"] == "3-101"
