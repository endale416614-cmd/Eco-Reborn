use std::collections::BTreeMap;
use std::io;
use std::path::Path;

const STARTER_ITEMS: &[(u32, &str, bool, u16)] = &[
    (50000000, "ARMOR_UPPER", false, 5),
    (50000055, "ARMOR_UPPER", false, 5),
    (50000180, "ARMOR_UPPER", false, 5),
    (50001381, "ARMOR_UPPER", false, 5),
    (50001454, "ARMOR_UPPER", false, 5),
    (50002754, "ARMOR_UPPER", false, 5),
    (50010155, "ARMOR_LOWER", false, 5),
    (50010253, "ARMOR_LOWER", false, 5),
    (50010300, "ARMOR_LOWER", false, 5),
    (50010456, "SLACKS", false, 5),
    (50012360, "ARMOR_LOWER", false, 10),
    (50060100, "SHOES", false, 10),
    (50060150, "SHOES", false, 10),
    (10020114, "USE", true, 0),
    (60010082, "SWORD", false, 10),
];

// CalcEquipBonus consumes these catalog fields; instance modifiers are separate.
const STARTER_STATUS_FIELDS: &[usize] = &[39, 40, 41, 45, 46, 47, 48, 49, 50];

/// Validate only catalog fields that determine the configured starter instances.
pub fn validate(path: &Path) -> io::Result<()> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .comment(Some(b'#'))
        .from_path(path)
        .map_err(|_| invalid("could not open starter item catalog"))?;
    let expected = STARTER_ITEMS
        .iter()
        .map(|(id, item_type, stock, durability)| (*id, (*item_type, *stock, *durability)))
        .collect::<BTreeMap<_, _>>();
    let mut found = std::collections::BTreeSet::new();

    for result in reader.byte_records() {
        let record =
            result.map_err(|_| invalid("starter item catalog contains an unreadable record"))?;
        let Some(raw_id) = record
            .get(0)
            .filter(|raw| !raw.is_empty() && !raw.iter().all(u8::is_ascii_whitespace))
        else {
            continue;
        };
        let Some(id) = parse_ascii_u32(raw_id) else {
            let candidate = raw_id
                .trim_ascii()
                .strip_prefix(b"+")
                .unwrap_or(raw_id.trim_ascii());
            if parse_ascii_u32(candidate).is_some_and(|id| expected.contains_key(&id)) {
                return Err(invalid("starter item catalog has an invalid numeric ID"));
            }
            // The supplied catalog also includes non-item notes with no numeric ID.
            continue;
        };
        let Some(spec) = expected.get(&id) else {
            continue;
        };
        if found.contains(&id) {
            return Err(invalid(
                "starter item catalog contains a duplicate starter ID",
            ));
        }

        let item_type = record
            .get(4)
            .ok_or_else(|| invalid("starter item catalog record is missing item type"))?;
        let stock_raw = record
            .get(16)
            .ok_or_else(|| invalid("starter item catalog record is missing stock flag"))?;
        let durability_raw = record
            .get(20)
            .ok_or_else(|| invalid("starter item catalog record is missing durability"))?;
        let stock = match stock_raw {
            b"0" => false,
            b"1" => true,
            _ => {
                return Err(invalid(
                    "starter item catalog has an invalid starter stock flag",
                ))
            }
        };
        let durability = parse_ascii_u16(durability_raw)
            .ok_or_else(|| invalid("starter item catalog has invalid starter durability"))?;
        if matches!(spec.0, "ARMOR_UPPER" | "ARMOR_LOWER" | "SLACKS" | "SHOES")
            && STARTER_STATUS_FIELDS
                .iter()
                .any(|&field| record.get(field) != Some(b"0"))
        {
            return Err(invalid(
                "starter item catalog has invalid equipped-item status values",
            ));
        }
        if item_type != spec.0.as_bytes() || stock != spec.1 || durability != spec.2 {
            return Err(invalid(
                "starter item catalog conflicts with configured starter item data",
            ));
        }
        found.insert(id);
    }

    if found.len() != expected.len() {
        return Err(invalid(
            "starter item catalog is missing a configured starter ID",
        ));
    }
    Ok(())
}

fn parse_ascii_u16(raw: &[u8]) -> Option<u16> {
    (!raw.is_empty() && raw.iter().all(u8::is_ascii_digit))
        .then(|| std::str::from_utf8(raw).ok()?.parse().ok())
        .flatten()
}

