//! Source-backed description of the one documented start-map training spawn.

use crate::terrain::Terrain;
use roxmltree::Document;
use std::fs;
use std::io;
use std::path::Path;

const MAP_ID: u32 = 20_013_003;
const MOB_ID: u32 = 30_030_018;

#[derive(Debug, PartialEq, Eq)]
struct TrainingActor {
    id: u32,
    map: u32,
    x: u8,
    y: u8,
    amount: u32,
    range: u32,
    delay: u32,
    hp: u32,
    speed: u32,
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn number(value: Option<&str>, message: &'static str) -> io::Result<u32> {
    value
        .filter(|value| !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| invalid(message))
}

fn single_child<'a>(node: roxmltree::Node<'a, 'a>, name: &str) -> io::Result<&'a str> {
    let mut matches = node
        .children()
        .filter(|child| child.is_element() && child.tag_name().name() == name);
    let child = matches
        .next()
        .ok_or_else(|| invalid("training spawn data is incomplete"))?;
    if matches.next().is_some()
        || child.tag_name().namespace().is_some()
        || child.attributes().next().is_some()
        || child.children().count() != 1
        || !child.children().next().is_some_and(|part| part.is_text())
    {
        return Err(invalid("training spawn data is unsupported"));
    }
    let value = child
        .text()
        .filter(|value| value.trim() == *value)
        .ok_or_else(|| invalid("training spawn data is invalid"))?;
    Ok(value)
}

fn read_actor(db: &Path, terrain_path: &Path) -> io::Result<TrainingActor> {
    let xml = fs::read_to_string(db.join("Spawns/20013003.xml"))
        .map_err(|_| invalid("training spawn data is unavailable"))?;
    let doc = Document::parse(&xml).map_err(|_| invalid("training spawn data is invalid"))?;
    let root = doc.root_element();
    if root.tag_name().name() != "Spawns" || root.tag_name().namespace().is_some() {
        return Err(invalid("training spawn data is unsupported"));
    }
    let spawns = root
        .children()
        .filter(|node| node.is_element())
        .collect::<Vec<_>>();
    if spawns.len() != 1
        || spawns[0].tag_name().name() != "spawn"
        || spawns[0].tag_name().namespace().is_some()
    {
        return Err(invalid("training spawn data is unsupported"));
    }
    let spawn = spawns[0];
    if spawn.attributes().next().is_some() {
        return Err(invalid("training spawn data is unsupported"));
    }
    let id = number(
        Some(single_child(spawn, "id")?),
        "training spawn data is invalid",
    )?;
    let map = number(
        Some(single_child(spawn, "map")?),
        "training spawn data is invalid",
    )?;
    let x = number(
        Some(single_child(spawn, "x")?),
        "training spawn data is invalid",
    )?;
    let y = number(
        Some(single_child(spawn, "y")?),
        "training spawn data is invalid",
    )?;
    let amount = number(
        Some(single_child(spawn, "amount")?),
        "training spawn data is invalid",
    )?;
    let range = number(
        Some(single_child(spawn, "range")?),
        "training spawn data is invalid",
    )?;
    let delay = number(
        Some(single_child(spawn, "delay")?),
        "training spawn data is invalid",
    )?;
    if id != MOB_ID
        || map != MAP_ID
        || x != 61
        || y != 65
        || amount != 1
        || range != 0
        || delay != 10
        || spawn.children().filter(|node| node.is_element()).count() != 7
    {
        return Err(invalid("training spawn data is outside supported scope"));
    }

    let file = fs::File::open(db.join("monster.csv"))
        .map_err(|_| invalid("training monster data is unavailable"))?;
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .comment(Some(b'#'))
        .from_reader(file);
    let mut rows = reader.records();
    let mut found = None;
    while let Some(row) = rows.next() {
        let row = row.map_err(|_| invalid("training monster data is invalid"))?;
        if row
            .get(0)
            .and_then(|value| value.trim().parse::<u32>().ok())
            == Some(MOB_ID)
        {
            if found.is_some() {
                return Err(invalid("training monster data is ambiguous"));
            }
            // MobFactory.Init reads HP from column 19 and speed from column 22.
            found = Some((
                number(row.get(19), "training monster data is invalid")?,
                number(row.get(22), "training monster data is invalid")?,
            ));
        }
    }
    let (hp, speed) = found.ok_or_else(|| invalid("training monster data is unavailable"))?;
    if speed > u16::MAX as u32 {
        return Err(invalid("training monster data is outside supported scope"));
    }
    let terrain = Terrain::load(terrain_path)
        .map_err(|_| invalid("training terrain is unavailable or invalid"))?;
    if terrain.map_id() != map || !terrain.passable(x as u8, y as u8) {
        return Err(invalid("training actor tile is outside the walkable map"));
    }
    Ok(TrainingActor {
        id,
        map,
        x: x as u8,
        y: y as u8,
        amount,
        range,
        delay,
        hp,
        speed,
    })
}

pub fn inspect(db: &Path, terrain: &Path) -> io::Result<String> {
    let actor = read_actor(db, terrain)?;
    Ok(format!(
        "Static training actor (Saga10 source data; current-client packet schema unverified)\n\
         Mob template ID: {}\nMap: {}\nTile: ({}, {})\nAmount: {}\nRange: {}\nSpawn delay: {} seconds\nHP: {}\nSpeed: {}",
        actor.id, actor.map, actor.x, actor.y, actor.amount, actor.range, actor.delay, actor.hp, actor.speed
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            loop {
                let path = std::env::temp_dir().join(format!(
                    "eco-world-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                match fs::create_dir(&path) {
                    Ok(()) => {
                        fs::create_dir(path.join("Spawns")).unwrap();
                        return Self(path);
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("could not create isolated test directory: {error}"),
                }
            }
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    use std::path::PathBuf;

    fn terrain_file(dir: &Path, passable: bool) -> PathBuf {
        let path = dir.join("map.bin");
        let mut bytes = vec![0; 49 + 128 * 128 * 17];
        bytes[..4].copy_from_slice(&MAP_ID.to_le_bytes());
        bytes[36..38].copy_from_slice(&128u16.to_le_bytes());
        bytes[38..40].copy_from_slice(&128u16.to_le_bytes());
        if passable {
            bytes[49 + (65 * 128 + 61) * 17 + 13] = 2;
        }
        fs::write(&path, bytes).unwrap();
        path
    }
    fn sources(dir: &Path, spawn: &str, mob_row: &str) {
        fs::write(dir.join("Spawns/20013003.xml"), spawn).unwrap();
        fs::write(dir.join("monster.csv"), format!("{mob_row}\n")).unwrap();
    }
    const SPAWN: &str = "<Spawns><spawn><id>30030018</id><map>20013003</map><x>61</x><y>65</y><amount>1</amount><range>0</range><delay>10</delay></spawn></Spawns>";
    const MOB: &str =
        "30030018,PRIVATE_MOB_NAME_CANARY,pict,ROCK,1,1,1.5,0,0,0,0,0,0,0,0,0,0,0,0,9999,1,1,1";

    #[test]
    fn real_source_spawn_and_monster_record_match_scoped_actor() {
        let root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../research/SagaECO/Configuration/DB");
        let terrain = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/20013003.map");
        let actor = read_actor(&root, &terrain).unwrap();
        assert_eq!(
            (
                actor.id,
                actor.map,
                actor.x,
                actor.y,
                actor.amount,
                actor.range,
                actor.delay,
                actor.hp,
                actor.speed
            ),
            (MOB_ID, MAP_ID, 61, 65, 1, 0, 10, 9999, 1)
        );
        let report = inspect(&root, &terrain).unwrap();
        assert!(report.contains("current-client packet schema unverified"));
        assert!(report.contains("Mob template ID: 30030018"));
        assert!(!report.contains("PRIVATE_MOB_NAME_CANARY"));
        assert!(!report.contains("沙袋"));
    }

    #[test]
    fn rejects_bad_or_out_of_scope_source_and_unwalkable_tile() {
        let temp = Temp::new();
        let terrain = terrain_file(&temp.0, true);
        sources(
            &temp.0,
            "<Spawns><spawn><id>PRIVATE_SOURCE_CANARY</id></spawn></Spawns>",
            MOB,
        );
        let error = read_actor(&temp.0, &terrain).unwrap_err();
        assert!(!error.to_string().contains("PRIVATE_SOURCE_CANARY"));
        assert!(!error.to_string().contains("20013003.xml"));
        sources(&temp.0, "<Spawns><spawn><id>30030018</id><map>20013003</map><x>61<!--split-->9</x><y>65</y><amount>1</amount><range>0</range><delay>10</delay></spawn></Spawns>", MOB);
        assert!(read_actor(&temp.0, &terrain).is_err());
        sources(
            &temp.0,
            "<Spawns><x:spawn xmlns:x='urn:unexpected'/></Spawns>",
            MOB,
        );
        assert!(read_actor(&temp.0, &terrain).is_err());
        sources(&temp.0, "<Spawns><spawn><id>30030018</id><id>30030018</id><map>20013003</map><x>61</x><y>65</y><amount>1</amount><range>0</range><delay>10</delay></spawn></Spawns>", MOB);
        assert!(read_actor(&temp.0, &terrain).is_err());
        sources(&temp.0, "<Spawns><spawn><id>30030018</id><map>20013003</map><x>61</x><y>65</y><amount>2</amount><range>0</range><delay>10</delay></spawn></Spawns>", MOB);
        assert!(read_actor(&temp.0, &terrain).is_err());
        sources(&temp.0, SPAWN, "30030019,name");
        assert!(read_actor(&temp.0, &terrain).is_err());
        let duplicate = MOB.replacen("30030018", "030030018", 1);
        fs::write(temp.0.join("monster.csv"), format!("{MOB}\n{duplicate}\n")).unwrap();
        assert!(read_actor(&temp.0, &terrain)
            .unwrap_err()
            .to_string()
            .contains("ambiguous"));
        for row in [
            MOB.replace("9999", "4294967296"),
            MOB.replace("9999,1,1,1", "9999,1,1,65536"),
        ] {
            sources(&temp.0, SPAWN, &row);
            assert!(read_actor(&temp.0, &terrain).is_err());
        }
        sources(&temp.0, SPAWN, MOB);
        let blocked = terrain_file(&temp.0, false);
        assert!(read_actor(&temp.0, &blocked).is_err());
        let outside = SPAWN.replace("<x>61</x>", "<x>255</x>");
        sources(&temp.0, &outside, MOB);
        assert!(read_actor(&temp.0, &terrain).is_err());
    }
}
