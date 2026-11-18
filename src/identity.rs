use crate::cli::{CliError, CliResult, Context, Outcome};
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{Map, Value, json};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const SESSION_FILENAME: &str = "session.json";
const CREDENTIALS_FILENAME: &str = "credentials.json";
const CACHE_VERSION: u64 = 2;
const UCLOUD_EXPIRY_BUFFER_SECONDS: f64 = 60.0;
const SAFE_MOBILE_DIAGNOSTIC: &str = "cached mobile JWGL login failed; details suppressed";
const PUBLIC_USER_FIELDS: &[&str] = &["user_id", "user_name", "real_name"];
const CAS_SERVICE_URL: &str =
    "https://auth.bupt.edu.cn/authserver/login?service=https%3A%2F%2Fucloud.bupt.edu.cn%2Fuclass";

// The synthetic protocol is not compiled into production until the live-wiring gate is met.
#[cfg(test)]
mod cas;

const DIAGNOSE_GROUPS: &[&str] = &[
    "public",
    "cas",
    "electricity",
    "portal",
    "ucloud",
    "mobile_jwgl",
    "jwgl_dom",
    "other",
];

pub(crate) fn commands() -> Vec<Command> {
    vec![
        Command::new("login")
            .about("Register and verify a campus account")
            .arg(
                Arg::new("user")
                    .long("user")
                    .value_name("ID")
                    .help("Student id"),
            )
            .arg(
                Arg::new("password")
                    .long("password")
                    .value_name("PASSWORD")
                    .help("Campus password (visible in shell history)"),
            )
            .arg(
                Arg::new("role")
                    .long("role")
                    .value_name("ROLE")
                    .help("Explicit academic role: graduate or undergraduate"),
            )
            .arg(
                Arg::new("force")
                    .long("force")
                    .action(ArgAction::SetTrue)
                    .help("Ignore the cached session"),
            ),
        Command::new("status").about("Show local account and session state"),
        Command::new("logout").about("Forget saved credentials and sessions"),
        Command::new("diagnose")
            .about("Check service availability")
            .arg(
                Arg::new("only")
                    .long("only")
                    .value_name("GROUPS")
                    .help("Comma-separated groups to check"),
            )
            .arg(
                Arg::new("request_timeout")
                    .long("request-timeout")
                    .value_name("SECONDS")
                    .value_parser(positive_timeout)
                    .help("Per-request timeout in seconds"),
            ),
    ]
}

fn positive_timeout(text: &str) -> Result<f64, String> {
    let value = text
        .parse::<f64>()
        .map_err(|_| "expected seconds as a number")?;
    if !value.is_finite() || value <= 0.0 {
        return Err("expected a finite positive timeout".into());
    }
    Ok(value)
}

pub(crate) fn run(
    name: &str,
    matches: &ArgMatches,
    context: &mut Context<'_>,
) -> CliResult<Outcome> {
    match name {
        "login" => run_login(matches),
        "status" => run_status(context),
        "logout" => run_logout(context),
        "diagnose" => run_diagnose(matches, context),
        _ => Err(CliError::usage(format!("unknown identity command: {name}"))),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Role {
    Graduate,
    Undergraduate,
}

impl Role {
    fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "graduate" | "研究生" | "硕士" | "博士" | "grad" | "postgrad" => {
                Some(Self::Graduate)
            }
            "undergraduate" | "本科生" | "本科" | "undergrad" | "ug" => {
                Some(Self::Undergraduate)
            }
            _ => None,
        }
    }
}

/// Read-only local profile metadata; it contains no password or session material.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LocalProfile {
    username: String,
    role: Option<Role>,
    has_password: bool,
}

impl LocalProfile {
    pub(crate) fn username(&self) -> &str {
        &self.username
    }

    pub(crate) fn role(&self) -> Option<Role> {
        self.role
    }

    pub(crate) fn has_password(&self) -> bool {
        self.has_password
    }
}

