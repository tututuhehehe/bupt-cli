use crate::cli::{CliError, CliResult, Context, Outcome};
use crate::transport::{Request, Transport};
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{Map, Value, json};
use std::fs;
use std::path::{Path, PathBuf};

const UCLOUD_API: &str = "https://apiucloud.bupt.edu.cn";
const DETAIL_PATH: &str = "/ykt-site/work/detail";
const SUBMIT_VIEW_PATH: &str = "/ykt-site/work/submit-view";
const UPLOAD_PATH: &str = "/blade-source/resource/upload/biz";
const SUBMIT_PATH: &str = "/ykt-site/work/submit";
const MULTIPART_BOUNDARY: &str = "----bupt-cli-boundary";

pub(crate) fn commands() -> Vec<Command> {
    vec![
        Command::new("courses").about("List every enrolled UCloud course site"),
        Command::new("homework")
            .about("List UCloud assignments, or inspect and submit one")
            .arg(
                Arg::new("undone")
                    .long("undone")
                    .action(ArgAction::SetTrue)
                    .help("Only assignments that are not submitted"),
            )
            .arg(
                Arg::new("course")
                    .long("course")
                    .value_name("ID")
                    .help("Filter by UCloud course/site id"),
            )
            .arg(
                Arg::new("new")
                    .long("new")
                    .value_name("DAYS")
                    .value_parser(clap::value_parser!(u64))
                    .help("Only assignments published within DAYS days"),
            )
            .arg(
                Arg::new("limit")
                    .long("limit")
                    .value_name("N")
                    .default_value("0")
                    .value_parser(clap::value_parser!(u64))
                    .help("Maximum results (0 = all)"),
            )
            .subcommand(
                Command::new("show")
                    .about("Show one assignment and its attachments")
                    .arg(Arg::new("id").value_name("ID").required(true))
                    .arg(
                        Arg::new("course")
                            .long("course")
                            .value_name("SITE_ID")
                            .value_parser(numeric_id)
                            .help("Only look up the submission time in this course/site id"),
                    ),
            )
            .subcommand(
                Command::new("download")
                    .about("Download teacher attachments for one assignment")
                    .arg(Arg::new("id").value_name("ID").required(true))
                    .arg(
                        Arg::new("dir")
                            .long("dir")
                            .value_name("DIR")
                            .default_value(".")
                            .help("Output directory"),
                    )
                    .arg(
                        Arg::new("force")
                            .long("force")
                            .action(ArgAction::SetTrue)
                            .help("Overwrite existing files"),
                    ),
            )
            .subcommand(
                Command::new("submit")
                    .about("Upload files/text and submit an assignment (gated)")
                    .arg(Arg::new("id").value_name("ID").required(true))
                    .arg(
                        Arg::new("file")
                            .long("file")
                            .value_name("PATH")
                            .action(ArgAction::Append)
                            .help("Local file to upload (repeatable)"),
                    )
                    .arg(
                        Arg::new("text")
                            .long("text")
                            .value_name("CONTENT")
                            .default_value("")
                            .help("Assignment text content"),
                    )
                    .arg(
                        Arg::new("resource_id")
                            .long("resource-id")
                            .value_name("ID")
                            .action(ArgAction::Append)
                            .help("Already-uploaded UCloud resource id (repeatable)"),
                    )
                    .arg(
                        Arg::new("dry_run")
                            .long("dry-run")
                            .action(ArgAction::SetTrue)
                            .help("Validate inputs and show the planned payload; no writes"),
                    )
                    .arg(
                        Arg::new("i_confirm_submit")
                            .long("i-confirm-submit")
                            .action(ArgAction::SetTrue)
                            .help("Required to actually upload and submit"),
                    )
                    .arg(
                        Arg::new("no_verify")
                            .long("no-verify")
                            .action(ArgAction::SetTrue)
                            .help("Skip the read-only post-submit verification"),
                    ),
            )
            .subcommand(
                Command::new("verify")
                    .about("Read-only check that a submission is visible upstream")
                    .arg(Arg::new("id").value_name("ID").required(true))
                    .arg(
                        Arg::new("resource_id")
                            .long("resource-id")
                            .value_name("RID")
                            .action(ArgAction::Append)
                            .help("Expected attachment resource id (repeatable)"),
                    )
                    .arg(
                        Arg::new("text")
                            .long("text")
                            .value_name("TEXT")
                            .help("Expected submitted text"),
                    ),
            ),
    ]
}

