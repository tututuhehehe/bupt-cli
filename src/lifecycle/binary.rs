//! Platform-native executable checks for the self-update path.
//!
//! A magic-number check is not enough for an update target: a truncated file,
//! a shared library, or a binary for another architecture must not be replaced
//! or installed. These parsers deliberately validate only the small structural
//! subset needed to recognize a native executable; they never execute it.

#[cfg(target_os = "macos")]
const MACHO_ARM64: u32 = 0x0100_000c;
#[cfg(all(target_os = "macos", test))]
const MACHO_X86_64: u32 = 0x0100_0007;
#[cfg(target_os = "macos")]
const MH_MAGIC_64: u32 = 0xfeed_facf;
#[cfg(target_os = "macos")]
const MH_EXECUTE: u32 = 0x2;
#[cfg(target_os = "macos")]
const LC_SEGMENT_64: u32 = 0x19;
#[cfg(target_os = "macos")]
const FAT_MAGIC: u32 = 0xcafe_babe;
#[cfg(target_os = "macos")]
const FAT_MAGIC_64: u32 = 0xcafe_babf;
#[cfg(target_os = "macos")]
const MAX_LOAD_COMMANDS: usize = 4096;

#[cfg(windows)]
const PE_X64: u16 = 0x8664;
#[cfg(windows)]
const PE32_PLUS: u16 = 0x20b;

#[cfg(all(unix, not(target_os = "macos")))]
const ELF_X86_64: u16 = 0x3e;
#[cfg(all(unix, not(target_os = "macos")))]
const ELF_AARCH64: u16 = 0xb7;
#[cfg(all(unix, not(target_os = "macos")))]
const ELF_EXEC: u16 = 2;
#[cfg(all(unix, not(target_os = "macos")))]
const ELF_DYN: u16 = 3;
#[cfg(all(unix, not(target_os = "macos")))]
const PT_LOAD: u32 = 1;

pub(super) fn is_native_executable(bytes: &[u8]) -> bool {
    #[cfg(target_os = "macos")]
    {
        is_macho_arm64(bytes)
    }
    #[cfg(windows)]
    {
        is_pe_x64(bytes)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        is_elf_for_this_arch(bytes)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = bytes;
        false
    }
}

pub(super) fn is_python_launcher(bytes: &[u8]) -> bool {
    // PyInstaller onefile bootloaders end with this cookie; script launchers
    // start with a shebang even when their name is `bupt`. Only inspect the
    // tail so an unrelated marker embedded in a large Rust binary is ignored.
    if bytes.starts_with(b"#!") {
        return true;
    }
    let tail = &bytes[bytes.len().saturating_sub(256)..];
    tail.windows(8)
        .any(|window| window == b"MEI\x0c\x0b\x0a\x0b\x0e")
}

#[cfg(target_os = "macos")]
fn is_macho_arm64(bytes: &[u8]) -> bool {
    if read_u32_be(bytes, 0) == Some(FAT_MAGIC) {
        return fat_contains_arm64(bytes, false);
    }
    if read_u32_be(bytes, 0) == Some(FAT_MAGIC_64) {
        return fat_contains_arm64(bytes, true);
    }
    is_thin_macho_arm64(bytes)
}

#[cfg(target_os = "macos")]
fn is_thin_macho_arm64(bytes: &[u8]) -> bool {
    if bytes.len() < 32 || read_u32_le(bytes, 0) != Some(MH_MAGIC_64) {
        return false;
    }
    if read_u32_le(bytes, 4) != Some(MACHO_ARM64) || read_u32_le(bytes, 12) != Some(MH_EXECUTE) {
        return false;
    }
    let Some(command_count) = read_u32_le(bytes, 16).map(|value| value as usize) else {
        return false;
    };
    let Some(command_bytes) = read_u32_le(bytes, 20).map(|value| value as usize) else {
        return false;
    };
    if command_count == 0
        || command_count > MAX_LOAD_COMMANDS
        || command_bytes < command_count.saturating_mul(8)
        || command_bytes > bytes.len() - 32
    {
        return false;
    }

    let mut cursor = 32;
    let end = 32 + command_bytes;
    let mut has_file_segment = false;
    for _ in 0..command_count {
        let Some(command) = read_u32_le(bytes, cursor) else {
            return false;
        };
        let Some(size) = read_u32_le(bytes, cursor + 4).map(|value| value as usize) else {
            return false;
        };
        if size < 8 || size % 8 != 0 || cursor + size > end {
            return false;
        }
        if command == LC_SEGMENT_64 {
            if size < 72 {
                return false;
            }
            let Some(file_offset) = read_u64_le(bytes, cursor + 40) else {
                return false;
            };
            let Some(file_size) = read_u64_le(bytes, cursor + 48) else {
                return false;
            };
            if file_offset > bytes.len() as u64 || file_size > bytes.len() as u64 - file_offset {
                return false;
            }
            if file_size > 0 {
                has_file_segment = true;
            }
        }
        cursor += size;
    }
    cursor == end && has_file_segment
}

