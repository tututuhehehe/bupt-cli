mod contract_support;

use contract_support::{Invocation, Sandbox};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const USER: &str = "2026000001";
const OTHER_USER: &str = "2026000002";

const PRIMARY_PASSWORD: &str = "TEST-ONLY-CAS-PASSWORD-7f3a";
const OTHER_PASSWORD: &str = "TEST-ONLY-OTHER-PASSWORD-1c2b";
const ATTEMPT_PASSWORD: &str = "TEST-ONLY-ATTEMPT-PASSWORD-f6d1";
const CORRUPT_PASSWORD: &str = "TEST-ONLY-CORRUPT-PASSWORD-4d5e";
const COOKIE_SECRET: &str = "TEST-ONLY-COOKIE-SECRET-a1b2";
const SECOND_COOKIE_SECRET: &str = "TEST-ONLY-COOKIE-SECRET-b2c3";
const ACCESS_SECRET: &str = "TEST-ONLY-ACCESS-TOKEN-SECRET-c3d4";
const REFRESH_SECRET: &str = "TEST-ONLY-REFRESH-TOKEN-SECRET-e5f6";
const MOBILE_SECRET: &str = "TEST-ONLY-MOBILE-TOKEN-SECRET-g7h8";

const CREDENTIALS: &str = include_str!("fixtures/contracts/identity/credentials-primary.json");
const CREDENTIALS_OTHER: &str = include_str!("fixtures/contracts/identity/credentials-other.json");
const CREDENTIALS_UNDERGRADUATE: &str =
    include_str!("fixtures/contracts/identity/credentials-undergraduate.json");
const CREDENTIALS_CORRUPT: &str =
    include_str!("fixtures/contracts/identity/credentials-corrupt.json");
const SESSION: &str = include_str!("fixtures/contracts/identity/session-primary.json");
const SESSION_NO_MOBILE: &str = include_str!("fixtures/contracts/identity/session-no-mobile.json");
const SESSION_STALE_MOBILE: &str =
    include_str!("fixtures/contracts/identity/session-stale-mobile.json");
const CONFIG: &str = include_str!("fixtures/contracts/identity/config-settings.json");
const TIMETABLE: &str = include_str!("fixtures/contracts/identity/timetable.json");

fn config_dir(sandbox: &Sandbox) -> PathBuf {
    sandbox.root().join("config")
}

fn cache_dir(sandbox: &Sandbox) -> PathBuf {
    sandbox.root().join("cache")
}

fn seed_file(directory: &Path, name: &str, contents: &str) -> PathBuf {
    fs::create_dir_all(directory).expect("create fixture directory");
    let path = directory.join(name);
    fs::write(&path, contents).expect("write fixture");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&path).expect("fixture metadata").permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(&path, permissions).expect("make fixture user-private");
    }
    path
}

fn seed_json(directory: &Path, name: &str, value: &Value) -> PathBuf {
    let contents = serde_json::to_string_pretty(value).expect("serialize fixture");
    seed_file(directory, name, &contents)
}

fn seed_credentials(sandbox: &Sandbox, contents: &str) -> PathBuf {
    seed_file(&config_dir(sandbox), "credentials.json", contents)
}

fn seed_session(sandbox: &Sandbox, contents: &str) -> PathBuf {
    seed_file(&cache_dir(sandbox), "session.json", contents)
}

fn session_value(contents: &str) -> Value {
    serde_json::from_str(contents).expect("session fixture is valid JSON")
}

