use std::fmt;

const MAX_ARCHIVE_BYTES: usize = 256 * 1024 * 1024;
const MAX_BINARY_BYTES: usize = 256 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;
const MAX_NAME_BYTES: usize = 1024;

const EOCD_SIGNATURE: u32 = 0x0605_4b50;
const CENTRAL_SIGNATURE: u32 = 0x0201_4b50;
const LOCAL_SIGNATURE: u32 = 0x0403_4b50;

const STORED: u16 = 0;
const DEFLATED: u16 = 8;

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
/// The archive is parsed in memory. Names are never used as output paths, so
/// traversal cannot escape; symlink entries and duplicate targets are rejected
/// before any bytes are returned. Only stored and DEFLATE entries are accepted.
pub(super) fn extract_bupt(archive: &[u8]) -> Result<Vec<u8>, ArchiveError> {
    if archive.len() > MAX_ARCHIVE_BYTES {
        return Err(ArchiveError::new("release archive is too large"));
    }
    let end = find_directory_end(archive)?;
    let total_entries = le_u16(archive, end + 10)? as usize;
    let directory_size = le_u32(archive, end + 12)? as usize;
    let directory_offset = le_u32(archive, end + 16)? as usize;
    let disk = le_u16(archive, end + 4)?;
    let directory_disk = le_u16(archive, end + 6)?;
    let entries_on_disk = le_u16(archive, end + 8)? as usize;
    if disk != 0 || directory_disk != 0 || entries_on_disk != total_entries {
        return Err(ArchiveError::new("multi-disk archives are not supported"));
    }
    if total_entries == 0 || total_entries > MAX_ENTRIES {
        return Err(ArchiveError::new(
            "release archive has an invalid entry count",
        ));
    }
    if directory_offset == u32::MAX as usize
        || directory_size == u32::MAX as usize
        || total_entries == u16::MAX as usize
    {
        return Err(ArchiveError::new(
            "ZIP64 release archives are not supported",
        ));
    }
    if directory_offset
        .checked_add(directory_size)
        .is_none_or(|directory_end| directory_end != end)
    {
        return Err(ArchiveError::new(
            "release archive has an inconsistent directory",
        ));
    }

    let mut cursor = directory_offset;
    let mut candidate: Option<Entry> = None;
    for _ in 0..total_entries {
        let entry = read_central_entry(archive, &mut cursor, end)?;
        if entry.is_symlink() {
            return Err(ArchiveError::new(
                "release archive contains a symbolic link",
            ));
        }
        if entry.name.ends_with('/') || entry.is_directory() {
            if !entry.name_is_safe_directory() {
                return Err(ArchiveError::new(
                    "release archive has an unsafe entry name",
                ));
            }
            if entry.name.trim_end_matches('/') == "bupt"
                || entry.name.trim_end_matches('/').ends_with("/bupt")
            {
                return Err(ArchiveError::new("release archive has a bupt directory"));
            }
            continue;
        }
        if !entry.name_is_safe() {
            return Err(ArchiveError::new(
                "release archive has an unsafe entry name",
            ));
        }
        if entry.name == "bupt" || entry.name.ends_with("/bupt") {
            if candidate.is_some() {
                return Err(ArchiveError::new(
                    "release archive has more than one bupt executable",
                ));
            }
            candidate = Some(entry);
        }
    }
    if cursor != end {
        return Err(ArchiveError::new(
            "release archive has trailing directory data",
        ));
    }
    let entry =
        candidate.ok_or_else(|| ArchiveError::new("release archive has no bupt executable"))?;
    read_entry(archive, directory_offset, &entry)
}

#[derive(Clone, Debug)]
struct Entry {
    name: String,
    flags: u16,
    method: u16,
    crc: u32,
    compressed_size: usize,
    uncompressed_size: usize,
    external_attributes: u32,
    local_offset: usize,
}

impl Entry {
    fn name_is_safe(&self) -> bool {
        !self.name.is_empty()
            && self.name.len() <= MAX_NAME_BYTES
            && !self.name.starts_with('/')
            && !self.name.contains('\\')
            && !self.name.contains('\0')
            && self
                .name
                .split('/')
                .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
    }