fn numeric_id(raw: &str) -> Result<String, String> {
    let text = raw.trim();
    if !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()) {
        Ok(text.to_owned())
    } else {
        Err("id must be numeric".into())
    }
}

pub(crate) fn run(
    command: &str,
    matches: &ArgMatches,
    context: &mut Context<'_>,
) -> CliResult<Outcome> {
    match command {
        "courses" => {
            require_credentials(context)?;
            Err(unsupported("course listing"))
        }
        "homework" => run_homework(matches, context),
        _ => Err(CliError::pending()),
    }
}

fn run_homework(matches: &ArgMatches, context: &mut Context<'_>) -> CliResult<Outcome> {
    match matches.subcommand() {
        Some(("show", _)) => {
            require_credentials(context)?;
            Err(unsupported("homework show"))
        }
        Some(("download", _)) => {
            require_credentials(context)?;
            Err(unsupported("homework download"))
        }
        Some(("submit", submit)) => {
            let request = SubmitRequest::from_matches(submit)?;
            request.ensure_gate()?;
            let session = load_session(context)?;
            execute_submit(&request, &session, context.transport)
        }
        Some(("verify", _)) => {
            require_credentials(context)?;
            Err(unsupported("homework verify"))
        }
        _ => {
            require_credentials(context)?;
            Err(unsupported("homework listing"))
        }
    }
}

fn unsupported(operation: &str) -> CliError {
    CliError::new(
        5,
        "upstream",
        format!("{operation} is not implemented in this migration build"),
        "No campus request was made. This batch only covers local argument validation and the submit gate.",
    )
}

/// First-batch seam: the identity lane owns real session creation. Until that
/// adapter lands, a configured account stops here before any campus request.
fn load_session(context: &Context<'_>) -> CliResult<Session> {
    require_credentials(context)?;
    Err(unsupported("homework session wiring"))
}

fn require_credentials(context: &Context<'_>) -> CliResult<()> {
    let config = read_credentials_config(context)?;
    let username = env_value("BUPT_USERNAME")
        .or_else(|| config_text(&config, "username"))
        .filter(|value| !value.trim().is_empty());
    let password = env_value("BUPT_PASSWORD")
        .or_else(|| config_text(&config, "password"))
        .filter(|value| !value.is_empty());
    if username.is_some() && password.is_some() {
        return Ok(());
    }
    Err(CliError::new(
        3,
        "auth",
        "BUPT credentials are not configured",
        "Run `bupt login`, or set BUPT_USERNAME and BUPT_PASSWORD.",
    ))
}

fn read_credentials_config(context: &Context<'_>) -> CliResult<Map<String, Value>> {
    let path = context.config_dir.join("credentials.json");
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(_) => {
            return Err(CliError::new(
                3,
                "auth",
                format!("cannot read credentials file {}", path.display()),
                "Fix the file permissions or content, then retry.",
            ));
        }
    };
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Object(object)) => Ok(object),
        _ => Err(CliError::new(
            3,
            "auth",
            format!(
                "credentials file {} must contain a JSON object",
                path.display()
            ),
            "Fix the file content, then retry.",
        )),
    }
}

