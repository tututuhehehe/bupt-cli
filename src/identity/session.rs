//! Native identity storage and lazy read-path token access.
//!
//! This module is the production bridge between the reviewed CAS protocol and
//! the local account/cache files. It is always driven by an injected
//! `Transport`; the CLI still chooses `Offline` until the parent wires the
//! bounded transport. No function here reads credentials from the process
//! environment; `credentials.json` is the sole persisted account source.

use super::*;
use crate::transport::Transport;
use std::io::{Read, Write};
use std::path::PathBuf;

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

const SESSION_REVISION_KEY: &str = "session_revision";

struct NativeAccount {
    username: String,
    role: Role,
    revision: String,
}

/// Verify the supplied password, then persist a new native session.
///
/// The password is used exactly once for the CAS form POST. A successful CAS
/// service-ticket issuance is the only path that reaches the local writer.
pub(super) fn login(context: &mut Context<'_>, request: &LoginRequest) -> CliResult<Outcome> {
    let revision = preflight_login(context)?;
    let login = cas::cas_login(context.transport, &request.username, &request.password)?;
    let cookies = login.cookies_json()?;
    if !login.has_usable_cookies() {
        return Err(session_error(
            "CAS login did not produce a reusable local session",
        ));
    }

    let credentials = credentials_bytes(&context.config_dir, request, &revision)?;
    let session = session_bytes(&context.cache_dir, request, &revision, cookies)?;
    commit_login(
        &context.config_dir,
        &context.cache_dir,
        &credentials,
        &session,
    )?;
    Ok(login_outcome(context, request))
}

/// Load a verified native session and return `(access_token, user_id)`.
///
/// A fresh cached token is loaded without writing local state or touching the
/// network. When a token is absent or expired, the persisted CAS cookies are
/// used lazily for one service-ticket/token exchange. The password is never
/// submitted.
pub(super) fn require_ucloud_session(context: &mut Context<'_>) -> CliResult<(String, String)> {
    let (mut data, mut cas_session) = load_bound_session(context)?;

    // A fresh token is already a usable verified session and does not need the
    // CAS cookie jar. Only a token refresh requires usable CAS cookies.
    let reused = cas_session.has_usable_ucloud_token();
    if !reused {
        if !cas_session.has_usable_cookies() {
            return Err(session_error("the saved CAS session has no usable cookies"));
        }
        preflight_session_write(&context.cache_dir)?;
    }

    let (access_token, user_id, token_json) = {
        let token = cas_session.ucloud_token(context.transport)?;
        let (access_token, user_id) = token.identity_pair().ok_or_else(|| {
            CliError::new(
                3,
                "auth",
                "the UCloud token did not identify a user",
                "Run `bupt login` again to verify the account.",
            )
        })?;
        (access_token, user_id, token.as_session_json())
    };

    if !reused {
        let cookies = cas_session.cookies_json()?;
        data.insert("cookies".to_owned(), cookies);
        data.insert("ucloud".to_owned(), token_json.clone());
        data.insert(
            "user".to_owned(),
            user_from_token(data.get("user"), &token_json),
        );
        data.insert(
            "updated_at".to_owned(),
            json!(iso_epoch(unix_now()).unwrap_or_default()),
        );
        write_session_json(&context.cache_dir, &data)?;
    }

    Ok((access_token, user_id))
}

/// An owned, verified native CAS session for read-only protocol consumers.
///
/// It can request tickets only for `CasService` values; it has no password and
/// never performs the login form POST. All cookie/ticket state is in memory.
pub(crate) struct OwnedCasSession {
    cas: cas::CasLogin,
    data: Map<String, Value>,
}

impl OwnedCasSession {
    pub(crate) fn ticket_for(
        &mut self,
        transport: &mut dyn Transport,
        service: cas::CasService,
    ) -> CliResult<String> {
        self.cas.ticket_for(transport, service)
    }

    pub(crate) fn has_usable_cookies(&self) -> bool {
        self.cas.has_usable_cookies()
    }

    pub(crate) fn cached_ucloud_credentials(&self) -> Option<(String, String)> {
        self.cas.cached_ucloud_credentials()
    }

    pub(crate) fn exchange_ucloud_ticket(
        &mut self,
        transport: &mut dyn Transport,
        ticket: &str,
    ) -> CliResult<()> {
        self.cas.exchange_ucloud_ticket(transport, ticket)?;
        Ok(())
    }

    pub(super) fn cookie_header_for(&self, url: &str) -> Option<String> {
        self.cas.cookie_header_for_authenticated(url)
    }

    pub(crate) fn electricity_cookie(&self, url: &str) -> Option<String> {
        let path = url.strip_prefix("https://app.bupt.edu.cn/buptdf/wap/default/")?;
        matches!(path, "chong" | "part" | "floor" | "drom" | "search")
            .then(|| self.cas.cookie_header_for_authenticated(url))
            .flatten()
    }

    pub(super) fn mobile_token(&self) -> Option<(String, Option<f64>)> {
        let mobile = self.data.get("mobile_jwgl")?.as_object()?;
        let token = mobile
            .get("token")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())?
            .to_owned();
        let expires_at = mobile.get("expires_at").and_then(number_value);
        Some((token, expires_at))
    }
}

/// Load only the current owner's native, revision-bound CAS session.
///
/// Public callers get no password, no legacy v1 activation and no login path.
pub(crate) fn load_owned_cas_session(context: &Context<'_>) -> CliResult<OwnedCasSession> {
    let (data, cas_session) = load_bound_session(context)?;
    Ok(OwnedCasSession {
        cas: cas_session,
        data,
    })
}

fn load_bound_session(context: &Context<'_>) -> CliResult<(Map<String, Value>, cas::CasLogin)> {
    ensure_supported_platform()?;
    let account = load_native_account(&context.config_dir)?;
    validate_owner_relationship(&context.config_dir, &context.cache_dir)?;
    let data = load_native_session(&context.cache_dir)?;
    validate_binding(&account, &data)?;
    let empty_cookies = Value::Array(Vec::new());
    let cookies = data.get("cookies").unwrap_or(&empty_cookies);
    let cas_session = cas::CasLogin::from_persisted(cookies, data.get("ucloud"))?;
    Ok((data, cas_session))
}

fn login_outcome(context: &Context<'_>, request: &LoginRequest) -> Outcome {
    let data = json!({
        "username": request.username,
        "role": role_name(request.role),
        "role_label": role_label(request.role),
        "user": {
            "username": request.username,
            "user_id": Value::Null,
            "user_name": Value::Null,
            "real_name": Value::Null,
        },
        "ucloud_token": false,
        "mobile_jwgl": {
            "attempted": false,
            "ok": false,
            "reason": "not part of this migration milestone",
        },
        "cas_logins_this_run": 1,
        "credentials_path": path_string(&context.config_dir.join(CREDENTIALS_FILENAME)),
    });
    Outcome {
        data,
        code: 0,
        diagnostic: None,
    }
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Graduate => "graduate",
        Role::Undergraduate => "undergraduate",
    }
}

fn role_label(role: Role) -> &'static str {
    match role {
        Role::Graduate => "研究生",
        Role::Undergraduate => "本科生",
    }
}

