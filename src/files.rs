//! Saving downloaded attachments.
//!
//! One rule for every download command: the caller resolves an output
//! directory, turns an upstream name into a single safe path component, and
//! writes through a temporary file that is renamed into place only once the
//! whole body is available. A partial body never reaches the destination.

use crate::cli::{CliError, CliResult};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const MAX_FILENAME_BYTES: usize = 200;
const MAX_EXTENSION_BYTES: usize = 16;

/// Expand the leading `~` the shell would have expanded for a quoted argument.
pub(crate) fn expand_dir(raw: &str, home_dir: &Path) -> PathBuf {
    if raw == "~" {
        home_dir.to_path_buf()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home_dir.join(rest)
    } else {
        PathBuf::from(raw)
    }
}

/// Create the output directory if needed and resolve it, so every reported
/// path is absolute and every write is checked against one resolved directory.
pub(crate) fn prepare_dir(dir: &Path) -> CliResult<PathBuf> {
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
/// An extension is kept while truncating so a long name stays openable.
pub(crate) fn safe_filename(name: &str, fallback: &str) -> String {
    let mut cleaned = String::with_capacity(name.len());
    for character in name.chars() {
        if character.is_control()
            || matches!(
                character,
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
            )
        {
            cleaned.push('_');
        } else {
            cleaned.push(character);
        }
    }
    let cleaned = cleaned.trim_end_matches([' ', '.']).to_owned();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        return fallback.to_owned();
    }
    if cleaned.len() <= MAX_FILENAME_BYTES {
        return cleaned;
    }
    let (stem, extension) = split_extension(&cleaned);
    let budget = MAX_FILENAME_BYTES.saturating_sub(extension.len());
    let mut truncated = String::with_capacity(budget + extension.len());
    for character in stem.chars() {
        if truncated.len() + character.len_utf8() > budget {
            break;
        }
        truncated.push(character);
    }
    let truncated = truncated.trim_end_matches([' ', '.']).to_owned();
    if truncated.is_empty() {
        fallback.to_owned()
    } else {
        truncated + extension
    }
}

/// Split `name.ext` into its stem and `.ext`. The extension is only treated as
/// one when it is a short run of ASCII alphanumerics, so a dotted title keeps
/// its text instead of losing it to a bogus extension.
fn split_extension(name: &str) -> (&str, &str) {
    let Some((stem, extension)) = name.rsplit_once('.') else {
        return (name, "");
    };
    let usable = !stem.is_empty()
        && extension.len() <= MAX_EXTENSION_BYTES
        && extension.bytes().all(|byte| byte.is_ascii_alphanumeric());
    if usable {
        (&name[..stem.len()], &name[stem.len()..])
    } else {
        (name, "")
    }
}

pub(crate) enum WriteOutcome {
    Written(u64),
    Exists,
}

/// Write one body into `dest`, which must be a direct child of `dir`. The
/// temporary file lives in the same directory so the rename is atomic, and an
/// existing destination is only replaced when `force` is set.
pub(crate) fn write_atomic(
    dir: &Path,
    dest: &Path,
    body: &[u8],
    force: bool,
) -> CliResult<WriteOutcome> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_one_path_component_and_the_extension() {
        assert_eq!(safe_filename("../报告.pdf", "attachment-1"), ".._报告.pdf");
        assert_eq!(safe_filename("../../evil", "attachment-1"), ".._.._evil");
        assert_eq!(safe_filename("/etc/passwd", "attachment-1"), "_etc_passwd");
        assert_eq!(
            safe_filename("a/b\\c:d*e?f\"g<h>i|j", "attachment-1"),
            "a_b_c_d_e_f_g_h_i_j"
        );
        assert_eq!(safe_filename("..", "attachment-3"), "attachment-3");
        assert_eq!(safe_filename(" . ", "attachment-2"), "attachment-2");
        assert_eq!(
            safe_filename("附件1：汇总表(1).xlsx", "attachment-1"),
            "附件1：汇总表(1).xlsx"
        );
        let long = format!("{}.docx", "很".repeat(MAX_FILENAME_BYTES));
        let name = safe_filename(&long, "attachment-1");
        assert!(name.len() <= MAX_FILENAME_BYTES, "{name}");
        assert!(name.ends_with(".docx"), "{name}");
        assert!(!name.contains('/'));
        let long = "长".repeat(MAX_FILENAME_BYTES);
        assert!(safe_filename(&long, "attachment-1").len() <= MAX_FILENAME_BYTES);
    }

    #[test]
    fn writes_once_and_respects_force() {
        let temp = tempfile::tempdir().expect("temp dir");
        let dir = prepare_dir(temp.path()).expect("resolved dir");
        let dest = dir.join("a.txt");
        assert!(matches!(
            write_atomic(&dir, &dest, b"one", false),
            Ok(WriteOutcome::Written(3))
        ));
        assert!(matches!(
            write_atomic(&dir, &dest, b"two", false),
            Ok(WriteOutcome::Exists)
        ));
        assert_eq!(fs::read(&dest).unwrap(), b"one");
        assert!(matches!(
            write_atomic(&dir, &dest, b"two", true),
            Ok(WriteOutcome::Written(3))
        ));
        assert_eq!(fs::read(&dest).unwrap(), b"two");
        assert_eq!(
            fs::read_dir(&dir).unwrap().count(),
            1,
            "a rejected write leaves no temporary file"
        );
    }

    #[test]
    fn refuses_a_destination_outside_the_output_directory() {
        // The escape destination is a real sibling directory, not the same one
        // reached through a non-canonical path: on a platform whose temp dir is
        // already canonical (a typical Linux `/tmp`), the latter would be
        // accepted and the test would prove nothing.
        let parent = tempfile::tempdir().expect("temp dir");
        let dir = prepare_dir(&parent.path().join("out")).expect("resolved dir");
        let outside = parent.path().join("outside");
        fs::create_dir_all(&outside).expect("create sibling dir");
        for dest in [
            outside.join("escape.txt"),
            dir.join("..").join("escape.txt"),
            dir.join("nested").join("escape.txt"),
        ] {
            assert!(
                !matches!(
                    write_atomic(&dir, &dest, b"x", false),
                    Ok(WriteOutcome::Written(_))
                ),
                "{dest:?} must not be writable through the output directory"
            );
            assert!(!dest.exists(), "{dest:?} must not exist");
        }
        assert_eq!(
            fs::read_dir(&dir).unwrap().count(),
            0,
            "a rejected write leaves no temporary file"
        );
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    }

    #[test]
    fn expands_a_home_directory_and_reports_a_bad_output_path() {
        let temp = tempfile::tempdir().expect("temp dir");
        let home = temp.path().join("home");
        assert_eq!(expand_dir("~", &home), home);
        assert_eq!(expand_dir("~/notes", &home), home.join("notes"));
        // `~user` is not expanded: the shell does not expand a quoted argument.
        assert_eq!(expand_dir("~other/x", &home), PathBuf::from("~other/x"));
        assert_eq!(expand_dir("/tmp/x", &home), PathBuf::from("/tmp/x"));

        // An existing file is not a directory, and the error says so.
        let file = temp.path().join("file.txt");
        fs::write(&file, b"x").unwrap();
        let error = prepare_dir(&file).unwrap_err();
        assert_eq!(error.code, 5);
        assert!(
            error.hint.contains("--dir"),
            "the hint names the flag that fixes it; hint={}",
            error.hint
        );
    }
}
