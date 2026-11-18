use crate::cli::{CliError, CliResult, Context, Outcome};
use crate::transport::{Request, Response};
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const OPAC_BASE: &str = "https://opac.bupt.edu.cn";
const SEARCH_URL: &str = "https://opac.bupt.edu.cn/search-full-result.json";
const OPAC_REFERER: &str = "https://opac.bupt.edu.cn/search-full-result.html";
const OPAC_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/157.0.0.0 Safari/537.36";

const DEFAULT_BUILDING: &str = "学一楼";
const DEFAULT_ROOM: &str = "101";
const STATUS_WORDS: &[&str] = &[
    "可借",
    "借出",
    "在架",
    "归还",
    "预约",
    "不外借",
    "遗失",
    "维修",
];
const CAMPUS_HINTS: &[&str] = &["沙河", "本部", "宏福", "西土城", "校区"];

#[derive(Clone, Copy)]
struct Building {
    key: &'static str,
    name: &'static str,
}

const BUILDINGS: &[Building] = &[
    Building {
        key: "SHAHE_A",
        name: "沙河校区雁北园A楼",
    },
    Building {
        key: "SHAHE_B",
        name: "沙河校区雁北园B楼",
    },
    Building {
        key: "SHAHE_C",
        name: "沙河校区雁北园C楼",
    },
    Building {
        key: "SHAHE_D1",
        name: "沙河校区雁北园D1楼",
    },
    Building {
        key: "SHAHE_D2",
        name: "沙河校区雁北园D2楼",
    },
    Building {
        key: "SHAHE_E",
        name: "沙河校区雁北园E楼",
    },
    Building {
        key: "SHAHE_S2",
        name: "沙河校区雁南园S2楼",
    },
    Building {
        key: "SHAHE_S3",
        name: "沙河校区雁南园S3楼",
    },
    Building {
        key: "SHAHE_S4",
        name: "沙河校区雁南园S4楼",
    },
    Building {
        key: "SHAHE_S5",
        name: "沙河校区雁南园S5楼",
    },
    Building {
        key: "SHAHE_S6",
        name: "沙河校区雁南园S6楼",
    },
    Building {
        key: "HAIDIAN_XUE_1",
        name: "学一楼",
    },
    Building {
        key: "HAIDIAN_XUE_2",
        name: "学二楼",
    },
    Building {
        key: "HAIDIAN_XUE_3",
        name: "学三楼",
    },
    Building {
        key: "HAIDIAN_XUE_4",
        name: "学四楼",
    },
    Building {
        key: "HAIDIAN_XUE_5",
        name: "学五楼",
    },
    Building {
        key: "HAIDIAN_XUE_6",
        name: "学六楼",
    },
    Building {
        key: "HAIDIAN_XUE_8",
        name: "学八楼",
    },
    Building {
        key: "HAIDIAN_XUE_9",
        name: "学九楼",
    },
    Building {
        key: "HAIDIAN_XUE_10",
        name: "学十楼",
    },
    Building {
        key: "HAIDIAN_XUE_11",
        name: "学十一楼",
    },
    Building {
        key: "HAIDIAN_XUE_29",
        name: "90学二十九楼",
    },
];

#[derive(Clone, Copy)]
enum Area {
    Shahe,
    Xitucheng,
}

impl Area {
    fn name(self) -> &'static str {
        match self {
            Self::Shahe => "shahe",
            Self::Xitucheng => "xitucheng",
        }
    }
}

#[derive(Default)]
struct DormFlags {
    building: Option<String>,
    room: Option<String>,
    floor: Option<String>,
}

#[derive(Default)]
struct DormEnv {
    building: Option<String>,
    room: Option<String>,
    floor: Option<String>,
}

struct Dorm {
    building: &'static Building,
    room: String,
    floor: Option<String>,
    source: &'static str,
}

