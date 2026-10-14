//! Read-only byte-level audit of the selected Japanese map text tables.

#[path = "../src/archive.rs"]
mod archive;

use std::collections::HashSet;
use std::env;
use std::io::{self, Read};
use std::path::Path;
use std::process::ExitCode;

const MAX_REFERENCE_BYTES: usize = 1024 * 1024;
const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;
const ERROR: &str = "Map text audit failed: invalid input or unsupported archive.";

#[derive(Debug, Default, PartialEq, Eq)]
struct Stats {
    rows: usize,
    shape_mismatches: usize,
    numeric_candidates: HashSet<u32>,
    duplicate_numeric_candidates: usize,
    missing_first_fields: usize,
    nonnumeric_first_fields: usize,
    invalid_decimal_fields: usize,
    overflow_decimal_fields: usize,
    empty_fields: usize,
}

fn main() -> ExitCode {
    std::panic::set_hook(Box::new(|_| {}));
    match run() {
        Ok(report) => {
            print!("{report}");
            ExitCode::SUCCESS
        }
        Err(()) => {
            eprintln!("{ERROR}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<String, ()> {
    let mut args = env::args_os().skip(1);
    let root = args.next().ok_or(())?;
    if args.next().is_some() {
        return Err(());
    }
    let mut input = Vec::new();
    io::stdin()
        .take((MAX_REFERENCE_BYTES + 1) as u64)
        .read_to_end(&mut input)
        .map_err(|_| ())?;
    if input.len() > MAX_REFERENCE_BYTES {
        return Err(());
    }
    let references = parse_reference_ids(&input)?;
    let xls = Path::new(&root).join("data").join("xls");
    let hed = xls.join("table.hed");
    let dat = xls.join("table.dat");
    let autorun = archive::read_archive_entry(&hed, &dat, 83).map_err(|_| ())?;
    let overlap = archive::read_archive_entry(&hed, &dat, 84).map_err(|_| ())?;
    let configuration = archive::read_archive_entry(&hed, &dat, 142).map_err(|_| ())?;
    if autorun.name != "mapname_autorun.csv"
        || overlap.name != "mapname_overlap.csv"
        || configuration.name != "strtable.csv"
    {
        return Err(());
    }
    validate_configured_cp932(&configuration.data)?;
    let autorun_stats = audit_payload(&autorun.data, Some(3))?;
    let overlap_stats = audit_payload(&overlap.data, None)?;
    Ok(format_report(&references, &autorun_stats, &overlap_stats))
}

// shortcut: accepts only the observed literal entry; broaden when another source-backed form is needed.
fn validate_configured_cp932(bytes: &[u8]) -> Result<(), ()> {
    const KEY: &[u8] = b"CLIENT_DATA_CODE_PAGE";
    const ENTRY: &[u8] = b"CLIENT_DATA_CODE_PAGE,\"932\"";
    if bytes.is_empty()
        || bytes.len() > MAX_PAYLOAD_BYTES
        || bytes.contains(&0)
        || bytes
            .windows(KEY.len())
            .filter(|window| *window == KEY)
            .count()
            != 1
    {
        return Err(());
    }
    if !crlf_records(bytes)?.iter().any(|record| *record == ENTRY) {
        return Err(());
    }
    Ok(())
}

#[cfg(windows)]
fn validate_cp932_bytes(bytes: &[u8]) -> Result<(), ()> {
    use std::ffi::c_int;
    const MB_ERR_INVALID_CHARS: u32 = 0x8;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MultiByteToWideChar(
            code_page: u32,
            flags: u32,
            input: *const u8,
            input_len: c_int,
            output: *mut u16,
            output_len: c_int,
        ) -> c_int;
    }

    if bytes.is_empty() || bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(());
    }
    let input_len = c_int::try_from(bytes.len()).map_err(|_| ())?;
    // SAFETY: input is bounded and readable; null output is valid for a required-size query.
    let converted = unsafe {
        MultiByteToWideChar(
            932,
            MB_ERR_INVALID_CHARS,
            bytes.as_ptr(),
            input_len,
            std::ptr::null_mut(),
            0,
        )
    };
    (converted > 0).then_some(()).ok_or(())
}

#[cfg(not(windows))]
fn validate_cp932_bytes(_bytes: &[u8]) -> Result<(), ()> {
    Err(())
}

fn parse_reference_ids(input: &[u8]) -> Result<HashSet<u32>, ()> {
    if input.is_empty() {
        return Err(());
    }
    let input = input.strip_suffix(b"\n").unwrap_or(input);
    if input.is_empty() {
        return Err(());
    }
    let mut ids = HashSet::new();
    for line in input.split(|&byte| byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() || !line.iter().all(u8::is_ascii_digit) {
            return Err(());
        }
        let text = std::str::from_utf8(line).map_err(|_| ())?;
        let id = text.parse::<u32>().map_err(|_| ())?;
        if id == 0 || !ids.insert(id) {
            return Err(());
        }
    }
    Ok(ids)
}

fn audit_payload(bytes: &[u8], expected_fields: Option<usize>) -> Result<Stats, ()> {
    if bytes.is_empty()
        || bytes.len() > MAX_PAYLOAD_BYTES
        || bytes.iter().any(|&byte| matches!(byte, 0 | b'\t' | b'"'))
    {
        return Err(());
    }
    validate_cp932_bytes(bytes)?;
    let records = crlf_records(bytes)?;
    let header_index = records
        .iter()
        .position(|record| !record.is_empty())
        .ok_or(())?;
    if !records[header_index].starts_with(b"#") {
        return Err(());
    }

    let mut stats = Stats::default();
    for record in records.iter().skip(header_index + 1) {
        if record.is_empty() || record.starts_with(b"#") {
            continue;
        }
        stats.rows += 1;
        let fields: Vec<&[u8]> = record.split(|&byte| byte == b',').collect();
        if expected_fields.is_some_and(|expected| fields.len() != expected) {
            stats.shape_mismatches += 1;
        }
        stats.empty_fields += fields.iter().filter(|field| field.is_empty()).count();
        match fields.first().copied().unwrap_or_default() {
            [] => stats.missing_first_fields += 1,
            field if field.iter().all(u8::is_ascii_digit) => {
                let text = std::str::from_utf8(field).map_err(|_| ())?;
                match text.parse::<u32>() {
                    Ok(id) if id > 0 => {
                        if !stats.numeric_candidates.insert(id) {
                            stats.duplicate_numeric_candidates += 1;
                        }
                    }
                    Ok(_) => stats.invalid_decimal_fields += 1,
                    Err(_) => stats.overflow_decimal_fields += 1,
                }
            }
            _ => stats.nonnumeric_first_fields += 1,
        }
    }
    Ok(stats)
}

fn crlf_records(bytes: &[u8]) -> Result<Vec<&[u8]>, ()> {
    let mut records = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\n' => {
                if index == 0 || bytes[index - 1] != b'\r' {
                    return Err(());
                }
                records.push(&bytes[start..index - 1]);
                start = index + 1;
            }
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => {}
            b'\r' => return Err(()),
            _ => {}
        }
        index += 1;
    }
    if start < bytes.len() {
        if bytes[start..].contains(&b'\r') {
            return Err(());
        }
        records.push(&bytes[start..]);
    }
    Ok(records)
}

