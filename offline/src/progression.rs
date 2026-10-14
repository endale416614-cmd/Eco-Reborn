use roxmltree::{Document, Node};
use std::io;
use std::path::Path;

/// Validate the supplied cumulative novice EXP thresholds through level 3.
pub fn validate_novice_exp_table(db: &Path) -> io::Result<()> {
    let source = std::fs::read_to_string(db.join("exp.xml"))
        .map_err(|_| invalid_table("experience table is missing or unreadable"))?;
    let document =
        Document::parse(&source).map_err(|_| invalid_table("experience table XML is invalid"))?;
    let root = document.root_element();
    if root.tag_name().name() != "exp"
        || root.tag_name().namespace().is_some()
        || has_attributes(root)
    {
        return Err(invalid_table("experience table root is invalid"));
    }

    let mut levels = std::collections::BTreeSet::new();
    let mut level_one = None;
    let mut level_two = None;
    let mut level_three = None;
    for child in root.children().filter(Node::is_element) {
        if child.tag_name().name() != "level"
            || child.tag_name().namespace().is_some()
            || has_attributes(child)
        {
            return Err(invalid_table("experience table record is invalid"));
        }
        let lv = field(child, "lv", &source)?;
        let level =
            decimal(lv).ok_or_else(|| invalid_table("experience table level is invalid"))?;
        if !levels.insert(level) {
            return Err(invalid_table("experience table has duplicate levels"));
        }
        if (1..=3).contains(&level) {
            let c = decimal(field(child, "c", &source)?)
                .ok_or_else(|| invalid_table("base experience threshold is invalid"))?;
            let j1 = decimal(field(child, "j1", &source)?)
                .ok_or_else(|| invalid_table("first-job experience threshold is invalid"))?;
            match level {
                1 => level_one = Some((c, j1)),
                2 => level_two = Some((c, j1)),
                3 => level_three = Some((c, j1)),
                _ => unreachable!(),
            }
        }
    }
    if level_one != Some((0, 0)) || level_two != Some((55, 55)) || level_three != Some((155, 155)) {
        return Err(invalid_table(
            "novice experience thresholds are unsupported",
        ));
    }
    Ok(())
}

/// Validate the source map flag used by status recalculation; an omitted row retains its source default 0.
pub fn validate_novice_map_flags(db: &Path) -> io::Result<()> {
    let source = std::fs::read_to_string(db.join("MapFlags.xml"))
        .map_err(|_| invalid_table("map flags are missing or unreadable"))?;
    let document =
        Document::parse(&source).map_err(|_| invalid_table("map flags XML is invalid"))?;
    let root = document.root_element();
    if root.tag_name().name() != "MapFlags"
        || root.tag_name().namespace().is_some()
        || has_attributes(root)
    {
        return Err(invalid_table("map flags root is invalid"));
    }
    let mut target_flag = None;
    for map in root.children().filter(Node::is_element) {
        if !map.tag_name().name().eq_ignore_ascii_case("Map") {
            continue;
        }
        if map.tag_name().namespace().is_some()
            || map.attributes().count() != 1
            || map
                .attributes()
                .any(|attribute| attribute.name() != "MapID" || attribute.namespace().is_some())
            || map.children().count() != 1
            || !map.first_child().is_some_and(|child| child.is_text())
        {
            return Err(invalid_table("map flag record is invalid"));
        }
        let id = map
            .attribute("MapID")
            .and_then(decimal)
            .ok_or_else(|| invalid_table("map flag ID is invalid"))?;
        let flag = decimal(map.text().unwrap_or(""))
            .ok_or_else(|| invalid_table("map flag value is invalid"))?;
        if id == 20_013_003 && target_flag.replace(flag).is_some() {
            return Err(invalid_table("map flag ID is duplicated"));
        }
    }
    if target_flag.unwrap_or(0) != 0 {
        return Err(invalid_table("novice map flag is unsupported"));
    }
    Ok(())
}