    fn name_is_safe_directory(&self) -> bool {
        !self.name.is_empty()
            && self.name.len() <= MAX_NAME_BYTES
            && !self.name.starts_with('/')
            && !self.name.contains('\\')
            && !self.name.contains('\0')
            && self
                .name
                .trim_end_matches('/')
                .split('/')
                .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
    }

    fn is_directory(&self) -> bool {
        unix_mode(self.external_attributes) & 0o170000 == 0o040000
    }

    fn is_symlink(&self) -> bool {
        unix_mode(self.external_attributes) & 0o170000 == 0o120000
    }
}

fn unix_mode(external_attributes: u32) -> u32 {
    external_attributes >> 16
}

fn find_directory_end(archive: &[u8]) -> Result<usize, ArchiveError> {
    if archive.len() < 22 {
        return Err(ArchiveError::new("release archive is too short"));
    }
    let earliest = archive.len().saturating_sub(22 + u16::MAX as usize);
    for offset in (earliest..=archive.len() - 22).rev() {
        if le_u32(archive, offset)? != EOCD_SIGNATURE {
            continue;
        }
        let comment = le_u16(archive, offset + 20)? as usize;
        if offset + 22 + comment == archive.len() {
            return Ok(offset);
        }
    }
    Err(ArchiveError::new(
        "release archive has no end-of-directory record",
    ))
}

fn read_central_entry(
    archive: &[u8],
    cursor: &mut usize,
    end: usize,
) -> Result<Entry, ArchiveError> {
    if *cursor + 46 > end {
        return Err(ArchiveError::new("release archive directory is truncated"));
    }
    if le_u32(archive, *cursor)? != CENTRAL_SIGNATURE {
        return Err(ArchiveError::new(
            "release archive directory has a bad signature",
        ));
    }
    let flags = le_u16(archive, *cursor + 8)?;
    let method = le_u16(archive, *cursor + 10)?;
    let crc = le_u32(archive, *cursor + 16)?;
    let compressed_size = le_u32(archive, *cursor + 20)? as usize;
    let uncompressed_size = le_u32(archive, *cursor + 24)? as usize;
    let name_len = le_u16(archive, *cursor + 28)? as usize;
    let extra_len = le_u16(archive, *cursor + 30)? as usize;
    let comment_len = le_u16(archive, *cursor + 32)? as usize;
    let disk = le_u16(archive, *cursor + 34)?;
    let external_attributes = le_u32(archive, *cursor + 38)?;
    let local_offset = le_u32(archive, *cursor + 42)? as usize;
    let next = *cursor + 46 + name_len + extra_len + comment_len;
    if name_len == 0 || name_len > MAX_NAME_BYTES || next > end {
        return Err(ArchiveError::new("release archive directory is malformed"));
    }
    if disk != 0 {
        return Err(ArchiveError::new("multi-disk archives are not supported"));
    }
    if flags & 0x0001 != 0 || flags & 0x0040 != 0 || flags & 0x2000 != 0 {
        return Err(ArchiveError::new(
            "encrypted release archives are not supported",
        ));
    }
    if !matches!(method, STORED | DEFLATED) {
        return Err(ArchiveError::new(
            "release archive uses an unsupported compression",
        ));
    }
    if compressed_size == u32::MAX as usize
        || uncompressed_size == u32::MAX as usize
        || local_offset == u32::MAX as usize
    {
        return Err(ArchiveError::new(
            "ZIP64 release archives are not supported",
        ));
    }
    if uncompressed_size > MAX_BINARY_BYTES || compressed_size > MAX_ARCHIVE_BYTES {
        return Err(ArchiveError::new("release archive entry is too large"));
    }
    if local_offset >= end {
        return Err(ArchiveError::new(
            "release archive entry has an invalid offset",
        ));
    }
    let name = std::str::from_utf8(&archive[*cursor + 46..*cursor + 46 + name_len])
        .map_err(|_| ArchiveError::new("release archive entry name is not UTF-8"))?
        .to_owned();
    *cursor = next;
    Ok(Entry {
        name,
        flags,
        method,
        crc,
        compressed_size,
        uncompressed_size,
        external_attributes,
        local_offset,
    })
}