fn env_value(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn config_text(config: &Map<String, Value>, key: &str) -> Option<String> {
    match config.get(key) {
        Some(Value::String(value)) => Some(value.clone()),
        Some(Value::Number(value)) => Some(value.to_string()),
        _ => None,
    }
}

#[derive(Debug)]
struct SubmitRequest {
    assignment_id: String,
    files: Vec<PathBuf>,
    text: String,
    resource_ids: Vec<String>,
    dry_run: bool,
    confirm: bool,
    no_verify: bool,
}

impl SubmitRequest {
    fn from_matches(matches: &ArgMatches) -> CliResult<Self> {
        let files = matches
            .get_many::<String>("file")
            .map(|values| values.map(PathBuf::from).collect::<Vec<_>>())
            .unwrap_or_default();
        let text = matches
            .get_one::<String>("text")
            .cloned()
            .unwrap_or_default();
        let resource_ids = matches
            .get_many::<String>("resource_id")
            .map(|values| values.cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let dry_run = matches.get_flag("dry_run");
        let confirm = matches.get_flag("i_confirm_submit");
        let no_verify = matches.get_flag("no_verify");

        if dry_run && confirm {
            return Err(CliError::new(
                2,
                "usage",
                "--dry-run cannot be combined with --i-confirm-submit",
                "Use --dry-run alone to preview the planned payload.",
            ));
        }
        if files.is_empty() && resource_ids.is_empty() && text.trim().is_empty() {
            return Err(CliError::new(
                2,
                "usage",
                "nothing to submit",
                "Pass --file PATH and/or --text CONTENT (or --resource-id).",
            ));
        }
        let mut resolved = Vec::with_capacity(files.len());
        for path in files {
            if !path.is_file() {
                return Err(CliError::new(
                    2,
                    "usage",
                    format!("file not found: {}", path.display()),
                    "Pass an existing file path.",
                ));
            }
            resolved.push(fs::canonicalize(&path).unwrap_or(path));
        }

        Ok(Self {
            assignment_id: matches.get_one::<String>("id").cloned().unwrap_or_default(),
            files: resolved,
            text,
            resource_ids,
            dry_run,
            confirm,
            no_verify,
        })
    }

    fn ensure_gate(&self) -> CliResult<()> {
        if self.dry_run && self.confirm {
            return Err(CliError::new(
                2,
                "usage",
                "--dry-run cannot be combined with --i-confirm-submit",
                "Use --dry-run alone to preview the planned payload.",
            ));
        }
        if self.dry_run || self.confirm {
            return Ok(());
        }
        Err(CliError::new(
            2,
            "usage",
            "refusing to submit without --i-confirm-submit",
            "Run `bupt homework submit ID --dry-run` first, then add --i-confirm-submit.",
        ))
    }
}

#[derive(Debug)]
struct Session {
    access_token: String,
    user_id: String,
}

#[derive(Debug)]
struct Detail {
    title: String,
    submitted: bool,
}

#[derive(Debug)]
struct PlannedFile {
    path: PathBuf,
    name: String,
    size: u64,
    mime_type: &'static str,
}

#[derive(Debug)]
struct UploadedFile {
    resource_id: String,
    filename: String,
    size: u64,
    mime_type: &'static str,
}

impl UploadedFile {
    fn as_json(&self) -> Value {
        json!({
            "resource_id": self.resource_id,
            "filename": self.filename,
            "size": self.size,
            "mime_type": self.mime_type,
        })
    }
}

#[derive(Debug)]
struct SubmitAck {
    success: bool,
    code: Option<Value>,
    msg: Option<String>,
}

impl SubmitAck {
    fn as_json(&self) -> Value {
        json!({
            "success": self.success,
            "code": self.code,
            "msg": self.msg,
        })
    }
}

/// Transport-injected submit flow. Tests use `Scripted` to assert the exact
/// upload/submit journal; the CLI cannot reach it until `load_session` is wired.
fn execute_submit(
    request: &SubmitRequest,
    session: &Session,
    transport: &mut dyn Transport,
) -> CliResult<Outcome> {
    execute_submit_inner(request, session, transport)
        .map_err(|error| redact_error(error, &session.access_token))
}

fn execute_submit_inner(
    request: &SubmitRequest,
    session: &Session,
    transport: &mut dyn Transport,
) -> CliResult<Outcome> {
    request.ensure_gate()?;
    let detail = fetch_detail(transport, session, &request.assignment_id)?;
    let submit_view = fetch_submit_view(transport, session, &request.assignment_id)?;
    let existing_ids = submit_view_ids(&submit_view);
    let planned_files = planned_files(request)?;
    let planned = json!({
        "assignment_id": request.assignment_id,
        "title": detail.title,
        "already_submitted": detail.submitted,
        "text_chars": text_chars(&request.text),
        "files": planned_files.iter().map(|file| json!({
            "path": file.path.to_string_lossy(),
            "size": file.size,
            "name": file.name,
        })).collect::<Vec<_>>(),
        "existing_resource_ids": existing_ids,
        "submit_view_status": submit_view.get("status").cloned().unwrap_or(Value::Null),
        "submit_view_assignment_status": submit_view
            .get("assignmentStatus")
            .cloned()
            .unwrap_or(Value::Null),
    });

    if request.dry_run {
        return Ok(Outcome {
            data: json!({
                "dry_run": true,
                "would_upload": planned_files
                    .iter()
                    .map(|file| file.name.clone())
                    .collect::<Vec<_>>(),
                "would_submit": true,
                "planned": planned,
                "note": "no upload and no submit performed",
            }),
            code: 0,
            diagnostic: None,
        });
    }

    let mut uploaded = Vec::with_capacity(planned_files.len());
    let mut attachment_ids = request.resource_ids.clone();
    for file in &planned_files {
        let meta = upload_file(transport, session, file)?;
        attachment_ids.push(meta.resource_id.clone());
        uploaded.push(meta);
    }
    dedupe_ids(&mut attachment_ids);

    let ack = match submit_once(transport, session, request, &attachment_ids) {
        Ok(ack) => ack,
        Err(error) => {
            return Ok(ambiguous_submit(
                request,
                &detail,
                uploaded,
                attachment_ids,
                redact_error(error, &session.access_token),
            ));
        }
    };

    let warning = if request.no_verify {
        None
    } else {
        Some(format!(
            "submitted, but post-submit verification is not implemented in this migration build; check with `bupt homework verify {}`",
            request.assignment_id
        ))
    };
    let verification = if request.no_verify {
        json!({"status": "skipped"})
    } else {
        json!({"status": "not_run"})
    };
    Ok(Outcome {
        data: json!({
            "submitted": true,
            "assignment_id": request.assignment_id,
            "title": detail.title,
            "submitted_at": Value::Null,
            "uploaded": uploaded.iter().map(UploadedFile::as_json).collect::<Vec<_>>(),
            "attachment_ids": attachment_ids,
            "text_chars": text_chars(&request.text),
            "upstream": ack.as_json(),
            "verified": Value::Null,
            "verification": verification,
            "warning": warning,
        }),
        code: if request.no_verify { 0 } else { 6 },
        diagnostic: warning,
    })
}

fn redact_error(mut error: CliError, secret: &str) -> CliError {
    if secret.len() >= 4 {
        error.message = error.message.replace(secret, "[REDACTED]");
        error.hint = error.hint.replace(secret, "[REDACTED]");
    }
    error
}

fn ambiguous_submit(
    request: &SubmitRequest,
    detail: &Detail,
    uploaded: Vec<UploadedFile>,
    attachment_ids: Vec<String>,
    error: CliError,
) -> Outcome {
    let warning = format!(
        "the submit request was sent, but its outcome is unknown ({}). The assignment may already be submitted; do not retry blindly. Run `bupt homework verify {}` or `bupt homework show {}` to check.",
        error.message, request.assignment_id, request.assignment_id
    );
    Outcome {
        data: json!({
            "submitted": Value::Null,
            "assignment_id": request.assignment_id,
            "title": detail.title,
            "uploaded": uploaded.iter().map(UploadedFile::as_json).collect::<Vec<_>>(),
            "attachment_ids": attachment_ids,
            "text_chars": text_chars(&request.text),
            "verified": Value::Null,
            "verification": {"status": "unknown"},
            "warning": warning,
        }),
        code: 6,
        diagnostic: Some(warning),
    }
}

fn planned_files(request: &SubmitRequest) -> CliResult<Vec<PlannedFile>> {
    let mut planned = Vec::with_capacity(request.files.len());
    for path in &request.files {
        let metadata = fs::metadata(path).map_err(|_| {
            CliError::new(
                2,
                "usage",
                format!("file not found: {}", path.display()),
                "Pass an existing file path.",
            )
        })?;
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into());
        let mime_type = mime_type(&name);
        planned.push(PlannedFile {
            path: path.clone(),
            name,
            size: metadata.len(),
            mime_type,
        });
    }
    Ok(planned)
}

fn fetch_detail(
    transport: &mut dyn Transport,
    session: &Session,
    assignment_id: &str,
) -> CliResult<Detail> {
    let payload = send_json(
        transport,
        "UCloud assignment detail",
        Request {
            method: "GET",
            url: query_url(DETAIL_PATH, "assignmentId", assignment_id),
            headers: auth_headers(&session.access_token),
            body: Vec::new(),
        },
    )?;
    let data = payload.get("data").ok_or_else(|| {
        CliError::new(
            5,
            "parse",
            "UCloud assignment detail response missing data",
            "",
        )
    })?;
    let object = data.as_object().ok_or_else(|| {
        CliError::new(
            5,
            "parse",
            "UCloud assignment detail returned malformed data",
            "",
        )
    })?;
    if object.is_empty() {
        return Err(CliError::new(
            5,
            "not_found",
            format!("assignment detail not found for id {assignment_id}"),
            "",
        ));
    }
    if object
        .get("submitInfo")
        .is_some_and(|value| !value.is_null() && !value.is_object())
    {
        return Err(CliError::new(
            5,
            "parse",
            "UCloud assignment detail returned malformed submitInfo",
            "",
        ));
    }
    let submit_info = object.get("submitInfo").and_then(Value::as_object);
    let title = object
        .get("assignmentTitle")
        .or_else(|| object.get("title"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let submitted = submit_info
        .and_then(|info| info.get("isSubmit"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || object
            .get("isSubmit")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        || is_submitted_status(object.get("assignmentStatus"));
    Ok(Detail { title, submitted })
}

fn fetch_submit_view(
    transport: &mut dyn Transport,
    session: &Session,
    assignment_id: &str,
) -> CliResult<Value> {
    let payload = send_json(
        transport,
        "UCloud submit-view",
        Request {
            method: "GET",
            url: query_url(SUBMIT_VIEW_PATH, "assignmentId", assignment_id),
            headers: auth_headers(&session.access_token),
            body: Vec::new(),
        },
    )?;
    let data = payload
        .get("data")
        .ok_or_else(|| CliError::new(5, "parse", "UCloud submit-view response missing data", ""))?;
    Ok(data
        .as_object()
        .map(|object| Value::Object(object.clone()))
        .unwrap_or_else(|| Value::Object(Map::new())))
}

fn submit_view_ids(view: &Value) -> Vec<String> {
    view.get("attachmentIds")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(value_as_string)
                .filter(|id| !id.trim().is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn upload_file(
    transport: &mut dyn Transport,
    session: &Session,
    file: &PlannedFile,
) -> CliResult<UploadedFile> {
    let bytes = fs::read(&file.path).map_err(|_| {
        CliError::new(
            5,
            "upstream",
            format!("cannot read upload file {}", file.path.display()),
            "",
        )
    })?;
    let mut headers = auth_headers(&session.access_token);
    headers.push((
        "Content-Type".into(),
        format!("multipart/form-data; boundary={MULTIPART_BOUNDARY}"),
    ));
    let response = transport.send(Request {
        method: "POST",
        url: format!("{UCLOUD_API}{UPLOAD_PATH}"),
        headers,
        body: multipart_body(&session.user_id, file, &bytes),
    })?;
    let payload = response_json(response, "UCloud upload")?;
    let object = payload
        .as_object()
        .ok_or_else(|| CliError::new(5, "parse", "UCloud upload returned malformed data", ""))?;
    if object.get("success").and_then(Value::as_bool) != Some(true) {
        let message = object
            .get("msg")
            .and_then(Value::as_str)
            .unwrap_or("upload failed");
        return Err(CliError::new(
            5,
            "upstream",
            format!("UCloud upload failed: {message}"),
            "",
        ));
    }
    let resource_id = object
        .get("data")
        .and_then(value_as_string)
        .ok_or_else(|| {
            CliError::new(
                5,
                "parse",
                "UCloud upload succeeded but returned no resource id",
                "",
            )
        })?;
    Ok(UploadedFile {
        resource_id,
        filename: file.name.clone(),
        size: file.size,
        mime_type: file.mime_type,
    })
}

fn submit_once(
    transport: &mut dyn Transport,
    session: &Session,
    request: &SubmitRequest,
    attachment_ids: &[String],
) -> CliResult<SubmitAck> {
    let body = json!({
        "assignmentId": request.assignment_id,
        "assignmentType": 0,
        "assignmentContent": request.text.trim_end(),
        "attachmentIds": attachment_ids,
        "userId": session.user_id,
        "groupId": "",
        "commitId": "",
    });
    let body = serde_json::to_vec(&body)
        .map_err(|_| CliError::new(5, "upstream", "cannot serialize submission payload", ""))?;
    let mut headers = auth_headers(&session.access_token);
    headers.push(("Content-Type".into(), "application/json".into()));
    let response = transport.send(Request {
        method: "POST",
        url: format!("{UCLOUD_API}{SUBMIT_PATH}"),
        headers,
        body,
    })?;
    let payload = response_json(response, "UCloud submission")?;
    let object = payload.as_object().ok_or_else(|| {
        CliError::new(5, "parse", "UCloud submission returned malformed data", "")
    })?;
    let success = object
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let code = object.get("code").cloned().filter(|value| !value.is_null());
    if !success && !code_is_ok(code.as_ref()) {
        let message = object
            .get("msg")
            .and_then(Value::as_str)
            .unwrap_or("submission failed");
        return Err(CliError::new(
            5,
            "upstream",
            format!("UCloud submission failed: {message}"),
            "",
        ));
    }
    Ok(SubmitAck {
        success,
        code,
        msg: object.get("msg").and_then(Value::as_str).map(str::to_owned),
    })
}

fn send_json(transport: &mut dyn Transport, operation: &str, request: Request) -> CliResult<Value> {
    let response = transport.send(request)?;
    response_json(response, operation)
}

fn response_json(response: crate::transport::Response, operation: &str) -> CliResult<Value> {
    if response.status == 401 || response.status == 403 {
        return Err(CliError::new(
            3,
            "auth",
            format!("{operation} was rejected as unauthorized"),
            "Run `bupt login --force`, then retry.",
        ));
    }
    if !(200..300).contains(&response.status) {
        return Err(CliError::new(
            5,
            "upstream",
            format!("{operation} returned HTTP {}", response.status),
            "",
        ));
    }
    let payload: Value = serde_json::from_slice(&response.body)
        .map_err(|_| CliError::new(5, "parse", format!("{operation} returned invalid JSON"), ""))?;
    let object = payload.as_object().ok_or_else(|| {
        CliError::new(
            5,
            "parse",
            format!("{operation} returned a non-object response"),
            "",
        )
    })?;
    if !code_is_ok(object.get("code")) {
        let message = object
            .get("msg")
            .and_then(Value::as_str)
            .unwrap_or("upstream error");
        if looks_like_auth_error(message) {
            return Err(CliError::new(
                3,
                "auth",
                format!("{operation} failed authentication: {message}"),
                "Run `bupt login --force`, then retry.",
            ));
        }
        return Err(CliError::new(
            5,
            "upstream",
            format!("{operation} failed: {message}"),
            "",
        ));
    }
    Ok(payload)
}

fn code_is_ok(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::Number(number)) => number.as_i64().is_some_and(|code| code == 0 || code == 200),
        Some(Value::String(text)) => text == "0" || text == "200",
        _ => false,
    }
}

fn looks_like_auth_error(message: &str) -> bool {
    let text = message.to_ascii_lowercase();
    text.contains("401")
        || text.contains("403")
        || text.contains("token")
        || text.contains("auth")
        || text.contains("login")
        || message.contains("登录")
}

fn auth_headers(access_token: &str) -> Vec<(String, String)> {
    vec![
        ("Blade-Auth".into(), access_token.to_owned()),
        ("Tenant-Id".into(), "000000".into()),
        (
            "Authorization".into(),
            "Basic c3dvcmQ6c3dvcmRfc2VjcmV0".into(),
        ),
    ]
}

fn query_url(path: &str, key: &str, value: &str) -> String {
    format!(
        "{UCLOUD_API}{path}?{key}={}",
        percent_encode(value.as_bytes())
    )
}

fn percent_encode(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len());
    for byte in bytes {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(*byte));
        } else {
            encoded.push('%');
            encoded.push_str(&format!("{byte:02X}"));
        }
    }
    encoded
}

fn multipart_body(user_id: &str, file: &PlannedFile, bytes: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{MULTIPART_BOUNDARY}\r\nContent-Disposition: form-data; name=\"userId\"\r\n\r\n{user_id}\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(
        format!(
            "--{MULTIPART_BOUNDARY}\r\nContent-Disposition: form-data; name=\"bizType\"\r\n\r\n3\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(
        format!(
            "--{MULTIPART_BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\nContent-Type: {}\r\n\r\n",
            multipart_name(&file.name),
            file.mime_type
        )
        .as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{MULTIPART_BOUNDARY}--\r\n").as_bytes());
    body
}

fn multipart_name(name: &str) -> String {
    name.replace(['\r', '\n', '"'], "_")
}

fn mime_type(name: &str) -> &'static str {
    match Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "pdf" => "application/pdf",
        "txt" | "md" => "text/plain",
        "csv" => "text/csv",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "zip" => "application/zip",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        _ => "application/octet-stream",
    }
}

fn value_as_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn is_submitted_status(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Number(number)) => number.as_i64() == Some(0),
        Some(Value::String(text)) => text == "0",
        _ => false,
    }
}

