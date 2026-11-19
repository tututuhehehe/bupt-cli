//! `homework download` attachment saving.
//!
//! The handler runs through the injected `Transport`. It never sends a UCloud
//! token or Cookie to a download source: preview URLs are signed CDN URLs, and
//! a source/redirect outside the narrow allowlist is rejected before another
//! request is made. Files are written through a temporary file in the target
//! directory and only renamed into place after the whole body is available.

use super::detail::{self, Attachment};
use super::read::Session;
use crate::cli::{CliError, CliResult, Context, Outcome};
use crate::transport::{Request, Response, Transport};
use clap::ArgMatches;
use serde_json::{Value, json};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use url::Url;

const USER_AGENT: &str = "Mozilla/5.0 (compatible; bupt-cli/1.0)";
const FILE_HOST: &str = "fileucloud.bupt.edu.cn";
const API_HOST: &str = "apiucloud.bupt.edu.cn";
const API_DOWNLOAD_PATH: &str = "/blade-source/resource/download";
const MAX_REDIRECTS: usize = 5;
const MAX_FILENAME_BYTES: usize = 200;

pub(super) fn run(matches: &ArgMatches, context: &mut Context<'_>) -> CliResult<Outcome> {
    let assignment_id = matches.get_one::<String>("id").cloned().unwrap_or_default();
    let raw_dir = matches
        .get_one::<String>("dir")
        .map(String::as_str)
        .unwrap_or(".");
    let force = matches.get_flag("force");
    let session = super::load_read_session(context)?;
    let dir = expand_output_dir(raw_dir, &context.home_dir);
    execute_download(&assignment_id, &dir, force, &session, context.transport)
}

pub(super) fn execute_download(
    assignment_id: &str,
    dir: &Path,
    force: bool,
    session: &Session,
    transport: &mut dyn Transport,
) -> CliResult<Outcome> {
    let dir = prepare_output_dir(dir)?;
    let detail = detail::fetch_detail(transport, session, assignment_id)?;
    let mut attachments = detail.attachments.clone();
    detail::resolve_attachment_urls(transport, session, &mut attachments, false);

    let mut saved = Vec::new();
    let mut skipped = Vec::new();
    for (index, attachment) in attachments.iter().enumerate() {
        let title = attachment_title(attachment, index);
        let filename = safe_filename(&title, index);
        let dest = dir.join(&filename);
        if dest.exists() && !force {
            skipped.push(skipped_json(&title, Some(&dest), "exists"));
            continue;
        }
        let Some(url) = attachment
            .url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
        else {
            skipped.push(skipped_json(&title, None, "no_url"));
            continue;
        };
        let body = download_body(transport, url)?;
        match write_atomic(&dir, &dest, &body, force) {
            Ok(WriteOutcome::Written(size)) => saved.push(json!({
                "title": title,
                "resource_id": attachment.resource_id,
                "path": path_string(&dest),
                "size": size,
                "url": url,
            })),
            Ok(WriteOutcome::Exists) => {
                skipped.push(skipped_json(&title, Some(&dest), "exists"));
            }
            Err(error) => return Err(error),
        }
    }

    Ok(Outcome {
        data: json!({
            "assignment_id": assignment_id,
            "title": detail.title,
            "dir": path_string(&dir),
            "saved": saved,
            "skipped": skipped,
            "count_saved": saved.len(),
            "count_skipped": skipped.len(),
        }),
        code: 0,
        diagnostic: None,
    })
}

fn attachment_title(attachment: &Attachment, index: usize) -> String {
    if attachment.title.is_empty() {
        format!("attachment-{}", index + 1)
    } else {
        attachment.title.clone()
    }
}

fn skipped_json(title: &str, path: Option<&Path>, reason: &str) -> Value {
    json!({
        "title": title,
        "path": path.map(path_string),
        "reason": reason,
    })
}

fn expand_output_dir(raw: &str, home_dir: &Path) -> PathBuf {
    if raw == "~" {
        home_dir.to_path_buf()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home_dir.join(rest)
    } else {
        PathBuf::from(raw)
    }
}

