use crate::cli::{CliError, CliResult, Context, Outcome};
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{Value, json};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const DEFAULT_REPO: &str = "tututuhehehe/bupt-cli";
const MACOS_ARM64_ASSET: &str = "bupt-macos-arm64.zip";
const WINDOWS_X64_ASSET: &str = "bupt-windows-x64.zip";

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
        "update" => update(matches),
        "uninstall" => uninstall(matches, context),
        _ => Err(CliError::usage(format!(
            "unknown lifecycle command: {name}"
        ))),
    }
}

fn update(matches: &ArgMatches) -> CliResult<Outcome> {
    let repo = selected_repo(matches)?;
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
    let action = if matches.get_flag("check") {
        "update check"
    } else {
        "update installation"
    };
    Err(CliError::new(
        5,
        "upstream",
        format!("{action} for {repo} is not available in this migration build"),
        format!(
            "Would use {asset} and target {target}; release checks, downloads and replacement are pending."
        ),
    ))
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

    if let Some(executable) = current_verified_executable() {
        plan_current_executable(&executable, dry_run, &mut removed, &mut skipped);
    }

    let mut data_dirs = Vec::new();
    for path in [&context.config_dir, &context.cache_dir] {
        if !data_dirs.contains(path) {
            data_dirs.push(path.clone());
        }
    }
    for path in data_dirs {
        match plan_data_dir(&path, keep_data, context) {
            DataPlan::Missing => {}
            DataPlan::Remove => {
                if dry_run {
                    removed.push(path.to_string_lossy().into_owned());
                } else {
                    match fs::remove_dir_all(&path) {
                        Ok(()) => removed.push(path.to_string_lossy().into_owned()),
                        Err(error) => skipped.push(with_reason(&path, &error.to_string())),
                    }
                }
            }
            DataPlan::Skip(reason) => skipped.push(with_reason(&path, &reason)),
        }
    }

    Ok(Outcome {
        data: json!({
            "dry_run": dry_run,
            "removed": removed,
            "skipped": skipped,
            "note": if dry_run { "未做任何修改" } else { "卸载完成" },
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

enum DataPlan {
    Missing,
    Remove,
    Skip(String),
}

fn plan_data_dir(path: &Path, keep_data: bool, context: &Context<'_>) -> DataPlan {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return DataPlan::Missing,
        Err(error) => return DataPlan::Skip(format!("cannot inspect: {error}")),
    };
    if metadata.file_type().is_symlink() {
        return DataPlan::Skip("symbolic link".into());
    }
    if !metadata.is_dir() {
        return DataPlan::Skip("not a directory".into());
    }
    if keep_data {
        return DataPlan::Skip("kept by --keep-data".into());
    }
    if dangerous_data_root(path, context) {
        return DataPlan::Skip("refusing dangerous data root".into());
    }
    match verified_own_dir(path) {
        Ok(true) => DataPlan::Remove,
        Ok(false) => DataPlan::Skip("unverified or mixed data directory".into()),
        Err(error) => DataPlan::Skip(format!("cannot verify: {error}")),
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

fn verified_own_dir(path: &Path) -> io::Result<bool> {
    let mut found = false;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            return Ok(false);
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            return Ok(false);
        };
        let bytes = fs::read(entry.path())?;
        if !is_own_file(&name, &bytes) {
            return Ok(false);
        }
        found = true;
    }
    Ok(found)
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
            matches!(object.get("version").and_then(Value::as_u64), Some(1 | 2))
                && object.get("courses").is_some_and(Value::is_array)
                && object.get("calendar").is_some_and(Value::is_object)
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
    use std::fs;

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
        write(&timetable, r#"{"version":1,"courses":[],"calendar":{}}"#);
        assert!(verified_own_dir(root.path()).expect("verify owned directory"));

        write(
            &timetable,
            r#"{"version":999,"term":"lookalike","other":true}"#,
        );
        assert!(!verified_own_dir(root.path()).expect("verify lookalike directory"));

        write(&root.path().join("unrelated.txt"), "not ours");
        assert!(!verified_own_dir(root.path()).expect("verify mixed directory"));
    }

    #[test]
    fn logged_out_credentials_remain_recognizable() {
        let root = tempfile::tempdir().expect("create credentials test directory");
        let credentials = root.path().join("credentials.json");
        write(
            &credentials,
            r#"{"username":"2026000001","role":"graduate"}"#,
        );
        assert!(verified_own_dir(root.path()).expect("verify logged-out credentials"));

        write(&credentials, r#"{"username":"2026000001"}"#);
        assert!(!verified_own_dir(root.path()).expect("reject bare username"));
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