fn read_entry(
    archive: &[u8],
    directory_offset: usize,
    entry: &Entry,
) -> Result<Vec<u8>, ArchiveError> {
    let header = entry.local_offset;
    if header + 30 > archive.len() {
        return Err(ArchiveError::new(
            "release archive local header is truncated",
        ));
    }
    if le_u32(archive, header)? != LOCAL_SIGNATURE {
        return Err(ArchiveError::new(
            "release archive local header has a bad signature",
        ));
    }
    let local_flags = le_u16(archive, header + 6)?;
    let local_method = le_u16(archive, header + 8)?;
    let local_crc = le_u32(archive, header + 14)?;
    let local_compressed_size = le_u32(archive, header + 18)? as usize;
    let local_uncompressed_size = le_u32(archive, header + 22)? as usize;
    let name_len = le_u16(archive, header + 26)? as usize;
    let extra_len = le_u16(archive, header + 28)? as usize;
    let data_start = header + 30 + name_len + extra_len;
    let data_end = data_start
        .checked_add(entry.compressed_size)
        .ok_or_else(|| ArchiveError::new("release archive entry is too large"))?;
    if data_end > directory_offset {
        return Err(ArchiveError::new(
            "release archive data overlaps its directory",
        ));
    }
    if local_flags != entry.flags || local_method != entry.method {
        return Err(ArchiveError::new(
            "release archive local header disagrees with the directory",
        ));
    }
    if name_len != entry.name.len()
        || &archive[header + 30..header + 30 + name_len] != entry.name.as_bytes()
    {
        return Err(ArchiveError::new(
            "release archive entry name disagrees with the directory",
        ));
    }
    if entry.flags & 0x0008 == 0
        && (local_crc != entry.crc
            || local_compressed_size != entry.compressed_size
            || local_uncompressed_size != entry.uncompressed_size)
    {
        return Err(ArchiveError::new(
            "release archive local header disagrees with the directory",
        ));
    }

    let compressed = &archive[data_start..data_end];
    let output = if entry.method == STORED {
        if entry.compressed_size != entry.uncompressed_size {
            return Err(ArchiveError::new(
                "stored release entry has mismatched sizes",
            ));
        }
        compressed.to_vec()
    } else {
        inflate(compressed, entry.uncompressed_size)?
    };
    if output.len() != entry.uncompressed_size {
        return Err(ArchiveError::new(
            "release archive entry has an unexpected size",
        ));
    }
    if crc32(&output) != entry.crc {
        return Err(ArchiveError::new(
            "release archive entry failed its CRC check",
        ));
    }
    Ok(output)
}

fn le_u16(bytes: &[u8], offset: usize) -> Result<u16, ArchiveError> {
    let slice = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| ArchiveError::new("release archive is truncated"))?;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

fn le_u32(bytes: &[u8], offset: usize) -> Result<u32, ArchiveError> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| ArchiveError::new("release archive is truncated"))?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

struct BitReader<'a> {
    data: &'a [u8],
    position: usize,
    buffer: u64,
    bits: u8,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            position: 0,
            buffer: 0,
            bits: 0,
        }
    }

    fn read(&mut self, count: u8) -> Result<u32, ArchiveError> {
        if count > 16 {
            return Err(ArchiveError::new("invalid DEFLATE bit count"));
        }
        while self.bits < count {
            let byte = *self
                .data
                .get(self.position)
                .ok_or_else(|| ArchiveError::new("DEFLATE data is truncated"))?;
            self.buffer |= u64::from(byte) << self.bits;
            self.bits += 8;
            self.position += 1;
        }
        let value = if count == 0 {
            0
        } else {
            (self.buffer & ((1u64 << count) - 1)) as u32
        };
        self.buffer >>= count;
        self.bits -= count;
        Ok(value)
    }

    fn align(&mut self) {
        let discard = self.bits % 8;
        self.buffer >>= discard;
        self.bits -= discard;
    }
}

#[derive(Clone, Debug)]
struct Huffman {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Result<Self, ArchiveError> {
        let mut counts = [0u16; 16];
        for &length in lengths {
            if length > 15 {
                return Err(ArchiveError::new("invalid DEFLATE code length"));
            }
            counts[length as usize] += 1;
        }
        let mut remaining: i32 = 1;
        for count in counts.iter().skip(1) {
            remaining = (remaining << 1) - i32::from(*count);
            if remaining < 0 {
                return Err(ArchiveError::new("over-subscribed DEFLATE Huffman tree"));
            }
        }
        let mut offsets = [0u16; 16];
        for length in 1..15 {
            offsets[length + 1] = offsets[length] + counts[length];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (symbol, &length) in lengths.iter().enumerate() {
            if length != 0 {
                let offset = offsets[length as usize] as usize;
                if offset >= symbols.len() {
                    return Err(ArchiveError::new("invalid DEFLATE Huffman tree"));
                }
                symbols[offset] = symbol as u16;
                offsets[length as usize] += 1;
            }
        }
        Ok(Self { counts, symbols })
    }

