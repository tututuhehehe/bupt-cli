use crate::cli::{CliError, CliResult, Context};
use crate::transport::{Request, Response};
use serde_json::{Value, json};

const OPAC_BASE: &str = "https://opac.bupt.edu.cn";
const SEARCH_URL: &str = "https://opac.bupt.edu.cn/search-full-result.json";
const OPAC_REFERER: &str = "https://opac.bupt.edu.cn/search-full-result.html";
const OPAC_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/157.0.0.0 Safari/537.36";

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

pub(super) fn library_search(
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

pub(super) fn library_holdings(id: &str, context: &mut Context<'_>) -> CliResult<Value> {
    let id = id.trim();
    if id.is_empty() {
        return Err(CliError::usage("recCtrlId 不能为空"));
    }
    let url = format!("{OPAC_BASE}/bookInfo_{}.html", path_segment_encode(id));
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

pub(super) fn electronic_resources(id: &str, context: &mut Context<'_>) -> CliResult<Value> {
    let id = id.trim();
    if id.is_empty() {
        return Err(CliError::usage("recCtrlId 不能为空"));
    }
    let body = form_body(&[("sid", id)]);
    let referer = format!("{OPAC_BASE}/bookInfo_{}.html", path_segment_encode(id));
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
    context.transport.send(request).map_err(|_| {
        CliError::new(
            2,
            "usage",
            "library request failed",
            "opac.bupt.edu.cn must be reachable; try `bupt diagnose`",
        )
    })
}

fn ensure_success(response: &Response, operation: &str) -> CliResult<()> {
    if (200..300).contains(&response.status) {
        return Ok(());
    }
    Err(CliError::new(
        2,
        "usage",
        format!("{operation} failed: HTTP {}", response.status),
        "retry later; if it persists, run `bupt diagnose`",
    ))
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

fn path_segment_encode(value: &str) -> String {
    percent_encode(value, "%20")
}

fn form_encode(value: &str) -> String {
    percent_encode(value, "+")
}

fn percent_encode(value: &str, space: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else if byte == b' ' {
            encoded.push_str(space);
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
    serde_json::from_str(text).map_err(|_| {
        CliError::new(
            2,
            "usage",
            format!("{operation} returned invalid JSON"),
            "the OPAC response format may have changed; retry or run `bupt diagnose`",
        )
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::testing::Scripted;
    use std::fs;
    use std::path::{Path, PathBuf};

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
    fn library_ids_are_escaped_as_single_path_segment() {
        let id = "记录/一?x#y%";
        let expected = "bookInfo_%E8%AE%B0%E5%BD%95%2F%E4%B8%80%3Fx%23y%25.html";
        let hold_url = format!("{OPAC_BASE}/{expected}");
        let mut transport = Scripted::default();
        transport.expected.push_back((
            "GET",
            hold_url.clone(),
            response(200, fixture("library_holdings.html")),
        ));
        transport.expected.push_back((
            "POST",
            format!("{OPAC_BASE}/dianzi.json"),
            response(200, fixture("library_ebook.json")),
        ));

        let hold = {
            let mut context = test_context(&mut transport);
            library_holdings(id, &mut context).unwrap()
        };
        let ebook = {
            let mut context = test_context(&mut transport);
            electronic_resources(id, &mut context).unwrap()
        };

        assert_eq!(hold["id"], id);
        assert_eq!(hold["url"], hold_url);
        assert_eq!(ebook["id"], id);
        assert_eq!(path_segment_encode("fixture-book_1"), "fixture-book_1");
        assert_eq!(transport.calls.len(), 2);
        assert_eq!(transport.calls[0].url, hold_url);
        assert_eq!(
            transport.calls[1]
                .headers
                .iter()
                .find(|(name, _)| name == "Referer")
                .map(|(_, value)| value.as_str()),
            Some(hold_url.as_str())
        );
        let body = String::from_utf8_lossy(&transport.calls[1].body);
        assert_eq!(body, format!("sid={}", form_encode(id)));
    }

    #[test]
    fn library_transport_errors_do_not_echo_internal_diagnostics() {
        const INTERNAL: &str = "connect failed: 10.0.0.1:443 secret detail";
        let mut transport = Scripted::default();
        transport.expected.push_back((
            "POST",
            SEARCH_URL.into(),
            Err(CliError::new(4, "network", INTERNAL, "")),
        ));
        let error = {
            let mut context = test_context(&mut transport);
            library_search("x", 1, 10, "all", false, &mut context).unwrap_err()
        };
        assert_eq!(error.code, 2);
        assert_eq!(error.kind, "usage");
        assert_eq!(error.message, "library request failed");
        assert!(!error.message.contains(INTERNAL));
        assert!(error.hint.contains("diagnose"));
    }
}
