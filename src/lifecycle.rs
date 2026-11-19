use crate::cli::{CliError, CliResult, Context, Outcome};
use crate::transport::Request;
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{Value, json};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const DEFAULT_REPO: &str = "tututuhehehe/bupt-cli";
const GITHUB_RELEASE_HOST: &str = "github.com";
const MACOS_ARM64_ASSET: &str = "bupt-macos-arm64.zip";
const WINDOWS_X64_ASSET: &str = "bupt-windows-x64.zip";
const USER_AGENT: &str = "bupt-cli";

pub(crate) fn commands() -> Vec<Command> {
    vec![
        Command::new("update")
            .about("Check for or install a new release")
            .arg(
                Arg::new("check")
                    .long("check")
                    .action(ArgAction::SetTrue)
                    .help("Check only; do not install"),
            )
            .arg(
                Arg::new("repo")
                    .long("repo")
                    .value_name("OWNER/REPO")
                    .help("GitHub release repository"),
            ),
        Command::new("uninstall")
            .about("Remove the current installation")
            .arg(
                Arg::new("keep-data")
                    .long("keep-data")
                    .action(ArgAction::SetTrue)
                    .help("Keep credentials, sessions and timetable data"),
            )
            .arg(
                Arg::new("dry-run")
                    .long("dry-run")
                    .action(ArgAction::SetTrue)
                    .help("List planned changes without touching files"),
            ),
    ]
}

pub(crate) fn run(
    name: &str,
    matches: &ArgMatches,
    context: &mut Context<'_>,
) -> CliResult<Outcome> {
    match name {
        "update" => update(matches, context),
        "uninstall" => uninstall(matches, context),
        _ => Err(CliError::usage(format!(
            "unknown lifecycle command: {name}"
        ))),
    }
}

fn update(matches: &ArgMatches, context: &mut Context<'_>) -> CliResult<Outcome> {
    let repo = selected_repo(matches)?;
    if !matches.get_flag("check") {
        return apply_update(&repo);
    }

    let release = latest_release(context, &repo)?;
    let current = env!("CARGO_PKG_VERSION");
    let (latest, update_available, note) = match release {
        Release::Missing => (
            None,
            Some(false),
            format!("{repo} 没有可用的 release（或仓库不存在）；用 --repo 指定仓库"),
        ),
        Release::NoVersion => (None, None, "release 跳转里没有版本号".to_owned()),
        Release::Tag(tag) => {
            let latest = tag.trim_start_matches('v').to_owned();
            if is_newer(&latest, current) {
                (
                    Some(latest),
                    Some(true),
                    "有新版本；此构建仅提供版本检查，不执行安装".to_owned(),
                )
            } else {
                (Some(latest), Some(false), "已经是最新版本".to_owned())
            }
        }
    };
    Ok(Outcome {
        data: json!({
            "current": current,
            "repo": repo,
            "latest": latest,
            "update_available": update_available,
            "note": note,
        }),
        code: 0,
        diagnostic: None,
    })
}

/// The apply path still needs download, integrity checks, staging and a safe
/// replacement. Keep it offline and pending instead of pretending to update.
fn apply_update(repo: &str) -> CliResult<Outcome> {
    let asset = release_asset(std::env::consts::OS, std::env::consts::ARCH).ok_or_else(|| {
        CliError::new(
            5,
            "upstream",
            format!(
                "no release asset is defined for {}/{}",
                std::env::consts::OS,
                std::env::consts::ARCH
            ),
            "This build targets macOS Apple Silicon and Windows x64.",
        )
    })?;
    let target = current_verified_executable()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| "no verified current executable".into());
    Err(CliError::new(
        5,
        "upstream",
        format!("update installation for {repo} is not available in this migration build"),
        format!(
            "Would use {asset} and target {target}; downloads, replacement and the Windows runtime swap are pending."
        ),
    ))
}

enum Release {
    Missing,
    NoVersion,
    Tag(String),
}

fn latest_release(context: &mut Context<'_>, repo: &str) -> CliResult<Release> {
    let url = format!("https://{GITHUB_RELEASE_HOST}/{repo}/releases/latest");
    let response = context.transport.send(Request {
        method: "GET",
        url,
        headers: vec![("User-Agent".into(), USER_AGENT.into())],
        body: Vec::new(),
    })?;
    match response.status {
        301 | 302 | 303 | 307 | 308 => {
            let location = header(&response.headers, "location")
                .ok_or_else(|| malformed_release_redirect("redirect has no Location header"))?;
            let tag = release_tag_from_location(location, repo)?;
            if tag == "latest" {
                Ok(Release::NoVersion)
            } else {
                Ok(Release::Tag(tag))
            }
        }
        404 => match header(&response.headers, "location") {
            Some(location) if !location.trim().is_empty() => {
                Err(upstream_http_error(response.status))
            }
            _ => Ok(Release::Missing),
        },
        401 | 403 => Err(CliError::new(
            3,
            "auth",
            format!("release check was rejected (HTTP {})", response.status),
            "GitHub refused the release request.",
        )),
        status => Err(upstream_http_error(status)),
    }
}

fn release_tag_from_location(location: &str, repo: &str) -> CliResult<String> {
    let location = location.trim();
    if location.is_empty()
        || location
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(malformed_release_redirect(
            "Location is empty or contains whitespace",
        ));
    }
    let path = location
        .strip_prefix("https://")
        .and_then(|rest| rest.strip_prefix(GITHUB_RELEASE_HOST))
        .and_then(|rest| rest.strip_prefix('/'))
        .ok_or_else(|| malformed_release_redirect("Location must be HTTPS on github.com"))?;
    if path.contains(['?', '#']) {
        return Err(malformed_release_redirect(
            "Location must not carry a query or fragment",
        ));
    }
    let prefix = format!("{repo}/releases/tag/");
    let tag = path
        .get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(&prefix))
        .and_then(|_| path.get(prefix.len()..))
        .ok_or_else(|| {
            malformed_release_redirect("Location must name this repository's release tag")
        })?;
    if tag.is_empty()
        || tag.contains('/')
        || tag.contains('%')
        || tag.contains('\\')
        || tag == "."
        || tag == ".."
    {
        return Err(malformed_release_redirect(
            "Location does not name a single release tag",
        ));
    }
    Ok(tag.to_owned())
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn malformed_release_redirect(reason: &str) -> CliError {
    CliError::new(
        5,
        "upstream",
        format!("release check returned an invalid redirect: {reason}"),
        "Expected https://github.com/OWNER/REPO/releases/tag/TAG.",
    )
}

