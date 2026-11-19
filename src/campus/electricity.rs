use super::dorm::Area;
use crate::cli::{CliError, CliResult, Context};
use crate::transport::{Request, Response};
use serde_json::{Map, Value, json};

const ELECTRICITY_BASE: &str = "https://app.bupt.edu.cn/buptdf/wap/default";
const ELECTRICITY_REFERER: &str = "https://app.bupt.edu.cn/buptdf/wap/default/chong";
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

#[allow(dead_code)] // exercised offline; wired when the session/room resolver lands
pub(super) fn get_buildings(context: &mut Context<'_>, area: Area) -> CliResult<Value> {
    let area_id = area_id(area).to_string();
    let payload = post(context, "/part", &[("areaid", area_id.as_str())])?;
    let data = extract_data(&payload, "/part", Expected::Array)?;
    let buildings = data.as_array().expect("extract_data validated an array");
    Ok(json!({
        "area": area.name(),
        "count": buildings.len(),
        "buildings": buildings,
    }))
}

#[allow(dead_code)] // exercised offline; wired when the session/room resolver lands
pub(super) fn get_balance(context: &mut Context<'_>, target: &MeterTarget<'_>) -> CliResult<Value> {
    let area_id = target.area_id.to_string();
    let payload = post(
        context,
        "/search",
        &[
            ("partmentId", target.partment_id),
            ("floorId", target.floor_id),
            ("dromNumber", target.meter_number),
            ("areaid", area_id.as_str()),
        ],
    )?;
    let data = extract_data(&payload, "/search", Expected::Object)?;
    let object = data.as_object().expect("extract_data validated an object");
    Ok(balance_output(object, target))
}

fn post(context: &mut Context<'_>, path: &str, fields: &[(&str, &str)]) -> CliResult<Value> {
    let response = context
        .transport
        .send(Request {
            method: "POST",
            url: format!("{ELECTRICITY_BASE}{path}"),
            headers: vec![
                ("Referer".into(), ELECTRICITY_REFERER.into()),
                ("X-Requested-With".into(), "XMLHttpRequest".into()),
                (
                    "Content-Type".into(),
                    "application/x-www-form-urlencoded".into(),
                ),
            ],
            body: form_body(fields),
        })
        .map_err(|_| {
            CliError::new(
                4,
                "network",
                "electricity request failed",
                "app.bupt.edu.cn must be reachable; try `bupt diagnose`",
            )
        })?;
    check_http(&response, path)?;
    let body = String::from_utf8_lossy(&response.body);
    serde_json::from_str(&body).map_err(|_| parse_error(path, "returned invalid JSON"))
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
            timeout: Duration::from_secs(20),
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
                .contains(&("Referer".into(), ELECTRICITY_REFERER.into()))
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
}