fn read_text(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

fn stdout_text(inv: &Invocation) -> String {
    String::from_utf8_lossy(&inv.0.stdout).into_owned()
}

fn stderr_text(inv: &Invocation) -> String {
    String::from_utf8_lossy(&inv.0.stderr).into_owned()
}

fn run_with_env(sandbox: &Sandbox, args: &[&str], envs: &[(&str, &str)]) -> Invocation {
    let mut command = sandbox.command();
    command.args(args);
    for (name, value) in envs {
        command.env(name, value);
    }
    Invocation(command.output().expect("run isolated CLI"))
}

fn success(inv: &Invocation) -> Value {
    assert_eq!(
        inv.code(),
        0,
        "expected success; stdout={:?}; stderr={:?}",
        stdout_text(inv),
        stderr_text(inv)
    );
    assert!(
        inv.0.stderr.is_empty(),
        "successful command wrote stderr: {:?}",
        stderr_text(inv)
    );
    let payload = inv.stdout_json();
    assert_eq!(payload["schema"], json!("bupt.v1"));
    assert_eq!(payload["ok"], json!(true));
    payload
}

fn usage_error(inv: &Invocation) -> Value {
    assert_eq!(
        inv.code(),
        2,
        "expected usage exit 2; stdout={:?}; stderr={:?}",
        stdout_text(inv),
        stderr_text(inv)
    );
    assert!(
        inv.0.stdout.is_empty(),
        "usage error wrote stdout: {:?}",
        stdout_text(inv)
    );
    let payload = inv.stderr_json();
    assert_eq!(payload["schema"], json!("bupt.v1"));
    assert_eq!(payload["ok"], json!(false));
    assert_eq!(payload["error"]["type"], json!("usage"));
    payload
}

fn auth_error(inv: &Invocation) -> Value {
    assert_eq!(
        inv.code(),
        3,
        "expected auth exit 3; stdout={:?}; stderr={:?}",
        stdout_text(inv),
        stderr_text(inv)
    );
    assert!(
        inv.0.stdout.is_empty(),
        "auth error wrote stdout: {:?}",
        stdout_text(inv)
    );
    let payload = inv.stderr_json();
    assert_eq!(payload["schema"], json!("bupt.v1"));
    assert_eq!(payload["ok"], json!(false));
    assert_eq!(payload["error"]["type"], json!("auth"));
    payload
}

fn assert_no_secret(inv: &Invocation, secret: &str) {
    for (stream, bytes) in [("stdout", &inv.0.stdout), ("stderr", &inv.0.stderr)] {
        assert!(
            !String::from_utf8_lossy(bytes).contains(secret),
            "synthetic secret leaked on {stream}"
        );
    }
}

fn assert_no_fixture_secrets(inv: &Invocation) {
    for secret in [
        PRIMARY_PASSWORD,
        OTHER_PASSWORD,
        COOKIE_SECRET,
        SECOND_COOKIE_SECRET,
        ACCESS_SECRET,
        REFRESH_SECRET,
        MOBILE_SECRET,
    ] {
        assert_no_secret(inv, secret);
    }
}

#[derive(Debug, PartialEq, Eq)]
struct EntryState {
    kind: &'static str,
    bytes: Option<Vec<u8>>,
    modified: Option<SystemTime>,
}

fn snapshot_tree(root: &Path) -> BTreeMap<String, EntryState> {
    let mut snapshot = BTreeMap::new();
    if root.exists() {
        snapshot_entry(root, root, &mut snapshot);
    }
    snapshot
}

fn snapshot_entry(root: &Path, path: &Path, snapshot: &mut BTreeMap<String, EntryState>) {
    let metadata = fs::symlink_metadata(path)
        .unwrap_or_else(|err| panic!("inspect {}: {err}", path.display()));
    let relative = path
        .strip_prefix(root)
        .expect("snapshot path is below root")
        .to_string_lossy()
        .replace('\\', "/");
    let kind = if metadata.is_dir() {
        "dir"
    } else if metadata.is_file() {
        "file"
    } else {
        "other"
    };
    let bytes = if metadata.is_file() {
        Some(fs::read(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display())))
    } else {
        None
    };
    snapshot.insert(
        relative,
        EntryState {
            kind,
            bytes,
            modified: metadata.modified().ok(),
        },
    );
    if metadata.is_dir() {
        let mut children: Vec<PathBuf> = fs::read_dir(path)
            .unwrap_or_else(|err| panic!("read directory {}: {err}", path.display()))
            .map(|entry| {
                entry
                    .unwrap_or_else(|err| panic!("read entry in {}: {err}", path.display()))
                    .path()
            })
            .collect();
        children.sort();
        for child in children {
            snapshot_entry(root, &child, snapshot);
        }
    }
}

