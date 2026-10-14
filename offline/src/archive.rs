//! Read-only reader for Emil Chronicle Online HED/DAT archives.
//!
//! Experimental research reader for ECO HED/DAT archives. The HED layout
//! and codec framing follow the public `coremaze/AECO-Archive` format
//! description. This standalone decoder applies the source's 100-byte phrase
//! update limit and dictionary recycling; selected-entry reads
//! let tools inspect supported blocks without decoding unrelated entries.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

const HED_ENTRY_SIZE: usize = 12;
const MAX_HED_BYTES: usize = 64 * 1024 * 1024;
const MAX_DAT_BYTES: usize = 1024 * 1024 * 1024;
const MAX_ENTRIES: usize = 500_000;
const MAX_BLOCK_BYTES: usize = 256 * 1024 * 1024;
const MAX_TOTAL_OUTPUT_BYTES: usize = 512 * 1024 * 1024;
const FIRST_PHRASE_CODE: usize = 256;
const END_CODE: usize = 8192;
const DICTIONARY_CAPACITY: usize = END_CODE;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
struct HedEntry {
    offset: u32,
    packed: bool,
    packed_len: u32,
    unpacked_len: u32,
}

impl HedEntry {
    fn is_end_marker(self) -> bool {
        !self.packed && self.offset == 0 && self.packed_len == 0 && self.unpacked_len == 0
    }
}

/// Reads a HED/DAT pair and decompresses every entry in memory.
///
/// This experimental API supports bounded phrase streams with dictionary
/// recycling. Unsupported streams return errors. This API never writes into
/// either source file; file, index, per-block, and total output sizes are
/// bounded. Archive names are validated as flat names.
pub fn read_archive(hed_path: &Path, dat_path: &Path) -> io::Result<Vec<Entry>> {
    let (hed_entries, had_end_marker) = read_hed(hed_path)?;
    if !had_end_marker && hed_entries.is_empty() {
        return Err(invalid("HED file has no entries"));
    }
    let dat = read_bounded(dat_path, MAX_DAT_BYTES, "DAT")?;
    let (names_data, names) = read_archive_names(&dat, &hed_entries)?;

    let mut entries = Vec::with_capacity(names.len());
    let mut total_output = names_data.len();
    for (name, record) in names.into_iter().zip(hed_entries.into_iter().skip(1)) {
        let next_total = total_output
            .checked_add(record.unpacked_len as usize)
            .filter(|&total| total <= MAX_TOTAL_OUTPUT_BYTES)
            .ok_or_else(|| invalid("archive exceeds the configured aggregate output limit"))?;
        let data = read_block(&dat, record)?;
        total_output = next_total;
        entries.push(Entry { name, data });
    }
    Ok(entries)
}

/// Reads one entry by its one-based ordinal, without decoding unrelated blocks.
/// This supports inspection of archives containing codec variants this reader
/// does not yet handle; it does not imply that the whole archive was decoded.
pub fn read_archive_entry(hed_path: &Path, dat_path: &Path, ordinal: usize) -> io::Result<Entry> {
    if ordinal == 0 {
        return Err(invalid("archive entry ordinal must be one-based"));
    }
    let (hed_entries, had_end_marker) = read_hed(hed_path)?;
    if !had_end_marker && hed_entries.is_empty() {
        return Err(invalid("HED file has no entries"));
    }
    let dat = read_bounded(dat_path, MAX_DAT_BYTES, "DAT")?;
    let (names_data, names) = read_archive_names(&dat, &hed_entries)?;
    let index = ordinal - 1;
    let name = names
        .get(index)
        .ok_or_else(|| invalid("archive entry ordinal is outside the filename list"))?;
    let record = *hed_entries
        .get(index + 1)
        .ok_or_else(|| invalid("HED entry count does not match filename count"))?;
    let _total = names_data
        .len()
        .checked_add(record.unpacked_len as usize)
        .filter(|&total| total <= MAX_TOTAL_OUTPUT_BYTES)
        .ok_or_else(|| invalid("archive exceeds the configured aggregate output limit"))?;
    Ok(Entry {
        name: name.clone(),
        data: read_block(&dat, record)?,
    })
}