fn credentials_bytes(
    config_dir: &Path,
    request: &LoginRequest,
    revision: &str,
) -> CliResult<Vec<u8>> {
    let mut credentials = read_credentials_for_update(config_dir)?;
    credentials.insert(
        "username".to_owned(),
        Value::String(request.username.clone()),
    );
    credentials.insert(
        "password".to_owned(),
        Value::String(request.password.clone()),
    );
    credentials.insert("role".to_owned(), json!(role_name(request.role)));
    credentials.insert(
        SESSION_REVISION_KEY.to_owned(),
        Value::String(revision.to_owned()),
    );
    let mut bytes = serde_json::to_vec_pretty(&Value::Object(credentials))
        .map_err(|_| local_error("cannot encode the local credentials file"))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn session_bytes(
    cache_dir: &Path,
    request: &LoginRequest,
    revision: &str,
    cookies: Value,
) -> CliResult<Vec<u8>> {
    let previous = read_old_session(cache_dir)?;
    let same_owner = previous
        .as_ref()
        .is_some_and(|session| session_owner_matches(session, request));
    let now = iso_epoch(unix_now()).unwrap_or_default();
    let created_at = previous
        .as_ref()
        .filter(|_| same_owner)
        .and_then(|session| session.get("created_at"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or(&now);
    let cas_logins_total = previous
        .as_ref()
        .filter(|_| same_owner)
        .and_then(|session| session.get("cas_logins_total"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .saturating_add(1);
    let session = json!({
        "version": CACHE_VERSION,
        "username": request.username,
        "role": role_name(request.role),
        SESSION_REVISION_KEY: revision,
        "created_at": created_at,
        "updated_at": now,
        "cookies": cookies,
        "cas_logins_total": cas_logins_total,
    });
    let mut bytes = serde_json::to_vec_pretty(&session)
        .map_err(|_| local_error("cannot encode the local session cache"))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn session_owner_matches(session: &Map<String, Value>, request: &LoginRequest) -> bool {
    session
        .get("username")
        .and_then(Value::as_str)
        .map(str::trim)
        == Some(request.username.as_str())
        && session
            .get("role")
            .and_then(Value::as_str)
            .and_then(Role::parse)
            == Some(request.role)
}

fn read_credentials_for_update(config_dir: &Path) -> CliResult<Map<String, Value>> {
    let path = config_dir.join(CREDENTIALS_FILENAME);
    ensure_replaceable(&path)?;
    let Ok(bytes) = fs::read(&path) else {
        return Ok(Map::new());
    };
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Object(object)) => Ok(object),
        _ => Ok(Map::new()),
    }
}

fn read_old_session(cache_dir: &Path) -> CliResult<Option<Map<String, Value>>> {
    let path = cache_dir.join(SESSION_FILENAME);
    ensure_replaceable(&path)?;
    let Ok(bytes) = fs::read(&path) else {
        return Ok(None);
    };
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Object(object)) => Ok(Some(object)),
        _ => Ok(None),
    }
}

fn load_native_account(config_dir: &Path) -> CliResult<NativeAccount> {
    validate_private_directory(config_dir)?;
    let path = config_dir.join(CREDENTIALS_FILENAME);
    let credentials = read_private_json(&path, "credentials")?;
    let username = credentials
        .get("username")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| session_error("BUPT credentials are not configured"))?;
    let has_password = credentials
        .get("password")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty());
    if !has_password {
        return Err(session_error("BUPT credentials are not configured"));
    }
    let role = credentials
        .get("role")
        .and_then(Value::as_str)
        .and_then(Role::parse)
        .ok_or_else(|| session_error("the saved credentials have no academic role"))?;
    let revision = credentials
        .get(SESSION_REVISION_KEY)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| session_error("the saved credentials have no session revision"))?;
    Ok(NativeAccount {
        username: username.to_owned(),
        role,
        revision: revision.to_owned(),
    })
}

fn load_native_session(cache_dir: &Path) -> CliResult<Map<String, Value>> {
    validate_private_directory(cache_dir)?;
    let path = cache_dir.join(SESSION_FILENAME);
    read_private_json(&path, "session cache")
}

fn validate_binding(account: &NativeAccount, data: &Map<String, Value>) -> CliResult<()> {
    if data.get("version").and_then(Value::as_u64) != Some(CACHE_VERSION) {
        return Err(session_error(
            "the saved session is not a native verified session",
        ));
    }
    let username = data
        .get("username")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if username != Some(account.username.as_str()) {
        return Err(session_error(
            "the saved session belongs to a different account",
        ));
    }
    let role = data
        .get("role")
        .and_then(Value::as_str)
        .and_then(Role::parse);
    if role != Some(account.role) {
        return Err(session_error(
            "the saved session belongs to a different role",
        ));
    }
    let revision = data
        .get(SESSION_REVISION_KEY)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if revision != Some(account.revision.as_str()) {
        return Err(session_error(
            "the saved session is not bound to the current credential revision",
        ));
    }
    Ok(())
}

fn user_from_token(existing: Option<&Value>, token: &Value) -> Value {
    let mut user = existing
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let Some(token) = token.as_object() else {
        return Value::Object(user);
    };
    for field in PUBLIC_USER_FIELDS {
        let Some(value) = token.get(*field).filter(|value| !value.is_null()) else {
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
            user.insert((*field).to_owned(), scalar);
        }
    }
    Value::Object(user)
}

fn read_private_json(path: &Path, label: &str) -> CliResult<Map<String, Value>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(session_error("a verified local session is not configured"));
        }
        Err(_) => return Err(local_error("cannot inspect the local identity state")),
    };
    if metadata.file_type().is_symlink() {
        return Err(local_error(
            "refusing to use a symlink as local identity state",
        ));
    }
    if !metadata.file_type().is_file() {
        return Err(local_error(
            "the local identity state is not a regular file",
        ));
    }
    #[cfg(unix)]
    {
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(CliError::new(
                3,
                "auth",
                format!("the local {label} file is not private"),
                "Run `bupt login` again to recreate it with owner-only permissions.",
            ));
        }
        if let Some(directory) = path.parent() {
            if let Ok(directory) = fs::symlink_metadata(directory) {
                if metadata.uid() != directory.uid() {
                    return Err(local_error(
                        "the local identity state is owned by another user",
                    ));
                }
            }
        }
    }
    let bytes = fs::read(path).map_err(|_| local_error("cannot read the local identity state"))?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| session_error("the saved local identity state is malformed"))?;
    match value {
        Value::Object(object) => Ok(object),
        _ => Err(session_error(
            "the saved local identity state must be a JSON object",
        )),
    }
}

fn ensure_replaceable(path: &Path) -> CliResult<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(local_error("cannot inspect the local identity state")),
    };
    if metadata.file_type().is_symlink() {
        return Err(local_error(
            "refusing to replace a symlink in the local identity state",
        ));
    }
    if !metadata.file_type().is_file() {
        return Err(local_error(
            "refusing to replace a non-regular local identity file",
        ));
    }
    #[cfg(unix)]
    if let Some(directory) = path.parent() {
        if let Ok(directory) = fs::symlink_metadata(directory) {
            if metadata.uid() != directory.uid() {
                return Err(local_error(
                    "the local identity state is owned by another user",
                ));
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn preflight_login(context: &Context<'_>) -> CliResult<String> {
    prepare_private_directory(&context.config_dir)?;
    prepare_private_directory(&context.cache_dir)?;
    validate_owner_relationship(&context.config_dir, &context.cache_dir)?;
    ensure_replaceable(&context.config_dir.join(CREDENTIALS_FILENAME))?;
    ensure_replaceable(&context.cache_dir.join(SESSION_FILENAME))?;
    probe_private_directory(&context.config_dir)?;
    probe_private_directory(&context.cache_dir)?;
    new_session_revision()
}

#[cfg(not(unix))]
fn preflight_login(_context: &Context<'_>) -> CliResult<String> {
    Err(platform_error())
}

#[cfg(unix)]
fn preflight_session_write(cache_dir: &Path) -> CliResult<()> {
    validate_private_directory(cache_dir)?;
    ensure_replaceable(&cache_dir.join(SESSION_FILENAME))?;
    probe_private_directory(cache_dir)
}

#[cfg(not(unix))]
fn preflight_session_write(_cache_dir: &Path) -> CliResult<()> {
    Err(platform_error())
}

#[cfg(unix)]
fn prepare_private_directory(path: &Path) -> CliResult<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path)
                .map_err(|_| local_error("cannot create the local state directory"))?;
            let metadata = fs::symlink_metadata(path)
                .map_err(|_| local_error("cannot inspect the local state directory"))?;
            if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
                return Err(local_error(
                    "refusing to use an unsafe local state directory",
                ));
            }
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .map_err(|_| local_error("cannot secure the local state directory"))?;
            return validate_private_directory(path);
        }
        Err(_) => return Err(local_error("cannot inspect the local state directory")),
    };
    if metadata.file_type().is_symlink() {
        return Err(local_error(
            "refusing to use a symlink as the local state directory",
        ));
    }
    if !metadata.file_type().is_dir() {
        return Err(local_error(
            "the local state directory path is not a directory",
        ));
    }
    validate_private_directory(path)
}

