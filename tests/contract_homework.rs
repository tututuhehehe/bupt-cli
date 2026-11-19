mod contract_support;

use contract_support::{Invocation, Sandbox};
use serde_json::Value;
use std::fs;

const SCHEMA: &str = "bupt.v1";
const ASSIGNMENT_ID: &str = "1000000000000000001";

fn describe(inv: &Invocation) -> String {
    format!(
        "exit={:?}; stdout={:?}; stderr={:?}",
        inv.0.status.code(),
        String::from_utf8_lossy(&inv.0.stdout),
        String::from_utf8_lossy(&inv.0.stderr)
    )
}

fn assert_error(inv: &Invocation, code: i32, error_type: &str) -> Value {
    assert_eq!(inv.0.status.code(), Some(code), "{}", describe(inv));
    assert!(
        inv.0.stdout.is_empty(),
        "errors must not write a result document: {}",
        describe(inv)
    );
    let document = inv.stderr_json();
    assert_eq!(document["schema"], SCHEMA, "{}", describe(inv));
    assert_eq!(document["ok"], false, "{}", describe(inv));
    assert_eq!(
        document["error"]["type"],
        error_type,
        "wrong error layer: {}",
        describe(inv)
    );
    document
}

#[test]
fn courses_reaches_the_auth_boundary_without_credentials() {
    let sandbox = Sandbox::new();
    assert_error(&sandbox.run(&["courses"]), 3, "auth");
}

#[test]
fn environment_only_credentials_are_ignored() {
    let sandbox = Sandbox::new();
    let password = "TEST-ONLY-HOMEWORK-ENV-PASSWORD-5e8a";
    let invocation = Invocation(
        sandbox
            .command()
            .env("BUPT_USERNAME", "2026000099")
            .env("BUPT_PASSWORD", password)
            .env("BUPT_ROLE", "undergraduate")
            .args(["courses"])
            .output()
            .expect("run with synthetic environment credentials"),
    );
    let error = assert_error(&invocation, 3, "auth");
    assert!(
        error["error"]["hint"]
            .as_str()
            .is_some_and(|hint| hint.contains("login")),
        "auth hint should point at login: {error}"
    );
    assert!(!String::from_utf8_lossy(&invocation.0.stderr).contains(password));
}

#[test]
fn malformed_saved_credentials_are_an_auth_error() {
    let sandbox = Sandbox::new();
    fs::write(
        sandbox.root().join("config/credentials.json"),
        "{not valid synthetic JSON",
    )
    .expect("write synthetic malformed credentials");

    let invocation = sandbox.run(&["courses"]);
    let error = assert_error(&invocation, 3, "auth");
    assert!(
        error["error"]["hint"]
            .as_str()
            .is_some_and(|hint| hint.contains("login")),
        "malformed credentials should keep the auth hint: {error}"
    );
    assert!(!String::from_utf8_lossy(&invocation.0.stderr).contains("not valid synthetic JSON"));
}

#[test]
fn legacy_roleless_credentials_cannot_prove_a_verified_login() {
    let sandbox = Sandbox::new();
    fs::write(
        sandbox.root().join("config/credentials.json"),
        r#"{"username":"2026000097","password":"TEST-ONLY-HOMEWORK-PASSWORD-3c4d"}"#,
    )
    .expect("write synthetic roleless credentials");

    let invocation = sandbox.run(&["courses"]);
    let error = assert_error(&invocation, 3, "auth");
    assert!(
        error["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("bupt login")
    );
    assert!(!sandbox.root().join("cache/session.json").exists());
}

#[test]
fn new_filter_reaches_the_verified_session_boundary() {
    let sandbox = Sandbox::new();
    let credentials = sandbox.root().join("config/credentials.json");
    fs::write(
        &credentials,
        r#"{"username":"2026000096","password":"TEST-ONLY-HOMEWORK-PASSWORD-6a7b","role":"undergraduate"}"#,
    )
    .expect("write synthetic role credentials");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&credentials)
            .expect("synthetic credentials metadata")
            .permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(&credentials, permissions).expect("make credentials user-private");
    }

    let invocation = sandbox.run(&["homework", "--new", "7"]);
    let error = assert_error(&invocation, 3, "auth");
    assert!(
        error["error"]["hint"]
            .as_str()
            .is_some_and(|hint| hint.contains("login")),
        "--new must reach the verified-session boundary: {error}"
    );
    assert!(!sandbox.root().join("cache/session.json").exists());
}

