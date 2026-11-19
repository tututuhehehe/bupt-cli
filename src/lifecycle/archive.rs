use std::fmt;
use std::io::{Cursor, Read};

use zip::{CompressionMethod, ZipArchive};

const MAX_ARCHIVE_BYTES: usize = 256 * 1024 * 1024;
const MAX_BINARY_BYTES: usize = 256 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;
const MAX_NAME_BYTES: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ArchiveError(String);

impl ArchiveError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for ArchiveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Extract exactly one `bupt` executable from a ZIP archive.
///
/// The archive is parsed in memory by the maintained `zip` crate. Names are
/// only inspected, never used as output paths; symlink entries, duplicate
/// targets, unsafe names and unavailable compression methods are rejected
/// before any bytes are returned. The reader verifies each entry's CRC and
/// declared size.
pub(super) fn extract_bupt(archive: &[u8]) -> Result<Vec<u8>, ArchiveError> {
    if archive.len() > MAX_ARCHIVE_BYTES {
        return Err(ArchiveError::new("release archive is too large"));
    }

    let mut zip = ZipArchive::new(Cursor::new(archive))
        .map_err(|error| ArchiveError::new(format!("release archive is invalid: {error}")))?;
    let entry_count = zip.len();
    if entry_count == 0 || entry_count > MAX_ENTRIES {
        return Err(ArchiveError::new(
            "release archive has an invalid entry count",
        ));
    }

    let mut candidate: Option<usize> = None;
    for index in 0..entry_count {
        let file = zip.by_index(index).map_err(|error| {
            ArchiveError::new(format!("release archive entry is invalid: {error}"))
        })?;
        let name = file
            .name()
            .map_err(|error| {
                ArchiveError::new(format!("release archive entry name is invalid: {error}"))
            })?
            .into_owned();
        if file.is_symlink() {
            return Err(ArchiveError::new(
                "release archive contains a symbolic link",
            ));
        }
        if file.encrypted() {
            return Err(ArchiveError::new(
                "encrypted release archives are not supported",
            ));
        }
        if !matches!(
            file.compression(),
            CompressionMethod::Stored | CompressionMethod::Deflated
        ) {
            return Err(ArchiveError::new(
                "release archive uses an unsupported compression",
            ));
        }
        if !name_is_safe(&name, file.is_dir()) {
            return Err(ArchiveError::new(
                "release archive has an unsafe entry name",
            ));
        }
        let trimmed = name.trim_end_matches('/');
        if file.is_dir() {
            if trimmed == "bupt" || trimmed.ends_with("/bupt") {
                return Err(ArchiveError::new("release archive has a bupt directory"));
            }
            continue;
        }
        if trimmed == "bupt" || trimmed.ends_with("/bupt") {
            if candidate.is_some() {
                return Err(ArchiveError::new(
                    "release archive has more than one bupt executable",
                ));
            }
            candidate = Some(index);
        }
    }

    let index =
        candidate.ok_or_else(|| ArchiveError::new("release archive has no bupt executable"))?;
    let file = zip
        .by_index(index)
        .map_err(|error| ArchiveError::new(format!("release archive entry is invalid: {error}")))?;
    let expected_size = file.size();
    if expected_size > MAX_BINARY_BYTES as u64 {
        return Err(ArchiveError::new("release archive entry is too large"));
    }
    if file.compressed_size() > MAX_ARCHIVE_BYTES as u64 {
        return Err(ArchiveError::new("release archive entry is too large"));
    }

    let mut payload = Vec::with_capacity(expected_size as usize);
    file.take(MAX_BINARY_BYTES as u64 + 1)
        .read_to_end(&mut payload)
        .map_err(|error| {
            ArchiveError::new(format!("release archive entry cannot be read: {error}"))
        })?;
    if payload.len() as u64 != expected_size {
        return Err(ArchiveError::new(
            "release archive entry has an unexpected size",
        ));
    }
    if payload.len() > MAX_BINARY_BYTES {
        return Err(ArchiveError::new("release archive entry is too large"));
    }
    Ok(payload)
}