fn run_login(matches: &ArgMatches) -> CliResult<Outcome> {
    let (_username, _password, _role) = login_arguments(matches)?;
    // Credential verification needs the campus transport. The local
    // milestone deliberately stops before reading or writing account state.
    Err(CliError::pending())
}

fn login_arguments(matches: &ArgMatches) -> CliResult<(String, String, Role)> {
    let username = matches
        .get_one::<String>("user")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| CliError::usage("login requires --user"))?
        .to_owned();
    let password = matches
        .get_one::<String>("password")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| CliError::usage("login requires --password"))?
        .clone();
    let role = matches
        .get_one::<String>("role")
        .and_then(|value| Role::parse(value))
        .ok_or_else(|| CliError::usage("login requires --role graduate or --role undergraduate"))?;
    Ok((username, password, role))
}

fn run_status(context: &mut Context<'_>) -> CliResult<Outcome> {
    let account = load_profile(&context.config_dir)?;
    let session = load_session(&context.cache_dir, account.as_ref());
    Ok(Outcome {
        data: status_document(context, account.as_ref(), &session),
        code: 0,
        diagnostic: None,
    })
}

fn load_account(config_dir: &Path) -> CliResult<Option<LocalProfile>> {
    let path = config_dir.join(CREDENTIALS_FILENAME);
    let config = match fs::read(&path) {
        Ok(bytes) => {
            let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
                CliError::new(
                    3,
                    "auth",
                    "cannot parse the local credentials file",
                    "Fix credentials.json and run `bupt login` again.",
                )
            })?;
            match value {
                Value::Object(object) => object,
                _ => {
                    return Err(CliError::new(
                        3,
                        "auth",
                        "the local credentials file must contain a JSON object",
                        "Fix credentials.json and run `bupt login` again.",
                    ));
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Map::new(),
        Err(_) => {
            return Err(CliError::new(
                3,
                "auth",
                "cannot read the local credentials file",
                "Check credentials.json permissions/content, then run `bupt login` again.",
            ));
        }
    };

    let username = config
        .get("username")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let Some(username) = username else {
        return Ok(None);
    };

    let role = config
        .get("role")
        .and_then(Value::as_str)
        .and_then(Role::parse);
    let has_password = config
        .get("password")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty());
    Ok(Some(LocalProfile {
        username: username.to_owned(),
        role,
        has_password,
    }))
}

/// Load local profile metadata without reading the session cache or writing state.
///
/// `credentials.json` is the sole source for the username, password presence and
/// role. Account environment variables are intentionally ignored.
pub(crate) fn load_profile(config_dir: &Path) -> CliResult<Option<LocalProfile>> {
    load_account(config_dir)
}

/// Load a profile that can open an authenticated command.
///
/// This only requires a username and a nonempty saved password. Role remains
/// operation-specific and is deliberately not validated here.
pub(crate) fn require_authenticated_profile(config_dir: &Path) -> CliResult<LocalProfile> {
    match load_profile(config_dir)? {
        Some(profile) if profile.has_password() => Ok(profile),
        _ => Err(CliError::new(
            3,
            "auth",
            "BUPT credentials are not configured",
            "Run `bupt login` to save credentials.json.",
        )),
    }
}

#[derive(Debug)]
struct SessionSnapshot {
    data: Map<String, Value>,
    accepted: bool,
    mismatch: bool,
    error: String,
}

impl SessionSnapshot {
    fn empty(error: impl Into<String>) -> Self {
        Self {
            data: Map::new(),
            accepted: false,
            mismatch: false,
            error: error.into(),
        }
    }
}