pub(crate) fn commands() -> Vec<Command> {
    vec![
        Command::new("library")
            .about("Search the campus library")
            .subcommand_required(true)
            .subcommand(
                Command::new("search")
                    .about("Find a title")
                    .arg(Arg::new("keyword").required(true).value_name("KEYWORD"))
                    .arg(
                        Arg::new("page")
                            .long("page")
                            .value_parser(positive_usize)
                            .default_value("1"),
                    )
                    .arg(
                        Arg::new("page-size")
                            .long("page-size")
                            .value_parser(positive_usize)
                            .default_value("10"),
                    )
                    .arg(
                        Arg::new("location")
                            .long("location")
                            .default_value("all")
                            .value_name("CAMPUS"),
                    )
                    .arg(
                        Arg::new("with-ebook")
                            .long("with-ebook")
                            .action(ArgAction::SetTrue),
                    ),
            )
            .subcommand(
                Command::new("hold")
                    .about("Show physical holdings")
                    .arg(Arg::new("id").required(true).value_name("RECCTRLID")),
            )
            .subcommand(
                Command::new("ebook")
                    .about("Show electronic copies")
                    .arg(Arg::new("id").required(true).value_name("RECCTRLID")),
            ),
        Command::new("electricity")
            .about("Read dorm electricity balance")
            .arg(Arg::new("building").long("building").value_name("BUILDING"))
            .arg(Arg::new("room").long("room").value_name("ROOM"))
            .arg(Arg::new("floor").long("floor").value_name("FLOOR"))
            .arg(
                Arg::new("threshold")
                    .long("threshold")
                    .value_parser(non_negative_f64)
                    .default_value("10"),
            )
            .arg(Arg::new("area").long("area").value_name("AREA").hide(true))
            .subcommand(
                Command::new("buildings").about("List dorm buildings").arg(
                    Arg::new("area")
                        .long("area")
                        .default_value("shahe")
                        .value_name("AREA"),
                ),
            ),
        Command::new("notices")
            .about("List campus notices")
            .arg(Arg::new("type").long("type").value_name("TYPES"))
            .arg(
                Arg::new("count")
                    .long("count")
                    .value_parser(non_negative_u64)
                    .default_value("10"),
            )
            .subcommand(
                Command::new("show")
                    .about("Read a notice")
                    .arg(Arg::new("url").required(true).value_name("URL"))
                    .arg(
                        Arg::new("max-chars")
                            .long("max-chars")
                            .value_parser(non_negative_usize)
                            .default_value("0"),
                    ),
            ),
        Command::new("card").about("Read campus card balance"),
    ]
}

pub(crate) fn run(
    name: &str,
    matches: &ArgMatches,
    context: &mut Context<'_>,
) -> CliResult<Outcome> {
    match name {
        "library" => run_library(matches, context),
        "electricity" => run_electricity(matches, context),
        "notices" => run_notices(matches, context),
        "card" => run_card(context),
        _ => Err(CliError::usage("unknown campus command")),
    }
}

fn positive_usize(text: &str) -> Result<usize, String> {
    let value = text
        .parse::<usize>()
        .map_err(|_| "expected a positive integer")?;
    if value == 0 {
        return Err("expected a positive integer".into());
    }
    Ok(value)
}

fn non_negative_usize(text: &str) -> Result<usize, String> {
    text.parse::<usize>()
        .map_err(|_| "expected a non-negative integer".into())
}

fn non_negative_u64(text: &str) -> Result<u64, String> {
    text.parse::<u64>()
        .map_err(|_| "expected a non-negative integer".into())
}

fn non_negative_f64(text: &str) -> Result<f64, String> {
    let value = text
        .parse::<f64>()
        .map_err(|_| "expected a non-negative number")?;
    if !value.is_finite() || value < 0.0 {
        return Err("expected a finite non-negative number".into());
    }
    Ok(value)
}

fn run_library(matches: &ArgMatches, context: &mut Context<'_>) -> CliResult<Outcome> {
    let Some((subcommand, arguments)) = matches.subcommand() else {
        return Err(CliError::usage("library needs a subcommand"));
    };
    let data = match subcommand {
        "search" => {
            let keyword = arguments
                .get_one::<String>("keyword")
                .map(String::as_str)
                .unwrap_or("");
            let page = *arguments
                .get_one::<usize>("page")
                .expect("page has a default");
            let page_size = *arguments
                .get_one::<usize>("page-size")
                .expect("page-size has a default");
            let location = arguments
                .get_one::<String>("location")
                .map(String::as_str)
                .unwrap_or("all");
            let with_ebook = arguments.get_flag("with-ebook");
            library_search(keyword, page, page_size, location, with_ebook, context)?
        }
        "hold" => {
            let id = arguments
                .get_one::<String>("id")
                .map(String::as_str)
                .unwrap_or("");
            library_holdings(id, context)?
        }
        "ebook" => {
            let id = arguments
                .get_one::<String>("id")
                .map(String::as_str)
                .unwrap_or("");
            electronic_resources(id, context)?
        }
        _ => return Err(CliError::usage("library needs a subcommand")),
    };
    Ok(Outcome {
        data,
        code: 0,
        diagnostic: None,
    })
}

fn library_search(
    keyword: &str,
    page: usize,
    page_size: usize,
    location: &str,
    with_ebook: bool,
    context: &mut Context<'_>,
) -> CliResult<Value> {
    let keyword = keyword.trim();
    if keyword.is_empty() {
        return Err(CliError::usage("关键词不能为空"));
    }

    let body = form_body(&[
        ("xiaoqu", location),
        ("keyword", keyword),
        ("solrType", "TITLE"),
        ("pageNo", &page.to_string()),
        ("pageSize", &page_size.to_string()),
        ("classSearch", "0"),
        ("searchPrecision", ""),
    ]);
    let response = library_send(
        context,
        Request {
            method: "POST",
            url: SEARCH_URL.into(),
            headers: opac_headers(OPAC_REFERER, "XMLHttpRequest"),
            body,
        },
    )?;
    ensure_success(&response, "library search")?;
    let mut payload = parse_search(&text_body(&response), keyword, page)?;

    if with_ebook {
        if let Some(books) = payload.get_mut("books").and_then(Value::as_array_mut) {
            for book in books {
                let id = book
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let value = match electronic_resources(&id, context) {
                    Ok(result) => result["has_electronic"].clone(),
                    Err(_) => Value::Null,
                };
                book["has_electronic"] = value;
            }
        }
    }
    Ok(payload)
}