fn name_is_safe(name: &str, directory: bool) -> bool {
    if name.is_empty()
        || name.len() > MAX_NAME_BYTES
        || name.starts_with('/')
        || name.contains('\\')
        || name.contains('\0')
    {
        return false;
    }
    let trimmed = if directory {
        name.trim_end_matches('/')
    } else {
        if name.ends_with('/') {
            return false;
        }
        name
    };
    !trimmed.is_empty()
        && trimmed
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

#[cfg(test)]
pub(super) fn test_stored_zip(name: &str, payload: &[u8]) -> Vec<u8> {
    test_zip(name, payload, CompressionMethod::Stored)
}

#[cfg(test)]
fn test_zip(name: &str, payload: &[u8], method: CompressionMethod) -> Vec<u8> {
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default()
        .compression_method(method)
        .unix_permissions(0o755);
    writer
        .start_file(name, options)
        .expect("start test zip entry");
    writer.write_all(payload).expect("write test zip entry");
    writer.finish().expect("finish test zip").into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn zip_entries(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Stored)
            .unix_permissions(0o755);
        for (name, payload) in entries {
            writer.start_file(*name, options).expect("start test entry");
            writer.write_all(payload).expect("write test entry");
        }
        writer.finish().expect("finish test zip").into_inner()
    }

    fn symlink_zip(name: &str, target: &str) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .add_symlink(name, target, SimpleFileOptions::default())
            .expect("add symlink entry");
        writer.finish().expect("finish symlink zip").into_inner()
    }

    #[test]
    fn stored_entry_is_extracted() {
        let archive = test_stored_zip("bupt", b"native-payload");
        assert_eq!(
            extract_bupt(&archive).expect("extract stored entry"),
            b"native-payload"
        );
    }

    #[test]
    fn prefixed_entry_is_extracted() {
        let archive = test_zip("pkg/bupt", b"prefixed", CompressionMethod::Stored);
        assert_eq!(
            extract_bupt(&archive).expect("extract prefixed entry"),
            b"prefixed"
        );
    }

    #[test]
    fn deflated_entry_is_extracted() {
        let payload = b"deflated payload ".repeat(128);
        let archive = test_zip("bupt", &payload, CompressionMethod::Deflated);
        assert_eq!(
            extract_bupt(&archive).expect("extract deflated entry"),
            payload
        );
    }

    #[test]
    fn corrupt_and_unsafe_archives_are_rejected() {
        for name in ["../bupt", "/bupt", "pkg\\bupt", "pkg/../bupt", "pkg//bupt"] {
            let archive = zip_entries(&[(name, b"payload")]);
            assert!(
                extract_bupt(&archive).is_err(),
                "accepted unsafe name {name}"
            );
        }

        let archive = symlink_zip("bupt", "/tmp/target");
        assert!(extract_bupt(&archive).is_err(), "accepted a symlink entry");

        let archive = zip_entries(&[("bupt", b"first"), ("nested/bupt", b"second")]);
        assert!(
            extract_bupt(&archive).is_err(),
            "accepted duplicate targets"
        );

        let archive = zip_entries(&[("README", b"not the binary")]);
        assert!(extract_bupt(&archive).is_err(), "accepted a missing binary");

        let archive = zip_entries(&[("bupt", b"payload-with-a-unique-marker")]);
        let mut corrupt = archive.clone();
        let marker = b"payload-with-a-unique-marker";
        let offset = corrupt
            .windows(marker.len())
            .position(|window| window == marker)
            .expect("find stored payload");
        corrupt[offset] ^= 0xff;
        assert!(extract_bupt(&corrupt).is_err(), "accepted a bad CRC");
    }

    #[test]
    fn too_many_entries_are_rejected() {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default();
        for index in 0..=MAX_ENTRIES {
            writer
                .start_file(format!("entry-{index}"), options)
                .expect("start capped entry");
            writer.write_all(b"x").expect("write capped entry");
        }
        let archive = writer.finish().expect("finish capped zip").into_inner();
        assert!(extract_bupt(&archive).is_err());
    }

    #[test]
    fn truncated_and_mutated_archives_never_panic() {
        let archive = zip_entries(&[("bupt", b"native-payload")]);
        for end in 0..archive.len() {
            assert!(
                extract_bupt(&archive[..end]).is_err(),
                "accepted a truncated archive at {end} bytes"
            );
        }
        for index in 0..archive.len() {
            let mut mutated = archive.clone();
            mutated[index] ^= 0xff;
            let _ = extract_bupt(&mutated);
        }
    }
}
