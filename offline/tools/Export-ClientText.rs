//! Export the raw decompressed client table/text archives into a new directory.
#[path = "../src/archive.rs"]
mod archive;

use archive::Entry;
use std::collections::HashSet;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

const ERROR: &str = "Client text export failed: invalid, unsupported, or unsafe input/output.";

#[derive(Clone, Copy)]
enum ArchiveKind {
    Table,
    Text,
}

impl ArchiveKind {
    fn name(self) -> &'static str {
        match self {
            Self::Table => "table",
            Self::Text => "text",
        }
    }
}

struct ExportedArchive {
    kind: ArchiveKind,
    entries: Vec<Entry>,
}

fn main() -> ExitCode {
    std::panic::set_hook(Box::new(|_| eprintln!("{ERROR}")));
    match export_from_args(env::args_os().skip(1)) {
        Ok((table_count, text_count)) => {
            println!("Exported table entries: {table_count}");
            println!("Exported text entries: {text_count}");
            ExitCode::SUCCESS
        }
        Err(()) => {
            eprintln!("{ERROR}");
            ExitCode::FAILURE
        }
    }
}

fn export_from_args(
    args: impl IntoIterator<Item = std::ffi::OsString>,
) -> Result<(usize, usize), ()> {
    let mut args = args.into_iter();
    let root = args.next().map(PathBuf::from).ok_or(())?;
    let output = args.next().map(PathBuf::from).ok_or(())?;
    if args.next().is_some() {
        return Err(());
    }

    let source_root = root.canonicalize().map_err(|_| ())?;
    let output_resolved = resolve_nonexistent_path(&output).map_err(|_| ())?;
    if path_is_within(&output_resolved, &source_root) {
        return Err(());
    }

    let xls = root.join("data").join("xls");
    let archives = [
        load_archive(ArchiveKind::Table, &xls).map_err(|_| ())?,
        load_archive(ArchiveKind::Text, &xls).map_err(|_| ())?,
    ];
    for archive in &archives {
        validate_entries(&archive.entries).map_err(|_| ())?;
    }

    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|_| ())?;
    fs::create_dir(&output).map_err(|_| ())?;
    write_export(&output, &archives).map_err(|_| ())?;
    Ok((archives[0].entries.len(), archives[1].entries.len()))
}

fn load_archive(kind: ArchiveKind, xls: &Path) -> io::Result<ExportedArchive> {
    let name = kind.name();
    Ok(ExportedArchive {
        kind,
        entries: archive::read_archive(
            &xls.join(format!("{name}.hed")),
            &xls.join(format!("{name}.dat")),
        )?,
    })
}

fn validate_entries(entries: &[Entry]) -> io::Result<()> {
    let mut names = HashSet::with_capacity(entries.len());
    for entry in entries {
        validate_output_name(&entry.name)?;
        if !names.insert(entry.name.to_ascii_lowercase()) {
            return Err(invalid());
        }
    }
    Ok(())
}

fn validate_output_name(name: &str) -> io::Result<()> {
    if name.is_empty()
        || !name.is_ascii()
        || name == "."
        || name == ".."
        || name.ends_with(['.', ' '])
        || name.bytes().any(|byte| {
            byte < 0x20
                || matches!(
                    byte,
                    b'<' | b'>' | b':' | b'"' | b'/' | b'\\' | b'|' | b'?' | b'*'
                )
        })
    {
        return Err(invalid());
    }
    let stem = name.split('.').next().unwrap_or_default();
    let upper = stem.to_ascii_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            upper.strip_prefix(prefix).is_some_and(|suffix| {
                suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9')
            })
        })
    {
        return Err(invalid());
    }
    Ok(())
}

fn write_export(output: &Path, archives: &[ExportedArchive; 2]) -> io::Result<()> {
    let mut manifest = File::create(output.join("manifest.tsv"))?;
    manifest.write_all(b"archive\tordinal\tname\tbytes\n")?;
    for archive in archives {
        let directory = output.join(archive.kind.name());
        fs::create_dir(&directory)?;
        for (index, entry) in archive.entries.iter().enumerate() {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(directory.join(&entry.name))?;
            file.write_all(&entry.data)?;
            writeln!(
                manifest,
                "{}\t{}\t{}\t{}",
                archive.kind.name(),
                index + 1,
                entry.name,
                entry.data.len()
            )?;
        }
    }
    manifest.flush()
}

fn resolve_nonexistent_path(path: &Path) -> io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()?.join(path)
    };
    let mut ancestor = absolute.as_path();
    let mut suffix = Vec::new();
    while !ancestor.exists() {
        let name = ancestor.file_name().ok_or_else(invalid)?;
        suffix.push(name.to_os_string());
        ancestor = ancestor.parent().ok_or_else(invalid)?;
    }
    let mut resolved = ancestor.canonicalize()?;
    for component in suffix.iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