fn prepare_output_dir(dir: &Path) -> CliResult<PathBuf> {
    let dir = if dir.as_os_str().is_empty() {
        std::env::current_dir().map_err(|_| directory_error())?
    } else {
        dir.to_path_buf()
    };
    fs::create_dir_all(&dir).map_err(|_| directory_error())?;
    fs::canonicalize(&dir).map_err(|_| directory_error())
}

fn directory_error() -> CliError {
    CliError::new(
        5,
        "upstream",
        "cannot create or resolve the download directory",
        "Choose an existing writable directory with `--dir`.",
    )
}

/// Replace path separators and characters that are invalid on the supported
/// filesystems, then reject `.`/`..`/empty results. The returned value is a
/// single path component; callers must join it to the resolved output dir.
fn safe_filename(title: &str, index: usize) -> String {
    let fallback = format!("attachment-{}", index + 1);
    let mut name = String::with_capacity(title.len());
    for character in title.chars() {
        if character.is_control()
            || matches!(
                character,
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
            )
        {
            name.push('_');
        } else {
            name.push(character);
        }
    }
    let name = name.trim_end_matches([' ', '.']).to_owned();
    if name.is_empty() || name == "." || name == ".." {
        return fallback;
    }
    let mut truncated = String::with_capacity(name.len().min(MAX_FILENAME_BYTES));
    for character in name.chars() {
        if truncated.len() + character.len_utf8() > MAX_FILENAME_BYTES {
            break;
        }
        truncated.push(character);
    }
    let truncated = truncated.trim_end_matches([' ', '.']).to_owned();
    if truncated.is_empty() {
        fallback
    } else {
        truncated
    }
}

fn download_body(transport: &mut dyn Transport, source: &str) -> CliResult<Vec<u8>> {
    if source.contains('\\') || source.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(source_error());
    }
    let mut current = Url::parse(source).map_err(|_| source_error())?;
    if !allowed_source(&current) {
        return Err(source_error());
    }
    for redirects in 0..=MAX_REDIRECTS {
        let response = transport.send(Request {
            method: "GET",
            url: current.as_str().to_owned(),
            headers: vec![("User-Agent".into(), USER_AGENT.into())],
            body: Vec::new(),
        })?;
        if (300..400).contains(&response.status) {
            if redirects == MAX_REDIRECTS {
                return Err(redirect_error("too many download redirects"));
            }
            let location = location(&response).ok_or_else(|| redirect_error("missing Location"))?;
            if location.is_empty()
                || location.contains('\\')
                || location.bytes().any(|byte| byte.is_ascii_control())
            {
                return Err(redirect_error("malformed Location"));
            }
            let next = current
                .join(&location)
                .map_err(|_| redirect_error("malformed Location"))?;
            if !allowed_source(&next) {
                return Err(redirect_error("redirect left the download allowlist"));
            }
            current = next;
            continue;
        }
        if !(200..300).contains(&response.status) {
            return Err(download_http_error(response.status));
        }
        return Ok(response.body);
    }
    Err(redirect_error("too many download redirects"))
}

fn location(response: &Response) -> Option<String> {
    response
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("location"))
        .map(|(_, value)| value.trim().to_owned())
}

fn allowed_source(url: &Url) -> bool {
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.port().is_some()
    {
        return false;
    }
    match url.host_str() {
        Some(FILE_HOST) => !url.path().is_empty(),
        Some(API_HOST) => url.path() == API_DOWNLOAD_PATH,
        _ => false,
    }
}

fn source_error() -> CliError {
    CliError::new(
        5,
        "upstream",
        "download source is not an allowlisted absolute HTTPS URL",
        "No file was written. Refresh the assignment with `homework show` and retry.",
    )
}

fn redirect_error(message: &str) -> CliError {
    CliError::new(
        5,
        "upstream",
        format!("download redirect rejected: {message}"),
        "No file was written. The source did not stay on the allowlisted HTTPS host.",
    )
}

