//! The gated submission path for the homework lane.
//!
//! `run` validates the payload and gates on `--i-confirm-submit`; only a
//! confirmed invocation reaches `execute_submit`, which uploads each planned
//! file (bounded by the pre-flight caps, staged to a 0600 temporary file by
//! the campus wire layer) and submits exactly once. Any ambiguous outcome is
//! reported as unverified and never retried automatically.

use super::detail;
use super::load_read_session;
use super::read::Session;
use crate::cli::{CliError, CliResult, Context, Outcome};
use crate::transport::{Request, Response, Transport};
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

const UCLOUD_API: &str = "https://apiucloud.bupt.edu.cn";
const UPLOAD_PATH: &str = "/blade-source/resource/upload/biz";
const SUBMIT_PATH: &str = "/ykt-site/work/submit";
use crate::transport::HOMEWORK_MULTIPART_BOUNDARY as MULTIPART_BOUNDARY;

/// Pre-flight mirrors of the campus wire caps: a local-input overflow is a
/// usage error here, never an upstream failure after a full read.
const MAX_UPLOAD_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_TEXT_CHARS: usize = 100_000;

pub(super) fn command() -> Command {
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
        )
}

pub(super) fn run(matches: &ArgMatches, context: &mut Context<'_>) -> CliResult<Outcome> {
    let request = SubmitRequest::from_matches(matches)?;
    request.ensure_gate()?;
    // The preview and the confirmed write share the read session; only the
    // confirmation flag decides whether the upload and submit requests run.
    let session = load_read_session(context)?;
    execute_submit(&request, &session, context.transport)
}

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
        // The same caps the campus wire layer enforces; refusing here keeps a
        // local-input problem a usage error instead of an upstream failure.
        if text.chars().count() > MAX_TEXT_CHARS {
            return Err(CliError::new(
                2,
                "usage",
                format!("--text exceeds the {} character limit", MAX_TEXT_CHARS),
                "Shorten the text or attach it as a file.",
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
            // Refuse the same caps the campus wire layer enforces, before any
            // session or transport use: a local overflow stays a usage error.
            let size = fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
            if size > MAX_UPLOAD_FILE_BYTES {
                return Err(CliError::new(
                    2,
                    "usage",
                    format!(
                        "file exceeds the upload limit: {} ({} MiB)",
                        path.display(),
                        MAX_UPLOAD_FILE_BYTES / (1024 * 1024)
                    ),
                    "Split the file or upload it outside bupt.",
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

struct PlannedFile {
    path: PathBuf,
    name: String,
    size: u64,
    mime_type: &'static str,
}

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

enum SubmitAttempt {
    Accepted(SubmitAck),
    Refused(CliError),
    Ambiguous(String),
}

#[derive(Clone, Copy)]
enum SuccessFlag {
    Missing,
    True,
    False,
    Invalid,
}

#[derive(Clone, Copy)]
enum ResultCode {
    Missing,
    Success,
    Failure,
    Invalid,
}

/// The production submission flow. Tests use `Scripted` to verify both the
/// preview reads and the synthetic upload/submit journal.
fn execute_submit(
    request: &SubmitRequest,
    session: &Session,
    transport: &mut dyn Transport,
) -> CliResult<Outcome> {
    let secrets = [session.access_token.clone()];
    execute_submit_inner(request, session, transport)
        .map_err(|error| redact_error(error, &secrets))
        .map(|outcome| redact_outcome(outcome, &secrets))
}

fn execute_submit_inner(
    request: &SubmitRequest,
    session: &Session,
    transport: &mut dyn Transport,
) -> CliResult<Outcome> {
    request.ensure_gate()?;
    let detail = detail::fetch_detail(transport, session, &request.assignment_id)?;
    let submit_view = detail::fetch_submit_view(transport, session, &request.assignment_id)?;
    let existing_ids = detail::submit_view_ids(&submit_view);
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

    let ack = match submit_once(transport, session, request, &attachment_ids)? {
        SubmitAttempt::Accepted(ack) => ack,
        SubmitAttempt::Refused(error) => return Err(error),
        SubmitAttempt::Ambiguous(reason) => {
            return Ok(ambiguous_submit(
                request,
                &detail,
                uploaded,
                attachment_ids,
                reason,
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

fn ambiguous_submit(
    request: &SubmitRequest,
    detail: &detail::Detail,
    uploaded: Vec<UploadedFile>,
    attachment_ids: Vec<String>,
    reason: String,
) -> Outcome {
    let warning = format!(
        "the submission outcome is unknown ({reason}). The assignment may already be submitted; do not retry. Run `bupt homework verify {}` to check.",
        request.assignment_id
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
        let size = metadata.len();
        if size > MAX_UPLOAD_FILE_BYTES {
            return Err(CliError::new(
                2,
                "usage",
                format!(
                    "file exceeds the upload limit: {} ({} MiB)",
                    path.display(),
                    MAX_UPLOAD_FILE_BYTES / (1024 * 1024)
                ),
                "Split the file or upload it outside bupt.",
            ));
        }
        planned.push(PlannedFile {
            path: path.clone(),
            name,
            size,
            mime_type,
        });
    }
    Ok(planned)
}

fn upload_file(
    transport: &mut dyn Transport,
    session: &Session,
    file: &PlannedFile,
) -> CliResult<UploadedFile> {
    // The pre-flight cap bounds this read; the binary body is staged to a
    // 0600 temporary file by the campus wire layer, never into argv or the
    // curl configuration text.
    let bytes = fs::read(&file.path).map_err(|_| {
        CliError::new(
            2,
            "usage",
            format!("cannot read upload file {}", file.path.display()),
            "Check the file exists and is readable.",
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
        return Err(CliError::new(
            5,
            "upstream",
            "UCloud upload was explicitly refused by the upstream service",
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
) -> CliResult<SubmitAttempt> {
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
    let response = match transport.send(Request {
        method: "POST",
        url: format!("{UCLOUD_API}{SUBMIT_PATH}"),
        headers,
        body,
    }) {
        Ok(response) => response,
        Err(_) => {
            return Ok(SubmitAttempt::Ambiguous(
                "no confirming response was received".into(),
            ));
        }
    };

    let status = response.status;
    if status == 401 || status == 403 {
        return Ok(SubmitAttempt::Refused(CliError::new(
            3,
            "auth",
            "UCloud submission was rejected as unauthorized",
            "Run `bupt login --force`, then retry only after confirming the previous attempt.",
        )));
    }
    if status < 200 || status == 408 || (300..400).contains(&status) || status >= 500 {
        return Ok(SubmitAttempt::Ambiguous(format!(
            "the submit request received HTTP {status} without a confirmable success response"
        )));
    }
    if !(200..300).contains(&status) {
        return Ok(SubmitAttempt::Refused(CliError::new(
            5,
            "upstream",
            format!("UCloud submission was explicitly refused with HTTP {status}"),
            "The command did not retry the submit request.",
        )));
    }
    let payload: Value = match serde_json::from_slice(&response.body) {
        Ok(payload) => payload,
        Err(_) => {
            return Ok(SubmitAttempt::Ambiguous(
                "the submit response was not valid JSON".into(),
            ));
        }
    };
    Ok(classify_submit_reply(&payload))
}

fn classify_submit_reply(payload: &Value) -> SubmitAttempt {
    let Some(object) = payload.as_object() else {
        return SubmitAttempt::Ambiguous("the submit response was not a JSON object".into());
    };
    let success = match object.get("success") {
        None | Some(Value::Null) => SuccessFlag::Missing,
        Some(Value::Bool(true)) => SuccessFlag::True,
        Some(Value::Bool(false)) => SuccessFlag::False,
        Some(_) => SuccessFlag::Invalid,
    };
    let code = match object.get("code") {
        None | Some(Value::Null) => ResultCode::Missing,
        Some(Value::Number(number))
            if number.as_i64().is_some_and(|code| code == 0 || code == 200) =>
        {
            ResultCode::Success
        }
        Some(Value::Number(_)) => ResultCode::Failure,
        Some(Value::String(text)) if text == "0" || text == "200" => ResultCode::Success,
        Some(Value::String(_)) => ResultCode::Failure,
        Some(_) => ResultCode::Invalid,
    };

    match (success, code) {
        (SuccessFlag::True, ResultCode::Success | ResultCode::Missing)
        | (SuccessFlag::Missing, ResultCode::Success) => SubmitAttempt::Accepted(SubmitAck {
            success: true,
            code: object.get("code").cloned().filter(|value| !value.is_null()),
            msg: object.get("msg").and_then(Value::as_str).map(str::to_owned),
        }),
        (SuccessFlag::False, ResultCode::Failure | ResultCode::Missing | ResultCode::Invalid)
        | (SuccessFlag::Missing, ResultCode::Failure)
        | (SuccessFlag::Invalid, ResultCode::Failure) => SubmitAttempt::Refused(CliError::new(
            5,
            "upstream",
            "UCloud submission was explicitly refused by the upstream service",
            "The command did not retry the submit request.",
        )),
        (SuccessFlag::False, ResultCode::Success)
        | (SuccessFlag::True, ResultCode::Failure | ResultCode::Invalid)
        | (SuccessFlag::Missing, ResultCode::Missing | ResultCode::Invalid)
        | (SuccessFlag::Invalid, ResultCode::Missing | ResultCode::Success | ResultCode::Invalid) => {
            SubmitAttempt::Ambiguous(
                "the submit acknowledgement was malformed or contradictory".into(),
            )
        }
    }
}

fn response_json(response: Response, operation: &str) -> CliResult<Value> {
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
            format!(
                "{operation} was explicitly refused with HTTP {}",
                response.status
            ),
            "The command did not retry.",
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
        let message = object.get("msg").and_then(Value::as_str).unwrap_or("");
        if looks_like_auth_error(message) {
            return Err(CliError::new(
                3,
                "auth",
                format!("{operation} failed authentication"),
                "Run `bupt login --force`, then retry.",
            ));
        }
        return Err(CliError::new(
            5,
            "upstream",
            format!("{operation} failed with an upstream error"),
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

fn redact_error(mut error: CliError, secrets: &[String]) -> CliError {
    error.message = redact_text(&error.message, secrets);
    error.hint = redact_text(&error.hint, secrets);
    error
}

fn redact_outcome(mut outcome: Outcome, secrets: &[String]) -> Outcome {
    redact_value(&mut outcome.data, secrets);
    if let Some(diagnostic) = outcome.diagnostic.take() {
        outcome.diagnostic = Some(redact_text(&diagnostic, secrets));
    }
    outcome
}

fn redact_value(value: &mut Value, secrets: &[String]) {
    match value {
        Value::String(text) => *text = redact_text(text, secrets),
        Value::Array(items) => {
            for item in items {
                redact_value(item, secrets);
            }
        }
        Value::Object(object) => {
            for value in object.values_mut() {
                redact_value(value, secrets);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn redact_text(text: &str, secrets: &[String]) -> String {
    let mut cleaned = text.to_owned();
    for secret in secrets {
        if secret.len() >= 4 {
            cleaned = cleaned.replace(secret, "[REDACTED]");
        }
    }
    cleaned
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

/// One fixed-boundary multipart body. The wire layer re-checks the exact
/// boundary openings, closing marker, and header set before staging it.
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
    use crate::transport::testing::Scripted;
    use serde_json::{Value, json};
    use std::collections::VecDeque;

    fn json_response(status: u16, body: Value) -> CliResult<Response> {
        raw_response(
            status,
            &serde_json::to_vec(&body).expect("serialize synthetic response"),
        )
    }

    fn raw_response(status: u16, body: &[u8]) -> CliResult<Response> {
        Ok(Response {
            status,
            headers: Vec::new(),
            body: body.to_vec(),
        })
    }

    fn response(body: Value) -> CliResult<Response> {
        json_response(200, body)
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

    fn detail_url(assignment_id: &str) -> String {
        format!("{UCLOUD_API}/ykt-site/work/detail?assignmentId={assignment_id}")
    }

    fn submit_view_url(assignment_id: &str) -> String {
        format!("{UCLOUD_API}/ykt-site/work/submit-view?assignmentId={assignment_id}")
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

    fn scripted_submit(request: &SubmitRequest, reply: CliResult<Response>) -> Scripted {
        scripted(vec![
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
            ("POST", format!("{UCLOUD_API}{SUBMIT_PATH}"), reply),
        ])
    }

    fn execute_with_reply(
        request: &SubmitRequest,
        reply: CliResult<Response>,
    ) -> (CliResult<Outcome>, Scripted) {
        let mut transport = scripted_submit(request, reply);
        let result = execute_submit(request, &session(), &mut transport);
        (result, transport)
    }

    fn posts(transport: &Scripted) -> Vec<String> {
        transport
            .calls
            .iter()
            .filter(|request| request.method == "POST")
            .map(|request| request.url.clone())
            .collect()
    }

    fn assert_single_write_journal(transport: &Scripted) {
        assert_eq!(transport.calls.len(), 4, "expected four read/write calls");
        assert_eq!(
            posts(transport),
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
            "a submit outcome must never trigger an automatic retry"
        );
    }

    fn assert_ambiguous(outcome: &Outcome, transport: &Scripted) {
        assert_eq!(outcome.code, 6);
        assert_ne!(outcome.data["submitted"], true);
        assert_eq!(outcome.data["submitted"], Value::Null);
        assert_eq!(outcome.data["verified"], Value::Null);
        assert_eq!(outcome.data["verification"], json!({"status": "unknown"}));
        let warning = outcome.data["warning"]
            .as_str()
            .expect("ambiguous outcome needs a warning");
        assert!(
            warning.to_ascii_lowercase().contains("verify"),
            "ambiguous guidance must point to verify: {warning}"
        );
        assert!(
            !warning.to_ascii_lowercase().contains("show"),
            "ambiguous guidance must be verify-only: {warning}"
        );
        assert!(!warning.contains("synthetic-access-token"));
        let diagnostic = outcome
            .diagnostic
            .as_deref()
            .expect("ambiguous outcome needs a diagnostic");
        assert!(diagnostic.to_ascii_lowercase().contains("verify"));
        assert!(!diagnostic.to_ascii_lowercase().contains("show"));
        assert!(!diagnostic.contains("synthetic-access-token"));
        assert_single_write_journal(transport);
    }

    #[test]
    fn oversized_upload_and_text_are_usage_errors_before_any_request() {
        let directory = tempfile::tempdir().expect("test directory");
        let huge = directory.path().join("huge.bin");
        let payload = vec![0u8; (MAX_UPLOAD_FILE_BYTES + 1) as usize];
        fs::write(&huge, payload).expect("write the oversized fixture");

        // The oversized file is refused before any read of the remote detail,
        // so the transport journal stays empty and the error is a usage error.
        // from_matches is the production entry: the cap fires before any
        // session, transport, or remote detail read.
        let matches = command()
            .try_get_matches_from([
                "submit",
                "1000000000000000004",
                "--file",
                huge.to_string_lossy().as_ref(),
                "--i-confirm-submit",
            ])
            .expect("parse submit arguments");
        let error = match SubmitRequest::from_matches(&matches) {
            Ok(_) => panic!("an oversized upload must be refused locally"),
            Err(error) => error,
        };
        assert_eq!(error.code, 2);
        assert!(error.kind == "usage");
        assert!(error.message.contains("upload limit"));

        let mut oversized_text = String::new();
        for _ in 0..=MAX_TEXT_CHARS {
            oversized_text.push('长');
        }
        let matches = command()
            .try_get_matches_from([
                "submit",
                "1000000000000000004",
                "--text",
                oversized_text.as_str(),
                "--i-confirm-submit",
            ])
            .expect("parse submit arguments");
        let error = match SubmitRequest::from_matches(&matches) {
            Ok(_) => panic!("an oversized text must be refused locally"),
            Err(error) => error,
        };
        assert_eq!(error.code, 2);
        assert!(error.kind == "usage");
        assert!(error.message.contains("--text"));
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
        let (result, transport) = execute_with_reply(
            &request,
            response(json!({"success": true, "code": 200, "msg": "synthetic ok"})),
        );

        let outcome = result.expect("confirmed submit should complete with synthetic responses");

        assert_eq!(outcome.code, 0);
        assert_eq!(outcome.data["submitted"], true);
        assert_eq!(outcome.data["verified"], Value::Null);
        assert_eq!(outcome.data["verification"], json!({"status": "skipped"}));
        assert_eq!(outcome.data["uploaded"][0]["resource_id"], "uploaded-rid");
        assert_eq!(outcome.data["attachment_ids"], json!(["uploaded-rid"]));
        assert_single_write_journal(&transport);
        let body: Value =
            serde_json::from_slice(&transport.calls[3].body).expect("submit body should be JSON");
        assert_eq!(body["assignmentContent"], "合成答案");
        assert_eq!(body["attachmentIds"], json!(["uploaded-rid"]));
    }

    #[test]
    fn explicit_success_flag_or_code_alone_is_accepted() {
        let (_directory, file) = answer_file();
        for reply in [
            response(json!({"code": 200})),
            response(json!({"success": true})),
        ] {
            let request = request("1000000000000000005", file.clone(), false, true, true);
            let (result, transport) = execute_with_reply(&request, reply);

            let outcome =
                result.expect("a recognized success flag or code is affirmative evidence");

            assert_eq!(outcome.code, 0);
            assert_eq!(outcome.data["submitted"], true);
            assert_eq!(outcome.data["upstream"]["success"], true);
            assert_single_write_journal(&transport);
        }
    }

    #[test]
    fn empty_submit_acknowledgement_is_ambiguous_and_verify_only() {
        let (_directory, file) = answer_file();
        let request = request("1000000000000000005", file, false, true, false);
        let (result, transport) = execute_with_reply(&request, response(json!({})));

        let outcome = result.expect("an empty acknowledgement must keep the result ambiguous");
        assert_ambiguous(&outcome, &transport);
    }

    #[test]
    fn invalid_submit_acknowledgement_is_ambiguous_and_verify_only() {
        let (_directory, file) = answer_file();
        for reply in [
            response(json!({"success": "true", "code": 200})),
            response(json!({"success": false, "code": 200})),
            response(json!({"success": true, "code": 500})),
        ] {
            let request = request("1000000000000000005", file.clone(), false, true, false);
            let (result, transport) = execute_with_reply(&request, reply);

            let outcome =
                result.expect("a malformed or contradictory acknowledgement must remain ambiguous");
            assert_ambiguous(&outcome, &transport);
        }
    }

    #[test]
    fn non_json_submit_response_is_ambiguous_and_verify_only() {
        let (_directory, file) = answer_file();
        let request = request("1000000000000000005", file, false, true, false);
        let (result, transport) = execute_with_reply(&request, raw_response(200, b"not json"));

        let outcome = result.expect("an invalid response body must remain ambiguous");
        assert_ambiguous(&outcome, &transport);
    }

    #[test]
    fn timeout_server_error_and_redirect_statuses_after_submit_are_ambiguous_and_not_retried() {
        let (_directory, file) = answer_file();
        for status in [302, 408, 502] {
            let request = request("1000000000000000005", file.clone(), false, true, false);
            let (result, transport) = execute_with_reply(
                &request,
                json_response(
                    status,
                    json!({"msg": "synthetic status synthetic-access-token"}),
                ),
            );

            let outcome =
                result.expect("a redirect, 408, or 5xx submit response must remain ambiguous");
            assert_ambiguous(&outcome, &transport);
        }
    }

    #[test]
    fn explicit_http_and_upstream_refusals_are_not_ambiguous_and_not_retried() {
        let (_directory, file) = answer_file();
        for reply in [
            response(json!({
                "success": false,
                "code": 500,
                "msg": "synthetic refusal synthetic-access-token",
            })),
            json_response(
                400,
                json!({"msg": "synthetic refusal synthetic-access-token"}),
            ),
        ] {
            let request = request("1000000000000000005", file.clone(), false, true, true);
            let (result, transport) = execute_with_reply(&request, reply);
            let error = match result {
                Ok(_) => panic!("an explicit refusal must not become a result document"),
                Err(error) => error,
            };

            assert_eq!(error.code, 5);
            assert_eq!(error.kind, "upstream");
            assert!(!error.message.contains("synthetic refusal"));
            assert!(!error.message.contains("synthetic-access-token"));
            assert!(!error.hint.contains("synthetic-access-token"));
            assert_single_write_journal(&transport);
        }
    }

    #[test]
    fn lost_submit_response_is_ambiguous_and_never_retried() {
        let (_directory, file) = answer_file();
        let request = request("1000000000000000005", file, false, true, false);
        let (result, transport) = execute_with_reply(
            &request,
            Err(CliError::new(
                4,
                "network",
                "synthetic timeout with synthetic-access-token",
                "",
            )),
        );

        let outcome = result.expect("a lost submit response should become an ambiguous result");
        assert_ambiguous(&outcome, &transport);
        let warning = outcome.data["warning"]
            .as_str()
            .expect("ambiguous outcome needs a warning");
        assert!(
            warning.contains("no confirming response was received"),
            "transport failures must not claim the request reached the server: {warning}"
        );
        assert!(
            !warning.contains("request was sent"),
            "transport failures must not claim bytes were delivered: {warning}"
        );
        assert!(
            !warning.contains("synthetic-access-token"),
            "diagnostics must redact the session token: {warning}"
        );
    }

    #[test]
    fn upstream_message_echoing_token_is_redacted_at_the_boundary() {
        let (_directory, file) = answer_file();
        let request = request("1000000000000000005", file, false, true, true);
        let (result, transport) = execute_with_reply(
            &request,
            response(json!({
                "success": true,
                "code": 200,
                "msg": "synthetic ok synthetic-access-token",
            })),
        );

        let outcome = result.expect("an explicit success is still a result");
        let message = outcome.data["upstream"]["msg"]
            .as_str()
            .expect("accepted upstream message field");
        assert!(!message.contains("synthetic-access-token"));
        assert!(message.contains("[REDACTED]"));
        assert_single_write_journal(&transport);
    }
}