fn parse_search(text: &str, keyword: &str, page: usize) -> CliResult<Value> {
    let payload: Value = parse_json(text, "OPAC 检索")?;
    let result = payload.get("result").and_then(Value::as_object);
    let code = result.and_then(|result| result.get("code"));
    if !success_code(code) {
        let message = result
            .and_then(|result| result.get("msg"))
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return Err(CliError::usage(format!("OPAC 检索失败: {message}")));
    }

    let books: Vec<Value> = payload
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .map(book_record)
        .collect();
    Ok(json!({
        "keyword": keyword,
        "total": unsigned_field(&payload, "total", 0),
        "page": unsigned_field(&payload, "pageNo", page as u64),
        "total_page": unsigned_field(&payload, "totalPage", 0),
        "count": books.len(),
        "books": books,
    }))
}

fn book_record(item: &serde_json::Map<String, Value>) -> Value {
    json!({
        "id": clean_text(&text_value(item.get("recCtrlId"))),
        "title": clean_text(&text_value(item.get("title"))),
        "authors": clean_text(&text_value(item.get("authors"))),
        "isbn": clean_text(&text_value(item.get("isn"))),
        "publisher": clean_text(&text_value(item.get("publisher"))),
        "year": clean_text(&text_value(item.get("pubdateDate"))),
        "call_number": clean_text(&text_value(item.get("bookSearchNo"))),
        "library": clean_text(&text_value(item.get("libraryName"))),
        "holding_count": int_or_none(item.get("guancangCount")),
        "available_count": int_or_none(item.get("kejieCount")),
    })
}

fn library_holdings(id: &str, context: &mut Context<'_>) -> CliResult<Value> {
    let id = id.trim();
    if id.is_empty() {
        return Err(CliError::usage("recCtrlId 不能为空"));
    }
    let url = format!("{OPAC_BASE}/bookInfo_{id}.html");
    let response = library_send(
        context,
        Request {
            method: "GET",
            url: url.clone(),
            headers: opac_headers(OPAC_REFERER, ""),
            body: Vec::new(),
        },
    )?;
    if response.status == 404 || response.status >= 500 {
        return Err(CliError::usage(format!(
            "找不到该记录: {id}（id 来自 `bupt library search`）"
        )));
    }
    ensure_success(&response, "library hold")?;
    parse_holdings(&text_body(&response), id, &url)
}

fn parse_holdings(text: &str, id: &str, url: &str) -> CliResult<Value> {
    let page_title = extract_tag_blocks(text, "title")
        .first()
        .map(|title| clean_text(title))
        .unwrap_or_default();

    let mut copies = Vec::new();
    for row in extract_tag_blocks(text, "tr") {
        let cells: Vec<String> = extract_tag_blocks(row, "td")
            .into_iter()
            .map(clean_text)
            .collect();
        if cells.is_empty() {
            continue;
        }
        let department = &cells[0];
        if !CAMPUS_HINTS.iter().any(|hint| department.contains(hint)) {
            continue;
        }
        copies.push(json!({
            "department": department,
            "barcode": cells.get(1).cloned().unwrap_or_default(),
            "call_number": cells.get(2).cloned().unwrap_or_default(),
            "status": row_status(&cells),
        }));
    }
    let available = copies
        .iter()
        .filter(|copy| {
            copy["status"]
                .as_str()
                .is_some_and(|status| status.contains("可借"))
        })
        .count();
    Ok(json!({
        "id": id,
        "url": url,
        "page_title": page_title,
        "count": copies.len(),
        "available": available,
        "copies": copies,
    }))
}

fn row_status(cells: &[String]) -> String {
    if cells
        .get(6)
        .is_some_and(|status| STATUS_WORDS.iter().any(|word| status.contains(word)))
    {
        return cells[6].clone();
    }
    cells
        .iter()
        .skip(4)
        .find(|cell| STATUS_WORDS.iter().any(|word| cell.contains(word)))
        .cloned()
        .unwrap_or_default()
}