    fn decode(&self, reader: &mut BitReader<'_>) -> Result<u16, ArchiveError> {
        if self.symbols.is_empty() {
            return Err(ArchiveError::new("missing DEFLATE Huffman tree"));
        }
        let mut code: u32 = 0;
        let mut first: u32 = 0;
        let mut index: usize = 0;
        for length in 1..=15 {
            code = (code << 1) | reader.read(1)?;
            let count = u32::from(self.counts[length]);
            if code >= first && code - first < count {
                let symbol = index + (code - first) as usize;
                return self
                    .symbols
                    .get(symbol)
                    .copied()
                    .ok_or_else(|| ArchiveError::new("invalid DEFLATE Huffman symbol"));
            }
            index += count as usize;
            first = (first + count) << 1;
        }
        Err(ArchiveError::new("invalid DEFLATE Huffman code"))
    }
}

fn inflate(compressed: &[u8], expected: usize) -> Result<Vec<u8>, ArchiveError> {
    const LENGTH_BASE: [u16; 29] = [
        3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115,
        131, 163, 195, 227, 258,
    ];
    const LENGTH_EXTRA: [u8; 29] = [
        0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
    ];
    const DISTANCE_BASE: [u16; 30] = [
        1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
        2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
    ];
    const DISTANCE_EXTRA: [u8; 30] = [
        0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12,
        13, 13,
    ];

    let mut reader = BitReader::new(compressed);
    let mut output = Vec::with_capacity(expected.min(MAX_BINARY_BYTES));
    loop {
        let last = reader.read(1)? == 1;
        let kind = reader.read(2)?;
        match kind {
            0 => {
                reader.align();
                let length = reader.read(16)? as u16;
                let complement = reader.read(16)? as u16;
                if length != !complement {
                    return Err(ArchiveError::new("invalid stored DEFLATE block"));
                }
                let length = length as usize;
                if output.len() + length > expected {
                    return Err(ArchiveError::new(
                        "DEFLATE output exceeds its declared size",
                    ));
                }
                for _ in 0..length {
                    output.push(reader.read(8)? as u8);
                }
            }
            1 => {
                let mut lengths = [0u8; 288];
                lengths[..144].fill(8);
                lengths[144..256].fill(9);
                lengths[256..280].fill(7);
                lengths[280..].fill(8);
                let literal = Huffman::new(&lengths)?;
                let distance = Huffman::new(&[5u8; 32])?;
                inflate_block(
                    &mut reader,
                    &literal,
                    &distance,
                    &mut output,
                    expected,
                    &LENGTH_BASE,
                    &LENGTH_EXTRA,
                    &DISTANCE_BASE,
                    &DISTANCE_EXTRA,
                )?;
            }
            2 => {
                let literal_count = reader.read(5)? as usize + 257;
                let distance_count = reader.read(5)? as usize + 1;
                let code_count = reader.read(4)? as usize + 4;
                if literal_count > 286 || distance_count > 32 {
                    return Err(ArchiveError::new("invalid dynamic DEFLATE counts"));
                }
                const ORDER: [usize; 19] = [
                    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
                ];
                let mut code_lengths = [0u8; 19];
                for &index in ORDER.iter().take(code_count) {
                    code_lengths[index] = reader.read(3)? as u8;
                }
                let code_tree = Huffman::new(&code_lengths)?;
                let total = literal_count + distance_count;
                let mut lengths = vec![0u8; total];
                let mut index = 0;
                while index < total {
                    match code_tree.decode(&mut reader)? {
                        value @ 0..=15 => {
                            lengths[index] = value as u8;
                            index += 1;
                        }
                        16 => {
                            let previous = *lengths
                                .get(index.wrapping_sub(1))
                                .ok_or_else(|| ArchiveError::new("invalid DEFLATE repeat"))?;
                            let repeat = reader.read(2)? as usize + 3;
                            if index + repeat > total {
                                return Err(ArchiveError::new("invalid DEFLATE repeat"));
                            }
                            lengths[index..index + repeat].fill(previous);
                            index += repeat;
                        }
                        17 => {
                            let repeat = reader.read(3)? as usize + 3;
                            if index + repeat > total {
                                return Err(ArchiveError::new("invalid DEFLATE repeat"));
                            }
                            lengths[index..index + repeat].fill(0);
                            index += repeat;
                        }
                        18 => {
                            let repeat = reader.read(7)? as usize + 11;
                            if index + repeat > total {
                                return Err(ArchiveError::new("invalid DEFLATE repeat"));
                            }
                            lengths[index..index + repeat].fill(0);
                            index += repeat;
                        }
                        _ => return Err(ArchiveError::new("invalid DEFLATE code length")),
                    }
                }
                if lengths[256] == 0 {
                    return Err(ArchiveError::new("DEFLATE block has no end marker"));
                }
                let literal = Huffman::new(&lengths[..literal_count])?;
                let distance = Huffman::new(&lengths[literal_count..])?;
                inflate_block(
                    &mut reader,
                    &literal,
                    &distance,
                    &mut output,
                    expected,
                    &LENGTH_BASE,
                    &LENGTH_EXTRA,
                    &DISTANCE_BASE,
                    &DISTANCE_EXTRA,
                )?;
            }
            _ => return Err(ArchiveError::new("invalid DEFLATE block type")),
        }
        if last {
            break;
        }
    }
    if output.len() != expected {
        return Err(ArchiveError::new("DEFLATE output has an unexpected size"));
    }
    Ok(output)
}

#[allow(clippy::too_many_arguments)]
fn inflate_block(
    reader: &mut BitReader<'_>,
    literal: &Huffman,
    distance: &Huffman,
    output: &mut Vec<u8>,
    expected: usize,
    length_base: &[u16; 29],
    length_extra: &[u8; 29],
    distance_base: &[u16; 30],
    distance_extra: &[u8; 30],
) -> Result<(), ArchiveError> {
    loop {
        match literal.decode(reader)? {
            value @ 0..=255 => {
                if output.len() >= expected {
                    return Err(ArchiveError::new(
                        "DEFLATE output exceeds its declared size",
                    ));
                }
                output.push(value as u8);
            }
            256 => return Ok(()),
            value @ 257..=285 => {
                let index = (value - 257) as usize;
                let length =
                    usize::from(length_base[index]) + reader.read(length_extra[index])? as usize;
                let symbol = distance.decode(reader)? as usize;
                if symbol >= distance_base.len() {
                    return Err(ArchiveError::new("invalid DEFLATE distance code"));
                }
                let back = usize::from(distance_base[symbol])
                    + reader.read(distance_extra[symbol])? as usize;
                if back == 0 || back > output.len() || output.len() + length > expected {
                    return Err(ArchiveError::new("invalid DEFLATE back-reference"));
                }
                for _ in 0..length {
                    let byte = output[output.len() - back];
                    output.push(byte);
                }
            }
            _ => return Err(ArchiveError::new("invalid DEFLATE length code")),
        }
    }
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (index, slot) in table.iter_mut().enumerate() {
        let mut value = index as u32;
        for _ in 0..8 {
            value = if value & 1 != 0 {
                0xedb8_8320 ^ (value >> 1)
            } else {
                value >> 1
            };
        }
        *slot = value;
    }
    let mut crc = 0xffff_ffffu32;
    for &byte in bytes {
        crc = table[((crc ^ u32::from(byte)) & 0xff) as usize] ^ (crc >> 8);
    }
    !crc
}

#[cfg(test)]
fn test_zip_entry(
    name: &str,
    method: u16,
    compressed: &[u8],
    uncompressed: &[u8],
    crc: u32,
    external_attributes: u32,
) -> Vec<u8> {
    let name = name.as_bytes();
    let mut local = Vec::new();
    local.extend(LOCAL_SIGNATURE.to_le_bytes());
    local.extend(20u16.to_le_bytes());
    local.extend(0u16.to_le_bytes());
    local.extend(method.to_le_bytes());
    local.extend(0u16.to_le_bytes());
    local.extend(0u16.to_le_bytes());
    local.extend(crc.to_le_bytes());
    local.extend((compressed.len() as u32).to_le_bytes());
    local.extend((uncompressed.len() as u32).to_le_bytes());
    local.extend((name.len() as u16).to_le_bytes());
    local.extend(0u16.to_le_bytes());
    local.extend_from_slice(name);
    local.extend_from_slice(compressed);

    let directory_offset = local.len();
    let mut directory = Vec::new();
    directory.extend(CENTRAL_SIGNATURE.to_le_bytes());
    directory.extend(20u16.to_le_bytes());
    directory.extend(20u16.to_le_bytes());
    directory.extend(0u16.to_le_bytes());
    directory.extend(method.to_le_bytes());
    directory.extend(0u16.to_le_bytes());
    directory.extend(0u16.to_le_bytes());
    directory.extend(crc.to_le_bytes());
    directory.extend((compressed.len() as u32).to_le_bytes());
    directory.extend((uncompressed.len() as u32).to_le_bytes());
    directory.extend((name.len() as u16).to_le_bytes());
    directory.extend(0u16.to_le_bytes());
    directory.extend(0u16.to_le_bytes());
    directory.extend(0u16.to_le_bytes());
    directory.extend(0u16.to_le_bytes());
    directory.extend(external_attributes.to_le_bytes());
    directory.extend(0u32.to_le_bytes());
    directory.extend_from_slice(name);
    let directory_size = directory.len();

    let mut archive = local;
    archive.extend_from_slice(&directory);
    archive.extend(EOCD_SIGNATURE.to_le_bytes());
    archive.extend(0u16.to_le_bytes());
    archive.extend(0u16.to_le_bytes());
    archive.extend(1u16.to_le_bytes());
    archive.extend(1u16.to_le_bytes());
    archive.extend((directory_size as u32).to_le_bytes());
    archive.extend((directory_offset as u32).to_le_bytes());
    archive.extend(0u16.to_le_bytes());
    archive
}

#[cfg(test)]
pub(super) fn test_stored_zip(name: &str, payload: &[u8]) -> Vec<u8> {
    test_zip_entry(
        name,
        STORED,
        payload,
        payload,
        crc32(payload),
        0o100755 << 16,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deflate_hex(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).expect("hex"))
            .collect()
    }

