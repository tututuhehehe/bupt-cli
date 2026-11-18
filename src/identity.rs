use crate::cli::{CliError, CliResult, Context, Outcome};
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{Map, Value, json};
use std::fmt;
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

struct LoginRequest {
    username: String,
    password: String,
    role: Role,
}

impl fmt::Debug for LoginRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoginRequest")
            .field("username", &self.username)
            .field("role", &self.role)
            .field("password", &"<redacted>")
            .finish()
    }
}

fn run_login(matches: &ArgMatches) -> CliResult<Outcome> {
    let request = login_arguments(matches)?;
    // Read every validated field so production builds retain the same request
    // shape even though registration is compiled only for synthetic tests.
    let _ = (request.username, request.password, request.role);
    // The verified registration path is compiled only for synthetic tests until
    // the production transport and live-wiring gate are approved.
    Err(CliError::pending())
}

fn login_arguments(matches: &ArgMatches) -> CliResult<LoginRequest> {
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
    Ok(LoginRequest {
        username,
        password,
        role,
    })
}

// Offline verification-to-registration orchestration. Production CAS remains
// disabled; this module exists only for synthetic tests until live wiring is
// approved.
#[cfg(test)]
mod registration {
    use super::*;
    use crate::transport::Transport;
    #[cfg(unix)]
    use std::io::Write;
    use std::path::Path;
    #[cfg(unix)]
    use std::process;

    /// Proof that the supplied password completed CAS verification.
    ///
    /// The constructor is private to this module; only `verify_login` can make
    /// one. It intentionally carries no ticket, cookie, token or redirect URL.
    struct VerifiedLogin;

    pub(super) fn verify_and_register(
        config_dir: &Path,
        transport: &mut dyn Transport,
        request: &LoginRequest,
    ) -> CliResult<()> {
        let verified = verify_login(transport, request)?;
        register_verified_login(config_dir, request, verified)
    }

    fn verify_login(
        transport: &mut dyn Transport,
        request: &LoginRequest,
    ) -> CliResult<VerifiedLogin> {
        // Always verify the supplied password against CAS. No cached session or
        // previous credential is consulted here.
        let login = cas::cas_login(transport, &request.username, &request.password)?;
        // Drop protocol material immediately; only a zero-sized proof survives.
        drop(login);
        Ok(VerifiedLogin)
    }

    fn register_verified_login(
        config_dir: &Path,
        request: &LoginRequest,
        _verified: VerifiedLogin,
    ) -> CliResult<()> {
        // Only credentials.json is written. Invalidating or rewriting the
        // session cache is a separate decision; doing both here would create an
        // unsafe cross-file transaction with no atomic rollback.
        write_credentials_atomic(config_dir, request)
    }

    fn write_credentials_atomic(config_dir: &Path, request: &LoginRequest) -> CliResult<()> {
        let mut credentials = read_credentials_for_update(config_dir)?;
        credentials.insert("username".to_owned(), json!(request.username));
        credentials.insert("password".to_owned(), json!(request.password));
        credentials.insert("role".to_owned(), json!(role_name(request.role)));
        let mut bytes = serde_json::to_vec_pretty(&Value::Object(credentials))
            .map_err(|_| write_error("cannot encode the local credentials file"))?;
        bytes.push(b'\n');
        write_credentials_file(config_dir, &config_dir.join(CREDENTIALS_FILENAME), &bytes)
    }

