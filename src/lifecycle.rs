use crate::cli::{CliError, CliResult, Context, Outcome};
use crate::transport::Request;
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{Value, json};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

mod archive;
mod binary;

const DEFAULT_REPO: &str = "tututuhehehe/bupt-cli";
const GITHUB_RELEASE_HOST: &str = "github.com";
const MACOS_ARM64_ASSET: &str = "bupt-macos-arm64.zip";
const WINDOWS_X64_ASSET: &str = "bupt-windows-x64.zip";
const USER_AGENT: &str = "bupt-cli";
const RELEASE_BUILD_MARKER: Option<&str> = option_env!("BUPT_CLI_RELEASE_BUILD");
const MAX_ASSET_BYTES: usize = 256 * 1024 * 1024;
const MAX_TARGET_BYTES: u64 = 256 * 1024 * 1024;
const MAX_ASSET_REDIRECTS: usize = 5;
const ASSET_HOSTS: &[&str] = &[
    GITHUB_RELEASE_HOST,
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
    "github-releases.githubusercontent.com",
];

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
    update_with_target(matches, context, None, &RealFs)
}

fn update_with_target(
    matches: &ArgMatches,
    context: &mut Context<'_>,
    target_override: Option<&Path>,
    fs: &dyn UpdateFs,
) -> CliResult<Outcome> {
    let repo = selected_repo(matches)?;
    let check_only = matches.get_flag("check");
    let release = latest_release(context, &repo)?;
    let current = env!("CARGO_PKG_VERSION");
    match release {
        Release::Missing => Ok(release_outcome(
            &repo,
            current,
            None,
            Some(false),
            format!("{repo} 没有可用的 release（或仓库不存在）；用 --repo 指定仓库"),
        )),
        Release::NoVersion => Ok(release_outcome(
            &repo,
            current,
            None,
            None,
            "release 跳转里没有版本号".to_owned(),
        )),
        Release::Tag(tag) => {
            let latest = tag.trim_start_matches('v').to_owned();
            let newer = is_newer(&latest, current);
            if check_only || !newer {
                let note = if newer {
                    update_check_note(
                        std::env::consts::OS,
                        std::env::consts::ARCH,
                        running_installed_binary(),
                    )
                } else {
                    "已经是最新版本"
                };
                return Ok(release_outcome(
                    &repo,
                    current,
                    Some(latest),
                    Some(newer),
                    note.to_owned(),
                ));
            }
            install_release(context, &repo, &tag, &latest, current, target_override, fs)
        }
    }
}

#[cfg(not(windows))]
fn not_installed_outcome(repo: &str, latest: &str, current: &str) -> Outcome {
    Outcome {
        data: json!({
            "current": current,
            "repo": repo,
            "latest": latest,
            "update_available": true,
            "installed_to": null,
            "note": format!(
                "找到新版本 {latest}，但当前不是独立二进制，未自动安装；请下载 release 附件，或使用带发布标记的构建手动替换"
            ),
        }),
        code: 0,
        diagnostic: None,
    }
}

fn release_outcome(
    repo: &str,
    current: &str,
    latest: Option<String>,
    update_available: Option<bool>,
    note: String,
) -> Outcome {
    Outcome {
        data: json!({
            "current": current,
            "repo": repo,
            "latest": latest,
            "update_available": update_available,
            "note": note,
        }),
        code: 0,
        diagnostic: None,
    }
}

fn is_release_build() -> bool {
    !cfg!(test) && matches!(RELEASE_BUILD_MARKER, Some("1")) && !cfg!(debug_assertions)
}

fn running_installed_binary() -> bool {
    if !is_release_build() {
        return false;
    }
    std::env::current_exe().is_ok()
}

fn update_check_note(os: &str, arch: &str, can_install: bool) -> &'static str {
    if os == "windows" {
        "有新版本；Windows 原位替换仍待实现，未自动安装"
    } else if release_asset(os, arch).is_none() {
        "有新版本；当前平台没有可用的发行物，未自动安装"
    } else if !can_install {
        "有新版本；当前不是独立发行二进制，未自动安装"
    } else {
        "有新版本；去掉 --check 即可安装"
    }
}

#[cfg(windows)]
fn install_release(
    _context: &mut Context<'_>,
    _repo: &str,
    _tag: &str,
    latest: &str,
    _current: &str,
    _target_override: Option<&Path>,
    _fs: &dyn UpdateFs,
) -> CliResult<Outcome> {
    Err(CliError::new(
        5,
        "upstream",
        format!(
            "version {latest} is available, but Windows in-place replacement is not implemented"
        ),
        "No files were changed; download the release archive manually.",
    ))
}