fn read_hed(hed_path: &Path) -> io::Result<(Vec<HedEntry>, bool)> {
    let hed_bytes = read_bounded(hed_path, MAX_HED_BYTES, "HED")?;
    if hed_bytes.len() % HED_ENTRY_SIZE != 0 {
        return Err(invalid("HED file ends with a partial 12-byte entry"));
    }
    parse_hed(&hed_bytes)
}

fn read_archive_names(dat: &[u8], hed_entries: &[HedEntry]) -> io::Result<(Vec<u8>, Vec<String>)> {
    let name_entry = hed_entries
        .first()
        .copied()
        .ok_or_else(|| invalid("HED file has no filename-list entry"))?;
    let names_data = read_block(&dat, name_entry)?;
    let names = parse_names(&names_data)?;

    if names.len() > MAX_ENTRIES {
        return Err(invalid(
            "archive filename count exceeds the configured limit",
        ));
    }
    if hed_entries.len() != names.len() + 1 {
        return Err(invalid("HED entry count does not match filename count"));
    }
    for name in &names {
        validate_name(name)?;
    }
    Ok((names_data, names))
}

fn read_bounded(path: &Path, limit: usize, label: &'static str) -> io::Result<Vec<u8>> {
    let file = File::open(path)?;
    let max_plus_one = limit as u64 + 1;
    let mut bytes = Vec::new();
    file.take(max_plus_one).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(invalid(match label {
            "HED" => "HED file exceeds the configured size limit",
            _ => "DAT file exceeds the configured size limit",
        }));
    }
    Ok(bytes)
}

fn parse_hed(bytes: &[u8]) -> io::Result<(Vec<HedEntry>, bool)> {
    if bytes.len() % HED_ENTRY_SIZE != 0 {
        return Err(invalid("HED file ends with a partial 12-byte entry"));
    }
    let mut entries = Vec::with_capacity(bytes.len() / HED_ENTRY_SIZE);
    let mut ended = false;
    for chunk in bytes.chunks_exact(HED_ENTRY_SIZE) {
        let offset = u32::from_le_bytes(chunk[0..4].try_into().unwrap());
        let packed_word = u32::from_le_bytes(chunk[4..8].try_into().unwrap());
        let unpacked_len = u32::from_le_bytes(chunk[8..12].try_into().unwrap());
        let entry = HedEntry {
            offset,
            packed: packed_word & 0x8000_0000 != 0,
            packed_len: packed_word & 0x7fff_ffff,
            unpacked_len,
        };
        if entry.is_end_marker() {
            ended = true;
            break;
        }
        if entries.len() == MAX_ENTRIES + 1 {
            return Err(invalid("HED entry count exceeds the configured limit"));
        }
        entries.push(entry);
    }
    Ok((entries, ended))
}

fn read_block(dat: &[u8], entry: HedEntry) -> io::Result<Vec<u8>> {
    let start = entry.offset as usize;
    let packed_len = entry.packed_len as usize;
    let end = start
        .checked_add(packed_len)
        .filter(|&end| end <= dat.len())
        .ok_or_else(|| invalid("HED block lies outside the DAT file"))?;
    let unpacked_len = entry.unpacked_len as usize;
    if unpacked_len > MAX_BLOCK_BYTES {
        return Err(invalid("DAT block exceeds the configured output limit"));
    }
    let packed = &dat[start..end];
    if entry.packed {
        decode_phrase_stream(packed, unpacked_len)
    } else {
        if packed.len() != unpacked_len {
            return Err(invalid("unpacked HED block has inconsistent lengths"));
        }
        Ok(packed.to_vec())
    }
}

