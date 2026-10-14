//! Loader for source-backed SagaECO `.map` walkability grids.
//!
//! The extracted `.map` files remain external data under `offline/data`; this
//! module does not embed or decompress the GPL-licensed MapInfo archive.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

const HEADER_LEN: usize = 49;
const TILE_RECORD_LEN: usize = 17;
const WALKABLE_OFFSET: usize = 13;
const SOURCE_WALKABLE_VALUE: u8 = 2;
// Saved and wire tile coordinates are single bytes.
const MAX_DIMENSION: u16 = 256;
const MAX_FILE_LEN: usize =
    HEADER_LEN + MAX_DIMENSION as usize * MAX_DIMENSION as usize * TILE_RECORD_LEN;

#[derive(Debug, Clone)]
pub struct Terrain {
    map_id: u32,
    width: u16,
    height: u16,
    walkable: Vec<u8>,
}

impl Terrain {
    /// Load an extracted `.map` file without modifying it.
    pub fn load(path: &Path) -> io::Result<Self> {
        let mut bytes = Vec::new();
        File::open(path)?
            .take(MAX_FILE_LEN as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_FILE_LEN {
            return Err(invalid("map file exceeds the largest supported map"));
        }
        Self::parse(&bytes)
    }

    /// Whether a destination tile has the source-defined walkability value 2.
    /// Coordinates outside the map are impassable.
    pub fn passable(&self, x: u8, y: u8) -> bool {
        if u16::from(x) >= self.width || u16::from(y) >= self.height {
            return false;
        }
        let offset = usize::from(y) * usize::from(self.width) + usize::from(x);
        self.walkable[offset] == SOURCE_WALKABLE_VALUE
    }

    pub fn map_id(&self) -> u32 {
        self.map_id
    }

    pub fn dimensions(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    fn parse(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() < HEADER_LEN {
            return Err(invalid("map file is shorter than its header"));
        }
        let map_id = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        let width = u16::from_le_bytes(bytes[36..38].try_into().unwrap());
        let height = u16::from_le_bytes(bytes[38..40].try_into().unwrap());
        if map_id == 0 {
            return Err(invalid("map file has no map ID"));
        }
        if !(1..=MAX_DIMENSION).contains(&width) || !(1..=MAX_DIMENSION).contains(&height) {
            return Err(invalid("map dimensions exceed byte tile coordinates"));
        }
        let tile_count = usize::from(width) * usize::from(height);
        let expected_len = HEADER_LEN + tile_count * TILE_RECORD_LEN;
        if bytes.len() != expected_len {
            return Err(invalid(
                "map tile data length does not match its dimensions",
            ));
        }

        // MapInfoFactory iterates rows (y) then columns (x), reads each 17-byte
        // record, and places walkable at record offset 13. Saga map/path code
        // considers a tile passable only when that byte equals 2.
        let mut walkable = Vec::with_capacity(tile_count);
        for record in bytes[HEADER_LEN..].chunks_exact(TILE_RECORD_LEN) {
            walkable.push(record[WALKABLE_OFFSET]);
        }
        Ok(Self {
            map_id,
            width,
            height,
            walkable,
        })
    }
}

/// Walkability grids for every map the server can place a character on.
#[derive(Debug, Clone, Default)]
pub struct Maps {
    by_id: HashMap<u32, Terrain>,
}

impl Maps {
    pub fn load(paths: &[&Path]) -> io::Result<Self> {
        let mut maps = Self::default();
        for path in paths {
            maps.insert(Terrain::load(path)?)?;
        }
        Ok(maps)
    }

    pub fn insert(&mut self, terrain: Terrain) -> io::Result<()> {
        if self.by_id.insert(terrain.map_id, terrain).is_some() {
            return Err(invalid("map file ID is loaded twice"));
        }
        Ok(())
    }

    pub fn get(&self, map_id: u32) -> Option<&Terrain> {
        self.by_id.get(&map_id)
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_MAP_ID: u32 = 20_013_003;

    fn test_map(map_id: u32, width: u16, height: u16) -> Vec<u8> {
        let mut bytes =
            vec![0; HEADER_LEN + usize::from(width) * usize::from(height) * TILE_RECORD_LEN];
        bytes[..4].copy_from_slice(&map_id.to_le_bytes());
        bytes[36..38].copy_from_slice(&width.to_le_bytes());
        bytes[38..40].copy_from_slice(&height.to_le_bytes());
        bytes
    }

    fn fixture(name: &str) -> Terrain {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("data")
            .join(name);
        Terrain::load(&path).expect("read the extracted source map fixture")
    }

    fn passable_count(terrain: &Terrain) -> usize {
        let (width, height) = terrain.dimensions();
        (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .filter(|&(x, y)| terrain.passable(x as u8, y as u8))
            .count()
    }

    #[test]
    fn only_source_value_two_is_passable_and_bounds_are_checked() {
        let mut bytes = test_map(TEST_MAP_ID, 128, 128);
        bytes[HEADER_LEN + WALKABLE_OFFSET] = 2;
        bytes[HEADER_LEN + TILE_RECORD_LEN + WALKABLE_OFFSET] = 0;
        bytes[HEADER_LEN + 2 * TILE_RECORD_LEN + WALKABLE_OFFSET] = 1;
        let terrain = Terrain::parse(&bytes).unwrap();
        assert!(terrain.passable(0, 0));
        assert!(!terrain.passable(1, 0));
        assert!(!terrain.passable(2, 0));
        assert!(!terrain.passable(128, 0));
        assert!(!terrain.passable(0, 128));
    }

    #[test]
    fn rejects_missing_id_bad_dimensions_and_nonexact_tile_length() {
        for bytes in [
            test_map(0, 128, 128),
            test_map(TEST_MAP_ID, 0, 128),
            test_map(TEST_MAP_ID, 128, 0),
            test_map(TEST_MAP_ID, 257, 1),
        ] {
            assert_eq!(
                Terrain::parse(&bytes).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }

        let mut short = test_map(TEST_MAP_ID, 128, 128);
        short.pop();
        let mut long = test_map(TEST_MAP_ID, 128, 128);
        long.push(0);
        for bytes in [short, long, vec![0; 48]] {
            assert_eq!(
                Terrain::parse(&bytes).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
    }

    #[test]
    fn maps_are_keyed_by_file_id_and_reject_duplicates() {
        let mut maps = Maps::default();
        maps.insert(Terrain::parse(&test_map(7, 2, 2)).unwrap())
            .unwrap();
        maps.insert(Terrain::parse(&test_map(8, 3, 3)).unwrap())
            .unwrap();
        assert_eq!(maps.get(8).unwrap().dimensions(), (3, 3));
        assert!(maps.get(9).is_none());
        assert_eq!(
            maps.insert(Terrain::parse(&test_map(7, 4, 4)).unwrap())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn extracted_real_map_fixtures_have_verified_shape_and_walkability_counts() {
        let room = fixture("20013003.map");
        assert_eq!(room.map_id(), TEST_MAP_ID);
        assert_eq!(room.dimensions(), (128, 128));
        assert!(room.passable(63, 62));
        assert_eq!(passable_count(&room), 96);

        let up_town = fixture("10023000.map");
        assert_eq!(up_town.map_id(), 10_023_000);
        assert_eq!(up_town.dimensions(), (256, 256));
        // Arrival tile used by SagaECO's return warps into Up Town.
        assert!(up_town.passable(130, 139));
        assert!(!up_town.passable(126, 126));
        assert_eq!(passable_count(&up_town), 21_216);
    }
}