    #[test]
    fn stored_entry_is_extracted() {
        let payload = b"a synthetic bupt binary";
        let archive = test_zip_entry(
            "bupt",
            STORED,
            payload,
            payload,
            crc32(payload),
            0o100755 << 16,
        );
        assert_eq!(extract_bupt(&archive).expect("extract stored"), payload);
    }

    #[test]
    fn prefixed_entry_is_extracted() {
        let payload = b"a synthetic bupt binary";
        let archive = test_zip_entry(
            "release/bupt",
            STORED,
            payload,
            payload,
            crc32(payload),
            0o100755 << 16,
        );
        assert_eq!(extract_bupt(&archive).expect("extract prefixed"), payload);
    }

    #[test]
    fn deflated_fixed_and_dynamic_blocks_are_extracted() {
        let fixed_payload = b"hello hello hello hello hello hello";
        let fixed = deflate_hex("cb48cdc9c957c8c0470200");
        let archive = test_zip_entry(
            "bupt",
            DEFLATED,
            &fixed,
            fixed_payload,
            crc32(fixed_payload),
            0o100755 << 16,
        );
        assert_eq!(
            extract_bupt(&archive).expect("extract fixed"),
            fixed_payload
        );

        let dynamic_payload: Vec<u8> = (0..4).flat_map(|_| 0u8..=255).collect();
        let dynamic = deflate_hex(
            "6360646266616563e7e0e4e2e6e1e5e3171014121611151397909492969195935750545256515553d7d0d4d2d6d1d5d33730343236313533b7b0b4b2b6b1b5b37770747276717573f7f0f4f2f6f1f5f30f080c0a0e090d0b8f888c8a8e898d8b4f484c4a4e494d4bcfc8cccacec9cdcb2f282c2a2e292d2bafa8acaaaea9adab6f686c6a6e696d6befe8eceaeee9edeb9f3071d2e42953a74d9f3173d6ec3973e7cd5fb070d1e2254b972d5fb172d5ea356bd7addfb071d3e62d5bb76ddfb173d7ee3d7bf7ed3f70f0d0e123478f1d3f71f2d4e93367cf9dbf70f1d2e52b57af5dbf71f3d6ed3b77efdd7ff0f0d1e3274f9f3d7ff1f2d5eb376fdfbdfff0f1d3e72f5fbf7dfff1f3d7ef3f7ffffd6718f5ffa8ff47b0ff01",
        );
        let archive = test_zip_entry(
            "bupt",
            DEFLATED,
            &dynamic,
            &dynamic_payload,
            crc32(&dynamic_payload),
            0o100755 << 16,
        );
        assert_eq!(
            extract_bupt(&archive).expect("extract dynamic"),
            dynamic_payload
        );

        let stored_payload = b"stored block payloadstored block payloadstored block payload";
        let stored = deflate_hex(
            "013c00c3ff73746f72656420626c6f636b207061796c6f616473746f72656420626c6f636b207061796c6f616473746f72656420626c6f636b207061796c6f6164",
        );
        let archive = test_zip_entry(
            "bupt",
            DEFLATED,
            &stored,
            stored_payload,
            crc32(stored_payload),
            0o100755 << 16,
        );
        assert_eq!(
            extract_bupt(&archive).expect("extract stored deflate block"),
            stored_payload
        );
    }