fn download_http_error(status: u16) -> CliError {
    if (400..600).contains(&status) {
        CliError::new(
            5,
            "upstream",
            format!("download source was explicitly refused with HTTP {status}"),
            "The signed download URL may have expired. Retry `homework download`.",
        )
    } else {
        CliError::new(
            4,
            "network",
            format!("download source failed with HTTP {status}"),
            "The command did not retry.",
        )
    }
}

enum WriteOutcome {
    Written(u64),
    Exists,
}

fn write_atomic(dir: &Path, dest: &Path, body: &[u8], force: bool) -> CliResult<WriteOutcome> {
    if dest.parent() != Some(dir) {
        return Err(CliError::new(
            5,
            "upstream",
            "download destination escaped the output directory",
            "No file was written.",
        ));
    }
    let mut temp = tempfile::NamedTempFile::new_in(dir).map_err(|_| write_error())?;
    temp.write_all(body).map_err(|_| write_error())?;
    temp.flush().map_err(|_| write_error())?;
    let persisted = if force {
        temp.persist(dest)
    } else {
        temp.persist_noclobber(dest)
    };
    match persisted {
        Ok(_) => Ok(WriteOutcome::Written(body.len() as u64)),
        Err(error) if !force && error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            Ok(WriteOutcome::Exists)
        }
        Err(_) => Err(write_error()),
    }
}

