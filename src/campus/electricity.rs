use super::dorm::{Area, Building, Dorm};
use super::session::{Route, ScopedTransport, Service, SessionDriver};
#[cfg(test)]
use crate::cli::Context;
use crate::cli::{CliError, CliResult};
use crate::transport::{Request, Response, Transport};
use serde_json::{Map, Value, json};

const ELECTRICITY_BASE: &str = "https://app.bupt.edu.cn/buptdf/wap/default";
pub(super) use crate::transport::electricity_scope::LANDING as REFERER;
const AUTH_MARKERS: &[&str] = &["未登录", "请先登录", "登录失效", "登录过期"];
const NOT_FOUND_MARKERS: &[&str] = &["未找到", "不存在", "无数据"];

/// Everything needed for one `/search` request. Floor and room resolution are
/// deliberately supplied by the caller; this module does not guess them.
pub(super) struct MeterTarget<'a> {
    pub(super) partment_id: &'a str,
    pub(super) floor_id: &'a str,
    pub(super) meter_number: &'a str,
    pub(super) area_id: i32,
    pub(super) building: &'a str,
    pub(super) floor: &'a str,
    pub(super) room: &'a str,
    pub(super) threshold: f64,
    pub(super) source: &'a str,
}

pub(super) fn request_url(path: &str) -> String {
    format!("{ELECTRICITY_BASE}{path}")
}

#[cfg(test)]
pub(super) fn get_buildings(context: &mut Context<'_>, area: Area) -> CliResult<Value> {
    buildings_from_transport(context.transport, area)
}

#[cfg(test)]
fn buildings_from_transport(transport: &mut dyn Transport, area: Area) -> CliResult<Value> {
    let area_id = area_id(area).to_string();
    let payload = post(transport, "/part", &[("areaid", area_id.as_str())])?;
    let data = extract_data(&payload, "/part", Expected::Array)?;
    Ok(buildings_output(data, area))
}

pub(super) fn get_buildings_with_session(
    driver: &mut dyn SessionDriver,
    area: Area,
) -> CliResult<Value> {
    let area_id = area_id(area).to_string();
    let data = post_with_session(
        driver,
        Route::electricity_buildings(),
        "/part",
        &[("areaid", area_id.as_str())],
        Expected::Array,
    )?;
    Ok(buildings_output(&data, area))
}

fn buildings_output(data: &Value, area: Area) -> Value {
    let buildings = data.as_array().expect("extract_data validated an array");
    json!({
        "area": area.name(),
        "count": buildings.len(),
        "buildings": buildings,
    })
}

#[cfg(test)]
pub(super) fn get_balance(context: &mut Context<'_>, target: &MeterTarget<'_>) -> CliResult<Value> {
    balance_from_transport(context.transport, target)
}

#[cfg(test)]
fn balance_from_transport(
    transport: &mut dyn Transport,
    target: &MeterTarget<'_>,
) -> CliResult<Value> {
    let area_id = target.area_id.to_string();
    let payload = post(
        transport,
        "/search",
        &[
            ("partmentId", target.partment_id),
            ("floorId", target.floor_id),
            ("dromNumber", target.meter_number),
            ("areaid", area_id.as_str()),
        ],
    )?;
    balance_output_from_payload(&payload, target)
}

pub(super) fn get_balance_with_session(
    driver: &mut dyn SessionDriver,
    target: &MeterTarget<'_>,
) -> CliResult<Value> {
    let area_id = target.area_id.to_string();
    let data = post_with_session(
        driver,
        Route::electricity_balance(),
        "/search",
        &[
            ("partmentId", target.partment_id),
            ("floorId", target.floor_id),
            ("dromNumber", target.meter_number),
            ("areaid", area_id.as_str()),
        ],
        Expected::Object,
    )?;
    Ok(balance_output_from_data(&data, target))
}

#[cfg(test)]
fn balance_output_from_payload(payload: &Value, target: &MeterTarget<'_>) -> CliResult<Value> {
    let data = extract_data(payload, "/search", Expected::Object)?;
    Ok(balance_output_from_data(data, target))
}

