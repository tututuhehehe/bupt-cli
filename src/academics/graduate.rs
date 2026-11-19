//! Parsers and the page-source boundary for the graduate system (yjxt).
//!
//! This module never performs network or browser I/O. It turns already-fetched
//! HTML into the same JSON-ready shapes as the Python reference, and exposes a
//! shallow `GradPageSource` seam so command tests can inject synthetic pages.
//! A concrete Chrome/CDP source lives in `graduate::browser` and is selected by
//! the production command wrappers only.

use crate::cli::{CliError, CliResult};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};

mod browser;

pub(super) use browser::ChromePageSource;

pub(super) const WEEKDAYS: [&str; 7] = [
    "星期一",
    "星期二",
    "星期三",
    "星期四",
    "星期五",
    "星期六",
    "星期日",
];

// Official 45-minute periods shown by yjxt. Unknown periods keep empty clocks
// so a layout change fails complete-export validation instead of inventing one.
const PERIOD_TIMES: [(&str, &str); 15] = [
    ("08:00", "08:45"),
    ("08:50", "09:35"),
    ("09:50", "10:35"),
    ("10:40", "11:25"),
    ("11:30", "12:15"),
    ("13:00", "13:45"),
    ("13:50", "14:35"),
    ("14:45", "15:30"),
    ("15:40", "16:25"),
    ("16:35", "17:20"),
    ("17:25", "18:10"),
    ("18:30", "19:15"),
    ("19:20", "20:05"),
    ("20:10", "20:55"),
    ("21:00", "21:45"),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum GradPage {
    Schedule,
    Calendar,
    Exams,
}

#[derive(Clone, Debug, Default)]
pub(super) struct GradPages {
    pub(super) schedule: Option<String>,
    pub(super) calendar: Option<String>,
    pub(super) exams: Option<String>,
}

/// Shallow seam: tests provide synthetic pages; production uses Chrome/CDP.
pub(super) trait GradPageSource {
    fn collect(&mut self, wanted: &[GradPage]) -> CliResult<GradPages>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Course {
    pub(super) name: String,
    pub(super) campus: String,
    pub(super) weeks: Vec<i64>,
    pub(super) teachers: Vec<String>,
    pub(super) rooms: Vec<String>,
    pub(super) weekday_index: i64,
    pub(super) period_start: i64,
    pub(super) period_end: i64,
    pub(super) time_start: String,
    pub(super) time_end: String,
}

fn upstream(message: impl Into<String>, hint: impl Into<String>) -> CliError {
    CliError::new(5, "upstream", message, hint)
}

fn timetable_error(message: impl Into<String>) -> CliError {
    upstream(
        message,
        "页面结构可能已变；重试一次，仍失败时反馈课表页面结构。",
    )
}

fn calendar_error(message: impl Into<String>) -> CliError {
    upstream(message, "页面结构可能已变；不会用周次猜日期。")
}

fn exam_error(message: impl Into<String>) -> CliError {
    upstream(message, "页面结构可能已变；不会把布局漂移当成“没有考试”。")
}

// ---------------------------------------------------------------------------
// Minimal HTML reading (no regex crate, no HTML-parser dependency).
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Element {
    start: usize,
    attrs: String,
    inner: String,
}

fn tag_elements(html: &str, tag: &str) -> Vec<Element> {
    let lower = html.to_ascii_lowercase();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut elements = Vec::new();
    let mut position = 0;
    while let Some(relative) = lower[position..].find(&open) {
        let start = position + relative;
        let after_name = start + open.len();
        if !lower[after_name..]
            .chars()
            .next()
            .is_some_and(|c| c == '>' || c.is_whitespace() || c == '/')
        {
            position = after_name;
            continue;
        }
        let Some(gt_relative) = lower[after_name..].find('>') else {
            break;
        };
        let gt = after_name + gt_relative;
        let Some(close_relative) = lower[gt + 1..].find(&close) else {
            break;
        };
        let inner_start = gt + 1;
        let inner_end = inner_start + close_relative;
        elements.push(Element {
            start,
            attrs: html[after_name..gt].to_string(),
            inner: html[inner_start..inner_end].to_string(),
        });
        position = inner_end + close.len();
    }
    elements
}

fn row_elements(html: &str) -> Vec<Element> {
    tag_elements(html, "tr")
}

fn cell_elements(html: &str) -> Vec<Element> {
    let mut cells = tag_elements(html, "td");
    cells.extend(tag_elements(html, "th"));
    cells.sort_by_key(|cell| cell.start);
    cells
}

fn attribute(attrs: &str, name: &str) -> Option<String> {
    let lower = attrs.to_ascii_lowercase();
    let mut position = 0;
    while let Some(relative) = lower[position..].find(name) {
        let start = position + relative;
        let after = start + name.len();
        if let Some(before) = lower[..start].chars().next_back()
            && !before.is_whitespace()
        {
            position = after;
            continue;
        }
        let rest = &attrs[after..];
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            position = after;
            continue;
        };
        let rest = rest.trim_start();
        let quote = rest.chars().next()?;
        if quote != '"' && quote != '\'' {
            return Some(rest.split_whitespace().next().unwrap_or("").to_string());
        }
        let tail = &rest[quote.len_utf8()..];
        return tail.find(quote).map(|end| tail[..end].to_string());
    }
    None
}

fn strip_tags(value: &str) -> String {
    let mut output = String::new();
    let mut in_tag = false;
    for character in value.chars() {
        match character {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => output.push(character),
            _ => {}
        }
    }
    output
}

fn decode_entities(value: &str) -> String {
    value
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
}

fn clean_text(value: &str) -> String {
    decode_entities(&strip_tags(value))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn compact_text(value: &str) -> String {
    clean_text(value)
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn rowspan(attrs: &str) -> usize {
    attribute(attrs, "rowspan")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|span| *span > 0)
        .unwrap_or(1)
        .min(100)
}

// ---------------------------------------------------------------------------
// Timetable grid.
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct GridCourse {
    weekday_index: i64,
    period: i64,
    time_start: String,
    time_end: String,
    course: Course,
}

pub(super) fn parse_timetable(html: &str) -> CliResult<Vec<Course>> {
    let table = tag_elements(html, "table")
        .into_iter()
        .find(|table| table.inner.contains("星期一"))
        .ok_or_else(|| timetable_error("课表页面里找不到星期表头（页面结构可能已变）"))?;
    let rows = row_elements(&table.inner);
    let header_cells = rows
        .first()
        .map(|row| cell_elements(&row.inner))
        .unwrap_or_default();
    let header_text = header_cells
        .iter()
        .map(|cell| clean_text(&cell.inner))
        .collect::<Vec<_>>();
    let first_weekday_column = header_text
        .iter()
        .position(|text| WEEKDAYS.contains(&text.as_str()))
        .unwrap_or(1);
    let last_weekday_column = first_weekday_column + 6;
    let mut grid: BTreeMap<(usize, usize), String> = BTreeMap::new();
    for (row_index, row) in rows.iter().enumerate().skip(1) {
        let mut column = 0;
        for cell in cell_elements(&row.inner) {
            while grid.contains_key(&(row_index, column)) {
                column += 1;
            }
            if column > last_weekday_column {
                break;
            }
            let span = rowspan(&cell.attrs);
            for offset in 0..span {
                grid.insert((row_index + offset, column), cell.inner.clone());
            }
            column += 1;
        }
    }

    let mut raw = Vec::new();
    let mut seen = HashSet::new();
    for row_index in 1..rows.len() {
        let period_label = grid
            .get(&(row_index, 0))
            .map(|cell| clean_text(cell))
            .unwrap_or_default();
        if period_label.is_empty() || !period_label.as_bytes().iter().all(u8::is_ascii_digit) {
            continue;
        }
        let Ok(period) = period_label.parse::<i64>() else {
            continue;
        };
        for weekday_index in 0..7 {
            let column = first_weekday_column + weekday_index;
            let Some(raw_cell) = grid.get(&(row_index, column)) else {
                continue;
            };
            let Some(course) = parse_course_cell(raw_cell) else {
                continue;
            };
            if !seen.insert((weekday_index, period, course.name.clone())) {
                continue;
            }
            let (time_start, time_end) = period_times(period);
            raw.push(GridCourse {
                weekday_index: weekday_index as i64,
                period,
                time_start: time_start.to_string(),
                time_end: time_end.to_string(),
                course,
            });
        }
    }

    raw.sort_by(|left, right| {
        (left.weekday_index, left.period, left.course.name.as_str()).cmp(&(
            right.weekday_index,
            right.period,
            right.course.name.as_str(),
        ))
    });
    Ok(merge_periods(raw))
}

fn period_times(period: i64) -> (&'static str, &'static str) {
    let index = usize::try_from(period - 1).ok();
    index
        .and_then(|index| PERIOD_TIMES.get(index).copied())
        .unwrap_or(("", ""))
}

fn merge_periods(raw: Vec<GridCourse>) -> Vec<Course> {
    let mut merged: Vec<Course> = Vec::new();
    let mut positions: BTreeMap<(i64, String), usize> = BTreeMap::new();
    for entry in raw {
        let key = (entry.weekday_index, entry.course.name.clone());
        if let Some(&position) = positions.get(&key)
            && entry.period == merged[position].period_end + 1
        {
            merged[position].period_end = entry.period;
            merged[position].time_end = entry.time_end;
            continue;
        }
        let mut course = entry.course;
        course.weekday_index = entry.weekday_index;
        course.period_start = entry.period;
        course.period_end = entry.period;
        course.time_start = entry.time_start;
        course.time_end = entry.time_end;
        // `weekday_names` is only used as a sanity check for header order; the
        // stable public field is the zero-based weekday index.
        positions.insert(key, merged.len());
        merged.push(course);
    }
    merged.sort_by(|left, right| {
        (left.weekday_index, left.period_start, left.name.as_str()).cmp(&(
            right.weekday_index,
            right.period_start,
            right.name.as_str(),
        ))
    });
    merged
}

fn parse_course_cell(value: &str) -> Option<Course> {
    let text = clean_text(value);
    if text.is_empty() {
        return None;
    }
    let Some(open_campus) = text.find('[') else {
        return Some(unrecognized_course(&text));
    };
    if open_campus != 0 {
        return Some(unrecognized_course(&text));
    }
    let Some(close_campus_relative) = text[open_campus + 1..].find(']') else {
        return Some(unrecognized_course(&text));
    };
    let close_campus = open_campus + 1 + close_campus_relative;
    let campus = text[open_campus + 1..close_campus].trim().to_string();
    let after_campus = &text[close_campus + 1..];
    let Some(open_body_relative) = after_campus.find(['｛', '{']) else {
        return Some(unrecognized_course(&text));
    };
    let open_body = close_campus + 1 + open_body_relative;
    let open_body_len = text[open_body..].chars().next()?.len_utf8();
    let name = text[close_campus + 1..open_body].trim().to_string();
    if name.is_empty() {
        return Some(unrecognized_course(&text));
    }
    let body_start = open_body + open_body_len;
    let Some(close_body_relative) = text[body_start..].rfind(['｝', '}']) else {
        return Some(unrecognized_course(&text));
    };
    let close_body = body_start + close_body_relative;
    let body = &text[body_start..close_body];
    let (weeks, teachers, rooms) = course_details(body);
    Some(Course {
        name,
        campus,
        weeks,
        teachers,
        rooms,
        weekday_index: 0,
        period_start: 0,
        period_end: 0,
        time_start: String::new(),
        time_end: String::new(),
    })
}

fn unrecognized_course(text: &str) -> Course {
    Course {
        name: text.to_string(),
        campus: String::new(),
        weeks: Vec::new(),
        teachers: Vec::new(),
        rooms: Vec::new(),
        weekday_index: 0,
        period_start: 0,
        period_end: 0,
        time_start: String::new(),
        time_end: String::new(),
    }
}

fn course_details(body: &str) -> (Vec<i64>, Vec<String>, Vec<String>) {
    let characters = body.chars().collect::<Vec<_>>();
    let mut weeks = Vec::new();
    let mut teachers = Vec::new();
    let mut rooms = Vec::new();
    let mut index = 0;
    while index < characters.len() {
        if !(characters[index].is_ascii_digit()
            || characters[index] == ','
            || characters[index] == '-')
        {
            index += 1;
            continue;
        }
        let start = index;
        while index < characters.len()
            && (characters[index].is_ascii_digit()
                || characters[index] == ','
                || characters[index] == '-')
        {
            index += 1;
        }
        if characters.get(index) != Some(&'周') {
            continue;
        }
        let spec = characters[start..index].iter().collect::<String>();
        weeks.extend(expand_weeks(&spec));
        index += 1;
        while characters.get(index).is_some_and(|c| c.is_whitespace()) {
            index += 1;
        }
        if characters.get(index) != Some(&'[') {
            continue;
        }
        let detail_start = index + 1;
        let Some(detail_end_relative) = characters[detail_start..]
            .iter()
            .position(|character| *character == ']')
        else {
            break;
        };
        let detail_end = detail_start + detail_end_relative;
        let detail = characters[detail_start..detail_end]
            .iter()
            .collect::<String>();
        if let Some(teacher) = field_after(&detail, &["教师:", "教师："], true) {
            for person in teacher.split(',') {
                let person = person.trim();
                if !person.is_empty() && !teachers.iter().any(|known| known == person) {
                    teachers.push(person.to_string());
                }
            }
        }
        if let Some(room) = field_after(&detail, &["地点:", "地点："], false) {
            let room = room.trim();
            if !room.is_empty() && !rooms.iter().any(|known| known == room) {
                rooms.push(room.to_string());
            }
        }
        index = detail_end + 1;
    }
    weeks.sort_unstable();
    weeks.dedup();
    (weeks, teachers, rooms)
}

fn field_after(text: &str, markers: &[&str], until_room: bool) -> Option<String> {
    let start = markers.iter().find_map(|marker| text.find(marker))?;
    let marker = markers
        .iter()
        .find(|marker| text[start..].starts_with(**marker))?;
    let after = start + marker.len();
    let tail = &text[after..];
    let boundary = if until_room {
        tail.find("地点:").or_else(|| tail.find("地点："))
    } else {
        tail.find(',')
    };
    Some(tail[..boundary.unwrap_or(tail.len())].trim().to_string())
}

fn expand_weeks(spec: &str) -> Vec<i64> {
    let mut weeks = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((low, high)) = part.split_once('-') {
            let Ok(low) = low.parse::<i64>() else {
                continue;
            };
            let Ok(high) = high.parse::<i64>() else {
                continue;
            };
            for week in low.max(1)..=high.min(30) {
                weeks.push(week);
            }
        } else if let Ok(week) = part.parse::<i64>()
            && (1..=30).contains(&week)
        {
            weeks.push(week);
        }
    }
    weeks
}

// ---------------------------------------------------------------------------
// Teaching calendar and term label.
// ---------------------------------------------------------------------------

pub(super) fn parse_calendar(
    html: &str,
    academic_year_start: i32,
) -> CliResult<BTreeMap<i64, Vec<String>>> {
    let table = tag_elements(html, "table")
        .into_iter()
        .find(|table| table.attrs.to_ascii_lowercase().contains("dgdata"))
        .ok_or_else(|| calendar_error("教学日历页面结构已变（找不到 dgData 表格）"))?;
    let mut weeks = BTreeMap::new();
    for row in row_elements(&table.inner) {
        let cells = cell_elements(&row.inner);
        if cells.len() < 8 {
            continue;
        }
        let head = compact_text(&cells[0].inner);
        let Some(week) = chinese_week_number(&head) else {
            continue;
        };
        let dates = cells[1..8]
            .iter()
            .map(|cell| calendar_date(&compact_text(&cell.inner), academic_year_start))
            .collect::<Vec<_>>();
        weeks.insert(week, dates);
    }
    if weeks.is_empty() {
        return Err(calendar_error("教学日历里没有解析出任何周次"));
    }
    Ok(weeks)
}

fn calendar_date(text: &str, academic_year_start: i32) -> String {
    let characters = text.chars().collect::<Vec<_>>();
    let mut index = 0;
    let Some(day) = take_digits(&characters, &mut index, 2) else {
        return String::new();
    };
    if !characters
        .get(index)
        .is_some_and(|character| *character == '(' || *character == '（')
    {
        return String::new();
    }
    index += 1;
    let Some(month) = take_digits(&characters, &mut index, 2) else {
        return String::new();
    };
    if characters.get(index) != Some(&'月') {
        return String::new();
    }
    index += 1;
    if !characters
        .get(index)
        .is_some_and(|character| *character == ')' || *character == '）')
    {
        return String::new();
    }
    let year = if month >= 8 {
        academic_year_start
    } else {
        academic_year_start + 1
    };
    format!("{year:04}-{month:02}-{day:02}")
}

fn take_digits(characters: &[char], index: &mut usize, max: usize) -> Option<i32> {
    let start = *index;
    while *index < characters.len() && *index - start < max && characters[*index].is_ascii_digit() {
        *index += 1;
    }
    if *index == start {
        return None;
    }
    characters[start..*index]
        .iter()
        .collect::<String>()
        .parse()
        .ok()
}

fn chinese_week_number(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let digit = |character: char| -> Option<i64> {
        match character {
            '一' => Some(1),
            '二' => Some(2),
            '三' => Some(3),
            '四' => Some(4),
            '五' => Some(5),
            '六' => Some(6),
            '七' => Some(7),
            '八' => Some(8),
            '九' => Some(9),
            '十' => Some(10),
            _ => None,
        }
    };
    if text == "十" {
        return Some(10);
    }
    if let Some(rest) = text.strip_prefix('十') {
        return digit(rest.chars().next()?).map(|value| 10 + value);
    }
    if let Some(rest) = text.strip_suffix('十') {
        return digit(rest.chars().next()?).map(|value| value * 10);
    }
    if let Some((head, tail)) = text.split_once('十') {
        return Some(digit(head.chars().next()?)? * 10 + digit(tail.chars().next()?)?);
    }
    digit(text.chars().next()?)
}

pub(super) fn term_from_html(html: &str) -> Option<String> {
    let characters = html.chars().collect::<Vec<_>>();
    let mut matches: Vec<(usize, String)> = Vec::new();
    let mut index = 0;
    while index + 4 <= characters.len() {
        if !characters[index..index + 4]
            .iter()
            .all(char::is_ascii_digit)
        {
            index += 1;
            continue;
        }
        let mut end = index + 4;
        if end + 5 <= characters.len()
            && characters[end] == '-'
            && characters[end + 1..end + 5]
                .iter()
                .all(char::is_ascii_digit)
        {
            end += 5;
        }
        let mut suffix_start = end;
        let mut allowed = 0;
        while allowed <= 8 && suffix_start <= characters.len() {
            let text = if characters[suffix_start..].starts_with(&['学', '期']) {
                let stop = suffix_start + 2;
                Some((stop, characters[index..stop].iter().collect::<String>()))
            } else if characters[suffix_start..].starts_with(&['暑', '期', '学', '校']) {
                let stop = suffix_start + 4;
                Some((stop, characters[index..stop].iter().collect::<String>()))
            } else {
                None
            };
            if let Some((_, candidate)) = text {
                matches.push((index, candidate));
            }
            if suffix_start == characters.len() {
                break;
            }
            let character = characters[suffix_start];
            if character.is_whitespace() || matches!(character, '<' | '>' | '（' | '）' | '(' | ')')
            {
                break;
            }
            suffix_start += 1;
            allowed += 1;
        }
        index = end.max(index + 1);
    }
    if matches.is_empty() {
        return None;
    }
    if let Some(anchor) = html.find("dgData") {
        let anchor = html[..anchor].chars().count();
        matches.sort_by(|left, right| {
            left.0
                .abs_diff(anchor)
                .cmp(&right.0.abs_diff(anchor))
                .then_with(|| right.1.len().cmp(&left.1.len()))
                .then_with(|| left.0.cmp(&right.0))
        });
        Some(matches[0].1.clone())
    } else {
        matches.sort_by_key(|(start, _)| *start);
        Some(matches.last().expect("non-empty").1.clone())
    }
}

pub(super) fn academic_year_start(term: &str) -> Option<i32> {
    let characters = term.chars().collect::<Vec<_>>();
    characters.windows(4).find_map(|window| {
        if window.iter().all(char::is_ascii_digit) {
            window.iter().collect::<String>().parse().ok()
        } else {
            None
        }
    })
}

// ---------------------------------------------------------------------------
// Exams.
// ---------------------------------------------------------------------------

pub(super) fn parse_exams(html: &str) -> CliResult<Value> {
    let mut header: Vec<String> = Vec::new();
    let mut exams: Vec<Value> = Vec::new();
    for table in tag_elements(html, "table") {
        for row in row_elements(&table.inner) {
            let cells = cell_elements(&row.inner)
                .iter()
                .map(|cell| clean_text(&cell.inner))
                .collect::<Vec<_>>();
            if cells.is_empty() {
                continue;
            }
            if header.is_empty() && cells[0].contains("考试课程") {
                header = cells;
                continue;
            }
            if !header.is_empty()
                && cells.len() >= 6
                && cells[0] != "没有信息数据"
                && !cells[0].contains("考试课程")
            {
                exams.push(json!({
                    "course": cells[0],
                    "mode": cells.get(1).cloned().unwrap_or_default(),
                    "date": cells.get(2).cloned().unwrap_or_default(),
                    "time": cells.get(3).cloned().unwrap_or_default(),
                    "room": cells.get(4).cloned().unwrap_or_default(),
                    "note": cells.get(6).cloned().unwrap_or_default(),
                }));
            }
        }
    }
    if header.is_empty() {
        return Err(exam_error(
            "考试安排页面里找不到「考试课程」表头（页面结构可能已变）",
        ));
    }
    let count = exams.len();
    let note = if count == 0 {
        "本学期暂无考试安排（研究生课程多为论文/大作业考核）"
    } else {
        ""
    };
    Ok(json!({
        "columns": header,
        "count": count,
        "exams": exams,
        "note": note,
    }))
}

// ---------------------------------------------------------------------------
// Snapshot conversion.
// ---------------------------------------------------------------------------

pub(super) fn snapshot_from_pages(username: &str, pages: &GradPages) -> CliResult<super::Snapshot> {
    let schedule = pages.schedule.as_deref().ok_or_else(|| {
        upstream(
            "研究生课表页面缺失，无法生成快照",
            "重试 `bupt schedule update`；不会只保存半份课表。",
        )
    })?;
    let calendar_html = pages.calendar.as_deref().ok_or_else(|| {
        upstream(
            "研究生教学日历页面缺失，无法生成快照",
            "重试 `bupt schedule update`；不会用周次猜日期。",
        )
    })?;
    let term = term_from_html(calendar_html).ok_or_else(|| {
        upstream(
            "教学日历页里找不到学期标签，无法确定学年（不会猜年份）",
            "页面结构可能已变；重试一次，仍失败时反馈教学日历页面结构。",
        )
    })?;
    let year = academic_year_start(&term).ok_or_else(|| {
        upstream(
            "学期标签里没有年份，无法确定教学周日期",
            "页面结构可能已变；不会猜年份。",
        )
    })?;
    let courses = parse_timetable(schedule)?;
    let calendar = parse_calendar(calendar_html, year)?;
    Ok(super::Snapshot {
        username: username.to_string(),
        role: "graduate".to_string(),
        term,
        source: "yjxt".to_string(),
        courses: courses
            .into_iter()
            .map(|course| {
                let periods = (course.period_start..=course.period_end).collect::<Vec<_>>();
                json!({
                    "name": course.name,
                    "weekday": WEEKDAYS[course.weekday_index as usize],
                    "weekday_index": course.weekday_index,
                    "weeks": course.weeks,
                    "period_start": course.period_start,
                    "period_end": course.period_end,
                    "periods": periods,
                    "time_start": course.time_start,
                    "time_end": course.time_end,
                    "teachers": course.teachers,
                    "rooms": course.rooms,
                    "campus": course.campus,
                })
            })
            .collect(),
        calendar,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIMETABLE: &str =
        include_str!("../../tests/fixtures/contracts/academics/timetable_graduate.html");
    const CALENDAR: &str =
        include_str!("../../tests/fixtures/contracts/academics/calendar_graduate.html");
    const EXAMS_EMPTY: &str =
        include_str!("../../tests/fixtures/contracts/academics/exams_graduate_empty.html");
    const EXAMS_ROWS: &str =
        include_str!("../../tests/fixtures/contracts/academics/exams_graduate_rows.html");

    #[test]
    fn timetable_expands_rowspans_and_merges_consecutive_periods() {
        let courses = parse_timetable(TIMETABLE).expect("parse synthetic timetable");

        assert_eq!(courses.len(), 2, "{courses:#?}");
        let fibre = courses
            .iter()
            .find(|course| course.name == "合成光纤课程")
            .expect("rowspan course");
        assert_eq!(fibre.campus, "西土城");
        assert_eq!(fibre.weeks, vec![4]);
        assert_eq!(fibre.teachers, vec!["测试教师甲"]);
        assert_eq!(fibre.rooms, vec!["3-105"]);
        assert_eq!(fibre.weekday_index, 2);
        assert_eq!((fibre.period_start, fibre.period_end), (1, 2));
        assert_eq!(
            (fibre.time_start.as_str(), fibre.time_end.as_str()),
            ("08:00", "09:35")
        );

        let communication = courses
            .iter()
            .find(|course| course.name == "合成通信课程")
            .expect("second course");
        assert_eq!(communication.campus, "沙河");
        assert_eq!(communication.weeks.first(), Some(&4));
        assert_eq!(communication.weeks.last(), Some(&19));
        assert_eq!(
            (communication.period_start, communication.period_end),
            (3, 4)
        );
        assert_eq!(communication.weekday_index, 1);
    }

    #[test]
    fn calendar_maps_chinese_weeks_and_crosses_the_new_year() {
        let weeks = parse_calendar(CALENDAR, 2026).expect("parse synthetic calendar");
        assert_eq!(
            weeks.get(&1).expect("week one"),
            &vec![
                "2026-08-31".to_string(),
                "2026-09-01".to_string(),
                "2026-09-02".to_string(),
                "2026-09-03".to_string(),
                "2026-09-04".to_string(),
                "2026-09-05".to_string(),
                "2026-09-06".to_string(),
            ]
        );
        assert_eq!(
            weeks.get(&18).expect("week eighteen"),
            &vec![
                "2026-12-29".to_string(),
                "2026-12-30".to_string(),
                "2026-12-31".to_string(),
                "2027-01-01".to_string(),
                "2027-01-02".to_string(),
                "2027-01-03".to_string(),
                "2027-01-04".to_string(),
            ]
        );
    }

    #[test]
    fn term_label_is_taken_from_the_calendar_year_and_never_guessed() {
        assert_eq!(term_from_html(CALENDAR).as_deref(), Some("2026-2027秋学期"));
        assert_eq!(academic_year_start("2026-2027秋学期"), Some(2026));
        assert_eq!(academic_year_start("暑期学校"), None);
        assert_eq!(term_from_html("<table id='dgData'></table>"), None);
        assert!(parse_calendar("<table id='dgData'></table>", 2026).is_err());
    }

    #[test]
    fn missing_page_markers_are_errors_not_empty_results() {
        assert!(parse_timetable("<table><tr><td>布局漂移</td></tr></table>").is_err());
        assert!(parse_calendar("<table id='dgData'></table>", 2026).is_err());
        assert!(parse_calendar("<table><tr><td>布局漂移</td></tr></table>", 2026).is_err());
    }

    #[test]
    fn exams_empty_marker_keeps_the_header_and_invents_no_rows() {
        let value = parse_exams(EXAMS_EMPTY).expect("parse empty exams");
        assert_eq!(value["count"], json!(0));
        assert_eq!(value["exams"], json!([]));
        assert!(
            value["columns"]
                .as_array()
                .is_some_and(|cells| cells[0] == "考试课程")
        );
        assert!(
            value["note"]
                .as_str()
                .is_some_and(|note| note.contains("暂无"))
        );
    }

    #[test]
    fn exams_real_row_preserves_every_reported_field() {
        let value = parse_exams(EXAMS_ROWS).expect("parse exam rows");
        assert_eq!(value["count"], json!(1));
        assert_eq!(value["exams"][0]["course"], json!("合成通信网课程"));
        assert_eq!(value["exams"][0]["date"], json!("2026-01-05"));
        assert_eq!(value["exams"][0]["time"], json!("09:00-11:00"));
        assert_eq!(value["exams"][0]["room"], json!("3-101"));
        assert_eq!(value["exams"][0]["note"], json!("合成备注"));
    }

    #[test]
    fn malformed_exam_page_is_an_error_not_an_empty_schedule() {
        let error = parse_exams("<table><tr><td>布局漂移</td></tr></table>")
            .expect_err("missing header must fail");
        assert_eq!(error.code, 5);
        assert!(error.message.contains("考试课程"));
    }

    #[test]
    fn snapshot_from_pages_requires_both_pages_and_a_term_label() {
        let pages = GradPages {
            schedule: Some(TIMETABLE.to_string()),
            calendar: Some(CALENDAR.to_string()),
            exams: None,
        };
        let snapshot = snapshot_from_pages("20260001", &pages).expect("synthetic snapshot");
        assert_eq!(snapshot.term, "2026-2027秋学期");
        assert_eq!(snapshot.source, "yjxt");
        assert_eq!(snapshot.courses.len(), 2);
        assert_eq!(snapshot.calendar.len(), 2);

        let missing_calendar = GradPages {
            schedule: Some(TIMETABLE.to_string()),
            calendar: None,
            exams: None,
        };
        assert!(snapshot_from_pages("20260001", &missing_calendar).is_err());

        let no_term = GradPages {
            schedule: Some(TIMETABLE.to_string()),
            calendar: Some("<table id='dgData'></table>".to_string()),
            exams: None,
        };
        assert!(snapshot_from_pages("20260001", &no_term).is_err());
    }
}