fn format_report(references: &HashSet<u32>, autorun: &Stats, overlap: &Stats) -> String {
    let mut report = String::from("Map text byte audit\n");
    report.push_str(&format!(
        "Reference archive index IDs: {}\n",
        references.len()
    ));
    append_stats(&mut report, "Autorun", references, autorun, true);
    append_stats(&mut report, "Overlap", references, overlap, false);
    report.push_str(
        "Reference source: map archive index only\nEncoding: CP932 byte validation only; runtime unverified\nSchema: unverified\nEnglish coverage: unverified\n",
    );
    report
}

fn append_stats(
    report: &mut String,
    label: &str,
    references: &HashSet<u32>,
    stats: &Stats,
    has_expected_shape: bool,
) {
    let matches = stats.numeric_candidates.intersection(references).count();
    let candidates_absent = stats.numeric_candidates.len() - matches;
    report.push_str(&format!("{label} rows: {}\n", stats.rows));
    if has_expected_shape {
        report.push_str(&format!(
            "{label} three-field row-shape mismatches: {}\n",
            stats.shape_mismatches
        ));
    }
    report.push_str(&format!(
        "{label} unique positive-u32 candidates: {}\n{label} missing first fields: {}\n{label} nonnumeric first fields: {}\n{label} zero decimal fields: {}\n{label} overflow decimal fields: {}\n{label} duplicate numeric candidates: {}\n{label} candidates absent from reference index: {candidates_absent}\n{label} unique reference index matches: {matches}\n{label} empty fields: {}\n",
        stats.numeric_candidates.len(),
        stats.missing_first_fields,
        stats.nonnumeric_first_fields,
        stats.invalid_decimal_fields,
        stats.overflow_decimal_fields,
        stats.duplicate_numeric_candidates,
        stats.empty_fields,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bounded_positive_ascii_reference_ids() {
        assert_eq!(
            parse_reference_ids(b"100\r\n200\n").unwrap(),
            HashSet::from([100, 200])
        );
        for invalid in [
            b"".as_slice(),
            b"0",
            b"1\n\n",
            b"1\n1",
            b"+1",
            b" 1",
            b"4294967296",
            "１２".as_bytes(),
        ] {
            assert!(parse_reference_ids(invalid).is_err());
        }
    }

    #[test]
    fn counts_candidates_duplicates_missing_and_malformed_fields_without_text() {
        let payload = b"#private header\r\n100,x,y\r\n100,,z\r\n300,x,y\r\n0,x,y\r\n4294967296,x,y\r\nname,x,y\r\n,x,y\r\n";
        let stats = audit_payload(payload, Some(3)).unwrap();
        assert_eq!(stats.rows, 7);
        assert_eq!(stats.shape_mismatches, 0);
        assert_eq!(stats.numeric_candidates, HashSet::from([100, 300]));
        assert_eq!(stats.duplicate_numeric_candidates, 1);
        assert_eq!(stats.missing_first_fields, 1);
        assert_eq!(stats.nonnumeric_first_fields, 1);
        assert_eq!(stats.invalid_decimal_fields, 1);
        assert_eq!(stats.overflow_decimal_fields, 1);
        assert_eq!(stats.empty_fields, 2);
        let report = format_report(&HashSet::from([100, 200]), &stats, &stats);
        assert!(!report.contains("private"));
        assert!(!report.contains("name"));
        assert!(report.contains("Autorun unique reference index matches: 1"));
        assert!(report.contains("Autorun candidates absent from reference index: 1"));
    }

    #[test]
    fn rejects_malformed_record_bytes_and_non_header_first_record() {
        for invalid in [
            b"#h\n1,x\r\n".as_slice(),
            b"#h\r1,x\r\n",
            b"#h\r\n1\"x\r\n",
            b"#h\r\n1\0x\r\n",
            b"#h\r\n1\tx\r\n",
            b"not-header\r\n1,x\r\n",
        ] {
            assert!(audit_payload(invalid, None).is_err());
        }
        let blank_row = audit_payload(b"\r\n#h\r\n\r\n1,x", None).unwrap();
        assert_eq!(blank_row.rows, 1);
        assert_eq!(blank_row.missing_first_fields, 0);
    }

    #[test]
    fn skips_comment_and_blank_records_without_trimming_record_bytes() {
        let payload = b"#private header\r\n1,first\r\n#999,numeric-looking\r\n\r\n  #private leading space\r\n2,last\r\n#private final\r\n";
        let stats = audit_payload(payload, None).unwrap();
        assert_eq!(stats.rows, 3);
        assert_eq!(stats.numeric_candidates, HashSet::from([1, 2]));
        assert_eq!(stats.nonnumeric_first_fields, 1);
        let report = format_report(&HashSet::from([1, 2]), &stats, &stats);
        assert!(!report.contains("private"));
    }

    #[test]
    fn counts_autorun_shape_mismatches_without_assigning_overlap_shape() {
        let payload = b"#header\r\n1,a\r\n2,a,b,c";
        let autorun = audit_payload(payload, Some(3)).unwrap();
        let overlap = audit_payload(payload, None).unwrap();
        assert_eq!(autorun.shape_mismatches, 2);
        assert_eq!(overlap.shape_mismatches, 0);
        assert_eq!(autorun.rows, overlap.rows);
    }

    #[test]
    fn overlap_keeps_lexical_counts_for_ambiguous_and_terminated_rows() {
        let payload = b"#private header\r\n1001\r\n1002,5,text\r\n1003,5,\r\n1004,5\r\n1005,0,private tail,,999\r\n1007,-1,private tail,,888\r\n1006,2,a,3,b\r\n#private comment,777\r\n\r\n";
        let stats = audit_payload(payload, None).unwrap();
        assert_eq!(stats.rows, 7);
        assert_eq!(stats.shape_mismatches, 0);
        assert_eq!(
            stats.numeric_candidates,
            HashSet::from([1001, 1002, 1003, 1004, 1005, 1006, 1007])
        );
        assert_eq!(stats.missing_first_fields, 0);
        assert_eq!(stats.nonnumeric_first_fields, 0);
        assert_eq!(stats.invalid_decimal_fields, 0);
        assert_eq!(stats.overflow_decimal_fields, 0);
        assert_eq!(stats.duplicate_numeric_candidates, 0);
        assert_eq!(stats.empty_fields, 3);
        let report = format_report(
            &HashSet::from([1001, 1002, 1003, 1004, 1005, 1006, 1007]),
            &stats,
            &stats,
        );
        assert!(!report.contains("private"));
        assert!(!report.contains("777") && !report.contains("888") && !report.contains("999"));
        assert!(report.contains("Schema: unverified"));
    }

    #[test]
    fn secondary_numeric_fields_are_not_candidates_or_reference_matches() {
        let references = HashSet::from([5, 6, 7]);
        let autorun = audit_payload(
            b"#private header\r\n1001,5,private text\r\n1001,6,private text\r\n1002,7,private text",
            Some(3),
        )
        .unwrap();
        let overlap = audit_payload(
            b"#private header\r\n2001,5,private text,6,private text\r\n2001,7,private text",
            None,
        )
        .unwrap();

        assert_eq!(autorun.numeric_candidates, HashSet::from([1001, 1002]));
        assert_eq!(autorun.duplicate_numeric_candidates, 1);
        assert_eq!(overlap.numeric_candidates, HashSet::from([2001]));
        assert_eq!(overlap.duplicate_numeric_candidates, 1);
        let report = format_report(&references, &autorun, &overlap);
        assert!(report.contains("Autorun candidates absent from reference index: 2"));
        assert!(report.contains("Autorun unique reference index matches: 0"));
        assert!(report.contains("Overlap candidates absent from reference index: 1"));
        assert!(report.contains("Overlap unique reference index matches: 0"));
        assert!(!report.contains("private text"));
    }

    #[test]
    fn lexical_candidates_keep_u32_range_without_accepting_signed_text() {
        let references = HashSet::from([2_147_483_648]);
        let stats = audit_payload(
            b"#private header\r\n2147483647,private\r\n2147483648,private\r\n4294967295,private\r\n-1,private\r\n+1,private",
            None,
        )
        .unwrap();

        assert_eq!(
            stats.numeric_candidates,
            HashSet::from([2_147_483_647, 2_147_483_648, 4_294_967_295])
        );
        assert_eq!(stats.duplicate_numeric_candidates, 0);
        assert_eq!(stats.missing_first_fields, 0);
        assert_eq!(stats.nonnumeric_first_fields, 2);
        assert_eq!(stats.invalid_decimal_fields, 0);
        assert_eq!(stats.overflow_decimal_fields, 0);
        let report = format_report(&references, &stats, &stats);
        assert!(report.contains("Autorun unique reference index matches: 1"));
        assert!(report.contains("Autorun candidates absent from reference index: 2"));
        assert!(!report.contains("private"));
    }

    #[test]
    fn rejects_empty_and_oversized_payloads() {
        assert!(audit_payload(b"", None).is_err());
        assert!(audit_payload(&vec![b'x'; MAX_PAYLOAD_BYTES + 1], None).is_err());
    }

    #[test]
    fn accepts_only_the_single_supported_config_entry() {
        assert!(
            validate_configured_cp932(b"# header\r\nCLIENT_DATA_CODE_PAGE,\"932\"\r\n").is_ok()
        );
        for invalid in [
            b"".as_slice(),
            b"# header\r\n".as_slice(),
            b"# CLIENT_DATA_CODE_PAGE,\"932\"\r\n",
            b"# header\r\nCLIENT_DATA_CODE_PAGE,\"932\"\r\nCLIENT_DATA_CODE_PAGE,\"932\"\r\n",
            b"# header\r\n\"CLIENT_DATA_CODE_PAGE\",\"932\"\r\n",
            b"# header\r\nCLIENT_DATA_CODE_PAGE,\"65001\"\r\n",
            b"# header\r\nCLIENT_DATA_CODE_PAGE,932\r\n",
            b"# header\r\nCLIENT_DATA_CODE_PAGE,\"932\",extra\r\n",
            b"# header\r\nCLIENT_DATA_CODE_PAGE,\"932\"\n",
            b"# header\r\nCLIENT_DATA_CODE_PAGE,\"932\"\r\n\0",
        ] {
            assert!(validate_configured_cp932(invalid).is_err());
        }
        assert!(validate_configured_cp932(&vec![b'x'; MAX_PAYLOAD_BYTES + 1]).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn strictly_validates_cp932_bytes_before_record_parsing() {
        assert!(validate_cp932_bytes(b"ASCII\r\n\x82\xa0\xa6\x95\\\r\n").is_ok());
        for invalid in [
            b"# header\r\n\x82".as_slice(),
            b"# comment \x82\x20\r\n",
            b"# header \x82\r\n1,x",
            b"# header\r\n1,x\r\n# comment \x82\x20\r\n2,y",
            b"# header\r\n1,x\r\n2,\x82",
        ] {
            assert!(audit_payload(invalid, None).is_err());
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn cp932_validation_fails_closed_off_windows() {
        assert!(validate_cp932_bytes(b"ASCII").is_err());
    }
}