fn balance_output_from_data(data: &Value, target: &MeterTarget<'_>) -> Value {
    let object = data.as_object().expect("extract_data validated an object");
    balance_output(object, target)
}

fn post(transport: &mut dyn Transport, path: &str, fields: &[(&str, &str)]) -> CliResult<Value> {
    let response = transport
        .send(Request {
            method: "POST",
            url: request_url(path),
            headers: vec![
                ("Referer".into(), REFERER.into()),
                ("X-Requested-With".into(), "XMLHttpRequest".into()),
                (
                    "Content-Type".into(),
                    "application/x-www-form-urlencoded".into(),
                ),
            ],
            body: form_body(fields),
        })
        .map_err(|error| {
            if error.kind == "network" {
                CliError::new(
                    4,
                    "network",
                    format!("electricity {path} HTTPS request failed"),
                    "app.bupt.edu.cn must be reachable; try `bupt diagnose`",
                )
            } else {
                CliError::new(
                    error.code,
                    error.kind,
                    format!("electricity {path} request failed"),
                    "",
                )
            }
        })?;
    check_http(&response, path)?;
    let body = String::from_utf8_lossy(&response.body);
    serde_json::from_str(&body).map_err(|_| parse_error(path, "returned invalid JSON"))
}

/// Retry one `/post` only after a confirmed auth failure, mirroring Python's
/// invalidate-and-retry recursion.
fn post_with_session(
    driver: &mut dyn SessionDriver,
    route: Route,
    path: &str,
    fields: &[(&str, &str)],
    expected: Expected,
) -> CliResult<Value> {
    let mut renewed = false;
    loop {
        let result = {
            let mut transport = ScopedTransport::new(driver, route);
            post(&mut transport, path, fields).and_then(|payload| {
                let data = extract_data(&payload, path, expected)?;
                Ok(data.clone())
            })
        };
        match result {
            Err(error) if !renewed && error.kind == "auth" => {
                driver.renew(Service::Electricity)?;
                renewed = true;
            }
            result => return result,
        }
    }
}

fn get_floors_with_session(
    driver: &mut dyn SessionDriver,
    partment_id: &str,
    area: Area,
) -> CliResult<Vec<Value>> {
    let area_id = area_id(area).to_string();
    let data = post_with_session(
        driver,
        Route::electricity_floors(),
        "/floor",
        &[("partmentId", partment_id), ("areaid", area_id.as_str())],
        Expected::Array,
    )?;
    Ok(data
        .as_array()
        .expect("extract_data validated an array")
        .clone())
}

fn get_rooms_with_session(
    driver: &mut dyn SessionDriver,
    partment_id: &str,
    floor_id: &str,
    area: Area,
) -> CliResult<Vec<Value>> {
    let area_id = area_id(area).to_string();
    let data = post_with_session(
        driver,
        Route::electricity_rooms(),
        "/drom",
        &[
            ("partmentId", partment_id),
            ("floorId", floor_id),
            ("areaid", area_id.as_str()),
        ],
        Expected::Array,
    )?;
    Ok(data
        .as_array()
        .expect("extract_data validated an array")
        .clone())
}