fn text_chars(text: &str) -> usize {
    text.chars().count()
}

fn dedupe_ids(ids: &mut Vec<String>) {
    let mut seen = Vec::new();
    ids.retain(|id| {
        if id.is_empty() || seen.contains(id) {
            false
        } else {
            seen.push(id.clone());
            true
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{Response, testing::Scripted};
    use serde_json::{Value, json};
    use std::collections::VecDeque;

    fn response(body: Value) -> CliResult<Response> {
        Ok(Response {
            status: 200,
            headers: Vec::new(),
            body: serde_json::to_vec(&body).expect("serialize synthetic response"),
        })
    }

    fn detail_body(assignment_id: &str) -> Value {
        json!({
            "code": 200,
            "data": {
                "id": assignment_id,
                "assignmentTitle": "synthetic assignment",
                "assignmentStatus": 99,
            }
        })
    }

    fn submit_view_body(assignment_id: &str, existing_ids: &[&str], status: u64) -> Value {
        json!({
            "code": 200,
            "data": {
                "assignmentId": assignment_id,
                "attachmentIds": existing_ids,
                "assignmentContent": "",
                "assignmentStatus": status,
            }
        })
    }

    fn upload_body() -> Value {
        json!({"success": true, "data": "uploaded-rid", "msg": "synthetic ok"})
    }

    fn submit_body() -> Value {
        json!({"success": true, "code": 200, "msg": "synthetic ok"})
    }

    fn detail_url(assignment_id: &str) -> String {
        query_url(DETAIL_PATH, "assignmentId", assignment_id)
    }

    fn submit_view_url(assignment_id: &str) -> String {
        query_url(SUBMIT_VIEW_PATH, "assignmentId", assignment_id)
    }

    fn session() -> Session {
        Session {
            access_token: "synthetic-access-token".into(),
            user_id: "synthetic-user".into(),
        }
    }

    fn request(
        assignment_id: &str,
        file: PathBuf,
        dry_run: bool,
        confirm: bool,
        no_verify: bool,
    ) -> SubmitRequest {
        SubmitRequest {
            assignment_id: assignment_id.into(),
            files: vec![file],
            text: "合成答案".into(),
            resource_ids: Vec::new(),
            dry_run,
            confirm,
            no_verify,
        }
    }

    fn answer_file() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().expect("create temp directory");
        let path = directory.path().join("answer.pdf");
        fs::write(&path, b"synthetic answer\n").expect("write synthetic answer");
        (directory, path)
    }

    fn scripted(entries: Vec<(&'static str, String, CliResult<Response>)>) -> Scripted {
        Scripted {
            expected: VecDeque::from(entries),
            calls: Vec::new(),
        }
    }

    fn posts(transport: &Scripted) -> Vec<String> {
        transport
            .calls
            .iter()
            .filter(|request| request.method == "POST")
            .map(|request| request.url.clone())
            .collect()
    }

    #[test]
    fn submit_without_confirmation_never_contacts_transport() {
        let (_directory, file) = answer_file();
        let request = request("1000000000000000004", file, false, false, false);
        let mut transport = Scripted::default();

        let error = match execute_submit(&request, &session(), &mut transport) {
            Ok(_) => panic!("the submit gate must reject an unconfirmed submit"),
            Err(error) => error,
        };

        assert_eq!(error.code, 2);
        assert!(error.kind == "usage");
        assert!(error.message.contains("--i-confirm-submit"));
        assert!(transport.calls.is_empty());
    }

    #[test]
    fn submit_dry_run_reads_the_plan_without_writes() {
        let (_directory, file) = answer_file();
        let request = request("1000000000000000004", file, true, false, false);
        let mut transport = scripted(vec![
            (
                "GET",
                detail_url(&request.assignment_id),
                response(detail_body(&request.assignment_id)),
            ),
            (
                "GET",
                submit_view_url(&request.assignment_id),
                response(submit_view_body(
                    &request.assignment_id,
                    &["existing-res"],
                    99,
                )),
            ),
        ]);

        let outcome = execute_submit(&request, &session(), &mut transport)
            .expect("dry run should succeed with synthetic reads");

        assert_eq!(outcome.code, 0);
        assert_eq!(outcome.data["dry_run"], true);
        assert_eq!(outcome.data["would_upload"], json!(["answer.pdf"]));
        assert_eq!(
            outcome.data["planned"]["existing_resource_ids"],
            json!(["existing-res"])
        );
        assert!(outcome.data.get("verification").is_none());
        assert!(posts(&transport).is_empty());
        assert_eq!(transport.calls.len(), 2);
        assert!(
            transport
                .calls
                .iter()
                .all(|request| request.method == "GET")
        );
    }

    #[test]
    fn confirmed_submit_without_verification_uploads_then_submits_once() {
        let (_directory, file) = answer_file();
        let request = request("1000000000000000005", file, false, true, true);
        let mut transport = scripted(vec![
            (
                "GET",
                detail_url(&request.assignment_id),
                response(detail_body(&request.assignment_id)),
            ),
            (
                "GET",
                submit_view_url(&request.assignment_id),
                response(submit_view_body(&request.assignment_id, &[], 99)),
            ),
            (
                "POST",
                format!("{UCLOUD_API}{UPLOAD_PATH}"),
                response(upload_body()),
            ),
            (
                "POST",
                format!("{UCLOUD_API}{SUBMIT_PATH}"),
                response(submit_body()),
            ),
        ]);

        let outcome = execute_submit(&request, &session(), &mut transport)
            .expect("confirmed submit should complete with synthetic responses");

        assert_eq!(outcome.code, 0);
        assert_eq!(outcome.data["submitted"], true);
        assert_eq!(outcome.data["verified"], Value::Null);
        assert_eq!(outcome.data["verification"], json!({"status": "skipped"}));
        assert_eq!(outcome.data["uploaded"][0]["resource_id"], "uploaded-rid");
        assert_eq!(outcome.data["attachment_ids"], json!(["uploaded-rid"]));
        assert_eq!(
            posts(&transport),
            vec![
                format!("{UCLOUD_API}{UPLOAD_PATH}"),
                format!("{UCLOUD_API}{SUBMIT_PATH}"),
            ]
        );
        let body: Value =
            serde_json::from_slice(&transport.calls[3].body).expect("submit body should be JSON");
        assert_eq!(body["assignmentContent"], "合成答案");
        assert_eq!(body["attachmentIds"], json!(["uploaded-rid"]));
    }

    #[test]
    fn lost_submit_response_is_ambiguous_and_never_retried() {
        let (_directory, file) = answer_file();
        let request = request("1000000000000000005", file, false, true, false);
        let mut transport = scripted(vec![
            (
                "GET",
                detail_url(&request.assignment_id),
                response(detail_body(&request.assignment_id)),
            ),
            (
                "GET",
                submit_view_url(&request.assignment_id),
                response(submit_view_body(&request.assignment_id, &[], 99)),
            ),
            (
                "POST",
                format!("{UCLOUD_API}{UPLOAD_PATH}"),
                response(upload_body()),
            ),
            (
                "POST",
                format!("{UCLOUD_API}{SUBMIT_PATH}"),
                Err(CliError::new(
                    4,
                    "network",
                    "synthetic response lost after request with synthetic-access-token",
                    "",
                )),
            ),
        ]);

        let outcome = execute_submit(&request, &session(), &mut transport)
            .expect("a lost submit response should become an ambiguous result");

        assert_eq!(outcome.code, 6);
        assert_eq!(outcome.data["submitted"], Value::Null);
        assert_eq!(outcome.data["verified"], Value::Null);
        assert_eq!(outcome.data["verification"], json!({"status": "unknown"}));
        let warning = outcome.data["warning"]
            .as_str()
            .expect("ambiguous outcome needs a warning");
        assert!(warning.contains("verify"));
        assert!(
            !warning.contains("synthetic-access-token"),
            "diagnostics must redact the session token: {warning}"
        );
        assert_eq!(transport.calls.len(), 4);
        assert_eq!(
            posts(&transport),
            vec![
                format!("{UCLOUD_API}{UPLOAD_PATH}"),
                format!("{UCLOUD_API}{SUBMIT_PATH}"),
            ]
        );
        assert_eq!(
            transport
                .calls
                .iter()
                .filter(|request| request.url.ends_with(SUBMIT_PATH))
                .count(),
            1,
            "a lost response must not trigger an automatic retry"
        );
    }
}