#[cfg(not(windows))]
fn install_release(
    context: &mut Context<'_>,
    repo: &str,
    tag: &str,
    latest: &str,
    current: &str,
    target_override: Option<&Path>,
    fs: &dyn UpdateFs,
) -> CliResult<Outcome> {
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

    let target = match target_override {
        Some(path) => verify_install_target(fs, path, false)?,
        None => {
            if !is_release_build() {
                return Ok(not_installed_outcome(repo, latest, current));
            }
            let path = std::env::current_exe()
                .map_err(|error| fs_error("cannot resolve the running executable", error))?;
            verify_install_target(fs, &path, true)?
        }
    };
    let backup = backup_path(&target.path)?;
    ensure_backup_absent(fs, &backup)?;

    let archive = download_asset(context, repo, tag, asset)?;
    let binary = archive::extract_bupt(&archive).map_err(|error| {
        CliError::new(
            5,
            "upstream",
            format!("release archive is invalid: {error}"),
            "The downloaded release asset was rejected; no files were changed.",
        )
    })?;
    if binary::is_python_launcher(&binary) {
        return Err(CliError::new(
            5,
            "upstream",
            "release archive contains a Python launcher, not the native Rust binary",
            "The downloaded release asset was rejected; no files were changed.",
        ));
    }
    if binary.is_empty() || !binary::is_native_executable(&binary) {
        return Err(CliError::new(
            5,
            "upstream",
            "release archive does not contain a native bupt executable",
            "The downloaded release asset was rejected; no files were changed.",
        ));
    }

    let parent = target
        .path
        .parent()
        .ok_or_else(|| target_error("target has no parent directory"))?;
    let staging_path = fs
        .create_staging_dir(parent)
        .map_err(|error| fs_error("cannot create a staging directory", error))?;
    let staging = StagingDir {
        fs,
        path: staging_path,
    };
    let staged = staging.path.join(target_file_name());
    fs.write_new(&staged, &binary, 0o755)
        .map_err(|error| fs_error("cannot write the staged binary", error))?;
    let staged_bytes = fs
        .read(&staged)
        .map_err(|error| fs_error("cannot verify the staged binary", error))?;
    if staged_bytes != binary {
        return Err(CliError::new(
            5,
            "upstream",
            "the staged binary does not match the verified archive",
            "No files were changed; check free space and retry.",
        ));
    }

    verify_target_unchanged(fs, &target)?;
    fs.copy_new(&target.path, &backup)
        .map_err(|error| fs_error("cannot back up the current binary", error))?;
    let mut backup_guard = BackupGuard {
        fs,
        path: backup.clone(),
        keep: false,
    };
    let original = fs
        .read(&target.path)
        .map_err(|error| fs_error("cannot verify the current binary", error))?;
    let backed_up = fs
        .read(&backup)
        .map_err(|error| fs_error("cannot verify the backup", error))?;
    if original != backed_up {
        return Err(CliError::new(
            5,
            "upstream",
            "the backup does not match the current binary",
            "No files were replaced; check free space and retry.",
        ));
    }
    verify_target_unchanged(fs, &target)?;

    if let Err(error) = fs.rename(&staged, &target.path) {
        let cleanup = fs.remove_file(&backup);
        backup_guard.keep = cleanup.is_err();
        let message = match cleanup {
            Ok(()) => format!("cannot replace the current binary: {error}"),
            Err(cleanup) => format!(
                "cannot replace the current binary: {error}; the backup remains at {}: {cleanup}",
                backup.display()
            ),
        };
        return Err(CliError::new(
            5,
            "upstream",
            message,
            "The current binary was not replaced.",
        ));
    }

    if let Err(reason) = verify_replaced_target(fs, &target.path, &binary) {
        return match fs.rename(&backup, &target.path) {
            Ok(()) => Err(CliError::new(
                5,
                "upstream",
                format!(
                    "replacement verification failed: {reason}; the original binary was restored"
                ),
                "No update was applied.",
            )),
            Err(error) => {
                backup_guard.keep = true;
                Err(CliError::new(
                    5,
                    "upstream",
                    format!(
                        "replacement verification failed: {reason}; the backup remains at {} but restoring it failed: {error}",
                        backup.display()
                    ),
                    "Inspect the backup before running bupt again.",
                ))
            }
        };
    }

    backup_guard.keep = true;
    let cleanup_note = match fs.remove_dir_all(&staging.path) {
        Ok(()) => String::new(),
        Err(error) => format!("；暂存目录清理失败：{error}"),
    };
    Ok(Outcome {
        data: json!({
            "current": current,
            "repo": repo,
            "latest": latest,
            "update_available": true,
            "installed_to": target.path.to_string_lossy(),
            "backup": backup.to_string_lossy(),
            "note": format!("已更新到 {latest}，重新运行即可生效{cleanup_note}"),
        }),
        code: 0,
        diagnostic: None,
    })
}

fn backup_path(target: &Path) -> CliResult<PathBuf> {
    let backup = target.with_extension("old");
    if backup == target {
        return Err(target_error("target has no usable backup path"));
    }
    Ok(backup)
}

