use crate::quest::{fields, number, one, validate_lifecycle_definition};
use roxmltree::Document;
use std::io;
use std::path::Path;

const ITEM_ID: u32 = 60_060_000;

/// Validate the bounded quest reward inputs before any save is opened.
pub(crate) fn validate(db: &Path, saga_map_xml: &Path) -> io::Result<()> {
    validate_lifecycle_definition(db)
        .map_err(|_| invalid("quest reward definition is missing or unsupported"))?;
    let quest_source = std::fs::read_to_string(db.join("Quests").join("QuestDB_1.xml"))
        .map_err(|_| invalid("quest reward source is missing or unreadable"))?;
    let config_source = std::fs::read_to_string(saga_map_xml)
        .map_err(|_| invalid("quest reward configuration is missing or unreadable"))?;
    validate_quest_rewards(&quest_source)?;
    let quest_rate = config_value(&config_source, "QuestRate")?;
    let gold_rate = config_value(&config_source, "QuestGoldRate")?;
    if quest_rate != 100 || gold_rate != 600 {
        return Err(invalid("quest reward rates are unsupported"));
    }
    validate_item(&db.join("item.csv"))
}

fn validate_quest_rewards(source: &str) -> io::Result<()> {
    let doc = Document::parse(source).map_err(|_| invalid("quest reward XML is malformed"))?;
    let root = doc.root_element();
    if root.tag_name().name() != "QuestDB" || root.tag_name().namespace().is_some() {
        return Err(invalid("quest reward definition is unsupported"));
    }
    let mut target = None;
    for quest in root
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "Quest")
    {
        let ids = fields(quest, "ID").map_err(|_| invalid("quest identifier is malformed"))?;
        if ids.len() != 1 {
            return Err(invalid("quest identifier is missing or duplicated"));
        }
        let id = number(ids[0]).map_err(|_| invalid("quest identifier is malformed"))?;
        if id == 10_000_001 && target.replace(quest).is_some() {
            return Err(invalid("target quest definition is duplicated"));
        }
    }
    let quest = target.ok_or_else(|| invalid("target quest definition is missing"))?;
    for (name, expected) in [
        ("MinLV", 255),
        ("EXP", 55),
        ("JEXP", 55),
        ("Gold", 80),
        ("Fame", 1),
        ("RewardItem", ITEM_ID),
        ("RewardCount", 1),
    ] {
        let value = one(quest, name)
            .and_then(number)
            .map_err(|_| invalid("target quest reward field is missing or malformed"))?;
        if value != expected {
            return Err(invalid("target quest reward data is unsupported"));
        }
    }
    let cp = fields(quest, "CP").map_err(|_| invalid("target quest CP field is malformed"))?;
    if cp.len() > 1 || cp.first().is_some_and(|node| number(*node).ok() != Some(0)) {
        return Err(invalid("target quest CP reward is unsupported"));
    }
    Ok(())
}

fn validate_item(path: &Path) -> io::Result<()> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .comment(Some(b'#'))
        .from_path(path)
        .map_err(|_| invalid("quest reward catalog is missing or unreadable"))?;
    let mut found = false;
    for result in reader.byte_records() {
        let record =
            result.map_err(|_| invalid("quest reward catalog contains an unreadable record"))?;
        let Some(raw_id) = record.get(0) else {
            continue;
        };
        let trimmed = raw_id.trim_ascii();
        let candidate = trimmed.strip_prefix(b"+").unwrap_or(trimmed);
        let id = decimal_bytes(candidate);
        if id == Some(ITEM_ID) && raw_id != b"60060000" {
            return Err(invalid("quest reward catalog item ID is malformed"));
        }
        if id != Some(ITEM_ID) {
            continue;
        }
        if found {
            return Err(invalid("quest reward catalog item is duplicated"));
        }
        if record.get(1) != Some(b"60060000".as_slice())
            || record.get(4) != Some(b"SPEAR".as_slice())
            || record.get(16) != Some(b"0".as_slice())
            || record.get(20) != Some(b"0".as_slice())
        {
            return Err(invalid("quest reward catalog item is unsupported"));
        }
        found = true;
    }
    if !found {
        return Err(invalid("quest reward catalog item is missing"));
    }
    Ok(())
}