    #[test]
    fn unsafe_or_broken_archives_are_rejected() {
        let payload = b"a synthetic bupt binary";
        let crc = crc32(payload);

        let traversal = test_zip_entry("../bupt", STORED, payload, payload, crc, 0o100755 << 16);
        assert!(extract_bupt(&traversal).is_err());

        let absolute = test_zip_entry("/bupt", STORED, payload, payload, crc, 0o100755 << 16);
        assert!(extract_bupt(&absolute).is_err());

        let symlink = test_zip_entry("bupt", STORED, payload, payload, crc, 0o120777 << 16);
        assert!(extract_bupt(&symlink).is_err());

        let bad_crc = test_zip_entry("bupt", STORED, payload, payload, crc ^ 1, 0o100755 << 16);
        assert!(extract_bupt(&bad_crc).is_err());

        let unsupported = test_zip_entry("bupt", 12, payload, payload, crc, 0o100755 << 16);
        assert!(extract_bupt(&unsupported).is_err());

        let mut truncated = test_zip_entry("bupt", STORED, payload, payload, crc, 0o100755 << 16);
        truncated.truncate(truncated.len() - 1);
        assert!(extract_bupt(&truncated).is_err());

        let no_binary = test_zip_entry("README", STORED, payload, payload, crc, 0o100644 << 16);
        assert!(extract_bupt(&no_binary).is_err());
    }