fn ensure_backup_absent(fs: &dyn UpdateFs, backup: &Path) -> CliResult<()> {
    match fs.symlink_metadata(backup) {
        Ok(_) => Err(CliError::new(
            5,
            "upstream",
            format!(
                "refusing to overwrite the existing backup at {}",
                backup.display()
            ),
            "Move or remove that backup, then rerun `bupt update`.",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(fs_error("cannot inspect the backup path", error)),
    }
}

fn target_file_name() -> &'static str {
    if cfg!(windows) { "bupt.exe" } else { "bupt" }
}

fn verify_install_target(
    fs: &dyn UpdateFs,
    path: &Path,
    require_release_build: bool,
) -> CliResult<VerifiedTarget> {
    if require_release_build && !is_release_build() {
        return Err(development_build_error());
    }
    if !path.is_absolute() {
        return Err(target_error("target path is not absolute"));
    }
    let metadata = fs
        .symlink_metadata(path)
        .map_err(|error| target_error(format!("cannot inspect the target: {error}")))?;
    if metadata.file_type().is_symlink() {
        return Err(target_error("target is a symbolic link"));
    }
    if !metadata.file_type().is_file() {
        return Err(target_error("target is not a regular file"));
    }
    if metadata.len() == 0 {
        return Err(target_error("target is empty"));
    }
    if metadata.len() > MAX_TARGET_BYTES {
        return Err(target_error("target is too large to replace safely"));
    }
    if path.file_name().and_then(|name| name.to_str()) != Some(target_file_name()) {
        return Err(target_error(
            "target does not have the expected binary name",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(target_error("target is not executable"));
        }
    }
    let contents = fs
        .read(path)
        .map_err(|error| target_error(format!("cannot read the target: {error}")))?;
    if binary::is_python_launcher(&contents) {
        return Err(target_error("target looks like a Python launcher"));
    }
    if !binary::is_native_executable(&contents) {
        return Err(target_error("target is not a native executable"));
    }
    Ok(VerifiedTarget {
        path: path.to_path_buf(),
        stamp: file_stamp(&metadata),
    })
}

fn verify_target_unchanged(fs: &dyn UpdateFs, target: &VerifiedTarget) -> CliResult<()> {
    let metadata = fs
        .symlink_metadata(&target.path)
        .map_err(|error| target_error(format!("cannot recheck the target: {error}")))?;
    if !metadata.file_type().is_file() || file_stamp(&metadata) != target.stamp {
        return Err(target_error(
            "target changed while the update was being prepared",
        ));
    }
    let contents = fs
        .read(&target.path)
        .map_err(|error| target_error(format!("cannot recheck the target: {error}")))?;
    if binary::is_python_launcher(&contents) || !binary::is_native_executable(&contents) {
        return Err(target_error(
            "target is no longer a verified native executable",
        ));
    }
    Ok(())
}

fn verify_replaced_target(fs: &dyn UpdateFs, path: &Path, expected: &[u8]) -> Result<(), String> {
    let metadata = fs
        .symlink_metadata(path)
        .map_err(|error| format!("cannot inspect the replaced binary: {error}"))?;
    if !metadata.file_type().is_file() {
        return Err("the replaced path is not a regular file".into());
    }
    if metadata.len() != expected.len() as u64 {
        return Err("the replaced binary has an unexpected size".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if metadata.permissions().mode() & 0o111 == 0 {
            return Err("the replaced binary is not executable".into());
        }
    }
    let actual = fs
        .read(path)
        .map_err(|error| format!("cannot read the replaced binary: {error}"))?;
    if actual != expected {
        return Err("the replaced binary does not match the staged bytes".into());
    }
    Ok(())
}

fn download_asset(
    context: &mut Context<'_>,
    repo: &str,
    tag: &str,
    asset: &str,
) -> CliResult<Vec<u8>> {
    let mut url = format!("https://github.com/{repo}/releases/download/{tag}/{asset}");
    for _ in 0..=MAX_ASSET_REDIRECTS {
        let response = context.transport.send(Request {
            method: "GET",
            url: url.clone(),
            headers: vec![("User-Agent".into(), USER_AGENT.into())],
            body: Vec::new(),
        })?;
        match response.status {
            200..=299 => {
                if response.body.is_empty() {
                    return Err(CliError::new(
                        5,
                        "upstream",
                        "release asset download returned an empty body",
                        "No files were changed.",
                    ));
                }
                if response.body.len() > MAX_ASSET_BYTES {
                    return Err(CliError::new(
                        5,
                        "upstream",
                        "release asset is too large",
                        "No files were changed.",
                    ));
                }
                return Ok(response.body);
            }
            301 | 302 | 303 | 307 | 308 => {
                let location = header(&response.headers, "location")
                    .ok_or_else(|| malformed_asset_redirect("redirect has no Location header"))?;
                url = asset_redirect(location, repo)?;
            }
            401 | 403 => {
                return Err(CliError::new(
                    3,
                    "auth",
                    format!(
                        "release asset download was rejected (HTTP {})",
                        response.status
                    ),
                    "GitHub refused the release asset request.",
                ));
            }
            status => return Err(asset_http_error(status)),
        }
    }
    Err(CliError::new(
        5,
        "upstream",
        "release asset download redirected too many times",
        "No files were changed.",
    ))
}

fn asset_redirect(location: &str, repo: &str) -> CliResult<String> {
    let location = location.trim();
    if location.is_empty()
        || location
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
        || location.contains('\\')
        || location.contains('#')
    {
        return Err(malformed_asset_redirect(
            "Location is empty or contains invalid characters",
        ));
    }
    let rest = location
        .strip_prefix("https://")
        .ok_or_else(|| malformed_asset_redirect("Location must use HTTPS"))?;
    let (host, path) = rest
        .split_once('/')
        .ok_or_else(|| malformed_asset_redirect("Location has no path"))?;
    if !ASSET_HOSTS.contains(&host) {
        return Err(malformed_asset_redirect(
            "Location must stay on approved GitHub asset hosts",
        ));
    }
    if host == GITHUB_RELEASE_HOST {
        let prefix = format!("{repo}/releases/download/");
        let path_without_query = path.split('?').next().unwrap_or("");
        if !path_without_query
            .get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(&prefix))
        {
            return Err(malformed_asset_redirect(
                "Location must name this repository's release download",
            ));
        }
    }
    if path.is_empty() || path.split('/').any(|segment| matches!(segment, "." | "..")) {
        return Err(malformed_asset_redirect(
            "Location has an invalid asset path",
        ));
    }
    Ok(location.to_owned())
}

fn malformed_asset_redirect(reason: &str) -> CliError {
    CliError::new(
        5,
        "upstream",
        format!("release asset download returned an invalid redirect: {reason}"),
        "No files were changed.",
    )
}

fn target_error(message: impl Into<String>) -> CliError {
    CliError::new(
        5,
        "upstream",
        message,
        "Only the verified current bupt executable can be replaced.",
    )
}

fn development_build_error() -> CliError {
    CliError::new(
        5,
        "upstream",
        "refusing to replace a development or test build",
        "Run the installed release binary, or download the release archive manually.",
    )
}

fn fs_error(operation: &str, error: io::Error) -> CliError {
    CliError::new(
        5,
        "upstream",
        format!("{operation}: {error}"),
        "No update was applied.",
    )
}

struct VerifiedTarget {
    path: PathBuf,
    stamp: FileStamp,
}

struct StagingDir<'a> {
    fs: &'a dyn UpdateFs,
    path: PathBuf,
}

impl Drop for StagingDir<'_> {
    fn drop(&mut self) {
        let _ = self.fs.remove_dir_all(&self.path);
    }
}

