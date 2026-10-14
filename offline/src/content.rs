use roxmltree::{Document, Node};
use std::collections::BTreeSet;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

/// Audit SagaECO's `Configuration/DB` directory. `root` must contain
/// `monster.csv`, `Quests/`, and `Spawns/`.
pub fn audit(root: &Path) -> Result<String, Box<dyn Error>> {
    let mut issues = Vec::new();
    let (mob_rows, mobs, duplicate_mobs) = read_mobs(&root.join("monster.csv"), &mut issues)?;
    let (spawn_files, spawn_rows, spawn_ids) = read_spawns(&root.join("Spawns"), &mut issues)?;
    let (quest_files, quest_rows, quest_id_rows, quest_ids, hunt_quests, hunt_targets) =
        read_quests(&root.join("Quests"), &mut issues)?;

    let missing_spawn_ids = spawn_ids.difference(&mobs).copied().collect::<Vec<_>>();
    let missing_quest_ids = hunt_targets.difference(&mobs).copied().collect::<Vec<_>>();
    let mob_range = id_range(&mobs);

    let mut report = format!(
        "SagaECO content audit (source: supplied Configuration/DB dataset)\n\
         Scope: ID parsing and spawn/HUNT-target references only; this does not validate all mechanics, event scripts, non-HUNT objectives, maps, or rewards.\n\
         Monster CSV: {mob_rows} data rows, {} unique IDs, {duplicate_mobs} duplicate IDs, ID range {mob_range}\n\
         Spawns: {spawn_files} XML files, {spawn_rows} spawn records, {} unique mob IDs, {} missing from monster.csv: {}\n\
         Quests: {quest_files} XML files, {quest_rows} quest records, {} records with valid IDs / {} unique IDs, {hunt_quests} HUNT quests, {} unique HUNT target IDs, {} missing from monster.csv: {}",
        mobs.len(), spawn_ids.len(), missing_spawn_ids.len(), sample(&missing_spawn_ids),
        quest_id_rows, quest_ids.len(), hunt_targets.len(), missing_quest_ids.len(), sample(&missing_quest_ids)
    );
    report.push_str(&format!("\nValidation issues: {}", issues.len()));
    for issue in issues.iter().take(20) {
        report.push_str(&format!("\n- {issue}"));
    }
    if issues.len() > 20 {
        report.push_str(&format!("\n- ... {} more", issues.len() - 20));
    }
    Ok(report)
}

fn read_mobs(
    path: &Path,
    issues: &mut Vec<String>,
) -> Result<(usize, BTreeSet<u32>, usize), Box<dyn Error>> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .comment(Some(b'#'))
        .from_path(path)
        .map_err(|_| "could not open monster.csv")?;
    let mut rows = 0;
    let mut ids = BTreeSet::new();
    let mut duplicates = 0;
    for (index, result) in reader.records().enumerate() {
        let record =
            result.map_err(|_| format!("monster.csv record #{} is unreadable", index + 1))?;
        rows += 1;
        let Some(raw) = record.get(0).filter(|s| !s.trim().is_empty()) else {
            issues.push(format!(
                "monster.csv record #{} is missing field ID",
                index + 1
            ));
            continue;
        };
        let id: u32 = match raw.trim().parse() {
            Ok(id) => id,
            Err(_) => {
                issues.push(format!(
                    "monster.csv record #{} has invalid numeric ID",
                    index + 1,
                ));
                continue;
            }
        };
        if !ids.insert(id) {
            duplicates += 1;
        }
    }
    Ok((rows, ids, duplicates))
}