#[test]
fn read_commands_reach_the_verified_session_boundary() {
    let sandbox = Sandbox::new();
    fs::write(
        sandbox.root().join("config/credentials.json"),
        r#"{"username":"2026000098","password":"TEST-ONLY-HOMEWORK-PASSWORD-4e5f"}"#,
    )
    .expect("write synthetic roleless credentials");

    for args in [
        vec!["homework"],
        vec!["homework", "--undone"],
        vec!["homework", "--course", "site-1"],
        vec!["homework", "--limit", "1"],
        vec!["homework", "--new", "7"],
        vec!["homework", "show", ASSIGNMENT_ID],
        vec!["homework", "verify", ASSIGNMENT_ID],
        vec!["homework", "download", ASSIGNMENT_ID, "--dir", "downloads"],
    ] {
        let invocation = sandbox.run(&args);
        let error = assert_error(&invocation, 3, "auth");
        assert!(
            error["error"]["hint"]
                .as_str()
                .is_some_and(|hint| hint.contains("login")),
            "verified-session boundary should point at login for {args:?}: {error}"
        );
    }
    assert!(!sandbox.root().join("cache/session.json").exists());
}

#[test]
fn homework_defaults_to_the_list_command() {
    let sandbox = Sandbox::new();
    assert_error(&sandbox.run(&["homework"]), 3, "auth");
}

#[test]
fn explicit_homework_list_alias_is_rejected_as_usage() {
    let sandbox = Sandbox::new();
    let error = assert_error(&sandbox.run(&["homework", "list"]), 2, "usage");
    assert!(
        error["error"]["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty()),
        "usage error needs an explanation: {error}"
    );
}

#[test]
fn default_homework_accepts_the_list_flags() {
    let sandbox = Sandbox::new();
    for args in [
        vec!["homework", "--undone"],
        vec!["homework", "--limit", "1"],
        vec!["homework", "--course", "site-1"],
        vec!["homework", "--new", "7"],
    ] {
        let invocation = sandbox.run(&args);
        assert_error(&invocation, 3, "auth");
    }
}

#[test]
fn invalid_list_values_are_usage_errors_before_credentials() {
    let sandbox = Sandbox::new();
    for args in [
        vec!["homework", "--limit", "-1"],
        vec!["homework", "--new", "-1"],
    ] {
        assert_error(&sandbox.run(&args), 2, "usage");
    }
}

#[test]
fn show_rejects_a_non_numeric_course_id_before_credentials() {
    let sandbox = Sandbox::new();
    assert_error(
        &sandbox.run(&["homework", "show", ASSIGNMENT_ID, "--course", "abc"]),
        2,
        "usage",
    );
}

#[test]
fn submit_rejects_an_empty_payload_before_credentials() {
    let sandbox = Sandbox::new();
    assert_error(
        &sandbox.run(&["homework", "submit", ASSIGNMENT_ID]),
        2,
        "usage",
    );
}

#[test]
fn submit_rejects_dry_run_with_confirmation_before_credentials() {
    let sandbox = Sandbox::new();
    assert_error(
        &sandbox.run(&[
            "homework",
            "submit",
            ASSIGNMENT_ID,
            "--text",
            "synthetic",
            "--dry-run",
            "--i-confirm-submit",
        ]),
        2,
        "usage",
    );
}

#[test]
fn homework_subcommands_accept_documented_flags_before_credentials() {
    let sandbox = Sandbox::new();
    fs::write(sandbox.root().join("answer.pdf"), b"synthetic answer\n")
        .expect("write synthetic answer file");
    for args in [
        vec!["homework", "show", ASSIGNMENT_ID, "--course", "123"],
        vec![
            "homework",
            "download",
            ASSIGNMENT_ID,
            "--dir",
            "downloads",
            "--force",
        ],
        vec![
            "homework",
            "verify",
            ASSIGNMENT_ID,
            "--resource-id",
            "resource-1",
            "--text",
            "synthetic",
        ],
        vec![
            "homework",
            "submit",
            ASSIGNMENT_ID,
            "--file",
            "answer.pdf",
            "--text",
            "synthetic",
            "--resource-id",
            "resource-1",
            "--i-confirm-submit",
            "--no-verify",
        ],
    ] {
        assert_error(&sandbox.run(&args), 3, "auth");
    }
}

#[test]
fn submit_without_confirmation_is_usage_before_credentials() {
    let sandbox = Sandbox::new();
    fs::write(sandbox.root().join("answer.pdf"), b"synthetic answer\n")
        .expect("write synthetic answer file");
    let error = assert_error(
        &sandbox.run(&[
            "homework",
            "submit",
            ASSIGNMENT_ID,
            "--file",
            "answer.pdf",
            "--text",
            "synthetic",
        ]),
        2,
        "usage",
    );
    assert!(
        error["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("--i-confirm-submit")),
        "the gate must name the required confirmation flag: {error}"
    );
}

#[test]
fn submit_rejects_a_missing_file_before_credentials() {
    let sandbox = Sandbox::new();
    assert_error(
        &sandbox.run(&[
            "homework",
            "submit",
            ASSIGNMENT_ID,
            "--file",
            "missing.pdf",
            "--dry-run",
        ]),
        2,
        "usage",
    );
}