#[cfg(target_os = "macos")]
fn fat_contains_arm64(bytes: &[u8], is_64: bool) -> bool {
    let Some(count) = read_u32_be(bytes, 4).map(|value| value as usize) else {
        return false;
    };
    if count == 0 || count > 64 {
        return false;
    }
    let entry_size = if is_64 { 32 } else { 20 };
    let Some(table_end) = 8usize.checked_add(count.saturating_mul(entry_size)) else {
        return false;
    };
    if table_end > bytes.len() {
        return false;
    }
    for index in 0..count {
        let entry = 8 + index * entry_size;
        let Some(cpu) = read_u32_be(bytes, entry) else {
            return false;
        };
        if cpu != MACHO_ARM64 {
            continue;
        }
        let (offset, size) = if is_64 {
            (
                read_u64_be(bytes, entry + 8),
                read_u64_be(bytes, entry + 16),
            )
        } else {
            (
                read_u32_be(bytes, entry + 8).map(u64::from),
                read_u32_be(bytes, entry + 12).map(u64::from),
            )
        };
        let (Some(offset), Some(size)) = (offset, size) else {
            return false;
        };
        if offset > bytes.len() as u64 || size > bytes.len() as u64 - offset {
            continue;
        }
        let start = offset as usize;
        let end = start + size as usize;
        if is_thin_macho_arm64(&bytes[start..end]) {
            return true;
        }
    }
    false
}

#[cfg(windows)]
fn is_pe_x64(bytes: &[u8]) -> bool {
    if bytes.len() < 0x40 || &bytes[..2] != b"MZ" {
        return false;
    }
    let Some(pe) = read_u32_le(bytes, 0x3c).map(|value| value as usize) else {
        return false;
    };
    if pe + 24 > bytes.len() || &bytes[pe..pe + 4] != b"PE\0\0" {
        return false;
    }
    if read_u16_le(bytes, pe + 4) != Some(PE_X64) {
        return false;
    }
    let Some(section_count) = read_u16_le(bytes, pe + 6).map(|value| value as usize) else {
        return false;
    };
    let Some(optional_size) = read_u16_le(bytes, pe + 20).map(|value| value as usize) else {
        return false;
    };
    let Some(characteristics) = read_u16_le(bytes, pe + 22) else {
        return false;
    };
    if characteristics & 0x0002 == 0
        || section_count == 0
        || section_count > 96
        || optional_size < 2
    {
        return false;
    }
    let optional = pe + 24;
    if optional + optional_size > bytes.len() || read_u16_le(bytes, optional) != Some(PE32_PLUS) {
        return false;
    }
    let sections = optional + optional_size;
    if sections + section_count * 40 > bytes.len() {
        return false;
    }
    for index in 0..section_count {
        let section = sections + index * 40;
        let Some(raw_size) = read_u32_le(bytes, section + 16).map(|value| value as usize) else {
            return false;
        };
        let Some(raw_offset) = read_u32_le(bytes, section + 20).map(|value| value as usize) else {
            return false;
        };
        if raw_size > 0 && (raw_offset > bytes.len() || raw_size > bytes.len() - raw_offset) {
            return false;
        }
    }
    true
}