fn electronic_resources(id: &str, context: &mut Context<'_>) -> CliResult<Value> {
    let id = id.trim();
    if id.is_empty() {
        return Err(CliError::usage("recCtrlId 不能为空"));
    }
    let body = form_body(&[("sid", id)]);
    let referer = format!("{OPAC_BASE}/bookInfo_{id}.html");
    let response = library_send(
        context,
        Request {
            method: "POST",
            url: format!("{OPAC_BASE}/dianzi.json"),
            headers: opac_headers(&referer, "XMLHttpRequest"),
            body,
        },
    )?;
    ensure_success(&response, "library ebook")?;
    parse_electronic_resources(&text_body(&response), id)
}

fn parse_electronic_resources(text: &str, id: &str) -> CliResult<Value> {
    let payload: Value = parse_json(text, "电子资源查询")?;
    let result = payload.get("result").and_then(Value::as_object);
    let code = result.and_then(|result| result.get("code"));
    if !success_code(code) {
        let message = result
            .and_then(|result| result.get("msg"))
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return Err(CliError::usage(format!("电子资源查询失败: {message}")));
    }

    let mut resources = Vec::new();
    for item in payload
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
    {
        let url = text_value(item.get("locate"))
            .replace('\u{1e}', "")
            .trim()
            .to_string();
        let label = clean_text(&text_value(item.get("recNote")));
        if !url.is_empty() || !label.is_empty() {
            resources.push(json!({"label": label, "url": url}));
        }
    }
    Ok(json!({
        "id": id,
        "count": resources.len(),
        "has_electronic": !resources.is_empty(),
        "resources": resources,
    }))
}

fn library_send(context: &mut Context<'_>, request: Request) -> CliResult<Response> {
    context.transport.send(request).map_err(|error| {
        CliError::new(
            2,
            "usage",
            format!("library request failed: {}", error.message),
            "opac.bupt.edu.cn must be reachable; try `bupt diagnose`",
        )
    })
}

fn ensure_success(response: &Response, operation: &str) -> CliResult<()> {
    if (200..300).contains(&response.status) {
        return Ok(());
    }
    Err(CliError::usage(format!(
        "{operation} failed: HTTP {}",
        response.status
    )))
}

fn opac_headers(referer: &str, requested_with: &str) -> Vec<(String, String)> {
    vec![
        ("User-Agent".into(), OPAC_USER_AGENT.into()),
        ("Referer".into(), referer.into()),
        ("X-Requested-With".into(), requested_with.into()),
        (
            "Content-Type".into(),
            "application/x-www-form-urlencoded; charset=UTF-8".into(),
        ),
    ]
}

fn form_body(fields: &[(&str, &str)]) -> Vec<u8> {
    fields
        .iter()
        .map(|(key, value)| format!("{}={}", form_encode(key), form_encode(value)))
        .collect::<Vec<_>>()
        .join("&")
        .into_bytes()
}

fn form_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else if byte == b' ' {
            encoded.push('+');
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn text_body(response: &Response) -> String {
    String::from_utf8_lossy(&response.body).into_owned()
}

fn parse_json(text: &str, operation: &str) -> CliResult<Value> {
    serde_json::from_str(text)
        .map_err(|error| CliError::usage(format!("{operation} returned invalid JSON: {error}")))
}

fn success_code(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Number(number)) => number.as_i64() == Some(0),
        Some(Value::String(text)) => text.trim() == "0",
        _ => false,
    }
}

fn unsigned_field(payload: &Value, key: &str, default: u64) -> u64 {
    match payload.get(key) {
        Some(Value::Number(number)) => number.as_u64().unwrap_or(default),
        Some(Value::String(text)) => text.parse::<u64>().unwrap_or(default),
        _ => default,
    }
}

fn int_or_none(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(Value::Number(number)) => number
            .as_i64()
            .or_else(|| number.as_f64().map(|value| value.trunc() as i64)),
        Some(Value::String(text)) => text.parse::<i64>().ok(),
        _ => None,
    }
}

fn text_value(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}

fn clean_text(value: &str) -> String {
    let without_tags = strip_tags(value);
    let decoded = decode_entities(&without_tags);
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_tags(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;
    let mut copied = 0;
    while cursor < text.len() {
        if text.as_bytes()[cursor] == b'<' {
            if let Some(end) = text[cursor..].find('>') {
                output.push_str(&text[copied..cursor]);
                cursor += end + 1;
                copied = cursor;
                continue;
            }
        }
        cursor += 1;
    }
    output.push_str(&text[copied..]);
    output
}

fn decode_entities(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find('&') {
        output.push_str(&rest[..index]);
        let tail = &rest[index..];
        if let Some(end) = tail.find(';') {
            let entity = &tail[1..end];
            if let Some(character) = decode_entity(entity) {
                output.push(character);
                rest = &tail[end + 1..];
                continue;
            }
        }
        output.push('&');
        rest = &tail[1..];
    }
    output.push_str(rest);
    output
}

fn decode_entity(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some('\u{a0}'),
        hex if hex.starts_with("#x") || hex.starts_with("#X") => {
            char::from_u32(u32::from_str_radix(&hex[2..], 16).ok()?)
        }
        decimal if decimal.starts_with('#') => char::from_u32(decimal[1..].parse::<u32>().ok()?),
        _ => None,
    }
}