fn load_session(cache_dir: &Path, account: Option<&LocalProfile>) -> SessionSnapshot {
    let path = cache_dir.join(SESSION_FILENAME);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return SessionSnapshot::empty("");
        }
        Err(_) => return SessionSnapshot::empty("cannot read the session cache"),
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return SessionSnapshot::empty("cannot parse the session cache"),
    };
    let Value::Object(mut data) = value else {
        return SessionSnapshot::empty("the session cache must contain a JSON object");
    };

    let Some(account) = account else {
        return SessionSnapshot::empty("no configured account; session cache not adopted");
    };
    if !account.has_password() {
        return SessionSnapshot::empty(
            "the saved profile has no password; session cache not adopted",
        );
    }
    let Some(session_username) = data
        .get("username")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return SessionSnapshot::empty("the session cache has no account owner");
    };
    if session_username != account.username() {
        let mut snapshot =
            SessionSnapshot::empty("the session cache belongs to a different account");
        snapshot.mismatch = true;
        return snapshot;
    }

    let version = data.get("version").and_then(Value::as_u64);
    match version {
        Some(1) => {
            // Legacy caches have no verified role. CAS/UCloud state is
            // role-independent; mobile JWGL state is not.
            data.remove("mobile_jwgl");
            SessionSnapshot {
                data,
                accepted: true,
                mismatch: false,
                error: String::new(),
            }
        }
        Some(CACHE_VERSION) => {
            let session_role = data
                .get("role")
                .and_then(Value::as_str)
                .and_then(Role::parse);
            match (account.role(), session_role) {
                (Some(expected), Some(actual)) if expected == actual => SessionSnapshot {
                    data,
                    accepted: true,
                    mismatch: false,
                    error: String::new(),
                },
                _ => SessionSnapshot::empty("the session cache role does not match this account"),
            }
        }
        _ => SessionSnapshot::empty("unsupported session cache version"),
    }
}

fn status_document(
    context: &Context<'_>,
    account: Option<&LocalProfile>,
    session: &SessionSnapshot,
) -> Value {
    let now = unix_now();
    let session_path = context.cache_dir.join(SESSION_FILENAME);
    let empty = Map::new();
    let data = if session.accepted {
        &session.data
    } else {
        &empty
    };

    let username = if session.accepted {
        data.get("username")
            .and_then(Value::as_str)
            .map(|value| json!(value))
    } else {
        account
            .map(|account| json!(account.username()))
            .or(Some(Value::Null))
    }
    .unwrap_or(Value::Null);
    let user = public_user(data.get("user"));
    let cookies = cookie_status(data.get("cookies"), now);
    let ucloud = ucloud_status(data.get("ucloud"), now);
    let mobile_data =
        if session.accepted && account.and_then(|account| account.role()) != Some(Role::Graduate) {
            data.get("mobile_jwgl")
        } else {
            None
        };
    let mobile = mobile_status(mobile_data, now);
    let ucloud_present = ucloud["present"].as_bool().unwrap_or(false);
    let ucloud_expired = ucloud["expired"].as_bool().unwrap_or(false);
    let cas_logins_total = if session.accepted {
        data.get("cas_logins_total")
            .and_then(Value::as_u64)
            .unwrap_or(0)
    } else {
        0
    };

    json!({
        "exists": session.accepted,
        "path": path_string(&session_path),
        "username": username,
        "user": user,
        "created_at": string_or_null(data.get("created_at")),
        "updated_at": string_or_null(data.get("updated_at")),
        "cache_error": session.error,
        "cache_username_mismatch": session.mismatch,
        "cookie_count": cookies.total,
        "cookies_expire_at": cookies.earliest_expiry.and_then(iso_epoch).map(Value::String).unwrap_or(Value::Null),
        "ucloud": ucloud,
        "mobile_jwgl": mobile,
        "cas_logins_total": cas_logins_total,
        "locally_valid": session.accepted
            && cookies.usable > 0
            && ucloud_present
            && !ucloud_expired,
        "note": "locally_valid is based on cached expiries; use `bupt diagnose` for live checks",
    })
}

#[derive(Debug, Default)]
struct CookieStatus {
    total: u64,
    usable: u64,
    earliest_expiry: Option<f64>,
}