/// Resolve the caller's dorm through the Python request sequence:
/// `/floor` -> `/drom` -> `/search`. Metadata caching is intentionally omitted
/// because a CLI invocation performs one query; the session may still cache at
/// the HTTP layer.
pub(super) fn query_with_session(
    driver: &mut dyn SessionDriver,
    dorm: &Dorm,
    threshold: f64,
) -> CliResult<Value> {
    let building = dorm.building;
    let partment_id = building.partment_id();
    let area = building.area();
    let area_id = building.area_id();
    let floor_name = resolved_floor_name(*building, dorm.floor.as_deref(), &dorm.room);

    let floors = get_floors_with_session(driver, partment_id, area)?;
    let mut floor_id = String::new();
    for floor in &floors {
        let Some(object) = floor.as_object() else {
            return Err(parse_error(
                "/floor",
                "field 'd.data' contains a non-object item",
            ));
        };
        let name = object
            .get("floorName")
            .and_then(Value::as_str)
            .unwrap_or("");
        if name == floor_name {
            floor_id = string_value(object.get("floorId"));
        }
    }
    if floor_id.is_empty() {
        return Err(CliError::new(
            5,
            "not_found",
            format!(
                "electricity floor '{floor_name}' was not found for building '{}'",
                building.name
            ),
            "check the room/floor or run `bupt electricity buildings`",
        ));
    }

    let rooms = get_rooms_with_session(driver, partment_id, &floor_id, area)?;
    let mut target_room = None;
    let mut best_score = 0;
    for room in &rooms {
        let Some(object) = room.as_object() else {
            return Err(parse_error(
                "/drom",
                "field 'd.data' contains a non-object item",
            ));
        };
        let score = room_match_score(
            &string_value(object.get("dromName")),
            &string_value(object.get("dromNum")),
            &dorm.room,
        );
        if score > best_score {
            target_room = Some(object);
            best_score = score;
        }
    }
    let Some(target_room) = target_room else {
        return Err(CliError::new(
            5,
            "not_found",
            format!(
                "electricity room '{}' was not found on floor '{floor_name}' in '{}'",
                dorm.room, building.name
            ),
            "check the room number or run `bupt electricity buildings`",
        ));
    };

    let meter_number = string_value(target_room.get("dromNum"));
    if meter_number.is_empty() {
        let name = string_value(target_room.get("dromName"));
        let name = if name.is_empty() {
            dorm.room.as_str()
        } else {
            name.as_str()
        };
        return Err(parse_error(
            "/drom",
            &format!("room '{name}' is missing its meter number"),
        ));
    }
    let room_name = {
        let name = string_value(target_room.get("dromName"));
        if name.is_empty() {
            dorm.room.clone()
        } else {
            name
        }
    };
    let target = MeterTarget {
        partment_id,
        floor_id: &floor_id,
        meter_number: &meter_number,
        area_id,
        building: building.name,
        floor: &floor_name,
        room: &room_name,
        threshold,
        source: dorm.source,
    };
    get_balance_with_session(driver, &target)
}

fn resolved_floor_name(building: Building, explicit: Option<&str>, room: &str) -> String {
    if let Some(floor) = explicit {
        let mut floor = floor.trim().to_string();
        if building.area_id() == 2
            && !floor.is_empty()
            && !floor.ends_with('层')
            && floor.chars().all(|character| character.is_ascii_digit())
        {
            floor.push('层');
        }
        return floor;
    }
    infer_floor_name(room, building.default_floors(), building.area_id())
}

fn infer_floor_name(room: &str, floors: &[&str], area_id: i32) -> String {
    let room_digits = room
        .split(|character: char| !character.is_ascii_digit())
        .rfind(|part| !part.is_empty());
    let floor_num = match room_digits {
        Some(digits) if digits.len() >= 3 => &digits[..digits.len() - 2],
        Some(digits) => digits,
        None => {
            return floors
                .first()
                .map(|floor| (*floor).to_string())
                .unwrap_or_else(|| {
                    if area_id == 2 {
                        "1层".into()
                    } else {
                        "1".into()
                    }
                });
        }
    };
    let suffix = if area_id == 2 { "层" } else { "" };
    let target = format!("{floor_num}{suffix}");
    for floor in floors {
        if *floor == target || floor.replace('层', "") == floor_num {
            return (*floor).to_string();
        }
    }
    floors
        .first()
        .map(|floor| (*floor).to_string())
        .unwrap_or_else(|| {
            if area_id == 2 {
                "1层".into()
            } else {
                "1".into()
            }
        })
}

fn normalize_room_key(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_whitespace() && !matches!(character, '_' | '-'))
        .flat_map(char::to_uppercase)
        .collect()
}