fn upstream_http_error(status: u16) -> CliError {
    let hint = if status == 429 {
        "GitHub is rate limiting release checks; wait before retrying."
    } else {
        "GitHub returned an unexpected response."
    };
    CliError::new(
        5,
        "upstream",
        format!("release check failed with HTTP {status}"),
        hint,
    )
}

/// Mirrors the Python comparison: only the first three numeric runs count, and
/// a shorter tuple is older than a longer one with the same prefix. Comparing
/// strings would rank `1.10.0` below `1.9.0`.
fn is_newer(latest: &str, current: &str) -> bool {
    version_parts(latest) > version_parts(current)
}

fn version_parts(text: &str) -> Vec<u64> {
    let mut parts = Vec::new();
    let mut digits = String::new();
    for character in text.chars() {
        if character.is_ascii_digit() {
            digits.push(character);
        } else if !digits.is_empty() {
            parts.push(digits.parse().unwrap_or(u64::MAX));
            digits.clear();
            if parts.len() == 3 {
                return parts;
            }
        }
    }
    if parts.len() < 3 && !digits.is_empty() {
        parts.push(digits.parse().unwrap_or(u64::MAX));
    }
    if parts.is_empty() {
        parts.push(0);
    }
    parts
}

fn selected_repo(matches: &ArgMatches) -> CliResult<String> {
    let repo = matches
        .get_one::<String>("repo")
        .map(String::as_str)
        .map(str::to_owned)
        .or_else(|| {
            std::env::var("BUPT_CLI_REPO")
                .ok()
                .filter(|repo| !repo.is_empty())
        })
        .unwrap_or_else(|| DEFAULT_REPO.to_owned());
    validate_repo(&repo)
}

fn validate_repo(repo: &str) -> CliResult<String> {
    let mut parts = repo.split('/');
    let owner = parts.next().unwrap_or("");
    let name = parts.next().unwrap_or("");
    if parts.next().is_some() || !valid_repo_part(owner) || !valid_repo_part(name) {
        return Err(CliError::usage(
            "--repo must be a GitHub OWNER/REPO, not a URL or path",
        ));
    }
    Ok(repo.to_owned())
}

fn valid_repo_part(part: &str) -> bool {
    !part.is_empty()
        && part != "."
        && part != ".."
        && part.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
}

fn release_asset(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Some(MACOS_ARM64_ASSET),
        ("windows", "x86_64") => Some(WINDOWS_X64_ASSET),
        _ => None,
    }
}

fn uninstall(matches: &ArgMatches, context: &Context<'_>) -> CliResult<Outcome> {
    let dry_run = matches.get_flag("dry-run");
    let keep_data = matches.get_flag("keep-data");
    let mut removed = Vec::new();
    let mut skipped = Vec::new();
    let mut other_skips = 0usize;
    let mut kept = 0usize;

    if let Ok(path) = std::env::current_exe() {
        if let Some(executable) = verified_executable(&path) {
            let before = skipped.len();
            plan_current_executable(&executable, dry_run, &mut removed, &mut skipped);
            if skipped.len() > before {
                other_skips += 1;
            }
        } else {
            skipped.push(with_reason(
                &path,
                "current executable is not a verified bupt binary",
            ));
            other_skips += 1;
        }
    }

    let mut data_dirs = Vec::new();
    for path in [&context.config_dir, &context.cache_dir] {
        if !data_dirs.contains(path) {
            data_dirs.push(path.clone());
        }
    }
    for path in data_dirs {
        let plan = plan_data_dir(&path, keep_data, context);
        execute_data_plan(
            plan,
            dry_run,
            &mut removed,
            &mut skipped,
            &mut other_skips,
            &mut kept,
        );
    }

    let note = uninstall_note(dry_run, removed.len(), other_skips, kept);
    Ok(Outcome {
        data: json!({
            "dry_run": dry_run,
            "removed": removed,
            "skipped": skipped,
            "note": note,
        }),
        code: 0,
        diagnostic: None,
    })
}

fn current_verified_executable() -> Option<PathBuf> {
    verified_executable(&std::env::current_exe().ok()?)
}

fn verified_executable(path: &Path) -> Option<PathBuf> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() == 0 {
        return None;
    }
    let expected = if cfg!(windows) { "bupt.exe" } else { "bupt" };
    if path.file_name()?.to_str()? != expected {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if metadata.permissions().mode() & 0o111 == 0 {
            return None;
        }
    }
    Some(path.to_path_buf())
}

