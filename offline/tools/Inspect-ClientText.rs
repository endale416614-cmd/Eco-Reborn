//! Headless, read-only smoke tool for the supplied ECO text archive.
//! Build directly with rustc; this is not wired into the offline server CLI.

#[path = "../src/archive.rs"]
mod archive;

use std::env;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

#[derive(Debug, PartialEq, Eq)]
enum ArchiveKind {
    Text,
    Table,
}

impl ArchiveKind {
    fn basename(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Table => "table",
        }
    }
}

struct Options {
    root: PathBuf,
    archive: ArchiveKind,
    entry_number: Option<usize>,
}

fn main() -> std::process::ExitCode {
    std::panic::set_hook(Box::new(|_| eprintln!("Text archive inspection failed.")));
    match inspect() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(_) => {
            eprintln!("Text archive inspection failed: missing, malformed, or unsupported input.");
            std::process::ExitCode::FAILURE
        }
    }
}

fn inspect() -> Result<(), Box<dyn std::error::Error>> {
    let options = parse_args(env::args_os().skip(1)).map_err(|_| "invalid arguments")?;
    let xls = options.root.join("data").join("xls");
    let basename = options.archive.basename();
    let hed = xls.join(format!("{basename}.hed"));
    let dat = xls.join(format!("{basename}.dat"));
    if let Some(index) = options.entry_number {
        let entry = archive::read_archive_entry(&hed, &dat, index)?;
        println!("experimental {basename} archive selected entries: 1");
        println!("entry {index}: {} bytes", entry.data.len());
    } else {
        let entries = archive::read_archive(&hed, &dat)?;
        println!("experimental {basename} archive entries: {}", entries.len());
        for (index, entry) in entries.into_iter().enumerate() {
            println!("entry {}: {} bytes", index + 1, entry.data.len());
        }
    }
    Ok(())
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Options, ()> {
    let mut args = args.into_iter();
    let root = args.next().map(PathBuf::from).ok_or(())?;
    let archive = match args.next().as_deref() {
        None => ArchiveKind::Text,
        Some(value) if value == OsStr::new("text") => ArchiveKind::Text,
        Some(value) if value == OsStr::new("table") => ArchiveKind::Table,
        _ => return Err(()),
    };
    let entry_number = args.next().as_deref().map(parse_entry_number).transpose()?;
    if args.next().is_some() {
        return Err(());
    }
    Ok(Options {
        root,
        archive,
        entry_number,
    })
}

fn parse_entry_number(value: &OsStr) -> Result<usize, ()> {
    let value = value.to_str().ok_or(())?;
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(());
    }
    let number = value.parse::<usize>().map_err(|_| ())?;
    if number == 0 {
        return Err(());
    }
    Ok(number)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_default_text_or_table_archive_and_rejects_bad_arguments() {
        let root = OsString::from("client");
        assert_eq!(
            parse_args([root.clone()]).unwrap().archive,
            ArchiveKind::Text
        );
        assert_eq!(
            parse_args([root.clone(), OsString::from("text")])
                .unwrap()
                .archive,
            ArchiveKind::Text
        );
        assert_eq!(
            parse_args([root.clone(), OsString::from("table")])
                .unwrap()
                .archive,
            ArchiveKind::Table
        );
        assert_eq!(
            parse_args([root.clone(), OsString::from("table"), OsString::from("12")])
                .unwrap()
                .entry_number,
            Some(12)
        );
        assert!(parse_args([]).is_err());
        assert!(parse_args([root.clone(), OsString::from("json")]).is_err());
        assert!(parse_args([
            root.clone(),
            OsString::from("text"),
            OsString::from("extra")
        ])
        .is_err());
        assert!(parse_args([
            root.clone(),
            OsString::from("text"),
            OsString::from("1"),
            OsString::from("extra")
        ])
        .is_err());
    }

    #[test]
    fn accepts_only_positive_ascii_decimal_entry_numbers() {
        assert_eq!(parse_entry_number(OsStr::new("1")), Ok(1));
        assert_eq!(parse_entry_number(OsStr::new("0012")), Ok(12));
        for invalid in ["0", "+1", "-1", " 1", "1 ", "one", ""] {
            assert!(parse_entry_number(OsStr::new(invalid)).is_err());
        }
        assert!(parse_entry_number(OsStr::new(&format!("{}0", usize::MAX))).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn accepts_non_utf8_windows_client_root() {
        use std::os::windows::ffi::OsStringExt;

        let root = OsString::from_wide(&[0xd800]);
        assert_eq!(
            parse_args([root.clone()]).unwrap().root,
            PathBuf::from(root)
        );
    }
}