/// Read the six configured starting attributes for one supported SagaECO race.
pub fn start_stats(path: &Path) -> io::Result<[[u32; 6]; 3]> {
    let source = std::fs::read_to_string(path)
        .map_err(|_| invalid_table("login configuration is missing or unreadable"))?;
    let document = Document::parse(&source)
        .map_err(|_| invalid_table("login configuration XML is invalid"))?;
    let root = document.root_element();
    if root.tag_name().name() != "SagaLogin"
        || root.tag_name().namespace().is_some()
        || has_attributes(root)
    {
        return Err(invalid_table("login configuration root is invalid"));
    }
    let mut found = [None; 3];
    let mut dem_found = false;
    for status in root
        .children()
        .filter(Node::is_element)
        .filter(|node| node.tag_name().name().eq_ignore_ascii_case("StartStatus"))
    {
        if status.tag_name().namespace().is_some()
            || status.attributes().count() != 1
            || status
                .attributes()
                .any(|attribute| attribute.name() != "race" || attribute.namespace().is_some())
        {
            return Err(invalid_table("login race record is invalid"));
        }
        let race_name = status
            .attribute("race")
            .ok_or_else(|| invalid_table("login race record is invalid"))?;
        if !["Emil", "Titania", "Dominion", "DEM"].contains(&race_name) {
            return Err(invalid_table("login race record is invalid"));
        }
        {
            let names = ["STR", "DEX", "INT", "VIT", "AGI", "MAG"];
            let mut values = [0; 6];
            for (index, name) in names.iter().enumerate() {
                let value = field(status, name, &source)?;
                values[index] = decimal(value)
                    .filter(|value| *value > 0)
                    .ok_or_else(|| invalid_table("login starting attribute is invalid"))?;
            }
            if let Some(index) = ["Emil", "Titania", "Dominion"]
                .iter()
                .position(|name| *name == race_name)
            {
                if found[index].replace(values).is_some() {
                    return Err(invalid_table("login race record is duplicated"));
                }
            } else if std::mem::replace(&mut dem_found, true) {
                return Err(invalid_table("login race record is duplicated"));
            }
        }
    }
    if found.iter().any(Option::is_none) || !dem_found {
        return Err(invalid_table("login race record is missing"));
    }
    let values = found.map(Option::unwrap);
    if values
        != [
            [8, 3, 3, 10, 4, 3],
            [6, 3, 6, 4, 2, 10],
            [10, 5, 1, 1, 5, 8],
        ]
    {
        return Err(invalid_table("login starting attributes are unsupported"));
    }
    Ok(values)
}