fn assert_tree_empty(root: &Path) {
    let snapshot = snapshot_tree(root);
    assert!(
        snapshot.keys().all(String::is_empty),
        "expected no local files, found {:?}",
        snapshot.keys().collect::<Vec<_>>()
    );
}

#[cfg(unix)]
fn assert_private_file(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(path)
        .unwrap_or_else(|err| panic!("inspect {}: {err}", path.display()))
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "{} must be user-private", path.display());
}

#[cfg(not(unix))]
fn assert_private_file(_path: &Path) {}

#[test]
fn login_requires_explicit_role_and_rejects_unknown_role_before_writing() {
    let sandbox = Sandbox::new();

    let missing_role = run_with_env(
        &sandbox,
        &["login", "--user", USER, "--password", ATTEMPT_PASSWORD],
        &[],
    );
    usage_error(&missing_role);
    assert_no_secret(&missing_role, ATTEMPT_PASSWORD);
    assert!(!config_dir(&sandbox).join("credentials.json").exists());
    assert!(!cache_dir(&sandbox).join("session.json").exists());

    let unknown_role = run_with_env(
        &sandbox,
        &[
            "login",
            "--user",
            USER,
            "--password",
            ATTEMPT_PASSWORD,
            "--role",
            "staff",
        ],
        &[],
    );
    usage_error(&unknown_role);
    assert_no_secret(&unknown_role, ATTEMPT_PASSWORD);
    assert!(!config_dir(&sandbox).join("credentials.json").exists());
    assert!(!cache_dir(&sandbox).join("session.json").exists());
}

#[test]
fn failed_login_validation_keeps_last_registered_credentials_and_session() {
    let sandbox = Sandbox::new();
    let credentials_path = seed_credentials(&sandbox, CREDENTIALS);
    let session_path = seed_session(&sandbox, SESSION);
    let config_before = snapshot_tree(&config_dir(&sandbox));
    let cache_before = snapshot_tree(&cache_dir(&sandbox));

    let inv = run_with_env(
        &sandbox,
        &[
            "login",
            "--user",
            USER,
            "--password",
            ATTEMPT_PASSWORD,
            "--role",
            "not-a-role",
        ],
        &[],
    );
    usage_error(&inv);
    assert_no_secret(&inv, ATTEMPT_PASSWORD);
    assert_no_fixture_secrets(&inv);

    assert_eq!(read_text(&credentials_path), CREDENTIALS);
    assert_eq!(read_text(&session_path), SESSION);
    assert_eq!(snapshot_tree(&config_dir(&sandbox)), config_before);
    assert_eq!(snapshot_tree(&cache_dir(&sandbox)), cache_before);
}

#[test]
fn changed_account_does_not_reuse_previous_owners_session() {
    let sandbox = Sandbox::new();
    seed_credentials(&sandbox, CREDENTIALS_OTHER);
    seed_session(&sandbox, SESSION);
    let config_before = snapshot_tree(&config_dir(&sandbox));
    let cache_before = snapshot_tree(&cache_dir(&sandbox));

    let inv = run_with_env(&sandbox, &["status"], &[]);
    let status = success(&inv);

    assert_eq!(status["exists"], json!(false));
    assert_eq!(status["cache_username_mismatch"], json!(true));
    assert_eq!(status["cookie_count"], json!(0));
    assert_eq!(status["locally_valid"], json!(false));
    assert_no_fixture_secrets(&inv);

    assert_eq!(snapshot_tree(&config_dir(&sandbox)), config_before);
    assert_eq!(snapshot_tree(&cache_dir(&sandbox)), cache_before);
}