fn room_match_score(room_name: &str, room_number: &str, query: &str) -> u8 {
    let normalized_query = normalize_room_key(query);
    if normalized_query.is_empty() {
        return 0;
    }
    let candidates = [
        normalize_room_key(room_name),
        normalize_room_key(room_number),
    ]
    .into_iter()
    .filter(|candidate| !candidate.is_empty())
    .collect::<Vec<_>>();
    if candidates.contains(&normalized_query) {
        return 3;
    }
    if candidates.iter().any(|candidate| {
        candidate.ends_with(&normalized_query) || normalized_query.ends_with(candidate)
    }) {
        return 2;
    }
    if candidates
        .iter()
        .any(|candidate| candidate.contains(&normalized_query))
    {
        return 1;
    }
    0
}

fn string_value(value: Option<&Value>) -> String {
    match value {
        Some(value) if python_truthy(value) => python_string(value),
        _ => String::new(),
    }
}

fn check_http(response: &Response, path: &str) -> CliResult<()> {
    // 3xx is left to body parsing; only explicit 401/403 is auth. Do not infer
    // a login redirect or accept a new destination here.
    if response.status == 401 || response.status == 403 {
        return Err(auth_error(format!(
            "electricity {path} authentication failed: HTTP {}",
            response.status
        )));
    }
    if response.status >= 400 {
        return Err(CliError::new(
            5,
            "upstream",
            format!("electricity {path} failed: HTTP {}", response.status),
            "retry later; if it persists, run `bupt diagnose`",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Expected {
    Object,
    Array,
}

fn extract_data<'a>(payload: &'a Value, path: &str, expected: Expected) -> CliResult<&'a Value> {
    let object = payload
        .as_object()
        .ok_or_else(|| parse_error(path, "response must be a JSON object"))?;
    let status = object
        .get("e")
        .ok_or_else(|| parse_error(path, "response missing status field 'e'"))?;
    let status = python_string(status);
    let message = object
        .get("m")
        .filter(|value| python_truthy(value))
        .map(python_string)
        .unwrap_or_default();

    if AUTH_MARKERS.iter().any(|marker| message.contains(marker)) {
        return Err(auth_error(format!(
            "electricity {path} authentication failed: {message}"
        )));
    }

    if status != "0" {
        let detail = if message.is_empty() {
            format!("code {status}")
        } else {
            message
        };
        if NOT_FOUND_MARKERS
            .iter()
            .any(|marker| detail.contains(marker))
        {
            return Err(CliError::new(
                5,
                "not_found",
                format!("electricity {path} resource not found: {detail}"),
                "check `bupt electricity buildings` and the room number",
            ));
        }
        return Err(CliError::new(
            4,
            "network",
            format!("electricity {path} failed: {detail}"),
            "retry later; if it persists, run `bupt diagnose`",
        ));
    }

    let data = object
        .get("d")
        .and_then(Value::as_object)
        .and_then(|envelope| envelope.get("data"))
        .ok_or_else(|| parse_error(path, "response missing object 'd.data'"))?;
    match (expected, data) {
        (Expected::Object, Value::Object(_)) | (Expected::Array, Value::Array(_)) => Ok(data),
        (Expected::Object, _) => Err(parse_error(path, "field 'd.data' must be a JSON object")),
        (Expected::Array, _) => Err(parse_error(path, "field 'd.data' must be a JSON array")),
    }
}

fn balance_output(data: &Map<String, Value>, target: &MeterTarget<'_>) -> Value {
    let surplus = nonzero_float(data.get("surplus")).unwrap_or(0.0);
    let price = nonzero_float(data.get("price")).unwrap_or(0.48);
    let total_source = data
        .get("vTotal")
        .filter(|value| python_truthy(value))
        .or_else(|| data.get("freeEnd"));
    let total_consumed = float_or_none(total_source);
    let remaining = if price <= 0.0 {
        0.0
    } else {
        round2(surplus / price)
    };
    let meter_time = data.get("time").cloned().unwrap_or(Value::Null);

    json!({
        "building": value_or(data.get("parName"), target.building),
        "floor": value_or(data.get("floorName"), target.floor),
        "room": target.room,
        "surplus": surplus,
        "price": price,
        "remaining_kwh": remaining,
        "total_consumed_kwh": total_consumed,
        "meter_time": meter_time,
        "low_balance": surplus < target.threshold,
        "threshold": target.threshold,
        "source": target.source,
    })
}

fn float_or_none(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(number) => number.as_f64().filter(|value| value.is_finite()),
        Value::String(text) => {
            let text = text.trim();
            if text.is_empty() {
                None
            } else {
                text.parse::<f64>().ok().filter(|value| value.is_finite())
            }
        }
        Value::Bool(value) => Some(if *value { 1.0 } else { 0.0 }),
        _ => None,
    }
}

fn nonzero_float(value: Option<&Value>) -> Option<f64> {
    float_or_none(value).filter(|value| *value != 0.0)
}

fn value_or(value: Option<&Value>, fallback: &str) -> Value {
    match value {
        Some(value) if python_truthy(value) => value.clone(),
        _ => Value::String(fallback.to_string()),
    }
}

fn python_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_f64().is_some_and(|value| value != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(values) => !values.is_empty(),
        Value::Object(values) => !values.is_empty(),
    }
}