fn plan_current_executable(
    path: &Path,
    dry_run: bool,
    removed: &mut Vec<String>,
    skipped: &mut Vec<String>,
) {
    #[cfg(windows)]
    {
        let _ = dry_run;
        skipped.push(with_reason(
            path,
            "replacing or removing the running .exe is pending on Windows",
        ));
    }
    #[cfg(not(windows))]
    {
        if dry_run {
            removed.push(path.to_string_lossy().into_owned());
        } else {
            match fs::remove_file(path) {
                Ok(()) => removed.push(path.to_string_lossy().into_owned()),
                Err(error) => skipped.push(with_reason(path, &error.to_string())),
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(not(unix))]
    created: Option<SystemTime>,
}

fn file_stamp(metadata: &fs::Metadata) -> FileStamp {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        FileStamp {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            dev: metadata.dev(),
            ino: metadata.ino(),
        }
    }
    #[cfg(not(unix))]
    {
        FileStamp {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            created: metadata.created().ok(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DirIdentity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(not(unix))]
    canonical: PathBuf,
    #[cfg(not(unix))]
    created: Option<SystemTime>,
}

fn dir_identity(path: &Path, metadata: &fs::Metadata) -> DirIdentity {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let _ = path;
        DirIdentity {
            dev: metadata.dev(),
            ino: metadata.ino(),
        }
    }
    #[cfg(not(unix))]
    {
        DirIdentity {
            canonical: fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
            created: metadata.created().ok(),
        }
    }
}

#[derive(Clone, Debug)]
struct VerifiedFile {
    path: PathBuf,
    stamp: FileStamp,
}

#[derive(Clone, Debug)]
struct VerifiedDir {
    path: PathBuf,
    identity: DirIdentity,
    files: Vec<VerifiedFile>,
}

enum DataPlan {
    Missing,
    Remove(VerifiedDir),
    Skip {
        path: PathBuf,
        reason: String,
        keep: bool,
    },
}

fn plan_data_dir(path: &Path, keep_data: bool, context: &Context<'_>) -> DataPlan {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return DataPlan::Missing,
        Err(error) => {
            return DataPlan::Skip {
                path: path.to_path_buf(),
                reason: format!("cannot inspect: {error}"),
                keep: false,
            };
        }
    };
    if keep_data {
        return DataPlan::Skip {
            path: path.to_path_buf(),
            reason: "kept by --keep-data".into(),
            keep: true,
        };
    }
    if metadata.file_type().is_symlink() {
        return DataPlan::Skip {
            path: path.to_path_buf(),
            reason: "symbolic link".into(),
            keep: false,
        };
    }
    if !metadata.is_dir() {
        return DataPlan::Skip {
            path: path.to_path_buf(),
            reason: "not a directory".into(),
            keep: false,
        };
    }
    if dangerous_data_root(path, context) {
        return DataPlan::Skip {
            path: path.to_path_buf(),
            reason: "refusing dangerous data root".into(),
            keep: false,
        };
    }
    match verified_own_dir(path) {
        Ok(Some(verified)) => DataPlan::Remove(verified),
        Ok(None) => DataPlan::Skip {
            path: path.to_path_buf(),
            reason: "unverified or mixed data directory".into(),
            keep: false,
        },
        Err(error) => DataPlan::Skip {
            path: path.to_path_buf(),
            reason: format!("cannot verify: {error}"),
            keep: false,
        },
    }
}

fn execute_data_plan(
    plan: DataPlan,
    dry_run: bool,
    removed: &mut Vec<String>,
    skipped: &mut Vec<String>,
    other_skips: &mut usize,
    kept: &mut usize,
) {
    match plan {
        DataPlan::Missing => {}
        DataPlan::Remove(verified) => {
            if dry_run {
                removed.push(verified.path.to_string_lossy().into_owned());
            } else {
                let before = skipped.len();
                remove_verified_dir(verified, removed, skipped);
                if skipped.len() > before {
                    *other_skips += 1;
                }
            }
        }
        DataPlan::Skip { path, reason, keep } => {
            skipped.push(with_reason(&path, &reason));
            if keep {
                *kept += 1;
            } else {
                *other_skips += 1;
            }
        }
    }
}

fn dangerous_data_root(path: &Path, context: &Context<'_>) -> bool {
    let Ok(resolved) = path.canonicalize() else {
        return true;
    };
    let home = context
        .home_dir
        .canonicalize()
        .unwrap_or_else(|_| context.home_dir.clone());
    let Ok(current) = std::env::current_dir().and_then(|path| path.canonicalize()) else {
        return true;
    };
    is_dangerous_root(&resolved, &home, &current)
}

fn is_dangerous_root(resolved: &Path, home: &Path, current: &Path) -> bool {
    resolved.parent().is_none() || home.starts_with(resolved) || current.starts_with(resolved)
}

fn verified_own_dir(path: &Path) -> io::Result<Option<VerifiedDir>> {
    let directory = fs::symlink_metadata(path)?;
    if !directory.file_type().is_dir() {
        return Ok(None);
    }
    let identity = dir_identity(path, &directory);
    let mut files = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            return Ok(None);
        };
        if !is_known_name(&name) {
            return Ok(None);
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if !metadata.file_type().is_file() {
            return Ok(None);
        }
        let bytes = fs::read(entry.path())?;
        if !is_own_file(&name, &bytes) {
            return Ok(None);
        }
        files.push(VerifiedFile {
            path: entry.path(),
            stamp: file_stamp(&metadata),
        });
    }
    if files.is_empty() {
        return Ok(None);
    }
    let after = fs::symlink_metadata(path)?;
    if !after.file_type().is_dir() || dir_identity(path, &after) != identity {
        return Ok(None);
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(Some(VerifiedDir {
        path: path.to_path_buf(),
        identity,
        files,
    }))
}

fn verify_dir_for_removal(verified: &VerifiedDir) -> Result<(), String> {
    let metadata = fs::symlink_metadata(&verified.path)
        .map_err(|error| format!("cannot inspect directory: {error}"))?;
    if !metadata.file_type().is_dir()
        || dir_identity(&verified.path, &metadata) != verified.identity
    {
        return Err("directory metadata changed after the plan".into());
    }

    let mut current = Vec::new();
    for entry in
        fs::read_dir(&verified.path).map_err(|error| format!("cannot read directory: {error}"))?
    {
        current.push(
            entry
                .map_err(|error| format!("cannot read directory entry: {error}"))?
                .file_name(),
        );
    }
    current.sort();
    let mut expected = Vec::new();
    for file in &verified.files {
        let name = file
            .path
            .file_name()
            .ok_or_else(|| "invalid file path in plan".to_owned())?;
        expected.push(name.to_os_string());
    }
    expected.sort();
    if current != expected {
        return Err("directory contents changed after the plan".into());
    }

    for file in &verified.files {
        let metadata = fs::symlink_metadata(&file.path)
            .map_err(|error| format!("cannot inspect {}: {error}", file.path.display()))?;
        if !metadata.file_type().is_file() || file_stamp(&metadata) != file.stamp {
            return Err(format!("{} changed after the plan", file.path.display()));
        }
    }
    Ok(())
}

fn dir_path_unchanged(verified: &VerifiedDir) -> bool {
    fs::symlink_metadata(&verified.path)
        .map(|metadata| {
            metadata.file_type().is_dir()
                && dir_identity(&verified.path, &metadata) == verified.identity
        })
        .unwrap_or(false)
}

fn remove_verified_dir(
    verified: VerifiedDir,
    removed: &mut Vec<String>,
    skipped: &mut Vec<String>,
) {
    if let Err(reason) = verify_dir_for_removal(&verified) {
        skipped.push(with_reason(
            &verified.path,
            &format!("not removed: {reason}"),
        ));
        return;
    }

    let mut deleted = Vec::new();
    for file in &verified.files {
        if !dir_path_unchanged(&verified) {
            report_partial(
                &verified.path,
                &deleted,
                "directory metadata changed during removal",
                removed,
                skipped,
            );
            return;
        }
        let unchanged = fs::symlink_metadata(&file.path).is_ok_and(|metadata| {
            metadata.file_type().is_file() && file_stamp(&metadata) == file.stamp
        });
        if !unchanged {
            report_partial(
                &verified.path,
                &deleted,
                &format!("{} changed during removal", file.path.display()),
                removed,
                skipped,
            );
            return;
        }
        if let Err(error) = fs::remove_file(&file.path) {
            report_partial(
                &verified.path,
                &deleted,
                &format!("cannot remove {}: {error}", file.path.display()),
                removed,
                skipped,
            );
            return;
        }
        deleted.push(file.path.clone());
    }

    if !dir_path_unchanged(&verified) {
        report_partial(
            &verified.path,
            &deleted,
            "directory metadata changed before the final removal",
            removed,
            skipped,
        );
        return;
    }
    match fs::read_dir(&verified.path) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                report_partial(
                    &verified.path,
                    &deleted,
                    "new entries appeared after the plan; directory kept",
                    removed,
                    skipped,
                );
                return;
            }
        }
        Err(error) => {
            report_partial(
                &verified.path,
                &deleted,
                &format!("cannot verify empty directory: {error}"),
                removed,
                skipped,
            );
            return;
        }
    }
    match fs::remove_dir(&verified.path) {
        Ok(()) => removed.push(verified.path.to_string_lossy().into_owned()),
        Err(error) => report_partial(
            &verified.path,
            &deleted,
            &format!("directory could not be removed: {error}"),
            removed,
            skipped,
        ),
    }
}