#[test]
fn environment_account_override_does_not_reuse_configured_owners_session() {
    let sandbox = Sandbox::new();
    seed_credentials(&sandbox, CREDENTIALS);
    seed_session(&sandbox, SESSION);

    let inv = run_with_env(
        &sandbox,
        &["status"],
        &[
            ("BUPT_USERNAME", OTHER_USER),
            ("BUPT_PASSWORD", OTHER_PASSWORD),
        ],
    );
    let status = success(&inv);

    assert_eq!(status["exists"], json!(false));
    assert_eq!(status["cache_username_mismatch"], json!(true));
    assert_eq!(status["cookie_count"], json!(0));
    assert_eq!(status["locally_valid"], json!(false));
    assert_no_fixture_secrets(&inv);
}

#[test]
fn unowned_session_cache_is_not_adopted() {
    let sandbox = Sandbox::new();
    seed_credentials(&sandbox, CREDENTIALS);

    let mut unowned = session_value(SESSION);
    unowned
        .as_object_mut()
        .expect("session fixture is an object")
        .remove("username");
    let unowned_text = serde_json::to_string_pretty(&unowned).expect("serialize fixture");
    let session_path = seed_file(&cache_dir(&sandbox), "session.json", &unowned_text);
    let cache_before = snapshot_tree(&cache_dir(&sandbox));

    let inv = run_with_env(&sandbox, &["status"], &[]);
    let status = success(&inv);

    assert_eq!(status["exists"], json!(false));
    assert_eq!(status["cookie_count"], json!(0));
    assert_eq!(status["locally_valid"], json!(false));
    assert_no_fixture_secrets(&inv);

    assert_eq!(read_text(&session_path), unowned_text);
    assert_eq!(snapshot_tree(&cache_dir(&sandbox)), cache_before);
}

#[test]
fn status_without_credentials_succeeds_without_creating_state() {
    let sandbox = Sandbox::new();

    let inv = run_with_env(&sandbox, &["status"], &[]);
    let status = success(&inv);

    assert_eq!(status["exists"], json!(false));
    assert_eq!(status["cookie_count"], json!(0));
    assert_eq!(status["locally_valid"], json!(false));
    assert_tree_empty(&config_dir(&sandbox));
    assert_tree_empty(&cache_dir(&sandbox));
}

#[test]
fn status_reads_cached_state_without_mutation_or_secret_leak() {
    let sandbox = Sandbox::new();
    let credentials_path = seed_credentials(&sandbox, CREDENTIALS);
    let session_path = seed_session(&sandbox, SESSION);
    seed_file(&config_dir(&sandbox), "config.json", CONFIG);
    seed_file(&cache_dir(&sandbox), "timetable.json", TIMETABLE);
    assert_private_file(&credentials_path);
    assert_private_file(&session_path);
    let config_before = snapshot_tree(&config_dir(&sandbox));
    let cache_before = snapshot_tree(&cache_dir(&sandbox));

    let inv = run_with_env(&sandbox, &["status"], &[]);
    let status = success(&inv);

    assert_eq!(status["exists"], json!(true));
    assert_eq!(status["username"], json!(USER));
    assert_eq!(status["cookie_count"], json!(1));
    assert_eq!(status["ucloud"]["present"], json!(true));
    assert_eq!(status["mobile_jwgl"]["present"], json!(true));
    assert_eq!(status["locally_valid"], json!(true));
    assert_eq!(status["cas_logins_total"], json!(2));
    assert_no_fixture_secrets(&inv);

    assert_eq!(snapshot_tree(&config_dir(&sandbox)), config_before);
    assert_eq!(snapshot_tree(&cache_dir(&sandbox)), cache_before);
}

#[test]
fn status_pretty_changes_format_not_values() {
    let sandbox = Sandbox::new();
    seed_credentials(&sandbox, CREDENTIALS);
    seed_session(&sandbox, SESSION);

    let plain_inv = run_with_env(&sandbox, &["status"], &[]);
    let plain = success(&plain_inv);
    let pretty_inv = run_with_env(&sandbox, &["status", "--pretty"], &[]);
    let pretty = success(&pretty_inv);

    assert_eq!(plain, pretty);
    assert!(stdout_text(&pretty_inv).contains("\n  "));
    assert_no_fixture_secrets(&plain_inv);
    assert_no_fixture_secrets(&pretty_inv);
}