fn cookie_status(cookies: Option<&Value>, now: f64) -> CookieStatus {
    let Some(cookies) = cookies.and_then(Value::as_array) else {
        return CookieStatus::default();
    };
    let mut status = CookieStatus {
        total: cookies.len() as u64,
        ..CookieStatus::default()
    };
    for cookie in cookies {
        let Some(cookie) = cookie.as_object() else {
            continue;
        };
        let expiry = cookie
            .get("expires")
            .and_then(number_value)
            .filter(|value| value.is_finite());
        if let Some(expiry) = expiry {
            status.earliest_expiry = Some(
                status
                    .earliest_expiry
                    .map_or(expiry, |current| current.min(expiry)),
            );
        }
        let domained = cookie
            .get("domain")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty());
        let named = cookie
            .get("name")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty());
        let valued = cookie
            .get("value")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty());
        let expired = expiry.is_some_and(|value| value <= now);
        if domained && named && valued && !expired {
            status.usable += 1;
        }
    }
    status
}

fn public_user(user: Option<&Value>) -> Value {
    let mut public = Map::new();
    let Some(user) = user.and_then(Value::as_object) else {
        return Value::Object(public);
    };
    for field in PUBLIC_USER_FIELDS {
        let Some(value) = user.get(*field) else {
            continue;
        };
        let scalar = if *field == "user_id" {
            if let Some(value) = value.as_str() {
                Some(Value::String(value.to_owned()))
            } else if let Some(value) = value.as_i64() {
                Some(Value::from(value))
            } else {
                value.as_u64().map(Value::from)
            }
        } else {
            value.as_str().map(|value| Value::String(value.to_owned()))
        };
        if let Some(scalar) = scalar {
            public.insert((*field).to_owned(), scalar);
        }
    }
    Value::Object(public)
}

fn ucloud_status(ucloud: Option<&Value>, now: f64) -> Value {
    let Some(ucloud) = ucloud.and_then(Value::as_object) else {
        return empty_ucloud();
    };
    let Some(token) = ucloud
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return empty_ucloud();
    };
    let explicit_expiry = ucloud.get("expires_at").and_then(number_value);
    let expiry = explicit_expiry.or_else(|| jwt_expiry(token));
    let expired = expiry.map(|value| value <= now + UCLOUD_EXPIRY_BUFFER_SECONDS);
    json!({
        "present": true,
        "expired": expired.unwrap_or(false),
        "expires_at": explicit_expiry.and_then(iso_epoch).map(Value::String).unwrap_or(Value::Null),
        "user_id": string_or_null(ucloud.get("user_id")),
        "user_name": string_or_null(ucloud.get("user_name")),
    })
}

fn empty_ucloud() -> Value {
    json!({
        "present": false,
        "expired": Value::Null,
        "expires_at": Value::Null,
        "user_id": Value::Null,
        "user_name": Value::Null,
    })
}

fn mobile_status(mobile: Option<&Value>, now: f64) -> Value {
    let Some(mobile) = mobile.and_then(Value::as_object) else {
        return empty_mobile();
    };
    let token = mobile
        .get("token")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    let expiry = mobile.get("expires_at").and_then(number_value);
    let last_error = mobile
        .get("last_error")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(|_| json!(SAFE_MOBILE_DIAGNOSTIC))
        .unwrap_or(Value::Null);
    json!({
        "present": token.is_some(),
        "expired": token.map(|_| expiry.is_some_and(|value| value <= now)),
        "expires_at": expiry.and_then(iso_epoch).map(Value::String).unwrap_or(Value::Null),
        "last_error": last_error,
        "blocked_until": mobile
            .get("blocked_until")
            .and_then(number_value)
            .and_then(iso_epoch)
            .map(Value::String)
            .unwrap_or(Value::Null),
    })
}

fn empty_mobile() -> Value {
    json!({
        "present": false,
        "expired": Value::Null,
        "expires_at": Value::Null,
        "last_error": Value::Null,
        "blocked_until": Value::Null,
    })
}

fn string_or_null(value: Option<&Value>) -> Value {
    value
        .and_then(Value::as_str)
        .map(|value| json!(value))
        .unwrap_or(Value::Null)
}

fn number_value(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
}