fn python_string(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn round2(value: f64) -> f64 {
    // Fixed formatting follows Python's decimal tie-to-even behavior for the
    // binary value; multiplying and calling round() can differ on ties.
    if value.is_finite() {
        format!("{value:.2}").parse::<f64>().unwrap_or(value)
    } else {
        value
    }
}

fn area_id(area: Area) -> i32 {
    match area {
        Area::Shahe => 2,
        Area::Xitucheng => 1,
    }
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

fn auth_error(message: String) -> CliError {
    CliError::new(3, "auth", message, "run `bupt login`")
}

fn parse_error(path: &str, detail: &str) -> CliError {
    CliError::new(
        5,
        "parse",
        format!("electricity {path} {detail}"),
        "the electricity response format may have changed; retry or run `bupt diagnose`",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::testing::Scripted;
    use std::fs;
    use std::path::{Path, PathBuf};
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
            transport,
        }
    }

    fn target() -> MeterTarget<'static> {
        MeterTarget {
            partment_id: "fixture-partment",
            floor_id: "fixture-floor-1",
            meter_number: "fixture-meter-101",
            area_id: 1,
            building: "fallback-building",
            floor: "fallback-floor",
            room: "101",
            threshold: 10.0,
            source: "default",
        }
    }

    #[test]
    fn buildings_maps_area_count_and_response_items() {
        let mut transport = Scripted::default();
        transport.expected.push_back((
            "POST",
            format!("{ELECTRICITY_BASE}/part"),
            response(200, fixture("electricity_buildings_shahe.json")),
        ));
        transport.expected.push_back((
            "POST",
            format!("{ELECTRICITY_BASE}/part"),
            response(200, fixture("electricity_buildings_empty.json")),
        ));

        let shahe = {
            let mut context = test_context(&mut transport);
            get_buildings(&mut context, Area::Shahe).unwrap()
        };
        let xitucheng = {
            let mut context = test_context(&mut transport);
            get_buildings(&mut context, Area::Xitucheng).unwrap()
        };

        assert_eq!(shahe["area"], "shahe");
        assert_eq!(shahe["count"], 2);
        assert_eq!(shahe["buildings"][0]["partmentName"], "沙河校区雁北园A楼");
        assert_eq!(shahe["buildings"][0]["partmentId"], "fixture-shahe-a");
        assert_eq!(shahe["buildings"][0]["areaId"], 2);
        assert_eq!(xitucheng["area"], "xitucheng");
        assert_eq!(xitucheng["count"], 0);
        assert_eq!(xitucheng["buildings"], json!([]));
        assert_eq!(
            String::from_utf8_lossy(&transport.calls[0].body),
            "areaid=2"
        );
        assert_eq!(
            String::from_utf8_lossy(&transport.calls[1].body),
            "areaid=1"
        );
        assert!(
            transport.calls[0]
                .headers
                .contains(&("Referer".into(), REFERER.into()))
        );
    }

    #[test]
    fn balance_maps_fixture_fields_and_response_overrides() {
        let mut transport = Scripted::default();
        transport.expected.push_back((
            "POST",
            format!("{ELECTRICITY_BASE}/search"),
            response(200, fixture("electricity_meter.json")),
        ));

        let data = {
            let mut context = test_context(&mut transport);
            get_balance(&mut context, &target()).unwrap()
        };

        assert_eq!(data["building"], "学一楼");
        assert_eq!(data["floor"], "1");
        assert_eq!(data["room"], "101");
        assert_eq!(data["surplus"], 42.5);
        assert_eq!(data["price"], 0.48);
        assert_eq!(data["remaining_kwh"], 88.54);
        assert_eq!(data["total_consumed_kwh"], 88.54);
        assert_eq!(data["meter_time"], "2026-10-21 03:10:51");
        assert_eq!(data["low_balance"], false);
        assert_eq!(data["threshold"], 10.0);
        assert_eq!(data["source"], "default");
        assert_eq!(
            String::from_utf8_lossy(&transport.calls[0].body),
            "partmentId=fixture-partment&floorId=fixture-floor-1&dromNumber=fixture-meter-101&areaid=1"
        );
        assert!(
            transport.calls[0]
                .headers
                .contains(&("X-Requested-With".into(), "XMLHttpRequest".into()))
        );
    }

    #[test]
    fn balance_keeps_empty_and_zero_values_compatible() {
        let mut transport = Scripted::default();
        transport.expected.push_back((
            "POST",
            format!("{ELECTRICITY_BASE}/search"),
            response(200, fixture("electricity_meter_empty.json")),
        ));
        let target = MeterTarget {
            building: "学五楼",
            floor: "2",
            room: "202",
            threshold: 50.0,
            source: "config",
            ..target()
        };

        let data = {
            let mut context = test_context(&mut transport);
            get_balance(&mut context, &target).unwrap()
        };

        assert_eq!(data["building"], "学五楼");
        assert_eq!(data["floor"], "2");
        assert_eq!(data["room"], "202");
        assert_eq!(data["surplus"], 0.0);
        assert_eq!(data["price"], 0.48);
        assert_eq!(data["remaining_kwh"], 0.0);
        assert_eq!(data["total_consumed_kwh"], 12.5);
        assert_eq!(data["meter_time"], "");
        assert_eq!(data["low_balance"], true);
        assert_eq!(data["threshold"], 50.0);
        assert_eq!(data["source"], "config");
    }

    #[test]
    fn confirmed_login_markers_are_auth_errors() {
        for marker in AUTH_MARKERS {
            let mut transport = Scripted::default();
            transport.expected.push_back((
                "POST",
                format!("{ELECTRICITY_BASE}/part"),
                response(200, format!(r#"{{"e":1,"m":"{marker}","d":{{}}}}"#)),
            ));
            let error = {
                let mut context = test_context(&mut transport);
                get_buildings(&mut context, Area::Shahe).unwrap_err()
            };
            assert_eq!(error.code, 3, "marker {marker}");
            assert_eq!(error.kind, "auth", "marker {marker}");
            assert!(error.hint.contains("login"), "marker {marker}");
        }
    }

    #[test]
    fn nonzero_status_without_auth_marker_keeps_python_classification() {
        let mut transport = Scripted::default();
        transport.expected.push_back((
            "POST",
            format!("{ELECTRICITY_BASE}/part"),
            response(200, fixture("electricity_upstream_error.json")),
        ));
        let error = {
            let mut context = test_context(&mut transport);
            get_buildings(&mut context, Area::Shahe).unwrap_err()
        };
        assert_eq!(error.code, 4);
        assert_eq!(error.kind, "network");

        for (message, code, kind) in [("请登录", 4, "network"), ("无数据", 5, "not_found")] {
            let mut transport = Scripted::default();
            transport.expected.push_back((
                "POST",
                format!("{ELECTRICITY_BASE}/part"),
                response(200, format!(r#"{{"e":1,"m":"{message}","d":{{}}}}"#)),
            ));
            let error = {
                let mut context = test_context(&mut transport);
                get_buildings(&mut context, Area::Shahe).unwrap_err()
            };
            assert_eq!(error.code, code, "message {message}");
            assert_eq!(error.kind, kind, "message {message}");
        }
    }

    #[test]
    fn malformed_payloads_are_parse_errors() {
        for (payload, expected) in [
            (json!([]), Expected::Array),
            (
                json!({"m": "missing e", "d": {"data": []}}),
                Expected::Array,
            ),
            (json!({"e": 0, "m": "", "d": {}}), Expected::Array),
            (
                json!({"e": 0, "m": "", "d": {"data": "not-a-container"}}),
                Expected::Array,
            ),
            (
                json!({"e": 0, "m": "", "d": {"data": []}}),
                Expected::Object,
            ),
        ] {
            let error = extract_data(&payload, "/part", expected).unwrap_err();
            assert_eq!(error.code, 5);
            assert_eq!(error.kind, "parse");
            assert!(!error.message.is_empty());
        }
    }

    #[test]
    fn electricity_transport_error_preserves_the_layer_and_route_without_diagnostics() {
        let mut transport = Scripted::default();
        transport.expected.push_back((
            "POST",
            request_url("/floor"),
            Err(CliError::new(
                5,
                "upstream",
                "TEST-ONLY-INTERNAL",
                "TEST-ONLY-HINT",
            )),
        ));
        let err = post(
            &mut transport,
            "/floor",
            &[("partmentId", "fixture"), ("areaid", "1")],
        )
        .unwrap_err();
        assert_eq!((err.code, err.kind), (5, "upstream"));
        assert_eq!(err.message, "electricity /floor request failed");
        assert!(err.hint.is_empty());
    }

    #[test]
    fn invalid_json_is_a_parse_error() {
        let mut transport = Scripted::default();
        transport.expected.push_back((
            "POST",
            format!("{ELECTRICITY_BASE}/part"),
            response(200, "not json".into()),
        ));
        let error = {
            let mut context = test_context(&mut transport);
            get_buildings(&mut context, Area::Shahe).unwrap_err()
        };
        assert_eq!(error.code, 5);
        assert_eq!(error.kind, "parse");
    }
    struct TestSession {
        transport: Scripted,
        routes: Vec<Route>,
        renew_calls: usize,
    }

    impl TestSession {
        fn new() -> Self {
            Self {
                transport: Scripted::default(),
                routes: Vec::new(),
                renew_calls: 0,
            }
        }

        fn queue(&mut self, url: String, body: String) {
            self.transport
                .expected
                .push_back(("POST", url, response(200, body)));
        }
    }

    impl SessionDriver for TestSession {
        fn ensure(&mut self, _: Service) -> CliResult<()> {
            Ok(())
        }

        fn send(&mut self, route: Route, request: Request) -> CliResult<Response> {
            self.routes.push(route);
            self.transport.send(request)
        }

        fn renew(&mut self, _: Service) -> CliResult<()> {
            self.renew_calls += 1;
            Ok(())
        }
    }

    fn dorm(building: &str, room: &str, floor: Option<&str>) -> Dorm {
        let flags = super::super::dorm::DormFlags {
            building: Some(building.into()),
            room: Some(room.into()),
            floor: floor.map(str::to_string),
        };
        super::super::dorm::resolve_dorm(
            &flags,
            &super::super::dorm::DormEnv::default(),
            Path::new("/bupt-test/no-config"),
        )
        .expect("resolve test dorm")
    }

    #[test]
    fn floor_inference_and_explicit_suffix_match_python_examples() {
        let xue1 = dorm("学一楼", "101", None);
        assert_eq!(resolved_floor_name(*xue1.building, None, "101"), "1");
        assert_eq!(resolved_floor_name(*xue1.building, None, "1204"), "-1");
        assert_eq!(resolved_floor_name(*xue1.building, Some(" 2 "), "101"), "2");

        let shahe = dorm("沙河校区雁北园A楼", "1204", None);
        assert_eq!(resolved_floor_name(*shahe.building, None, "1204"), "1层");
        assert_eq!(
            resolved_floor_name(*shahe.building, Some("2"), "101"),
            "2层"
        );
        assert_eq!(
            resolved_floor_name(*shahe.building, Some("2层"), "101"),
            "2层"
        );
        assert_eq!(
            resolved_floor_name(*shahe.building, Some("强电"), "101"),
            "强电"
        );

        let xue29 = dorm("学29楼", "1204", None);
        assert_eq!(resolved_floor_name(*xue29.building, None, "1204"), "1");
        let no_digits = dorm("学29楼", "A", None);
        assert_eq!(resolved_floor_name(*no_digits.building, None, "A"), "1");
    }

    #[test]
    fn room_scores_are_exact_then_suffix_then_containment() {
        assert_eq!(room_match_score("A-101", "M-101", "a101"), 3);
        assert_eq!(room_match_score("Room-101", "M-102", "101"), 2);
        assert_eq!(room_match_score("A-1101", "M-102", "101"), 2);
        assert_eq!(room_match_score("A-1010", "M-102", "101"), 1);
        assert_eq!(room_match_score("A-101", "M-101", ""), 0);
        assert_eq!(room_match_score("A-101", "M-101", "202"), 0);
        assert_eq!(room_match_score("A 101", "M-102", "a_101"), 3);
    }

    #[test]
    fn query_rejects_a_floor_name_that_is_not_exactly_present() {
        let mut session = TestSession::new();
        session.queue(request_url("/floor"), fixture("electricity_floors.json"));
        let target = dorm("学一楼", "101", None);
        let error = query_with_session(&mut session, &target, 10.0).unwrap_err();
        assert_eq!((error.code, error.kind), (5, "not_found"));
        assert!(error.message.contains("'1'"));
        assert_eq!(session.routes.len(), 1);
    }

    #[test]
    fn query_rejects_a_room_without_a_match_after_floor_and_room_requests() {
        let mut session = TestSession::new();
        session.queue(
            request_url("/floor"),
            fixture("electricity_floors_xitucheng.json"),
        );
        session.queue(request_url("/drom"), fixture("electricity_rooms.json"));
        let target = dorm("学一楼", "103", None);
        let error = query_with_session(&mut session, &target, 10.0).unwrap_err();
        assert_eq!((error.code, error.kind), (5, "not_found"));
        assert!(error.message.contains("'103'"), "message={}", error.message);
        assert_eq!(session.routes.len(), 2);
    }

    #[test]
    fn query_fails_honestly_on_non_object_floor_or_room_items() {
        let mut session = TestSession::new();
        session.queue(
            request_url("/floor"),
            r#"{"e":0,"m":"","d":{"data":["not-an-object"]}}"#.into(),
        );
        let target = dorm("学一楼", "101", None);
        let error = query_with_session(&mut session, &target, 10.0).unwrap_err();
        assert_eq!((error.code, error.kind), (5, "parse"));
        assert!(error.message.contains("/floor"));
        assert_eq!(session.routes.len(), 1);

        let mut session = TestSession::new();
        session.queue(
            request_url("/floor"),
            fixture("electricity_floors_xitucheng.json"),
        );
        session.queue(
            request_url("/drom"),
            r#"{"e":0,"m":"","d":{"data":["not-an-object"]}}"#.into(),
        );
        let error = query_with_session(&mut session, &target, 10.0).unwrap_err();
        assert_eq!((error.code, error.kind), (5, "parse"));
        assert!(error.message.contains("/drom"));
        assert_eq!(session.routes.len(), 2);
    }

    #[test]
    fn query_rejects_a_room_without_a_meter_number() {
        let mut session = TestSession::new();
        session.queue(
            request_url("/floor"),
            fixture("electricity_floors_xitucheng.json"),
        );
        session.queue(
            request_url("/drom"),
            r#"{"e":0,"m":"","d":{"data":[{"dromName":"101","dromNum":""}]}}"#.into(),
        );
        let target = dorm("学一楼", "101", None);
        let error = query_with_session(&mut session, &target, 10.0).unwrap_err();
        assert_eq!((error.code, error.kind), (5, "parse"));
        assert!(error.message.contains("meter number"));
        assert_eq!(session.routes.len(), 2);
    }
}