#[cfg(all(unix, not(target_os = "macos")))]
fn is_elf_for_this_arch(bytes: &[u8]) -> bool {
    if bytes.len() < 64
        || &bytes[..4] != b"\x7fELF"
        || bytes[4] != 2
        || bytes[5] != 1
        || bytes[6] != 1
    {
        return false;
    }
    let expected_machine = if cfg!(target_arch = "x86_64") {
        ELF_X86_64
    } else if cfg!(target_arch = "aarch64") {
        ELF_AARCH64
    } else {
        return false;
    };
    if read_u16_le(bytes, 18) != Some(expected_machine) {
        return false;
    }
    if !matches!(read_u16_le(bytes, 16), Some(ELF_EXEC) | Some(ELF_DYN)) {
        return false;
    }
    if read_u16_le(bytes, 52) != Some(64) || read_u16_le(bytes, 54) != Some(56) {
        return false;
    }
    let Some(phoff) = read_u64_le(bytes, 32).map(|value| value as usize) else {
        return false;
    };
    let Some(phnum) = read_u16_le(bytes, 56).map(|value| value as usize) else {
        return false;
    };
    if phnum == 0 || phnum > 4096 {
        return false;
    }
    let Some(table_end) = phoff.checked_add(phnum.saturating_mul(56)) else {
        return false;
    };
    if table_end > bytes.len() {
        return false;
    }
    for index in 0..phnum {
        let header = phoff + index * 56;
        if read_u32_le(bytes, header) == Some(PT_LOAD) {
            return true;
        }
    }
    false
}

#[cfg(any(windows, all(unix, not(target_os = "macos"))))]
fn read_u16_le(bytes: &[u8], offset: usize) -> Option<u16> {
    let slice = bytes.get(offset..offset + 2)?;
    Some(u16::from_le_bytes([slice[0], slice[1]]))
}