    fn read_credentials_for_update(config_dir: &Path) -> CliResult<Map<String, Value>> {
        let path = config_dir.join(CREDENTIALS_FILENAME);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
            Err(_) => return Err(write_error("cannot inspect the local credentials file")),
        };
        if metadata.file_type().is_symlink() {
            return Err(write_error(
                "refusing to replace a symlink at the local credentials path",
            ));
        }
        if !metadata.file_type().is_file() {
            return Err(write_error(
                "refusing to replace a non-regular local credentials file",
            ));
        }
        // A verified login may repair a malformed or unreadable regular file.
        // Only a valid JSON object contributes unknown keys to the replacement.
        let Ok(bytes) = fs::read(&path) else {
            return Ok(Map::new());
        };
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(Value::Object(credentials)) => Ok(credentials),
            _ => Ok(Map::new()),
        }
    }

    fn role_name(role: Role) -> &'static str {
        match role {
            Role::Graduate => "graduate",
            Role::Undergraduate => "undergraduate",
        }
    }

    fn write_error(message: &'static str) -> CliError {
        CliError::new(
            3,
            "auth",
            message,
            "Check the config directory and credentials.json permissions, then retry.",
        )
    }

    #[cfg(unix)]
    fn write_credentials_file(config_dir: &Path, path: &Path, bytes: &[u8]) -> CliResult<()> {
        use std::os::unix::fs::OpenOptionsExt;

        ensure_private_directory(config_dir)?;
        ensure_replaceable(path)?;
        for attempt in 0..100 {
            let temporary = config_dir.join(format!(
                ".{CREDENTIALS_FILENAME}.tmp.{}.{attempt}",
                process::id()
            ));
            let file = match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)
            {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(write_error("cannot create a temporary credentials file")),
            };
            let result = finish_atomic_write(file, &temporary, path, bytes);
            if result.is_err() {
                let _ = fs::remove_file(&temporary);
            }
            return result;
        }
        Err(write_error(
            "cannot create a unique temporary credentials file",
        ))
    }

    #[cfg(unix)]
    fn ensure_private_directory(path: &Path) -> CliResult<()> {
        use std::os::unix::fs::PermissionsExt;

        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir_all(path)
                    .map_err(|_| write_error("cannot create the config directory"))?;
                let metadata = fs::symlink_metadata(path)
                    .map_err(|_| write_error("cannot inspect the config directory"))?;
                if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
                    return Err(write_error("refusing to use an unsafe config directory"));
                }
                fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                    .map_err(|_| write_error("cannot make the config directory user-private"))?;
                return Ok(());
            }
            Err(_) => return Err(write_error("cannot inspect the config directory")),
        };
        if metadata.file_type().is_symlink() {
            return Err(write_error(
                "refusing to use a symlink as the config directory",
            ));
        }
        if !metadata.file_type().is_dir() {
            return Err(write_error("the config directory is not a directory"));
        }
        Ok(())
    }

    #[cfg(unix)]
    fn ensure_replaceable(path: &Path) -> CliResult<()> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => Err(write_error(
                "refusing to replace a symlink at the local credentials path",
            )),
            Ok(metadata) if !metadata.file_type().is_file() => Err(write_error(
                "refusing to replace a non-regular local credentials file",
            )),
            Ok(_) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(write_error("cannot inspect the local credentials file")),
        }
    }

    #[cfg(unix)]
    fn finish_atomic_write(
        mut file: fs::File,
        temporary: &Path,
        path: &Path,
        bytes: &[u8],
    ) -> CliResult<()> {
        use std::os::unix::fs::PermissionsExt;

        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| write_error("cannot make the credentials file user-private"))?;
        file.write_all(bytes)
            .map_err(|_| write_error("cannot write the credentials file"))?;
        file.sync_all()
            .map_err(|_| write_error("cannot flush the credentials file"))?;
        drop(file);
        fs::rename(temporary, path)
            .map_err(|_| write_error("cannot replace the local credentials file"))
    }

    #[cfg(not(unix))]
    fn write_credentials_file(_config_dir: &Path, _path: &Path, _bytes: &[u8]) -> CliResult<()> {
        Err(CliError::new(
            5,
            "upstream",
            "secure credential writes are not implemented on this platform",
            "Do not treat this platform as supported until its user-only file protections are verified.",
        ))
    }
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
    use super::registration::verify_and_register;
    use super::*;
    use crate::transport::Response;
    use crate::transport::testing::Scripted;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::{PermissionsExt, symlink};
    #[cfg(unix)]
    use std::path::PathBuf;
    use tempfile::tempdir;

    const PROFILE_PASSWORD: &str = "TEST-ONLY-PROFILE-PASSWORD-4a7c";
    const PROFILE_COOKIE: &str = "TEST-ONLY-PROFILE-COOKIE-b8d2";
    const PROFILE_TOKEN: &str = "TEST-ONLY-PROFILE-TOKEN-c9e3";

    const REGISTER_USER: &str = "2026000001";
    const REGISTER_OLD_PASSWORD: &str = "TEST-ONLY-REGISTER-OLD-PASSWORD-1a2b";
    const REGISTER_NEW_PASSWORD: &str = "TEST-ONLY-REGISTER-NEW-PASSWORD-3c4d";
    const REGISTER_LOGIN_URL: &str = "https://auth.bupt.edu.cn/authserver/login";
    const REGISTER_EXECUTION: &str = "TEST-ONLY-REGISTER-EXECUTION-9i0j";
    const REGISTER_COOKIE: &str = "TEST-ONLY-REGISTER-CAS_SESSION-e5f6";
    const REGISTER_CASTGC: &str = "TEST-ONLY-REGISTER-CASTGC-m3n4";
    const REGISTER_TICKET: &str = "ST-TEST-ONLY-REGISTER-TICKET-g7h8";
    const REGISTER_TICKET_URL: &str =
        "https://ucloud.bupt.edu.cn/uclass?ticket=ST-TEST-ONLY-REGISTER-TICKET-g7h8";
    const REGISTER_SESSION_SECRET: &str = "TEST-ONLY-REGISTER-SESSION-SECRET-k1l2";

    fn write_profile(directory: &Path, value: &Value) {
        let text = serde_json::to_string_pretty(value).expect("serialize profile");
        fs::write(directory.join(CREDENTIALS_FILENAME), text).expect("write profile");
    }

    fn load_isolated_profile(directory: &Path) -> CliResult<Option<LocalProfile>> {
        load_profile(directory)
    }

    fn register_request(password: &str, role: Role) -> LoginRequest {
        LoginRequest {
            username: REGISTER_USER.to_owned(),
            password: password.to_owned(),
            role,
        }
    }

    fn register_response(status: u16, headers: &[(&str, &str)], body: &str) -> Response {
        Response {
            status,
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
            body: body.as_bytes().to_vec(),
        }
    }

    fn register_enqueue(
        transport: &mut Scripted,
        method: &'static str,
        url: &str,
        status: u16,
        headers: &[(&str, &str)],
        body: &str,
    ) {
        transport.expected.push_back((
            method,
            url.to_owned(),
            Ok(register_response(status, headers, body)),
        ));
    }

    fn register_login_page_body() -> String {
        format!(
            "<html><form><input value='{REGISTER_EXECUTION}' type='hidden' name='execution'></form></html>"
        )
    }

    fn register_login_page() -> Response {
        let cookie = format!("CAS_SESSION={REGISTER_COOKIE}; Path=/authserver; Secure; HttpOnly");
        register_response(
            200,
            &[("Set-Cookie", cookie.as_str())],
            &register_login_page_body(),
        )
    }

    fn successful_verification_script() -> Scripted {
        let mut transport = Scripted::default();
        transport.expected.push_back((
            "GET",
            REGISTER_LOGIN_URL.to_owned(),
            Ok(register_login_page()),
        ));
        register_enqueue(
            &mut transport,
            "POST",
            REGISTER_LOGIN_URL,
            302,
            &[("Location", REGISTER_TICKET_URL)],
            "",
        );
        register_enqueue(
            &mut transport,
            "GET",
            REGISTER_TICKET_URL,
            200,
            &[],
            "<html><body>synthetic UCloud workspace</body></html>",
        );
        register_enqueue(
            &mut transport,
            "GET",
            CAS_SERVICE_URL,
            302,
            &[("Location", REGISTER_TICKET_URL)],
            "",
        );
        transport
    }

    fn verification_script_with_form_status(status: u16) -> Scripted {
        let mut transport = Scripted::default();
        transport.expected.push_back((
            "GET",
            REGISTER_LOGIN_URL.to_owned(),
            Ok(register_login_page()),
        ));
        register_enqueue(&mut transport, "POST", REGISTER_LOGIN_URL, status, &[], "");
        transport
    }

    fn verification_script_with_login_page_after_post() -> Scripted {
        let mut transport = Scripted::default();
        transport.expected.push_back((
            "GET",
            REGISTER_LOGIN_URL.to_owned(),
            Ok(register_login_page()),
        ));
        register_enqueue(
            &mut transport,
            "POST",
            REGISTER_LOGIN_URL,
            200,
            &[],
            &register_login_page_body(),
        );
        transport
    }

    fn verification_script_with_network_failure() -> Scripted {
        let mut transport = Scripted::default();
        transport.expected.push_back((
            "GET",
            REGISTER_LOGIN_URL.to_owned(),
            Err(CliError::new(
                4,
                "network",
                "synthetic connection failure",
                "Check the network connection and retry.",
            )),
        ));
        transport
    }

    fn entry_names(directory: &Path) -> Vec<String> {
        if !directory.exists() {
            return Vec::new();
        }
        let mut names = fs::read_dir(directory)
            .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
            .map(|entry| {
                entry
                    .expect("read directory entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    fn assert_error_redacted(error: &CliError, secrets: &[&str], paths: &[&Path]) {
        let rendered = format!("{error:?}");
        for secret in secrets {
            assert!(
                !rendered.contains(secret),
                "error leaked a synthetic secret: {rendered}"
            );
        }
        for path in paths {
            let path = path.to_string_lossy();
            assert!(
                !rendered.contains(path.as_ref()),
                "error leaked an unsafe path: {rendered}"
            );
        }
    }

    #[cfg(unix)]
    fn seed_register_file(directory: &Path, name: &str, contents: &str) -> PathBuf {
        fs::create_dir_all(directory).expect("create registration directory");
        let path = directory.join(name);
        fs::write(&path, contents).expect("write registration fixture");
        set_register_mode(&path, 0o600);
        path
    }

    #[cfg(unix)]
    fn set_register_mode(path: &Path, mode: u32) {
        let mut permissions = fs::metadata(path)
            .unwrap_or_else(|error| panic!("inspect {}: {error}", path.display()))
            .permissions();
        permissions.set_mode(mode);
        fs::set_permissions(path, permissions)
            .unwrap_or_else(|error| panic!("chmod {}: {error}", path.display()));
    }

    #[cfg(unix)]
    fn register_mode(path: &Path) -> u32 {
        fs::metadata(path)
            .unwrap_or_else(|error| panic!("inspect {}: {error}", path.display()))
            .permissions()
            .mode()
            & 0o777
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

    #[test]
    fn login_request_debug_redacts_password() {
        let request = register_request(REGISTER_NEW_PASSWORD, Role::Graduate);
        let debug = format!("{request:?}");
        assert!(!debug.contains(REGISTER_NEW_PASSWORD));
        assert!(debug.contains("<redacted>"));
        assert!(debug.contains(REGISTER_USER));
    }

    #[cfg(unix)]
    #[test]
    fn verified_registration_writes_private_credentials_and_preserves_unknown_keys() {
        let root = tempdir().expect("create registration root");
        let config = root.path().join("config");
        let cache = root.path().join("cache");
        fs::create_dir_all(&config).expect("create config directory");
        fs::create_dir_all(&cache).expect("create cache directory");

        let existing = serde_json::json!({
            "username": REGISTER_USER,
            "password": REGISTER_OLD_PASSWORD,
            "role": "graduate",
            "preserved": {"nested": true}
        });
        let credentials_path = seed_register_file(
            &config,
            CREDENTIALS_FILENAME,
            &serde_json::to_string_pretty(&existing).expect("serialize existing credentials"),
        );
        set_register_mode(&credentials_path, 0o644);
        let session_text = serde_json::to_string_pretty(&serde_json::json!({
            "version": 2,
            "username": REGISTER_USER,
            "role": "undergraduate",
            "cookies": [{
                "name": "CASTGC",
                "value": REGISTER_SESSION_SECRET,
                "domain": "auth.bupt.edu.cn"
            }],
            "cas_logins_total": 2
        }))
        .expect("serialize session");
        let session_path = seed_register_file(&cache, SESSION_FILENAME, &session_text);
        let session_before = fs::read(&session_path).expect("read session before registration");

        let mut transport = successful_verification_script();
        verify_and_register(
            &config,
            &mut transport,
            &register_request(REGISTER_NEW_PASSWORD, Role::Undergraduate),
        )
        .expect("verified registration succeeds");

        assert_eq!(transport.calls.len(), 4);
        let post = &transport.calls[1];
        assert_eq!(post.method, "POST");
        assert_eq!(post.url, REGISTER_LOGIN_URL);
        let post_body = String::from_utf8_lossy(&post.body);
        assert!(post_body.contains(REGISTER_NEW_PASSWORD));
        assert!(!post_body.contains(REGISTER_OLD_PASSWORD));

        assert_eq!(register_mode(&credentials_path), 0o600);
        let bytes = fs::read(&credentials_path).expect("read registered credentials");
        let saved: Value = serde_json::from_slice(&bytes).expect("registered credentials are JSON");
        assert_eq!(saved["username"], serde_json::json!(REGISTER_USER));
        assert_eq!(saved["password"], serde_json::json!(REGISTER_NEW_PASSWORD));
        assert_eq!(saved["role"], serde_json::json!("undergraduate"));
        assert_eq!(saved["preserved"]["nested"], serde_json::json!(true));
        let saved_object = saved.as_object().expect("credentials are an object");
        for transient in [
            "ticket",
            "final_url",
            "redirect_url",
            "cookies",
            "access_token",
            "refresh_token",
        ] {
            assert!(
                !saved_object.contains_key(transient),
                "registered credentials persisted {transient}"
            );
        }
        let text = String::from_utf8(bytes).expect("credentials are UTF-8");
        for secret in [
            REGISTER_OLD_PASSWORD,
            REGISTER_COOKIE,
            REGISTER_CASTGC,
            REGISTER_TICKET,
            REGISTER_TICKET_URL,
            CAS_SERVICE_URL,
        ] {
            assert!(!text.contains(secret), "credentials persisted {secret}");
        }

        assert_eq!(
            fs::read(&session_path).expect("read session"),
            session_before
        );
        assert_eq!(entry_names(&config), vec![CREDENTIALS_FILENAME.to_owned()]);
        assert_eq!(entry_names(&cache), vec![SESSION_FILENAME.to_owned()]);
    }

    #[cfg(unix)]
    #[test]
    fn change_password_verification_uses_supplied_password_and_never_cached_session() {
        let root = tempdir().expect("create change-password root");
        let config = root.path().join("config");
        let cache = root.path().join("cache");
        fs::create_dir_all(&config).expect("create config directory");
        fs::create_dir_all(&cache).expect("create cache directory");

        let credentials_path = seed_register_file(
            &config,
            CREDENTIALS_FILENAME,
            &serde_json::to_string_pretty(&serde_json::json!({
                "username": REGISTER_USER,
                "password": REGISTER_OLD_PASSWORD,
                "role": "undergraduate"
            }))
            .expect("serialize old credentials"),
        );
        let session_text = serde_json::to_string_pretty(&serde_json::json!({
            "version": 2,
            "username": REGISTER_USER,
            "role": "undergraduate",
            "cookies": [{
                "name": "CASTGC",
                "value": REGISTER_SESSION_SECRET,
                "domain": "auth.bupt.edu.cn"
            }],
            "cas_logins_total": 3
        }))
        .expect("serialize cached session");
        let session_path = seed_register_file(&cache, SESSION_FILENAME, &session_text);
        let session_before = fs::read(&session_path).expect("read session before registration");

        let mut transport = successful_verification_script();
        verify_and_register(
            &config,
            &mut transport,
            &register_request(REGISTER_NEW_PASSWORD, Role::Undergraduate),
        )
        .expect("change-password registration succeeds");

        assert_eq!(transport.calls.len(), 4);
        let post_body = String::from_utf8_lossy(&transport.calls[1].body);
        assert!(post_body.contains(REGISTER_NEW_PASSWORD));
        assert!(!post_body.contains(REGISTER_OLD_PASSWORD));
        for call in &transport.calls {
            let mut rendered = format!("{} {} ", call.method, call.url);
            for (name, value) in &call.headers {
                rendered.push_str(name);
                rendered.push(':');
                rendered.push_str(value);
                rendered.push('\n');
            }
            rendered.push_str(&String::from_utf8_lossy(&call.body));
            assert!(
                !rendered.contains(REGISTER_OLD_PASSWORD),
                "cached old password reached the network: {rendered}"
            );
            assert!(
                !rendered.contains(REGISTER_SESSION_SECRET),
                "cached session secret reached the network: {rendered}"
            );
        }

        let saved: Value = serde_json::from_slice(
            &fs::read(&credentials_path).expect("read registered credentials"),
        )
        .expect("registered credentials are JSON");
        assert_eq!(saved["password"], serde_json::json!(REGISTER_NEW_PASSWORD));
        assert_eq!(
            fs::read(&session_path).expect("read session"),
            session_before,
            "registration must not rewrite the session cache"
        );
    }

    #[cfg(unix)]
    #[test]
    fn failed_verification_preserves_credentials_and_session_bytes() {
        let cases: [(&str, Scripted, u8, &str); 4] = [
            (
                "rejected credentials",
                verification_script_with_form_status(401),
                3,
                "auth",
            ),
            (
                "login page returned after POST",
                verification_script_with_login_page_after_post(),
                3,
                "auth",
            ),
            (
                "rate limited",
                verification_script_with_form_status(429),
                5,
                "upstream",
            ),
            (
                "connection failure",
                verification_script_with_network_failure(),
                4,
                "network",
            ),
        ];

        for (label, mut transport, expected_code, expected_kind) in cases {
            let root = tempdir().expect("create failure root");
            let config = root.path().join("config");
            let cache = root.path().join("cache");
            fs::create_dir_all(&config).expect("create config directory");
            fs::create_dir_all(&cache).expect("create cache directory");

            let credentials_path = seed_register_file(
                &config,
                CREDENTIALS_FILENAME,
                &serde_json::to_string_pretty(&serde_json::json!({
                    "username": REGISTER_USER,
                    "password": REGISTER_OLD_PASSWORD,
                    "role": "undergraduate"
                }))
                .expect("serialize existing credentials"),
            );
            set_register_mode(&credentials_path, 0o640);
            let session_path = seed_register_file(
                &cache,
                SESSION_FILENAME,
                &serde_json::to_string_pretty(&serde_json::json!({
                    "version": 2,
                    "username": REGISTER_USER,
                    "role": "undergraduate",
                    "cookie": REGISTER_SESSION_SECRET,
                    "cas_logins_total": 3
                }))
                .expect("serialize existing session"),
            );
            let credentials_before =
                fs::read(&credentials_path).expect("read credentials before failure");
            let session_before = fs::read(&session_path).expect("read session before failure");

            let error = verify_and_register(
                &config,
                &mut transport,
                &register_request(REGISTER_NEW_PASSWORD, Role::Undergraduate),
            )
            .expect_err(label);
            assert_eq!(error.code, expected_code, "{label}: {error:?}");
            assert_eq!(error.kind, expected_kind, "{label}: {error:?}");
            if expected_code == 5 {
                assert!(
                    !error.hint.to_ascii_lowercase().contains("password"),
                    "HTTP 5xx/429 must not prompt a password retry: {error:?}"
                );
            }
            assert_error_redacted(
                &error,
                &[
                    REGISTER_OLD_PASSWORD,
                    REGISTER_NEW_PASSWORD,
                    REGISTER_COOKIE,
                    REGISTER_TICKET,
                    REGISTER_TICKET_URL,
                    REGISTER_SESSION_SECRET,
                ],
                &[
                    credentials_path.as_path(),
                    session_path.as_path(),
                    config.as_path(),
                ],
            );

            assert_eq!(
                fs::read(&credentials_path).expect("read credentials after failure"),
                credentials_before,
                "{label}: credentials changed"
            );
            assert_eq!(
                fs::read(&session_path).expect("read session after failure"),
                session_before,
                "{label}: session changed"
            );
            assert_eq!(register_mode(&credentials_path), 0o640);
            assert_eq!(entry_names(&config), vec![CREDENTIALS_FILENAME.to_owned()]);
            assert_eq!(entry_names(&cache), vec![SESSION_FILENAME.to_owned()]);
        }
    }

    #[cfg(unix)]
    #[test]
    fn registration_refuses_symlinked_credentials_without_touching_target() {
        let root = tempdir().expect("create symlink root");
        let config = root.path().join("config");
        fs::create_dir_all(&config).expect("create config directory");
        let target = seed_register_file(
            root.path(),
            "target-credentials.json",
            "TEST-ONLY-TARGET-CREDENTIALS-n5o6",
        );
        let credentials_path = config.join(CREDENTIALS_FILENAME);
        symlink(&target, &credentials_path).expect("create credentials symlink");
        let target_before = fs::read(&target).expect("read symlink target");

        let mut transport = successful_verification_script();
        let error = verify_and_register(
            &config,
            &mut transport,
            &register_request(REGISTER_NEW_PASSWORD, Role::Undergraduate),
        )
        .expect_err("symlinked credentials must be rejected");

        assert_eq!(error.code, 3, "{error:?}");
        assert_eq!(error.kind, "auth", "{error:?}");
        assert!(error.message.contains("symlink"), "{error:?}");
        assert_error_redacted(
            &error,
            &[
                REGISTER_OLD_PASSWORD,
                REGISTER_NEW_PASSWORD,
                REGISTER_COOKIE,
                REGISTER_TICKET,
                REGISTER_SESSION_SECRET,
            ],
            &[
                credentials_path.as_path(),
                target.as_path(),
                config.as_path(),
            ],
        );
        assert!(
            fs::symlink_metadata(&credentials_path)
                .expect("inspect credentials symlink")
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read(&target).expect("read symlink target"),
            target_before
        );
        assert_eq!(entry_names(&config), vec![CREDENTIALS_FILENAME.to_owned()]);
    }

    #[cfg(unix)]
    #[test]
    fn registration_refuses_symlinked_config_directory() {
        let root = tempdir().expect("create config-symlink root");
        let real_config = root.path().join("real-config");
        fs::create_dir_all(&real_config).expect("create real config directory");
        let config = root.path().join("config");
        symlink(&real_config, &config).expect("create config symlink");

        let mut transport = successful_verification_script();
        let error = verify_and_register(
            &config,
            &mut transport,
            &register_request(REGISTER_NEW_PASSWORD, Role::Undergraduate),
        )
        .expect_err("symlinked config directories must be rejected");

        assert_eq!(error.code, 3, "{error:?}");
        assert_eq!(error.kind, "auth", "{error:?}");
        assert!(
            error.message.contains("symlink"),
            "expected a symlink refusal: {error:?}"
        );
        assert_error_redacted(
            &error,
            &[REGISTER_NEW_PASSWORD, REGISTER_TICKET],
            &[
                config.as_path(),
                real_config.as_path(),
                config.join(CREDENTIALS_FILENAME).as_path(),
            ],
        );
        assert!(
            fs::symlink_metadata(&config)
                .expect("inspect config symlink")
                .file_type()
                .is_symlink()
        );
        assert!(
            entry_names(&real_config).is_empty(),
            "registration wrote through the config symlink"
        );
    }

    #[cfg(unix)]
    #[test]
    fn registration_refuses_non_regular_credentials_path() {
        let root = tempdir().expect("create non-regular root");
        let config = root.path().join("config");
        fs::create_dir_all(&config).expect("create config directory");
        let credentials_path = config.join(CREDENTIALS_FILENAME);
        fs::create_dir(&credentials_path).expect("create directory at credentials path");

        let mut transport = successful_verification_script();
        let error = verify_and_register(
            &config,
            &mut transport,
            &register_request(REGISTER_NEW_PASSWORD, Role::Undergraduate),
        )
        .expect_err("non-regular credentials must be rejected");

        assert_eq!(error.code, 3, "{error:?}");
        assert_eq!(error.kind, "auth", "{error:?}");
        assert!(error.message.contains("non-regular"), "{error:?}");
        assert_error_redacted(
            &error,
            &[REGISTER_NEW_PASSWORD, REGISTER_TICKET],
            &[credentials_path.as_path(), config.as_path()],
        );
        assert!(
            fs::symlink_metadata(&credentials_path)
                .expect("inspect credentials path")
                .is_dir()
        );
        assert_eq!(entry_names(&config), vec![CREDENTIALS_FILENAME.to_owned()]);
    }

    #[cfg(unix)]
    #[test]
    fn registration_creates_user_private_config_directory_and_file() {
        let root = tempdir().expect("create missing-directory root");
        let config = root.path().join("config");
        let cache = root.path().join("cache");
        fs::create_dir_all(&cache).expect("create cache directory");

        let mut transport = successful_verification_script();
        verify_and_register(
            &config,
            &mut transport,
            &register_request(REGISTER_NEW_PASSWORD, Role::Undergraduate),
        )
        .expect("registration creates missing config directory");

        assert_eq!(register_mode(&config), 0o700);
        let credentials_path = config.join(CREDENTIALS_FILENAME);
        assert_eq!(register_mode(&credentials_path), 0o600);
        assert_eq!(entry_names(&config), vec![CREDENTIALS_FILENAME.to_owned()]);
        assert!(entry_names(&cache).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn registration_replaces_malformed_credentials_after_verified_login() {
        let root = tempdir().expect("create malformed root");
        let config = root.path().join("config");
        fs::create_dir_all(&config).expect("create config directory");
        let malformed = "TEST-ONLY-MALFORMED-CREDENTIALS-q1w2 { not json";
        let credentials_path = seed_register_file(&config, CREDENTIALS_FILENAME, malformed);

        let mut transport = successful_verification_script();
        verify_and_register(
            &config,
            &mut transport,
            &register_request(REGISTER_NEW_PASSWORD, Role::Undergraduate),
        )
        .expect("verified login repairs malformed credentials");

        let saved: Value = serde_json::from_slice(
            &fs::read(&credentials_path).expect("read repaired credentials"),
        )
        .expect("repaired credentials are JSON");
        assert_eq!(saved["username"], serde_json::json!(REGISTER_USER));
        assert_eq!(saved["password"], serde_json::json!(REGISTER_NEW_PASSWORD));
        assert_eq!(saved["role"], serde_json::json!("undergraduate"));
        let text = fs::read_to_string(&credentials_path).expect("read repaired credentials");
        assert!(!text.contains("TEST-ONLY-MALFORMED-CREDENTIALS-q1w2"));
        assert_eq!(register_mode(&credentials_path), 0o600);
        assert_eq!(entry_names(&config), vec![CREDENTIALS_FILENAME.to_owned()]);
    }

    #[cfg(not(unix))]
    #[test]
    fn registration_fails_closed_without_verified_platform_protections() {
        let root = tempdir().expect("create unsupported-platform root");
        let config = root.path().join("config");
        let mut transport = successful_verification_script();
        let error = verify_and_register(
            &config,
            &mut transport,
            &register_request(REGISTER_NEW_PASSWORD, Role::Undergraduate),
        )
        .expect_err("unsupported platforms must fail closed");

        assert_eq!(error.code, 5, "{error:?}");
        assert_eq!(error.kind, "upstream", "{error:?}");
        assert_error_redacted(
            &error,
            &[REGISTER_NEW_PASSWORD, REGISTER_TICKET],
            &[config.as_path()],
        );
        assert!(
            entry_names(&config).is_empty(),
            "unsupported platform wrote local state"
        );
    }
}