struct BackupGuard<'a> {
    fs: &'a dyn UpdateFs,
    path: PathBuf,
    keep: bool,
}

impl Drop for BackupGuard<'_> {
    fn drop(&mut self) {
        if !self.keep {
            let _ = self.fs.remove_file(&self.path);
        }
    }
}

trait UpdateFs {
    fn symlink_metadata(&self, path: &Path) -> io::Result<fs::Metadata>;
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    fn create_staging_dir(&self, parent: &Path) -> io::Result<PathBuf>;
    fn write_new(&self, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()>;
    fn copy_new(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn remove_file(&self, path: &Path) -> io::Result<()>;
    fn remove_dir_all(&self, path: &Path) -> io::Result<()>;
}

struct RealFs;

impl UpdateFs for RealFs {
    fn symlink_metadata(&self, path: &Path) -> io::Result<fs::Metadata> {
        fs::symlink_metadata(path)
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        fs::read(path)
    }

    fn create_staging_dir(&self, parent: &Path) -> io::Result<PathBuf> {
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..16 {
            let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(".bupt-update-{}-{sequence}", std::process::id()));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "cannot allocate a unique staging directory",
        ))
    }

    fn write_new(&self, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
        use std::io::Write;

        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        let result = file.write_all(bytes).and_then(|()| file.sync_all());
        if let Err(error) = result {
            drop(file);
            let _ = fs::remove_file(path);
            return Err(error);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = file.metadata()?.permissions();
            permissions.set_mode(mode);
            fs::set_permissions(path, permissions)?;
        }
        let _ = mode;
        Ok(())
    }

    fn copy_new(&self, from: &Path, to: &Path) -> io::Result<()> {
        let mut source = fs::File::open(from)?;
        let metadata = source.metadata()?;
        let mut destination = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(to)?;
        let result = io::copy(&mut source, &mut destination).and_then(|written| {
            if written != metadata.len() {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "backup copy was incomplete",
                ));
            }
            destination.sync_all()
        });
        if let Err(error) = result {
            drop(destination);
            let _ = fs::remove_file(to);
            return Err(error);
        }
        #[cfg(unix)]
        {
            if let Err(error) = fs::set_permissions(to, metadata.permissions()) {
                let _ = fs::remove_file(to);
                return Err(error);
            }
        }
        Ok(())
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        match fs::remove_file(path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        }
    }

    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
            Ok(metadata) if metadata.file_type().is_symlink() => fs::remove_file(path),
            Ok(metadata) if metadata.file_type().is_dir() => fs::remove_dir_all(path),
            Ok(_) => fs::remove_file(path),
        }
    }
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