fn run_logout(context: &mut Context<'_>) -> CliResult<Outcome> {
    let session_path = context.cache_dir.join(SESSION_FILENAME);
    let credentials_path = context.config_dir.join(CREDENTIALS_FILENAME);
    ensure_removable(&session_path, "session cache")?;
    ensure_removable(&credentials_path, "credentials file")?;

    let removed = remove_owned_file(&session_path, "session cache")?;
    remove_owned_file(&credentials_path, "credentials file")?;
    Ok(Outcome {
        data: json!({"removed": removed, "path": path_string(&session_path)}),
        code: 0,
        diagnostic: None,
    })
}

fn ensure_removable(path: &Path, label: &str) -> CliResult<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => {
            return Err(CliError::new(
                3,
                "auth",
                format!("cannot inspect the local {label}"),
                "Check the file permissions.",
            ));
        }
    };
    if metadata.file_type().is_symlink() {
        return Err(CliError::new(
            3,
            "auth",
            format!("refusing to remove a symlink at the local {label} path"),
            "Remove the symlink manually, then run `bupt logout` again.",
        ));
    }
    if !metadata.file_type().is_file() {
        return Err(CliError::new(
            3,
            "auth",
            format!("refusing to remove a non-regular local {label}"),
            "Remove it manually, then run `bupt logout` again.",
        ));
    }
    Ok(())
}

fn remove_owned_file(path: &Path, label: &str) -> CliResult<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(CliError::new(
            3,
            "auth",
            format!("cannot delete the local {label}"),
            "Check the file permissions.",
        )),
    }
}

fn run_diagnose(matches: &ArgMatches, context: &mut Context<'_>) -> CliResult<Outcome> {
    let _request_timeout = matches.get_one::<f64>("request_timeout");
    let groups = requested_groups(matches)?;
    let account = load_profile(&context.config_dir)?;
    let session = load_session(&context.cache_dir, account.as_ref());
    let has_cookies =
        session.accepted && cookie_status(session.data.get("cookies"), unix_now()).usable > 0;

    if !has_cookies && groups.iter().all(|group| group == "cas") {
        return Ok(Outcome {
            data: skipped_cas_report(context),
            code: 0,
            diagnostic: None,
        });
    }
    Err(CliError::pending())
}

fn requested_groups(matches: &ArgMatches) -> CliResult<Vec<String>> {
    let Some(raw) = matches.get_one::<String>("only") else {
        return Ok(DIAGNOSE_GROUPS
            .iter()
            .map(|group| (*group).to_owned())
            .collect());
    };
    let mut groups = Vec::new();
    for part in raw.split(',') {
        let group = part.trim();
        if group.is_empty() {
            continue;
        }
        if !DIAGNOSE_GROUPS.contains(&group) {
            return Err(CliError::usage(format!(
                "unknown diagnose group: {group}; available: {}",
                DIAGNOSE_GROUPS.join(", ")
            )));
        }
        if !groups.iter().any(|existing| existing == group) {
            groups.push(group.to_owned());
        }
    }
    if groups.is_empty() {
        Ok(DIAGNOSE_GROUPS
            .iter()
            .map(|group| (*group).to_owned())
            .collect())
    } else {
        Ok(groups)
    }
}

fn skipped_cas_report(context: &Context<'_>) -> Value {
    json!({
        "generated_at": iso_epoch(unix_now()).unwrap_or_default(),
        "duration_s": 0.0,
        "session_path": path_string(&context.cache_dir.join(SESSION_FILENAME)),
        "auth": {
            "cached_cas_cookies": false,
            "ucloud_token": false,
            "ucloud_token_refreshed": false,
            "mobile_jwgl_token": false,
        },
        "summary": {"ok": 0, "fail": 0, "skipped": 1, "total": 1},
        "endpoints": [{
            "group": "cas",
            "name": "CAS service ticket",
            "method": "GET",
            "url": CAS_SERVICE_URL,
            "status": "skipped",
            "evidence": "no cached CAS session; run `bupt login`",
        }],
    })
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}