fn report_partial(
    dir: &Path,
    deleted: &[PathBuf],
    reason: &str,
    removed: &mut Vec<String>,
    skipped: &mut Vec<String>,
) {
    if deleted.is_empty() {
        skipped.push(with_reason(dir, &format!("not removed: {reason}")));
        return;
    }
    removed.extend(
        deleted
            .iter()
            .map(|path| path.to_string_lossy().into_owned()),
    );
    skipped.push(with_reason(dir, &format!("partial removal: {reason}")));
}

fn uninstall_note(dry_run: bool, removed: usize, other_skips: usize, kept: usize) -> &'static str {
    if dry_run {
        return "未做任何修改";
    }
    match (removed > 0, other_skips > 0, kept > 0) {
        (false, false, false) => "没有可移除的已验证目标",
        (true, false, false) => "卸载完成",
        (false, false, true) => "已按 --keep-data 保留数据；未移除任何项目",
        (true, false, true) => "已清理可移除项目；数据按 --keep-data 保留",
        (false, true, false) => "未移除内容：项目被跳过、删除失败或仍待处理",
        (true, true, false) => "部分完成：部分项目已移除；另有项目被跳过、删除失败或仍待处理",
        (false, true, true) => {
            "未移除内容：数据按 --keep-data 保留；另有项目被跳过、删除失败或仍待处理"
        }
        (true, true, true) => {
            "部分完成：已移除部分项目；数据按 --keep-data 保留；另有项目被跳过、删除失败或仍待处理"
        }
    }
}

fn is_known_name(name: &str) -> bool {
    matches!(
        name,
        "credentials.json" | "session.json" | "config.json" | "timetable.json"
    )
}

fn is_own_file(name: &str, bytes: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return false;
    };
    let Some(object) = value.as_object() else {
        return false;
    };
    match name {
        "credentials.json" => {
            nonempty_string(object, "username")
                && (nonempty_string(object, "password") || is_known_role(object))
        }
        "session.json" => {
            matches!(object.get("version").and_then(Value::as_u64), Some(1 | 2))
                && nonempty_string(object, "username")
        }
        "config.json" => object
            .get("dorm")
            .and_then(Value::as_object)
            .is_some_and(|dorm| {
                dorm.values()
                    .any(|value| value.as_str().is_some_and(|text| !text.trim().is_empty()))
            }),
        "timetable.json" => {
            let version = object.get("version").and_then(Value::as_u64);
            if !matches!(version, Some(1 | 2)) {
                return false;
            }
            nonempty_string(object, "term")
                && is_known_role(object)
                && object.get("courses").is_some_and(Value::is_array)
                && object.get("calendar").is_some_and(Value::is_object)
                && (version != Some(2) || nonempty_string(object, "username"))
        }
        _ => false,
    }
}

fn is_known_role(object: &serde_json::Map<String, Value>) -> bool {
    matches!(
        object.get("role").and_then(Value::as_str),
        Some("graduate" | "undergraduate")
    )
}

fn nonempty_string(object: &serde_json::Map<String, Value>, key: &str) -> bool {
    object
        .get(key)
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty())
}