fn asset_http_error(status: u16) -> CliError {
    let hint = if status == 429 {
        "GitHub is rate limiting release downloads; wait before retrying."
    } else {
        "GitHub returned an unexpected response."
    };
    CliError::new(
        5,
        "upstream",
        format!("release asset download failed with HTTP {status}"),
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

    fn expect_error<T>(result: CliResult<T>, message: &str) -> CliError {
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

    fn fixture_target(root: &Path) -> PathBuf {
        let target = root.join(binary_name());
        let current = std::env::current_exe().expect("resolve the test binary");
        fs::copy(current, &target).expect("copy the test binary into the fixture");
        make_executable(&target);
        target
    }

    fn native_payload(marker: &str) -> Vec<u8> {
        binary::test_native_payload(marker)
    }

    fn test_asset() -> &'static str {
        release_asset(std::env::consts::OS, std::env::consts::ARCH)
            .expect("the test platform has a release asset")
    }

    fn script_release(
        transport: &mut Scripted,
        repo: &str,
        tag: &str,
        asset_response: CliResult<Response>,
    ) {
        let release_url = format!("https://github.com/{repo}/releases/latest");
        let location = format!("https://github.com/{repo}/releases/tag/{tag}");
        transport.expected.push_back((
            "GET",
            release_url,
            Ok(Response {
                status: 302,
                headers: vec![("Location".into(), location)],
                body: Vec::new(),
            }),
        ));
        let asset_url = format!(
            "https://github.com/{repo}/releases/download/{tag}/{}",
            test_asset()
        );
        transport
            .expected
            .push_back(("GET", asset_url, asset_response));
    }

    fn script_apply(transport: &mut Scripted, repo: &str, tag: &str, payload: &[u8]) {
        let archive = archive::test_stored_zip("bupt", payload);
        script_release(
            transport,
            repo,
            tag,
            Ok(Response {
                status: 200,
                headers: vec![],
                body: archive,
            }),
        );
    }

    fn run_update(
        root: &Path,
        transport: &mut Scripted,
        args: &[&str],
        target: Option<&Path>,
        fs: &dyn UpdateFs,
    ) -> CliResult<Outcome> {
        let matches = update_matches(args);
        let mut context = test_context(root, transport);
        update_with_target(&matches, &mut context, target, fs)
    }

    fn no_staging_left(root: &Path) -> bool {
        !tree_paths(root).iter().any(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(".bupt-update-"))
        })
    }

    #[derive(Default)]
    struct FaultFs {
        fail_staging: bool,
        fail_write: bool,
        fail_copy: bool,
        fail_replace: bool,
        fail_restore: bool,
        corrupt_after_replace: bool,
        mutate_target_after_copy: bool,
    }

    impl FaultFs {
        fn stage_rename(&self, from: &Path, to: &Path) -> bool {
            from.parent() != to.parent()
        }
    }

    impl UpdateFs for FaultFs {
        fn symlink_metadata(&self, path: &Path) -> std::io::Result<fs::Metadata> {
            RealFs.symlink_metadata(path)
        }

        fn read(&self, path: &Path) -> std::io::Result<Vec<u8>> {
            RealFs.read(path)
        }

        fn create_staging_dir(&self, parent: &Path) -> std::io::Result<PathBuf> {
            if self.fail_staging {
                return Err(std::io::Error::other("synthetic staging failure"));
            }
            RealFs.create_staging_dir(parent)
        }

        fn write_new(&self, path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
            if self.fail_write {
                return Err(std::io::Error::other("synthetic write failure"));
            }
            RealFs.write_new(path, bytes, mode)
        }

        fn copy_new(&self, from: &Path, to: &Path) -> std::io::Result<()> {
            if self.fail_copy {
                return Err(std::io::Error::other("synthetic backup failure"));
            }
            RealFs.copy_new(from, to)?;
            if self.mutate_target_after_copy {
                let mut contents = fs::read(from)?;
                contents.push(0);
                fs::write(from, contents)?;
            }
            Ok(())
        }

        fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
            let stage_rename = self.stage_rename(from, to);
            if self.fail_replace && stage_rename {
                return Err(std::io::Error::other("synthetic replace failure"));
            }
            if self.fail_restore && !stage_rename {
                return Err(std::io::Error::other("synthetic restore failure"));
            }
            let result = RealFs.rename(from, to);
            if result.is_ok() && self.corrupt_after_replace && stage_rename {
                fs::write(to, b"corrupt").expect("corrupt the replaced target");
            }
            result
        }

        fn remove_file(&self, path: &Path) -> std::io::Result<()> {
            RealFs.remove_file(path)
        }

        fn remove_dir_all(&self, path: &Path) -> std::io::Result<()> {
            RealFs.remove_dir_all(path)
        }
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
                    "https://github.com/owner/repo/releases/tag/v99.0.0",
                )],
            )),
        );
        let outcome = result.expect("release check succeeds");

        assert_eq!(outcome.code, 0);
        assert_eq!(outcome.data["current"], json!(env!("CARGO_PKG_VERSION")));
        assert_eq!(outcome.data["repo"], json!("owner/repo"));
        assert_eq!(outcome.data["latest"], json!("99.0.0"));
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
            ("99.0.0", "0.0.0", true),
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
    fn update_apply_replaces_a_verified_fixture_target() {
        let root = tempfile::tempdir().expect("create apply test directory");
        let target = fixture_target(root.path());
        let original = fs::read(&target).expect("read fixture target");
        let payload = native_payload("NEW-BINARY");
        let backup = target.with_extension("old");
        let target_text = target.to_string_lossy().into_owned();
        let backup_text = backup.to_string_lossy().into_owned();

        let mut transport = Scripted::default();
        script_apply(&mut transport, "owner/repo", "v99.0.0", &payload);
        let outcome = run_update(
            root.path(),
            &mut transport,
            &["update", "--repo", "owner/repo"],
            Some(&target),
            &RealFs,
        )
        .expect("apply the fixture update");

        assert_eq!(outcome.code, 0);
        assert_eq!(outcome.data["latest"], json!("99.0.0"));
        assert_eq!(outcome.data["update_available"], json!(true));
        assert_eq!(outcome.data["installed_to"], json!(target_text));
        assert_eq!(outcome.data["backup"], json!(backup_text));
        assert!(
            outcome.data["note"]
                .as_str()
                .is_some_and(|note| note.contains("已更新到"))
        );
        assert_eq!(fs::read(&target).expect("read replaced target"), payload);
        assert_eq!(fs::read(&backup).expect("read backup"), original);
        assert!(no_staging_left(root.path()));
        assert_eq!(transport.calls.len(), 2);
        assert_eq!(transport.calls[0].method, "GET");
        assert_eq!(
            transport.calls[0].url,
            "https://github.com/owner/repo/releases/latest"
        );
        assert!(
            transport.calls[1]
                .url
                .ends_with(&format!("/releases/download/v99.0.0/{}", test_asset()))
        );
    }

    #[test]
    fn update_apply_rejects_a_symlinked_target() {
        let root = tempfile::tempdir().expect("create symlink apply directory");
        let real = root.path().join("real-bupt");
        write(&real, "not a native binary");
        let target = root.path().join(binary_name());
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &target).expect("create target symlink");
        #[cfg(not(unix))]
        fs::copy(&real, &target).expect("copy target on non-Unix");

        let mut transport = Scripted::default();
        script_apply(
            &mut transport,
            "owner/repo",
            "v99.0.0",
            &native_payload("new"),
        );
        let error = assert_upstream(expect_error(
            run_update(
                root.path(),
                &mut transport,
                &["update", "--repo", "owner/repo"],
                Some(&target),
                &RealFs,
            ),
            "a symlinked target must be refused",
        ));
        assert_eq!(error.code, 5);
        assert_eq!(transport.calls.len(), 1);
        assert!(!target.with_extension("old").exists());
        assert!(no_staging_left(root.path()));
    }

    #[test]
    fn update_apply_rejects_scripts_wrong_names_and_non_executables() {
        for case in [
            "script",
            "python-launcher",
            "wrong-arch",
            "truncated",
            "wrong-name",
            "non-executable",
        ] {
            let root = tempfile::tempdir().expect("create invalid target directory");
            let target = match case {
                "script" => {
                    let target = root.path().join(binary_name());
                    write(&target, "#!/bin/sh\nexit 0\n");
                    make_executable(&target);
                    target
                }
                "python-launcher" => {
                    let target = root.path().join(binary_name());
                    let mut launcher = native_payload("old-python");
                    launcher.extend_from_slice(b"MEI\x0c\x0b\x0a\x0b\x0e");
                    fs::write(&target, &launcher).expect("write Python launcher fixture");
                    make_executable(&target);
                    target
                }
                "wrong-arch" => {
                    let target = root.path().join(binary_name());
                    fs::write(&target, binary::test_wrong_arch_payload("old"))
                        .expect("write wrong-architecture fixture");
                    make_executable(&target);
                    target
                }
                "truncated" => {
                    let target = root.path().join(binary_name());
                    fs::write(&target, binary::test_truncated_payload("old"))
                        .expect("write truncated fixture");
                    make_executable(&target);
                    target
                }
                "wrong-name" => {
                    let target = root.path().join("other");
                    fs::copy(fixture_target(root.path()), &target).expect("copy wrong-name target");
                    make_executable(&target);
                    target
                }
                _ => {
                    let target = fixture_target(root.path());
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;

                        let mut permissions = fs::metadata(&target)
                            .expect("read target permissions")
                            .permissions();
                        permissions.set_mode(0o644);
                        fs::set_permissions(&target, permissions)
                            .expect("remove target execute bit");
                    }
                    target
                }
            };
            let original = fs::read(&target).expect("read invalid target");
            let mut transport = Scripted::default();
            script_apply(
                &mut transport,
                "owner/repo",
                "v99.0.0",
                &native_payload("new"),
            );
            let error = assert_upstream(expect_error(
                run_update(
                    root.path(),
                    &mut transport,
                    &["update", "--repo", "owner/repo"],
                    Some(&target),
                    &RealFs,
                ),
                "an unverified target must be refused",
            ));
            assert_eq!(error.code, 5);
            assert_eq!(transport.calls.len(), 1, "case {case}");
            assert_eq!(
                fs::read(&target).expect("read unchanged target"),
                original,
                "case {case}"
            );
            #[cfg(unix)]
            assert!(!target.with_extension("old").exists(), "case {case}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn update_apply_refuses_a_development_build_even_with_a_target() {
        let root = tempfile::tempdir().expect("create development target directory");
        let target = fixture_target(root.path());
        let error = expect_error(
            verify_install_target(&RealFs, &target, true),
            "a debug build must not be a release target",
        );
        assert_eq!(error.code, 5);
        assert!(error.message.contains("development"));
    }

    #[test]
    fn update_apply_requires_the_backup_path_to_be_free() {
        let root = tempfile::tempdir().expect("create backup collision directory");
        let target = fixture_target(root.path());
        let backup = target.with_extension("old");
        write(&backup, "existing backup");
        let original = fs::read(&target).expect("read fixture target");

        let mut transport = Scripted::default();
        script_apply(
            &mut transport,
            "owner/repo",
            "v99.0.0",
            &native_payload("new"),
        );
        let error = assert_upstream(expect_error(
            run_update(
                root.path(),
                &mut transport,
                &["update", "--repo", "owner/repo"],
                Some(&target),
                &RealFs,
            ),
            "an existing backup must be preserved",
        ));
        assert!(error.message.contains("backup"));
        assert_eq!(transport.calls.len(), 1);
        assert_eq!(
            fs::read(&backup).expect("read existing backup"),
            b"existing backup"
        );
        assert_eq!(fs::read(&target).expect("read unchanged target"), original);
    }

    #[test]
    fn update_apply_download_and_archive_failures_leave_the_target_untouched() {
        let mut python = native_payload("old-python");
        python.extend_from_slice(b"MEI\x0c\x0b\x0a\x0b\x0e");
        let python_archive = archive::test_stored_zip("bupt", &python);
        let cases = [
            (
                Err(CliError::new(
                    4,
                    "network",
                    "synthetic transport failure",
                    "",
                )),
                4,
            ),
            (Ok(response(500, &[])), 5),
            (
                Ok(Response {
                    status: 200,
                    headers: vec![],
                    body: b"not a zip archive".to_vec(),
                }),
                5,
            ),
            (
                Ok(Response {
                    status: 200,
                    headers: vec![],
                    body: archive::test_stored_zip("README", b"not the binary"),
                }),
                5,
            ),
            (
                Ok(Response {
                    status: 200,
                    headers: vec![],
                    body: python_archive,
                }),
                5,
            ),
            (
                Ok(Response {
                    status: 200,
                    headers: vec![],
                    body: archive::test_stored_zip("bupt", &binary::test_wrong_arch_payload("new")),
                }),
                5,
            ),
            (
                Ok(Response {
                    status: 200,
                    headers: vec![],
                    body: archive::test_stored_zip("bupt", &binary::test_truncated_payload("new")),
                }),
                5,
            ),
            (
                Ok(Response {
                    status: 302,
                    headers: vec![("Location".into(), "https://evil.example/release.zip".into())],
                    body: vec![],
                }),
                5,
            ),
        ];

        for (asset_response, expected_code) in cases {
            let root = tempfile::tempdir().expect("create failed apply directory");
            let target = fixture_target(root.path());
            let original = fs::read(&target).expect("read fixture target");
            let mut transport = Scripted::default();
            script_release(&mut transport, "owner/repo", "v99.0.0", asset_response);
            let error = expect_error(
                run_update(
                    root.path(),
                    &mut transport,
                    &["update", "--repo", "owner/repo"],
                    Some(&target),
                    &RealFs,
                ),
                "a failed download must not replace the target",
            );
            assert_eq!(error.code, expected_code);
            assert_eq!(fs::read(&target).expect("read unchanged target"), original);
            assert!(!target.with_extension("old").exists());
            assert!(no_staging_left(root.path()));
        }
    }

    #[test]
    fn update_apply_stage_write_and_backup_failures_preserve_the_target() {
        for fs in [
            FaultFs {
                fail_staging: true,
                ..FaultFs::default()
            },
            FaultFs {
                fail_write: true,
                ..FaultFs::default()
            },
            FaultFs {
                fail_copy: true,
                ..FaultFs::default()
            },
        ] {
            let root = tempfile::tempdir().expect("create failed stage directory");
            let target = fixture_target(root.path());
            let original = fs::read(&target).expect("read fixture target");
            let mut transport = Scripted::default();
            script_apply(
                &mut transport,
                "owner/repo",
                "v99.0.0",
                &native_payload("new"),
            );
            let error = assert_upstream(expect_error(
                run_update(
                    root.path(),
                    &mut transport,
                    &["update", "--repo", "owner/repo"],
                    Some(&target),
                    &fs,
                ),
                "a filesystem failure must not replace the target",
            ));
            assert_eq!(error.code, 5);
            assert_eq!(fs::read(&target).expect("read unchanged target"), original);
            assert!(!target.with_extension("old").exists());
            assert!(no_staging_left(root.path()));
        }
    }

    #[test]
    fn update_apply_failure_after_backup_cleans_the_backup() {
        let root = tempfile::tempdir().expect("create post-backup failure directory");
        let target = fixture_target(root.path());
        let mut transport = Scripted::default();
        script_apply(
            &mut transport,
            "owner/repo",
            "v99.0.0",
            &native_payload("new"),
        );
        let fs = FaultFs {
            mutate_target_after_copy: true,
            ..FaultFs::default()
        };
        let error = assert_upstream(expect_error(
            run_update(
                root.path(),
                &mut transport,
                &["update", "--repo", "owner/repo"],
                Some(&target),
                &fs,
            ),
            "a target changed after backup must not be replaced",
        ));
        assert_eq!(error.code, 5);
        assert!(!target.with_extension("old").exists());
        assert!(no_staging_left(root.path()));
    }

    #[test]
    fn update_apply_replace_failure_cleans_up_the_backup() {
        let root = tempfile::tempdir().expect("create replace failure directory");
        let target = fixture_target(root.path());
        let original = fs::read(&target).expect("read fixture target");
        let mut transport = Scripted::default();
        script_apply(
            &mut transport,
            "owner/repo",
            "v99.0.0",
            &native_payload("new"),
        );
        let fs = FaultFs {
            fail_replace: true,
            ..FaultFs::default()
        };
        let error = assert_upstream(expect_error(
            run_update(
                root.path(),
                &mut transport,
                &["update", "--repo", "owner/repo"],
                Some(&target),
                &fs,
            ),
            "a failed replace must leave the target untouched",
        ));
        assert!(error.message.contains("cannot replace"));
        assert_eq!(fs::read(&target).expect("read unchanged target"), original);
        assert!(!target.with_extension("old").exists());
        assert!(no_staging_left(root.path()));
    }

    #[test]
    fn update_apply_restore_failure_keeps_the_backup_and_reports_it() {
        let root = tempfile::tempdir().expect("create restore failure directory");
        let target = fixture_target(root.path());
        let original = fs::read(&target).expect("read fixture target");
        let backup = target.with_extension("old");
        let mut transport = Scripted::default();
        script_apply(
            &mut transport,
            "owner/repo",
            "v99.0.0",
            &native_payload("new"),
        );
        let fs = FaultFs {
            corrupt_after_replace: true,
            fail_restore: true,
            ..FaultFs::default()
        };
        let error = assert_upstream(expect_error(
            run_update(
                root.path(),
                &mut transport,
                &["update", "--repo", "owner/repo"],
                Some(&target),
                &fs,
            ),
            "a failed restore must keep the backup",
        ));
        assert!(error.message.contains("backup remains"));
        assert_eq!(fs::read(&backup).expect("read retained backup"), original);
        assert_eq!(
            fs::read(&target).expect("read corrupted target"),
            b"corrupt"
        );
        assert!(no_staging_left(root.path()));
    }

    #[test]
    fn update_apply_corruption_rolls_back_from_the_backup() {
        let root = tempfile::tempdir().expect("create rollback directory");
        let target = fixture_target(root.path());
        let original = fs::read(&target).expect("read fixture target");
        let mut transport = Scripted::default();
        script_apply(
            &mut transport,
            "owner/repo",
            "v99.0.0",
            &native_payload("new"),
        );
        let fs = FaultFs {
            corrupt_after_replace: true,
            ..FaultFs::default()
        };
        let error = assert_upstream(expect_error(
            run_update(
                root.path(),
                &mut transport,
                &["update", "--repo", "owner/repo"],
                Some(&target),
                &fs,
            ),
            "corrupt replacement must be rolled back",
        ));
        assert!(error.message.contains("restored"));
        assert_eq!(fs::read(&target).expect("read restored target"), original);
        assert!(!target.with_extension("old").exists());
        assert!(no_staging_left(root.path()));
    }

    #[test]
    fn update_apply_source_build_reports_not_installed_without_touching_it() {
        let root = tempfile::tempdir().expect("create development apply directory");
        let current = std::env::current_exe().expect("resolve the test binary");
        let before = fs::metadata(&current).expect("read test binary metadata");
        let mut transport = Scripted::default();
        script_apply(
            &mut transport,
            "owner/repo",
            "v99.0.0",
            &native_payload("new"),
        );
        let outcome = run_update(
            root.path(),
            &mut transport,
            &["update", "--repo", "owner/repo"],
            None,
            &RealFs,
        )
        .expect("a source build must report that it did not install");
        assert_eq!(outcome.code, 0);
        assert_eq!(outcome.data["latest"], json!("99.0.0"));
        assert_eq!(outcome.data["update_available"], json!(true));
        assert_eq!(outcome.data["installed_to"], json!(null));
        assert!(
            outcome.data["note"]
                .as_str()
                .is_some_and(|note| note.contains("未自动安装"))
        );
        let after = fs::metadata(&current).expect("reread test binary metadata");
        assert_eq!(after.len(), before.len());
        assert_eq!(transport.calls.len(), 1);
        assert!(tree_paths(root.path()).is_empty());
    }

    #[test]
    fn update_notes_do_not_promise_unavailable_installs() {
        assert!(update_check_note("macos", "aarch64", true).contains("去掉 --check"));
        let source = update_check_note("macos", "aarch64", false);
        assert!(source.contains("未自动安装"));
        assert!(!source.contains("去掉 --check"));
        let windows = update_check_note("windows", "x86_64", true);
        assert!(windows.contains("Windows"));
        assert!(!windows.contains("去掉 --check"));
        let other = update_check_note("linux", "x86_64", true);
        assert!(other.contains("没有可用的发行物"));
        assert!(!other.contains("去掉 --check"));
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