#[test]
fn status_reports_explicit_expiry_without_refreshing() {
    let sandbox = Sandbox::new();
    seed_credentials(&sandbox, CREDENTIALS);

    let mut expired = session_value(SESSION);
    expired["cookies"][0]["expires"] = json!(1);
    expired["ucloud"]["expires_at"] = json!(1);
    expired["ucloud"]["jwt_payload"]["exp"] = json!(1);
    expired["mobile_jwgl"]["expires_at"] = json!(1);
    expired["cas_logins_total"] = json!(4);
    seed_json(&cache_dir(&sandbox), "session.json", &expired);
    let cache_before = snapshot_tree(&cache_dir(&sandbox));

    let inv = run_with_env(&sandbox, &["status"], &[]);
    let status = success(&inv);

    assert_eq!(status["exists"], json!(true));
    assert_eq!(status["ucloud"]["present"], json!(true));
    assert_eq!(status["ucloud"]["expired"], json!(true));
    assert_eq!(status["mobile_jwgl"]["present"], json!(true));
    assert_eq!(status["mobile_jwgl"]["expired"], json!(true));
    assert_eq!(status["locally_valid"], json!(false));
    assert_eq!(status["cas_logins_total"], json!(4));
    assert_no_fixture_secrets(&inv);

    assert_eq!(snapshot_tree(&cache_dir(&sandbox)), cache_before);
}

#[test]
fn status_treats_missing_expiry_as_unknown_not_expired() {
    let sandbox = Sandbox::new();
    seed_credentials(&sandbox, CREDENTIALS);

    let mut unknown = session_value(SESSION);
    unknown["ucloud"]
        .as_object_mut()
        .expect("ucloud is an object")
        .remove("expires_at");
    unknown["ucloud"]["jwt_payload"] = json!({"user_id": "42"});
    unknown["mobile_jwgl"]
        .as_object_mut()
        .expect("mobile_jwgl is an object")
        .remove("expires_at");
    seed_json(&cache_dir(&sandbox), "session.json", &unknown);
    let cache_before = snapshot_tree(&cache_dir(&sandbox));

    let inv = run_with_env(&sandbox, &["status"], &[]);
    let status = success(&inv);

    assert_eq!(status["exists"], json!(true));
    assert_eq!(status["ucloud"]["present"], json!(true));
    assert_eq!(status["ucloud"]["expired"], json!(false));
    assert_eq!(status["mobile_jwgl"]["present"], json!(true));
    assert_eq!(status["mobile_jwgl"]["expired"], json!(false));
    assert_no_fixture_secrets(&inv);

    assert_eq!(snapshot_tree(&cache_dir(&sandbox)), cache_before);
}

#[test]
fn status_tolerates_non_numeric_cookie_expiry() {
    let sandbox = Sandbox::new();
    seed_credentials(&sandbox, CREDENTIALS);

    let mut mixed = session_value(SESSION);
    mixed["cookies"][0]["expires"] = json!("not-a-number");
    let mut second_cookie = mixed["cookies"][0].clone();
    second_cookie["name"] = json!("SID2");
    second_cookie["value"] = json!(SECOND_COOKIE_SECRET);
    second_cookie["expires"] = json!("4102444800");
    mixed["cookies"]
        .as_array_mut()
        .expect("cookies is an array")
        .push(second_cookie);
    seed_json(&cache_dir(&sandbox), "session.json", &mixed);
    let cache_before = snapshot_tree(&cache_dir(&sandbox));

    let inv = run_with_env(&sandbox, &["status"], &[]);
    let status = success(&inv);

    assert_eq!(status["cookie_count"], json!(2));
    assert!(
        status["cookies_expire_at"].is_string(),
        "numeric expiry must survive a malformed neighbour"
    );
    assert_no_fixture_secrets(&inv);

    assert_eq!(snapshot_tree(&cache_dir(&sandbox)), cache_before);
}