fn iso_epoch(epoch: f64) -> Option<String> {
    if !epoch.is_finite() {
        return None;
    }
    let seconds = epoch.floor() as i64;
    let days = seconds.div_euclid(86_400);
    let day_seconds = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = day_seconds / 3_600;
    let minute = (day_seconds % 3_600) / 60;
    let second = day_seconds % 60;
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"
    ))
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    if month <= 2 {
        year += 1;
    }
    (year, month, day)
}

fn jwt_payload(token: &str) -> Option<Map<String, Value>> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64url_decode(payload)?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    value.as_object().cloned()
}

fn jwt_expiry(token: &str) -> Option<f64> {
    jwt_payload(token)?.get("exp").and_then(number_value)
}

fn base64url_decode(text: &str) -> Option<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = 0_u32;
    let mut bits = 0_u32;
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            b'=' => continue,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    const PROFILE_PASSWORD: &str = "TEST-ONLY-PROFILE-PASSWORD-4a7c";
    const PROFILE_COOKIE: &str = "TEST-ONLY-PROFILE-COOKIE-b8d2";
    const PROFILE_TOKEN: &str = "TEST-ONLY-PROFILE-TOKEN-c9e3";

    fn write_profile(directory: &Path, value: &Value) {
        let text = serde_json::to_string_pretty(value).expect("serialize profile");
        fs::write(directory.join(CREDENTIALS_FILENAME), text).expect("write profile");
    }

    fn load_isolated_profile(directory: &Path) -> CliResult<Option<LocalProfile>> {
        load_profile(directory)
    }

    #[test]
    fn profile_normalizes_username_role_and_password_presence() {
        let directory = tempdir().expect("create profile directory");
        write_profile(
            directory.path(),
            &serde_json::json!({
                "username": " 2026000001 ",
                "password": PROFILE_PASSWORD,
                "role": "研究生",
                "cookie": PROFILE_COOKIE,
                "access_token": PROFILE_TOKEN,
                "unrelated": {"nested": "ignored"}
            }),
        );

        let profile = load_isolated_profile(directory.path())
            .expect("load profile")
            .expect("profile is present");
        assert_eq!(profile.username(), "2026000001");
        assert_eq!(profile.role(), Some(Role::Graduate));
        assert!(profile.has_password());

        let debug = format!("{profile:?}");
        for secret in [PROFILE_PASSWORD, PROFILE_COOKIE, PROFILE_TOKEN] {
            assert!(!debug.contains(secret), "Debug output leaked {secret}");
        }
    }

    #[test]
    fn profile_keeps_passwordless_and_missing_or_invalid_role_explicit() {
        let passwordless_dir = tempdir().expect("create passwordless directory");
        write_profile(
            passwordless_dir.path(),
            &serde_json::json!({"username": " 2026000002 ", "role": "本科生"}),
        );
        let passwordless = load_isolated_profile(passwordless_dir.path())
            .expect("load passwordless profile")
            .expect("passwordless profile is present");
        assert_eq!(passwordless.username(), "2026000002");
        assert_eq!(passwordless.role(), Some(Role::Undergraduate));
        assert!(!passwordless.has_password());

        let missing_role_dir = tempdir().expect("create missing-role directory");
        write_profile(
            missing_role_dir.path(),
            &serde_json::json!({"username": "2026000003", "password": PROFILE_PASSWORD}),
        );
        let missing_role = load_isolated_profile(missing_role_dir.path())
            .expect("load missing-role profile")
            .expect("missing-role profile is present");
        assert_eq!(missing_role.role(), None);

        let invalid_role_dir = tempdir().expect("create invalid-role directory");
        write_profile(
            invalid_role_dir.path(),
            &serde_json::json!({
                "username": "2026000004",
                "password": PROFILE_PASSWORD,
                "role": "staff"
            }),
        );
        let invalid_role = load_isolated_profile(invalid_role_dir.path())
            .expect("load invalid-role profile")
            .expect("invalid-role profile is present");
        assert_eq!(invalid_role.role(), None);
    }

    #[test]
    fn profile_absent_file_is_none() {
        let directory = tempdir().expect("create empty profile directory");
        assert!(
            load_isolated_profile(directory.path())
                .expect("load profile")
                .is_none()
        );
    }
}
