use crate::cli::{CliError, CliResult, Context, Outcome};
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{Map, Value, json};
use std::env;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const SESSION_FILENAME: &str = "session.json";
const CREDENTIALS_FILENAME: &str = "credentials.json";
const CACHE_VERSION: u64 = 2;
const UCLOUD_EXPIRY_BUFFER_SECONDS: f64 = 60.0;
const CAS_SERVICE_URL: &str =
    "https://auth.bupt.edu.cn/authserver/login?service=https%3A%2F%2Fucloud.bupt.edu.cn%2Fuclass";

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
enum Role {
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

#[derive(Debug)]
struct Account {
    username: String,
    role: Option<Role>,
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
    let account = load_account(&context.config_dir)?;
    let session = load_session(&context.cache_dir, account.as_ref());
    Ok(Outcome {
        data: status_document(context, account.as_ref(), &session),
        code: 0,
        diagnostic: None,
    })
}

fn load_account(config_dir: &Path) -> CliResult<Option<Account>> {
    let path = config_dir.join(CREDENTIALS_FILENAME);
    let config = match fs::read(&path) {
        Ok(bytes) => {
            let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
                CliError::new(
                    3,
                    "auth",
                    "cannot parse the local credentials file",
                    "Fix credentials.json; it must contain a JSON object.",
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
                "Check credentials.json permissions/content.",
            ));
        }
    };

    let config_username = config.get("username").and_then(Value::as_str);
    let username = env::var("BUPT_USERNAME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| config_username.map(str::to_owned))
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let Some(username) = username else {
        return Ok(None);
    };

    let config_role = config
        .get("role")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let role_text = env::var("BUPT_ROLE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or(config_role);
    Ok(Some(Account {
        username,
        role: role_text.as_deref().and_then(Role::parse),
    }))
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

fn load_session(cache_dir: &Path, account: Option<&Account>) -> SessionSnapshot {
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
    let Some(session_username) = data
        .get("username")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return SessionSnapshot::empty("the session cache has no account owner");
    };
    if session_username != account.username {
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
            match (account.role, session_role) {
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
    account: Option<&Account>,
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
            .map(|account| json!(account.username))
            .or(Some(Value::Null))
    }
    .unwrap_or(Value::Null);
    let user = data
        .get("user")
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    let (cookie_count, cookie_expiry) = cookie_status(data.get("cookies"));
    let ucloud = ucloud_status(data.get("ucloud"), now);
    let mobile_data =
        if session.accepted && account.and_then(|account| account.role) != Some(Role::Graduate) {
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
        "cookie_count": cookie_count,
        "cookies_expire_at": cookie_expiry.and_then(iso_epoch).map(Value::String).unwrap_or(Value::Null),
        "ucloud": ucloud,
        "mobile_jwgl": mobile,
        "cas_logins_total": cas_logins_total,
        "locally_valid": session.accepted
            && cookie_count > 0
            && ucloud_present
            && !ucloud_expired,
        "note": "locally_valid is based on cached expiries; use `bupt diagnose` for live checks",
    })
}

fn cookie_status(cookies: Option<&Value>) -> (u64, Option<f64>) {
    let Some(cookies) = cookies.and_then(Value::as_array) else {
        return (0, None);
    };
    let count = cookies.len() as u64;
    let expiry = cookies
        .iter()
        .filter_map(|cookie| cookie.as_object()?.get("expires").and_then(number_value))
        .reduce(f64::min);
    (count, expiry)
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
    json!({
        "present": token.is_some(),
        "expired": token.map(|_| expiry.is_some_and(|value| value <= now)),
        "expires_at": expiry.and_then(iso_epoch).map(Value::String).unwrap_or(Value::Null),
        "last_error": string_or_null(mobile.get("last_error")),
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
    let removed = match fs::remove_file(&session_path) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => {
            return Err(CliError::new(
                3,
                "auth",
                "cannot delete the local session cache",
                "Check the session file permissions.",
            ));
        }
    };
    clear_saved_password(&context.config_dir.join(CREDENTIALS_FILENAME))?;
    Ok(Outcome {
        data: json!({"removed": removed, "path": path_string(&session_path)}),
        code: 0,
        diagnostic: None,
    })
}

fn clear_saved_password(path: &Path) -> CliResult<()> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => {
            return Err(CliError::new(
                3,
                "auth",
                "cannot read the local credentials file",
                "Check the credentials file permissions.",
            ));
        }
    };
    let mut value: Value = match serde_json::from_slice(&bytes) {
        Ok(value @ Value::Object(_)) => value,
        _ => {
            fs::remove_file(path).map_err(|_| {
                CliError::new(
                    3,
                    "auth",
                    "cannot remove the unreadable credentials file",
                    "Check the credentials file permissions.",
                )
            })?;
            return Ok(());
        }
    };
    if let Some(object) = value.as_object_mut() {
        object.remove("password");
    }
    let text = serde_json::to_string_pretty(&value).map_err(|_| {
        CliError::new(
            5,
            "upstream",
            "cannot serialize the local credentials file",
            "",
        )
    })?;
    write_private(path, text.as_bytes())
}

fn write_private(path: &Path, bytes: &[u8]) -> CliResult<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|_| {
                CliError::new(
                    3,
                    "auth",
                    "cannot update the local credentials file",
                    "Check the credentials file permissions.",
                )
            })?;
        file.write_all(bytes).map_err(|_| {
            CliError::new(
                3,
                "auth",
                "cannot update the local credentials file",
                "Check the credentials file permissions.",
            )
        })?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|_| {
            CliError::new(
                3,
                "auth",
                "cannot secure the local credentials file",
                "Check the credentials file permissions.",
            )
        })?;
    }
    #[cfg(not(unix))]
    {
        fs::write(path, bytes).map_err(|_| {
            CliError::new(
                3,
                "auth",
                "cannot update the local credentials file",
                "Check the credentials file permissions.",
            )
        })?;
    }
    Ok(())
}

fn run_diagnose(matches: &ArgMatches, context: &mut Context<'_>) -> CliResult<Outcome> {
    let _request_timeout = matches.get_one::<f64>("request_timeout");
    let groups = requested_groups(matches)?;
    let account = load_account(&context.config_dir)?;
    let session = load_session(&context.cache_dir, account.as_ref());
    let (cookie_count, _) = cookie_status(session.data.get("cookies"));
    let has_cookies = session.accepted && cookie_count > 0;

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

fn jwt_expiry(token: &str) -> Option<f64> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64url_decode(payload)?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    value.get("exp").and_then(number_value)
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