#[test]
fn graduate_status_drops_stale_mobile_jwgl_failure_but_undergraduate_keeps_it() {
    let graduate = Sandbox::new();
    seed_credentials(&graduate, CREDENTIALS);
    seed_session(&graduate, SESSION_STALE_MOBILE);
    let graduate_cache_before = snapshot_tree(&cache_dir(&graduate));

    let graduate_inv = run_with_env(&graduate, &["status"], &[]);
    let graduate_status = success(&graduate_inv);
    assert_eq!(graduate_status["exists"], json!(true));
    assert_eq!(graduate_status["mobile_jwgl"]["present"], json!(false));
    assert_eq!(graduate_status["mobile_jwgl"]["last_error"], Value::Null);
    assert_no_fixture_secrets(&graduate_inv);
    assert_eq!(
        snapshot_tree(&cache_dir(&graduate)),
        graduate_cache_before,
        "status must not persist the cleanup"
    );

    let undergraduate = Sandbox::new();
    seed_credentials(&undergraduate, CREDENTIALS_UNDERGRADUATE);
    seed_session(&undergraduate, SESSION_STALE_MOBILE);
    let undergraduate_cache_before = snapshot_tree(&cache_dir(&undergraduate));

    let undergraduate_inv = run_with_env(&undergraduate, &["status"], &[]);
    let undergraduate_status = success(&undergraduate_inv);
    assert_eq!(undergraduate_status["exists"], json!(true));
    assert_eq!(undergraduate_status["mobile_jwgl"]["present"], json!(false));
    assert!(
        undergraduate_status["mobile_jwgl"]["last_error"]
            .as_str()
            .is_some(),
        "undergraduate stale JWGL failure must stay visible"
    );
    assert_no_fixture_secrets(&undergraduate_inv);
    assert_eq!(
        snapshot_tree(&cache_dir(&undergraduate)),
        undergraduate_cache_before
    );
}

#[test]
fn unreadable_credentials_are_auth_error_before_network_or_write() {
    let sandbox = Sandbox::new();
    seed_credentials(&sandbox, CREDENTIALS_CORRUPT);
    let config_before = snapshot_tree(&config_dir(&sandbox));
    let cache_before = snapshot_tree(&cache_dir(&sandbox));

    let inv = run_with_env(&sandbox, &["diagnose", "--only", "cas"], &[]);
    auth_error(&inv);
    assert_no_secret(&inv, CORRUPT_PASSWORD);
    assert_no_fixture_secrets(&inv);

    assert_eq!(snapshot_tree(&config_dir(&sandbox)), config_before);
    assert_eq!(snapshot_tree(&cache_dir(&sandbox)), cache_before);
    assert!(!cache_dir(&sandbox).join("session.json").exists());
}

#[test]
fn logout_clears_password_and_session_but_keeps_timetable_and_config() {
    let sandbox = Sandbox::new();
    let credentials_path = seed_credentials(&sandbox, CREDENTIALS);
    let session_path = seed_session(&sandbox, SESSION);
    let config_path = seed_file(&config_dir(&sandbox), "config.json", CONFIG);
    let timetable_path = seed_file(&cache_dir(&sandbox), "timetable.json", TIMETABLE);

    let inv = run_with_env(&sandbox, &["logout"], &[]);
    let result = success(&inv);

    assert_eq!(result["removed"], json!(true));
    assert!(!session_path.exists());
    assert_eq!(read_text(&config_path), CONFIG);
    assert_eq!(read_text(&timetable_path), TIMETABLE);
    assert_no_fixture_secrets(&inv);

    if credentials_path.exists() {
        let contents = read_text(&credentials_path);
        assert!(
            !contents.contains(PRIMARY_PASSWORD),
            "logout must remove the stored password"
        );
        let credentials: Value =
            serde_json::from_str(&contents).expect("remaining credentials must be valid JSON");
        if let Some(password) = credentials.get("password") {
            assert!(
                password.is_null() || password.as_str() == Some(""),
                "logout left a usable password behind"
            );
        }
        assert_private_file(&credentials_path);
    }

    let second_inv = run_with_env(&sandbox, &["logout"], &[]);
    let second = success(&second_inv);
    assert_eq!(second["removed"], json!(false));
    assert_eq!(read_text(&config_path), CONFIG);
    assert_eq!(read_text(&timetable_path), TIMETABLE);
    assert!(!session_path.exists());
}