fn read_u32_le(bytes: &[u8], offset: usize) -> Option<u32> {
    let slice = bytes.get(offset..offset + 4)?;
    Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

#[cfg(target_os = "macos")]
fn read_u32_be(bytes: &[u8], offset: usize) -> Option<u32> {
    let slice = bytes.get(offset..offset + 4)?;
    Some(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

#[cfg(any(target_os = "macos", all(unix, not(target_os = "macos"))))]
fn read_u64_le(bytes: &[u8], offset: usize) -> Option<u64> {
    let slice = bytes.get(offset..offset + 8)?;
    Some(u64::from_le_bytes([
        slice[0], slice[1], slice[2], slice[3], slice[4], slice[5], slice[6], slice[7],
    ]))
}

#[cfg(target_os = "macos")]
fn read_u64_be(bytes: &[u8], offset: usize) -> Option<u64> {
    let slice = bytes.get(offset..offset + 8)?;
    Some(u64::from_be_bytes([
        slice[0], slice[1], slice[2], slice[3], slice[4], slice[5], slice[6], slice[7],
    ]))
}

#[cfg(test)]
pub(super) fn test_native_payload(marker: &str) -> Vec<u8> {
    #[cfg(target_os = "macos")]
    {
        test_macho(marker, MACHO_ARM64)
    }
    #[cfg(windows)]
    {
        let _ = marker;
        test_pe(true)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        test_elf_for_this_arch(marker)
    }
    #[cfg(not(any(unix, windows)))]
    {
        marker.as_bytes().to_vec()
    }
}

#[cfg(test)]
pub(super) fn test_wrong_arch_payload(marker: &str) -> Vec<u8> {
    #[cfg(target_os = "macos")]
    {
        test_macho(marker, MACHO_X86_64)
    }
    #[cfg(windows)]
    {
        let _ = marker;
        test_pe(false)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut payload = test_elf_for_this_arch(marker);
        let machine = if cfg!(target_arch = "x86_64") {
            ELF_AARCH64
        } else {
            ELF_X86_64
        };
        payload[18..20].copy_from_slice(&machine.to_le_bytes());
        payload
    }
    #[cfg(not(any(unix, windows)))]
    {
        marker.as_bytes().to_vec()
    }
}

#[cfg(test)]
pub(super) fn test_truncated_payload(marker: &str) -> Vec<u8> {
    let mut payload = test_native_payload(marker);
    // Cut before all platform load/segment tables; this must fail structural
    // validation even though the executable magic may still be present.
    payload.truncate(64);
    payload
}

#[cfg(test)]
#[cfg(target_os = "macos")]
fn test_macho(marker: &str, cputype: u32) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&MH_MAGIC_64.to_le_bytes());
    payload.extend_from_slice(&cputype.to_le_bytes());
    payload.extend_from_slice(&0u32.to_le_bytes());
    payload.extend_from_slice(&MH_EXECUTE.to_le_bytes());
    payload.extend_from_slice(&1u32.to_le_bytes());
    payload.extend_from_slice(&72u32.to_le_bytes());
    payload.extend_from_slice(&0u32.to_le_bytes());
    payload.extend_from_slice(&0u32.to_le_bytes());
    let segment_start = payload.len();
    payload.extend_from_slice(&LC_SEGMENT_64.to_le_bytes());
    payload.extend_from_slice(&72u32.to_le_bytes());
    payload.extend_from_slice(b"__TEXT\0\0\0\0\0\0\0\0\0\0");
    payload.extend_from_slice(&0x1000_0000u64.to_le_bytes());
    payload.extend_from_slice(&0x1000u64.to_le_bytes());
    payload.extend_from_slice(&0u64.to_le_bytes());
    payload.extend_from_slice(&0u64.to_le_bytes());
    payload.extend_from_slice(&7u32.to_le_bytes());
    payload.extend_from_slice(&5u32.to_le_bytes());
    payload.extend_from_slice(&0u32.to_le_bytes());
    payload.extend_from_slice(&0u32.to_le_bytes());
    payload.extend_from_slice(marker.as_bytes());
    payload.extend_from_slice(&[0u8; 64]);
    let total = payload.len() as u64;
    payload[segment_start + 48..segment_start + 56].copy_from_slice(&total.to_le_bytes());
    payload
}

#[cfg(test)]
#[cfg(windows)]
fn test_pe(x64: bool) -> Vec<u8> {
    let mut payload = vec![0u8; 0x200];
    payload[..2].copy_from_slice(b"MZ");
    payload[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
    payload[0x80..0x84].copy_from_slice(b"PE\0\0");
    let machine = if x64 { PE_X64 } else { 0xaa64 };
    payload[0x84..0x86].copy_from_slice(&machine.to_le_bytes());
    payload[0x86..0x88].copy_from_slice(&1u16.to_le_bytes());
    payload[0x94..0x96].copy_from_slice(&0x00f0u16.to_le_bytes());
    payload[0x96..0x98].copy_from_slice(&0x0022u16.to_le_bytes());
    payload[0x98..0x9a].copy_from_slice(&PE32_PLUS.to_le_bytes());
    let section = 0x98 + 0x00f0;
    payload[section + 16..section + 20].copy_from_slice(&0x80u32.to_le_bytes());
    payload[section + 20..section + 24].copy_from_slice(&0x180u32.to_le_bytes());
    payload
}

#[cfg(test)]
#[cfg(all(unix, not(target_os = "macos")))]
fn test_elf_for_this_arch(marker: &str) -> Vec<u8> {
    let mut payload = vec![0u8; 64 + 56 + marker.len()];
    payload[..4].copy_from_slice(b"\x7fELF");
    payload[4] = 2;
    payload[5] = 1;
    payload[6] = 1;
    payload[16..18].copy_from_slice(&ELF_DYN.to_le_bytes());
    let machine = if cfg!(target_arch = "x86_64") {
        ELF_X86_64
    } else {
        ELF_AARCH64
    };
    payload[18..20].copy_from_slice(&machine.to_le_bytes());
    payload[32..40].copy_from_slice(&64u64.to_le_bytes());
    payload[52..54].copy_from_slice(&64u16.to_le_bytes());
    payload[54..56].copy_from_slice(&56u16.to_le_bytes());
    payload[56..58].copy_from_slice(&1u16.to_le_bytes());
    payload[64..68].copy_from_slice(&PT_LOAD.to_le_bytes());
    payload[120..].copy_from_slice(marker.as_bytes());
    payload
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_payload_is_accepted_and_wrong_arch_is_rejected() {
        let native = test_native_payload("payload");
        assert!(is_native_executable(&native));
        assert!(!is_native_executable(&test_wrong_arch_payload("payload")));
    }

    #[test]
    fn truncated_native_payload_is_rejected() {
        assert!(!is_native_executable(&test_truncated_payload("payload")));
        let native = test_native_payload("payload");
        for end in 0..native.len() {
            let _ = is_native_executable(&native[..end]);
        }
    }

    #[test]
    fn python_launchers_are_not_native_candidates() {
        let mut payload = test_native_payload("payload");
        assert!(!is_python_launcher(&payload));
        payload.extend_from_slice(b"MEI\x0c\x0b\x0a\x0b\x0e");
        assert!(is_python_launcher(&payload));
        payload.truncate(payload.len() - 8);
        assert!(is_python_launcher(b"#!/bin/sh\n"));
    }
}
