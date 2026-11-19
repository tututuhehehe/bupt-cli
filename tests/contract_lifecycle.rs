mod contract_support;

use contract_support::{Invocation, Sandbox};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const SCHEMA: &str = "bupt.v1";

const PUBLIC_COMMANDS: &[&str] = &[
    "login",
    "status",
    "logout",
    "courses",
    "homework",
    "schedule",
    "exams",
    "scores",
    "classrooms",
    "library",
    "electricity",
    "notices",
    "card",
    "diagnose",
    "update",
    "uninstall",
];

const NESTED_COMMANDS: &[(&str, &[&str])] = &[
    ("homework", &["show", "download", "submit", "verify"]),
    ("schedule", &["update", "calendar", "export"]),
    ("library", &["search", "hold", "ebook"]),
    ("electricity", &["buildings"]),
    ("notices", &["show"]),
];

const CREDENTIALS: &str =
    include_str!("fixtures/contracts/lifecycle/data/credentials.fixture.json");
const SESSION: &str = include_str!("fixtures/contracts/lifecycle/data/session.fixture.json");
const CONFIG: &str = include_str!("fixtures/contracts/lifecycle/data/config.fixture.json");
const TIMETABLE: &str = include_str!("fixtures/contracts/lifecycle/data/timetable.fixture.json");
const UNRELATED: &str = include_str!("fixtures/contracts/lifecycle/data/unrelated.txt");

fn stdout_text(inv: &Invocation) -> String {
    String::from_utf8(inv.0.stdout.clone()).expect("CLI stdout is UTF-8")
}

fn stderr_text(inv: &Invocation) -> String {
    String::from_utf8(inv.0.stderr.clone()).expect("CLI stderr is UTF-8")
}

fn assert_code(inv: &Invocation, expected: i32) {
    assert_eq!(
        inv.code(),
        expected,
        "unexpected exit code; stdout={:?}; stderr={:?}",
        stdout_text(inv),
        stderr_text(inv)
    );
}

fn assert_text_output(inv: &Invocation) -> String {
    assert_code(inv, 0);
    assert!(
        inv.0.stderr.is_empty(),
        "help/version must not write to stderr: {:?}",
        stderr_text(inv)
    );
    let text = stdout_text(inv);
    let trimmed = text.trim();
    assert!(!trimmed.is_empty(), "expected non-empty text output");
    assert!(
        serde_json::from_str::<Value>(trimmed).is_err(),
        "help/version must remain text, got JSON: {text:?}"
    );
    text
}

fn assert_success_json(inv: &Invocation) -> Value {
    assert_code(inv, 0);
    assert!(
        inv.0.stderr.is_empty(),
        "successful command wrote to stderr: {:?}",
        stderr_text(inv)
    );
    let payload = inv.stdout_json();
    assert_eq!(
        payload["schema"], SCHEMA,
        "success JSON lacks the stable schema: {payload}"
    );
    assert_eq!(payload["ok"], true, "success JSON lacks ok=true: {payload}");
    payload
}

