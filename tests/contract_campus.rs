mod contract_support;

use contract_support::{Invocation, Sandbox};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;

const SCHEMA: &str = "bupt.v1";

fn config_path(sandbox: &Sandbox) -> PathBuf {
    sandbox.root().join("config/config.json")
}

fn credentials_path(sandbox: &Sandbox) -> PathBuf {
    sandbox.root().join("config/credentials.json")
}

fn write_config(sandbox: &Sandbox, contents: &str) {
    fs::write(config_path(sandbox), contents).expect("write synthetic config");
}

fn write_malformed_credentials(sandbox: &Sandbox) {
    fs::write(credentials_path(sandbox), "{not valid json")
        .expect("write synthetic malformed credentials");
}

fn run_with_env(sandbox: &Sandbox, args: &[&str], env: &[(&str, &str)]) -> Invocation {
    let mut command = sandbox.command();
    for (name, value) in env {
        command.env(name, value);
    }
    Invocation(
        command
            .args(args)
            .output()
            .expect("run isolated CLI with synthetic environment"),
    )
}

fn error_message(payload: &Value) -> &str {
    payload["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("missing error.message in {payload}"))
}

fn error_hint(payload: &Value) -> &str {
    payload["error"]["hint"].as_str().unwrap_or("")
}

fn assert_error_with_context(
    invocation: Invocation,
    expected_code: i32,
    expected_type: &str,
    context: &str,
) -> Value {
    let stdout = String::from_utf8_lossy(&invocation.0.stdout);
    assert!(
        invocation.0.stdout.is_empty(),
        "{context}: stdout must stay empty on errors, got {stdout:?}"
    );

    let payload = invocation.stderr_json();
    assert_eq!(
        invocation.code(),
        expected_code,
        "{context}: wrong exit code; stderr={payload}"
    );
    assert_eq!(
        payload["ok"],
        Value::Bool(false),
        "{context}: missing ok=false; stderr={payload}"
    );
    assert_eq!(
        payload["schema"], SCHEMA,
        "{context}: error envelope must carry schema={SCHEMA}; stderr={payload}"
    );
    assert_eq!(
        payload["error"]["type"], expected_type,
        "{context}: wrong error type; stderr={payload}"
    );
    assert!(
        !error_message(&payload).is_empty(),
        "{context}: error message must not be empty; stderr={payload}"
    );
    payload
}

fn assert_error(invocation: Invocation, expected_code: i32, expected_type: &str) -> Value {
    assert_error_with_context(invocation, expected_code, expected_type, "command")
}

fn assert_no_session_file(sandbox: &Sandbox) {
    let path = sandbox.root().join("cache/session.json");
    assert!(
        !path.exists(),
        "unexpected session cache at {}",
        path.display()
    );
}

// ---------------------------------------------------------------------------
// Library OPAC: anonymous input validation. Response parsing is covered by the
// in-process Scripted transport tests in src/campus/library.rs; process-level success
// output still needs an integrator-agreed transport seam (see migration notes).
// ---------------------------------------------------------------------------

#[test]
fn library_search_rejects_blank_keyword_without_reading_credentials() {
    let sandbox = Sandbox::new();
    write_malformed_credentials(&sandbox);

    for keyword in ["", "   ", "\t\n"] {
        let payload = assert_error_with_context(
            sandbox.run(&["library", "search", keyword]),
            2,
            "usage",
            "blank library keyword",
        );
        assert!(
            !error_message(&payload).is_empty(),
            "blank keyword error must stay actionable; stderr={payload}"
        );
        assert_no_session_file(&sandbox);
    }
}

#[test]
fn library_hold_and_ebook_reject_blank_ids_without_reading_credentials() {
    let sandbox = Sandbox::new();
    write_malformed_credentials(&sandbox);

    for command in ["hold", "ebook"] {
        for id in ["", "   ", "\t\n"] {
            let payload = assert_error_with_context(
                sandbox.run(&["library", command, id]),
                2,
                "usage",
                &format!("blank id for library {command}"),
            );
            assert!(
                error_message(&payload).contains("recCtrlId"),
                "blank id error should name recCtrlId; stderr={payload}"
            );
            assert_no_session_file(&sandbox);
        }
    }
}

#[test]
fn library_requires_explicit_subcommands_and_arguments() {
    let sandbox = Sandbox::new();
    write_malformed_credentials(&sandbox);

    for args in [
        vec!["library"],
        vec!["library", "search"],
        vec!["library", "hold"],
        vec!["library", "ebook"],
    ] {
        assert_error_with_context(
            sandbox.run(&args),
            2,
            "usage",
            &format!("missing argument for {args:?}"),
        );
        assert_no_session_file(&sandbox);
    }
}

#[test]
fn library_rejects_invalid_pagination() {
    let sandbox = Sandbox::new();
    write_malformed_credentials(&sandbox);

    for args in [
        vec!["library", "search", "示例", "--page", "0"],
        vec!["library", "search", "示例", "--page", "-1"],
        vec!["library", "search", "示例", "--page", "nope"],
        vec!["library", "search", "示例", "--page-size", "0"],
        vec!["library", "search", "示例", "--page-size", "-1"],
        vec!["library", "search", "示例", "--page-size", "nope"],
        vec!["library", "search", "示例", "--timeout", "0"],
    ] {
        assert_error_with_context(
            sandbox.run(&args),
            2,
            "usage",
            &format!("invalid library pagination in {args:?}"),
        );
        assert_no_session_file(&sandbox);
    }
}

// ---------------------------------------------------------------------------
// Electricity: local precedence and validation. Response/request mapping is
// covered by Scripted tests in src/campus/electricity.rs; full CLI success
// still needs the session/room resolver.
// ---------------------------------------------------------------------------

#[test]
fn electricity_default_placeholder_needs_a_campus_session() {
    let sandbox = Sandbox::new();

    let payload = assert_error(sandbox.run(&["electricity"]), 3, "auth");
    assert!(
        error_hint(&payload).contains("login"),
        "missing-credential hint should mention login; stderr={payload}"
    );
    assert_no_session_file(&sandbox);
}

#[test]
fn electricity_building_flag_requires_a_room_without_falling_back() {
    let sandbox = Sandbox::new();
    write_config(&sandbox, r#"{"dorm":{"room":"202","floor":"2"}}"#);
    let env = [("BUPT_DORM_ROOM", "414")];

    let payload = assert_error(
        run_with_env(&sandbox, &["electricity", "--building", "学五楼"], &env),
        2,
        "usage",
    );
    assert!(
        error_message(&payload).contains("--room"),
        "missing --room error should name the flag; stderr={payload}"
    );
    assert_no_session_file(&sandbox);
}

#[test]
fn electricity_rejects_unknown_building() {
    let sandbox = Sandbox::new();

    let payload = assert_error(
        sandbox.run(&["electricity", "--building", "不存在楼", "--room", "101"]),
        2,
        "usage",
    );
    assert!(
        error_message(&payload).contains("不存在楼"),
        "unknown-building error should quote the input; stderr={payload}"
    );
    assert!(
        error_hint(&payload).contains("electricity buildings"),
        "unknown-building hint should point at the building list; stderr={payload}"
    );
    assert_no_session_file(&sandbox);
}

#[test]
fn electricity_accepts_documented_building_aliases_before_auth() {
    let sandbox = Sandbox::new();

    for alias in [
        "学五楼",
        "学5楼",
        "学5",
        "5",
        "5楼",
        "学十一楼",
        "学29楼",
        "SHAHE_A",
        "shahe_a",
        "沙河校区雁北园A楼",
    ] {
        let payload = assert_error_with_context(
            sandbox.run(&["electricity", "--building", alias, "--room", "101"]),
            3,
            "auth",
            &format!("building alias {alias:?} should resolve locally"),
        );
        assert!(
            error_hint(&payload).contains("login"),
            "alias {alias:?} should reach credential loading; stderr={payload}"
        );
    }
    assert_no_session_file(&sandbox);
}

#[test]
fn electricity_environment_building_wins_over_config() {
    let sandbox = Sandbox::new();
    write_config(&sandbox, r#"{"dorm":{"building":"不存在楼","room":"202"}}"#);
    let env = [("BUPT_DORM_BUILDING", "学五楼"), ("BUPT_DORM_ROOM", "414")];

    assert_error(run_with_env(&sandbox, &["electricity"], &env), 3, "auth");
    assert_no_session_file(&sandbox);
}

#[test]
fn electricity_flag_building_wins_over_environment() {
    let sandbox = Sandbox::new();
    let env = [("BUPT_DORM_BUILDING", "学五楼"), ("BUPT_DORM_ROOM", "414")];

    let payload = assert_error(
        run_with_env(
            &sandbox,
            &["electricity", "--building", "不存在楼", "--room", "101"],
            &env,
        ),
        2,
        "usage",
    );
    assert!(
        error_message(&payload).contains("不存在楼"),
        "flag building should win before auth; stderr={payload}"
    );
}

#[test]
fn electricity_config_building_is_used_when_flags_and_env_are_absent() {
    let sandbox = Sandbox::new();
    write_config(&sandbox, r#"{"dorm":{"building":"学五楼","room":"202"}}"#);

    assert_error(sandbox.run(&["electricity"]), 3, "auth");

    // The default dorm would also reach auth. An invalid configured building
    // can only produce usage if config is actually read.
    write_config(&sandbox, r#"{"dorm":{"building":"不存在楼","room":"202"}}"#);
    let payload = assert_error(sandbox.run(&["electricity"]), 2, "usage");
    assert!(
        error_message(&payload).contains("不存在楼"),
        "configured building should be resolved from config; stderr={payload}"
    );
}

#[test]
fn electricity_rejects_legacy_area_on_the_query_command() {
    let sandbox = Sandbox::new();
    write_config(&sandbox, "{not valid json");

    let payload = assert_error(sandbox.run(&["electricity", "--area", "shahe"]), 2, "usage");
    assert!(
        error_message(&payload).contains("--area"),
        "top-level --area usage error should name the flag; stderr={payload}"
    );
    assert!(
        error_hint(&payload).contains("electricity buildings"),
        "--area hint should point at the buildings subcommand; stderr={payload}"
    );
}

#[test]
fn electricity_buildings_rejects_unknown_area() {
    let sandbox = Sandbox::new();
    write_config(&sandbox, "{not valid json");

    let payload = assert_error(
        sandbox.run(&["electricity", "buildings", "--area", "mars"]),
        2,
        "usage",
    );
    assert!(
        error_message(&payload).contains("--area"),
        "unknown area error should name --area; stderr={payload}"
    );
}

#[test]
fn electricity_buildings_ignores_malformed_dorm_config() {
    let sandbox = Sandbox::new();
    write_config(&sandbox, "{not valid json");

    for args in [
        vec!["electricity", "buildings"],
        vec!["electricity", "buildings", "--area", "shahe"],
        vec!["electricity", "buildings", "--area", "xitucheng"],
    ] {
        let payload = assert_error_with_context(
            sandbox.run(&args),
            3,
            "auth",
            &format!("buildings must ignore dorm config for {args:?}"),
        );
        assert!(
            error_hint(&payload).contains("login"),
            "missing credentials should be the winning failure; stderr={payload}"
        );
    }
}

#[test]
fn electricity_buildings_accepts_documented_area_aliases() {
    let sandbox = Sandbox::new();
    write_config(&sandbox, "{not valid json");

    for area in [
        "shahe",
        "沙河",
        "2",
        "xitucheng",
        "haidian",
        "西土城",
        "海淀",
        "1",
    ] {
        let payload = assert_error_with_context(
            sandbox.run(&["electricity", "buildings", "--area", area]),
            3,
            "auth",
            &format!("area alias {area:?} should resolve locally"),
        );
        assert!(
            error_hint(&payload).contains("login"),
            "area alias {area:?} should reach credential loading; stderr={payload}"
        );
    }
}

#[test]
fn electricity_rejects_invalid_thresholds() {
    let sandbox = Sandbox::new();

    for threshold in ["-1", "-0.01", "NaN", "nan", "inf", "-inf", "nope"] {
        assert_error_with_context(
            sandbox.run(&["electricity", "--threshold", threshold]),
            2,
            "usage",
            &format!("invalid threshold {threshold:?}"),
        );
    }
}

#[test]
fn electricity_accepts_zero_threshold_before_auth() {
    let sandbox = Sandbox::new();

    assert_error(sandbox.run(&["electricity", "--threshold", "0"]), 3, "auth");
}

#[test]
fn electricity_reports_malformed_config_as_usage_error() {
    let sandbox = Sandbox::new();
    write_config(&sandbox, "{not valid json");

    let payload = assert_error(sandbox.run(&["electricity"]), 2, "usage");
    assert!(
        error_hint(&payload).contains("JSON"),
        "malformed JSON hint should mention the format; stderr={payload}"
    );
    assert_no_session_file(&sandbox);
}

#[test]
fn electricity_rejects_non_object_config_and_dorm_sections() {
    let sandbox = Sandbox::new();

    for contents in [
        "[]",
        "\"text\"",
        "{\"dorm\":\"not-an-object\"}",
        "{\"dorm\":[]}",
    ] {
        write_config(&sandbox, contents);
        let payload = assert_error_with_context(
            sandbox.run(&["electricity"]),
            2,
            "usage",
            &format!("invalid config shape {contents}"),
        );
        assert!(
            !error_message(&payload).is_empty(),
            "config shape error must stay actionable; stderr={payload}"
        );
    }
}

#[test]
fn electricity_reports_unreadable_config_without_json_hint() {
    let sandbox = Sandbox::new();
    fs::create_dir(config_path(&sandbox)).expect("create directory where config.json is expected");

    let payload = assert_error(sandbox.run(&["electricity"]), 2, "usage");
    assert!(
        !error_message(&payload).contains("JSON") && !error_hint(&payload).contains("JSON"),
        "unreadable config must not be reported as a JSON syntax problem; stderr={payload}"
    );
}

// ---------------------------------------------------------------------------
// Notices and card: local validation and credential layering. Listing,
// detail parsing, and card widget parsing need the same offline response seam.
// ---------------------------------------------------------------------------

#[test]
fn notices_requires_credentials_without_creating_session() {
    let sandbox = Sandbox::new();

    let payload = assert_error(sandbox.run(&["notices"]), 3, "auth");
    assert!(
        error_hint(&payload).contains("login"),
        "notices auth hint should mention login; stderr={payload}"
    );
    assert_no_session_file(&sandbox);
}

#[test]
fn card_requires_credentials_without_creating_session() {
    let sandbox = Sandbox::new();

    let payload = assert_error(sandbox.run(&["card"]), 3, "auth");
    assert!(
        error_hint(&payload).contains("login"),
        "card auth hint should mention login; stderr={payload}"
    );
    assert_no_session_file(&sandbox);
}

#[test]
fn environment_only_credentials_are_ignored() {
    let sandbox = Sandbox::new();
    let password = "TEST-ONLY-CAMPUS-ENV-PASSWORD-7a9b";
    let payload = assert_error(
        run_with_env(
            &sandbox,
            &["card"],
            &[
                ("BUPT_USERNAME", "2026000098"),
                ("BUPT_PASSWORD", password),
                ("BUPT_ROLE", "graduate"),
            ],
        ),
        3,
        "auth",
    );
    assert!(
        error_hint(&payload).contains("login"),
        "environment credentials must not replace credentials.json; stderr={payload}"
    );
    assert!(!payload.to_string().contains(password));
    assert_no_session_file(&sandbox);
}

#[test]
fn malformed_saved_credentials_are_an_auth_error() {
    let sandbox = Sandbox::new();
    write_malformed_credentials(&sandbox);

    let payload = assert_error(sandbox.run(&["card"]), 3, "auth");
    assert!(
        error_hint(&payload).contains("login"),
        "malformed credentials should keep the auth hint; stderr={payload}"
    );
    assert!(!payload.to_string().contains("not valid json"));
    assert_no_session_file(&sandbox);
}

#[test]
fn saved_credentials_do_not_require_a_role() {
    let sandbox = Sandbox::new();
    fs::write(
        credentials_path(&sandbox),
        r#"{"username":"2026000096","password":"TEST-ONLY-CAMPUS-PASSWORD-2b3c"}"#,
    )
    .expect("write synthetic roleless credentials");

    assert_error(sandbox.run(&["card"]), 5, "upstream");
    assert_no_session_file(&sandbox);
}

#[test]
fn notices_rejects_unknown_types_before_auth() {
    let sandbox = Sandbox::new();
    write_malformed_credentials(&sandbox);

    for notice_type in ["bogus", "notice,bogus", "rules,unknown"] {
        let payload = assert_error_with_context(
            sandbox.run(&["notices", "--type", notice_type]),
            2,
            "usage",
            &format!("invalid notice type {notice_type:?}"),
        );
        assert!(
            error_hint(&payload).contains("notice"),
            "notice type hint should list valid categories; stderr={payload}"
        );
    }
    assert_no_session_file(&sandbox);
}

#[test]
fn notices_accepts_case_insensitive_types_before_auth() {
    let sandbox = Sandbox::new();

    for notice_type in ["NOTICE", "News", "notice,news,rules,ways"] {
        assert_error_with_context(
            sandbox.run(&["notices", "--type", notice_type, "--count", "1"]),
            3,
            "auth",
            &format!("valid notice type {notice_type:?}"),
        );
    }
}

#[test]
fn notices_rejects_invalid_count_as_usage_error() {
    let sandbox = Sandbox::new();
    write_malformed_credentials(&sandbox);

    for count in ["-1", "nope", "1.5"] {
        assert_error_with_context(
            sandbox.run(&["notices", "--count", count]),
            2,
            "usage",
            &format!("invalid notice count {count:?}"),
        );
    }
}

#[test]
fn notices_show_requires_a_url_and_non_negative_max_chars() {
    let sandbox = Sandbox::new();
    write_malformed_credentials(&sandbox);

    assert_error(sandbox.run(&["notices", "show"]), 2, "usage");
    assert_error(
        sandbox.run(&[
            "notices",
            "show",
            "https://example.invalid/notice/1",
            "--max-chars",
            "-1",
        ]),
        2,
        "usage",
    );
    assert_no_session_file(&sandbox);
}

#[test]
fn notices_show_rejects_blank_url_before_auth() {
    let sandbox = Sandbox::new();

    for url in ["", " ", "\t\n"] {
        let payload = assert_error_with_context(
            sandbox.run(&["notices", "show", url]),
            2,
            "usage",
            &format!("blank notice URL {url:?}"),
        );
        assert!(
            error_hint(&payload).contains("bupt notices"),
            "blank URL hint should point at the listing command; stderr={payload}"
        );
    }
    assert_no_session_file(&sandbox);
}

#[test]
fn notices_show_rejects_external_and_credential_bearing_urls_before_auth() {
    let sandbox = Sandbox::new();
    for url in [
        "https://example.invalid/notice/1",
        "https://my.bupt.edu.cn@foreign.example/info/1.htm",
        "https://test-user@my.bupt.edu.cn/info/1.htm",
    ] {
        assert_error(sandbox.run(&["notices", "show", url]), 2, "usage");
    }
    assert_no_session_file(&sandbox);
}

#[test]
fn notices_show_requires_credentials_without_creating_session() {
    let sandbox = Sandbox::new();

    let payload = assert_error(
        sandbox.run(&["notices", "show", "http://my.bupt.edu.cn/info/1.htm"]),
        3,
        "auth",
    );
    assert!(
        error_hint(&payload).contains("login"),
        "notice detail auth hint should mention login; stderr={payload}"
    );
    assert_no_session_file(&sandbox);
}

#[test]
fn notices_and_card_are_not_blocked_by_malformed_dorm_config() {
    let sandbox = Sandbox::new();
    write_config(&sandbox, "{not valid json");

    for args in [vec!["notices"], vec!["card"]] {
        let payload = assert_error_with_context(
            sandbox.run(&args),
            3,
            "auth",
            &format!("portal command must not read dorm config for {args:?}"),
        );
        assert!(
            error_hint(&payload).contains("login"),
            "portal command should stop at the credential layer; stderr={payload}"
        );
    }
}

#[test]
fn card_rejects_unexpected_arguments() {
    let sandbox = Sandbox::new();
    write_malformed_credentials(&sandbox);

    assert_error(sandbox.run(&["card", "--area", "shahe"]), 2, "usage");
    assert_error(sandbox.run(&["card", "extra"]), 2, "usage");
    assert_no_session_file(&sandbox);
}