fn xml_files(dir: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut pending = vec![dir.to_path_buf()];
    let mut files = Vec::new();
    while let Some(current) = pending.pop() {
        for entry in fs::read_dir(&current).map_err(|_| "could not enumerate XML collection")? {
            let entry = entry.map_err(|_| "could not enumerate XML collection")?;
            let kind = entry
                .file_type()
                .map_err(|_| "could not inspect XML collection entry")?;
            let path = entry.path();
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file()
                && path
                    .extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("xml"))
            {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn read_spawns(
    dir: &Path,
    issues: &mut Vec<String>,
) -> Result<(usize, usize, BTreeSet<u32>), Box<dyn Error>> {
    let files = xml_files(dir)?;
    let mut records = 0;
    let mut ids = BTreeSet::new();
    for (file_index, path) in files.iter().enumerate() {
        let file_index = file_index + 1;
        let text = fs::read_to_string(path)
            .map_err(|_| format!("Spawns file #{file_index} could not be read"))?;
        let doc = Document::parse(&text)
            .map_err(|_| format!("Spawns file #{file_index} contains malformed XML"))?;
        if doc.root_element().tag_name().name() != "Spawns" {
            return Err(format!("Spawns file #{file_index} has an unexpected root element").into());
        }
        let mut file_records = 0;
        for spawn in doc
            .root_element()
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "spawn")
        {
            records += 1;
            file_records += 1;
            if let Some(id) = parsed_u32(spawn, "id", "Spawns", file_index, file_records, issues) {
                ids.insert(id);
            }
        }
    }
    Ok((files.len(), records, ids))
}

fn read_quests(
    dir: &Path,
    issues: &mut Vec<String>,
) -> Result<(usize, usize, usize, BTreeSet<u32>, usize, BTreeSet<u32>), Box<dyn Error>> {
    let files = xml_files(dir)?;
    let mut records = 0;
    let mut id_rows = 0;
    let mut quest_ids = BTreeSet::new();
    let mut hunts = 0;
    let mut targets = BTreeSet::new();
    for (file_index, path) in files.iter().enumerate() {
        let file_index = file_index + 1;
        let text = fs::read_to_string(path)
            .map_err(|_| format!("Quests file #{file_index} could not be read"))?;
        let doc = Document::parse(&text)
            .map_err(|_| format!("Quests file #{file_index} contains malformed XML"))?;
        if doc.root_element().tag_name().name() != "QuestDB" {
            return Err(format!("Quests file #{file_index} has an unexpected root element").into());
        }
        let mut file_records = 0;
        for quest in doc
            .root_element()
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "Quest")
        {
            records += 1;
            file_records += 1;
            if let Some(id) = parsed_u32(quest, "ID", "Quests", file_index, file_records, issues) {
                id_rows += 1;
                if !quest_ids.insert(id) {
                    issues.push(format!(
                        "Quests file #{file_index} contains duplicate quest ID {id}"
                    ));
                }
            }
            let Some(kind) = child_text(quest, "Type") else {
                issues.push(format!(
                    "Quests file #{file_index} record #{file_records} is missing field Type"
                ));
                continue;
            };
            if kind.eq_ignore_ascii_case("HUNT") {
                hunts += 1;
                for name in ["ObjectID1", "ObjectID2", "ObjectID3"] {
                    if let Some(raw) = child_text(quest, name) {
                        match raw.trim().parse::<u32>() {
                            Ok(id) if id != 0 => {
                                targets.insert(id);
                            }
                            Ok(_) => {}
                            Err(_) => issues.push(format!(
                                "Quests file #{file_index} record #{file_records} target field {name} has invalid numeric ID"
                            )),
                        }
                    }
                }
            }
        }
    }
    Ok((files.len(), records, id_rows, quest_ids, hunts, targets))
}

fn child_text<'a>(node: Node<'a, 'a>, name: &str) -> Option<&'a str> {
    node.children()
        .find(|n| n.is_element() && n.tag_name().name().eq_ignore_ascii_case(name))
        .and_then(|n| n.text())
}

fn parsed_u32(
    node: Node<'_, '_>,
    name: &str,
    collection: &str,
    file_index: usize,
    record_index: usize,
    issues: &mut Vec<String>,
) -> Option<u32> {
    let Some(raw) = child_text(node, name) else {
        issues.push(format!(
            "{collection} file #{file_index} record #{record_index} is missing field {name}"
        ));
        return None;
    };
    match raw.trim().parse() {
        Ok(id) => Some(id),
        Err(_) => {
            issues.push(format!(
                "{collection} file #{file_index} record #{record_index} field {name} has invalid numeric ID"
            ));
            None
        }
    }
}

fn id_range(ids: &BTreeSet<u32>) -> String {
    match (ids.first(), ids.last()) {
        (Some(a), Some(b)) => format!("{a}..{b}"),
        _ => "empty".into(),
    }
}

fn sample(ids: &[u32]) -> String {
    let shown = ids
        .iter()
        .take(20)
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    if ids.len() > 20 {
        format!("[{shown}, ...] (first 20)")
    } else {
        format!("[{shown}]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = loop {
                let n = NEXT.fetch_add(1, Ordering::Relaxed);
                let candidate = std::env::temp_dir()
                    .join(format!("eco-content-audit-{}-{n}", std::process::id()));
                match fs::create_dir(&candidate) {
                    Ok(()) => break candidate,
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("cannot create test fixture: {error}"),
                }
            };
            fs::create_dir_all(path.join("Spawns")).unwrap();
            fs::create_dir_all(path.join("Quests")).unwrap();
            Self(path)
        }
        fn put(&self, relative: &str, data: &str) {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, data).unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn audits_spawn_and_hunt_target_references() {
        let f = Fixture::new();
        f.put("monster.csv", "# heading\n#ID,name\n100,known\n");
        f.put(
            "Spawns/a.xml",
            "<Spawns><spawn><id>100</id></spawn><spawn><id>200</id></spawn></Spawns>",
        );
        f.put(
            "Spawns/Dungeon/d.xml",
            "<Spawns><spawn><id>100</id></spawn></Spawns>",
        );
        f.put("Quests/q.xml", "<QuestDB GroupID=\"1\"><Quest><ID>1</ID><Type>HUNT</Type><ObjectID1>200</ObjectID1></Quest></QuestDB>");
        let report = audit(&f.0).unwrap();
        assert!(report.contains("2 XML files, 3 spawn records"));
        assert!(report.contains("1 missing from monster.csv: [200]"));
        assert!(report.contains("1 unique HUNT target IDs, 1 missing from monster.csv: [200]"));
    }

    #[test]
    fn rejects_malformed_ids_instead_of_skipping_them() {
        let f = Fixture::new();
        f.put("monster.csv", "#ID,name\nnot-an-id,mob\n");
        f.put("Spawns/a.xml", "<Spawns/>");
        f.put("Quests/q.xml", "<QuestDB/>");
        assert!(audit(&f.0).unwrap().contains("invalid numeric ID"));
    }

    #[test]
    fn rejects_spawn_without_mob_id() {
        let f = Fixture::new();
        f.put("monster.csv", "#ID,name\n100,mob\n");
        f.put(
            "Spawns/a.xml",
            "<Spawns><spawn><map>1</map></spawn></Spawns>",
        );
        f.put("Quests/q.xml", "<QuestDB/>");
        assert!(audit(&f.0).unwrap().contains("missing field id"));
    }

    #[test]
    fn report_omits_private_paths_filenames_and_raw_invalid_values() {
        let f = Fixture::new();
        let root = f.0.join("private-user_HOST_TOKEN_PRIVATE-PATH");
        fs::create_dir_all(root.join("Spawns")).unwrap();
        fs::create_dir_all(root.join("Quests")).unwrap();
        fs::write(
            root.join("monster.csv"),
            "PRIVATE_RAW_ACCOUNT_TOKEN_731,mob\n",
        )
        .unwrap();
        fs::write(
            root.join("Spawns")
                .join("private-user_MACHINE_TOKEN_814.xml"),
            "<Spawns><spawn><id>PRIVATE_RAW_SPAWN_TOKEN_629</id></spawn></Spawns>",
        )
        .unwrap();
        fs::write(
            root.join("Quests").join("private-account_HOST_TOKEN_972.xml"),
            "<QuestDB><Quest><Type>HUNT</Type><ObjectID1>PRIVATE_RAW_QUEST_TOKEN_381</ObjectID1></Quest></QuestDB>",
        )
        .unwrap();

        let report = audit(&root).unwrap();
        for private in [
            "private-user_HOST_TOKEN_PRIVATE-PATH",
            "private-user_MACHINE_TOKEN_814.xml",
            "private-account_HOST_TOKEN_972.xml",
            "PRIVATE_RAW_ACCOUNT_TOKEN_731",
            "PRIVATE_RAW_SPAWN_TOKEN_629",
            "PRIVATE_RAW_QUEST_TOKEN_381",
        ] {
            assert!(!report.contains(private), "report leaked {private}");
        }
        assert!(report.contains("source: supplied Configuration/DB dataset"));
        assert!(report.contains("Validation issues: 4"));
        assert!(report.contains("monster.csv record #1 has invalid numeric ID"));
        assert!(report.contains("Spawns file #1 record #1 field id has invalid numeric ID"));
        assert!(report.contains("Quests file #1 record #1 is missing field ID"));
        assert!(report
            .contains("Quests file #1 record #1 target field ObjectID1 has invalid numeric ID"));
    }
}