fn assert_error_json(inv: &Invocation, expected_code: i32, expected_type: &str) -> Value {
    assert_code(inv, expected_code);
    assert!(
        inv.0.stdout.is_empty(),
        "error must not write to stdout: {:?}",
        stdout_text(inv)
    );
    let payload = inv.stderr_json();
    assert_eq!(
        payload["schema"], SCHEMA,
        "error JSON lacks the stable schema: {payload}"
    );
    assert_eq!(payload["ok"], false, "error JSON lacks ok=false: {payload}");
    assert_eq!(
        payload["error"]["type"], expected_type,
        "unexpected error type: {payload}"
    );
    assert!(
        payload["error"]["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty()),
        "error JSON lacks a non-empty message: {payload}"
    );
    payload
}

fn canonical_join(path: &Path) -> PathBuf {
    let is_symlink = fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false);
    if !is_symlink {
        if let Ok(canonical) = path.canonicalize() {
            return canonical;
        }
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => parent
            .canonicalize()
            .map(|parent| parent.join(name))
            .unwrap_or_else(|_| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}

fn path_forms(path: &Path) -> Vec<String> {
    let mut forms = vec![path.to_string_lossy().into_owned()];
    let canonical = canonical_join(path).to_string_lossy().into_owned();
    if !forms.contains(&canonical) {
        forms.push(canonical);
    }
    forms
}

fn removed_listed(removed: &Value, path: &Path) -> bool {
    let Some(entries) = removed.as_array() else {
        return false;
    };
    let forms = path_forms(path);
    entries.iter().filter_map(Value::as_str).any(|entry| {
        let entry = entry.trim().trim_end_matches(['/', '\\']);
        forms.iter().any(|form| {
            entry == form
                || entry.starts_with(&format!("{form}（"))
                || entry.starts_with(&format!("{form} ("))
        })
    })
}

fn removed_covers(removed: &Value, path: &Path) -> bool {
    removed_listed(removed, path)
        || path
            .parent()
            .is_some_and(|parent| removed_listed(removed, parent))
}

fn array_mentions(array: &Value, path: &Path) -> bool {
    let forms = path_forms(path);
    array.as_array().is_some_and(|entries| {
        entries
            .iter()
            .filter_map(Value::as_str)
            .any(|entry| forms.iter().any(|form| entry.contains(form)))
    })
}

#[derive(Debug, PartialEq, Eq)]
enum TreeEntry {
    File(Vec<u8>),
    Symlink(PathBuf),
}

fn tree(root: &Path) -> BTreeMap<PathBuf, TreeEntry> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, TreeEntry>) {
        let entries = fs::read_dir(dir)
            .unwrap_or_else(|err| panic!("read directory {}: {err}", dir.display()));
        for entry in entries {
            let entry =
                entry.unwrap_or_else(|err| panic!("read an entry in {}: {err}", dir.display()));
            let path = entry.path();
            let relative = path.strip_prefix(base).unwrap_or(&path).to_path_buf();
            let metadata = fs::symlink_metadata(&path)
                .unwrap_or_else(|err| panic!("lstat {}: {err}", path.display()));
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                let target = fs::read_link(&path)
                    .unwrap_or_else(|err| panic!("read symlink {}: {err}", path.display()));
                out.insert(relative, TreeEntry::Symlink(target));
            } else if file_type.is_dir() {
                walk(base, &path, out);
            } else if file_type.is_file() {
                let bytes = fs::read(&path)
                    .unwrap_or_else(|err| panic!("read file {}: {err}", path.display()));
                out.insert(relative, TreeEntry::File(bytes));
            } else {
                panic!("unsupported fixture entry type at {}", path.display());
            }
        }
    }

    let mut result = BTreeMap::new();
    walk(root, root, &mut result);
    result
}

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create fixture parent directory");
    }
    fs::write(path, contents).expect("write fixture file");
}

fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(path)
            .expect("read fixture executable metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).expect("mark fixture executable");
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

fn platform_binary_name() -> &'static str {
    if cfg!(windows) { "bupt.exe" } else { "bupt" }
}

/// Run a copy of the CLI from a throwaway directory with the sandbox
/// environment.
///
/// Uninstall may consider the executable it is running from. Pointing that at a
/// temporary copy keeps the cargo test binary (and the rest of `target/`)
/// outside the candidate set for dry runs and real deletions alike.
fn run_isolated_copy(sandbox: &Sandbox, args: &[&str]) -> Invocation {
    run_isolated_copy_with(sandbox, args, |_| {})
}