/// Validate supplied starter equipment and return ordinary race/gender item IDs.
/// The final two positions are the two BODY items, in source configuration order.
pub fn start_items(path: &Path) -> io::Result<[[[u32; 5]; 2]; 3]> {
    let source = std::fs::read_to_string(path)
        .map_err(|_| invalid_table("login configuration is missing or unreadable"))?;
    let document = Document::parse(&source)
        .map_err(|_| invalid_table("login configuration XML is invalid"))?;
    let root = document.root_element();
    if root.tag_name().name() != "SagaLogin"
        || root.tag_name().namespace().is_some()
        || has_attributes(root)
    {
        return Err(invalid_table("login configuration root is invalid"));
    }

    let mut found = [[[None; 5]; 2]; 3];
    let mut dem_found = [false; 2];
    let slots = ["UPPER_BODY", "LOWER_BODY", "SHOES", "BODY", "BODY"];
    let ordinary = [
        [
            [50000000, 50010300, 50060150, 10020114, 60010082],
            [50000055, 50010300, 50060100, 10020114, 60010082],
        ],
        [
            [50000180, 50010456, 50060150, 10020114, 60010082],
            [50001381, 50012360, 50060100, 10020114, 60010082],
        ],
        [
            [50002754, 50010253, 50060150, 10020114, 60010082],
            [50001454, 50010155, 50060100, 10020114, 60010082],
        ],
    ];
    let dem = [50111700, 50000950, 50068550, 50060350, 10020114];
    let dem_slots = ["FACE_ACCE", "UPPER_BODY", "LOWER_BODY", "SHOES", "BODY"];

    for record in root.children().filter(Node::is_element) {
        if !record.tag_name().name().eq_ignore_ascii_case("StartItem") {
            continue;
        }
        if record.tag_name().namespace().is_some()
            || record.attributes().count() != 2
            || record
                .attributes()
                .any(|a| a.namespace().is_some() || !["race", "gender"].contains(&a.name()))
        {
            return Err(invalid_table("login starter item record is invalid"));
        }
        let race = record.attribute("race").unwrap();
        let gender = record.attribute("gender").unwrap();
        let race_index = match race {
            "Emil" => Some(0),
            "Titania" => Some(1),
            "Dominion" => Some(2),
            "DEM" => None,
            _ => return Err(invalid_table("login starter item record is invalid")),
        };
        let gender_index = match gender {
            "Male" => 0,
            "Female" => 1,
            _ => return Err(invalid_table("login starter item record is invalid")),
        };
        let mut values = Vec::new();
        for item in record.children().filter(Node::is_element) {
            if item.tag_name().name() != "Item"
                || item.tag_name().namespace().is_some()
                || has_attributes(item)
                || item.children().filter(Node::is_element).count() != 3
                || item
                    .children()
                    .filter(Node::is_element)
                    .map(|node| node.tag_name().name())
                    .collect::<Vec<_>>()
                    != ["ItemID", "Slot", "Count"]
            {
                return Err(invalid_table("login starter item is invalid"));
            }
            let id = decimal(field(item, "ItemID", &source)?)
                .ok_or_else(|| invalid_table("login starter item is invalid"))?;
            let slot = field(item, "Slot", &source)?;
            let count = decimal(field(item, "Count", &source)?)
                .ok_or_else(|| invalid_table("login starter item is invalid"))?;
            values.push((id, slot, count));
        }
        if values.len() != 5 {
            return Err(invalid_table("login starter item group is invalid"));
        }
        if let Some(race_index) = race_index {
            if found[race_index][gender_index].iter().any(Option::is_some) {
                return Err(invalid_table("login starter item group is duplicated"));
            }
            let mut ids = std::collections::BTreeSet::new();
            let mut parsed = [0; 5];
            for (index, (id, slot, count)) in values.into_iter().enumerate() {
                if !ids.insert(id) || count != 1 || slot != slots[index] {
                    return Err(invalid_table("login starter item is unsupported"));
                }
                parsed[index] = id;
            }
            if parsed != ordinary[race_index][gender_index] {
                return Err(invalid_table("login starter item is unsupported"));
            }
            found[race_index][gender_index] = parsed.map(Some);
        } else {
            if std::mem::replace(&mut dem_found[gender_index], true) {
                return Err(invalid_table("login starter item group is duplicated"));
            }
            let mut ids = std::collections::BTreeSet::new();
            for (index, (id, slot, count)) in values.into_iter().enumerate() {
                if !ids.insert(id) || count != 1 || slot != dem_slots[index] || id != dem[index] {
                    return Err(invalid_table("login starter item is unsupported"));
                }
            }
        }
    }
    if found
        .iter()
        .flatten()
        .any(|group| group.iter().any(Option::is_none))
        || dem_found.iter().any(|found| !found)
    {
        return Err(invalid_table("login starter item group is missing"));
    }
    Ok(found.map(|race| race.map(|group| group.map(Option::unwrap))))
}

fn field<'a>(record: Node<'a, 'a>, name: &str, source: &'a str) -> io::Result<&'a str> {
    let mut matches = record
        .children()
        .filter(Node::is_element)
        .filter(|node| node.tag_name().name() == name);
    let node = matches
        .next()
        .ok_or_else(|| invalid_table("experience table field is missing"))?;
    if matches.next().is_some()
        || node.tag_name().namespace().is_some()
        || has_attributes(node)
        || node.children().count() != 1
        || !node.first_child().is_some_and(|child| child.is_text())
    {
        return Err(invalid_table("experience table field is invalid"));
    }
    let raw = source
        .get(node.range())
        .ok_or_else(|| invalid_table("experience table field is invalid"))?;
    let content_start = raw
        .find('>')
        .map(|index| index + 1)
        .ok_or_else(|| invalid_table("experience table field is invalid"))?;
    let content_end = raw
        .rfind("</")
        .ok_or_else(|| invalid_table("experience table field is invalid"))?;
    let expected_close = format!("</{name}>");
    if raw.get(content_end..) != Some(expected_close.as_str()) {
        return Err(invalid_table("experience table field is invalid"));
    }
    raw.get(content_start..content_end)
        .ok_or_else(|| invalid_table("experience table field is invalid"))
}