fn parse_names(bytes: &[u8]) -> io::Result<Vec<String>> {
    if bytes.len() < 5 {
        return Err(invalid(
            "filename list is shorter than its count and terminator",
        ));
    }
    let count = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
    if count > MAX_ENTRIES {
        return Err(invalid("filename count exceeds the configured limit"));
    }
    let mut cursor = 4;
    let mut names = Vec::with_capacity(count);
    for _ in 0..count {
        let tail = bytes
            .get(cursor..)
            .ok_or_else(|| invalid("truncated filename list"))?;
        let len = tail
            .iter()
            .position(|&byte| byte == 0)
            .ok_or_else(|| invalid("unterminated filename"))?;
        if !tail[..len].is_ascii() {
            return Err(invalid("filename list contains a non-ASCII name"));
        }
        let name = std::str::from_utf8(&tail[..len])
            .map_err(|_| invalid("filename list contains invalid ASCII"))?
            .to_owned();
        names.push(name);
        cursor += len + 1;
    }
    if bytes.get(cursor) != Some(&0) || cursor + 1 != bytes.len() {
        return Err(invalid("filename list lacks its single trailing NUL"));
    }
    Ok(names)
}

fn validate_name(name: &str) -> io::Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name
            .as_bytes()
            .iter()
            .any(|&byte| byte < 0x20 || byte == 0x7f)
    {
        return Err(invalid("archive contains an unsafe or empty filename"));
    }
    Ok(())
}

/// Decodes the tested MSB-first literal/phrase stream subset.
fn decode_phrase_stream(input: &[u8], expected_len: usize) -> io::Result<Vec<u8>> {
    if expected_len > MAX_BLOCK_BYTES {
        return Err(invalid(
            "compressed block exceeds the configured output limit",
        ));
    }
    let mut bits = BitReader::new(input);
    let mut parents = vec![END_CODE; DICTIONARY_CAPACITY];
    let mut suffixes = vec![0u8; DICTIONARY_CAPACITY];
    let mut children = HashMap::<(usize, u8), usize>::new();
    let mut list_prev = vec![END_CODE; DICTIONARY_CAPACITY];
    let mut list_next = vec![END_CODE; DICTIONARY_CAPACITY];
    let mut list_head = END_CODE;
    let mut list_tail = END_CODE;
    for code in 0..256 {
        suffixes[code] = code as u8;
        children.insert((END_CODE, code as u8), code);
    }
    let mut next_code = FIRST_PHRASE_CODE;
    let mut code_width = 1usize;
    let mut previous: Option<usize> = None;
    let mut previous_phrase_len = 0usize;
    let mut output = Vec::with_capacity(expected_len);

    while output.len() < expected_len {
        while code_width < 13
            && next_code.saturating_sub(FIRST_PHRASE_CODE) >= (1usize << code_width)
        {
            code_width += 1;
        }
        let code = if bits.read_bit()? == 0 {
            bits.read_bits(8)? as usize
        } else {
            FIRST_PHRASE_CODE + bits.read_bits(code_width)? as usize
        };
        if code == END_CODE || code >= next_code {
            return Err(invalid(
                "compressed stream contains an unsupported or malformed phrase code",
            ));
        }
        let current = expand_phrase(code, &parents, &suffixes)?;
        if output.len().saturating_add(current.len()) > expected_len {
            return Err(invalid("compressed stream exceeds the HED output length"));
        }
        output.extend_from_slice(&current);

        // Phrase codes are touched in code-to-root order, as in the source.
        let mut touched = code;
        while touched >= FIRST_PHRASE_CODE {
            let parent = parents[touched];
            let before = list_prev[touched];
            let after = list_next[touched];
            if before == END_CODE {
                list_head = after;
            } else {
                list_next[before] = after;
            }
            if after == END_CODE {
                list_tail = before;
            } else {
                list_prev[after] = before;
            }
            list_prev[touched] = list_tail;
            list_next[touched] = END_CODE;
            if list_tail == END_CODE {
                list_head = touched;
            } else {
                list_next[list_tail] = touched;
            }
            list_tail = touched;
            touched = parent;
        }

        if let Some(previous_code) = previous {
            let mut parent = previous_code;
            // The reference decoder limits each combined phrase path to 100 bytes.
            for &byte in current
                .iter()
                .take(100usize.saturating_sub(previous_phrase_len))
            {
                if let Some(&child) = children.get(&(parent, byte)) {
                    parent = child;
                    continue;
                }
                let insertion_parent = parent;
                let child = if next_code < DICTIONARY_CAPACITY {
                    let child = next_code;
                    next_code += 1;
                    child
                } else {
                    let child = list_head;
                    if child == parent {
                        break;
                    }
                    // Descendants precede ancestors in the replacement order, so the head is a leaf.
                    let old_parent = parents[child];
                    children.remove(&(old_parent, suffixes[child]));
                    list_head = list_next[child];
                    list_prev[list_head] = END_CODE;
                    child
                };
                parents[child] = parent;
                suffixes[child] = byte;
                children.insert((parent, byte), child);
                parent = child;
                if insertion_parent >= FIRST_PHRASE_CODE {
                    let before = list_prev[insertion_parent];
                    list_prev[child] = before;
                    list_next[child] = insertion_parent;
                    list_prev[insertion_parent] = child;
                    if before == END_CODE {
                        list_head = child;
                    } else {
                        list_next[before] = child;
                    }
                } else {
                    list_prev[child] = list_tail;
                    list_next[child] = END_CODE;
                    if list_tail == END_CODE {
                        list_head = child;
                    } else {
                        list_next[list_tail] = child;
                    }
                    list_tail = child;
                }
            }
        }
        previous = Some(code);
        previous_phrase_len = current.len();
    }
    Ok(output)
}