fn parse_ascii_u32(raw: &[u8]) -> Option<u32> {
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

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    fn catalog_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../research/SagaECO/Configuration/DB/item.csv")
    }

    fn temporary_catalog(contents: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "eco-starter-catalog-{}-{}.csv",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, contents).expect("write temporary catalog");
        path
    }

    fn mutate_target(mutator: impl FnOnce(&mut Vec<String>)) -> io::Result<()> {
        let original = fs::read_to_string(catalog_path()).expect("read supplied item catalog");
        let mut lines = original.lines().map(str::to_owned).collect::<Vec<_>>();
        mutator(&mut lines);
        let path = temporary_catalog(&lines.join("\n"));
        let result = validate(&path);
        fs::remove_file(path).expect("remove temporary catalog");
        result
    }

    fn target_line<'a>(lines: &'a mut [String], id: &str) -> &'a mut String {
        lines
            .iter_mut()
            .find(|line| line.starts_with(&format!("{id},")))
            .expect("starter row exists")
    }

    fn set_field(lines: &mut [String], id: &str, field: usize, value: &str) {
        let line = target_line(lines, id);
        let mut fields = line.split(',').map(str::to_owned).collect::<Vec<_>>();
        fields[field] = value.to_owned();
        *line = fields.join(",");
    }

    #[test]
    fn supplied_catalog_is_valid() {
        assert!(
            validate(&catalog_path()).is_ok(),
            "{:?}",
            validate(&catalog_path())
        );
    }

    #[test]
    fn rejects_duplicate_missing_and_corrupt_starter_records() {
        assert!(mutate_target(|lines| {
            let duplicate = target_line(lines, "50000000").clone();
            lines.push(duplicate);
        })
        .is_err());
        assert!(mutate_target(|lines| {
            let alternate = target_line(lines, "50000000").replacen("50000000,", "+50000000,", 1);
            lines.push(alternate);
        })
        .is_err());
        assert!(mutate_target(|lines| {
            let alternate = target_line(lines, "50000000").replacen("50000000,", "+050000000,", 1);
            lines.push(alternate);
        })
        .is_err());
        assert!(mutate_target(|lines| {
            set_field(lines, "50000000", 0, " 50000000");
        })
        .is_err());
        assert!(
            mutate_target(|lines| lines.retain(|line| !line.starts_with("50000000,"))).is_err()
        );
        assert!(mutate_target(|lines| {
            set_field(lines, "50000000", 4, "SWORD");
        })
        .is_err());
        assert!(mutate_target(|lines| {
            set_field(lines, "50000000", 16, "1");
        })
        .is_err());
        assert!(mutate_target(|lines| {
            set_field(lines, "50000000", 20, "bad");
        })
        .is_err());
        assert!(mutate_target(|lines| {
            set_field(lines, "50000000", 20, "+5");
        })
        .is_err());
        assert!(mutate_target(|lines| {
            set_field(lines, "50000000", 20, "9");
        })
        .is_err());
    }

    #[test]
    fn validates_every_equipped_status_field_as_zero() {
        let fields = [
            (39, "HP"),
            (40, "MP"),
            (41, "SP"),
            (45, "STR"),
            (46, "MAG"),
            (47, "VIT"),
            (48, "DEX"),
            (49, "AGI"),
            (50, "INT"),
        ];
        for (field, name) in fields {
            for value in ["", " ", "1", "-1", "private-marker"] {
                let result = mutate_target(|lines| set_field(lines, "50000000", field, value));
                let error = result.expect_err("changed status value must be rejected");
                assert_eq!(
                    error.kind(),
                    io::ErrorKind::InvalidData,
                    "{name} field value {value:?}"
                );
                assert!(!error.to_string().contains("private-marker"));
            }
        }
    }

    #[test]
    fn rejects_changed_status_for_every_distinct_wearable() {
        for id in [
            "50000000", "50000055", "50000180", "50001381", "50001454", "50002754", "50010155",
            "50010253", "50010300", "50010456", "50012360", "50060100", "50060150",
        ] {
            let result = mutate_target(|lines| set_field(lines, id, 39, "1"));
            assert_eq!(
                result.expect_err("every wearable must validate HP").kind(),
                io::ErrorKind::InvalidData,
                "wearable {id}"
            );
        }
    }

    #[test]
    fn rejects_truncated_equipped_status_fields() {
        let result = mutate_target(|lines| {
            let line = target_line(lines, "50000000");
            *line = line.split(',').take(50).collect::<Vec<_>>().join(",");
        });
        assert_eq!(
            result
                .expect_err("truncated status fields must be rejected")
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn preserves_body_sword_and_unrelated_wearable_values() {
        assert!(mutate_target(|lines| set_field(lines, "60010082", 39, "7")).is_ok());
        assert!(mutate_target(|lines| set_field(lines, "10020114", 39, "7")).is_ok());
        assert!(mutate_target(|lines| set_field(lines, "50000000", 42, "7")).is_ok());
    }
}