fn decimal(value: &str) -> Option<u32> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

fn has_attributes(node: Node<'_, '_>) -> bool {
    node.attributes().next().is_some()
}

fn invalid_table(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::{start_items, start_stats, validate_novice_exp_table, validate_novice_map_flags};
    use std::fs;
    use std::io;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

    struct TempDb(PathBuf);

    impl TempDb {
        fn new(xml: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "eco-exp-{}-{}",
                std::process::id(),
                NEXT_DIR.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            fs::write(path.join("exp.xml"), xml).unwrap();
            Self(path)
        }

        fn empty() -> Self {
            let path = std::env::temp_dir().join(format!(
                "eco-exp-{}-{}",
                std::process::id(),
                NEXT_DIR.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn unreadable() -> Self {
            let db = Self::empty();
            fs::create_dir(db.0.join("exp.xml")).unwrap();
            db
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn table(changes: &str) -> String {
        format!("<exp><level><lv>1</lv><c>0</c><j1>0</j1><j2>0</j2></level><level><lv>2</lv><c>55</c><j1>55</j1><j2>318</j2></level><level><lv>3</lv><c>155</c><j1>155</j1><j2>954</j2></level>{changes}</exp>")
    }

    fn check(xml: &str) -> io::Result<()> {
        let db = TempDb::new(xml);
        validate_novice_exp_table(&db.0)
    }

    #[test]
    fn accepts_scoped_thresholds_with_other_levels() {
        assert!(check(&table(
            "<level><lv>4</lv><c>346</c><j1>346</j1><j2>1787</j2></level>"
        ))
        .is_ok());
    }

    #[test]
    fn rejects_mutated_level_three_threshold() {
        for xml in [
            table("").replace("<c>155</c>", "<c>154</c>"),
            table("").replace("<j1>155</j1>", "<j1>156</j1>"),
        ] {
            let db = TempDb::new(&xml);
            assert_eq!(
                validate_novice_exp_table(&db.0).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
    }

    fn map_flags(xml: &str) -> io::Result<()> {
        let db = TempDb::empty();
        fs::write(db.0.join("MapFlags.xml"), xml).unwrap();
        validate_novice_map_flags(&db.0)
    }

    #[test]
    fn validates_target_map_flags_and_source_default() {
        assert!(map_flags("<MapFlags><Map MapID=\"1\">32</Map></MapFlags>").is_ok());
        assert!(map_flags("<MapFlags><Map MapID=\"20013003\">0</Map></MapFlags>").is_ok());
        assert!(map_flags("<MapFlags/>").is_ok());
        for xml in [
            "<MapFlags><Map MapID=\"20013003\">32</Map></MapFlags>",
            "<MapFlags><Map MapID=\"20013003\">0</Map><Map MapID=\"20013003\">0</Map></MapFlags>",
            "<MapFlags><Map MapID=\"bad\">0</Map></MapFlags>",
            "<MapFlags><Map MapID=\"20013003x\">0</Map></MapFlags>",
            "<MapFlags><Map x:MapID=\"20013003\" xmlns:x=\"urn:x\">0</Map></MapFlags>",
            "<MapFlags xmlns=\"urn:x\"><Map MapID=\"20013003\">0</Map></MapFlags>",
            "<MapFlags><Map MapID=\"20013003\"><Value>0</Value></Map></MapFlags>",
        ] {
            assert_eq!(
                map_flags(xml).unwrap_err().kind(),
                io::ErrorKind::InvalidData,
                "{xml}"
            );
        }
    }

    #[test]
    fn matches_source_map_tag_case_and_validates_target_record() {
        assert!(map_flags("<MapFlags><map MapID=\"20013003\">0</map></MapFlags>").is_ok());
        assert!(map_flags("<MapFlags><mAp MapID=\"20013003\">0</mAp></MapFlags>").is_ok());
        for xml in [
            "<MapFlags><map MapID=\"20013003\">32</map></MapFlags>",
            "<MapFlags><mAp MapID=\"20013003\">32</mAp></MapFlags>",
            "<MapFlags><MaP MapID=\"20013003\">0</MaP><map MapID=\"20013003\">0</map></MapFlags>",
            "<MapFlags><map MapID=\"20013003\">PRIVATE_MAP_FLAG_VALUE</map></MapFlags>",
        ] {
            let error = map_flags(xml).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{xml}");
            assert!(!error.to_string().contains("PRIVATE_MAP_FLAG_VALUE"));
        }
        assert!(map_flags(
            "<MapFlags><Other>PRIVATE_MAP_FLAG_VALUE</Other><map MapID=\"1\">32</map></MapFlags>"
        )
        .is_ok());
        assert!(map_flags("<MapFlags/>").is_ok());
    }

    #[test]
    fn accepts_supplied_sagaeco_table() {
        let db =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../research/SagaECO/Configuration/DB");
        assert!(validate_novice_exp_table(&db).is_ok());
        assert!(validate_novice_map_flags(&db).is_ok());
    }

    #[test]
    fn reads_all_supplied_ordinary_race_stats() {
        let config = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../research/SagaECO/Configuration/Config/SagaLogin.xml");
        assert_eq!(
            start_stats(&config).unwrap(),
            [
                [8, 3, 3, 10, 4, 3],
                [6, 3, 6, 4, 2, 10],
                [10, 5, 1, 1, 5, 8]
            ]
        );
    }

    #[test]
    fn selects_login_records_case_insensitively_like_saga_login() {
        let config = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../research/SagaECO/Configuration/Config/SagaLogin.xml");
        let source = fs::read_to_string(config).unwrap();
        let mixed = source
            .replace("<StartStatus", "<sTaRtStAtUs")
            .replace("</StartStatus>", "</sTaRtStAtUs>")
            .replace("<StartItem", "<sTaRtItEm")
            .replace("</StartItem>", "</sTaRtItEm>");
        let read = |xml: &str| {
            let db = TempDb::new(xml);
            (
                start_stats(&db.0.join("exp.xml")),
                start_items(&db.0.join("exp.xml")),
            )
        };
        let baseline = read(&source);
        let mixed_result = read(&mixed);
        assert_eq!(mixed_result.0.unwrap(), baseline.0.unwrap());
        assert_eq!(mixed_result.1.unwrap(), baseline.1.unwrap());

        let duplicate_status = "<sTaRtStAtUs race='Emil'><STR>8</STR><DEX>3</DEX><INT>3</INT><VIT>10</VIT><AGI>4</AGI><MAG>3</MAG></sTaRtStAtUs>";
        let duplicate_item = "<sTaRtItEm race='Emil' gender='Male'><Item><ItemID>50000000</ItemID><Slot>UPPER_BODY</Slot><Count>1</Count></Item><Item><ItemID>50010300</ItemID><Slot>LOWER_BODY</Slot><Count>1</Count></Item><Item><ItemID>50060150</ItemID><Slot>SHOES</Slot><Count>1</Count></Item><Item><ItemID>10020114</ItemID><Slot>BODY</Slot><Count>1</Count></Item><Item><ItemID>60010082</ItemID><Slot>BODY</Slot><Count>1</Count></Item></sTaRtItEm>";
        let with_duplicate_status =
            source.replace("</SagaLogin>", &format!("{duplicate_status}</SagaLogin>"));
        let with_duplicate_item =
            source.replace("</SagaLogin>", &format!("{duplicate_item}</SagaLogin>"));
        assert!(read(&with_duplicate_status).0.is_err());
        assert!(read(&with_duplicate_item).1.is_err());

        let private_value = "PRIVATE_LOGIN_VALUE";
        let malformed = source.replace(
            "</SagaLogin>",
            &format!(
                "<sTaRtStAtUs race='Emil'><STR>{private_value}</STR></sTaRtStAtUs></SagaLogin>"
            ),
        );
        let error = read(&malformed).0.unwrap_err();
        assert!(!error.to_string().contains(private_value));
    }

    #[test]
    fn reads_supplied_ordinary_starter_items_and_checks_dem_groups() {
        let config = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../research/SagaECO/Configuration/Config/SagaLogin.xml");
        let expected = [
            [
                [50000000, 50010300, 50060150, 10020114, 60010082],
                [50000055, 50010300, 50060100, 10020114, 60010082],
            ],
            [
                [50000180, 50010456, 50060150, 10020114, 60010082],
                [50001381, 50012360, 50060100, 10020114, 60010082],
            ],
            [
                [50002754, 50010253, 50060150, 10020114, 60010082],
                [50001454, 50010155, 50060100, 10020114, 60010082],
            ],
        ];
        assert_eq!(start_items(&config).unwrap(), expected);
    }

    #[test]
    fn rejects_mutated_or_ambiguous_starter_items() {
        let config = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../research/SagaECO/Configuration/Config/SagaLogin.xml");
        let valid = fs::read_to_string(config).unwrap();
        let cases = [
            valid.replace("<Count>1</Count>", "<Count>2</Count>"),
            valid.replace("<ItemID>50000000</ItemID>", "<ItemID>50000001</ItemID>"),
            valid.replace("<Slot>UPPER_BODY</Slot>", "<Slot>LOWER_BODY</Slot>"),
            valid.replace(
                "race=\"Emil\" gender=\"Male\"",
                "race=\"Emil\" gender=\"Female\"",
            ),
            valid.replace("<ItemID>50000000</ItemID>", "<ItemID>50010300</ItemID>"),
            valid.replace(
                "<ItemID>50000000</ItemID>",
                "<ItemID>50000000</ItemID><Extra>1</Extra>",
            ),
            valid.replace(
                "<StartItem race=\"DEM\" gender=\"Male\">",
                "<StartItem race=\"DEM\" gender=\"Male\" extra=\"x\">",
            ),
            valid.replace("<Slot>FACE_ACCE</Slot>", "<Slot>NOT_A_SLOT</Slot>"),
            valid.replace(
                "<ItemID>50000000</ItemID>",
                "<ItemID xmlns=\"urn:x\">50000000</ItemID>",
            ),
        ];
        for xml in cases {
            let db = TempDb::new(&xml);
            assert_eq!(
                start_items(&db.0.join("exp.xml")).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
    }

    #[test]
    fn rejects_missing_duplicate_or_malformed_race_stats() {
        let valid = "<SagaLogin><StartStatus race='Emil'><STR>8</STR><DEX>3</DEX><INT>3</INT><VIT>10</VIT><AGI>4</AGI><MAG>3</MAG></StartStatus><StartStatus race='Titania'><STR>6</STR><DEX>3</DEX><INT>6</INT><VIT>4</VIT><AGI>2</AGI><MAG>10</MAG></StartStatus><StartStatus race='Dominion'><STR>10</STR><DEX>5</DEX><INT>1</INT><VIT>1</VIT><AGI>5</AGI><MAG>8</MAG></StartStatus><StartStatus race='DEM'><STR>5</STR><DEX>5</DEX><INT>5</INT><VIT>5</VIT><AGI>5</AGI><MAG>5</MAG></StartStatus></SagaLogin>";
        let cases = vec![
            "<SagaLogin/>".to_string(),
            valid.replace("</SagaLogin>", "<StartStatus race='Emil'><STR>8</STR><DEX>3</DEX><INT>3</INT><VIT>10</VIT><AGI>4</AGI><MAG>3</MAG></StartStatus></SagaLogin>"),
            valid.replace("<STR>8</STR>", "<STR>9</STR>"),
            valid.replace("<STR>8</STR>", "<STR>8</STR><STR>8</STR>"),
            valid.replace("<STR>8</STR>", "<STR>8<![CDATA[0]]></STR>"),
            valid.replace("<STR>8</STR>", "<STR a='x'>8</STR>"),
            valid.replace("race='Dominion'", "race='Unknown'"),
            valid.replace("race='Emil'", "race='s:Emil'").replace("race='Titania'", "race='s:Titania'").replace("race='Dominion'", "race='s:Dominion'").replace("race='DEM'", "race='s:DEM'"),
            valid.replace("<StartStatus race='Emil'>", "<StartStatus xmlns='urn:x' race='Emil'>"),
            valid.replace("race='Emil'", "x:race='Emil' xmlns:x='urn:x'"),
            valid.replace("<SagaLogin>", "<SagaLogin bad='x'>"),
        ];
        for source in cases {
            let db = TempDb::new(&source);
            assert_eq!(
                start_stats(&db.0.join("exp.xml")).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
    }

    #[test]
    fn missing_table_error_is_private() {
        let db = TempDb::empty();
        let error = validate_novice_exp_table(&db.0).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(
            error.to_string(),
            "experience table is missing or unreadable"
        );
        assert!(!error.to_string().contains(db.0.to_string_lossy().as_ref()));
    }

    #[test]
    fn unreadable_table_error_is_private() {
        let db = TempDb::unreadable();
        let error = validate_novice_exp_table(&db.0).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(
            error.to_string(),
            "experience table is missing or unreadable"
        );
        assert!(!error.to_string().contains(db.0.to_string_lossy().as_ref()));
    }

    #[test]
    fn rejects_malformed_or_ambiguous_tables_privately() {
        let valid_third = "<level><lv>3</lv><c>155</c><j1>155</j1></level>";
        let cases = [
            "<exp><level><lv>1</lv><c>0</c><j1>0</j1></level>",
            "<exp><level><lv>1</lv><c>0</c><j1>0</j1></level><level><lv>2</lv><c>55</c><j1>55</j1></level><level><lv>01</lv><c>0</c><j1>0</j1></level></exp>",
            "<exp><level><lv>1</lv><c>0</c><j1>0</j1></level><level><lv>2</lv><c>56</c><j1>55</j1></level></exp>",
            "<exp><level n='1'><lv>1</lv><c>0</c><j1>0</j1></level><level><lv>2</lv><c>55</c><j1>55</j1></level></exp>",
            "<exp xmlns='urn:test'><level><lv>1</lv><c>0</c><j1>0</j1></level><level><lv>2</lv><c>55</c><j1>55</j1></level></exp>",
            "<exp><level><lv>1</lv><c>0</c><j1>0</j1><j1>0</j1></level><level><lv>2</lv><c>55</c><j1>55</j1></level></exp>",
            "<exp><level><lv>1</lv><c>0<![CDATA[0]]></c><j1>0</j1></level><level><lv>2</lv><c>55</c><j1>55</j1></level></exp>",
            "<exp><level><lv><x>1</x></lv><c>0</c><j1>0</j1></level><level><lv>2</lv><c>55</c><j1>55</j1></level></exp>",
            "<exp><level><lv>1</lv><c a='b'>0</c><j1>0</j1></level><level><lv>2</lv><c>55</c><j1>55</j1></level></exp>",
            "<exp><level><lv>1</lv><c>0</c><j1 xmlns='urn:test'>0</j1></level><level><lv>2</lv><c>55</c><j1>55</j1></level></exp>",
            "<exp><level><lv>1</lv><c>0</c><j1>0</j1></level><level><lv>2</lv><c>55</c><j1>55</j1></level>",
        ];
        assert!(check(&table("")).is_ok());
        for (index, xml) in cases.into_iter().enumerate() {
            let xml = xml.replace("</exp>", &format!("{valid_third}</exp>"));
            let error = match check(&xml) {
                Err(error) => error,
                Ok(()) => panic!("case {index} unexpectedly accepted"),
            };
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(!error.to_string().contains("<exp>"));
        }
    }
}