fn config_value(source: &str, name: &str) -> io::Result<u32> {
    let doc = Document::parse(source)
        .map_err(|_| invalid("quest reward configuration XML is malformed"))?;
    let root = doc.root_element();
    if root.tag_name().name() != "SagaMap" || root.tag_name().namespace().is_some() {
        return Err(invalid("quest reward configuration is unsupported"));
    }
    let matches = root
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == name)
        .collect::<Vec<_>>();
    if matches.len() != 1
        || matches
            .iter()
            .any(|node| node.tag_name().namespace().is_some())
    {
        return Err(invalid(
            "quest reward rate is missing, duplicated or malformed",
        ));
    }
    let node = matches[0];
    if node.attributes().next().is_some()
        || node.children().count() != 1
        || !node.first_child().is_some_and(|child| child.is_text())
    {
        return Err(invalid("quest reward rate is malformed"));
    }
    decimal(node.text().unwrap_or("")).ok_or_else(|| invalid("quest reward rate is malformed"))
}

fn decimal(raw: &str) -> Option<u32> {
    (!raw.is_empty() && raw.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| raw.parse().ok())
        .flatten()
}

fn decimal_bytes(raw: &[u8]) -> Option<u32> {
    (!raw.is_empty() && raw.iter().all(u8::is_ascii_digit))
        .then(|| std::str::from_utf8(raw).ok()?.parse().ok())
        .flatten()
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::validate;
    use std::fs;
    use std::io;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    const QUEST: &str = "<QuestDB GroupID='1'><Quest><ID>10000001</ID><Type>HUNT</Type><Time>60</Time><MapID1>10025000</MapID1><ObjectID1>10000000</ObjectID1><Count1>5</Count1><Party>False</Party><RequiredQuestPoints>1</RequiredQuestPoints><DungeonID>0</DungeonID><MinLV>255</MinLV><MaxLv>255</MaxLv><Job>NONE</Job><JobType>NOVICE</JobType><Race>NONE</Race><Gender>NONE</Gender><EXP>55</EXP><JEXP>55</JEXP><Gold>80</Gold><Fame>1</Fame><RewardItem>60060000</RewardItem><RewardCount>1</RewardCount></Quest></QuestDB>";
    const CONFIG: &str =
        "<SagaMap><QuestRate>100</QuestRate><QuestGoldRate>600</QuestGoldRate></SagaMap>";
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "eco-quest-reward-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(path.join("Quests")).unwrap();
            Self(path)
        }
        fn write(&self, path: &str, text: &str) {
            fs::write(self.0.join(path), text).unwrap();
        }
        fn check(&self, quest: &str, config: &str, catalog: &str) -> io::Result<()> {
            self.write("Quests/QuestDB_1.xml", quest);
            self.write("SagaMap.xml", config);
            self.write("item.csv", catalog);
            validate(&self.0, &self.0.join("SagaMap.xml"))
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn item() -> String {
        let mut fields = vec!["0"; 21];
        fields[0] = "60060000";
        fields[1] = "60060000";
        fields[4] = "SPEAR";
        fields[16] = "0";
        fields[20] = "0";
        fields.join(",")
    }

    #[test]
    fn accepts_supported_reward_inputs() {
        let temp = Temp::new();
        assert!(temp.check(QUEST, CONFIG, &item()).is_ok());
        assert!(temp
            .check(
                &QUEST.replace("</Quest>", "<CP>0</CP></Quest>"),
                CONFIG,
                &item()
            )
            .is_ok());
        // Unrelated catalog text need not be UTF-8.
        fs::write(
            temp.0.join("item.csv"),
            [item().as_bytes(), b"\n#\xff\xfe"].concat(),
        )
        .unwrap();
        assert!(validate(&temp.0, &temp.0.join("SagaMap.xml")).is_ok());
    }

    #[test]
    fn refuses_missing_duplicate_and_malformed_quest_fields_privately() {
        let temp = Temp::new();
        for quest in [
            QUEST.replace("<MinLV>255</MinLV>", ""),
            QUEST.replace("<MinLV>255</MinLV>", "<MinLV>255</MinLV><MinLV>255</MinLV>"),
            QUEST.replace("<MinLV>255</MinLV>", "<MinLV>private-marker</MinLV>"),
            QUEST.replace("<Fame>1</Fame>", "<Fame>1</Fame><Fame>1</Fame>"),
            QUEST.replace("<EXP>55</EXP>", "<EXP>54</EXP>"),
            QUEST.replace("<Fame>1</Fame>", "<Fame a='x'>1</Fame>"),
            QUEST.replace("<MinLV>255</MinLV>", "<MinLV>255<!--hidden--></MinLV>"),
            QUEST.replace(
                "<MinLV>255</MinLV>",
                "<x:MinLV xmlns:x='urn:test'>255</x:MinLV>",
            ),
        ] {
            let error = temp.check(&quest, CONFIG, &item()).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(!error.to_string().contains("private-marker"));
        }
        assert!(temp
            .check("<QuestDB GroupID='1'></QuestDB>", CONFIG, &item())
            .is_err());
    }

    #[test]
    fn refuses_config_and_catalog_ambiguity_or_corruption() {
        let temp = Temp::new();
        for config in [
            "<SagaMap><QuestRate>100</QuestRate></SagaMap>",
            "<SagaMap><QuestRate>100</QuestRate><QuestRate>100</QuestRate><QuestGoldRate>600</QuestGoldRate></SagaMap>",
            "<SagaMap><QuestRate>100<!--private-marker--></QuestRate><QuestGoldRate>600</QuestGoldRate></SagaMap>",
            "<SagaMap><QuestRate xmlns='urn:test'>100</QuestRate><QuestGoldRate>600</QuestGoldRate></SagaMap>",
        ] { assert!(temp.check(QUEST, config, &item()).is_err()); }
        let mut bad_stack = item().split(',').map(str::to_owned).collect::<Vec<_>>();
        bad_stack[16] = "1".into();
        let malformed_extra = format!("{}\n{}", item(), item().replace("60060000,", " 60060000 ,"));
        let catalogs = [
            String::new(),
            format!("{}\n{}", item(), item()),
            item().replacen(",SPEAR,", ",BODY,", 1),
            bad_stack.join(","),
            item().replace("60060000,60060000", " 60060000 ,60060000"),
            malformed_extra,
        ];
        for catalog in catalogs {
            assert!(temp.check(QUEST, CONFIG, &catalog).is_err());
        }
    }

    #[test]
    fn all_reward_scalars_and_catalog_defaults_are_bounded() {
        let temp = Temp::new();
        for (name, original) in [
            ("EXP", 55),
            ("JEXP", 55),
            ("Gold", 80),
            ("Fame", 1),
            ("RewardItem", 60060000),
            ("RewardCount", 1),
        ] {
            let field = format!("<{name}>{original}</{name}>");
            for replacement in [
                String::new(),
                format!("{field}{field}"),
                format!("<{name}>4294967296</{name}>"),
                format!("<{name}>+{original}</{name}>"),
                format!("<{name}>{}</{name}>", original + 1),
            ] {
                assert!(temp
                    .check(&QUEST.replace(&field, &replacement), CONFIG, &item())
                    .is_err());
            }
        }
        for cp in [
            "<CP>1</CP>",
            "<CP>-1</CP>",
            "<CP>0</CP><CP>0</CP>",
            "<CP>private-marker</CP>",
        ] {
            assert!(temp
                .check(
                    &QUEST.replace("</Quest>", &format!("{cp}</Quest>")),
                    CONFIG,
                    &item()
                )
                .is_err());
        }
        for column in [1, 16, 20] {
            let mut row = item().split(',').map(str::to_owned).collect::<Vec<_>>();
            row[column] = "1".into();
            assert!(temp.check(QUEST, CONFIG, &row.join(",")).is_err());
        }
        assert!(temp
            .check(QUEST, &CONFIG.replace("600", "100"), &item())
            .is_err());
        assert!(temp
            .check(QUEST, &CONFIG.replace(">100<", ">101<"), &item())
            .is_err());
    }
}