fn expand_phrase(code: usize, parents: &[usize], suffixes: &[u8]) -> io::Result<Vec<u8>> {
    let mut phrase = Vec::new();
    let mut cursor = code;
    let mut links = 0usize;
    while cursor != END_CODE {
        let byte = *suffixes
            .get(cursor)
            .ok_or_else(|| invalid("phrase parent link is outside the dictionary"))?;
        phrase.push(byte);
        cursor = *parents
            .get(cursor)
            .ok_or_else(|| invalid("phrase parent link is outside the dictionary"))?;
        links += 1;
        if links > DICTIONARY_CAPACITY {
            return Err(invalid("cyclic phrase parent links"));
        }
    }
    phrase.reverse();
    Ok(phrase)
}

struct BitReader<'a> {
    bytes: &'a [u8],
    bit_offset: usize,
}

impl<'a> BitReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            bit_offset: 0,
        }
    }

    fn read_bit(&mut self) -> io::Result<u8> {
        let byte = *self
            .bytes
            .get(self.bit_offset / 8)
            .ok_or_else(|| invalid("compressed bitstream ended before its end code"))?;
        let bit = (byte >> (7 - (self.bit_offset % 8))) & 1;
        self.bit_offset += 1;
        Ok(bit)
    }

    fn read_bits(&mut self, count: usize) -> io::Result<u32> {
        let mut value = 0u32;
        for _ in 0..count {
            value = (value << 1) | u32::from(self.read_bit()?);
        }
        Ok(value)
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TestArchive(std::path::PathBuf);

    impl TestArchive {
        fn new() -> (Self, std::path::PathBuf, std::path::PathBuf) {
            static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
            let directory = std::env::temp_dir().join(format!(
                "eco-archive-test-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&directory).unwrap();
            let hed_path = directory.join("test.hed");
            let dat_path = directory.join("test.dat");
            (Self(directory), hed_path, dat_path)
        }
    }

    impl Drop for TestArchive {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn append_hed_entry(
        bytes: &mut Vec<u8>,
        offset: u32,
        packed_len: u32,
        unpacked_len: u32,
        packed: bool,
    ) {
        bytes.extend_from_slice(&offset.to_le_bytes());
        let packed_word = packed_len | if packed { 0x8000_0000 } else { 0 };
        bytes.extend_from_slice(&packed_word.to_le_bytes());
        bytes.extend_from_slice(&unpacked_len.to_le_bytes());
    }

    fn encode_short_codes(codes: &[(bool, u32, usize)]) -> Vec<u8> {
        let mut bits = Vec::new();
        for &(phrase, value, width) in codes {
            bits.push(u8::from(phrase));
            for bit in (0..if phrase { width } else { 8 }).rev() {
                bits.push(((value >> bit) & 1) as u8);
            }
        }
        let mut bytes = vec![0; bits.len().div_ceil(8)];
        for (i, bit) in bits.into_iter().enumerate() {
            bytes[i / 8] |= bit << (7 - i % 8);
        }
        bytes
    }

    #[test]
    fn decodes_literals_and_lzw_phrase_growth() {
        // Literal A, literal B, then phrase 256 => "AB". HED carries output length.
        let stream = encode_short_codes(&[
            (false, b'A' as u32, 0),
            (false, b'B' as u32, 0),
            (true, 0, 1),
        ]);
        assert_eq!(decode_phrase_stream(&stream, 4).unwrap(), b"ABAB");
    }

    #[test]
    fn grows_phrase_code_width_at_dictionary_boundary() {
        // Codes 256 and 257 require one and then two payload bits.
        let stream = encode_short_codes(&[
            (false, b'A' as u32, 0),
            (false, b'B' as u32, 0),
            (true, 0, 1),
            (true, 1, 2),
        ]);
        assert_eq!(decode_phrase_stream(&stream, 6).unwrap(), b"ABABBA");
    }

    fn full_dictionary_prefix() -> Vec<u8> {
        // This prefix ends with 255,16 and allocates codes 256..8191 as
        // two-byte phrases; in particular 256="00" and 257="01".
        let mut prefix = Vec::with_capacity(7937);
        for x in 0..256u16 {
            prefix.push(x as u8);
            for y in x + 1..256 {
                prefix.extend([x as u8, y as u8]);
            }
        }
        prefix.truncate(7937);
        let distinct_pairs: std::collections::HashSet<_> =
            prefix.windows(2).map(|p| (p[0], p[1])).collect();
        assert_eq!(prefix.len(), 7937);
        assert_eq!(distinct_pairs.len(), 7936);
        assert_eq!(
            &prefix[prefix.len() - 10..],
            &[251, 15, 252, 15, 253, 15, 254, 15, 255, 16]
        );
        prefix
    }

    fn stream_after_prefix(prefix: &[u8], suffix: &[(bool, u32, usize)]) -> Vec<u8> {
        let mut codes: Vec<_> = prefix
            .iter()
            .map(|&byte| (false, u32::from(byte), 0))
            .collect();
        codes.extend_from_slice(suffix);
        encode_short_codes(&codes)
    }

    #[test]
    fn recycling_stops_when_the_head_is_the_current_parent() {
        // Extending 0 by 01 reaches oldest child 00, which must survive extension.
        let prefix = full_dictionary_prefix();
        let stream = stream_after_prefix(&prefix, &[(false, 0, 0), (true, 1, 13), (true, 0, 13)]);
        let mut expected = prefix;
        expected.extend([0, 0, 1, 0, 0]);
        assert_eq!(
            decode_phrase_stream(&stream, expected.len()).unwrap(),
            expected
        );
    }

    #[test]
    fn touching_a_phrase_moves_it_behind_the_next_victim() {
        // Reading 256 protects it; 257 is reused for 16,0,0 instead of retaining 0,1.
        let prefix = full_dictionary_prefix();
        let stream = stream_after_prefix(&prefix, &[(true, 0, 13), (false, 255, 0), (true, 1, 13)]);
        let mut expected = prefix;
        expected.extend([0, 0, 255, 16, 0, 0]);
        assert_eq!(
            decode_phrase_stream(&stream, expected.len()).unwrap(),
            expected
        );
    }

    #[test]
    fn recycling_removes_the_old_edge_before_reassigning_its_code() {
        // Reusing 256 removes 0,0; seeing that pair again must allocate code 257.
        let prefix = full_dictionary_prefix();
        let stream = stream_after_prefix(
            &prefix,
            &[(false, 16, 0), (false, 0, 0), (false, 0, 0), (true, 1, 13)],
        );
        let mut expected = prefix;
        expected.extend([16, 0, 0, 0, 0]);
        assert_eq!(
            decode_phrase_stream(&stream, expected.len()).unwrap(),
            expected
        );
    }

    #[test]
    fn recycled_codes_are_inserted_before_phrase_parents() {
        // New 257 sits before parent 288, so 31 further insertions recycle it.
        let prefix = full_dictionary_prefix();
        let mut suffix = vec![(true, 0, 13), (false, 16, 0)];
        suffix.extend((16..=45).map(|byte| (false, byte, 0)));
        suffix.push((true, 1, 13));
        let stream = stream_after_prefix(&prefix, &suffix);
        let mut expected = prefix;
        expected.extend([0, 0, 16]);
        expected.extend(16..=45);
        expected.extend([44, 45]);
        assert_eq!(
            decode_phrase_stream(&stream, expected.len()).unwrap(),
            expected
        );
    }

    #[test]
    fn full_dictionary_stream_keeps_truncation_malformed_and_length_errors() {
        let prefix = full_dictionary_prefix();
        let stream = stream_after_prefix(&prefix, &[(false, 0, 0), (true, 1, 13), (true, 0, 13)]);
        let output_len = prefix.len() + 5;
        for end in [0, 1, prefix.len() * 9 / 8 - 1, prefix.len() * 9 / 8] {
            assert!(decode_phrase_stream(&stream[..end], output_len).is_err());
        }
        for end in stream.len().saturating_sub(3)..stream.len() {
            assert!(decode_phrase_stream(&stream[..end], output_len).is_err());
        }
        assert_eq!(
            decode_phrase_stream(&stream, output_len - 1)
                .unwrap_err()
                .to_string(),
            "compressed stream exceeds the HED output length"
        );
        let malformed = stream_after_prefix(&prefix, &[(true, 7936, 13)]);
        assert_eq!(
            decode_phrase_stream(&malformed, prefix.len() + 1)
                .unwrap_err()
                .to_string(),
            "compressed stream contains an unsupported or malformed phrase code"
        );
    }

    #[test]
    fn rejects_truncated_and_wrong_length_streams() {
        assert_eq!(decode_phrase_stream(&[], 0).unwrap(), b"");
        let stream = encode_short_codes(&[(false, b'A' as u32, 0)]);
        assert_eq!(
            decode_phrase_stream(&stream, 2).unwrap_err().to_string(),
            "compressed bitstream ended before its end code"
        );
        let invalid_code = encode_short_codes(&[(true, 0, 1)]);
        assert_eq!(
            decode_phrase_stream(&invalid_code, 1)
                .unwrap_err()
                .to_string(),
            "compressed stream contains an unsupported or malformed phrase code"
        );
    }

    #[test]
    fn phrase_dictionary_updates_stop_at_the_source_depth_limit() {
        // A-runs grow 1,1,2,3,5,8,13,21,34,55,89,100. Capping 55+89
        // leaves code 354 at width 7; uncapped growth wrongly switches to width 8.
        let stream = encode_short_codes(&[
            (false, b'A' as u32, 0),
            (false, b'A' as u32, 0),
            (true, 0, 1),
            (true, 1, 2),
            (true, 3, 3),
            (true, 6, 3),
            (true, 11, 4),
            (true, 19, 5),
            (true, 32, 6),
            (true, 53, 6),
            (true, 87, 7),
            (true, 98, 7),
            (false, b'B' as u32, 0),
            (true, 98, 7),
        ]);
        let mut expected = vec![b'A'; 332];
        expected.push(b'B');
        expected.extend([b'A'; 100]);
        assert_eq!(expected.len(), 433);
        assert_eq!(
            decode_phrase_stream(&stream, expected.len()).unwrap(),
            expected
        );
        for end in 0..stream.len() {
            assert!(decode_phrase_stream(&stream[..end], expected.len()).is_err());
        }
        assert_eq!(
            decode_phrase_stream(&stream, expected.len() - 1)
                .unwrap_err()
                .to_string(),
            "compressed stream exceeds the HED output length"
        );
    }

    #[test]
    fn rejects_block_bounds_and_inconsistent_lengths() {
        let valid = HedEntry {
            offset: 0,
            packed: false,
            packed_len: 2,
            unpacked_len: 2,
        };
        assert_eq!(read_block(b"ok", valid).unwrap(), b"ok");
        for entry in [
            HedEntry {
                offset: u32::MAX,
                ..valid
            },
            HedEntry {
                packed_len: u32::MAX,
                ..valid
            },
            HedEntry {
                unpacked_len: 1,
                ..valid
            },
            HedEntry {
                unpacked_len: MAX_BLOCK_BYTES as u32 + 1,
                ..valid
            },
        ] {
            assert!(read_block(b"ok", entry).is_err());
        }
    }

    #[test]
    fn selected_entry_skips_an_unrelated_unsupported_block() {
        let (_archive, hed_path, dat_path) = TestArchive::new();
        let mut names = 2u32.to_le_bytes().to_vec();
        names.extend_from_slice(b"bad.bin\0ok.txt\0\0");
        let mut dat = names.clone();
        let ok_offset = dat.len() as u32;
        dat.extend_from_slice(b"ok");
        let mut hed = Vec::new();
        hed.extend_from_slice(&0u32.to_le_bytes());
        hed.extend_from_slice(&(names.len() as u32).to_le_bytes());
        hed.extend_from_slice(&(names.len() as u32).to_le_bytes());
        append_hed_entry(&mut hed, names.len() as u32, 0, 1, true);
        append_hed_entry(&mut hed, ok_offset, 2, 2, false);

        std::fs::write(&hed_path, hed).unwrap();
        std::fs::write(&dat_path, dat).unwrap();
        let selected = read_archive_entry(&hed_path, &dat_path, 2).unwrap();
        assert_eq!(selected.name, "ok.txt");
        assert_eq!(selected.data, b"ok");
        assert_eq!(
            read_archive_entry(&hed_path, &dat_path, 1)
                .unwrap_err()
                .to_string(),
            "compressed bitstream ended before its end code"
        );
        assert_eq!(
            read_archive_entry(&hed_path, &dat_path, 0)
                .unwrap_err()
                .to_string(),
            "archive entry ordinal must be one-based"
        );
        assert_eq!(
            read_archive_entry(&hed_path, &dat_path, 3)
                .unwrap_err()
                .to_string(),
            "archive entry ordinal is outside the filename list"
        );
        assert!(read_archive(&hed_path, &dat_path).is_err());
    }

    #[test]
    fn parses_exact_archive_filename_table() {
        let names = [b"a.csv".as_slice(), b"b.csv".as_slice()];
        let mut bytes = 2u32.to_le_bytes().to_vec();
        for name in names {
            bytes.extend_from_slice(name);
            bytes.push(0);
        }
        bytes.push(0);
        assert_eq!(parse_names(&bytes).unwrap(), ["a.csv", "b.csv"]);
        assert!(parse_names(&bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn parses_hed_lengths_and_rejects_partial_records() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&0x8000_001cu32.to_le_bytes());
        bytes.extend_from_slice(&26u32.to_le_bytes());
        let (entries, marker) = parse_hed(&bytes).unwrap();
        assert!(!marker);
        assert_eq!(entries[0].offset, 0);
        assert_eq!(entries[0].packed_len, 28);
        assert_eq!(entries[0].unpacked_len, 26);
        assert!(parse_hed(&bytes[..11]).is_err());
    }
}