fn extract_tag_blocks<'a>(text: &'a str, tag: &str) -> Vec<&'a str> {
    let lower = text.to_ascii_lowercase();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut blocks = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = lower[cursor..].find(&open) {
        let start = cursor + relative;
        let after_name = start + open.len();
        let boundary = lower[after_name..].chars().next();
        if !matches!(boundary, Some(character) if character.is_ascii_whitespace() || character == '>' || character == '/')
        {
            cursor = after_name;
            continue;
        }
        let Some(open_end) = lower[after_name..].find('>') else {
            break;
        };
        let content_start = after_name + open_end + 1;
        let Some(close_relative) = lower[content_start..].find(&close) else {
            break;
        };
        let content_end = content_start + close_relative;
        blocks.push(&text[content_start..content_end]);
        cursor = content_end + close.len();
    }
    blocks
}

fn run_electricity(matches: &ArgMatches, context: &mut Context<'_>) -> CliResult<Outcome> {
    if let Some(("buildings", arguments)) = matches.subcommand() {
        let requested = arguments
            .get_one::<String>("area")
            .map(String::as_str)
            .unwrap_or("shahe");
        let area = resolve_area(requested)?;
        return Err(session_unavailable(
            context,
            &format!("electricity buildings ({})", area.name()),
        ));
    }
    if matches.get_one::<String>("area").is_some() {
        return Err(CliError::new(
            2,
            "usage",
            "--area is only valid for `bupt electricity buildings`",
            "try `bupt electricity buildings --area shahe` or `bupt electricity buildings --area xitucheng`",
        ));
    }

    let flags = DormFlags {
        building: matches.get_one::<String>("building").cloned(),
        room: matches.get_one::<String>("room").cloned(),
        floor: matches.get_one::<String>("floor").cloned(),
    };
    let dorm = resolve_dorm(&flags, &dorm_env(), &context.config_dir)?;
    let _floor = dorm.floor.as_deref();
    let _source = dorm.source;
    Err(session_unavailable(
        context,
        &format!("electricity {} {}", dorm.building.name, dorm.room),
    ))
}

fn run_notices(matches: &ArgMatches, context: &mut Context<'_>) -> CliResult<Outcome> {
    if let Some(("show", arguments)) = matches.subcommand() {
        let _url = arguments
            .get_one::<String>("url")
            .map(String::as_str)
            .unwrap_or("");
        let _max_chars = *arguments
            .get_one::<usize>("max-chars")
            .expect("max-chars has a default");
        return Err(session_unavailable(context, "notices show"));
    }

    let _types = notice_types(matches.get_one::<String>("type"))?;
    let _count = *matches
        .get_one::<u64>("count")
        .expect("count has a default");
    Err(session_unavailable(context, "notices"))
}

fn notice_types(value: Option<&String>) -> CliResult<Option<Vec<String>>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let mut resolved = Vec::new();
    for raw in value.split(',') {
        let key = raw.trim().to_ascii_lowercase();
        if key.is_empty() {
            continue;
        }
        if !matches!(key.as_str(), "notice" | "news" | "rules" | "ways") {
            return Err(CliError::new(
                2,
                "usage",
                format!("unknown notice type '{key}'"),
                "choose from notice, news, rules, ways",
            ));
        }
        resolved.push(key);
    }
    Ok(if resolved.is_empty() {
        None
    } else {
        Some(resolved)
    })
}

fn run_card(context: &mut Context<'_>) -> CliResult<Outcome> {
    Err(session_unavailable(context, "card"))
}

fn resolve_area(value: &str) -> CliResult<Area> {
    match value.trim().to_ascii_lowercase().as_str() {
        "shahe" | "沙河" | "2" => Ok(Area::Shahe),
        "xitucheng" | "haidian" | "西土城" | "海淀" | "1" => Ok(Area::Xitucheng),
        _ => Err(CliError::new(
            2,
            "usage",
            "--area must be 'shahe' or 'xitucheng'",
            "try `bupt electricity buildings --area shahe` or `bupt electricity buildings --area xitucheng`",
        )),
    }
}