fn run_isolated_copy_with<F>(sandbox: &Sandbox, args: &[&str], setup: F) -> Invocation
where
    F: FnOnce(&mut Command),
{
    let name = platform_binary_name();
    let binary_dir = tempfile::tempdir().expect("create isolated binary directory");
    let binary = binary_dir.path().join(name);
    fs::copy(env!("CARGO_BIN_EXE_bupt"), &binary).expect("copy CLI into the sandbox");
    make_executable(&binary);

    let template = sandbox.command();
    let mut command = Command::new(binary);
    command.env_clear();
    if let Some(directory) = template.get_current_dir() {
        command.current_dir(directory);
    }
    for (key, value) in template.get_envs() {
        match value {
            Some(value) => {
                command.env(key, value);
            }
            None => {
                command.env_remove(key);
            }
        }
    }
    setup(&mut command);

    Invocation(
        command
            .args(args)
            .output()
            .expect("run copied isolated CLI"),
    )
}

fn install_owned_data(sandbox: &Sandbox) -> (PathBuf, PathBuf) {
    let config = sandbox.root().join("config");
    let cache = sandbox.root().join("cache");
    write_file(&config.join("credentials.json"), CREDENTIALS);
    write_file(&config.join("config.json"), CONFIG);
    write_file(&cache.join("session.json"), SESSION);
    write_file(&cache.join("timetable.json"), TIMETABLE);
    (config, cache)
}

fn has_command_entry(text: &str, command: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim_start();
        line == command
            || line.starts_with(&format!("{command} "))
            || line.starts_with(&format!("{command}\t"))
    })
}

#[test]
fn help_overview_is_text_and_lists_the_public_tree() {
    let sandbox = Sandbox::new();

    let help = assert_text_output(&sandbox.run(&["help"]));
    for command in PUBLIC_COMMANDS {
        assert!(
            help.contains(command),
            "`bupt help` does not mention the public command {command:?}:\n{help}"
        );
    }
    assert!(
        help.contains("help"),
        "`bupt help` does not mention itself:\n{help}"
    );

    // Old top-level names must not silently remain as hidden commands.
    assert!(
        !has_command_entry(&help, "probe"),
        "`bupt help` still advertises the removed `probe` command:\n{help}"
    );
    assert!(
        !help.contains("homework list") && !help.contains("schedule exams"),
        "`bupt help` still advertises removed nested aliases:\n{help}"
    );

    let long_help = assert_text_output(&sandbox.run(&["--help"]));
    for command in PUBLIC_COMMANDS {
        assert!(
            long_help.contains(command),
            "`bupt --help` does not mention the public command {command:?}:\n{long_help}"
        );
    }
}

#[test]
fn help_topics_cover_every_public_command() {
    let sandbox = Sandbox::new();

    for command in PUBLIC_COMMANDS {
        let help = assert_text_output(&sandbox.run(&["help", command]));
        assert!(
            help.to_ascii_lowercase().contains(command),
            "`bupt help {command}` does not describe that command:\n{help}"
        );
    }

    let help_help = assert_text_output(&sandbox.run(&["help", "help"]));
    assert!(
        help_help.to_ascii_lowercase().contains("help"),
        "`bupt help help` does not describe the help command:\n{help_help}"
    );
}

#[test]
fn help_topics_list_the_canonical_nested_commands() {
    let sandbox = Sandbox::new();

    for (parent, children) in NESTED_COMMANDS {
        let help = assert_text_output(&sandbox.run(&["help", parent]));
        for child in *children {
            assert!(
                help.contains(child),
                "`bupt help {parent}` does not list the canonical `{parent} {child}` entry:\n{help}"
            );
        }
    }
}