#[test]
fn diagnose_skips_authenticated_checks_without_session_and_does_not_write() {
    let sandbox = Sandbox::new();
    seed_credentials(&sandbox, CREDENTIALS);
    let config_before = snapshot_tree(&config_dir(&sandbox));
    let cache_before = snapshot_tree(&cache_dir(&sandbox));

    let inv = run_with_env(&sandbox, &["diagnose", "--only", "cas"], &[]);
    let report = success(&inv);

    assert_eq!(report["auth"]["cached_cas_cookies"], json!(false));
    let rows = report["endpoints"]
        .as_array()
        .expect("diagnose report has endpoints");
    assert!(!rows.is_empty(), "the CAS check must be reported");
    assert!(
        rows.iter().all(|row| row["group"] == json!("cas")),
        "group filtering must not run other endpoint groups"
    );
    assert!(
        rows.iter().all(|row| row["status"] == json!("skipped")),
        "no cached CAS session must produce skipped rows"
    );
    assert!(report["summary"]["skipped"].as_u64().unwrap_or(0) >= 1);
    assert_no_fixture_secrets(&inv);

    assert_eq!(snapshot_tree(&config_dir(&sandbox)), config_before);
    assert_eq!(snapshot_tree(&cache_dir(&sandbox)), cache_before);
    assert!(!cache_dir(&sandbox).join("session.json").exists());
}

#[test]
fn diagnose_does_not_login_mobile_with_cached_cas_session() {
    let sandbox = Sandbox::new();
    seed_credentials(&sandbox, CREDENTIALS);
    let session_path = seed_session(&sandbox, SESSION_NO_MOBILE);
    let cache_before = snapshot_tree(&cache_dir(&sandbox));

    let inv = run_with_env(&sandbox, &["diagnose", "--only", "mobile_jwgl"], &[]);
    let report = success(&inv);

    assert_eq!(report["auth"]["mobile_jwgl_token"], json!(false));
    let rows = report["endpoints"]
        .as_array()
        .expect("diagnose report has endpoints");
    assert!(!rows.is_empty(), "mobile JWGL checks must be reported");
    assert!(
        rows.iter().all(|row| row["group"] == json!("mobile_jwgl")),
        "group filtering must not run other endpoint groups"
    );
    assert!(
        rows.iter().all(|row| row["status"] == json!("skipped")),
        "diagnose must not submit the mobile JWGL password"
    );
    assert_no_fixture_secrets(&inv);

    assert_eq!(read_text(&session_path), SESSION_NO_MOBILE);
    assert_eq!(snapshot_tree(&cache_dir(&sandbox)), cache_before);
}

#[test]
fn diagnose_unknown_group_is_usage_before_io() {
    let sandbox = Sandbox::new();
    seed_credentials(&sandbox, CREDENTIALS);
    seed_session(&sandbox, SESSION);
    seed_file(&config_dir(&sandbox), "config.json", CONFIG);
    seed_file(&cache_dir(&sandbox), "timetable.json", TIMETABLE);
    let config_before = snapshot_tree(&config_dir(&sandbox));
    let cache_before = snapshot_tree(&cache_dir(&sandbox));

    let inv = run_with_env(&sandbox, &["diagnose", "--only", "not-a-real-group"], &[]);
    usage_error(&inv);
    assert_no_fixture_secrets(&inv);

    assert_eq!(snapshot_tree(&config_dir(&sandbox)), config_before);
    assert_eq!(snapshot_tree(&cache_dir(&sandbox)), cache_before);
}