fn dorm_env() -> DormEnv {
    DormEnv {
        building: non_empty_env("BUPT_DORM_BUILDING"),
        room: non_empty_env("BUPT_DORM_ROOM"),
        floor: non_empty_env("BUPT_DORM_FLOOR"),
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn resolve_dorm(flags: &DormFlags, env: &DormEnv, config_dir: &Path) -> CliResult<Dorm> {
    let raw_building = flags.building.as_deref().unwrap_or("");
    let raw_room = flags.room.as_deref().unwrap_or("");
    if !raw_building.is_empty() && raw_room.is_empty() {
        return Err(CliError::new(
            2,
            "usage",
            "--room is required when --building is given",
            "e.g. `bupt electricity --building 学一楼 --room 101`",
        ));
    }

    let config = load_dorm_config(config_dir)?;
    let config_building = config.get("building");
    let config_room = config.get("room");
    let config_floor = config.get("floor");

    let (building_value, _) = choose(
        flags.building.as_deref(),
        env.building.as_deref(),
        config_building.map(String::as_str),
    );
    let (room, room_source) = choose(
        flags.room.as_deref(),
        env.room.as_deref(),
        config_room.map(String::as_str),
    );
    let (floor, _) = choose(
        flags.floor.as_deref(),
        env.floor.as_deref(),
        config_floor.map(String::as_str),
    );

    let building_value = building_value
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_BUILDING.to_string());
    let room = room.unwrap_or_else(|| DEFAULT_ROOM.to_string());
    Ok(Dorm {
        building: resolve_building(&building_value)?,
        room,
        floor,
        source: room_source.unwrap_or("default"),
    })
}

fn choose(
    flag: Option<&str>,
    env: Option<&str>,
    config: Option<&str>,
) -> (Option<String>, Option<&'static str>) {
    if let Some(value) = flag.filter(|value| !value.is_empty()) {
        (Some(value.trim().to_string()), Some("flag"))
    } else if let Some(value) = env.filter(|value| !value.trim().is_empty()) {
        (Some(value.trim().to_string()), Some("env"))
    } else if let Some(value) = config.map(str::trim).filter(|value| !value.is_empty()) {
        (Some(value.to_string()), Some("config"))
    } else {
        (None, None)
    }
}

fn load_dorm_config(config_dir: &Path) -> CliResult<BTreeMap<String, String>> {
    let path = config_dir.join("config.json");
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let text = fs::read_to_string(&path).map_err(|error| {
        CliError::new(
            2,
            "usage",
            format!("cannot read config file {}: {error}", path.display()),
            "check file permissions/readability",
        )
    })?;
    let payload: Value = serde_json::from_str(&text).map_err(|error| {
        CliError::new(
            2,
            "usage",
            format!("cannot parse config file {}: {error}", path.display()),
            "fix the JSON content",
        )
    })?;
    let Value::Object(object) = payload else {
        return Err(CliError::usage(format!(
            "config file {} must contain a JSON object",
            path.display()
        )));
    };
    let dorm = match object.get("dorm") {
        None | Some(Value::Null) => return Ok(BTreeMap::new()),
        Some(Value::Object(dorm)) => dorm,
        Some(_) => {
            return Err(CliError::usage(format!(
                "config file {}: 'dorm' must be a JSON object",
                path.display()
            )));
        }
    };
    let mut values = BTreeMap::new();
    for key in ["building", "room", "floor"] {
        let Some(value) = dorm.get(key) else {
            continue;
        };
        let text = match value {
            Value::Null => continue,
            Value::String(text) => text.trim().to_string(),
            other => other.to_string(),
        };
        if !text.is_empty() {
            values.insert(key.to_string(), text);
        }
    }
    Ok(values)
}

fn resolve_building(value: &str) -> CliResult<&'static Building> {
    let raw = value.trim();
    let key = raw.to_ascii_uppercase().replace('-', "_");
    if let Some(building) = BUILDINGS
        .iter()
        .find(|building| building.key == key || building.name == raw)
    {
        return Ok(building);
    }
    if let Some(building) = BUILDINGS
        .iter()
        .find(|building| matches_building_alias(building, raw))
    {
        return Ok(building);
    }
    let choices = BUILDINGS
        .iter()
        .map(|building| building.key)
        .collect::<Vec<_>>()
        .join(", ");
    Err(CliError::new(
        2,
        "usage",
        format!("unknown building '{value}'"),
        format!(
            "known buildings: {choices}; run `bupt electricity buildings --area xitucheng` (or --area shahe) for the upstream list"
        ),
    ))
}

fn matches_building_alias(building: &Building, value: &str) -> bool {
    let Some(number) = chinese_building_number(building.name) else {
        return false;
    };
    value == format!("学{number}楼")
        || value == format!("学{number}")
        || value == format!("{number}楼")
        || value == number.to_string()
}

fn chinese_building_number(name: &str) -> Option<u32> {
    let start = name.find('学')? + '学'.len_utf8();
    let rest = &name[start..];
    let end = rest.find('楼')?;
    chinese_number(&rest[..end])
}