fn write_error() -> CliError {
    CliError::new(
        5,
        "upstream",
        "cannot write the downloaded attachment",
        "No partial file was kept. Check the output directory permissions.",
    )
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::testing::Scripted;
    use serde_json::Value;
    use std::collections::VecDeque;

    const ASSIGNMENT: &str = "1000000000000000001";
    const TOKEN: &str = "synthetic-access-token";
    const API: &str = "https://apiucloud.bupt.edu.cn";
    const FILE: &str = "https://fileucloud.bupt.edu.cn/ucloud/document";

    fn session() -> Session {
        Session::new(TOKEN.into(), "synthetic-user".into())
    }

    fn scripted(entries: Vec<(&'static str, String, CliResult<Response>)>) -> Scripted {
        Scripted {
            expected: VecDeque::from(entries),
            calls: Vec::new(),
        }
    }

    fn response(status: u16, body: &[u8]) -> CliResult<Response> {
        Ok(Response {
            status,
            headers: Vec::new(),
            body: body.to_vec(),
        })
    }

    fn json_response(status: u16, body: Value) -> CliResult<Response> {
        response(status, &serde_json::to_vec(&body).expect("synthetic JSON"))
    }

    fn detail_url() -> String {
        format!("{API}/ykt-site/work/detail?assignmentId={ASSIGNMENT}")
    }

    fn preview_url(resource_id: &str) -> String {
        format!("{API}/blade-source/resource/preview-url?resourceId={resource_id}")
    }

    fn detail_with_attachments(attachments: Value) -> CliResult<Response> {
        json_response(
            200,
            json!({"code": 200, "data": {
                "assignmentTitle": "合成作业",
                "assignmentResource": attachments,
            }}),
        )
    }

    fn assert_download_headers(transport: &Scripted) {
        for call in &transport.calls {
            if call.url.starts_with("https://fileucloud.bupt.edu.cn/") {
                assert_eq!(call.method, "GET");
                assert_eq!(
                    call.headers,
                    vec![("User-Agent".to_owned(), USER_AGENT.to_owned())]
                );
                assert!(!call.headers.iter().any(|(name, _)| {
                    matches!(
                        name.to_ascii_lowercase().as_str(),
                        "blade-auth" | "authorization" | "cookie"
                    )
                }));
            }
        }
    }

    #[test]
    fn download_saves_preview_urls_and_skips_missing_urls() {
        let temp = tempfile::tempdir().expect("temp dir");
        let mut transport = scripted(vec![
            (
                "GET",
                detail_url(),
                detail_with_attachments(json!([
                    {"resourceId": "r1", "resourceName": "../报告.pdf"},
                    {"resourceId": "r2", "resourceName": "无链接.txt"},
                    {"resourceId": "r3", "resourceName": "有链接.txt"}
                ])),
            ),
            (
                "GET",
                preview_url("r1"),
                json_response(
                    200,
                    json!({"code": 200, "data": {"previewUrl": format!("{FILE}/report.pdf?token=one")}}),
                ),
            ),
            (
                "GET",
                preview_url("r2"),
                json_response(200, json!({"code": 200, "data": {"previewUrl": ""}})),
            ),
            (
                "GET",
                preview_url("r3"),
                json_response(
                    200,
                    json!({"code": 200, "data": {"previewUrl": format!("{FILE}/notes.txt?token=two")}}),
                ),
            ),
            (
                "GET",
                format!("{FILE}/report.pdf?token=one"),
                response(200, b"report"),
            ),
            (
                "GET",
                format!("{FILE}/notes.txt?token=two"),
                response(200, b"notes"),
            ),
        ]);

        let outcome = execute_download(ASSIGNMENT, temp.path(), false, &session(), &mut transport)
            .expect("download");

        assert_eq!(outcome.data["assignment_id"], ASSIGNMENT);
        assert_eq!(outcome.data["count_saved"], 2);
        assert_eq!(outcome.data["count_skipped"], 1);
        assert_eq!(outcome.data["skipped"][0]["reason"], "no_url");
        assert_eq!(outcome.data["skipped"][0]["path"], Value::Null);
        let first = &outcome.data["saved"][0];
        assert_eq!(first["title"], "../报告.pdf");
        assert_eq!(first["resource_id"], "r1");
        assert_eq!(first["size"], 6);
        let first_path = Path::new(first["path"].as_str().unwrap());
        let canonical_temp = fs::canonicalize(temp.path()).expect("canonical temp dir");
        assert_eq!(first_path.parent(), Some(canonical_temp.as_path()));
        assert_eq!(first_path.file_name().unwrap(), ".._报告.pdf");
        assert_eq!(fs::read(first_path).unwrap(), b"report");
        assert_eq!(fs::read(temp.path().join("有链接.txt")).unwrap(), b"notes");
        assert_download_headers(&transport);
    }

    #[test]
    fn download_skips_existing_files_without_force_and_overwrites_with_force() {
        let temp = tempfile::tempdir().expect("temp dir");
        fs::write(temp.path().join("keep.txt"), b"old").unwrap();
        let mut transport = scripted(vec![
            (
                "GET",
                detail_url(),
                detail_with_attachments(json!([{"resourceId": "r1", "resourceName": "keep.txt"}])),
            ),
            (
                "GET",
                preview_url("r1"),
                json_response(
                    200,
                    json!({"code": 200, "data": {"previewUrl": format!("{FILE}/keep.txt?token=1")}}),
                ),
            ),
        ]);
        let outcome = execute_download(ASSIGNMENT, temp.path(), false, &session(), &mut transport)
            .expect("skip existing");
        assert_eq!(outcome.data["count_saved"], 0);
        assert_eq!(outcome.data["skipped"][0]["reason"], "exists");
        assert_eq!(fs::read(temp.path().join("keep.txt")).unwrap(), b"old");
        assert_eq!(transport.calls.len(), 2);

        let mut transport = scripted(vec![
            (
                "GET",
                detail_url(),
                detail_with_attachments(json!([{"resourceId": "r1", "resourceName": "keep.txt"}])),
            ),
            (
                "GET",
                preview_url("r1"),
                json_response(
                    200,
                    json!({"code": 200, "data": {"previewUrl": format!("{FILE}/keep.txt?token=2")}}),
                ),
            ),
            (
                "GET",
                format!("{FILE}/keep.txt?token=2"),
                response(200, b"new"),
            ),
        ]);
        let outcome = execute_download(ASSIGNMENT, temp.path(), true, &session(), &mut transport)
            .expect("force existing");
        assert_eq!(outcome.data["count_saved"], 1);
        assert_eq!(fs::read(temp.path().join("keep.txt")).unwrap(), b"new");
    }

    #[test]
    fn download_follows_relative_and_absolute_allowlisted_redirects_without_auth() {
        let temp = tempfile::tempdir().expect("temp dir");
        let mut transport = scripted(vec![
            (
                "GET",
                detail_url(),
                detail_with_attachments(json!([{"resourceId": "r1", "resourceName": "a.txt"}])),
            ),
            (
                "GET",
                preview_url("r1"),
                json_response(
                    200,
                    json!({"code": 200, "data": {"previewUrl": format!("{FILE}/start")}}),
                ),
            ),
            (
                "GET",
                format!("{FILE}/start"),
                Ok(Response {
                    status: 302,
                    headers: vec![("Location".into(), "/next".into())],
                    body: Vec::new(),
                }),
            ),
            (
                "GET",
                "https://fileucloud.bupt.edu.cn/next".into(),
                Ok(Response {
                    status: 307,
                    headers: vec![(
                        "Location".into(),
                        "https://fileucloud.bupt.edu.cn/final".into(),
                    )],
                    body: Vec::new(),
                }),
            ),
            (
                "GET",
                "https://fileucloud.bupt.edu.cn/final".into(),
                response(200, b"payload"),
            ),
        ]);

        let outcome = execute_download(ASSIGNMENT, temp.path(), false, &session(), &mut transport)
            .expect("redirected download");
        assert_eq!(outcome.data["count_saved"], 1);
        assert_eq!(fs::read(temp.path().join("a.txt")).unwrap(), b"payload");
        assert_download_headers(&transport);
    }

    #[test]
    fn download_rejects_a_redirect_off_the_allowlist_without_writing() {
        let temp = tempfile::tempdir().expect("temp dir");
        let mut transport = scripted(vec![
            (
                "GET",
                detail_url(),
                detail_with_attachments(json!([{"resourceId": "r1", "resourceName": "a.txt"}])),
            ),
            (
                "GET",
                preview_url("r1"),
                json_response(
                    200,
                    json!({"code": 200, "data": {"previewUrl": format!("{FILE}/start")}}),
                ),
            ),
            (
                "GET",
                format!("{FILE}/start"),
                Ok(Response {
                    status: 302,
                    headers: vec![("Location".into(), "https://evil.invalid/a.txt".into())],
                    body: Vec::new(),
                }),
            ),
        ]);

        let error = execute_download(ASSIGNMENT, temp.path(), false, &session(), &mut transport)
            .err()
            .expect("unsafe redirect");

        assert_eq!(error.code, 5);
        assert!(error.message.contains("allowlist"));
        assert!(!temp.path().join("a.txt").exists());
        assert_eq!(transport.calls.len(), 3);
    }

    #[test]
    fn download_allows_only_the_exact_api_download_path_without_auth() {
        let source = format!("{API}/blade-source/resource/download?resourceId=r1");
        let mut transport = scripted(vec![("GET", source.clone(), response(200, b"api-bytes"))]);
        assert_eq!(
            download_body(&mut transport, &source).expect("api source"),
            b"api-bytes"
        );
        assert_eq!(transport.calls.len(), 1);
        assert_eq!(
            transport.calls[0].headers,
            vec![("User-Agent".to_owned(), USER_AGENT.to_owned())]
        );

        let mut transport = scripted(vec![]);
        let error = download_body(
            &mut transport,
            &format!("{API}/blade-source/resource/preview-url?resourceId=r1"),
        )
        .expect_err("preview URL is not a download source");
        assert_eq!(error.code, 5);
        assert!(transport.calls.is_empty());
    }

    #[test]
    fn download_rejects_a_redirect_loop_without_writing() {
        let temp = tempfile::tempdir().expect("temp dir");
        let redirect = || {
            Ok(Response {
                status: 302,
                headers: vec![("Location".into(), "/loop".into())],
                body: Vec::new(),
            })
        };
        let mut transport = scripted(vec![
            (
                "GET",
                detail_url(),
                detail_with_attachments(json!([{"resourceId": "r1", "resourceName": "a.txt"}])),
            ),
            (
                "GET",
                preview_url("r1"),
                json_response(
                    200,
                    json!({"code": 200, "data": {"previewUrl": format!("{FILE}/start")}}),
                ),
            ),
            ("GET", format!("{FILE}/start"), redirect()),
            (
                "GET",
                "https://fileucloud.bupt.edu.cn/loop".into(),
                redirect(),
            ),
            (
                "GET",
                "https://fileucloud.bupt.edu.cn/loop".into(),
                redirect(),
            ),
            (
                "GET",
                "https://fileucloud.bupt.edu.cn/loop".into(),
                redirect(),
            ),
            (
                "GET",
                "https://fileucloud.bupt.edu.cn/loop".into(),
                redirect(),
            ),
            (
                "GET",
                "https://fileucloud.bupt.edu.cn/loop".into(),
                redirect(),
            ),
        ]);

        let error = execute_download(ASSIGNMENT, temp.path(), false, &session(), &mut transport)
            .err()
            .expect("redirect loop");
        assert_eq!(error.code, 5);
        assert!(error.message.contains("too many"));
        assert!(!temp.path().join("a.txt").exists());
    }

    #[test]
    fn download_rejects_relative_non_https_and_credentialed_sources() {
        for source in [
            "/relative/file.pdf",
            "http://fileucloud.bupt.edu.cn/file.pdf",
            "https://user:pass@fileucloud.bupt.edu.cn/file.pdf",
            "https://evil.invalid/file.pdf",
            "https://fileucloud.bupt.edu.cn/file.pdf#fragment",
            "https://fileucloud.bupt.edu.cn:444/file.pdf",
        ] {
            let mut transport = scripted(vec![]);
            let error = download_body(&mut transport, source).expect_err(source);
            assert_eq!(error.code, 5, "{source}");
            assert!(transport.calls.is_empty(), "{source}");
        }
    }

    #[test]
    fn download_does_not_keep_a_partial_or_old_file_when_a_download_fails() {
        let temp = tempfile::tempdir().expect("temp dir");
        fs::write(temp.path().join("keep.txt"), b"old").unwrap();
        let mut transport = scripted(vec![
            (
                "GET",
                detail_url(),
                detail_with_attachments(json!([{"resourceId": "r1", "resourceName": "keep.txt"}])),
            ),
            (
                "GET",
                preview_url("r1"),
                json_response(
                    200,
                    json!({"code": 200, "data": {"previewUrl": format!("{FILE}/keep.txt")}}),
                ),
            ),
            (
                "GET",
                format!("{FILE}/keep.txt"),
                Err(CliError::new(4, "network", "synthetic disconnect", "")),
            ),
        ]);

        let error = execute_download(ASSIGNMENT, temp.path(), true, &session(), &mut transport)
            .err()
            .expect("failed download");
        assert_eq!(error.code, 4);
        assert_eq!(fs::read(temp.path().join("keep.txt")).unwrap(), b"old");
    }

    #[test]
    fn safe_filename_keeps_a_single_safe_component() {
        assert_eq!(safe_filename("../报告.pdf", 0), ".._报告.pdf");
        assert_eq!(safe_filename("../../evil", 0), ".._.._evil");
        assert_eq!(safe_filename("/etc/passwd", 0), "_etc_passwd");
        assert_eq!(
            safe_filename("a/b\\c:d*e?f\"g<h>i|j", 0),
            "a_b_c_d_e_f_g_h_i_j"
        );
        assert_eq!(safe_filename("..", 2), "attachment-3");
        assert_eq!(safe_filename(" . ", 1), "attachment-2");
        let long = "长".repeat(MAX_FILENAME_BYTES);
        assert!(safe_filename(&long, 0).len() <= MAX_FILENAME_BYTES);
    }
}