fn with_reason(path: &Path, reason: &str) -> String {
    format!("{}（{reason}）", path.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{Offline, Response, Transport, testing::Scripted};
    use std::fs;
    use std::time::Duration;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create test directory");
        }
        fs::write(path, contents).expect("write test file");
    }

    fn make_executable(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = fs::metadata(path)
                .expect("read test executable metadata")
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions).expect("mark test executable");
        }
    }

    fn binary_name() -> &'static str {
        if cfg!(windows) { "bupt.exe" } else { "bupt" }
    }

    fn test_context<'a>(root: &Path, transport: &'a mut dyn Transport) -> Context<'a> {
        Context {
            home_dir: root.join("home"),
            config_dir: root.join("config"),
            cache_dir: root.join("cache"),
            timeout: Duration::from_secs(1),
            transport,
        }
    }

    fn update_matches(args: &[&str]) -> ArgMatches {
        commands()
            .into_iter()
            .find(|command| command.get_name() == "update")
            .expect("update command")
            .try_get_matches_from(args)
            .expect("valid update arguments")
    }

    fn check_release(
        root: &Path,
        repo: &str,
        response: CliResult<Response>,
    ) -> (CliResult<Outcome>, Scripted) {
        let mut transport = Scripted::default();
        transport.expected.push_back((
            "GET",
            format!("https://github.com/{repo}/releases/latest"),
            response,
        ));
        let result = {
            let mut context = test_context(root, &mut transport);
            update(
                &update_matches(&["update", "--check", "--repo", repo]),
                &mut context,
            )
        };
        (result, transport)
    }

    fn response(status: u16, headers: &[(&str, &str)]) -> Response {
        Response {
            status,
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
            body: Vec::new(),
        }
    }

    fn tree_paths(root: &Path) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(&directory).expect("read test tree") {
                let entry = entry.expect("read test tree entry");
                let path = entry.path();
                paths.push(path.clone());
                if entry.file_type().expect("read test entry type").is_dir() {
                    pending.push(path);
                }
            }
        }
        paths.sort();
        paths
    }

    fn expect_error(result: CliResult<Outcome>, message: &str) -> CliError {
        match result {
            Ok(_) => panic!("{message}"),
            Err(error) => error,
        }
    }

    fn assert_upstream(error: CliError) -> CliError {
        assert_eq!(error.code, 5, "expected upstream exit: {error:?}");
        assert_eq!(error.kind, "upstream");
        error
    }

    fn valid_timetable() -> &'static str {
        r#"{"version":1,"term":"2099-2100秋学期","role":"undergraduate","courses":[],"calendar":{}}"#
    }

    #[test]
    fn repo_must_be_owner_slash_repo() {
        for valid in ["owner/repo", "owner/repo.js", "owner_1/repo-2"] {
            assert_eq!(validate_repo(valid).expect("valid repo"), valid);
        }
        for invalid in [
            "",
            "owner",
            "owner/",
            "/repo",
            "owner/repo/extra",
            "https://github.com/owner/repo",
            "owner/repo with space",
            "../repo",
        ] {
            let error = validate_repo(invalid).expect_err("invalid repo");
            assert_eq!(error.code, 2);
            assert_eq!(error.kind, "usage");
        }
    }

    #[test]
    fn release_assets_are_selected_per_supported_platform() {
        assert_eq!(release_asset("macos", "aarch64"), Some(MACOS_ARM64_ASSET));
        assert_eq!(release_asset("windows", "x86_64"), Some(WINDOWS_X64_ASSET));
        assert_eq!(release_asset("macos", "x86_64"), None);
        assert_eq!(release_asset("windows", "aarch64"), None);
        assert_eq!(release_asset("linux", "x86_64"), None);
    }

    #[test]
    fn only_a_regular_bupt_executable_is_a_target() {
        let root = tempfile::tempdir().expect("create target test directory");
        let target = root.path().join(binary_name());
        write(&target, "binary");
        make_executable(&target);
        assert_eq!(verified_executable(&target), Some(target.clone()));

        let other = root.path().join("other");
        write(&other, "binary");
        make_executable(&other);
        assert_eq!(verified_executable(&other), None);
        assert_eq!(verified_executable(&root.path().join("missing")), None);
        assert_eq!(verified_executable(root.path()), None);
    }

    #[cfg(not(windows))]
    #[test]
    fn removal_plans_only_the_given_executable() {
        let root = tempfile::tempdir().expect("create removal test directory");
        let target = root.path().join(binary_name());
        let other = root.path().join("other");
        write(&target, "binary");
        write(&other, "binary");
        make_executable(&target);
        make_executable(&other);

        let mut removed = Vec::new();
        let mut skipped = Vec::new();
        plan_current_executable(&target, false, &mut removed, &mut skipped);
        assert_eq!(removed, vec![target.to_string_lossy().into_owned()]);
        assert!(skipped.is_empty());
        assert!(!target.exists());
        assert!(other.exists());
    }

    #[cfg(not(windows))]
    #[test]
    fn dry_run_plans_without_removing_the_executable() {
        let root = tempfile::tempdir().expect("create dry-run test directory");
        let target = root.path().join(binary_name());
        write(&target, "binary");
        make_executable(&target);

        let mut removed = Vec::new();
        let mut skipped = Vec::new();
        plan_current_executable(&target, true, &mut removed, &mut skipped);
        assert_eq!(removed, vec![target.to_string_lossy().into_owned()]);
        assert!(skipped.is_empty());
        assert!(target.exists());
    }

    #[test]
    fn owned_data_requires_recognized_payloads() {
        let root = tempfile::tempdir().expect("create data test directory");
        let timetable = root.path().join("timetable.json");
        write(&timetable, valid_timetable());
        assert!(
            verified_own_dir(root.path())
                .expect("verify owned directory")
                .is_some()
        );

        write(
            &timetable,
            r#"{"version":999,"term":"lookalike","other":true}"#,
        );
        assert!(
            verified_own_dir(root.path())
                .expect("verify lookalike directory")
                .is_none()
        );

        write(&root.path().join("unrelated.txt"), "not ours");
        assert!(
            verified_own_dir(root.path())
                .expect("verify mixed directory")
                .is_none()
        );
    }

    #[test]
    fn timetable_ownership_requires_known_shape() {
        let root = tempfile::tempdir().expect("create timetable test directory");
        let timetable = root.path().join("timetable.json");
        write(&timetable, valid_timetable());
        assert!(
            verified_own_dir(root.path())
                .expect("accept v1 timetable")
                .is_some()
        );

        write(
            &timetable,
            r#"{"version":2,"term":"2099-2100秋学期","role":"graduate","courses":[],"calendar":{}}"#,
        );
        assert!(
            verified_own_dir(root.path())
                .expect("reject v2 without username")
                .is_none()
        );

        write(
            &timetable,
            r#"{"version":2,"username":"2026000001","term":"2099-2100秋学期","role":"graduate","courses":[],"calendar":{}}"#,
        );
        assert!(
            verified_own_dir(root.path())
                .expect("accept v2 with username")
                .is_some()
        );

        write(
            &timetable,
            r#"{"version":1,"term":"2099-2100秋学期","role":"student","courses":[],"calendar":{}}"#,
        );
        assert!(
            verified_own_dir(root.path())
                .expect("reject unknown role")
                .is_none()
        );

        write(
            &timetable,
            r#"{"version":1,"role":"graduate","courses":[],"calendar":{}}"#,
        );
        assert!(
            verified_own_dir(root.path())
                .expect("reject missing term")
                .is_none()
        );
    }

    #[test]
    fn logged_out_credentials_remain_recognizable() {
        let root = tempfile::tempdir().expect("create credentials test directory");
        let credentials = root.path().join("credentials.json");
        write(
            &credentials,
            r#"{"username":"2026000001","role":"graduate"}"#,
        );
        assert!(
            verified_own_dir(root.path())
                .expect("verify logged-out credentials")
                .is_some()
        );

        write(&credentials, r#"{"username":"2026000001"}"#);
        assert!(
            verified_own_dir(root.path())
                .expect("reject bare username")
                .is_none()
        );
    }

    #[test]
    fn dry_run_keeps_the_vetted_directory() {
        let root = tempfile::tempdir().expect("create dry-run data directory");
        fs::create_dir_all(root.path().join("home")).expect("create home");
        let cache = root.path().join("cache");
        fs::create_dir_all(&cache).expect("create cache");
        let timetable = cache.join("timetable.json");
        write(&timetable, valid_timetable());
        let mut transport = Offline;
        let context = test_context(root.path(), &mut transport);

        let plan = plan_data_dir(&cache, false, &context);
        assert!(matches!(&plan, DataPlan::Remove(_)));
        let mut removed = Vec::new();
        let mut skipped = Vec::new();
        let mut other_skips = 0;
        let mut kept = 0;
        execute_data_plan(
            plan,
            true,
            &mut removed,
            &mut skipped,
            &mut other_skips,
            &mut kept,
        );

        assert_eq!(removed, vec![cache.to_string_lossy().into_owned()]);
        assert!(skipped.is_empty());
        assert_eq!(other_skips, 0);
        assert_eq!(kept, 0);
        assert!(timetable.exists());
    }

    #[test]
    fn mixed_directory_is_rejected_before_removal() {
        let root = tempfile::tempdir().expect("create mixed test directory");
        fs::create_dir_all(root.path().join("home")).expect("create home");
        let cache = root.path().join("cache");
        fs::create_dir_all(&cache).expect("create cache");
        let timetable = cache.join("timetable.json");
        let unrelated = cache.join("notes.txt");
        write(&timetable, valid_timetable());
        write(&unrelated, "not ours");
        let mut transport = Offline;
        let context = test_context(root.path(), &mut transport);

        let plan = plan_data_dir(&cache, false, &context);
        assert!(matches!(&plan, DataPlan::Skip { keep: false, .. }));
        let mut removed = Vec::new();
        let mut skipped = Vec::new();
        let mut other_skips = 0;
        let mut kept = 0;
        execute_data_plan(
            plan,
            false,
            &mut removed,
            &mut skipped,
            &mut other_skips,
            &mut kept,
        );

        assert!(removed.is_empty());
        assert_eq!(other_skips, 1);
        assert_eq!(kept, 0);
        assert!(skipped.iter().any(|entry| entry.contains("cache")));
        assert!(timetable.exists());
        assert!(unrelated.exists());
    }

    #[test]
    fn late_unknown_file_survives_execution() {
        let root = tempfile::tempdir().expect("create late-file test directory");
        fs::create_dir_all(root.path().join("home")).expect("create home");
        let cache = root.path().join("cache");
        fs::create_dir_all(&cache).expect("create cache");
        let timetable = cache.join("timetable.json");
        write(&timetable, valid_timetable());
        let mut transport = Offline;
        let context = test_context(root.path(), &mut transport);

        let plan = plan_data_dir(&cache, false, &context);
        assert!(matches!(&plan, DataPlan::Remove(_)));
        let late = cache.join("late.txt");
        write(&late, "appeared after planning");
        let mut removed = Vec::new();
        let mut skipped = Vec::new();
        let mut other_skips = 0;
        let mut kept = 0;
        execute_data_plan(
            plan,
            false,
            &mut removed,
            &mut skipped,
            &mut other_skips,
            &mut kept,
        );

        assert!(removed.is_empty());
        assert_eq!(other_skips, 1);
        assert!(skipped.iter().any(|entry| entry.contains("cache")));
        assert!(timetable.exists());
        assert!(late.exists());
    }

    #[test]
    fn changed_owned_file_survives_execution() {
        let root = tempfile::tempdir().expect("create changed-file test directory");
        fs::create_dir_all(root.path().join("home")).expect("create home");
        let cache = root.path().join("cache");
        fs::create_dir_all(&cache).expect("create cache");
        let timetable = cache.join("timetable.json");
        write(&timetable, valid_timetable());
        let mut transport = Offline;
        let context = test_context(root.path(), &mut transport);

        let plan = plan_data_dir(&cache, false, &context);
        assert!(matches!(&plan, DataPlan::Remove(_)));
        let changed = r#"{"version":1,"term":"2099-2100秋学期","role":"undergraduate","courses":[],"calendar":{},"changed":true}"#;
        write(&timetable, changed);
        let mut removed = Vec::new();
        let mut skipped = Vec::new();
        let mut other_skips = 0;
        let mut kept = 0;
        execute_data_plan(
            plan,
            false,
            &mut removed,
            &mut skipped,
            &mut other_skips,
            &mut kept,
        );

        assert!(removed.is_empty());
        assert_eq!(other_skips, 1);
        assert!(timetable.exists());
        assert_eq!(
            fs::read_to_string(&timetable).expect("read timetable"),
            changed
        );
    }

    #[cfg(unix)]
    #[test]
    fn replaced_directory_survives_execution() {
        let root = tempfile::tempdir().expect("create replacement test directory");
        fs::create_dir_all(root.path().join("home")).expect("create home");
        let cache = root.path().join("cache");
        fs::create_dir_all(&cache).expect("create cache");
        write(&cache.join("timetable.json"), valid_timetable());
        let mut transport = Offline;
        let context = test_context(root.path(), &mut transport);

        let plan = plan_data_dir(&cache, false, &context);
        assert!(matches!(&plan, DataPlan::Remove(_)));
        let old = root.path().join("old-cache");
        fs::rename(&cache, &old).expect("move planned directory");
        fs::create_dir_all(&cache).expect("create replacement directory");
        let replacement = cache.join("timetable.json");
        write(&replacement, valid_timetable());

        let mut removed = Vec::new();
        let mut skipped = Vec::new();
        let mut other_skips = 0;
        let mut kept = 0;
        execute_data_plan(
            plan,
            false,
            &mut removed,
            &mut skipped,
            &mut other_skips,
            &mut kept,
        );

        assert!(removed.is_empty());
        assert_eq!(other_skips, 1);
        assert!(replacement.exists());
        assert!(old.join("timetable.json").exists());
    }

    #[test]
    fn uninstall_note_distinguishes_completion_from_skips_and_keep_data() {
        assert_eq!(uninstall_note(false, 1, 0, 0), "卸载完成");
        assert!(uninstall_note(false, 1, 1, 0).contains("部分完成"));
        assert!(uninstall_note(false, 0, 1, 0).contains("未移除内容"));
        assert!(uninstall_note(false, 1, 0, 1).contains("--keep-data"));
        assert_ne!(uninstall_note(false, 1, 1, 0), "卸载完成");
    }

    #[test]
    fn update_check_reports_a_newer_release_without_side_effects() {
        let root = tempfile::tempdir().expect("create update check directory");
        let sentinel = root.path().join("sentinel.json");
        write(&sentinel, "{}");
        let before = tree_paths(root.path());
        let (result, transport) = check_release(
            root.path(),
            "owner/repo",
            Ok(response(
                302,
                &[(
                    "location",
                    "https://github.com/owner/repo/releases/tag/v0.0.1",
                )],
            )),
        );
        let outcome = result.expect("release check succeeds");

        assert_eq!(outcome.code, 0);
        assert_eq!(outcome.data["current"], json!(env!("CARGO_PKG_VERSION")));
        assert_eq!(outcome.data["repo"], json!("owner/repo"));
        assert_eq!(outcome.data["latest"], json!("0.0.1"));
        assert_eq!(outcome.data["update_available"], json!(true));
        assert!(
            outcome.data["note"]
                .as_str()
                .is_some_and(|note| note.contains("新版本"))
        );

        assert_eq!(transport.calls.len(), 1);
        assert_eq!(transport.calls[0].method, "GET");
        assert_eq!(
            transport.calls[0].url,
            "https://github.com/owner/repo/releases/latest"
        );
        assert_eq!(
            transport.calls[0].headers,
            vec![("User-Agent".to_owned(), "bupt-cli".to_owned())]
        );
        assert!(transport.calls[0].body.is_empty());
        assert_eq!(tree_paths(root.path()), before);
    }

    #[test]
    fn update_check_reports_missing_release_and_no_upgrade() {
        let root = tempfile::tempdir().expect("create no-release directory");
        let (result, transport) = check_release(root.path(), "owner/repo", Ok(response(404, &[])));
        let outcome = result.expect("a missing release is not an error");
        assert_eq!(outcome.code, 0);
        assert!(outcome.data["latest"].is_null());
        assert_eq!(outcome.data["update_available"], json!(false));
        assert!(
            outcome.data["note"]
                .as_str()
                .is_some_and(|note| note.contains("没有可用的 release"))
        );
        assert_eq!(transport.calls.len(), 1);

        let (result, transport) = check_release(
            root.path(),
            "owner/repo",
            Ok(response(
                302,
                &[(
                    "Location",
                    "https://github.com/owner/repo/releases/tag/v0.0.0",
                )],
            )),
        );
        let outcome = result.expect("an equal release is not a failure");
        assert_eq!(outcome.code, 0);
        assert_eq!(outcome.data["latest"], json!("0.0.0"));
        assert_eq!(outcome.data["update_available"], json!(false));
        assert_eq!(outcome.data["note"], json!("已经是最新版本"));
        assert_eq!(transport.calls.len(), 1);

        let (result, transport) = check_release(
            root.path(),
            "owner/repo",
            Ok(response(
                302,
                &[("Location", "https://github.com/owner/repo/releases/tag/v0")],
            )),
        );
        let outcome = result.expect("an older short tag is not a failure");
        assert_eq!(outcome.code, 0);
        assert_eq!(outcome.data["latest"], json!("0"));
        assert_eq!(outcome.data["update_available"], json!(false));
        assert_eq!(transport.calls.len(), 1);
    }

    #[test]
    fn update_check_reports_a_redirect_without_a_version() {
        let root = tempfile::tempdir().expect("create no-version directory");
        let (result, _) = check_release(
            root.path(),
            "owner/repo",
            Ok(response(
                302,
                &[(
                    "Location",
                    "https://github.com/owner/repo/releases/tag/latest",
                )],
            )),
        );
        let outcome = result.expect("a valid redirect without a tag is still a check");
        assert_eq!(outcome.code, 0);
        assert!(outcome.data["latest"].is_null());
        assert!(outcome.data["update_available"].is_null());
        assert_eq!(outcome.data["note"], json!("release 跳转里没有版本号"));
    }

    #[test]
    fn release_location_must_name_the_same_repository_tag() {
        assert_eq!(
            release_tag_from_location(
                "https://github.com/owner/repo/releases/tag/v1.2.3",
                "owner/repo"
            )
            .expect("valid release location"),
            "v1.2.3"
        );
        assert_eq!(
            release_tag_from_location(
                "https://github.com/Owner/Repo/releases/tag/v1.2.3",
                "owner/repo"
            )
            .expect("GitHub repository names are case-insensitive"),
            "v1.2.3"
        );

        for location in [
            "https://evil.example/owner/repo/releases/tag/v1.2.3",
            "http://github.com/owner/repo/releases/tag/v1.2.3",
            "https://github.com/other/repo/releases/tag/v1.2.3",
            "https://github.com/owner/other/releases/tag/v1.2.3",
            "https://github.com/owner/repo/releases/latest",
            "https://github.com/owner/repo/releases/tag/",
            "https://github.com/owner/repo/releases/tag/v1.2.3/extra",
            "https://github.com/owner/repo/releases/tag/v1.2.3?asset=1",
            "https://github.com/owner/repo/releases/tag/v1.2.3#notes",
            "https://github.com/owner/repo/releases/tag/v1%2F2",
        ] {
            let error = release_tag_from_location(location, "owner/repo")
                .expect_err("foreign or malformed release location");
            assert_eq!(error.code, 5, "location: {location}");
            assert_eq!(error.kind, "upstream", "location: {location}");
        }
    }

    #[test]
    fn update_check_rejects_a_foreign_redirect() {
        let root = tempfile::tempdir().expect("create foreign redirect directory");
        let before = tree_paths(root.path());
        let (result, transport) = check_release(
            root.path(),
            "owner/repo",
            Ok(response(
                302,
                &[(
                    "Location",
                    "https://evil.example/owner/repo/releases/tag/v1.2.3",
                )],
            )),
        );
        let error = assert_upstream(expect_error(result, "a foreign redirect must fail closed"));
        assert!(error.message.contains("invalid redirect"));
        assert_eq!(transport.calls.len(), 1);
        assert_eq!(tree_paths(root.path()), before);
    }

    #[test]
    fn release_check_layers_transport_and_http_failures() {
        let root = tempfile::tempdir().expect("create release failure directory");
        let once = |response: CliResult<Response>| {
            let (result, transport) = check_release(root.path(), "owner/repo", response);
            (expect_error(result, "release failure"), transport)
        };

        let (error, transport) = once(Err(CliError::new(
            4,
            "network",
            "synthetic transport failure",
            "",
        )));
        assert_eq!(error.code, 4);
        assert_eq!(error.kind, "network");
        assert_eq!(transport.calls.len(), 1);

        for status in [500, 429] {
            let (error, transport) = once(Ok(response(status, &[])));
            assert_upstream(error);
            assert_eq!(transport.calls.len(), 1);
        }

        let (error, transport) = once(Ok(response(403, &[])));
        assert_eq!(error.code, 3);
        assert_eq!(error.kind, "auth");
        assert_eq!(transport.calls.len(), 1);

        let (error, transport) = once(Ok(response(200, &[])));
        assert_upstream(error);
        assert_eq!(transport.calls.len(), 1);
    }

    #[test]
    fn version_ordering_does_not_promote_older_tags() {
        for (latest, current, expected) in [
            ("1.10.0", "1.9.0", true),
            ("1.9.0", "1.10.0", false),
            ("1.2.3", "1.2.3", false),
            ("1.2.2", "1.2.3", false),
            ("1.2", "1.2.0", false),
            ("1.2.3.4", "1.2.3", false),
            ("0.0.1", "0.0.0", true),
            ("0.0", "0.0.0", false),
            ("nightly", "0.0.0", false),
        ] {
            assert_eq!(
                is_newer(latest, current),
                expected,
                "latest={latest} current={current}"
            );
        }
    }

    #[test]
    fn apply_update_remains_pending_without_network_or_files() {
        let root = tempfile::tempdir().expect("create apply directory");
        let sentinel = root.path().join("sentinel.json");
        write(&sentinel, "{}");
        let before = tree_paths(root.path());
        let mut transport = Scripted::default();
        let result = {
            let mut context = test_context(root.path(), &mut transport);
            update(
                &update_matches(&["update", "--repo", "owner/repo"]),
                &mut context,
            )
        };
        let error = assert_upstream(expect_error(result, "apply must remain pending"));
        assert!(error.message.contains("not available"));
        assert!(transport.calls.is_empty());
        assert_eq!(tree_paths(root.path()), before);
    }

    #[test]
    fn dangerous_roots_are_refused_without_following_aliases() {
        let root = tempfile::tempdir().expect("create danger test directory");
        let root = root.path().canonicalize().expect("canonical temp root");
        let home = root.join("home");
        let current = home.join("project");
        let data = home.join(".bupt-cli");
        fs::create_dir_all(&data).expect("create test data directory");
        fs::create_dir_all(&current).expect("create test project directory");

        let home = home.canonicalize().expect("canonical home");
        let current = current.canonicalize().expect("canonical current");
        let data = data.canonicalize().expect("canonical data");
        let parent = root.parent().expect("temp root parent");

        assert!(is_dangerous_root(parent, &home, &current));
        assert!(is_dangerous_root(&root, &home, &current));
        assert!(is_dangerous_root(&home, &home, &current));
        assert!(!is_dangerous_root(&data, &home, &current));
    }
}