fn chinese_number(text: &str) -> Option<u32> {
    fn digit(character: char) -> Option<u32> {
        Some(match character {
            '零' => 0,
            '一' => 1,
            '二' => 2,
            '三' => 3,
            '四' => 4,
            '五' => 5,
            '六' => 6,
            '七' => 7,
            '八' => 8,
            '九' => 9,
            _ => return None,
        })
    }
    if text.is_empty() {
        return None;
    }
    if let Some((left, right)) = text.split_once('十') {
        let tens = if left.is_empty() {
            1
        } else {
            let mut chars = left.chars();
            let value = digit(chars.next()?)?;
            if chars.next().is_some() {
                return None;
            }
            value
        };
        let ones = if right.is_empty() {
            0
        } else {
            let mut chars = right.chars();
            let value = digit(chars.next()?)?;
            if chars.next().is_some() {
                return None;
            }
            value
        };
        return Some(tens * 10 + ones);
    }
    let mut chars = text.chars();
    let value = digit(chars.next()?)?;
    if chars.next().is_some() {
        return None;
    }
    Some(value)
}

fn has_local_profile(context: &Context<'_>) -> bool {
    let mut username = std::env::var("BUPT_USERNAME")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let mut password = std::env::var("BUPT_PASSWORD")
        .ok()
        .filter(|value| !value.is_empty());
    if username.is_some() && password.is_some() {
        return true;
    }

    let path: PathBuf = context.config_dir.join("credentials.json");
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(Value::Object(object)) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    if username.is_none() {
        username = object
            .get("username")
            .and_then(Value::as_str)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
    }
    if password.is_none() {
        password = object
            .get("password")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
    }
    username.is_some() && password.is_some()
}