#[test]
fn unknown_help_topics_are_usage_errors_on_stderr() {
    let sandbox = Sandbox::new();

    let payload = assert_error_json(&sandbox.run(&["help", "not-a-command"]), 2, "usage");
    assert!(
        payload["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("not-a-command")),
        "usage error should name the unknown help topic: {payload}"
    );
}

#[test]
fn removed_command_names_are_not_aliases() {
    let sandbox = Sandbox::new();
    let cases: &[&[&str]] = &[
        &["homework", "list"],
        &["homework", "list", "--undone"],
        &["homework", "list", "--help"],
        &["schedule", "exams"],
        &["schedule", "exams", "--help"],
        &["probe"],
        &["probe", "--help"],
        &["help", "homework", "list"],
        &["help", "schedule", "exams"],
        &["help", "probe"],
    ];

    for args in cases {
        let inv = sandbox.run(args);
        assert_error_json(&inv, 2, "usage");
        assert_eq!(
            stderr_text(&inv).lines().count(),
            1,
            "usage errors must be one compact JSON document: {:?}",
            stderr_text(&inv)
        );
    }

    assert!(
        tree(sandbox.root()).is_empty(),
        "removed command names must not create local state: {:?}",
        tree(sandbox.root())
    );
}

#[test]
fn version_is_text_and_does_not_touch_state() {
    let sandbox = Sandbox::new();

    let version = assert_text_output(&sandbox.run(&["--version"]));
    let mut words = version.split_whitespace();
    assert_eq!(words.next(), Some("bupt"), "version output: {version:?}");
    let number = words.next().unwrap_or_default();
    assert!(
        number.contains('.') && number.chars().any(|character| character.is_ascii_digit()),
        "version output does not contain a version number: {version:?}"
    );
    assert!(
        tree(sandbox.root()).is_empty(),
        "`--version` must not create local state: {:?}",
        tree(sandbox.root())
    );
}

#[test]
fn missing_command_is_a_usage_error() {
    let sandbox = Sandbox::new();

    let inv = sandbox.run(&[]);
    assert_error_json(&inv, 2, "usage");
    assert_eq!(
        stderr_text(&inv).lines().count(),
        1,
        "error envelopes must be one compact JSON document: {:?}",
        stderr_text(&inv)
    );
    assert!(
        tree(sandbox.root()).is_empty(),
        "a missing command must not create local state: {:?}",
        tree(sandbox.root())
    );
}

#[test]
fn status_is_one_compact_json_document_with_schema() {
    let sandbox = Sandbox::new();

    let inv = sandbox.run(&["status"]);
    let payload = assert_success_json(&inv);
    assert_eq!(payload["exists"], false, "fresh status payload: {payload}");

    let reported_path = Path::new(
        payload["path"]
            .as_str()
            .expect("status must report the local session path"),
    );
    assert_eq!(
        canonical_join(reported_path),
        canonical_join(&sandbox.root().join("cache").join("session.json")),
        "status must read the isolated cache directory: {payload}"
    );
    assert_eq!(
        stdout_text(&inv).lines().count(),
        1,
        "default output must be one compact JSON line: {:?}",
        stdout_text(&inv)
    );
    assert!(
        tree(sandbox.root()).is_empty(),
        "read-only status must not create state files: {:?}",
        tree(sandbox.root())
    );
}

#[test]
fn pretty_changes_layout_without_changing_values() {
    let sandbox = Sandbox::new();

    let compact = sandbox.run(&["status"]);
    let pretty_after = sandbox.run(&["status", "--pretty"]);
    let pretty_before = sandbox.run(&["--pretty", "status"]);

    let compact_payload = assert_success_json(&compact);
    let after_payload = assert_success_json(&pretty_after);
    let before_payload = assert_success_json(&pretty_before);

    assert_eq!(compact_payload, after_payload);
    assert_eq!(compact_payload, before_payload);
    assert_eq!(pretty_after.0.stdout, pretty_before.0.stdout);

    assert_eq!(
        stdout_text(&compact).lines().count(),
        1,
        "default output must stay compact: {:?}",
        stdout_text(&compact)
    );
    assert!(
        stdout_text(&pretty_after).lines().count() > 1,
        "`--pretty` must actually change the layout: {:?}",
        stdout_text(&pretty_after)
    );
}

#[test]
fn pretty_changes_error_layout_without_changing_values() {
    let sandbox = Sandbox::new();

    let compact = sandbox.run(&["help", "not-a-command"]);
    let pretty = sandbox.run(&["help", "not-a-command", "--pretty"]);

    let compact_payload = assert_error_json(&compact, 2, "usage");
    let pretty_payload = assert_error_json(&pretty, 2, "usage");
    assert_eq!(compact_payload, pretty_payload);
    assert_eq!(
        stderr_text(&compact).lines().count(),
        1,
        "default error envelope must stay compact: {:?}",
        stderr_text(&compact)
    );
    assert!(
        stderr_text(&pretty).lines().count() > 1,
        "`--pretty` must actually change the error layout: {:?}",
        stderr_text(&pretty)
    );
}

#[test]
fn invalid_arguments_are_usage_errors_before_local_state_changes() {
    let sandbox = Sandbox::new();
    let (_, _) = install_owned_data(&sandbox);
    let before = tree(sandbox.root());
    let cases: &[&[&str]] = &[
        &["courses", "--definitely-not-an-option"],
        &["--json", "status"],
        &["status", "--json"],
        &["--timeout", "0", "status"],
        &["--timeout", "not-a-number", "status"],
    ];

    for args in cases {
        let inv = sandbox.run(args);
        assert_error_json(&inv, 2, "usage");
        assert_eq!(
            stderr_text(&inv).lines().count(),
            1,
            "usage errors must be one compact JSON document: {:?}",
            stderr_text(&inv)
        );
        assert_eq!(
            tree(sandbox.root()),
            before,
            "invalid arguments {args:?} must not read or write business state"
        );
    }
}

#[test]
fn homework_uses_the_canonical_default_entry() {
    let sandbox = Sandbox::new();

    let inv = sandbox.run(&["homework"]);
    let payload = assert_error_json(&inv, 3, "auth");
    assert!(
        payload["error"]["message"]
            .as_str()
            .is_some_and(|message| !message.contains("synthetic-password-for-offline-tests")),
        "auth errors must not echo secrets: {payload}"
    );
    assert_eq!(
        stderr_text(&inv).lines().count(),
        1,
        "auth errors must be one compact JSON document: {:?}",
        stderr_text(&inv)
    );
    assert!(
        tree(sandbox.root()).is_empty(),
        "missing credentials must be rejected before local writes: {:?}",
        tree(sandbox.root())
    );
}

#[test]
fn uninstall_dry_run_lists_owned_data_without_changing_anything() {
    let sandbox = Sandbox::new();
    let (config, cache) = install_owned_data(&sandbox);
    let before = tree(sandbox.root());

    let inv = run_isolated_copy(&sandbox, &["uninstall", "--dry-run"]);
    let payload = assert_success_json(&inv);
    assert_eq!(payload["dry_run"], true, "dry-run payload: {payload}");
    let removed = &payload["removed"];
    assert!(
        removed_covers(removed, &config.join("credentials.json")),
        "dry-run must report the owned credentials: {payload}"
    );
    assert!(
        removed_covers(removed, &config.join("config.json")),
        "dry-run must report the owned config: {payload}"
    );
    assert!(
        removed_covers(removed, &cache.join("session.json")),
        "dry-run must report the owned session: {payload}"
    );
    assert!(
        removed_covers(removed, &cache.join("timetable.json")),
        "dry-run must report the owned timetable: {payload}"
    );
    assert!(
        !removed_listed(removed, sandbox.root()),
        "dry-run must never list the sandbox root for removal: {payload}"
    );
    assert_eq!(
        tree(sandbox.root()),
        before,
        "uninstall --dry-run must not change any local file"
    );
}

#[test]
fn uninstall_keep_data_preserves_credentials_and_timetable() {
    let sandbox = Sandbox::new();
    let (config, cache) = install_owned_data(&sandbox);
    let before = tree(sandbox.root());

    let inv = run_isolated_copy(&sandbox, &["uninstall", "--keep-data"]);
    let payload = assert_success_json(&inv);
    assert_eq!(payload["dry_run"], false, "keep-data payload: {payload}");
    let removed = &payload["removed"];
    assert!(
        !removed_covers(removed, &config.join("credentials.json")),
        "keep-data must not report credentials as removed: {payload}"
    );
    assert!(
        !removed_covers(removed, &cache.join("timetable.json")),
        "keep-data must not report the timetable as removed: {payload}"
    );
    assert!(
        array_mentions(&payload["skipped"], &config) || array_mentions(&payload["skipped"], &cache),
        "keep-data must report the data directories as skipped: {payload}"
    );
    let note = payload["note"]
        .as_str()
        .expect("uninstall note is a string");
    assert!(
        note.contains("--keep-data"),
        "keep-data note must name the intentional preservation: {payload}"
    );
    assert_ne!(
        note, "卸载完成",
        "keep-data must not claim a complete removal: {payload}"
    );
    assert_eq!(
        tree(sandbox.root()),
        before,
        "uninstall --keep-data must preserve credentials, session and timetable"
    );
}

#[test]
fn uninstall_skips_mixed_data_directories_wholesale() {
    let sandbox = Sandbox::new();
    let (config, cache) = install_owned_data(&sandbox);
    let config_note = config.join("notes.txt");
    let cache_note = cache.join("scratch.dat");
    write_file(&config_note, UNRELATED);
    write_file(&cache_note, UNRELATED);
    let before = tree(sandbox.root());

    let inv = run_isolated_copy(&sandbox, &["uninstall"]);
    let payload = assert_success_json(&inv);
    assert_eq!(payload["dry_run"], false, "uninstall payload: {payload}");

    assert_eq!(
        tree(sandbox.root()),
        before,
        "mixed data directories must be preserved byte-for-byte: {payload}"
    );
    assert!(
        config.join("credentials.json").exists() && config.join("config.json").exists(),
        "owned files in a mixed directory must not be removed piecemeal: {payload}"
    );
    assert!(
        cache.join("session.json").exists() && cache.join("timetable.json").exists(),
        "owned cache files in a mixed directory must be preserved: {payload}"
    );
    assert!(
        config_note.exists() && cache_note.exists(),
        "unrelated files in mixed directories must be preserved: {payload}"
    );
    assert!(
        !removed_listed(&payload["removed"], &config)
            && !removed_listed(&payload["removed"], &cache),
        "mixed directories must never be reported as removed: {payload}"
    );
    assert!(
        !array_mentions(&payload["removed"], &config.join("credentials.json"))
            && !array_mentions(&payload["removed"], &cache.join("timetable.json")),
        "mixed directories must not report owned files as removed: {payload}"
    );
    assert!(
        array_mentions(&payload["skipped"], &config) && array_mentions(&payload["skipped"], &cache),
        "mixed directories must be reported as skipped: {payload}"
    );
    let note = payload["note"]
        .as_str()
        .expect("uninstall note is a string");
    assert_ne!(
        note, "卸载完成",
        "uninstall must not claim a complete removal when data dirs were skipped: {payload}"
    );
    assert!(
        note.contains("部分") || note.contains("跳过") || note.contains("待处理"),
        "uninstall note must disclose skipped or partial work: {payload}"
    );
}

#[test]
fn uninstall_removes_an_owned_timetable_only_directory() {
    let sandbox = Sandbox::new();
    let cache = sandbox.root().join("cache");
    let timetable = cache.join("timetable.json");
    write_file(&timetable, TIMETABLE);

    let inv = run_isolated_copy(&sandbox, &["uninstall"]);
    let payload = assert_success_json(&inv);
    assert!(
        removed_listed(&payload["removed"], &cache)
            || removed_listed(&payload["removed"], &timetable),
        "an owned-only timetable directory must be reported as removed: {payload}"
    );
    assert!(
        !timetable.exists(),
        "uninstall must recognise timetable.json as its own file: {payload}"
    );
    assert!(
        !cache.exists(),
        "an owned-only timetable directory must be removed completely: {payload}"
    );
}

#[test]
fn uninstall_does_not_remove_a_timetable_lookalike() {
    let sandbox = Sandbox::new();
    let cache = sandbox.root().join("cache");
    let lookalike = cache.join("timetable.json");
    write_file(
        &lookalike,
        "{\"version\": 999, \"term\": \"not-a-bupt-timetable\", \"other\": true}\n",
    );
    let before = tree(sandbox.root());

    let inv = run_isolated_copy(&sandbox, &["uninstall"]);
    let payload = assert_success_json(&inv);
    assert_eq!(
        tree(sandbox.root()),
        before,
        "a timetable.json name lookalike must be preserved byte-for-byte: {payload}"
    );
    assert!(
        lookalike.exists(),
        "uninstall must not delete a non-owned file merely because it is named timetable.json: {payload}"
    );
    assert!(
        !removed_listed(&payload["removed"], &lookalike)
            && !removed_listed(&payload["removed"], &cache),
        "a lookalike timetable must never be reported as removed: {payload}"
    );
    assert!(
        array_mentions(&payload["skipped"], &cache),
        "a non-owned timetable directory must be reported as skipped: {payload}"
    );
}

#[test]
fn uninstall_refuses_dangerous_data_roots_in_destructive_mode() {
    let sandbox = Sandbox::new();
    let home = sandbox.root().join("home");
    // The general ownership check would accept this content; the HOME guard
    // must be what prevents deletion.
    write_file(&home.join("credentials.json"), CREDENTIALS);
    write_file(&home.join("config.json"), CONFIG);
    let before = tree(&home);

    let inv = run_isolated_copy_with(&sandbox, &["uninstall"], |command| {
        command
            .env("BUPT_CLI_CONFIG_DIR", &home)
            .env("BUPT_CLI_CACHE_DIR", &home);
    });
    let payload = assert_success_json(&inv);
    assert_eq!(payload["dry_run"], false, "uninstall payload: {payload}");
    assert!(
        !removed_listed(&payload["removed"], &home),
        "uninstall must refuse the home directory: {payload}"
    );
    assert!(
        array_mentions(&payload["skipped"], &home),
        "uninstall must report the refused home directory as skipped: {payload}"
    );
    assert_eq!(
        tree(&home),
        before,
        "a dangerous data root must be preserved byte-for-byte: {payload}"
    );
}

#[cfg(unix)]
#[test]
fn uninstall_does_not_follow_a_symlinked_data_directory() {
    use std::os::unix::fs::symlink;

    let sandbox = Sandbox::new();
    let target = tempfile::tempdir().expect("create symlink target directory");
    let target_dir = target.path();
    write_file(&target_dir.join("credentials.json"), CREDENTIALS);
    write_file(&target_dir.join("config.json"), CONFIG);

    let link = sandbox.root().join("config");
    fs::remove_dir(&link).expect("remove empty sandbox config directory");
    symlink(target_dir, &link).expect("create symlinked config directory");
    let before = tree(target_dir);

    let inv = run_isolated_copy_with(&sandbox, &["uninstall"], |command| {
        command.env("BUPT_CLI_CONFIG_DIR", &link);
    });
    let payload = assert_success_json(&inv);
    assert_eq!(
        tree(target_dir),
        before,
        "uninstall must not follow a symlinked data directory: {payload}"
    );
    assert!(
        !removed_listed(&payload["removed"], target_dir),
        "uninstall must not report the symlink target as removed: {payload}"
    );
    assert!(
        !array_mentions(&payload["skipped"], target_dir),
        "uninstall must not inspect the symlink target as a data directory: {payload}"
    );
}

#[test]
fn uninstall_does_not_delete_an_unrelated_executable_on_path() {
    // The migration contract does not require scanning PATH. If the
    // implementation does scan it, this lookalike may only appear as skipped;
    // either way it must survive.
    let sandbox = Sandbox::new();
    let fixture = tempfile::tempdir().expect("create explicit PATH fixture directory");
    let binary = fixture.path().join(platform_binary_name());
    write_file(&binary, "#!/bin/sh\necho not-the-bupt-cli\n");
    make_executable(&binary);

    let inv = run_isolated_copy_with(&sandbox, &["uninstall"], |command| {
        command
            .env("PATH", fixture.path())
            .current_dir(fixture.path());
    });
    let payload = assert_success_json(&inv);
    assert!(
        !removed_listed(&payload["removed"], &binary),
        "uninstall must verify ownership before deleting an executable: {payload}"
    );
    assert!(
        binary.exists(),
        "uninstall must leave the unrelated executable in place: {payload}"
    );
}

#[cfg(unix)]
#[test]
fn uninstall_does_not_delete_a_source_file_named_like_the_binary() {
    let sandbox = Sandbox::new();
    let fixture = tempfile::tempdir().expect("create explicit PATH fixture directory");
    let source = fixture.path().join(platform_binary_name());
    write_file(
        &source,
        "from bupt_cli.cli import main\n\nif __name__ == '__main__':\n    raise SystemExit(main())\n",
    );

    let inv = run_isolated_copy_with(&sandbox, &["uninstall"], |command| {
        command
            .env("PATH", fixture.path())
            .current_dir(fixture.path());
    });
    let payload = assert_success_json(&inv);
    assert!(
        !removed_listed(&payload["removed"], &source),
        "uninstall must not treat a non-executable source file as its binary: {payload}"
    );
    assert!(
        source.exists(),
        "uninstall must leave the source file in place: {payload}"
    );
}

#[test]
fn update_help_documents_check_and_repo_inputs() {
    let sandbox = Sandbox::new();

    for args in [&["help", "update"][..], &["update", "--help"][..]] {
        let help = assert_text_output(&sandbox.run(args));
        assert!(
            help.contains("--check"),
            "update help must document --check:\n{help}"
        );
        assert!(
            help.contains("--repo"),
            "update help must document --repo:\n{help}"
        );
    }
}

#[test]
fn update_rejects_invalid_inputs_before_release_access() {
    let sandbox = Sandbox::new();
    let cases: &[&[&str]] = &[
        &["update", "--check", "--repo"],
        &["update", "--check", "--repo", ""],
        &["update", "--check", "--repo", "owner"],
        &["update", "--check", "--repo", "owner/repo/extra"],
        &[
            "update",
            "--check",
            "--repo",
            "https://github.com/owner/repo",
        ],
        &["update", "--unknown-option"],
        &["--timeout", "0", "update", "--check"],
    ];

    for args in cases {
        let inv = sandbox.run(args);
        assert_error_json(&inv, 2, "usage");
        assert_eq!(
            stderr_text(&inv).lines().count(),
            1,
            "update usage errors must be one compact JSON document: {:?}",
            stderr_text(&inv)
        );
    }

    assert!(
        tree(sandbox.root()).is_empty(),
        "invalid update inputs must not create state or stage files: {:?}",
        tree(sandbox.root())
    );
}

#[test]
fn update_check_is_blocked_without_a_scoped_production_transport() {
    let sandbox = Sandbox::new();

    let inv = sandbox.run(&["update", "--check", "--repo", "owner/repo"]);
    assert_error_json(&inv, 4, "network");
    assert!(
        tree(sandbox.root()).is_empty(),
        "a blocked release check must not create state or stage files: {:?}",
        tree(sandbox.root())
    );
}

#[test]
fn update_apply_is_blocked_at_the_release_check_without_a_production_transport() {
    let sandbox = Sandbox::new();

    let inv = sandbox.run(&["update", "--repo", "owner/repo"]);
    assert_error_json(&inv, 4, "network");
    assert!(
        tree(sandbox.root()).is_empty(),
        "a blocked update must not create state or staging files: {:?}",
        tree(sandbox.root())
    );
}