#[cfg(unix)]
fn validate_private_directory(path: &Path) -> CliResult<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return Err(local_error("cannot inspect the local state directory")),
    };
    if metadata.file_type().is_symlink() {
        return Err(local_error(
            "refusing to use a symlink as the local state directory",
        ));
    }
    if !metadata.file_type().is_dir() {
        return Err(local_error(
            "the local state directory path is not a directory",
        ));
    }
    if metadata.permissions().mode() & 0o022 != 0 {
        return Err(local_error(
            "the local state directory is writable by another user",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_directory(_path: &Path) -> CliResult<()> {
    Err(platform_error())
}

#[cfg(unix)]
fn validate_owner_relationship(config_dir: &Path, cache_dir: &Path) -> CliResult<()> {
    let config = fs::symlink_metadata(config_dir)
        .map_err(|_| local_error("cannot inspect the local state directory"))?;
    let cache = fs::symlink_metadata(cache_dir)
        .map_err(|_| local_error("cannot inspect the local state directory"))?;
    if config.uid() != cache.uid() {
        return Err(local_error(
            "the local identity directories are owned by different users",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_owner_relationship(_config_dir: &Path, _cache_dir: &Path) -> CliResult<()> {
    Err(platform_error())
}

#[cfg(unix)]
fn probe_private_directory(directory: &Path) -> CliResult<()> {
    for attempt in 0..100 {
        let temporary = directory.join(format!(".bupt-preflight.{}.{attempt}", std::process::id()));
        let mut file = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(local_error("cannot write to the local state directory")),
        };
        let result = (|| -> std::io::Result<()> {
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            file.write_all(b"")?;
            file.sync_all()
        })();
        drop(file);
        let removed = fs::remove_file(&temporary).is_ok();
        if result.is_err() || !removed {
            return Err(local_error("cannot use the local state directory"));
        }
        return Ok(());
    }
    Err(local_error(
        "cannot allocate a temporary file in the local state directory",
    ))
}

#[cfg(unix)]
fn new_session_revision() -> CliResult<String> {
    let mut bytes = [0u8; 32];
    fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|_| local_error("cannot create a private session revision"))?;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut revision = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        revision.push(HEX[(byte >> 4) as usize] as char);
        revision.push(HEX[(byte & 0x0f) as usize] as char);
    }
    Ok(revision)
}

#[cfg(not(unix))]
fn new_session_revision() -> CliResult<String> {
    Err(platform_error())
}

#[cfg(unix)]
fn commit_login(
    config_dir: &Path,
    cache_dir: &Path,
    credentials: &[u8],
    session: &[u8],
) -> CliResult<()> {
    // Stage both files before replacing either one. If staging fails, both old
    // files are untouched. The commits are deliberately ordered credentials
    // first: a failed session commit leaves a revision the old session cannot
    // satisfy, rather than leaving a new session paired with old credentials.
    let mut staged_session =
        StagedWrite::stage(cache_dir, &cache_dir.join(SESSION_FILENAME), session)?;
    let mut staged_credentials = StagedWrite::stage(
        config_dir,
        &config_dir.join(CREDENTIALS_FILENAME),
        credentials,
    )?;

    staged_credentials.commit().map_err(|_| {
        CliError::new(
            5,
            "upstream",
            "CAS verification succeeded but the local credentials could not be committed",
            "The previous credentials and session were left in place; inspect local permissions and retry `bupt login`.",
        )
    })?;
    if staged_session.commit().is_err() {
        return Err(CliError::new(
            5,
            "upstream",
            "CAS verification succeeded and new credentials were saved, but the local session could not be committed",
            "Run `bupt login` again; the previous session is no longer accepted for the new credential revision.",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn commit_login(
    _config_dir: &Path,
    _cache_dir: &Path,
    _credentials: &[u8],
    _session: &[u8],
) -> CliResult<()> {
    Err(platform_error())
}

#[cfg(unix)]
fn write_session_json(cache_dir: &Path, data: &Map<String, Value>) -> CliResult<()> {
    let mut bytes = serde_json::to_vec_pretty(&Value::Object(data.clone()))
        .map_err(|_| local_error("cannot encode the local session cache"))?;
    bytes.push(b'\n');
    let mut staged = StagedWrite::stage(cache_dir, &cache_dir.join(SESSION_FILENAME), &bytes)?;
    staged.commit().map_err(|_| {
        CliError::new(
            5,
            "upstream",
            "the UCloud token was refreshed but the local session could not be updated",
            "Retry the command; if it persists, inspect local permissions and run `bupt login`.",
        )
    })
}

#[cfg(not(unix))]
fn write_session_json(_cache_dir: &Path, _data: &Map<String, Value>) -> CliResult<()> {
    Err(platform_error())
}

#[cfg(unix)]
struct StagedWrite {
    destination: PathBuf,
    temporary: PathBuf,
    committed: bool,
}

#[cfg(unix)]
impl StagedWrite {
    fn stage(directory: &Path, destination: &Path, bytes: &[u8]) -> CliResult<Self> {
        validate_private_directory(directory)?;
        ensure_replaceable(destination)?;
        for attempt in 0..100 {
            let temporary = directory.join(format!(".bupt-write.{}.{attempt}", std::process::id()));
            let mut file = match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)
            {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(local_error("cannot stage the local identity state")),
            };
            let result = (|| -> std::io::Result<()> {
                file.set_permissions(fs::Permissions::from_mode(0o600))?;
                file.write_all(bytes)?;
                file.sync_all()
            })();
            drop(file);
            if result.is_err() {
                let _ = fs::remove_file(&temporary);
                return Err(local_error("cannot stage the local identity state"));
            }
            return Ok(Self {
                destination: destination.to_owned(),
                temporary,
                committed: false,
            });
        }
        Err(local_error(
            "cannot allocate a temporary file for the local identity state",
        ))
    }

    fn commit(&mut self) -> CliResult<()> {
        ensure_replaceable(&self.destination)?;
        fs::rename(&self.temporary, &self.destination)
            .map_err(|_| local_error("cannot replace the local identity state"))?;
        self.committed = true;
        Ok(())
    }
}

#[cfg(unix)]
impl Drop for StagedWrite {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.temporary);
        }
    }
}

#[cfg(unix)]
fn ensure_supported_platform() -> CliResult<()> {
    Ok(())
}

#[cfg(not(unix))]
fn ensure_supported_platform() -> CliResult<()> {
    Err(platform_error())
}

fn local_error(message: impl Into<String>) -> CliError {
    CliError::new(
        3,
        "auth",
        message,
        "Check the local identity state permissions and retry.",
    )
}

fn session_error(message: impl Into<String>) -> CliError {
    CliError::new(
        3,
        "auth",
        message,
        "Run `bupt login` to create a verified local session.",
    )
}

#[cfg(not(unix))]
fn platform_error() -> CliError {
    CliError::new(
        5,
        "upstream",
        "secure local identity storage is not implemented on this platform",
        "Use a supported Unix platform; no local state was changed.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::Response;
    use crate::transport::testing::Scripted;
    use cookie_store::CookieStore;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::Duration;
    use tempfile::{TempDir, tempdir};
    use url::Url;

    #[cfg(unix)]
    use std::os::unix::fs::{PermissionsExt, symlink};

    const USER: &str = "2026000001";
    const OLD_PASSWORD: &str = "TEST-ONLY-OLD-PASSWORD-1a2b";
    const NEW_PASSWORD: &str = "TEST-ONLY-NEW-PASSWORD-3c4d";
    const LOGIN_URL: &str = "https://auth.bupt.edu.cn/authserver/login";
    const EXECUTION: &str = "TEST-ONLY-EXECUTION-9i0j";
    const CAS_COOKIE: &str = "TEST-ONLY-CAS-SESSION-e5f6";
    const CASTGC: &str = "TEST-ONLY-CASTGC-m3n4";
    const TICKET: &str = "ST-TEST-ONLY-TICKET-g7h8";
    const TICKET_URL: &str = "https://ucloud.bupt.edu.cn/uclass?ticket=ST-TEST-ONLY-TICKET-g7h8";
    const TOKEN_URL: &str = "https://apiucloud.bupt.edu.cn/ykt-basics/oauth/token";
    const ACCESS_TOKEN: &str = "TEST-ONLY-ACCESS-TOKEN-c9e3";
    const REFRESH_TOKEN: &str = "TEST-ONLY-REFRESH-TOKEN-f1g2";
    const OLD_ACCESS_TOKEN: &str = "TEST-ONLY-OLD-ACCESS-TOKEN-h3i4";
    const SESSION_SECRET: &str = "TEST-ONLY-SESSION-SECRET-k1l2";
    const USER_ID: &str = "42";
    const OLD_REVISION: &str = "TEST-ONLY-OLD-REVISION-a1b2c3";
    const NEW_REVISION: &str = "TEST-ONLY-NEW-REVISION-d4e5f6";

    struct Sandbox {
        root: TempDir,
    }

    impl Sandbox {
        fn new() -> Self {
            Self {
                root: tempdir().expect("create synthetic state root"),
            }
        }

        fn config_dir(&self) -> PathBuf {
            self.root.path().join("config")
        }

        fn cache_dir(&self) -> PathBuf {
            self.root.path().join("cache")
        }

        fn context<'a>(&self, transport: &'a mut Scripted) -> Context<'a> {
            Context {
                home_dir: self.root.path().join("home"),
                config_dir: self.config_dir(),
                cache_dir: self.cache_dir(),
                timeout: Duration::from_secs(20),
                transport,
            }
        }
    }

    fn response(status: u16, headers: &[(&str, &str)], body: &str) -> Response {
        Response {
            status,
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
            body: body.as_bytes().to_vec(),
        }
    }

    fn enqueue(
        transport: &mut Scripted,
        method: &'static str,
        url: &str,
        status: u16,
        headers: &[(&str, &str)],
        body: &str,
    ) {
        transport
            .expected
            .push_back((method, url.to_owned(), Ok(response(status, headers, body))));
    }

    fn login_page_body() -> String {
        format!(
            "<html><form><input value='{EXECUTION}' type='hidden' name='execution'></form></html>"
        )
    }

    fn login_page() -> Response {
        let cookie = format!("CAS_SESSION={CAS_COOKIE}; Path=/authserver; Secure; HttpOnly");
        response(200, &[("Set-Cookie", cookie.as_str())], &login_page_body())
    }

    fn successful_login_script() -> Scripted {
        let mut transport = Scripted::default();
        transport
            .expected
            .push_back(("GET", LOGIN_URL.to_owned(), Ok(login_page())));
        enqueue(
            &mut transport,
            "POST",
            LOGIN_URL,
            302,
            &[("Location", TICKET_URL)],
            "",
        );
        enqueue(
            &mut transport,
            "GET",
            TICKET_URL,
            200,
            &[],
            "<html><body>synthetic UCloud workspace</body></html>",
        );
        enqueue(
            &mut transport,
            "GET",
            CAS_SERVICE_URL,
            302,
            &[("Location", TICKET_URL)],
            "",
        );
        transport
    }

    fn successful_token_script() -> Scripted {
        let mut transport = Scripted::default();
        enqueue(
            &mut transport,
            "GET",
            CAS_SERVICE_URL,
            302,
            &[("Location", TICKET_URL)],
            "",
        );
        enqueue(
            &mut transport,
            "POST",
            TOKEN_URL,
            200,
            &[],
            &format!(
                "{{\"access_token\":\"{ACCESS_TOKEN}\",\"refresh_token\":\"{REFRESH_TOKEN}\",\"user_id\":\"{USER_ID}\",\"user_name\":\"{USER}\",\"real_name\":\"Synthetic Tester\"}}"
            ),
        );
        transport
    }

    fn cookies_json(entries: &[(&str, &str)]) -> Value {
        let mut store = CookieStore::default();
        for (origin, raw) in entries {
            let url = Url::parse(origin).expect("parse synthetic cookie origin");
            store
                .parse(raw, &url)
                .expect("insert synthetic cookie into maintained store");
        }
        let mut bytes = Vec::new();
        cookie_store::serde::json::save_incl_expired_and_nonpersistent(&store, &mut bytes)
            .expect("serialize synthetic cookie store");
        serde_json::from_slice(&bytes).expect("parse synthetic cookie store JSON")
    }

    fn native_cookies() -> Value {
        let raw = format!("CASTGC={CASTGC}; Path=/authserver; Secure; HttpOnly");
        cookies_json(&[(LOGIN_URL, raw.as_str())])
    }

    fn expired_native_cookies() -> Value {
        let mut cookies = native_cookies();
        cookies[0]["expires"] = serde_json::json!({"AtUtc": "2000-01-01T00:00:00Z"});
        cookies
    }

    fn request(password: &str) -> LoginRequest {
        LoginRequest {
            username: USER.to_owned(),
            password: password.to_owned(),
            role: Role::Graduate,
        }
    }

    fn write_private(path: &Path, value: &Value) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create synthetic state directory");
            #[cfg(unix)]
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                .expect("secure synthetic state directory");
        }
        let mut bytes = serde_json::to_vec_pretty(value).expect("serialize synthetic state");
        bytes.push(b'\n');
        fs::write(path, bytes).expect("write synthetic state");
        #[cfg(unix)]
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .expect("secure synthetic state file");
    }

    fn read_json(path: &Path) -> Value {
        let bytes = fs::read(path).expect("read synthetic state");
        serde_json::from_slice(&bytes).expect("parse synthetic state")
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("set synthetic mode");
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        fs::metadata(path)
            .expect("stat synthetic state")
            .permissions()
            .mode()
            & 0o777
    }

    fn seed_native_state(sandbox: &Sandbox, password: &str, revision: &str) {
        write_private(
            &sandbox.config_dir().join(CREDENTIALS_FILENAME),
            &serde_json::json!({
                "username": USER,
                "password": password,
                "role": "graduate",
                SESSION_REVISION_KEY: revision,
                "unrelated": {"keep": "yes"},
            }),
        );
        write_private(
            &sandbox.cache_dir().join(SESSION_FILENAME),
            &serde_json::json!({
                "version": CACHE_VERSION,
                "username": USER,
                "role": "graduate",
                SESSION_REVISION_KEY: revision,
                "created_at": "2026-01-01T00:00:00Z",
                "updated_at": "2026-01-02T03:04:05Z",
                "cookies": native_cookies(),
                "ucloud": {
                    "access_token": OLD_ACCESS_TOKEN,
                    "refresh_token": REFRESH_TOKEN,
                    "user_id": USER_ID,
                    "user_name": USER,
                    "real_name": "Synthetic Tester",
                    "jwt_payload": {},
                    "expires_at": 4102444800.0,
                },
                "cas_logins_total": 2,
            }),
        );
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
                "error leaked a synthetic path: {rendered}"
            );
        }
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

    #[test]
    fn login_verifies_once_and_writes_private_native_state() {
        let sandbox = Sandbox::new();
        let mut transport = successful_login_script();
        let request = request(NEW_PASSWORD);
        let outcome = {
            let mut context = sandbox.context(&mut transport);
            login(&mut context, &request).expect("synthetic login succeeds")
        };
        assert_eq!(outcome.code, 0);
        assert_eq!(outcome.data["username"], USER);
        assert_eq!(outcome.data["role"], "graduate");
        assert_eq!(
            outcome.data["credentials_path"],
            path_string(&sandbox.config_dir().join(CREDENTIALS_FILENAME))
        );
        assert!(!outcome.data.to_string().contains(NEW_PASSWORD));
        assert_eq!(transport.calls.len(), 4);
        assert!(transport.calls.iter().all(|call| call.url != TOKEN_URL));

        let post = transport
            .calls
            .iter()
            .find(|call| call.method == "POST" && call.url == LOGIN_URL)
            .expect("login posts the form once");
        let post_body = String::from_utf8_lossy(&post.body);
        assert!(post_body.contains(NEW_PASSWORD));
        assert!(post_body.contains(USER));
        assert!(post_body.contains(EXECUTION));

        let credentials_path = sandbox.config_dir().join(CREDENTIALS_FILENAME);
        let session_path = sandbox.cache_dir().join(SESSION_FILENAME);
        let credentials = read_json(&credentials_path);
        let session = read_json(&session_path);
        let revision = credentials[SESSION_REVISION_KEY]
            .as_str()
            .expect("credentials have a revision")
            .to_owned();
        assert_eq!(revision.len(), 64);
        assert_eq!(credentials["username"], USER);
        assert_eq!(credentials["password"], NEW_PASSWORD);
        assert_eq!(credentials["role"], "graduate");
        assert_eq!(session["version"], CACHE_VERSION);
        assert_eq!(session["username"], USER);
        assert_eq!(session["role"], "graduate");
        assert_eq!(session[SESSION_REVISION_KEY], revision);
        assert_eq!(session["cas_logins_total"], 1);
        assert!(session["cookies"].as_array().is_some_and(|v| !v.is_empty()));
        assert!(session.get("ucloud").is_none());
        let session_text = session.to_string();
        let credentials_text = credentials.to_string();
        for secret in [
            NEW_PASSWORD,
            TICKET,
            "ticket",
            "final_url",
            "redirect_url",
            "access_token",
        ] {
            assert!(!session_text.contains(secret), "session leaked {secret}");
        }
        for secret in [
            "cookies",
            "ticket",
            "final_url",
            "redirect_url",
            "access_token",
        ] {
            assert!(
                !credentials_text.contains(secret),
                "credentials leaked {secret}"
            );
        }

        #[cfg(unix)]
        {
            assert_eq!(mode(&sandbox.config_dir()), 0o700);
            assert_eq!(mode(&sandbox.cache_dir()), 0o700);
            assert_eq!(mode(&credentials_path), 0o600);
            assert_eq!(mode(&session_path), 0o600);
        }

        let profile = load_profile(&sandbox.config_dir())
            .expect("load new profile")
            .expect("profile written");
        let snapshot = load_session(&sandbox.cache_dir(), Some(&profile));
        assert!(
            snapshot.accepted,
            "new session rejected: {}",
            snapshot.error
        );
        let cookie_view = cookie_status(snapshot.data.get("cookies"), unix_now());
        assert_eq!(cookie_view.total, 1);
        assert_eq!(cookie_view.usable, 1);
        assert!(cookie_view.earliest_expiry.is_none());
    }

    #[test]
    fn require_loads_native_state_from_disk_and_reuses_a_fresh_token() {
        let sandbox = Sandbox::new();
        let mut login_transport = successful_login_script();
        let request = request(NEW_PASSWORD);
        {
            let mut context = sandbox.context(&mut login_transport);
            login(&mut context, &request).expect("synthetic login succeeds");
        }
        let credentials_path = sandbox.config_dir().join(CREDENTIALS_FILENAME);
        let session_path = sandbox.cache_dir().join(SESSION_FILENAME);
        let credentials_before = fs::read(&credentials_path).expect("read credentials");
        let session_before = fs::read(&session_path).expect("read session");

        let mut token_transport = successful_token_script();
        let pair = {
            let mut context = sandbox.context(&mut token_transport);
            require_ucloud_session(&mut context).expect("lazy token exchange succeeds")
        };
        assert_eq!(pair, (ACCESS_TOKEN.to_owned(), USER_ID.to_owned()));
        assert_eq!(token_transport.calls.len(), 2);
        let service_call = token_transport
            .calls
            .iter()
            .find(|call| call.url == CAS_SERVICE_URL)
            .expect("service ticket is requested");
        assert!(
            service_call
                .headers
                .iter()
                .any(|(name, value)| name.eq_ignore_ascii_case("cookie")
                    && value.contains(CAS_COOKIE))
        );
        let token_call = token_transport
            .calls
            .iter()
            .find(|call| call.url == TOKEN_URL)
            .expect("token exchange is requested");
        assert!(
            token_call
                .headers
                .iter()
                .all(|(name, _)| !name.eq_ignore_ascii_case("cookie"))
        );
        assert!(!String::from_utf8_lossy(&token_call.body).contains(NEW_PASSWORD));
        assert!(
            token_transport
                .calls
                .iter()
                .all(|call| call.method != "POST" || call.url != LOGIN_URL)
        );
        assert_eq!(
            fs::read(&credentials_path).expect("read credentials"),
            credentials_before
        );

        let session = read_json(&session_path);
        assert_eq!(session["ucloud"]["access_token"], ACCESS_TOKEN);
        assert_eq!(session["ucloud"]["user_id"], USER_ID);
        assert_eq!(session["cas_logins_total"], 1);
        assert!(!session.to_string().contains(TICKET));
        assert_eq!(session["cookies"], read_json(&session_path)["cookies"]);
        assert_eq!(session["user"]["user_id"], USER_ID);
        #[cfg(unix)]
        assert_eq!(mode(&session_path), 0o600);

        let session_after_first_require = fs::read(&session_path).expect("read session");
        let mut empty_transport = Scripted::default();
        let reused = {
            let mut context = sandbox.context(&mut empty_transport);
            require_ucloud_session(&mut context).expect("cached token is reused")
        };
        assert_eq!(reused, (ACCESS_TOKEN.to_owned(), USER_ID.to_owned()));
        assert!(empty_transport.calls.is_empty());
        assert_eq!(
            fs::read(&session_path).expect("read reused session"),
            session_after_first_require
        );
        assert_ne!(
            fs::read(&session_path).expect("read reused session"),
            session_before
        );
    }

    #[test]
    fn require_rejects_a_different_credential_revision_before_network() {
        let sandbox = Sandbox::new();
        seed_native_state(&sandbox, OLD_PASSWORD, OLD_REVISION);
        let credentials_path = sandbox.config_dir().join(CREDENTIALS_FILENAME);
        let mut credentials = read_json(&credentials_path);
        credentials[SESSION_REVISION_KEY] = serde_json::json!(NEW_REVISION);
        write_private(&credentials_path, &credentials);

        let mut transport = Scripted::default();
        let error = {
            let mut context = sandbox.context(&mut transport);
            require_ucloud_session(&mut context)
                .err()
                .expect("stale revision is rejected")
        };
        assert_eq!(error.code, 3);
        assert_eq!(error.kind, "auth");
        assert!(transport.calls.is_empty());
        assert_error_redacted(
            &error,
            &[OLD_PASSWORD, OLD_ACCESS_TOKEN, SESSION_SECRET],
            &[&credentials_path],
        );
    }

    #[test]
    fn require_rejects_missing_password_before_network() {
        let sandbox = Sandbox::new();
        seed_native_state(&sandbox, OLD_PASSWORD, OLD_REVISION);
        let credentials_path = sandbox.config_dir().join(CREDENTIALS_FILENAME);
        let mut credentials = read_json(&credentials_path);
        credentials
            .as_object_mut()
            .expect("object")
            .remove("password");
        write_private(&credentials_path, &credentials);

        let mut transport = Scripted::default();
        let error = {
            let mut context = sandbox.context(&mut transport);
            require_ucloud_session(&mut context)
                .err()
                .expect("missing password is rejected")
        };
        assert_eq!(error.code, 3);
        assert_eq!(error.kind, "auth");
        assert!(transport.calls.is_empty());
    }

    #[test]
    fn require_rejects_foreign_cookie_scope_before_network() {
        let sandbox = Sandbox::new();
        seed_native_state(&sandbox, OLD_PASSWORD, OLD_REVISION);
        let session_path = sandbox.cache_dir().join(SESSION_FILENAME);
        let mut session = read_json(&session_path);
        session["cookies"] = cookies_json(&[(
            "https://evil.example/",
            "EVIL_SESSION=TEST-ONLY-EVIL-COOKIE; Path=/; Secure",
        )]);
        write_private(&session_path, &session);

        let mut transport = Scripted::default();
        let error = {
            let mut context = sandbox.context(&mut transport);
            require_ucloud_session(&mut context)
                .err()
                .expect("foreign cookie scope is rejected")
        };
        assert_eq!(error.code, 3);
        assert_eq!(error.kind, "auth");
        assert!(transport.calls.is_empty());
        assert_error_redacted(
            &error,
            &[OLD_PASSWORD, OLD_ACCESS_TOKEN, "TEST-ONLY-EVIL-COOKIE"],
            &[&session_path],
        );
    }

    #[test]
    fn require_rejects_expired_cookies_without_a_token_before_network() {
        let sandbox = Sandbox::new();
        seed_native_state(&sandbox, OLD_PASSWORD, OLD_REVISION);
        let session_path = sandbox.cache_dir().join(SESSION_FILENAME);
        let mut session = read_json(&session_path);
        session["cookies"] = expired_native_cookies();
        session.as_object_mut().expect("object").remove("ucloud");
        write_private(&session_path, &session);

        let mut transport = Scripted::default();
        let error = {
            let mut context = sandbox.context(&mut transport);
            require_ucloud_session(&mut context)
                .err()
                .expect("expired cookies cannot be used for a refresh")
        };
        assert_eq!(error.code, 3);
        assert_eq!(error.kind, "auth");
        assert!(transport.calls.is_empty());
    }

    #[test]
    fn require_rejects_an_unapproved_cookie_path_before_network() {
        let sandbox = Sandbox::new();
        seed_native_state(&sandbox, OLD_PASSWORD, OLD_REVISION);
        let session_path = sandbox.cache_dir().join(SESSION_FILENAME);
        let mut session = read_json(&session_path);
        session["cookies"] = cookies_json(&[(
            LOGIN_URL,
            "CASTGC=TEST-ONLY-PATH-COOKIE; Path=/other; Secure; HttpOnly",
        )]);
        write_private(&session_path, &session);

        let mut transport = Scripted::default();
        let error = {
            let mut context = sandbox.context(&mut transport);
            require_ucloud_session(&mut context)
                .err()
                .expect("unapproved cookie path is rejected")
        };
        assert_eq!(error.code, 3);
        assert_eq!(error.kind, "auth");
        assert!(transport.calls.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn require_rejects_non_private_files_and_directories_before_network() {
        for mutate in [
            "credentials-mode",
            "session-mode",
            "config-directory-mode",
            "cache-directory-mode",
        ] {
            let sandbox = Sandbox::new();
            seed_native_state(&sandbox, OLD_PASSWORD, OLD_REVISION);
            match mutate {
                "credentials-mode" => {
                    set_mode(&sandbox.config_dir().join(CREDENTIALS_FILENAME), 0o644)
                }
                "session-mode" => set_mode(&sandbox.cache_dir().join(SESSION_FILENAME), 0o644),
                "config-directory-mode" => set_mode(&sandbox.config_dir(), 0o777),
                "cache-directory-mode" => set_mode(&sandbox.cache_dir(), 0o777),
                _ => unreachable!(),
            }
            let mut transport = Scripted::default();
            let error = {
                let mut context = sandbox.context(&mut transport);
                require_ucloud_session(&mut context)
                    .err()
                    .expect("non-private local state is rejected")
            };
            assert_eq!(error.code, 3, "case {mutate}");
            assert_eq!(error.kind, "auth", "case {mutate}");
            assert!(transport.calls.is_empty(), "case {mutate}");
        }
    }

    #[test]
    fn require_refreshes_an_expired_token_and_persists_it() {
        let sandbox = Sandbox::new();
        seed_native_state(&sandbox, OLD_PASSWORD, OLD_REVISION);
        let session_path = sandbox.cache_dir().join(SESSION_FILENAME);
        let mut session = read_json(&session_path);
        session["ucloud"]["access_token"] = serde_json::json!(OLD_ACCESS_TOKEN);
        session["ucloud"]["expires_at"] = serde_json::json!(1.0);
        write_private(&session_path, &session);

        let mut transport = successful_token_script();
        let pair = {
            let mut context = sandbox.context(&mut transport);
            require_ucloud_session(&mut context).expect("expired token is refreshed")
        };
        assert_eq!(pair, (ACCESS_TOKEN.to_owned(), USER_ID.to_owned()));
        assert_eq!(transport.calls.len(), 2);
        let updated = read_json(&session_path);
        assert_eq!(updated["ucloud"]["access_token"], ACCESS_TOKEN);
        assert!(!updated.to_string().contains(OLD_ACCESS_TOKEN));
        assert!(!updated.to_string().contains(TICKET));
        assert_eq!(updated["ucloud"]["user_id"], USER_ID);
    }

    #[test]
    fn require_reuses_a_fresh_token_even_when_cas_cookies_are_expired() {
        let sandbox = Sandbox::new();
        seed_native_state(&sandbox, OLD_PASSWORD, OLD_REVISION);
        let session_path = sandbox.cache_dir().join(SESSION_FILENAME);
        let mut session = read_json(&session_path);
        session["cookies"] = expired_native_cookies();
        write_private(&session_path, &session);
        let before = fs::read(&session_path).expect("read session");
        let mut transport = Scripted::default();
        let pair = {
            let mut context = sandbox.context(&mut transport);
            require_ucloud_session(&mut context).expect("fresh token is still usable")
        };
        assert_eq!(pair, (OLD_ACCESS_TOKEN.to_owned(), USER_ID.to_owned()));
        assert!(transport.calls.is_empty());
        assert_eq!(fs::read(&session_path).expect("read session"), before);
    }

    #[test]
    fn failed_token_refresh_preserves_session_and_does_not_submit_password() {
        let sandbox = Sandbox::new();
        seed_native_state(&sandbox, OLD_PASSWORD, OLD_REVISION);
        let credentials_path = sandbox.config_dir().join(CREDENTIALS_FILENAME);
        let session_path = sandbox.cache_dir().join(SESSION_FILENAME);
        let mut session = read_json(&session_path);
        session.as_object_mut().expect("object").remove("ucloud");
        write_private(&session_path, &session);
        let credentials_before = fs::read(&credentials_path).expect("read credentials");
        let before = fs::read(&session_path).expect("read session");

        let mut transport = Scripted::default();
        enqueue(
            &mut transport,
            "GET",
            CAS_SERVICE_URL,
            302,
            &[("Location", TICKET_URL)],
            "",
        );
        enqueue(&mut transport, "POST", TOKEN_URL, 401, &[], "denied");
        let error = {
            let mut context = sandbox.context(&mut transport);
            require_ucloud_session(&mut context)
                .err()
                .expect("rejected token refresh fails")
        };
        assert_eq!(error.code, 3);
        assert_eq!(error.kind, "auth");
        assert_eq!(transport.calls.len(), 2);
        let token_call = transport
            .calls
            .iter()
            .find(|call| call.url == TOKEN_URL)
            .expect("token exchange is attempted");
        assert!(!String::from_utf8_lossy(&token_call.body).contains(OLD_PASSWORD));
        assert_eq!(fs::read(&session_path).expect("read session"), before);
        assert_eq!(
            fs::read(&credentials_path).expect("read credentials"),
            credentials_before
        );
        assert_error_redacted(
            &error,
            &[OLD_PASSWORD, OLD_ACCESS_TOKEN, ACCESS_TOKEN, TICKET],
            &[&session_path],
        );
    }

    #[test]
    fn failed_login_preserves_credentials_and_session_and_does_not_retry_password() {
        let sandbox = Sandbox::new();
        seed_native_state(&sandbox, OLD_PASSWORD, OLD_REVISION);
        let credentials_path = sandbox.config_dir().join(CREDENTIALS_FILENAME);
        let session_path = sandbox.cache_dir().join(SESSION_FILENAME);
        let credentials_before = fs::read(&credentials_path).expect("read credentials");
        let session_before = fs::read(&session_path).expect("read session");
        #[cfg(unix)]
        {
            assert_eq!(mode(&credentials_path), 0o600);
            assert_eq!(mode(&session_path), 0o600);
        }

        let mut transport = Scripted::default();
        enqueue(
            &mut transport,
            "GET",
            LOGIN_URL,
            200,
            &[],
            &login_page_body(),
        );
        enqueue(
            &mut transport,
            "POST",
            LOGIN_URL,
            401,
            &[],
            "invalid credentials",
        );
        let request = request(NEW_PASSWORD);
        let error = {
            let mut context = sandbox.context(&mut transport);
            login(&mut context, &request)
                .err()
                .expect("rejected login fails")
        };
        assert_eq!(error.code, 3);
        assert_eq!(error.kind, "auth");
        assert_eq!(transport.calls.len(), 2);
        assert_eq!(
            transport
                .calls
                .iter()
                .filter(|call| call.method == "POST" && call.url == LOGIN_URL)
                .count(),
            1,
            "login must not retry the password"
        );
        assert!(transport.calls.iter().all(|call| call.url != TOKEN_URL));
        assert_eq!(
            fs::read(&credentials_path).expect("read credentials"),
            credentials_before
        );
        assert_eq!(
            fs::read(&session_path).expect("read session"),
            session_before
        );
        #[cfg(unix)]
        {
            assert_eq!(mode(&credentials_path), 0o600);
            assert_eq!(mode(&session_path), 0o600);
        }
        assert_error_redacted(
            &error,
            &[OLD_PASSWORD, NEW_PASSWORD, OLD_ACCESS_TOKEN, SESSION_SECRET],
            &[&credentials_path, &session_path],
        );
        let mut entries = entry_names(&sandbox.config_dir());
        entries.extend(entry_names(&sandbox.cache_dir()));
        assert!(
            entries.iter().all(|name| !name.starts_with(".bupt-")),
            "temporary state was left behind: {entries:?}"
        );
    }

    #[test]
    fn incomplete_login_verification_preserves_state_and_skips_token_exchange() {
        let sandbox = Sandbox::new();
        seed_native_state(&sandbox, OLD_PASSWORD, OLD_REVISION);
        let credentials_path = sandbox.config_dir().join(CREDENTIALS_FILENAME);
        let session_path = sandbox.cache_dir().join(SESSION_FILENAME);
        let credentials_before = fs::read(&credentials_path).expect("read credentials");
        let session_before = fs::read(&session_path).expect("read session");

        let mut transport = Scripted::default();
        enqueue(
            &mut transport,
            "GET",
            LOGIN_URL,
            200,
            &[],
            &login_page_body(),
        );
        enqueue(
            &mut transport,
            "POST",
            LOGIN_URL,
            302,
            &[("Location", TICKET_URL)],
            "",
        );
        enqueue(
            &mut transport,
            "GET",
            TICKET_URL,
            200,
            &[],
            "<html><body>synthetic UCloud workspace</body></html>",
        );
        enqueue(
            &mut transport,
            "GET",
            CAS_SERVICE_URL,
            200,
            &[],
            "<html><body>no ticket</body></html>",
        );
        let request = request(NEW_PASSWORD);
        let error = {
            let mut context = sandbox.context(&mut transport);
            login(&mut context, &request)
                .err()
                .expect("missing ticket fails closed")
        };
        assert_eq!(error.code, 3);
        assert_eq!(error.kind, "auth");
        assert_eq!(transport.calls.len(), 4);
        assert!(transport.calls.iter().all(|call| call.url != TOKEN_URL));
        assert_eq!(
            fs::read(&credentials_path).expect("read credentials"),
            credentials_before
        );
        assert_eq!(
            fs::read(&session_path).expect("read session"),
            session_before
        );
    }

    #[cfg(unix)]
    #[test]
    fn login_refuses_unsafe_parents_before_password_post() {
        // Symlinked config directory.
        {
            let sandbox = Sandbox::new();
            let target = sandbox.root.path().join("real-config");
            fs::create_dir_all(&target).expect("create symlink target");
            set_mode(&target, 0o700);
            symlink(&target, sandbox.config_dir()).expect("create config symlink");
            let mut transport = successful_login_script();
            let error = {
                let mut context = sandbox.context(&mut transport);
                login(&mut context, &request(NEW_PASSWORD))
                    .err()
                    .expect("config symlink is rejected")
            };
            assert_eq!(error.code, 3);
            assert!(transport.calls.is_empty());
            assert_eq!(entry_names(&target), Vec::<String>::new());
        }

        // Symlinked cache directory.
        {
            let sandbox = Sandbox::new();
            fs::create_dir_all(sandbox.config_dir()).expect("create config");
            set_mode(&sandbox.config_dir(), 0o700);
            let target = sandbox.root.path().join("real-cache");
            fs::create_dir_all(&target).expect("create symlink target");
            set_mode(&target, 0o700);
            symlink(&target, sandbox.cache_dir()).expect("create cache symlink");
            let mut transport = successful_login_script();
            let error = {
                let mut context = sandbox.context(&mut transport);
                login(&mut context, &request(NEW_PASSWORD))
                    .err()
                    .expect("cache symlink is rejected")
            };
            assert_eq!(error.code, 3);
            assert!(transport.calls.is_empty());
            assert_eq!(entry_names(&target), Vec::<String>::new());
        }

        // Directories writable by other users.
        for which in ["config", "cache"] {
            let sandbox = Sandbox::new();
            fs::create_dir_all(sandbox.config_dir()).expect("create config");
            fs::create_dir_all(sandbox.cache_dir()).expect("create cache");
            set_mode(&sandbox.config_dir(), 0o700);
            set_mode(&sandbox.cache_dir(), 0o700);
            let directory = if which == "config" {
                sandbox.config_dir()
            } else {
                sandbox.cache_dir()
            };
            set_mode(&directory, 0o777);
            let mut transport = successful_login_script();
            let error = {
                let mut context = sandbox.context(&mut transport);
                login(&mut context, &request(NEW_PASSWORD))
                    .err()
                    .expect("unsafe directory is rejected")
            };
            assert_eq!(error.code, 3, "case {which}");
            assert!(transport.calls.is_empty(), "case {which}");
        }

        // Symlinked credentials target.
        {
            let sandbox = Sandbox::new();
            fs::create_dir_all(sandbox.config_dir()).expect("create config");
            set_mode(&sandbox.config_dir(), 0o700);
            let target = sandbox.root.path().join("real-credentials");
            let contents = b"TEST-ONLY-ORIGINAL-CREDENTIALS\n";
            fs::write(&target, contents).expect("write symlink target");
            set_mode(&target, 0o600);
            symlink(&target, sandbox.config_dir().join(CREDENTIALS_FILENAME))
                .expect("create credentials symlink");
            let mut transport = successful_login_script();
            let error = {
                let mut context = sandbox.context(&mut transport);
                login(&mut context, &request(NEW_PASSWORD))
                    .err()
                    .expect("credentials symlink is rejected")
            };
            assert_eq!(error.code, 3);
            assert!(transport.calls.is_empty());
            assert_eq!(fs::read(&target).expect("read target"), contents);
        }

        // Symlinked session target.
        {
            let sandbox = Sandbox::new();
            fs::create_dir_all(sandbox.config_dir()).expect("create config");
            fs::create_dir_all(sandbox.cache_dir()).expect("create cache");
            set_mode(&sandbox.config_dir(), 0o700);
            set_mode(&sandbox.cache_dir(), 0o700);
            let target = sandbox.root.path().join("real-session");
            let contents = b"TEST-ONLY-ORIGINAL-SESSION\n";
            fs::write(&target, contents).expect("write symlink target");
            set_mode(&target, 0o600);
            symlink(&target, sandbox.cache_dir().join(SESSION_FILENAME))
                .expect("create session symlink");
            let mut transport = successful_login_script();
            let error = {
                let mut context = sandbox.context(&mut transport);
                login(&mut context, &request(NEW_PASSWORD))
                    .err()
                    .expect("session symlink is rejected")
            };
            assert_eq!(error.code, 3);
            assert!(transport.calls.is_empty());
            assert_eq!(fs::read(&target).expect("read target"), contents);
        }
    }

    #[test]
    fn each_login_uses_a_new_private_revision() {
        let sandbox = Sandbox::new();
        let request = request(NEW_PASSWORD);
        let mut first_transport = successful_login_script();
        {
            let mut context = sandbox.context(&mut first_transport);
            login(&mut context, &request).expect("first synthetic login succeeds");
        }
        let credentials_path = sandbox.config_dir().join(CREDENTIALS_FILENAME);
        let session_path = sandbox.cache_dir().join(SESSION_FILENAME);
        let first_revision = read_json(&credentials_path)[SESSION_REVISION_KEY]
            .as_str()
            .expect("first revision")
            .to_owned();

        let mut second_transport = successful_login_script();
        {
            let mut context = sandbox.context(&mut second_transport);
            login(&mut context, &request).expect("second synthetic login succeeds");
        }
        let credentials = read_json(&credentials_path);
        let session = read_json(&session_path);
        let second_revision = credentials[SESSION_REVISION_KEY]
            .as_str()
            .expect("second revision");
        assert_ne!(first_revision, second_revision);
        assert_eq!(second_revision.len(), 64);
        assert_eq!(session[SESSION_REVISION_KEY], second_revision);
        assert_eq!(session["cas_logins_total"], 2);
        assert_eq!(second_transport.calls.len(), 4);
    }

    #[cfg(unix)]
    #[test]
    fn login_repairs_malformed_regular_files_after_verification() {
        let sandbox = Sandbox::new();
        fs::create_dir_all(sandbox.config_dir()).expect("create config");
        fs::create_dir_all(sandbox.cache_dir()).expect("create cache");
        set_mode(&sandbox.config_dir(), 0o700);
        set_mode(&sandbox.cache_dir(), 0o700);
        let credentials_path = sandbox.config_dir().join(CREDENTIALS_FILENAME);
        let session_path = sandbox.cache_dir().join(SESSION_FILENAME);
        fs::write(&credentials_path, "not-json-credentials").expect("write malformed credentials");
        fs::write(&session_path, "not-json-session").expect("write malformed session");
        set_mode(&credentials_path, 0o600);
        set_mode(&session_path, 0o600);

        let mut transport = successful_login_script();
        let request = request(NEW_PASSWORD);
        {
            let mut context = sandbox.context(&mut transport);
            login(&mut context, &request).expect("verified login repairs malformed state");
        }
        let credentials = read_json(&credentials_path);
        let session = read_json(&session_path);
        assert_eq!(credentials["password"], NEW_PASSWORD);
        assert_eq!(session["version"], CACHE_VERSION);
        assert_eq!(session["username"], USER);
        assert_eq!(
            session[SESSION_REVISION_KEY],
            credentials[SESSION_REVISION_KEY]
        );
        assert!(
            !fs::read_to_string(&credentials_path)
                .expect("read repaired credentials")
                .contains("not-json-credentials")
        );
        assert!(
            !fs::read_to_string(&session_path)
                .expect("read repaired session")
                .contains("not-json-session")
        );
        assert_eq!(mode(&credentials_path), 0o600);
        assert_eq!(mode(&session_path), 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn login_replaces_bound_state_and_preserves_unrelated_credentials() {
        let sandbox = Sandbox::new();
        seed_native_state(&sandbox, OLD_PASSWORD, OLD_REVISION);
        let credentials_path = sandbox.config_dir().join(CREDENTIALS_FILENAME);
        let session_path = sandbox.cache_dir().join(SESSION_FILENAME);
        let mut transport = successful_login_script();
        let request = request(NEW_PASSWORD);
        {
            let mut context = sandbox.context(&mut transport);
            login(&mut context, &request).expect("synthetic login succeeds");
        }
        assert!(
            transport
                .calls
                .iter()
                .all(|call| { !String::from_utf8_lossy(&call.body).contains(OLD_PASSWORD) })
        );
        assert!(
            transport
                .calls
                .iter()
                .all(|call| { !String::from_utf8_lossy(&call.body).contains(OLD_ACCESS_TOKEN) })
        );
        let credentials = read_json(&credentials_path);
        let session = read_json(&session_path);
        assert_eq!(credentials["password"], NEW_PASSWORD);
        assert_eq!(credentials["unrelated"]["keep"], "yes");
        assert_ne!(credentials[SESSION_REVISION_KEY], OLD_REVISION);
        assert_eq!(session["cas_logins_total"], 3);
        assert_eq!(session["created_at"], "2026-01-01T00:00:00Z");
        assert!(session.get("ucloud").is_none());
        assert!(!session.to_string().contains(OLD_ACCESS_TOKEN));
        assert!(!session.to_string().contains(TICKET));
        assert_eq!(
            session[SESSION_REVISION_KEY],
            credentials[SESSION_REVISION_KEY]
        );
        assert_ne!(session[SESSION_REVISION_KEY], OLD_REVISION);
    }

    #[cfg(not(unix))]
    #[test]
    fn login_and_require_fail_closed_on_an_unsupported_platform() {
        let sandbox = Sandbox::new();
        let mut transport = successful_login_script();
        let error = {
            let mut context = sandbox.context(&mut transport);
            login(&mut context, &request(NEW_PASSWORD))
                .err()
                .expect("login fails closed without secure storage")
        };
        assert_eq!(error.code, 5);
        assert_eq!(error.kind, "upstream");
        assert!(transport.calls.is_empty());
        assert!(entry_names(&sandbox.config_dir()).is_empty());
    }
}