fn path_is_within(path: &Path, root: &Path) -> bool {
    let path: Vec<_> = path.components().collect();
    let root: Vec<_> = root.components().collect();
    path.len() >= root.len()
        && path[..root.len()]
            .iter()
            .zip(root.iter())
            .all(|(left, right)| component_eq(left, right))
}

fn component_eq(left: &Component<'_>, right: &Component<'_>) -> bool {
    left.as_os_str()
        .to_string_lossy()
        .eq_ignore_ascii_case(&right.as_os_str().to_string_lossy())
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "invalid input")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    struct Temp(PathBuf);

    impl Temp {
        fn new() -> Self {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = env::temp_dir().join(format!("eco-export-test-{}-{id}", std::process::id()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_archive_pair(xls: &Path, kind: ArchiveKind, name: &str, payload: &[u8]) {
        let names = [name.as_bytes(), &[0], &[0]].concat();
        let mut names_block = (1u32).to_le_bytes().to_vec();
        names_block.extend_from_slice(&names);
        let mut dat = names_block.clone();
        dat.extend_from_slice(payload);
        let mut hed = Vec::new();
        for (offset, bytes) in [(0usize, &names_block[..]), (names_block.len(), payload)] {
            hed.extend_from_slice(&(offset as u32).to_le_bytes());
            hed.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            hed.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        }
        fs::write(xls.join(format!("{}.hed", kind.name())), hed).unwrap();
        fs::write(xls.join(format!("{}.dat", kind.name())), dat).unwrap();
    }

    #[test]
    fn exports_exact_bytes_and_refuses_existing_or_unsafe_destinations() {
        let temp = Temp::new();
        let client = temp.0.join("client");
        let xls = client.join("data").join("xls");
        fs::create_dir_all(&xls).unwrap();
        let table_bytes = [0x82, 0xa0, 0x00, 0xff];
        let text_bytes = [0x83, 0x5c, 0x0d, 0x0a];
        write_archive_pair(&xls, ArchiveKind::Table, "table.dat", &table_bytes);
        write_archive_pair(&xls, ArchiveKind::Text, "text.txt", &text_bytes);
        let source_before = [
            fs::read(xls.join("table.hed")).unwrap(),
            fs::read(xls.join("table.dat")).unwrap(),
            fs::read(xls.join("text.hed")).unwrap(),
            fs::read(xls.join("text.dat")).unwrap(),
        ];

        let output = temp.0.join("export");
        assert!(!path_is_within(
            &resolve_nonexistent_path(&output).unwrap(),
            &client.canonicalize().unwrap()
        ));
        assert_eq!(
            export_from_args([client.as_os_str().to_owned(), output.as_os_str().to_owned()]),
            Ok((1, 1))
        );
        assert_eq!(
            fs::read(output.join("table/table.dat")).unwrap(),
            table_bytes
        );
        assert_eq!(fs::read(output.join("text/text.txt")).unwrap(), text_bytes);
        assert_eq!(
            fs::read_to_string(output.join("manifest.tsv")).unwrap(),
            "archive\tordinal\tname\tbytes\ntable\t1\ttable.dat\t4\ntext\t1\ttext.txt\t4\n"
        );
        assert!(
            export_from_args([client.as_os_str().to_owned(), output.as_os_str().to_owned()])
                .is_err()
        );
        assert!(export_from_args([
            client.as_os_str().to_owned(),
            client.join("new-output").as_os_str().to_owned()
        ])
        .is_err());
        assert_eq!(
            source_before,
            [
                fs::read(xls.join("table.hed")).unwrap(),
                fs::read(xls.join("table.dat")).unwrap(),
                fs::read(xls.join("text.hed")).unwrap(),
                fs::read(xls.join("text.dat")).unwrap(),
            ]
        );
    }

    #[test]
    fn rejects_windows_unsafe_archive_names() {
        for name in [
            "../escape.txt",
            "sub/file",
            "bad:name",
            "NUL.txt",
            "name. ",
            "a\tb",
        ] {
            assert!(
                validate_output_name(name).is_err(),
                "accepted unsafe archive name"
            );
        }
        for name in ["text.txt", "table_001.csv", "mapname_autorun.csv"] {
            assert!(validate_output_name(name).is_ok());
        }
    }

    #[test]
    fn unsafe_archive_filename_refuses_export_before_creating_destination() {
        let temp = Temp::new();
        let client = temp.0.join("client");
        let xls = client.join("data").join("xls");
        fs::create_dir_all(&xls).unwrap();
        write_archive_pair(&xls, ArchiveKind::Table, "NUL.txt", b"raw");
        write_archive_pair(&xls, ArchiveKind::Text, "text.txt", b"raw");
        let output = temp.0.join("export");
        assert!(
            export_from_args([client.as_os_str().to_owned(), output.as_os_str().to_owned()])
                .is_err()
        );
        assert!(!output.exists());
        assert_eq!(fs::read(xls.join("table.dat")).unwrap().last(), Some(&b'w'));
    }
}