    #[test]
    fn truncated_and_mutated_archives_never_panic() {
        let archive = test_stored_zip("bupt", b"native-payload");
        for end in 0..archive.len() {
            let _ = extract_bupt(&archive[..end]);
        }
        for index in 0..archive.len() {
            let mut mutated = archive.clone();
            mutated[index] ^= 0xff;
            let _ = extract_bupt(&mutated);
        }
    }

    #[test]
    fn archive_rejects_duplicate_targets() {
        let payload = b"a synthetic bupt binary";
        let crc = crc32(payload);
        let first = test_zip_entry("bupt", STORED, payload, payload, crc, 0o100755 << 16);
        let second = test_zip_entry("bupt", STORED, payload, payload, crc, 0o100755 << 16);
        // Keep the first archive's local entry and central directory, then add
        // the second entry as a duplicate directory record.
        let directory_end = first.len() - 22;
        let directory_offset = u32::from_le_bytes(
            first[directory_end - 20..directory_end - 16]
                .try_into()
                .unwrap(),
        ) as usize;
        let mut archive = first[..directory_end].to_vec();
        archive.extend_from_slice(&second[second.len() - 22 - 46 - 5..second.len() - 22]);
        let directory_size = archive.len() - directory_offset;
        archive.extend(EOCD_SIGNATURE.to_le_bytes());
        archive.extend(0u16.to_le_bytes());
        archive.extend(0u16.to_le_bytes());
        archive.extend(2u16.to_le_bytes());
        archive.extend(2u16.to_le_bytes());
        archive.extend((directory_size as u32).to_le_bytes());
        archive.extend((directory_offset as u32).to_le_bytes());
        archive.extend(0u16.to_le_bytes());
        assert!(extract_bupt(&archive).is_err());
    }
}