fn session_unavailable(context: &Context<'_>, service: &str) -> CliError {
    if !has_local_profile(context) {
        return CliError::new(
            3,
            "auth",
            "BUPT credentials are not configured",
            "run `bupt login` (credentials come from BUPT_USERNAME/BUPT_PASSWORD or credentials.json)",
        );
    }
    CliError::new(
        5,
        "upstream",
        format!("{service} session adapter is not connected in this migration build"),
        "",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::testing::Scripted;
    use std::time::Duration;

    fn fixture(name: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/contracts/campus")
            .join(name);
        fs::read_to_string(path).expect("read synthetic fixture")
    }

    fn response(status: u16, body: String) -> CliResult<Response> {
        Ok(Response {
            status,
            headers: Vec::new(),
            body: body.into_bytes(),
        })
    }

    fn test_context(transport: &mut Scripted) -> Context<'_> {
        Context {
            home_dir: PathBuf::from("/bupt-test/home"),
            config_dir: PathBuf::from("/bupt-test/config"),
            cache_dir: PathBuf::from("/bupt-test/cache"),
            timeout: Duration::from_secs(20),
            transport,
        }
    }

    #[test]
    fn clean_text_strips_highlight_and_decodes_entities() {
        assert_eq!(clean_text("\n\n <b>机器学习方法</b>\n\n"), "机器学习方法");
        assert_eq!(clean_text("A &amp; B&nbsp;C"), "A & B C");
    }

    #[test]
    fn library_search_parses_fixture_and_sends_required_fields() {
        let mut transport = Scripted::default();
        transport.expected.push_back((
            "POST",
            SEARCH_URL.into(),
            response(200, fixture("library_search.json")),
        ));
        let data = {
            let mut context = test_context(&mut transport);
            library_search("机器学习", 2, 3, "all", false, &mut context).unwrap()
        };

        assert_eq!(data["keyword"], "机器学习");
        assert_eq!(data["total"], 2);
        assert_eq!(data["page"], 1);
        assert_eq!(data["total_page"], 1);
        assert_eq!(data["count"], 2);
        assert_eq!(data["books"][0]["title"], "示例 & 标题");
        assert_eq!(data["books"][0]["holding_count"], 5);
        assert_eq!(data["books"][0]["available_count"], 2);
        assert_eq!(data["books"][1]["title"], "缺少计数的记录");
        assert!(data["books"][1]["holding_count"].is_null());
        assert!(data["books"][1]["available_count"].is_null());

        assert_eq!(transport.calls.len(), 1);
        assert_eq!(transport.calls[0].method, "POST");
        assert!(
            transport.calls[0]
                .headers
                .contains(&("X-Requested-With".into(), "XMLHttpRequest".into()))
        );
        let body = String::from_utf8_lossy(&transport.calls[0].body);
        for field in [
            "xiaoqu=all",
            "solrType=TITLE",
            "pageNo=2",
            "pageSize=3",
            "classSearch=0",
            "searchPrecision=",
        ] {
            assert!(body.contains(field), "missing {field} in {body}");
        }
        assert!(body.contains("keyword=%E6%9C%BA%E5%99%A8%E5%AD%A6%E4%B9%A0"));
    }

    #[test]
    fn library_search_parses_empty_fixture() {
        let data = parse_search(&fixture("library_search_empty.json"), "无结果", 1).unwrap();
        assert_eq!(data["total"], 0);
        assert_eq!(data["total_page"], 0);
        assert_eq!(data["count"], 0);
        assert_eq!(data["books"], json!([]));
    }

    #[test]
    fn library_hold_parses_status_columns_and_campus_rows() {
        let mut transport = Scripted::default();
        transport.expected.push_back((
            "GET",
            format!("{OPAC_BASE}/bookInfo_fixture-book-1.html"),
            response(200, fixture("library_holdings.html")),
        ));
        let data = {
            let mut context = test_context(&mut transport);
            library_holdings("fixture-book-1", &mut context).unwrap()
        };

        assert_eq!(data["id"], "fixture-book-1");
        assert_eq!(data["page_title"], "书目详情 - 示例图书馆");
        assert_eq!(data["count"], 2);
        assert_eq!(data["available"], 1);
        assert_eq!(data["copies"][0]["status"], "在架可借");
        assert_eq!(data["copies"][1]["status"], "借出");
        assert_eq!(data["copies"][1]["barcode"], "FIXTURE-BARCODE-2");
        assert_eq!(transport.calls.len(), 1);
        assert_eq!(transport.calls[0].method, "GET");
        assert!(
            transport.calls[0]
                .headers
                .contains(&("X-Requested-With".into(), String::new()))
        );
    }

    #[test]
    fn library_ebook_parses_flat_resources_and_empty_state() {
        let mut transport = Scripted::default();
        transport.expected.push_back((
            "POST",
            format!("{OPAC_BASE}/dianzi.json"),
            response(200, fixture("library_ebook.json")),
        ));
        let data = {
            let mut context = test_context(&mut transport);
            electronic_resources("fixture-book-1", &mut context).unwrap()
        };
        assert_eq!(data["count"], 2);
        assert_eq!(data["has_electronic"], true);
        assert_eq!(
            data["resources"][0]["url"],
            "https://example.invalid/ebook/1"
        );
        assert_eq!(data["resources"][1]["label"], "电子书");

        let empty = parse_electronic_resources(&fixture("library_ebook_empty.json"), "x").unwrap();
        assert_eq!(empty["count"], 0);
        assert_eq!(empty["has_electronic"], false);
    }

    #[test]
    fn library_search_with_ebook_marks_failures_as_null() {
        let mut transport = Scripted::default();
        transport.expected.push_back((
            "POST",
            SEARCH_URL.into(),
            response(200, fixture("library_search.json")),
        ));
        transport.expected.push_back((
            "POST",
            format!("{OPAC_BASE}/dianzi.json"),
            response(200, fixture("library_ebook.json")),
        ));
        transport.expected.push_back((
            "POST",
            format!("{OPAC_BASE}/dianzi.json"),
            Err(CliError::new(4, "network", "synthetic failure", "")),
        ));
        let data = {
            let mut context = test_context(&mut transport);
            library_search("机器学习", 1, 10, "all", true, &mut context).unwrap()
        };

        assert_eq!(data["books"][0]["has_electronic"], true);
        assert!(data["books"][1]["has_electronic"].is_null());
        assert_eq!(transport.calls.len(), 3);
    }

    #[test]
    fn dorm_config_defaults_and_config_values_are_resolved_locally() {
        let directory = tempfile::tempdir().unwrap();
        let defaults =
            resolve_dorm(&DormFlags::default(), &DormEnv::default(), directory.path()).unwrap();
        assert_eq!(defaults.building.name, "学一楼");
        assert_eq!(defaults.room, "101");
        assert_eq!(defaults.floor, None);
        assert_eq!(defaults.source, "default");

        fs::write(
            directory.path().join("config.json"),
            r#"{"dorm":{"building":"学五楼","room":" 202 ","floor":"2"}}"#,
        )
        .unwrap();
        let configured =
            resolve_dorm(&DormFlags::default(), &DormEnv::default(), directory.path()).unwrap();
        assert_eq!(configured.building.name, "学五楼");
        assert_eq!(configured.room, "202");
        assert_eq!(configured.floor.as_deref(), Some("2"));
        assert_eq!(configured.source, "config");
    }

    #[test]
    fn dorm_flags_win_over_environment_and_config() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("config.json"),
            r#"{"dorm":{"building":"不存在楼","room":"202"}}"#,
        )
        .unwrap();
        let flags = DormFlags {
            building: Some("沙河校区雁北园A楼".into()),
            room: Some("102".into()),
            floor: Some("1层".into()),
        };
        let env = DormEnv {
            building: Some("不存在楼".into()),
            room: Some("414".into()),
            floor: None,
        };
        let resolved = resolve_dorm(&flags, &env, directory.path()).unwrap();
        assert_eq!(resolved.building.name, "沙河校区雁北园A楼");
        assert_eq!(resolved.room, "102");
        assert_eq!(resolved.floor.as_deref(), Some("1层"));
        assert_eq!(resolved.source, "flag");
    }
}
