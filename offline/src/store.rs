use crate::quest::{self, LifecycleState, QuestStatus};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior};
use std::fs::{self, OpenOptions};
use std::io;
use std::path::Path;

const SCHEMA_VERSION: i64 = 10;
const NOVICE_MAX_EP: u32 = 30;
const STARTER_ITEMS: [[[u32; 5]; 2]; 3] = [
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
const STARTER_DURABILITY: [u32; 5] = [5, 5, 10, 0, 10];
const NOVICE_BASE_STATS: [[u32; 6]; 3] = [
    [8, 3, 3, 10, 4, 3],
    [6, 3, 6, 4, 2, 10],
    [10, 5, 1, 1, 5, 8],
];
const START_MAP_ID: u32 = 20_013_003;
const DEMO_ACCOUNT_ID: u32 = 1;
const DEMO_USERNAME: &str = "test";
const DEMO_PASSWORD_TOKEN: &str = "96e79218965eb72c92a549dd5a330112";

pub struct Store {
    connection: Connection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub id: u32,
    pub username: String,
    pub password_token: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewCharacter {
    pub slot: u8,
    pub name: String,
    pub race: u8,
    pub sex: u8,
    pub hair: u16,
    pub hair_color: u8,
    pub face: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Character {
    pub id: u32,
    pub account_id: u32,
    pub slot: u8,
    pub name: String,
    pub race: u8,
    pub sex: u8,
    pub hair: u16,
    pub hair_color: u8,
    pub face: u16,
    pub map_id: u32,
    /// Map tile coordinate, stored as the client-facing byte value.
    pub x: u8,
    /// Map tile coordinate, stored as the client-facing byte value.
    pub y: u8,
    /// Direction in 45-degree octants, as stored by SagaECO.
    pub direction: u8,
}

#[derive(Debug)]
pub enum CreateError {
    InvalidSlot,
    NameTooShort,
    NameTooLong,
    BadName,
    SlotOccupied,
    NameConflict,
    InvalidAppearance,
    AccountNotFound,
    Storage(io::Error),
}

impl std::fmt::Display for CreateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSlot => f.write_str("character slot must be 0..3"),
            Self::NameTooShort => {
                f.write_str("character name must be at least 3 UTF-16 code units")
            }
            Self::NameTooLong => f.write_str("character name must be at most 8 UTF-16 code units"),
            Self::BadName => f.write_str("character name contains a forbidden character"),
            Self::SlotOccupied => f.write_str("character slot is already occupied"),
            Self::NameConflict => f.write_str("character name is already in use"),
            Self::InvalidAppearance => f.write_str("race or sex value is invalid"),
            Self::AccountNotFound => f.write_str("account does not exist"),
            Self::Storage(error) => write!(f, "storage error: {error}"),
        }
    }
}

impl std::error::Error for CreateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Storage(error) => Some(error),
            _ => None,
        }
    }
}

impl Store {
    /// Opens a local save. Only a newly created file gets the demo account and schema.
    /// Existing files with an unknown schema or failed integrity checks are left untouched.
    pub fn open(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        let is_new = match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(file) => {
                drop(file);
                true
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => false,
            Err(error) => return Err(error),
        };
        let connection = Connection::open(path).map_err(sql_error)?;
        Self::initialize_or_validate(connection, is_new)
    }

    /// Opens an existing save without creating the file or its parent directory.
    pub fn open_existing(path: &Path) -> io::Result<Self> {
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(sql_error)?;
        Self::initialize_or_validate(connection, false)
    }

    /// Creates an isolated in-memory store using the same schema and demo account.
    #[cfg(test)]
    pub fn in_memory() -> io::Result<Self> {
        let connection = Connection::open_in_memory().map_err(sql_error)?;
        Self::initialize_or_validate(connection, true)
    }

    fn initialize_or_validate(mut connection: Connection, is_new: bool) -> io::Result<Self> {
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(sql_error)?;
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .map_err(sql_error)?;

        // Serialize schema checks and creation against any other opener.
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Exclusive)
            .map_err(sql_error)?;
        let version: i64 = transaction
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(sql_error)?;
        if is_new {
            if version != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "new save unexpectedly has a schema version",
                ));
            }
            create_schema(&transaction)?;
        } else {
            match version {
                1 => {
                    migrate_v1_to_v2(&transaction)?;
                    migrate_v2_to_v3(&transaction)?;
                    validate_schema_v3(&transaction)?;
                    migrate_v3_to_v4(&transaction)?;
                    migrate_v4_to_v5(&transaction)?;
                    migrate_v5_to_v6(&transaction)?;
                    validate_schema_v6(&transaction)?;
                    migrate_v6_to_v7(&transaction)?;
                }
                2 => {
                    validate_schema_v2(&transaction)?;
                    migrate_v2_to_v3(&transaction)?;
                    validate_schema_v3(&transaction)?;
                    migrate_v3_to_v4(&transaction)?;
                    migrate_v4_to_v5(&transaction)?;
                    migrate_v5_to_v6(&transaction)?;
                    validate_schema_v6(&transaction)?;
                    migrate_v6_to_v7(&transaction)?;
                }
                3 => {
                    validate_schema_v3(&transaction)?;
                    migrate_v3_to_v4(&transaction)?;
                    migrate_v4_to_v5(&transaction)?;
                    migrate_v5_to_v6(&transaction)?;
                    validate_schema_v6(&transaction)?;
                    migrate_v6_to_v7(&transaction)?;
                }
                4 => {
                    validate_schema_v4(&transaction)?;
                    migrate_v4_to_v5(&transaction)?;
                    migrate_v5_to_v6(&transaction)?;
                    validate_schema_v6(&transaction)?;
                    migrate_v6_to_v7(&transaction)?;
                }
                5 => {
                    validate_schema_v5(&transaction)?;
                    migrate_v5_to_v6(&transaction)?;
                    validate_schema_v6(&transaction)?;
                    migrate_v6_to_v7(&transaction)?;
                }
                6 => {
                    validate_schema_v6(&transaction)?;
                    migrate_v6_to_v7(&transaction)?;
                }
                7 => {}
                8 => {
                    // Validated before the additive v9 migration below.
                }
                9 => {}
                SCHEMA_VERSION => {}
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("unsupported save schema version {version}"),
                    ));
                }
            }
            if version < 8 {
                validate_schema(&transaction)?;
                migrate_v7_to_v8(&transaction)?;
            }
            if version < 9 {
                validate_schema(&transaction)?;
                validate_gold_rows(&transaction)?;
                migrate_v8_to_v9(&transaction)?;
            }
            if version < 10 {
                validate_schema(&transaction)?;
                validate_reward_context_rows(&transaction)?;
                migrate_v9_to_v10(&transaction)?;
            }
            validate_schema(&transaction)?;
            validate_gold_rows(&transaction)?;
            validate_reward_context_rows(&transaction)?;
            validate_reward_inventory_rows(&transaction)?;
        }
        transaction.commit().map_err(sql_error)?;
        Ok(Self { connection })
    }

    pub fn account(&self, username: &str) -> io::Result<Option<Account>> {
        self.connection.query_row(
            "SELECT id, username, password_token FROM account WHERE username = ?1 COLLATE NOCASE",
            [username],
            |row| Ok(Account { id: row.get(0)?, username: row.get(1)?, password_token: row.get(2)? }),
        ).optional().map_err(sql_error)
    }

    pub fn characters(&self, account_id: u32) -> io::Result<Vec<Character>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, account_id, slot, name, race, sex, hair, hair_color, face, map_id, x, y, direction \
             FROM character WHERE account_id = ?1 ORDER BY slot, id",
            )
            .map_err(sql_error)?;
        let characters = statement
            .query_map([account_id], character_from_row)
            .map_err(sql_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql_error)?;
        Ok(characters)
    }

    /// Persists an explicitly known carried-gold value for an owned character.
    pub fn initialize_gold(
        &mut self,
        account_id: u32,
        character_id: u32,
        gold: u32,
    ) -> io::Result<()> {
        if account_id == 0 || character_id == 0 || gold > 99_999_999 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "character gold input is invalid",
            ));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql_error)?;
        validate_gold_rows(&transaction)?;
        let owned = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM character WHERE id=?1 AND account_id=?2)",
                (character_id, account_id),
                |r| r.get::<_, bool>(0),
            )
            .map_err(sql_error)?;
        if !owned {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "character does not exist for account",
            ));
        }
        let existing = transaction
            .query_row(
                "SELECT gold,typeof(gold) FROM character_gold WHERE character_id=?1",
                [character_id],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|_| gold_error())?;
        match existing {
            Some((saved, kind)) if kind == "integer" && saved == gold as i64 => {
                transaction.commit().map_err(sql_error)
            }
            Some(_) => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "character gold is already initialized",
            )),
            None => {
                transaction
                    .execute(
                        "INSERT INTO character_gold(character_id,gold) VALUES(?1,?2)",
                        (character_id, gold),
                    )
                    .map_err(|_| {
                        io::Error::new(io::ErrorKind::Other, "character gold could not be saved")
                    })?;
                if transaction.changes() != 1 {
                    return Err(io::Error::new(
                        io::ErrorKind::Other,
                        "character gold could not be saved",
                    ));
                }
                validate_gold_rows(&transaction)?;
                transaction.commit().map_err(sql_error)
            }
        }
    }

    /// Persists explicitly known EP, CP, and a caller declaration that this novice has no ring.
    pub fn initialize_reward_context(
        &mut self,
        account_id: u32,
        character_id: u32,
        ep: u32,
        cp: u32,
        declare_no_ring: bool,
    ) -> io::Result<()> {
        if account_id == 0
            || character_id == 0
            || ep > NOVICE_MAX_EP
            || cp > 99_999_999
            || !declare_no_ring
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "character reward context input is invalid",
            ));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql_error)?;
        validate_reward_context_rows(&transaction)?;
        let owned: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM character WHERE id=?1 AND account_id=?2)",
                (character_id, account_id),
                |r| r.get(0),
            )
            .map_err(sql_error)?;
        if !owned {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "character does not exist for account",
            ));
        }
        let progress = transaction.query_row(
            "SELECT base_level,job_level,base_exp,job_exp,stats_points,skill_points,typeof(base_level),typeof(job_level),typeof(base_exp),typeof(job_exp),typeof(stats_points),typeof(skill_points) FROM character_novice_progress WHERE character_id=?1",
            [character_id], novice_progress_from_row,
        ).optional().map_err(|_| novice_progress_error())?.ok_or_else(novice_progress_error)?;
        validate_novice_progress(progress).map_err(|_| novice_progress_error())?;
        validate_novice_status_rows(&transaction)?;
        if !novice_status_for_character(&transaction, character_id)? {
            return Err(novice_status_error());
        }
        let existing = transaction.query_row("SELECT ep,cp,ringless,typeof(ep),typeof(cp),typeof(ringless) FROM character_reward_context WHERE character_id=?1", [character_id], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?))).optional().map_err(|_| reward_context_error())?;
        match existing {
            Some((saved_ep, saved_cp, ringless, ep_type, cp_type, ring_type))
                if ep_type == "integer"
                    && cp_type == "integer"
                    && ring_type == "integer"
                    && saved_ep == i64::from(ep)
                    && saved_cp == i64::from(cp)
                    && ringless == 1 =>
            {
                transaction.commit().map_err(sql_error)
            }
            Some(_) => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "character reward context is already initialized",
            )),
            None => {
                transaction.execute("INSERT INTO character_reward_context(character_id,ep,cp,ringless) VALUES(?1,?2,?3,1)", (character_id,ep,cp)).map_err(|_| io::Error::new(io::ErrorKind::Other,"character reward context could not be saved"))?;
                if transaction.changes() != 1 {
                    return Err(io::Error::new(
                        io::ErrorKind::Other,
                        "character reward context could not be saved",
                    ));
                }
                validate_reward_context_rows(&transaction)?;
                transaction.commit().map_err(sql_error)
            }
        }
    }

    /// Declares that carried inventory was explicitly observed as the five starter rows.
    /// This is an initialization-time fact; repeats preserve any subsequently imported rewards.
    pub fn initialize_reward_inventory(
        &mut self,
        account_id: u32,
        character_id: u32,
        declare_starter_only: bool,
    ) -> io::Result<()> {
        if account_id == 0 || character_id == 0 || !declare_starter_only {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "character reward inventory input is invalid",
            ));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql_error)?;
        validate_reward_inventory_rows(&transaction)?;
        validate_gold_rows(&transaction)?;
        validate_reward_context_rows(&transaction)?;
        validate_novice_status_rows(&transaction)?;
        let owned: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM character WHERE id=?1 AND account_id=?2)",
                (character_id, account_id),
                |r| r.get(0),
            )
            .map_err(sql_error)?;
        if !owned {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "character does not exist for account",
            ));
        }
        if !transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM character_novice_progress WHERE character_id=?1)",
                [character_id],
                |r| r.get::<_, bool>(0),
            )
            .map_err(sql_error)?
            || !transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM character_gold WHERE character_id=?1)",
                    [character_id],
                    |r| r.get::<_, bool>(0),
                )
                .map_err(sql_error)?
            || !transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM character_reward_context WHERE character_id=?1)",
                    [character_id],
                    |r| r.get::<_, bool>(0),
                )
                .map_err(sql_error)?
            || !novice_status_for_character(&transaction, character_id)?
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "character reward prerequisites are incomplete",
            ));
        }
        let existing: Option<(i64, i64, String, String)> = transaction.query_row("SELECT next_slot,starter_only,typeof(next_slot),typeof(starter_only) FROM character_reward_inventory WHERE character_id=?1", [character_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(|_| reward_inventory_error())?;
        match existing {
            Some((_, 1, a, b)) if a == "integer" && b == "integer" => {
                transaction.commit().map_err(sql_error)
            }
            Some(_) => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "character reward inventory is already initialized",
            )),
            None => {
                transaction.execute("INSERT INTO character_reward_inventory(character_id,next_slot,starter_only) VALUES(?1,6,1)", [character_id]).map_err(|_| io::Error::new(io::ErrorKind::Other, "character reward inventory could not be saved"))?;
                if transaction.changes() != 1 {
                    return Err(io::Error::new(
                        io::ErrorKind::Other,
                        "character reward inventory could not be saved",
                    ));
                }
                validate_reward_inventory_rows(&transaction)?;
                transaction.commit().map_err(sql_error)
            }
        }
    }

    /// Persists map tile coordinates and octant direction for an owned character.
    pub fn update_position(
        &mut self,
        account_id: u32,
        character_id: u32,
        x: u8,
        y: u8,
        direction: u8,
    ) -> io::Result<()> {
        let changed = self
            .connection
            .execute(
                "UPDATE character SET x = ?1, y = ?2, direction = ?3 WHERE account_id = ?4 AND id = ?5",
                (x, y, direction, account_id, character_id),
            )
            .map_err(sql_error)?;
        if changed != 1 {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "character does not exist for account",
            ));
        }
        Ok(())
    }

    /// Moves an owned character to a tile on another map.
    ///
    /// Novice-status and reward-context rows are validated against the start map,
    /// so characters carrying them stay where they are.
    pub fn place_character(
        &mut self,
        account_id: u32,
        character_id: u32,
        map_id: u32,
        x: u8,
        y: u8,
        direction: u8,
    ) -> io::Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql_error)?;
        let pinned: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM character_novice_status WHERE character_id = ?1)
                     OR EXISTS(SELECT 1 FROM character_reward_context WHERE character_id = ?1)",
                [character_id],
                |row| row.get(0),
            )
            .map_err(sql_error)?;
        if pinned {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "character has start-map novice records",
            ));
        }
        let changed = transaction
            .execute(
                "UPDATE character SET map_id = ?1, x = ?2, y = ?3, direction = ?4 WHERE account_id = ?5 AND id = ?6",
                (map_id, x, y, direction, account_id, character_id),
            )
            .map_err(sql_error)?;
        if changed != 1 {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "character does not exist for account",
            ));
        }
        transaction.commit().map_err(sql_error)
    }

    /// Initializes the scoped ordinary novice progression row for an owned, non-DEM character.
    pub fn initialize_novice_progress(
        &mut self,
        account_id: u32,
        character_id: u32,
    ) -> io::Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql_error)?;
        let race = transaction
            .query_row(
                "SELECT race FROM character WHERE account_id = ?1 AND id = ?2",
                (account_id, character_id),
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(sql_error)?
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "character does not exist for account",
                )
            })?;
        if !(0..=2).contains(&race) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "character race is not supported for novice progression",
            ));
        }
        validate_novice_status_rows(&transaction)?;
        let current = transaction
            .query_row(
                "SELECT base_level, job_level, base_exp, job_exp, stats_points, skill_points,
                    typeof(base_level), typeof(job_level), typeof(base_exp), typeof(job_exp),
                    typeof(stats_points), typeof(skill_points)
             FROM character_novice_progress WHERE character_id = ?1",
                [character_id],
                novice_progress_from_row,
            )
            .optional()
            .map_err(|_| novice_progress_error())?;
        if let Some(progress) = current {
            validate_novice_progress(progress).map_err(|_| novice_progress_error())?;
        } else {
            transaction.execute(
                "INSERT INTO character_novice_progress
                    (character_id, base_level, job_level, base_exp, job_exp, stats_points, skill_points)
                 VALUES (?1, 1, 1, 0, 0, 2, 3)",
                [character_id],
            ).map_err(|_| io::Error::new(io::ErrorKind::Other, "novice progression could not be saved"))?;
        }
        transaction.commit().map_err(sql_error)
    }

    /// Saves the source-bounded initial five-item inventory for an owned novice character.
    pub fn initialize_starter_inventory(
        &mut self,
        account_id: u32,
        character_id: u32,
        items: [[[u32; 5]; 2]; 3],
    ) -> io::Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql_error)?;
        let (race, sex): (i64, i64) = transaction
            .query_row(
                "SELECT race,sex FROM character WHERE account_id=?1 AND id=?2",
                (account_id, character_id),
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(sql_error)?
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "character does not exist for account",
                )
            })?;
        if !(0..=2).contains(&race) || !(0..=1).contains(&sex) || items != STARTER_ITEMS {
            return Err(starter_inventory_error());
        }
        let progress = transaction.query_row(
            "SELECT base_level,job_level,base_exp,job_exp,stats_points,skill_points,typeof(base_level),typeof(job_level),typeof(base_exp),typeof(job_exp),typeof(stats_points),typeof(skill_points) FROM character_novice_progress WHERE character_id=?1", [character_id], novice_progress_from_row
        ).optional().map_err(|_| novice_progress_error())?.ok_or_else(novice_progress_error)?;
        validate_novice_progress(progress).map_err(|_| novice_progress_error())?;
        let existing = starter_inventory_for_character(&transaction, character_id)?;
        if !existing.is_empty() {
            if existing.len() == 5 && existing == expected_starter_rows(race as usize, sex as usize)
            {
                transaction.commit().map_err(sql_error)?;
                return Ok(());
            }
            return Err(starter_inventory_error());
        }
        for (slot, (item_id, durability)) in expected_starter_rows(race as usize, sex as usize)
            .into_iter()
            .enumerate()
        {
            transaction.execute(
                "INSERT INTO character_starter_item(character_id,instance_slot,container,item_id,count,durability,identified) VALUES (?1,?2,?3,?4,1,?5,1)",
                (character_id, slot as i64 + 1, [11,12,16,2,2][slot], item_id, durability),
            ).map_err(|_| io::Error::new(io::ErrorKind::Other, "starter inventory could not be saved"))?;
        }
        transaction.commit().map_err(sql_error)
    }

    /// Saves configured base attributes only for an existing initialized novice row.
    pub fn initialize_novice_base_stats(
        &mut self,
        account_id: u32,
        character_id: u32,
        source_stats: [[u32; 6]; 3],
    ) -> io::Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql_error)?;
        let race = transaction
            .query_row(
                "SELECT race FROM character WHERE account_id=?1 AND id=?2",
                (account_id, character_id),
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(sql_error)?
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "character does not exist for account",
                )
            })?;
        if !(0..=2).contains(&race) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "character race is not supported for novice progression",
            ));
        }
        let stats = source_stats[race as usize];
        if source_stats != NOVICE_BASE_STATS {
            return Err(novice_progress_error());
        }
        let progress = transaction.query_row("SELECT base_level,job_level,base_exp,job_exp,stats_points,skill_points,typeof(base_level),typeof(job_level),typeof(base_exp),typeof(job_exp),typeof(stats_points),typeof(skill_points) FROM character_novice_progress WHERE character_id=?1", [character_id], novice_progress_from_row).optional().map_err(|_| novice_progress_error())?.ok_or_else(novice_progress_error)?;
        validate_novice_progress(progress).map_err(|_| novice_progress_error())?;
        let mut current = [None; 6];
        transaction.query_row("SELECT base_str,base_dex,base_int,base_vit,base_agi,base_mag FROM character_novice_progress WHERE character_id=?1", [character_id], |row| {
            for (i, value) in current.iter_mut().enumerate() { *value = row.get::<_, Option<i64>>(i)?; }
            Ok(())
        }).map_err(|_| novice_progress_error())?;
        if current.iter().any(Option::is_some) {
            let saved = current.map(|value| value.and_then(|v| u32::try_from(v).ok()));
            if saved != stats.map(Some) {
                return Err(novice_progress_error());
            }
        } else {
            transaction.execute("UPDATE character_novice_progress SET base_str=?1,base_dex=?2,base_int=?3,base_vit=?4,base_agi=?5,base_mag=?6 WHERE character_id=?7", (stats[0],stats[1],stats[2],stats[3],stats[4],stats[5],character_id)).map_err(sql_error)?;
        }
        transaction.commit().map_err(sql_error)
    }

    /// Adds source-bounded EXP to an already initialized ordinary novice character.
    pub fn apply_novice_exp(
        &mut self,
        account_id: u32,
        character_id: u32,
        base_award: u32,
        job_award: u32,
        declare_unmodified_status: bool,
    ) -> io::Result<[u32; 2]> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql_error)?;
        let result = apply_novice_exp_in_transaction(
            &transaction,
            account_id,
            character_id,
            base_award,
            job_award,
            declare_unmodified_status,
        )?;
        transaction.commit().map_err(sql_error)?;
        Ok(result)
    }

    /// Applies the bounded reward batch for the explicitly sourced turn-in NPC.
    pub(crate) fn turn_in_completed_quest(
        &mut self,
        account_id: u32,
        character_id: u32,
        npc_event_id: u32,
    ) -> io::Result<()> {
        if npc_event_id != 11_000_435 || account_id == 0 || character_id == 0 {
            return Err(quest_state_error());
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| quest_storage_error())?;
        validate_quest_rows(&transaction)?;
        validate_novice_progress_rows(&transaction)?;
        validate_starter_inventory_rows(&transaction)?;
        validate_novice_status_rows(&transaction)?;
        validate_gold_rows(&transaction)?;
        validate_reward_context_rows(&transaction)?;
        validate_reward_inventory_rows(&transaction)?;
        let owned: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM character WHERE account_id=?1 AND id=?2)",
                (account_id, character_id),
                |r| r.get(0),
            )
            .map_err(|_| quest_storage_error())?;
        let quest_rows: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM quest_lifecycle WHERE character_id=?1",
                [character_id],
                |r| r.get(0),
            )
            .map_err(|_| quest_state_error())?;
        if quest_rows != 1 {
            return Err(quest_state_error());
        }
        let saved = transaction.query_row("SELECT status,fame,typeof(status),typeof(fame) FROM quest_lifecycle WHERE character_id=?1",[character_id],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?))).optional().map_err(|_|quest_state_error())?;
        if !owned {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "character does not exist for account",
            ));
        }
        let (status, fame, status_type, fame_type) = saved.ok_or_else(quest_state_error)?;
        if status != 2
            || status_type != "integer"
            || fame_type != "integer"
            || !(0..=i64::from(u32::MAX)).contains(&fame)
        {
            return Err(quest_state_error());
        }
        if !novice_status_for_character(&transaction, character_id)? {
            return Err(novice_status_error());
        }
        let gold: i64 = transaction
            .query_row(
                "SELECT gold FROM character_gold WHERE character_id=?1",
                [character_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|_| gold_error())?
            .ok_or_else(gold_error)?;
        let (ep, cp): (i64, i64) = transaction
            .query_row(
                "SELECT ep,cp FROM character_reward_context WHERE character_id=?1",
                [character_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|_| reward_context_error())?
            .ok_or_else(reward_context_error)?;
        let next: i64 = transaction
            .query_row(
                "SELECT next_slot FROM character_reward_inventory WHERE character_id=?1",
                [character_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|_| reward_inventory_error())?
            .ok_or_else(reward_inventory_error)?;
        if next >= i64::from(u32::MAX) || gold > 99_999_519 || fame >= i64::from(i32::MAX) {
            return Err(quest_state_error());
        }
        apply_novice_exp_in_transaction(&transaction, account_id, character_id, 55, 55, false)?;
        transaction
            .execute(
                "UPDATE character_gold SET gold=gold+480 WHERE character_id=?1 AND gold=?2",
                (character_id, gold),
            )
            .map_err(|_| quest_storage_error())?;
        if transaction.changes() != 1 {
            return Err(quest_storage_error());
        }
        transaction.execute("UPDATE character_reward_context SET ep=?1,cp=?2 WHERE character_id=?3 AND ep=?4 AND cp=?2",((ep + 1).min(i64::from(NOVICE_MAX_EP)),cp,character_id,ep)).map_err(|_|quest_storage_error())?;
        if transaction.changes() != 1 {
            return Err(quest_storage_error());
        }
        // MinLV 255 is literal for difficulty; resulting base levels 1/2 are never TOO_EASY.
        transaction.execute("UPDATE quest_lifecycle SET fame=fame+1 WHERE character_id=?1 AND status=2 AND fame=?2",(character_id,fame)).map_err(|_|quest_storage_error())?;
        if transaction.changes() != 1 {
            return Err(quest_storage_error());
        }
        transaction.execute("INSERT INTO character_reward_item(character_id,instance_slot,container,item_id,count,durability,identified,pict_id,default_profile) VALUES(?1,?2,2,60060000,1,0,1,0,1)",(character_id,next)).map_err(|_|quest_storage_error())?;
        if transaction.changes() != 1 {
            return Err(quest_storage_error());
        }
        transaction.execute("UPDATE character_reward_inventory SET next_slot=next_slot+1 WHERE character_id=?1 AND next_slot=?2",(character_id,next)).map_err(|_|quest_storage_error())?;
        if transaction.changes() != 1 {
            return Err(quest_storage_error());
        }
        // NPC 11000435 inherits Event.OnQuestUpdate's empty hook; the caller supplies that handling-event context.
        transaction.execute("UPDATE quest_lifecycle SET status=0,credited=0,deadline=NULL WHERE character_id=?1 AND status=2",[character_id]).map_err(|_|quest_storage_error())?;
        if transaction.changes() != 1 {
            return Err(quest_storage_error());
        }
        validate_quest_rows(&transaction)?;
        validate_novice_progress_rows(&transaction)?;
        validate_novice_status_rows(&transaction)?;
        validate_gold_rows(&transaction)?;
        validate_reward_context_rows(&transaction)?;
        validate_reward_inventory_rows(&transaction)?;
        transaction.commit().map_err(|_| quest_storage_error())
    }

    /// Replays one validated batch against an owned character and commits its full state atomically.
    /// Initial values are used only when this character has no saved quest row.
    pub(crate) fn replay_saved_quest(
        &mut self,
        account_id: u32,
        character_id: u32,
        db: &Path,
        events: &Path,
        initial_points: u16,
        initial_fame: u32,
    ) -> io::Result<String> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| quest_storage_error())?;
        let owned: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM character WHERE account_id = ?1 AND id = ?2)",
                (account_id, character_id),
                |row| row.get(0),
            )
            .map_err(|_| quest_storage_error())?;
        if !owned {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "character does not exist for account",
            ));
        }
        let saved = transaction
            .query_row(
                "SELECT character_id, points, fame, status, credited, deadline, last_elapsed,
                        typeof(points), typeof(fame), typeof(status), typeof(credited),
                        typeof(deadline), typeof(last_elapsed)
                 FROM quest_lifecycle WHERE character_id = ?1",
                [character_id],
                lifecycle_state_from_row,
            )
            .optional()
            .map_err(|_| quest_state_error())?;
        let mut state = saved.unwrap_or_else(|| LifecycleState::new(initial_points, initial_fame));
        state.validate().map_err(|_| quest_state_error())?;
        let report = quest::apply_lifecycle(db, events, &mut state).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "quest event batch is invalid")
        })?;
        state.validate().map_err(|_| quest_state_error())?;
        let status = match &state.status {
            QuestStatus::None => 0,
            QuestStatus::Open => 1,
            QuestStatus::Completed => 2,
            QuestStatus::Failed => 3,
        };
        let deadline = state
            .deadline
            .map(i64::try_from)
            .transpose()
            .map_err(|_| quest_state_error())?;
        transaction
            .execute(
                "INSERT INTO quest_lifecycle
                    (character_id, points, fame, status, credited, deadline, last_elapsed)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(character_id) DO UPDATE SET
                    points=excluded.points, fame=excluded.fame, status=excluded.status,
                    credited=excluded.credited, deadline=excluded.deadline,
                    last_elapsed=excluded.last_elapsed",
                (
                    character_id,
                    state.points,
                    state.fame,
                    status,
                    state.credited,
                    deadline,
                    state.last_elapsed,
                ),
            )
            .map_err(|_| quest_storage_error())?;
        transaction.commit().map_err(|_| quest_storage_error())?;
        let report = report.replacen(
            "Headless lifecycle replay; no live gameplay, dialogue, rewards or persistence.",
            "Headless saved lifecycle replay; no live gameplay, dialogue or rewards.",
            1,
        );
        Ok(report)
    }

    pub fn create_character(
        &mut self,
        account_id: u32,
        input: NewCharacter,
    ) -> Result<Character, CreateError> {
        validate_character(&input)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(create_storage)?;
        let exists: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM account WHERE id = ?1)",
                [account_id],
                |row| row.get(0),
            )
            .map_err(create_storage)?;
        if !exists {
            return Err(CreateError::AccountNotFound);
        }
        let slot_taken: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM character WHERE account_id = ?1 AND slot = ?2)",
                (account_id, input.slot),
                |row| row.get(0),
            )
            .map_err(create_storage)?;
        if slot_taken {
            return Err(CreateError::SlotOccupied);
        }
        let name_taken: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM character WHERE name = ?1 COLLATE NOCASE)",
                [&input.name],
                |row| row.get(0),
            )
            .map_err(create_storage)?;
        if name_taken {
            return Err(CreateError::NameConflict);
        }

        transaction.execute(
            "INSERT INTO character (account_id, slot, name, race, sex, hair, hair_color, face, map_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            (account_id, input.slot, &input.name, input.race, input.sex, input.hair, input.hair_color, input.face, START_MAP_ID),
        ).map_err(create_storage)?;
        let id = u32::try_from(transaction.last_insert_rowid()).map_err(|_| {
            CreateError::Storage(io::Error::new(
                io::ErrorKind::InvalidData,
                "character ID exceeds u32",
            ))
        })?;
        transaction.commit().map_err(create_storage)?;
        Ok(Character {
            id,
            account_id,
            slot: input.slot,
            name: input.name,
            race: input.race,
            sex: input.sex,
            hair: input.hair,
            hair_color: input.hair_color,
            face: input.face,
            map_id: START_MAP_ID,
            x: 0,
            y: 0,
            direction: 0,
        })
    }
}

fn create_schema(transaction: &Transaction<'_>) -> io::Result<()> {
    transaction
        .execute_batch(
            "CREATE TABLE account (
            id INTEGER PRIMARY KEY,
            username TEXT NOT NULL UNIQUE COLLATE NOCASE,
            password_token TEXT NOT NULL
        );
            CREATE TABLE character (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            account_id INTEGER NOT NULL REFERENCES account(id) ON DELETE CASCADE,
            slot INTEGER NOT NULL CHECK(slot BETWEEN 0 AND 3),
            name TEXT NOT NULL UNIQUE COLLATE NOCASE,
            race INTEGER NOT NULL CHECK(race BETWEEN 0 AND 3),
            sex INTEGER NOT NULL CHECK(sex BETWEEN 0 AND 1),
            hair INTEGER NOT NULL CHECK(hair BETWEEN 0 AND 65535),
            hair_color INTEGER NOT NULL CHECK(hair_color BETWEEN 0 AND 255),
            face INTEGER NOT NULL CHECK(face BETWEEN 0 AND 65535),
            map_id INTEGER NOT NULL,
            x INTEGER NOT NULL DEFAULT 0 CHECK(x BETWEEN 0 AND 255),
            y INTEGER NOT NULL DEFAULT 0 CHECK(y BETWEEN 0 AND 255),
            direction INTEGER NOT NULL DEFAULT 0 CHECK(direction BETWEEN 0 AND 255),
            UNIQUE(account_id, slot)
        );
        CREATE TABLE quest_lifecycle (
            character_id INTEGER PRIMARY KEY REFERENCES character(id) ON DELETE CASCADE,
            points INTEGER NOT NULL CHECK(points BETWEEN 0 AND 65535),
            fame INTEGER NOT NULL CHECK(fame BETWEEN 0 AND 4294967295),
            status INTEGER NOT NULL CHECK(status BETWEEN 0 AND 3),
            credited INTEGER NOT NULL CHECK(credited BETWEEN 0 AND 5),
            deadline INTEGER CHECK(deadline IS NULL OR deadline BETWEEN 0 AND 4294970895),
            last_elapsed INTEGER NOT NULL CHECK(last_elapsed BETWEEN 0 AND 4294967295)
        );",
        )
        .map_err(sql_error)?;
    create_novice_progress_table(transaction)?;
    migrate_v4_to_v5(transaction)?;
    migrate_v5_to_v6(transaction)?;
    migrate_v6_to_v7(transaction)?;
    migrate_v7_to_v8(transaction)?;
    migrate_v8_to_v9(transaction)?;
    migrate_v9_to_v10(transaction)?;
    transaction
        .execute(
            "INSERT INTO account (id, username, password_token) VALUES (?1, ?2, ?3)",
            (DEMO_ACCOUNT_ID, DEMO_USERNAME, DEMO_PASSWORD_TOKEN),
        )
        .map_err(sql_error)?;
    transaction
        .pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(sql_error)
}

fn validate_schema(transaction: &Transaction<'_>) -> io::Result<()> {
    let integrity: String = transaction
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(sql_error)?;
    if integrity != "ok" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("save integrity check failed: {integrity}"),
        ));
    }
    transaction
        .prepare("SELECT id, username, password_token FROM account LIMIT 0")
        .map_err(sql_error)?;
    transaction
        .prepare("SELECT id, account_id, slot, name, race, sex, hair, hair_color, face, map_id, x, y, direction FROM character LIMIT 0")
        .map_err(sql_error)?;
    transaction
        .prepare("SELECT character_id, points, fame, status, credited, deadline, last_elapsed FROM quest_lifecycle LIMIT 0")
        .map_err(sql_error)?;
    transaction.prepare("SELECT character_id,base_level,job_level,base_exp,job_exp,stats_points,skill_points,base_str,base_dex,base_int,base_vit,base_agi,base_mag FROM character_novice_progress LIMIT 0").map_err(|_| novice_progress_error())?;
    transaction.prepare("SELECT character_id,instance_slot,container,item_id,count,durability,identified FROM character_starter_item LIMIT 0").map_err(|_| starter_inventory_error())?;
    transaction.prepare("SELECT character_id,job,has_joint,form,item_instance_bonuses,iris_bonuses,chip_bonuses,skill_bonuses,marionette_bonuses,possession_bonuses,riding_pet_bonuses,map_id,map_flags,hp,max_hp,mp,max_mp,sp,max_sp FROM character_novice_status LIMIT 0").map_err(|_| novice_status_error())?;
    validate_foreign_keys(transaction)?;
    validate_quest_rows(transaction)?;
    validate_novice_progress_rows(transaction)?;
    validate_starter_inventory_rows(transaction)?;
    validate_novice_status_rows(transaction)?;
    Ok(())
}

fn validate_schema_v2(transaction: &Transaction<'_>) -> io::Result<()> {
    let integrity: String = transaction
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(sql_error)?;
    if integrity != "ok" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("save integrity check failed: {integrity}"),
        ));
    }
    transaction
        .prepare("SELECT id, username, password_token FROM account LIMIT 0")
        .map_err(sql_error)?;
    transaction
        .prepare("SELECT id, account_id, slot, name, race, sex, hair, hair_color, face, map_id, x, y, direction FROM character LIMIT 0")
        .map_err(sql_error)?;
    validate_foreign_keys(transaction)
}

fn validate_foreign_keys(transaction: &Transaction<'_>) -> io::Result<()> {
    let mut statement = transaction
        .prepare("PRAGMA foreign_key_check")
        .map_err(sql_error)?;
    if statement
        .query([])
        .map_err(sql_error)?
        .next()
        .map_err(sql_error)?
        .is_some()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "save foreign key check failed",
        ));
    }
    Ok(())
}

fn apply_novice_exp_in_transaction(
    transaction: &Transaction<'_>,
    account_id: u32,
    character_id: u32,
    base_award: u32,
    job_award: u32,
    declare_unmodified_status: bool,
) -> io::Result<[u32; 2]> {
    let (race, sex, map_id): (i64, i64, i64) = transaction
        .query_row(
            "SELECT race,sex,map_id FROM character WHERE account_id = ?1 AND id = ?2",
            (account_id, character_id),
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "character does not exist for account",
            )
        })?;
    if !(0..=2).contains(&race) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "character race is not supported for novice progression",
        ));
    }
    let current = transaction
        .query_row(
            "SELECT base_level, job_level, base_exp, job_exp, stats_points, skill_points,
                    typeof(base_level), typeof(job_level), typeof(base_exp), typeof(job_exp),
                    typeof(stats_points), typeof(skill_points)
             FROM character_novice_progress WHERE character_id = ?1",
            [character_id],
            novice_progress_from_row,
        )
        .optional()
        .map_err(|_| novice_progress_error())?
        .ok_or_else(novice_progress_error)?;
    validate_novice_progress(current).map_err(|_| novice_progress_error())?;
    validate_novice_status_rows(transaction)?;
    validate_reward_context_rows(transaction)?;
    let context = novice_status_for_character(transaction, character_id)?;
    if declare_unmodified_status || context {
        if map_id != i64::from(START_MAP_ID) || !(0..=1).contains(&sex) {
            return Err(novice_status_error());
        }
        let stats = novice_base_stats(transaction, character_id, race as usize)?;
        let items = starter_inventory_for_character(transaction, character_id)?;
        if items != expected_starter_rows(race as usize, sex as usize) {
            return Err(novice_status_error());
        }
        if !context {
            if !declare_unmodified_status {
                return Err(novice_status_error());
            }
            let (hp, mp, sp) = novice_max_vitals(stats, 1);
            transaction.execute(
                    "INSERT INTO character_novice_status(character_id,job,has_joint,form,item_instance_bonuses,iris_bonuses,chip_bonuses,skill_bonuses,marionette_bonuses,possession_bonuses,riding_pet_bonuses,map_id,map_flags,hp,max_hp,mp,max_mp,sp,max_sp) VALUES (?1,0,0,0,0,0,0,0,0,0,0,20013003,0,?2,?2,?3,?3,?4,?4)",
                    (character_id, hp, mp, sp),
                ).map_err(sql_error)?;
            if transaction.changes() != 1 {
                return Err(novice_status_error());
            }
        }
    }
    let base_exp = u32::try_from(current[2])
        .ok()
        .and_then(|value| value.checked_add(base_award))
        .filter(|value| {
            *value
                <= if context || declare_unmodified_status {
                    155
                } else {
                    55
                }
        })
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "base EXP award exceeds novice bound",
            )
        })?;
    let job_exp = u32::try_from(current[3])
        .ok()
        .and_then(|value| value.checked_add(job_award))
        .filter(|value| {
            *value
                <= if context || declare_unmodified_status {
                    155
                } else {
                    55
                }
        })
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "job EXP award exceeds novice bound",
            )
        })?;
    let base_level = if current[0] == 1 && base_exp > 55 {
        2
    } else {
        current[0]
    };
    let job_level = if current[1] == 1 && job_exp > 55 {
        2
    } else {
        current[1]
    };
    let leveled = base_level != current[0] || job_level != current[1];
    let stats_points = if base_level == 2 { 5 } else { 2 };
    let skill_points = if job_level == 2 { 4 } else { 3 };
    if leveled {
        let (hp, mp, sp) = novice_max_vitals(
            novice_base_stats(transaction, character_id, race as usize)?,
            base_level as u32,
        );
        transaction.execute("UPDATE character_novice_status SET hp=?1,max_hp=?1,mp=?2,max_mp=?2,sp=?3,max_sp=?3 WHERE character_id=?4", (hp,mp,sp,character_id)).map_err(sql_error)?;
        if transaction.changes() != 1 {
            return Err(novice_status_error());
        }
    }
    transaction
            .execute(
                "UPDATE character_novice_progress SET base_exp = ?1, job_exp = ?2,base_level=?3,job_level=?4,stats_points=?5,skill_points=?6 WHERE character_id = ?7",
                (base_exp, job_exp, base_level, job_level, stats_points, skill_points, character_id),
            )
            .map_err(sql_error)?;
    if transaction.changes() != 1 {
        return Err(novice_progress_error());
    }
    validate_novice_progress_rows(transaction)?;
    validate_novice_status_rows(transaction)?;
    validate_reward_context_rows(transaction)?;
    Ok([base_exp, job_exp])
}

fn validate_quest_rows(transaction: &Transaction<'_>) -> io::Result<()> {
    let duplicate: bool = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM quest_lifecycle GROUP BY character_id HAVING COUNT(*)>1)",
            [],
            |row| row.get(0),
        )
        .map_err(|_| quest_state_error())?;
    if duplicate {
        return Err(quest_state_error());
    }
    let mut statement = transaction
        .prepare(
            "SELECT character_id, points, fame, status, credited, deadline, last_elapsed,
                    typeof(points), typeof(fame), typeof(status), typeof(credited),
                    typeof(deadline), typeof(last_elapsed)
             FROM quest_lifecycle",
        )
        .map_err(sql_error)?;
    let mut rows = statement.query([]).map_err(sql_error)?;
    while let Some(row) = rows.next().map_err(sql_error)? {
        let _character_id: i64 = row.get(0).map_err(sql_error)?;
        let state = lifecycle_state_from_row(row).map_err(|_| quest_state_error())?;
        state.validate().map_err(|_| quest_state_error())?;
    }
    Ok(())
}

fn lifecycle_state_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LifecycleState> {
    for (index, expected) in [
        (7, "integer"),
        (8, "integer"),
        (9, "integer"),
        (10, "integer"),
        (12, "integer"),
    ] {
        if row.get::<_, String>(index)? != expected {
            return Err(rusqlite::Error::InvalidQuery);
        }
    }
    let deadline_type: String = row.get(11)?;
    if deadline_type != "null" && deadline_type != "integer" {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let status = match row.get::<_, i64>(3)? {
        0 => QuestStatus::None,
        1 => QuestStatus::Open,
        2 => QuestStatus::Completed,
        3 => QuestStatus::Failed,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    Ok(LifecycleState {
        points: row
            .get::<_, i64>(1)?
            .try_into()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        fame: row
            .get::<_, i64>(2)?
            .try_into()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        status,
        credited: row
            .get::<_, i64>(4)?
            .try_into()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        deadline: row
            .get::<_, Option<i64>>(5)?
            .map(u64::try_from)
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        last_elapsed: row
            .get::<_, i64>(6)?
            .try_into()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
    })
}

fn migrate_v1_to_v2(transaction: &Transaction<'_>) -> io::Result<()> {
    validate_v1_schema(transaction)?;
    transaction
        .execute_batch(
            "ALTER TABLE character ADD COLUMN x INTEGER NOT NULL DEFAULT 0 CHECK(x BETWEEN 0 AND 255);
             ALTER TABLE character ADD COLUMN y INTEGER NOT NULL DEFAULT 0 CHECK(y BETWEEN 0 AND 255);
             ALTER TABLE character ADD COLUMN direction INTEGER NOT NULL DEFAULT 0 CHECK(direction BETWEEN 0 AND 255);",
        )
        .map_err(sql_error)?;
    transaction
        .pragma_update(None, "user_version", 2)
        .map_err(sql_error)
}

fn migrate_v2_to_v3(transaction: &Transaction<'_>) -> io::Result<()> {
    transaction
        .execute_batch(
            "CREATE TABLE quest_lifecycle (
                character_id INTEGER PRIMARY KEY REFERENCES character(id) ON DELETE CASCADE,
                points INTEGER NOT NULL CHECK(points BETWEEN 0 AND 65535),
                fame INTEGER NOT NULL CHECK(fame BETWEEN 0 AND 4294967295),
                status INTEGER NOT NULL CHECK(status BETWEEN 0 AND 3),
                credited INTEGER NOT NULL CHECK(credited BETWEEN 0 AND 5),
                deadline INTEGER CHECK(deadline IS NULL OR deadline BETWEEN 0 AND 4294970895),
                last_elapsed INTEGER NOT NULL CHECK(last_elapsed BETWEEN 0 AND 4294967295)
            );",
        )
        .map_err(sql_error)?;
    transaction
        .pragma_update(None, "user_version", 3)
        .map_err(sql_error)
}

fn migrate_v3_to_v4(transaction: &Transaction<'_>) -> io::Result<()> {
    create_novice_progress_table(transaction)?;
    transaction
        .pragma_update(None, "user_version", 4)
        .map_err(sql_error)
}

fn migrate_v4_to_v5(transaction: &Transaction<'_>) -> io::Result<()> {
    transaction.execute_batch("ALTER TABLE character_novice_progress ADD COLUMN base_str INTEGER; ALTER TABLE character_novice_progress ADD COLUMN base_dex INTEGER; ALTER TABLE character_novice_progress ADD COLUMN base_int INTEGER; ALTER TABLE character_novice_progress ADD COLUMN base_vit INTEGER; ALTER TABLE character_novice_progress ADD COLUMN base_agi INTEGER; ALTER TABLE character_novice_progress ADD COLUMN base_mag INTEGER;").map_err(sql_error)?;
    transaction
        .pragma_update(None, "user_version", 5)
        .map_err(sql_error)
}

fn migrate_v5_to_v6(transaction: &Transaction<'_>) -> io::Result<()> {
    transaction
        .execute_batch(
            "CREATE TABLE character_starter_item (
        character_id INTEGER NOT NULL REFERENCES character(id) ON DELETE CASCADE,
        instance_slot INTEGER NOT NULL CHECK(instance_slot BETWEEN 1 AND 5),
        container INTEGER NOT NULL CHECK(container IN (2,11,12,16)),
        item_id INTEGER NOT NULL CHECK(item_id BETWEEN 0 AND 4294967295),
        count INTEGER NOT NULL CHECK(count = 1),
        durability INTEGER NOT NULL CHECK(durability BETWEEN 0 AND 4294967295),
        identified INTEGER NOT NULL CHECK(identified = 1),
        PRIMARY KEY(character_id,instance_slot)
    );",
        )
        .map_err(sql_error)?;
    transaction
        .pragma_update(None, "user_version", 6)
        .map_err(sql_error)
}

fn migrate_v6_to_v7(transaction: &Transaction<'_>) -> io::Result<()> {
    transaction.execute_batch("CREATE TABLE character_novice_status (
        character_id INTEGER PRIMARY KEY REFERENCES character(id) ON DELETE CASCADE,
        job INTEGER NOT NULL CHECK(job=0), has_joint INTEGER NOT NULL CHECK(has_joint=0), form INTEGER NOT NULL CHECK(form=0),
        item_instance_bonuses INTEGER NOT NULL CHECK(item_instance_bonuses=0), iris_bonuses INTEGER NOT NULL CHECK(iris_bonuses=0),
        chip_bonuses INTEGER NOT NULL CHECK(chip_bonuses=0), skill_bonuses INTEGER NOT NULL CHECK(skill_bonuses=0),
        marionette_bonuses INTEGER NOT NULL CHECK(marionette_bonuses=0), possession_bonuses INTEGER NOT NULL CHECK(possession_bonuses=0),
        riding_pet_bonuses INTEGER NOT NULL CHECK(riding_pet_bonuses=0), map_id INTEGER NOT NULL CHECK(map_id=20013003), map_flags INTEGER NOT NULL CHECK(map_flags=0),
        hp INTEGER NOT NULL, max_hp INTEGER NOT NULL, mp INTEGER NOT NULL, max_mp INTEGER NOT NULL, sp INTEGER NOT NULL, max_sp INTEGER NOT NULL
    );").map_err(sql_error)?;
    transaction
        .pragma_update(None, "user_version", 7)
        .map_err(sql_error)
}

fn migrate_v7_to_v8(transaction: &Transaction<'_>) -> io::Result<()> {
    transaction
        .execute_batch(
            "CREATE TABLE character_gold (
        character_id INTEGER PRIMARY KEY REFERENCES character(id) ON DELETE CASCADE,
        gold INTEGER NOT NULL CHECK(gold BETWEEN 0 AND 99999999)
    );",
        )
        .map_err(sql_error)?;
    transaction
        .pragma_update(None, "user_version", 8)
        .map_err(sql_error)
}

fn migrate_v8_to_v9(transaction: &Transaction<'_>) -> io::Result<()> {
    transaction
        .execute_batch(
            "CREATE TABLE character_reward_context (
        character_id INTEGER PRIMARY KEY REFERENCES character(id) ON DELETE CASCADE,
        ep INTEGER NOT NULL CHECK(ep BETWEEN 0 AND 30),
        cp INTEGER NOT NULL CHECK(cp BETWEEN 0 AND 99999999),
        ringless INTEGER NOT NULL CHECK(ringless=1)
    );",
        )
        .map_err(sql_error)?;
    transaction
        .pragma_update(None, "user_version", 9)
        .map_err(sql_error)
}

fn migrate_v9_to_v10(transaction: &Transaction<'_>) -> io::Result<()> {
    // Profile 1: zero bonuses/refinement, unlocked, non-rental, empty cards, no possession; rental time is unused.
    transaction
        .execute_batch(
            "CREATE TABLE character_reward_inventory (
        character_id INTEGER PRIMARY KEY REFERENCES character(id) ON DELETE CASCADE,
        next_slot INTEGER NOT NULL CHECK(next_slot BETWEEN 6 AND 4294967295),
        starter_only INTEGER NOT NULL CHECK(starter_only=1)
    );
    CREATE TABLE character_reward_item (
        character_id INTEGER NOT NULL REFERENCES character(id) ON DELETE CASCADE,
        instance_slot INTEGER NOT NULL CHECK(instance_slot BETWEEN 6 AND 4294967295),
        container INTEGER NOT NULL CHECK(container=2),
        item_id INTEGER NOT NULL CHECK(item_id=60060000),
        count INTEGER NOT NULL CHECK(count=1),
        durability INTEGER NOT NULL CHECK(durability=0),
        identified INTEGER NOT NULL CHECK(identified=1),
        pict_id INTEGER NOT NULL CHECK(pict_id=0),
        default_profile INTEGER NOT NULL CHECK(default_profile=1),
        PRIMARY KEY(character_id,instance_slot)
    );",
        )
        .map_err(sql_error)?;
    transaction
        .pragma_update(None, "user_version", 10)
        .map_err(sql_error)
}

fn reward_inventory_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "saved reward inventory state is invalid",
    )
}

fn validate_reward_inventory_rows(transaction: &Transaction<'_>) -> io::Result<()> {
    transaction
        .prepare(
            "SELECT character_id,next_slot,starter_only FROM character_reward_inventory LIMIT 0",
        )
        .map_err(|_| reward_inventory_error())?;
    transaction.prepare("SELECT character_id,instance_slot,container,item_id,count,durability,identified,pict_id,default_profile FROM character_reward_item LIMIT 0").map_err(|_| reward_inventory_error())?;
    let mut stmt = transaction.prepare("SELECT i.character_id,i.next_slot,i.starter_only,typeof(i.character_id),typeof(i.next_slot),typeof(i.starter_only),c.account_id,c.race,c.sex,c.map_id,p.character_id,s.character_id,g.character_id,r.character_id FROM character_reward_inventory i LEFT JOIN character c ON c.id=i.character_id LEFT JOIN character_novice_progress p ON p.character_id=i.character_id LEFT JOIN character_novice_status s ON s.character_id=i.character_id LEFT JOIN character_gold g ON g.character_id=i.character_id LEFT JOIN character_reward_context r ON r.character_id=i.character_id").map_err(|_| reward_inventory_error())?;
    let mut rows = stmt.query([]).map_err(|_| reward_inventory_error())?;
    while let Some(row) = rows.next().map_err(|_| reward_inventory_error())? {
        let id: i64 = row.get(0).map_err(|_| reward_inventory_error())?;
        let next: i64 = row.get(1).map_err(|_| reward_inventory_error())?;
        let starter: i64 = row.get(2).map_err(|_| reward_inventory_error())?;
        if !(1..=i64::from(u32::MAX)).contains(&id)
            || !(6..=i64::from(u32::MAX)).contains(&next)
            || starter != 1
            || (3..=5).any(|i| row.get::<_, String>(i).ok().as_deref() != Some("integer"))
            || row
                .get::<_, Option<i64>>(6)
                .map_err(|_| reward_inventory_error())?
                .is_none()
            || !matches!(
                row.get::<_, Option<i64>>(7)
                    .map_err(|_| reward_inventory_error())?,
                Some(0..=2)
            )
            || !matches!(
                row.get::<_, Option<i64>>(8)
                    .map_err(|_| reward_inventory_error())?,
                Some(0..=1)
            )
            || row
                .get::<_, Option<i64>>(9)
                .map_err(|_| reward_inventory_error())?
                != Some(i64::from(START_MAP_ID))
            || (10..=13).any(|i| row.get::<_, Option<i64>>(i).ok().flatten().is_none())
        {
            return Err(reward_inventory_error());
        }
        let progress=transaction.query_row("SELECT base_level,job_level,base_exp,job_exp,stats_points,skill_points,typeof(base_level),typeof(job_level),typeof(base_exp),typeof(job_exp),typeof(stats_points),typeof(skill_points) FROM character_novice_progress WHERE character_id=?1",[id],novice_progress_from_row).map_err(|_|reward_inventory_error())?;
        validate_novice_progress(progress).map_err(|_| reward_inventory_error())?;
        let race: usize = row.get(7).map_err(|_| reward_inventory_error())?;
        let sex: usize = row.get(8).map_err(|_| reward_inventory_error())?;
        if starter_inventory_for_character(transaction, id as u32)?
            != expected_starter_rows(race, sex)
        {
            return Err(reward_inventory_error());
        }
    }
    let mut stmt=transaction.prepare("SELECT character_id,instance_slot,container,item_id,count,durability,identified,pict_id,default_profile,typeof(character_id),typeof(instance_slot),typeof(container),typeof(item_id),typeof(count),typeof(durability),typeof(identified),typeof(pict_id),typeof(default_profile) FROM character_reward_item").map_err(|_|reward_inventory_error())?;
    let mut rows = stmt.query([]).map_err(|_| reward_inventory_error())?;
    while let Some(row) = rows.next().map_err(|_| reward_inventory_error())? {
        let vals: [i64; 9] = std::array::from_fn(|i| row.get(i).unwrap_or(i64::MIN));
        if (9..18).any(|i| row.get::<_, String>(i).ok().as_deref() != Some("integer"))
            || !(1..=i64::from(u32::MAX)).contains(&vals[0])
            || !(6..=i64::from(u32::MAX)).contains(&vals[1])
            || vals[2..] != [2, 60_060_000, 1, 0, 1, 0, 1]
        {
            return Err(reward_inventory_error());
        }
    }
    let duplicates:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM character_reward_inventory GROUP BY character_id HAVING COUNT(*)>1) OR EXISTS(SELECT 1 FROM character_reward_item GROUP BY character_id,instance_slot HAVING COUNT(*)>1) OR EXISTS(SELECT 1 FROM character_reward_item r LEFT JOIN character_reward_inventory i ON i.character_id=r.character_id WHERE i.character_id IS NULL)",[],|r|r.get(0)).map_err(|_|reward_inventory_error())?;
    let allocation:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM character_reward_inventory i WHERE i.next_slot != 6 + (SELECT COUNT(*) FROM character_reward_item r WHERE r.character_id=i.character_id) OR i.next_slot != COALESCE((SELECT MAX(instance_slot)+1 FROM character_reward_item r WHERE r.character_id=i.character_id),6))",[],|r|r.get(0)).map_err(|_|reward_inventory_error())?;
    if duplicates || allocation {
        return Err(reward_inventory_error());
    }
    Ok(())
}

fn validate_reward_context_rows(transaction: &Transaction<'_>) -> io::Result<()> {
    let mut stmt = transaction.prepare("SELECT r.character_id,r.ep,r.cp,r.ringless,typeof(r.character_id),typeof(r.ep),typeof(r.cp),typeof(r.ringless),c.account_id,c.race,c.sex,c.map_id,p.character_id,s.character_id FROM character_reward_context r LEFT JOIN character c ON c.id=r.character_id LEFT JOIN character_novice_progress p ON p.character_id=r.character_id LEFT JOIN character_novice_status s ON s.character_id=r.character_id").map_err(|_| reward_context_error())?;
    let mut rows = stmt.query([]).map_err(|_| reward_context_error())?;
    while let Some(r) = rows.next().map_err(|_| reward_context_error())? {
        let id: i64 = r.get(0).map_err(|_| reward_context_error())?;
        let ep: i64 = r.get(1).map_err(|_| reward_context_error())?;
        let cp: i64 = r.get(2).map_err(|_| reward_context_error())?;
        let ringless: i64 = r.get(3).map_err(|_| reward_context_error())?;
        if id <= 0
            || id > i64::from(u32::MAX)
            || !(0..=i64::from(NOVICE_MAX_EP)).contains(&ep)
            || !(0..=99_999_999).contains(&cp)
            || ringless != 1
            || (4..=7).any(|i| r.get::<_, String>(i).ok().as_deref() != Some("integer"))
            || r.get::<_, Option<i64>>(8)
                .map_err(|_| reward_context_error())?
                .is_none()
            || !matches!(
                r.get::<_, Option<i64>>(9)
                    .map_err(|_| reward_context_error())?,
                Some(0..=2)
            )
            || !matches!(
                r.get::<_, Option<i64>>(10)
                    .map_err(|_| reward_context_error())?,
                Some(0..=1)
            )
            || r.get::<_, Option<i64>>(11)
                .map_err(|_| reward_context_error())?
                != Some(i64::from(START_MAP_ID))
            || r.get::<_, Option<i64>>(12)
                .map_err(|_| reward_context_error())?
                .is_none()
            || r.get::<_, Option<i64>>(13)
                .map_err(|_| reward_context_error())?
                .is_none()
        {
            return Err(reward_context_error());
        }
        let progress = transaction.query_row("SELECT base_level,job_level,base_exp,job_exp,stats_points,skill_points,typeof(base_level),typeof(job_level),typeof(base_exp),typeof(job_exp),typeof(stats_points),typeof(skill_points) FROM character_novice_progress WHERE character_id=?1", [id], novice_progress_from_row).map_err(|_| reward_context_error())?;
        validate_novice_progress(progress).map_err(|_| reward_context_error())?;
    }
    let duplicate: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM character_reward_context GROUP BY character_id HAVING COUNT(*)>1)", [], |r| r.get(0)).map_err(|_| reward_context_error())?;
    if duplicate {
        return Err(reward_context_error());
    }
    Ok(())
}

fn reward_context_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "character reward context is invalid",
    )
}

fn validate_gold_rows(transaction: &Transaction<'_>) -> io::Result<()> {
    transaction
        .prepare("SELECT character_id,gold FROM character_gold LIMIT 0")
        .map_err(|_| gold_error())?;
    let mut statement = transaction
        .prepare("SELECT character_id,gold,typeof(character_id),typeof(gold) FROM character_gold")
        .map_err(|_| gold_error())?;
    let mut rows = statement.query([]).map_err(|_| gold_error())?;
    while let Some(row) = rows.next().map_err(|_| gold_error())? {
        let id: i64 = row.get(0).map_err(|_| gold_error())?;
        let gold: i64 = row.get(1).map_err(|_| gold_error())?;
        if id <= 0
            || id > u32::MAX as i64
            || gold < 0
            || gold > 99_999_999
            || row.get::<_, String>(2).map_err(|_| gold_error())? != "integer"
            || row.get::<_, String>(3).map_err(|_| gold_error())? != "integer"
        {
            return Err(gold_error());
        }
    }
    let orphan: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM character_gold g LEFT JOIN character c ON c.id=g.character_id WHERE c.id IS NULL)", [], |r| r.get(0)).map_err(sql_error)?;
    if orphan {
        return Err(gold_error());
    }
    let duplicate: bool = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM character_gold GROUP BY character_id HAVING COUNT(*)>1)",
            [],
            |r| r.get(0),
        )
        .map_err(sql_error)?;
    if duplicate {
        return Err(gold_error());
    }
    Ok(())
}

fn gold_error() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "character gold data is invalid")
}

fn validate_schema_v6(transaction: &Transaction<'_>) -> io::Result<()> {
    validate_schema_v5(transaction)?;
    transaction.prepare("SELECT character_id,instance_slot,container,item_id,count,durability,identified FROM character_starter_item LIMIT 0").map_err(|_| starter_inventory_error())?;
    validate_starter_inventory_rows(transaction)?;
    validate_legacy_novice_progress_rows(transaction)
}

fn validate_schema_v5(transaction: &Transaction<'_>) -> io::Result<()> {
    validate_schema_v4(transaction)?;
    // Validate the v5-added nullable base stats before extending the schema.
    validate_novice_progress_rows(transaction)
}

fn validate_schema_v4(transaction: &Transaction<'_>) -> io::Result<()> {
    validate_schema_v3(transaction)?;
    transaction.prepare("SELECT character_id,base_level,job_level,base_exp,job_exp,stats_points,skill_points FROM character_novice_progress LIMIT 0").map_err(sql_error)?;
    let mut statement = transaction.prepare("SELECT base_level,job_level,base_exp,job_exp,stats_points,skill_points,typeof(base_level),typeof(job_level),typeof(base_exp),typeof(job_exp),typeof(stats_points),typeof(skill_points) FROM character_novice_progress").map_err(sql_error)?;
    let mut rows = statement.query([]).map_err(sql_error)?;
    while let Some(row) = rows.next().map_err(sql_error)? {
        validate_novice_progress(
            novice_progress_from_row(row).map_err(|_| novice_progress_error())?,
        )?;
    }
    let orphan: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM character_novice_progress p LEFT JOIN character c ON c.id=p.character_id WHERE c.id IS NULL OR c.race NOT BETWEEN 0 AND 2)", [], |row| row.get(0)).map_err(sql_error)?;
    if orphan {
        return Err(novice_progress_error());
    }
    validate_legacy_novice_progress_rows(transaction)?;
    Ok(())
}

fn create_novice_progress_table(transaction: &Transaction<'_>) -> io::Result<()> {
    transaction
        .execute_batch(
            "CREATE TABLE character_novice_progress (
        character_id INTEGER PRIMARY KEY REFERENCES character(id) ON DELETE CASCADE,
        base_level INTEGER NOT NULL CHECK(base_level BETWEEN 0 AND 255),
        job_level INTEGER NOT NULL CHECK(job_level BETWEEN 0 AND 255),
        base_exp INTEGER NOT NULL CHECK(base_exp BETWEEN 0 AND 4294967295),
        job_exp INTEGER NOT NULL CHECK(job_exp BETWEEN 0 AND 4294967295),
        stats_points INTEGER NOT NULL CHECK(stats_points BETWEEN 0 AND 65535),
        skill_points INTEGER NOT NULL CHECK(skill_points BETWEEN 0 AND 65535)
    );",
        )
        .map_err(sql_error)
}

fn validate_schema_v3(transaction: &Transaction<'_>) -> io::Result<()> {
    validate_schema_v2(transaction)?;
    transaction.prepare("SELECT character_id, points, fame, status, credited, deadline, last_elapsed FROM quest_lifecycle LIMIT 0").map_err(sql_error)?;
    validate_quest_rows(transaction)
}

fn novice_progress_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "saved novice progression state is invalid",
    )
}

fn novice_status_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "saved novice status context is invalid",
    )
}

fn novice_base_stats(
    transaction: &Transaction<'_>,
    character_id: u32,
    race: usize,
) -> io::Result<[u32; 6]> {
    let values: [Option<i64>; 6] = transaction.query_row("SELECT base_str,base_dex,base_int,base_vit,base_agi,base_mag FROM character_novice_progress WHERE character_id=?1", [character_id], |row| Ok([row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?])).optional().map_err(|_| novice_status_error())?.ok_or_else(novice_status_error)?;
    let stats = values.map(|value| value.and_then(|v| u32::try_from(v).ok()));
    let stats = stats
        .map(|value| value.ok_or_else(novice_status_error))
        .into_iter()
        .collect::<io::Result<Vec<_>>>()?;
    let stats: [u32; 6] = stats.try_into().map_err(|_| novice_status_error())?;
    if stats != NOVICE_BASE_STATS[race] {
        return Err(novice_status_error());
    }
    Ok(stats)
}

fn novice_max_vitals(stats: [u32; 6], level: u32) -> (u32, u32, u32) {
    let v = stats[3];
    let m = stats[5];
    let i = stats[2];
    let hp = 3 * v + (v / 5).pow(2) + 2 * level + (level / 5).pow(2) + 50;
    let mp = 3 * m + level + (level / 9).pow(2) + 30;
    let sp = i + v + level + (level / 9).pow(2) + 20;
    (hp, mp, sp)
}

fn novice_status_for_character(
    transaction: &Transaction<'_>,
    character_id: u32,
) -> io::Result<bool> {
    transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM character_novice_status WHERE character_id=?1)",
            [character_id],
            |r| r.get(0),
        )
        .map_err(|_| novice_status_error())
}

fn validate_novice_status_rows(transaction: &Transaction<'_>) -> io::Result<()> {
    let duplicate: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM character_novice_status GROUP BY character_id HAVING COUNT(*)!=1)", [], |r| r.get(0)).map_err(|_| novice_status_error())?;
    if duplicate {
        return Err(novice_status_error());
    }
    let mut stmt = transaction.prepare("SELECT s.character_id,s.job,s.has_joint,s.form,s.item_instance_bonuses,s.iris_bonuses,s.chip_bonuses,s.skill_bonuses,s.marionette_bonuses,s.possession_bonuses,s.riding_pet_bonuses,s.map_id,s.map_flags,s.hp,s.max_hp,s.mp,s.max_mp,s.sp,s.max_sp,typeof(s.character_id),typeof(s.job),typeof(s.has_joint),typeof(s.form),typeof(s.item_instance_bonuses),typeof(s.iris_bonuses),typeof(s.chip_bonuses),typeof(s.skill_bonuses),typeof(s.marionette_bonuses),typeof(s.possession_bonuses),typeof(s.riding_pet_bonuses),typeof(s.map_id),typeof(s.map_flags),typeof(s.hp),typeof(s.max_hp),typeof(s.mp),typeof(s.max_mp),typeof(s.sp),typeof(s.max_sp),c.race,c.sex,p.base_level,p.base_str,p.base_dex,p.base_int,p.base_vit,p.base_agi,p.base_mag,c.map_id FROM character_novice_status s LEFT JOIN character c ON c.id=s.character_id LEFT JOIN character_novice_progress p ON p.character_id=s.character_id").map_err(|_| novice_status_error())?;
    let mut rows = stmt.query([]).map_err(|_| novice_status_error())?;
    while let Some(r) = rows.next().map_err(|_| novice_status_error())? {
        for i in 19..38 {
            if r.get::<_, String>(i).map_err(|_| novice_status_error())? != "integer" {
                return Err(novice_status_error());
            }
        }
        let v: Vec<i64> = (0..19)
            .map(|i| r.get(i).map_err(|_| novice_status_error()))
            .collect::<io::Result<_>>()?;
        if !(1..=i64::from(u32::MAX)).contains(&v[0])
            || v[1..13] != [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 20013003, 0]
        {
            return Err(novice_status_error());
        }
        let race: usize = r
            .get::<_, i64>(38)
            .ok()
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n < 3)
            .ok_or_else(novice_status_error)?;
        let sex: i64 = r.get(39).map_err(|_| novice_status_error())?;
        let level: i64 = r.get(40).map_err(|_| novice_status_error())?;
        let stats = novice_base_stats(
            transaction,
            u32::try_from(v[0]).map_err(|_| novice_status_error())?,
            race,
        )?;
        if !(0..=1).contains(&sex)
            || !(1..=2).contains(&level)
            || r.get::<_, i64>(47).map_err(|_| novice_status_error())? != 20013003
            || starter_inventory_for_character(
                transaction,
                u32::try_from(v[0]).map_err(|_| novice_status_error())?,
            )? != expected_starter_rows(race, sex as usize)
        {
            return Err(novice_status_error());
        }
        let (hp, mp, sp) = novice_max_vitals(stats, level as u32);
        if v[14] != i64::from(hp)
            || v[16] != i64::from(mp)
            || v[18] != i64::from(sp)
            || !(0..=v[14]).contains(&v[13])
            || !(0..=v[16]).contains(&v[15])
            || !(0..=v[18]).contains(&v[17])
        {
            return Err(novice_status_error());
        }
    }
    let invalid: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM character_novice_progress p LEFT JOIN character_novice_status s ON s.character_id=p.character_id WHERE (p.base_level=2 OR p.job_level=2) AND s.character_id IS NULL)", [], |r| r.get(0)).map_err(|_| novice_status_error())?;
    if invalid {
        return Err(novice_status_error());
    }
    Ok(())
}

fn validate_legacy_novice_progress_rows(transaction: &Transaction<'_>) -> io::Result<()> {
    let mut s=transaction.prepare("SELECT base_level,job_level,base_exp,job_exp,stats_points,skill_points,typeof(base_level),typeof(job_level),typeof(base_exp),typeof(job_exp),typeof(stats_points),typeof(skill_points) FROM character_novice_progress").map_err(|_|novice_progress_error())?;
    let mut rows = s.query([]).map_err(|_| novice_progress_error())?;
    while let Some(r) = rows.next().map_err(|_| novice_progress_error())? {
        let v = novice_progress_from_row(r).map_err(|_| novice_progress_error())?;
        if v[0] != 1
            || v[1] != 1
            || !(0..=55).contains(&v[2])
            || !(0..=55).contains(&v[3])
            || v[4] != 2
            || v[5] != 3
        {
            return Err(novice_progress_error());
        }
    }
    Ok(())
}

fn starter_inventory_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "saved starter inventory state is invalid",
    )
}

fn expected_starter_rows(race: usize, sex: usize) -> Vec<(u32, u32)> {
    let mut durability = STARTER_DURABILITY;
    if race == 1 && sex == 1 {
        durability[1] = 10;
    }
    STARTER_ITEMS[race][sex]
        .iter()
        .copied()
        .zip(durability)
        .collect()
}

fn starter_inventory_for_character(
    transaction: &Transaction<'_>,
    character_id: u32,
) -> io::Result<Vec<(u32, u32)>> {
    let mut statement = transaction.prepare("SELECT instance_slot,container,item_id,count,durability,identified,typeof(instance_slot),typeof(container),typeof(item_id),typeof(count),typeof(durability),typeof(identified) FROM character_starter_item WHERE character_id=?1 ORDER BY instance_slot").map_err(|_| starter_inventory_error())?;
    let mut rows = statement
        .query([character_id])
        .map_err(|_| starter_inventory_error())?;
    let mut values = Vec::new();
    while let Some(row) = rows.next().map_err(|_| starter_inventory_error())? {
        for index in 6..12 {
            if row
                .get::<_, String>(index)
                .map_err(|_| starter_inventory_error())?
                != "integer"
            {
                return Err(starter_inventory_error());
            }
        }
        let slot: i64 = row.get(0).map_err(|_| starter_inventory_error())?;
        let container: i64 = row.get(1).map_err(|_| starter_inventory_error())?;
        let item: i64 = row.get(2).map_err(|_| starter_inventory_error())?;
        let count: i64 = row.get(3).map_err(|_| starter_inventory_error())?;
        let durability: i64 = row.get(4).map_err(|_| starter_inventory_error())?;
        let identified: i64 = row.get(5).map_err(|_| starter_inventory_error())?;
        let index = usize::try_from(slot)
            .ok()
            .and_then(|value| value.checked_sub(1))
            .filter(|i| *i < 5 && *i == values.len())
            .ok_or_else(starter_inventory_error)?;
        if container != [11, 12, 16, 2, 2][index] || count != 1 || identified != 1 {
            return Err(starter_inventory_error());
        }
        values.push((
            u32::try_from(item).map_err(|_| starter_inventory_error())?,
            u32::try_from(durability).map_err(|_| starter_inventory_error())?,
        ));
    }
    Ok(values)
}

fn validate_starter_inventory_rows(transaction: &Transaction<'_>) -> io::Result<()> {
    let mut statement = transaction.prepare("SELECT DISTINCT s.character_id,c.race,c.sex,typeof(s.character_id),typeof(c.race),typeof(c.sex),EXISTS(SELECT 1 FROM character_novice_progress p WHERE p.character_id=s.character_id) FROM character_starter_item s LEFT JOIN character c ON c.id=s.character_id").map_err(|_| starter_inventory_error())?;
    let mut rows = statement.query([]).map_err(|_| starter_inventory_error())?;
    while let Some(row) = rows.next().map_err(|_| starter_inventory_error())? {
        let id: i64 = row.get(0).map_err(|_| starter_inventory_error())?;
        let race: Option<i64> = row.get(1).map_err(|_| starter_inventory_error())?;
        let sex: Option<i64> = row.get(2).map_err(|_| starter_inventory_error())?;
        if row
            .get::<_, String>(3)
            .map_err(|_| starter_inventory_error())?
            != "integer"
            || row
                .get::<_, String>(4)
                .map_err(|_| starter_inventory_error())?
                != "integer"
            || row
                .get::<_, String>(5)
                .map_err(|_| starter_inventory_error())?
                != "integer"
            || !row
                .get::<_, bool>(6)
                .map_err(|_| starter_inventory_error())?
            || !(1..=i64::from(u32::MAX)).contains(&id)
        {
            return Err(starter_inventory_error());
        }
        let (race, sex) = match (race, sex) {
            (Some(r), Some(s)) if (0..=2).contains(&r) && (0..=1).contains(&s) => {
                (r as usize, s as usize)
            }
            _ => return Err(starter_inventory_error()),
        };
        let values = starter_inventory_for_character(
            transaction,
            u32::try_from(id).map_err(|_| starter_inventory_error())?,
        )?;
        if values != expected_starter_rows(race, sex) {
            return Err(starter_inventory_error());
        }
    }
    Ok(())
}

fn novice_progress_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<[i64; 6]> {
    for index in 6..12 {
        if row.get::<_, String>(index)? != "integer" {
            return Err(rusqlite::Error::InvalidQuery);
        }
    }
    Ok([
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
    ])
}

fn validate_novice_progress(values: [i64; 6]) -> io::Result<()> {
    if (1..=2).contains(&values[0])
        && (1..=2).contains(&values[1])
        && (0..=155).contains(&values[2])
        && (0..=155).contains(&values[3])
        && (values[0] == 1) == (values[2] <= 55)
        && (values[1] == 1) == (values[3] <= 55)
        && values[4] == if values[0] == 1 { 2 } else { 5 }
        && values[5] == if values[1] == 1 { 3 } else { 4 }
    {
        Ok(())
    } else {
        Err(novice_progress_error())
    }
}

fn validate_novice_progress_rows(transaction: &Transaction<'_>) -> io::Result<()> {
    let mut keys = transaction.prepare(
        "SELECT character_id, typeof(character_id), COUNT(*) FROM character_novice_progress GROUP BY character_id"
    ).map_err(|_| novice_progress_error())?;
    let mut key_rows = keys.query([]).map_err(sql_error)?;
    while let Some(row) = key_rows.next().map_err(sql_error)? {
        let key: i64 = row.get(0).map_err(|_| novice_progress_error())?;
        if row
            .get::<_, String>(1)
            .map_err(|_| novice_progress_error())?
            != "integer"
            || row.get::<_, i64>(2).map_err(|_| novice_progress_error())? != 1
            || !(1..=i64::from(u32::MAX)).contains(&key)
        {
            return Err(novice_progress_error());
        }
    }
    let mut statement = transaction.prepare(
        "SELECT p.base_level, p.job_level, p.base_exp, p.job_exp, p.stats_points, p.skill_points,
                typeof(base_level), typeof(job_level), typeof(base_exp), typeof(job_exp),
                typeof(stats_points), typeof(skill_points), base_str,base_dex,base_int,base_vit,base_agi,base_mag,
                typeof(base_str),typeof(base_dex),typeof(base_int),typeof(base_vit),typeof(base_agi),typeof(base_mag), c.race
         FROM character_novice_progress p JOIN character c ON c.id=p.character_id"
    ).map_err(|_| novice_progress_error())?;
    let mut rows = statement.query([]).map_err(sql_error)?;
    while let Some(row) = rows.next().map_err(sql_error)? {
        validate_novice_progress(
            novice_progress_from_row(row).map_err(|_| novice_progress_error())?,
        )?;
        let mut empty = 0;
        for index in 18..24 {
            match row
                .get::<_, String>(index)
                .map_err(|_| novice_progress_error())?
                .as_str()
            {
                "null" => empty += 1,
                "integer"
                    if row
                        .get::<_, i64>(index - 6)
                        .map_err(|_| novice_progress_error())?
                        > 0 =>
                {
                    ()
                }
                _ => return Err(novice_progress_error()),
            }
        }
        if empty != 0 && empty != 6 {
            return Err(novice_progress_error());
        }
        if empty == 0 {
            let race: usize = row
                .get::<_, i64>(24)
                .ok()
                .and_then(|value| usize::try_from(value).ok())
                .filter(|value| *value < 3)
                .ok_or_else(novice_progress_error)?;
            for (index, expected) in NOVICE_BASE_STATS[race].iter().enumerate() {
                if row
                    .get::<_, i64>(12 + index)
                    .map_err(|_| novice_progress_error())?
                    != i64::from(*expected)
                {
                    return Err(novice_progress_error());
                }
            }
        }
    }
    let invalid: bool = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM character_novice_progress p
         LEFT JOIN character c ON c.id=p.character_id
         WHERE c.id IS NULL OR typeof(c.race)!='integer' OR c.race NOT BETWEEN 0 AND 2)",
            [],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    if invalid {
        return Err(novice_progress_error());
    }
    Ok(())
}

fn validate_v1_schema(transaction: &Transaction<'_>) -> io::Result<()> {
    let integrity: String = transaction
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(sql_error)?;
    if integrity != "ok" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("save integrity check failed: {integrity}"),
        ));
    }
    transaction
        .prepare("SELECT id, username, password_token FROM account LIMIT 0")
        .map_err(sql_error)?;
    transaction
        .prepare("SELECT id, account_id, slot, name, race, sex, hair, hair_color, face, map_id FROM character LIMIT 0")
        .map_err(sql_error)?;
    Ok(())
}

fn character_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Character> {
    Ok(Character {
        id: row.get(0)?,
        account_id: row.get(1)?,
        slot: row.get(2)?,
        name: row.get(3)?,
        race: row.get(4)?,
        sex: row.get(5)?,
        hair: row.get(6)?,
        hair_color: row.get(7)?,
        face: row.get(8)?,
        map_id: row.get(9)?,
        x: row.get(10)?,
        y: row.get(11)?,
        direction: row.get(12)?,
    })
}

fn validate_character(input: &NewCharacter) -> Result<(), CreateError> {
    if input.slot > 3 {
        return Err(CreateError::InvalidSlot);
    }
    let name_len = input.name.encode_utf16().count();
    if name_len < 3 {
        return Err(CreateError::NameTooShort);
    }
    if name_len > 8 {
        return Err(CreateError::NameTooLong);
    }
    if input.name.chars().any(|character| {
        character.is_whitespace()
            || character.is_control()
            || ".,/#!$%^&*;:{}=-_~()".contains(character)
    }) {
        return Err(CreateError::BadName);
    }
    if input.race > 3 || input.sex > 1 {
        return Err(CreateError::InvalidAppearance);
    }
    Ok(())
}

fn create_storage(error: rusqlite::Error) -> CreateError {
    CreateError::Storage(sql_error(error))
}

fn sql_error(error: rusqlite::Error) -> io::Error {
    let kind = match &error {
        rusqlite::Error::SqliteFailure(code, _) => match code.code {
            rusqlite::ErrorCode::DatabaseCorrupt
            | rusqlite::ErrorCode::NotADatabase
            | rusqlite::ErrorCode::SchemaChanged => io::ErrorKind::InvalidData,
            _ => io::ErrorKind::Other,
        },
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, error)
}

fn quest_storage_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::Other,
        "could not persist quest lifecycle state",
    )
}

fn quest_state_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "saved quest lifecycle state is invalid",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    struct TempDb {
        dir: std::path::PathBuf,
        file: std::path::PathBuf,
    }
    impl TempDb {
        fn new() -> Self {
            loop {
                let n = NEXT.fetch_add(1, Ordering::Relaxed);
                let dir =
                    std::env::temp_dir().join(format!("eco-store-{}-{n}", std::process::id()));
                match fs::create_dir(&dir) {
                    Ok(()) => {
                        return Self {
                            file: dir.join("save.db"),
                            dir,
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("cannot create temporary database directory: {error}"),
                }
            }
        }
    }
    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn lifecycle_db(temp: &TempDb) -> std::path::PathBuf {
        let root = temp.dir.join("source");
        let quests = root.join("Quests");
        fs::create_dir_all(&quests).unwrap();
        fs::write(
            quests.join("QuestDB_1.xml"),
            "<QuestDB GroupID=\"1\"><Quest><ID>10000001</ID><Type>HUNT</Type><Time>60</Time><MapID1>10025000</MapID1><ObjectID1>10000000</ObjectID1><Count1>5</Count1><Party>False</Party><RequiredQuestPoints>1</RequiredQuestPoints><DungeonID>0</DungeonID><MinLV>255</MinLV><MaxLv>255</MaxLv><Job>NONE</Job><JobType>NOVICE</JobType><Race>NONE</Race><Gender>NONE</Gender></Quest></QuestDB>",
        )
        .unwrap();
        root
    }

    fn lifecycle_row(
        connection: &Connection,
        character_id: u32,
    ) -> Option<(i64, i64, i64, i64, Option<i64>, i64)> {
        connection
            .query_row(
                "SELECT points, fame, status, credited, deadline, last_elapsed
                 FROM quest_lifecycle WHERE character_id = ?1",
                [character_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()
            .unwrap()
    }

    fn character(slot: u8, name: &str) -> NewCharacter {
        NewCharacter {
            slot,
            name: name.into(),
            race: 0,
            sex: 1,
            hair: 1,
            hair_color: 51,
            face: 3,
        }
    }

    #[test]
    fn new_database_seeds_demo_and_character_survives_reopen() {
        let db = TempDb::new();
        let expected;
        {
            let mut store = Store::open(&db.file).unwrap();
            let account = store.account("TEST").unwrap().unwrap();
            assert_eq!(account.id, 1);
            assert_eq!(account.password_token, DEMO_PASSWORD_TOKEN);
            expected = store.create_character(1, character(0, "みゃお")).unwrap();
            assert_eq!(expected.map_id, START_MAP_ID);
        }
        let store = Store::open(&db.file).unwrap();
        assert_eq!(store.characters(1).unwrap(), vec![expected]);
    }

    #[test]
    fn v1_save_migrates_additively_and_preserves_existing_rows() {
        let db = TempDb::new();
        let connection = Connection::open(&db.file).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE account (
                    id INTEGER PRIMARY KEY,
                    username TEXT NOT NULL UNIQUE COLLATE NOCASE,
                    password_token TEXT NOT NULL
                );
                CREATE TABLE character (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    account_id INTEGER NOT NULL REFERENCES account(id) ON DELETE CASCADE,
                    slot INTEGER NOT NULL CHECK(slot BETWEEN 0 AND 3),
                    name TEXT NOT NULL UNIQUE COLLATE NOCASE,
                    race INTEGER NOT NULL CHECK(race BETWEEN 0 AND 3),
                    sex INTEGER NOT NULL CHECK(sex BETWEEN 0 AND 1),
                    hair INTEGER NOT NULL CHECK(hair BETWEEN 0 AND 65535),
                    hair_color INTEGER NOT NULL CHECK(hair_color BETWEEN 0 AND 255),
                    face INTEGER NOT NULL CHECK(face BETWEEN 0 AND 65535),
                    map_id INTEGER NOT NULL,
                    UNIQUE(account_id, slot)
                );
                INSERT INTO account (id, username, password_token)
                    VALUES (7, 'saved-user', 'saved-token');
                INSERT INTO character (id, account_id, slot, name, race, sex, hair, hair_color, face, map_id)
                    VALUES (19, 7, 2, 'saved-char', 1, 0, 8, 52, 4, 20013003);
                PRAGMA user_version = 1;",
            )
            .unwrap();
        drop(connection);

        let mut store = Store::open(&db.file).unwrap();
        assert_eq!(
            store
                .connection
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            10
        );
        let account = store.account("saved-user").unwrap().unwrap();
        assert_eq!(account.id, 7);
        assert_eq!(account.password_token, "saved-token");
        let character = store.characters(7).unwrap().pop().unwrap();
        assert_eq!(character.id, 19);
        assert_eq!(character.name, "saved-char");
        assert_eq!(character.map_id, START_MAP_ID);
        assert_eq!((character.x, character.y, character.direction), (0, 0, 0));

        store.update_position(7, 19, 61, 65, 3).unwrap();
        let persisted = Store::open(&db.file).unwrap().characters(7).unwrap();
        assert_eq!(
            (persisted[0].x, persisted[0].y, persisted[0].direction),
            (61, 65, 3)
        );
    }

    #[test]
    fn v2_save_migrates_without_changing_character_data_and_adds_no_quest_defaults() {
        let db = TempDb::new();
        let connection = Connection::open(&db.file).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE account (
                    id INTEGER PRIMARY KEY, username TEXT NOT NULL UNIQUE COLLATE NOCASE,
                    password_token TEXT NOT NULL
                );
                CREATE TABLE character (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    account_id INTEGER NOT NULL REFERENCES account(id) ON DELETE CASCADE,
                    slot INTEGER NOT NULL CHECK(slot BETWEEN 0 AND 3),
                    name TEXT NOT NULL UNIQUE COLLATE NOCASE,
                    race INTEGER NOT NULL CHECK(race BETWEEN 0 AND 3),
                    sex INTEGER NOT NULL CHECK(sex BETWEEN 0 AND 1),
                    hair INTEGER NOT NULL CHECK(hair BETWEEN 0 AND 65535),
                    hair_color INTEGER NOT NULL CHECK(hair_color BETWEEN 0 AND 255),
                    face INTEGER NOT NULL CHECK(face BETWEEN 0 AND 65535),
                    map_id INTEGER NOT NULL, x INTEGER NOT NULL DEFAULT 0 CHECK(x BETWEEN 0 AND 255),
                    y INTEGER NOT NULL DEFAULT 0 CHECK(y BETWEEN 0 AND 255),
                    direction INTEGER NOT NULL DEFAULT 0 CHECK(direction BETWEEN 0 AND 255),
                    UNIQUE(account_id, slot)
                );
                INSERT INTO account VALUES (7, 'saved-user', 'saved-token');
                INSERT INTO character VALUES (19, 7, 2, 'saved-char', 1, 0, 8, 52, 4, 20013003, 61, 65, 3);
                PRAGMA user_version=2;",
            )
            .unwrap();
        drop(connection);

        let source = lifecycle_db(&db);
        {
            let store = Store::open_existing(&db.file).unwrap();
            assert_eq!(
                store.account("saved-user").unwrap().unwrap().password_token,
                "saved-token"
            );
            let character = store.characters(7).unwrap().pop().unwrap();
            assert_eq!(
                (
                    character.id,
                    character.name.as_str(),
                    character.x,
                    character.y,
                    character.direction
                ),
                (19, "saved-char", 61, 65, 3)
            );
            assert_eq!(lifecycle_row(&store.connection, 19), None);
            assert_eq!(
                store
                    .connection
                    .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                10
            );
        }
        let mut store = Store::open_existing(&db.file).unwrap();
        let events = db.dir.join("accept.csv");
        fs::write(&events, "0,accept,0,0,true\n").unwrap();
        store
            .replay_saved_quest(7, 19, &source, &events, 2, 1)
            .unwrap();
        drop(store);
        let store = Store::open_existing(&db.file).unwrap();
        assert_eq!(
            store.account("saved-user").unwrap().unwrap().password_token,
            "saved-token"
        );
        let character = store.characters(7).unwrap().pop().unwrap();
        assert_eq!(
            (
                character.name.as_str(),
                character.x,
                character.y,
                character.direction
            ),
            ("saved-char", 61, 65, 3)
        );
        assert_eq!(
            lifecycle_row(&store.connection, 19),
            Some((1, 1, 1, 0, Some(3600), 0))
        );
    }

    #[test]
    fn malformed_v2_schema_rolls_back_migration_without_touching_rows_or_version() {
        let db = TempDb::new();
        let connection = Connection::open(&db.file).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE account (id INTEGER PRIMARY KEY, username TEXT NOT NULL UNIQUE, password_token TEXT NOT NULL);
                 CREATE TABLE character (id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL, slot INTEGER NOT NULL, name TEXT NOT NULL,
                    race INTEGER NOT NULL, sex INTEGER NOT NULL, hair INTEGER NOT NULL, hair_color INTEGER NOT NULL,
                    face INTEGER NOT NULL, map_id INTEGER NOT NULL);
                 INSERT INTO account VALUES (7, 'saved-user', 'saved-token');
                 INSERT INTO character VALUES (19, 7, 2, 'saved-char', 1, 0, 8, 52, 4, 20013003);
                 PRAGMA user_version=2;",
            )
            .unwrap();
        drop(connection);

        assert!(Store::open(&db.file).is_err());
        let connection = Connection::open(&db.file).unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            connection
                .query_row("SELECT name FROM character WHERE id=19", [], |row| row
                    .get::<_, String>(
                    0
                ))
                .unwrap(),
            "saved-char"
        );
        let quest_table: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='quest_lifecycle')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!quest_table);
    }

    #[test]
    fn imported_v3_quest_rows_are_validated_even_without_table_checks() {
        let db = TempDb::new();
        let character_id;
        {
            let mut store = Store::open(&db.file).unwrap();
            character_id = store.create_character(1, character(0, "miao")).unwrap().id;
        }
        let connection = Connection::open(&db.file).unwrap();
        connection
            .execute_batch(
                "PRAGMA foreign_keys=OFF;
                 DROP TABLE IF EXISTS character_reward_item; DROP TABLE IF EXISTS character_reward_inventory; DROP TABLE character_reward_context; DROP TABLE character_gold; DROP TABLE character_novice_progress;
                 PRAGMA user_version=3;
                 DROP TABLE quest_lifecycle;
                 CREATE TABLE quest_lifecycle (
                    character_id INTEGER PRIMARY KEY REFERENCES character(id),
                    points, fame, status, credited, deadline, last_elapsed
                 );",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO quest_lifecycle VALUES (?1, 2, 1, 99, 0, NULL, 0)",
                [character_id],
            )
            .unwrap();
        drop(connection);

        let error = Store::open(&db.file).err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "saved quest lifecycle state is invalid");
        let connection = Connection::open(&db.file).unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            3
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT status FROM quest_lifecycle WHERE character_id=?1",
                    [character_id],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            99
        );
        let progress_table: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='character_novice_progress')",
            [], |row| row.get(0)
        ).unwrap();
        assert!(!progress_table);
    }

    #[test]
    fn v3_migration_preserves_rows_without_initializing_progress() {
        let db = TempDb::new();
        let character_id;
        {
            let mut store = Store::open(&db.file).unwrap();
            character_id = store.create_character(1, character(0, "miao")).unwrap().id;
            store.update_position(1, character_id, 61, 65, 3).unwrap();
            store
                .connection
                .execute(
                    "INSERT INTO quest_lifecycle VALUES (?1, 2, 4, 1, 1, 3600, 12)",
                    [character_id],
                )
                .unwrap();
            store
                .connection
                .execute_batch("DROP TABLE IF EXISTS character_reward_item; DROP TABLE IF EXISTS character_reward_inventory; DROP TABLE character_reward_context; DROP TABLE character_gold; DROP TABLE character_novice_progress; DROP TABLE character_starter_item; DROP TABLE character_novice_status; PRAGMA user_version=3;")
                .unwrap();
        }
        let store = Store::open_existing(&db.file).unwrap();
        assert_eq!(
            store
                .connection
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            10
        );
        assert_eq!(store.account("test").unwrap().unwrap().id, 1);
        let saved = &store.characters(1).unwrap()[0];
        assert_eq!(
            (
                saved.id,
                saved.name.as_str(),
                saved.x,
                saved.y,
                saved.direction
            ),
            (character_id, "miao", 61, 65, 3)
        );
        assert_eq!(
            lifecycle_row(&store.connection, character_id),
            Some((2, 4, 1, 1, Some(3600), 12))
        );
        let count: i64 = store
            .connection
            .query_row("SELECT COUNT(*) FROM character_novice_progress", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn v4_migration_preserves_progress_adds_empty_stats_and_reopens() {
        let db = TempDb::new();
        let id;
        {
            let mut store = Store::open(&db.file).unwrap();
            id = store.create_character(1, character(0, "miao")).unwrap().id;
            store.initialize_novice_progress(1, id).unwrap();
            store.apply_novice_exp(1, id, 17, 23, false).unwrap();
            store.connection.execute_batch("PRAGMA foreign_keys=OFF; DROP TABLE IF EXISTS character_reward_item; DROP TABLE IF EXISTS character_reward_inventory; DROP TABLE character_reward_context; DROP TABLE character_gold; DROP TABLE character_starter_item; DROP TABLE character_novice_status; ALTER TABLE character_novice_progress RENAME TO progress_v5; CREATE TABLE character_novice_progress (character_id INTEGER PRIMARY KEY REFERENCES character(id) ON DELETE CASCADE,base_level INTEGER NOT NULL,job_level INTEGER NOT NULL,base_exp INTEGER NOT NULL,job_exp INTEGER NOT NULL,stats_points INTEGER NOT NULL,skill_points INTEGER NOT NULL); INSERT INTO character_novice_progress SELECT character_id,base_level,job_level,base_exp,job_exp,stats_points,skill_points FROM progress_v5; DROP TABLE progress_v5; PRAGMA user_version=4;").unwrap();
        }
        {
            let mut store = Store::open_existing(&db.file).unwrap();
            let saved: (i64,i64,Option<i64>,Option<i64>) = store.connection.query_row("SELECT base_exp,job_exp,base_str,base_dex FROM character_novice_progress WHERE character_id=?1", [id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
            assert_eq!(saved, (17, 23, None, None));
            store
                .initialize_novice_base_stats(1, id, NOVICE_BASE_STATS)
                .unwrap();
        }
        let reopened = Store::open_existing(&db.file).unwrap();
        assert_eq!(reopened.connection.query_row("SELECT base_str,base_exp,job_exp FROM character_novice_progress WHERE character_id=?1", [id], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?))).unwrap(), (8,17,23));
        assert_eq!(reopened.characters(1).unwrap()[0].id, id);
    }

    #[test]
    fn malformed_v4_progress_rolls_back_stats_migration() {
        let db = TempDb::new();
        let id;
        {
            let mut store = Store::open(&db.file).unwrap();
            id = store.create_character(1, character(0, "miao")).unwrap().id;
            store.initialize_novice_progress(1, id).unwrap();
            store.connection.execute_batch("PRAGMA foreign_keys=OFF; DROP TABLE IF EXISTS character_reward_item; DROP TABLE IF EXISTS character_reward_inventory; DROP TABLE character_reward_context; DROP TABLE character_gold; DROP TABLE character_starter_item; UPDATE character_novice_progress SET skill_points=9; ALTER TABLE character_novice_progress RENAME TO progress_v5; CREATE TABLE character_novice_progress (character_id INTEGER PRIMARY KEY REFERENCES character(id) ON DELETE CASCADE,base_level INTEGER NOT NULL,job_level INTEGER NOT NULL,base_exp INTEGER NOT NULL,job_exp INTEGER NOT NULL,stats_points INTEGER NOT NULL,skill_points INTEGER NOT NULL); INSERT INTO character_novice_progress SELECT character_id,base_level,job_level,base_exp,job_exp,stats_points,skill_points FROM progress_v5; DROP TABLE progress_v5; PRAGMA user_version=4;").unwrap();
        }
        assert_eq!(
            Store::open_existing(&db.file).err().unwrap().kind(),
            io::ErrorKind::InvalidData
        );
        let connection = Connection::open(&db.file).unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            4
        );
        let stats_added: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('character_novice_progress') WHERE name='base_str')", [], |r| r.get(0)).unwrap();
        assert!(!stats_added);
        assert_eq!(
            connection
                .query_row(
                    "SELECT skill_points FROM character_novice_progress WHERE character_id=?1",
                    [id],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            9
        );
    }

    #[test]
    fn malformed_persisted_novice_stats_are_refused_without_changing_save_bytes() {
        for mutation in [
            "UPDATE character_novice_progress SET base_str=NULL WHERE character_id=?1",
            "UPDATE character_novice_progress SET base_str=6,base_dex=3,base_int=6,base_vit=4,base_agi=2,base_mag=10 WHERE character_id=?1",
            "UPDATE character_novice_progress SET base_str='bad' WHERE character_id=?1",
            "UPDATE character_novice_progress SET base_str=8.5 WHERE character_id=?1",
            "UPDATE character_novice_progress SET base_str=4294967296 WHERE character_id=?1",
        ] {
            let db = TempDb::new();
            let id;
            {
                let mut store = Store::open(&db.file).unwrap();
                let mut input = character(0, "miao");
                input.sex = 0;
                id = store.create_character(1, input).unwrap().id;
                store.initialize_novice_progress(1, id).unwrap();
                store.initialize_novice_base_stats(1, id, NOVICE_BASE_STATS).unwrap();
                store.connection.execute(mutation, [id]).unwrap();
            }
            let before = fs::read(&db.file).unwrap();
            let error = Store::open_existing(&db.file).err().unwrap();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{mutation}");
            assert_eq!(fs::read(&db.file).unwrap(), before, "{mutation}");
        }
    }

    #[test]
    fn novice_progress_initializes_once_and_requires_owned_supported_character() {
        let db = TempDb::new();
        let mut store = Store::open(&db.file).unwrap();
        let novice = store.create_character(1, character(0, "miao")).unwrap();
        let mut demon_input = character(1, "demon");
        demon_input.race = 3;
        let demon = store.create_character(1, demon_input).unwrap();
        assert_eq!(
            store
                .connection
                .query_row("SELECT COUNT(*) FROM character_novice_progress", [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );

        assert_eq!(
            store
                .initialize_novice_progress(2, novice.id)
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(
            store
                .initialize_novice_progress(1, demon.id)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        store.initialize_novice_progress(1, novice.id).unwrap();
        store.initialize_novice_progress(1, novice.id).unwrap();
        drop(store);
        let mut store = Store::open_existing(&db.file).unwrap();
        store.initialize_novice_progress(1, novice.id).unwrap();
        let baseline: (i64,i64,i64,i64,i64,i64) = store.connection.query_row(
            "SELECT base_level,job_level,base_exp,job_exp,stats_points,skill_points FROM character_novice_progress WHERE character_id=?1",
            [novice.id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))
        ).unwrap();
        assert_eq!(baseline, (1, 1, 0, 0, 2, 3));
        store
            .connection
            .execute(
                "UPDATE character_novice_progress SET skill_points=9 WHERE character_id=?1",
                [novice.id],
            )
            .unwrap();
        assert_eq!(
            store
                .initialize_novice_progress(1, novice.id)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        let values: (i64,i64,i64,i64,i64,i64) = store.connection.query_row(
            "SELECT base_level,job_level,base_exp,job_exp,stats_points,skill_points FROM character_novice_progress WHERE character_id=?1",
            [novice.id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))
        ).unwrap();
        assert_eq!(values, (1, 1, 0, 0, 2, 9));
    }

    #[test]
    fn novice_base_stats_require_initialized_owned_row_and_preserve_existing_values() {
        let db = TempDb::new();
        let mut store = Store::open(&db.file).unwrap();
        let stats = [
            [8, 3, 3, 10, 4, 3],
            [6, 3, 6, 4, 2, 10],
            [10, 5, 1, 1, 5, 8],
        ];
        let novice = store.create_character(1, character(0, "miao")).unwrap();
        assert!(store
            .initialize_novice_base_stats(1, novice.id, stats)
            .is_err());
        store.initialize_novice_progress(1, novice.id).unwrap();
        store
            .initialize_novice_base_stats(1, novice.id, stats)
            .unwrap();
        store
            .initialize_novice_base_stats(1, novice.id, stats)
            .unwrap();
        assert!(store
            .initialize_novice_base_stats(1, novice.id, [[1; 6]; 3])
            .is_err());
        let saved: Vec<i64> = store.connection.prepare("SELECT base_str,base_dex,base_int,base_vit,base_agi,base_mag FROM character_novice_progress WHERE character_id=?1").unwrap().query_map([novice.id], |r| Ok((0..6).map(|i| r.get(i).unwrap()).collect())).unwrap().next().unwrap().unwrap();
        assert_eq!(saved, [8, 3, 3, 10, 4, 3]);
        assert!(store
            .initialize_novice_base_stats(2, novice.id, stats)
            .is_err());
        let mut titan = character(1, "tita");
        titan.race = 1;
        let titan = store.create_character(1, titan).unwrap();
        store.initialize_novice_progress(1, titan.id).unwrap();
        store
            .initialize_novice_base_stats(1, titan.id, stats)
            .unwrap();
        let got: Vec<i64> = store.connection.query_row("SELECT base_str,base_dex,base_int,base_vit,base_agi,base_mag FROM character_novice_progress WHERE character_id=?1", [titan.id], |r| Ok((0..6).map(|i| r.get(i).unwrap()).collect())).unwrap();
        assert_eq!(got, [6, 3, 6, 4, 2, 10]);
        let mut dominion = character(2, "domi");
        dominion.race = 2;
        let dominion = store.create_character(1, dominion).unwrap();
        store.initialize_novice_progress(1, dominion.id).unwrap();
        store
            .initialize_novice_base_stats(1, dominion.id, stats)
            .unwrap();
        let got: Vec<i64> = store.connection.query_row("SELECT base_str,base_dex,base_int,base_vit,base_agi,base_mag FROM character_novice_progress WHERE character_id=?1", [dominion.id], |r| Ok((0..6).map(|i| r.get(i).unwrap()).collect())).unwrap();
        assert_eq!(got, [10, 5, 1, 1, 5, 8]);
    }

    #[test]
    fn novice_base_stats_failed_update_rolls_back() {
        let mut store = Store::in_memory().unwrap();
        let novice = store.create_character(1, character(0, "miao")).unwrap();
        store.initialize_novice_progress(1, novice.id).unwrap();
        store.connection.execute_batch("CREATE TRIGGER fail_base_stats BEFORE UPDATE OF base_str ON character_novice_progress BEGIN SELECT RAISE(ABORT,'fail'); END;").unwrap();
        assert!(store
            .initialize_novice_base_stats(
                1,
                novice.id,
                [
                    [8, 3, 3, 10, 4, 3],
                    [6, 3, 6, 4, 2, 10],
                    [10, 5, 1, 1, 5, 8]
                ]
            )
            .is_err());
        let saved: i64 = store.connection.query_row("SELECT COUNT(*) FROM character_novice_progress WHERE character_id=?1 AND base_str IS NULL", [novice.id], |r| r.get(0)).unwrap();
        assert_eq!(saved, 1);
    }

    #[test]
    fn novice_exp_accumulates_within_bound_and_preserves_other_saved_state() {
        let db = TempDb::new();
        let character_id;
        {
            let mut store = Store::open(&db.file).unwrap();
            character_id = store.create_character(1, character(0, "miao")).unwrap().id;
            let other_id = store.create_character(1, character(1, "mimi")).unwrap().id;
            let account_before = store.account("test").unwrap().unwrap();
            let other_before = store
                .characters(1)
                .unwrap()
                .into_iter()
                .find(|c| c.id == other_id)
                .unwrap();
            store.initialize_novice_progress(1, character_id).unwrap();
            store
                .connection
                .execute(
                    "INSERT INTO quest_lifecycle VALUES (?1, 4, 7, 1, 2, 3600, 0)",
                    [character_id],
                )
                .unwrap();
            store.update_position(1, character_id, 9, 8, 7).unwrap();
            assert_eq!(
                store
                    .apply_novice_exp(1, character_id, 0, 0, false)
                    .unwrap(),
                [0, 0]
            );
            assert_eq!(
                store
                    .apply_novice_exp(1, character_id, 20, 15, false)
                    .unwrap(),
                [20, 15]
            );
            store
                .connection
                .execute_batch(
                    "CREATE TRIGGER fail_novice_update BEFORE UPDATE ON character_novice_progress
                 BEGIN SELECT RAISE(ABORT, 'forced failure'); END;",
                )
                .unwrap();
            assert!(store
                .apply_novice_exp(1, character_id, 1, 1, false)
                .is_err());
            let values: (i64, i64) = store
                .connection
                .query_row(
                    "SELECT base_exp,job_exp FROM character_novice_progress WHERE character_id=?1",
                    [character_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(values, (20, 15));
            assert_eq!(
                store
                    .connection
                    .query_row(
                        "SELECT x,y,direction FROM character WHERE id=?1",
                        [character_id],
                        |r| Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, i64>(1)?,
                            r.get::<_, i64>(2)?
                        )),
                    )
                    .unwrap(),
                (9, 8, 7)
            );
            assert_eq!(
                lifecycle_row(&store.connection, character_id),
                Some((4, 7, 1, 2, Some(3600), 0))
            );
            store
                .connection
                .execute_batch("DROP TRIGGER fail_novice_update;")
                .unwrap();
            for (base, job) in [(1, 41), (36, 1), (u32::MAX, 0), (0, u32::MAX)] {
                assert_eq!(
                    store
                        .apply_novice_exp(1, character_id, base, job, false)
                        .unwrap_err()
                        .kind(),
                    io::ErrorKind::InvalidInput
                );
                let values: (i64, i64) = store.connection.query_row(
                    "SELECT base_exp,job_exp FROM character_novice_progress WHERE character_id=?1",
                    [character_id], |r| Ok((r.get(0)?, r.get(1)?)),
                ).unwrap();
                assert_eq!(values, (20, 15));
            }
            assert_eq!(
                store
                    .apply_novice_exp(2, character_id, 1, 1, false)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::NotFound
            );
            assert_eq!(
                store
                    .apply_novice_exp(1, other_id, 1, 1, false)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidData
            );
            assert_eq!(
                store
                    .apply_novice_exp(1, character_id, u32::MAX, u32::MAX, false)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
            assert_eq!(
                store
                    .apply_novice_exp(1, character_id, 35, 40, false)
                    .unwrap(),
                [55, 55]
            );
            assert_eq!(store.account("test").unwrap().unwrap(), account_before);
            assert_eq!(
                store
                    .characters(1)
                    .unwrap()
                    .into_iter()
                    .find(|c| c.id == other_id)
                    .unwrap(),
                other_before
            );
        }
        let mut store = Store::open_existing(&db.file).unwrap();
        assert_eq!(
            store
                .apply_novice_exp(1, character_id, 0, 0, false)
                .unwrap(),
            [55, 55]
        );
        store.initialize_novice_progress(1, character_id).unwrap();
        assert_eq!(store.characters(1).unwrap().len(), 2);
        let values: (i64, i64, i64, i64, i64, i64) = store.connection.query_row(
            "SELECT base_level,job_level,base_exp,job_exp,stats_points,skill_points FROM character_novice_progress WHERE character_id=?1",
            [character_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
        ).unwrap();
        assert_eq!(values, (1, 1, 55, 55, 2, 3));
        assert_eq!(
            lifecycle_row(&store.connection, character_id),
            Some((4, 7, 1, 2, Some(3600), 0))
        );
    }

    #[test]
    fn novice_level_two_requires_explicit_context_and_commits_atomically() {
        let db = TempDb::new();
        let id;
        {
            let mut store = Store::open(&db.file).unwrap();
            id = store.create_character(1, character(0, "miao")).unwrap().id;
            store.initialize_novice_progress(1, id).unwrap();
            assert_eq!(
                store
                    .apply_novice_exp(1, id, 56, 0, false)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
            assert_eq!(
                store
                    .apply_novice_exp(1, id, 55, 0, true)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidData
            );
            store
                .initialize_novice_base_stats(1, id, NOVICE_BASE_STATS)
                .unwrap();
            store
                .initialize_starter_inventory(1, id, STARTER_ITEMS)
                .unwrap();
            assert_eq!(store.apply_novice_exp(1, id, 55, 0, true).unwrap(), [55, 0]);
            let state:(i64,i64,i64,i64)=store.connection.query_row("SELECT base_level,stats_points,hp,max_hp FROM character_novice_progress p JOIN character_novice_status s USING(character_id) WHERE p.character_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
            assert_eq!(state, (1, 2, 86, 86));
            assert_eq!(store.apply_novice_exp(1, id, 1, 0, false).unwrap(), [56, 0]);
            let promoted:(i64,i64,i64,i64,i64,i64)=store.connection.query_row("SELECT base_level,stats_points,hp,max_hp,mp,max_sp FROM character_novice_progress p JOIN character_novice_status s USING(character_id) WHERE p.character_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).unwrap();
            assert_eq!(promoted, (2, 5, 88, 88, 41, 35));
            store
                .connection
                .execute(
                    "UPDATE character_novice_status SET hp=42 WHERE character_id=?1",
                    [id],
                )
                .unwrap();
            assert_eq!(store.apply_novice_exp(1, id, 0, 1, false).unwrap(), [56, 1]);
            assert_eq!(
                store
                    .connection
                    .query_row(
                        "SELECT hp FROM character_novice_status WHERE character_id=?1",
                        [id],
                        |r| r.get::<_, i64>(0)
                    )
                    .unwrap(),
                42
            );
            assert_eq!(
                store
                    .apply_novice_exp(1, id, 100, 0, false)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
            assert_eq!(
                store
                    .connection
                    .query_row(
                        "SELECT base_exp FROM character_novice_progress WHERE character_id=?1",
                        [id],
                        |r| r.get::<_, i64>(0)
                    )
                    .unwrap(),
                56
            );
            assert_eq!(
                store.apply_novice_exp(1, id, 0, 55, false).unwrap(),
                [56, 56]
            );
            let levels:(i64,i64,i64,i64,i64)=store.connection.query_row("SELECT base_level,job_level,skill_points,hp,max_hp FROM character_novice_progress p JOIN character_novice_status s USING(character_id) WHERE p.character_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).unwrap();
            assert_eq!(levels, (2, 2, 4, 88, 88));
        }
        let reopened = Store::open_existing(&db.file).unwrap();
        assert_eq!(reopened.connection.query_row("SELECT base_level,job_level,base_exp,job_exp,stats_points,skill_points FROM character_novice_progress WHERE character_id=?1",[id],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?,r.get::<_,i64>(4)?,r.get::<_,i64>(5)?))).unwrap(),(2,2,56,56,5,4));

        let mut rollback = Store::in_memory().unwrap();
        let failed = rollback
            .create_character(1, character(1, "mimi"))
            .unwrap()
            .id;
        rollback.initialize_novice_progress(1, failed).unwrap();
        rollback
            .initialize_novice_base_stats(1, failed, NOVICE_BASE_STATS)
            .unwrap();
        rollback
            .initialize_starter_inventory(1, failed, STARTER_ITEMS)
            .unwrap();
        rollback.connection.execute_batch("CREATE TRIGGER fail_level_update BEFORE UPDATE OF base_exp ON character_novice_progress BEGIN SELECT RAISE(ABORT,'failure'); END;").unwrap();
        assert!(rollback.apply_novice_exp(1, failed, 56, 0, true).is_err());
        assert_eq!(
            rollback
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM character_novice_status WHERE character_id=?1",
                    [failed],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(rollback.connection.query_row("SELECT base_exp,base_level FROM character_novice_progress WHERE character_id=?1",[failed],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?))).unwrap(),(0,1));
    }

    #[test]
    fn novice_progress_rejects_malformed_import_and_rolls_back_failed_insert() {
        let db = TempDb::new();
        let character_id;
        {
            let mut store = Store::open(&db.file).unwrap();
            character_id = store.create_character(1, character(0, "miao")).unwrap().id;
            store
                .connection
                .execute_batch(
                    "CREATE TRIGGER fail_novice_insert BEFORE INSERT ON character_novice_progress
                 BEGIN SELECT RAISE(ABORT, 'forced failure'); END;",
                )
                .unwrap();
            assert_eq!(
                store
                    .initialize_novice_progress(1, character_id)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::Other
            );
            let count: i64 = store
                .connection
                .query_row("SELECT COUNT(*) FROM character_novice_progress", [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(count, 0);
            store
                .connection
                .execute_batch("DROP TRIGGER fail_novice_insert;")
                .unwrap();
            store.initialize_novice_progress(1, character_id).unwrap();
        }
        let connection = Connection::open(&db.file).unwrap();
        connection.execute_batch(
            "PRAGMA foreign_keys=OFF; DROP TABLE character_novice_progress;
             CREATE TABLE character_novice_progress (character_id, base_level, job_level, base_exp, job_exp, stats_points, skill_points);
             INSERT INTO character_novice_progress VALUES (999, 1, 1, 0, 0, 2, 3);",
        ).unwrap();
        drop(connection);
        let error = Store::open_existing(&db.file).err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn starter_inventory_is_explicit_bounded_and_preserved_across_reopen() {
        let db = TempDb::new();
        let mut ids = Vec::new();
        {
            let mut store = Store::open(&db.file).unwrap();
            for race in 0..3 {
                for sex in 0..2 {
                    let account_id = 10 + race as u32 * 2 + sex as u32;
                    store
                        .connection
                        .execute(
                            "INSERT INTO account(id,username,password_token) VALUES(?1,?2,'token')",
                            (account_id, format!("account{race}{sex}")),
                        )
                        .unwrap();
                    let mut input = character(0, &format!("s{race}{sex}"));
                    input.race = race;
                    input.sex = sex;
                    let id = store.create_character(account_id, input).unwrap().id;
                    store.initialize_novice_progress(account_id, id).unwrap();
                    ids.push((id, race as usize, sex as usize));
                    assert!(store
                        .initialize_starter_inventory(account_id, id, STARTER_ITEMS)
                        .is_ok());
                    store
                        .initialize_starter_inventory(account_id, id, STARTER_ITEMS)
                        .unwrap();
                    assert_eq!(
                        starter_inventory_for_character(
                            &store.connection.transaction().unwrap(),
                            id
                        )
                        .unwrap(),
                        expected_starter_rows(race as usize, sex as usize)
                    );
                }
            }
            let count: i64 = store
                .connection
                .query_row("SELECT COUNT(*) FROM character_starter_item", [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(count, 30);
        }
        let mut store = Store::open_existing(&db.file).unwrap();
        assert_eq!(
            store
                .connection
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            10
        );
        let expected = [
            vec![
                (50000000, 5),
                (50010300, 5),
                (50060150, 10),
                (10020114, 0),
                (60010082, 10),
            ],
            vec![
                (50000055, 5),
                (50010300, 5),
                (50060100, 10),
                (10020114, 0),
                (60010082, 10),
            ],
            vec![
                (50000180, 5),
                (50010456, 5),
                (50060150, 10),
                (10020114, 0),
                (60010082, 10),
            ],
            vec![
                (50001381, 5),
                (50012360, 10),
                (50060100, 10),
                (10020114, 0),
                (60010082, 10),
            ],
            vec![
                (50002754, 5),
                (50010253, 5),
                (50060150, 10),
                (10020114, 0),
                (60010082, 10),
            ],
            vec![
                (50001454, 5),
                (50010155, 5),
                (50060100, 10),
                (10020114, 0),
                (60010082, 10),
            ],
        ];
        for (index, (id, race, sex)) in ids.into_iter().enumerate() {
            assert_eq!(
                starter_inventory_for_character(&store.connection.transaction().unwrap(), id)
                    .unwrap(),
                expected[index]
            );
            assert_eq!(expected_starter_rows(race, sex), expected[index]);
        }
    }

    #[test]
    fn starter_inventory_requires_owned_character_and_progress_and_rolls_back_partial_insert() {
        let mut store = Store::in_memory().unwrap();
        let id = store.create_character(1, character(0, "miao")).unwrap().id;
        assert_eq!(
            store
                .initialize_starter_inventory(1, id, STARTER_ITEMS)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        store.initialize_novice_progress(1, id).unwrap();
        assert_eq!(
            store
                .initialize_starter_inventory(2, id, STARTER_ITEMS)
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        store.connection.execute_batch("CREATE TRIGGER fail_starter BEFORE INSERT ON character_starter_item WHEN NEW.instance_slot=3 BEGIN SELECT RAISE(ABORT,'failure'); END;").unwrap();
        assert_eq!(
            store
                .initialize_starter_inventory(1, id, STARTER_ITEMS)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other
        );
        let count: i64 = store
            .connection
            .query_row("SELECT COUNT(*) FROM character_starter_item", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
        store
            .connection
            .execute_batch("DROP TRIGGER fail_starter")
            .unwrap();
        store
            .initialize_starter_inventory(1, id, STARTER_ITEMS)
            .unwrap();
    }

    #[test]
    fn malformed_imported_starter_inventory_is_refused_without_file_mutation() {
        let cases = [
            ("partial set", "DELETE FROM character_starter_item WHERE instance_slot=5;"),
            ("duplicate slot", "UPDATE character_starter_item SET instance_slot=1 WHERE instance_slot=2;"),
            ("minimum slot", "UPDATE character_starter_item SET instance_slot=-9223372036854775808 WHERE instance_slot=1;"),
            ("text character id", "UPDATE character_starter_item SET character_id='x' WHERE instance_slot=1;"),
            ("text slot", "UPDATE character_starter_item SET instance_slot='1' WHERE instance_slot=1;"),
            ("text container", "UPDATE character_starter_item SET container='11' WHERE instance_slot=1;"),
            ("text item", "UPDATE character_starter_item SET item_id='50000000' WHERE instance_slot=1;"),
            ("text count", "UPDATE character_starter_item SET count='1' WHERE instance_slot=1;"),
            ("text durability", "UPDATE character_starter_item SET durability='5' WHERE instance_slot=1;"),
            ("text identified", "UPDATE character_starter_item SET identified='1' WHERE instance_slot=1;"),
            ("wrong item", "UPDATE character_starter_item SET item_id=9 WHERE instance_slot=1;"),
            ("wrong count", "UPDATE character_starter_item SET count=2 WHERE instance_slot=1;"),
            ("wrong container", "UPDATE character_starter_item SET container=12 WHERE instance_slot=1;"),
            ("wrong durability", "UPDATE character_starter_item SET durability=6 WHERE instance_slot=1;"),
            ("unidentified", "UPDATE character_starter_item SET identified=0 WHERE instance_slot=1;"),
            ("missing progress", "DELETE FROM character_novice_progress;"),
            ("orphan character", "UPDATE character_starter_item SET character_id=999;"),
            ("DEM character", "PRAGMA ignore_check_constraints=ON; UPDATE character SET race=3;"),
            ("invalid sex", "PRAGMA ignore_check_constraints=ON; UPDATE character SET sex=2;"),
        ];
        for (label, mutation) in cases {
            let db = TempDb::new();
            let id;
            {
                let mut store = Store::open(&db.file).unwrap();
                let mut input = character(0, "miao");
                input.sex = 0;
                id = store.create_character(1, input).unwrap().id;
                store.initialize_novice_progress(1, id).unwrap();
                store.connection.execute_batch("DROP TABLE IF EXISTS character_reward_item; DROP TABLE IF EXISTS character_reward_inventory; DROP TABLE character_reward_context; DROP TABLE character_gold; DROP TABLE character_starter_item; DROP TABLE character_novice_status; CREATE TABLE character_starter_item(character_id,instance_slot,container,item_id,count,durability,identified); PRAGMA user_version=6;").unwrap();
            }
            let connection = Connection::open(&db.file).unwrap();
            connection.execute_batch(&format!("INSERT INTO character_starter_item VALUES ({id},1,11,50000000,1,5,1),({id},2,12,50010300,1,5,1),({id},3,16,50060150,1,10,1),({id},4,2,10020114,1,0,1),({id},5,2,60010082,1,10,1);")).unwrap();
            drop(connection);
            assert!(
                Store::open_existing(&db.file).is_ok(),
                "invalid baseline for {label}"
            );
            let connection = Connection::open(&db.file).unwrap();
            connection.execute_batch(mutation).unwrap();
            drop(connection);
            let before = fs::read(&db.file).unwrap();
            assert_eq!(
                Store::open_existing(&db.file).err().unwrap().kind(),
                io::ErrorKind::InvalidData,
                "accepted {label}"
            );
            assert_eq!(
                fs::read(&db.file).unwrap(),
                before,
                "mutated save for {label}"
            );
        }

        let db = TempDb::new();
        let id;
        {
            let mut store = Store::open(&db.file).unwrap();
            let mut input = character(0, "miao");
            input.sex = 0;
            id = store.create_character(1, input).unwrap().id;
            store.initialize_novice_progress(1, id).unwrap();
            store.connection.execute_batch("DROP TABLE IF EXISTS character_reward_item; DROP TABLE IF EXISTS character_reward_inventory; DROP TABLE character_reward_context; DROP TABLE character_gold; DROP TABLE character_starter_item; DROP TABLE character_novice_status; CREATE TABLE character_starter_item(character_id,instance_slot,container,item_id,count,durability,identified); PRAGMA user_version=6;").unwrap();
        }
        let connection = Connection::open(&db.file).unwrap();
        connection.execute_batch(&format!("INSERT INTO character_starter_item VALUES ({id},1,11,50000000,1,5,1),({id},2,12,50010300,1,5,1),({id},3,16,50060150,1,10,1),({id},4,2,10020114,1,0,1),({id},5,2,60010082,1,10,1);")).unwrap();
        drop(connection);
        let opened = Store::open_existing(&db.file);
        assert!(
            opened.is_ok(),
            "valid unconstrained imported inventory rejected: {:?}",
            opened.err()
        );
    }

    #[test]
    fn imported_novice_status_rejects_malformed_fields_without_touching_bytes() {
        fn valid_db() -> (TempDb, u32) {
            let db = TempDb::new();
            let mut store = Store::open(&db.file).unwrap();
            let id = store.create_character(1, character(0, "miao")).unwrap().id;
            store.initialize_novice_progress(1, id).unwrap();
            store
                .initialize_novice_base_stats(1, id, NOVICE_BASE_STATS)
                .unwrap();
            store
                .initialize_starter_inventory(1, id, STARTER_ITEMS)
                .unwrap();
            store.apply_novice_exp(1, id, 0, 0, true).unwrap();
            drop(store);
            (db, id)
        }
        let columns = [
            "character_id",
            "job",
            "has_joint",
            "form",
            "item_instance_bonuses",
            "iris_bonuses",
            "chip_bonuses",
            "skill_bonuses",
            "marionette_bonuses",
            "possession_bonuses",
            "riding_pet_bonuses",
            "map_id",
            "map_flags",
            "hp",
            "max_hp",
            "mp",
            "max_mp",
            "sp",
            "max_sp",
        ];
        let mut cases = Vec::new();
        for field in columns {
            cases.push((
                format!("real {field}"),
                format!("UPDATE character_novice_status SET {field}=1.5"),
            ));
            cases.push((
                format!("text {field}"),
                format!("UPDATE character_novice_status SET {field}='1'"),
            ));
            cases.push((
                format!("null {field}"),
                format!("UPDATE character_novice_status SET {field}=NULL"),
            ));
        }
        for field in [
            "job",
            "has_joint",
            "form",
            "item_instance_bonuses",
            "iris_bonuses",
            "chip_bonuses",
            "skill_bonuses",
            "marionette_bonuses",
            "possession_bonuses",
            "riding_pet_bonuses",
            "map_id",
            "map_flags",
        ] {
            cases.push((
                format!("nonzero {field}"),
                format!("UPDATE character_novice_status SET {field}=1"),
            ));
        }
        cases.extend([
            (
                "max below current".to_string(),
                "UPDATE character_novice_status SET max_hp=1".to_string(),
            ),
            (
                "negative current".to_string(),
                "UPDATE character_novice_status SET hp=-1".to_string(),
            ),
            (
                "mp above max".to_string(),
                "UPDATE character_novice_status SET mp=max_mp+1".to_string(),
            ),
            (
                "sp above max".to_string(),
                "UPDATE character_novice_status SET sp=max_sp+1".to_string(),
            ),
        ]);
        for (label, mutation) in cases {
            let (db, id) = valid_db();
            let conn = Connection::open(&db.file).unwrap();
            conn.execute_batch("DROP TABLE character_novice_status; CREATE TABLE character_novice_status(character_id,job,has_joint,form,item_instance_bonuses,iris_bonuses,chip_bonuses,skill_bonuses,marionette_bonuses,possession_bonuses,riding_pet_bonuses,map_id,map_flags,hp,max_hp,mp,max_mp,sp,max_sp);").unwrap();
            conn.execute("INSERT INTO character_novice_status VALUES(?1,0,0,0,0,0,0,0,0,0,0,20013003,0,86,86,40,40,34,34)",[id]).unwrap();
            drop(conn);
            assert!(
                Store::open_existing(&db.file).is_ok(),
                "valid rebuilt context rejected before {label}"
            );
            let conn = Connection::open(&db.file).unwrap();
            conn.execute_batch(&mutation).unwrap();
            drop(conn);
            let before = fs::read(&db.file).unwrap();
            assert_eq!(
                Store::open_existing(&db.file).err().unwrap().kind(),
                io::ErrorKind::InvalidData,
                "accepted {label}"
            );
            assert_eq!(
                fs::read(&db.file).unwrap(),
                before,
                "changed bytes after {label}"
            );
        }
        for (label,setup) in [
            ("duplicate rows","INSERT INTO character_novice_status SELECT * FROM character_novice_status"),
            ("orphan row","PRAGMA foreign_keys=OFF; INSERT INTO character_novice_status VALUES(999,0,0,0,0,0,0,0,0,0,0,20013003,0,86,86,40,40,34,34)"),
        ] {
            let (db,id)=valid_db(); let conn=Connection::open(&db.file).unwrap();
            conn.execute_batch("DROP TABLE character_novice_status; CREATE TABLE character_novice_status(character_id,job,has_joint,form,item_instance_bonuses,iris_bonuses,chip_bonuses,skill_bonuses,marionette_bonuses,possession_bonuses,riding_pet_bonuses,map_id,map_flags,hp,max_hp,mp,max_mp,sp,max_sp);").unwrap();
            conn.execute("INSERT INTO character_novice_status VALUES(?1,0,0,0,0,0,0,0,0,0,0,20013003,0,86,86,40,40,34,34)",[id]).unwrap();
            assert!(Store::open_existing(&db.file).is_ok(),"valid context rejected before {label}");
            conn.execute_batch(setup).unwrap(); drop(conn); let before=fs::read(&db.file).unwrap();
            assert_eq!(Store::open_existing(&db.file).err().unwrap().kind(),io::ErrorKind::InvalidData,"accepted {label}");
            assert_eq!(fs::read(&db.file).unwrap(),before,"changed bytes after {label}");
        }
    }

    #[test]
    fn novice_status_requires_matching_prerequisites_and_vitals() {
        for (name,mutation) in [
            ("missing progress", "DELETE FROM character_novice_progress WHERE character_id=1"),
            ("missing stats", "UPDATE character_novice_progress SET base_str=NULL,base_dex=NULL,base_int=NULL,base_vit=NULL,base_agi=NULL,base_mag=NULL WHERE character_id=1"),
            ("missing starter", "DELETE FROM character_starter_item WHERE character_id=1 AND instance_slot=3"),
            ("wrong map", "UPDATE character SET map_id=20013002 WHERE id=1"),
        ] {
            let db=TempDb::new(); let mut store=Store::open(&db.file).unwrap();
            let id=store.create_character(1,character(0,"miao")).unwrap().id;
            store.initialize_novice_progress(1,id).unwrap(); store.initialize_novice_base_stats(1,id,NOVICE_BASE_STATS).unwrap(); store.initialize_starter_inventory(1,id,STARTER_ITEMS).unwrap(); store.apply_novice_exp(1,id,0,0,true).unwrap(); drop(store);
            let conn=Connection::open(&db.file).unwrap();
            let mutation=mutation.replace("=1",&format!("={id}")); conn.execute_batch(&mutation).unwrap(); drop(conn);
            let before=fs::read(&db.file).unwrap();
            assert_eq!(Store::open_existing(&db.file).err().unwrap().kind(),io::ErrorKind::InvalidData,"accepted {name}");
            assert_eq!(fs::read(&db.file).unwrap(),before);
        }
        let db = TempDb::new();
        let mut store = Store::open(&db.file).unwrap();
        let id = store.create_character(1, character(0, "mimi")).unwrap().id;
        store.initialize_novice_progress(1, id).unwrap();
        store.connection.execute("UPDATE character_novice_progress SET base_level=2,base_exp=56,stats_points=5 WHERE character_id=?1",[id]).unwrap();
        drop(store);
        let before = fs::read(&db.file).unwrap();
        assert_eq!(
            Store::open_existing(&db.file).err().unwrap().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(fs::read(&db.file).unwrap(), before);
    }

    #[test]
    fn v6_migration_preserves_exp_without_inventing_context_and_rejects_level_two() {
        let db = TempDb::new();
        let id;
        {
            let mut store = Store::open(&db.file).unwrap();
            id = store.create_character(1, character(0, "miao")).unwrap().id;
            store.initialize_novice_progress(1, id).unwrap();
            store.apply_novice_exp(1, id, 17, 23, false).unwrap();
            store
                .connection
                .execute_batch("DROP TABLE IF EXISTS character_reward_item; DROP TABLE IF EXISTS character_reward_inventory; DROP TABLE character_reward_context; DROP TABLE character_gold; DROP TABLE character_novice_status; PRAGMA user_version=6;")
                .unwrap();
        }
        let store = Store::open_existing(&db.file).unwrap();
        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT base_exp,job_exp FROM character_novice_progress WHERE character_id=?1",
                    [id],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
                )
                .unwrap(),
            (17, 23)
        );
        assert_eq!(
            store
                .connection
                .query_row("SELECT COUNT(*) FROM character_novice_status", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );

        let bad = TempDb::new();
        let id;
        {
            let mut store = Store::open(&bad.file).unwrap();
            id = store.create_character(1, character(0, "mimi")).unwrap().id;
            store.initialize_novice_progress(1, id).unwrap();
            store.connection.execute("UPDATE character_novice_progress SET base_level=2,base_exp=56,stats_points=5 WHERE character_id=?1",[id]).unwrap();
            store
                .connection
                .execute_batch("DROP TABLE IF EXISTS character_reward_item; DROP TABLE IF EXISTS character_reward_inventory; DROP TABLE character_reward_context; DROP TABLE character_gold; DROP TABLE character_novice_status; PRAGMA user_version=6;")
                .unwrap();
        }
        let before = fs::read(&bad.file).unwrap();
        assert_eq!(
            Store::open_existing(&bad.file).err().unwrap().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(fs::read(&bad.file).unwrap(), before);
    }

    #[test]
    fn v5_migration_preserves_saved_state_and_adds_no_inventory() {
        let db = TempDb::new();
        let id;
        {
            let mut store = Store::open(&db.file).unwrap();
            id = store.create_character(1, character(0, "miao")).unwrap().id;
            store.initialize_novice_progress(1, id).unwrap();
            store.apply_novice_exp(1, id, 17, 23, false).unwrap();
            store
                .initialize_novice_base_stats(1, id, NOVICE_BASE_STATS)
                .unwrap();
            store
                .connection
                .execute(
                    "INSERT INTO quest_lifecycle VALUES(?1,2,4,1,1,3600,12)",
                    [id],
                )
                .unwrap();
            store
                .connection
                .execute_batch("DROP TABLE IF EXISTS character_reward_item; DROP TABLE IF EXISTS character_reward_inventory; DROP TABLE character_reward_context; DROP TABLE character_gold; DROP TABLE character_starter_item; DROP TABLE character_novice_status; PRAGMA user_version=5;")
                .unwrap();
        }
        let store = Store::open_existing(&db.file).unwrap();
        assert_eq!(
            store
                .connection
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            10
        );
        assert_eq!(store.connection.query_row("SELECT base_exp,job_exp,base_str FROM character_novice_progress WHERE character_id=?1", [id], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?))).unwrap(), (17,23,8));
        assert_eq!(
            lifecycle_row(&store.connection, id),
            Some((2, 4, 1, 1, Some(3600), 12))
        );
        assert_eq!(
            store
                .connection
                .query_row("SELECT COUNT(*) FROM character_starter_item", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn imported_novice_progress_rejects_bad_fields_duplicates_and_dem_rows() {
        for values in [
            "'1',1,0,0,2,3",
            "256,1,0,0,2,3",
            "1,'1',0,0,2,3",
            "1,256,0,0,2,3",
            "1,1,'0',0,2,3",
            "1,1,56,0,2,3",
            "1,1,4294967296,0,2,3",
            "1,1,0,'0',2,3",
            "1,1,0,56,2,3",
            "1,1,0,4294967296,2,3",
            "1,1,0,0,'2',3",
            "1,1,0,0,65536,3",
            "1,1,0,0,2,'3'",
            "1,1,0,0,2,65536",
        ] {
            let db = TempDb::new();
            let mut store = Store::open(&db.file).unwrap();
            let id = store.create_character(1, character(0, "miao")).unwrap().id;
            store.connection.execute_batch(
                "DROP TABLE character_novice_progress;
                 CREATE TABLE character_novice_progress (character_id, base_level, job_level, base_exp, job_exp, stats_points, skill_points);"
            ).unwrap();
            store
                .connection
                .execute_batch(&format!(
                    "INSERT INTO character_novice_progress VALUES ({id}, {values});"
                ))
                .unwrap();
            drop(store);
            assert_eq!(
                Store::open_existing(&db.file).err().unwrap().kind(),
                io::ErrorKind::InvalidData,
                "accepted {values}"
            );
        }

        for duplicate in [
            "INSERT INTO character_novice_progress VALUES (1,1,1,0,0,2,3); INSERT INTO character_novice_progress VALUES (1,1,1,0,0,2,3);",
        ] {
            let db = TempDb::new();
            let mut store = Store::open(&db.file).unwrap();
            store.create_character(1, character(0, "miao")).unwrap();
            store.connection.execute_batch(
                "DROP TABLE character_novice_progress;
                 CREATE TABLE character_novice_progress (character_id, base_level, job_level, base_exp, job_exp, stats_points, skill_points);"
            ).unwrap();
            store.connection.execute_batch(duplicate).unwrap();
            drop(store);
            assert_eq!(Store::open_existing(&db.file).err().unwrap().kind(), io::ErrorKind::InvalidData);
        }

        let db = TempDb::new();
        let mut store = Store::open(&db.file).unwrap();
        let mut dem = character(0, "demchar");
        dem.race = 3;
        let dem = store.create_character(1, dem).unwrap();
        store
            .connection
            .execute(
                "INSERT INTO character_novice_progress (character_id,base_level,job_level,base_exp,job_exp,stats_points,skill_points) VALUES (?1,1,1,0,0,2,3)",
                [dem.id],
            )
            .unwrap();
        drop(store);
        assert_eq!(
            Store::open_existing(&db.file).err().unwrap().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn saved_quest_reopens_and_resumes_from_persisted_state() {
        let db = TempDb::new();
        let source = lifecycle_db(&db);
        let character_id;
        {
            let mut store = Store::open(&db.file).unwrap();
            character_id = store.create_character(1, character(0, "miao")).unwrap().id;
            store.initialize_gold(1, character_id, 77).unwrap();
            store.initialize_novice_progress(1, character_id).unwrap();
            store
                .initialize_novice_base_stats(1, character_id, NOVICE_BASE_STATS)
                .unwrap();
            store
                .initialize_starter_inventory(1, character_id, STARTER_ITEMS)
                .unwrap();
            store.apply_novice_exp(1, character_id, 0, 0, true).unwrap();
            store
                .initialize_reward_context(1, character_id, 17, 23, true)
                .unwrap();
            let events = db.dir.join("first.csv");
            fs::write(&events, "0,accept,0,0,true\n1,kill,10000000,10025000,false\n2,kill,10000000,10025000,false\n").unwrap();
            let report = store
                .replay_saved_quest(1, character_id, &source, &events, 2, 1)
                .unwrap();
            assert!(report.starts_with(
                "Headless saved lifecycle replay; no live gameplay, dialogue or rewards."
            ));
            assert!(report
                .contains("quest_points=1\nfame=1\ncredited_count=2\nprogress=2/5\nstatus=OPEN"));
        }
        {
            let mut store = Store::open(&db.file).unwrap();
            let events = db.dir.join("second.csv");
            fs::write(&events, "3,kill,10000000,10025000,false\n4,kill,10000000,10025000,false\n5,kill,10000000,10025000,false\n").unwrap();
            let report = store
                .replay_saved_quest(1, character_id, &source, &events, 99, 88)
                .unwrap();
            assert!(report.contains(
                "quest_points=1\nfame=1\ncredited_count=5\nprogress=5/5\nstatus=COMPLETED"
            ));
            let cleanup = db.dir.join("completed-cleanup.csv");
            fs::write(&cleanup, "6,cleanup,0,0,false\n7,cleanup,0,0,false\n").unwrap();
            let report = store
                .replay_saved_quest(1, character_id, &source, &cleanup, 99, 99)
                .unwrap();
            assert!(report.contains("rejected_actions=2"));
        }
        let store = Store::open_existing(&db.file).unwrap();
        assert_eq!(
            lifecycle_row(&store.connection, character_id),
            Some((1, 1, 2, 5, Some(3600), 7))
        );
        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT gold FROM character_gold WHERE character_id=?1",
                    [character_id],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            77
        );
        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT ep,cp,ringless FROM character_reward_context WHERE character_id=?1",
                    [character_id],
                    |r| Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?
                    ))
                )
                .unwrap(),
            (17, 23, 1)
        );
    }

    #[test]
    fn existing_only_open_does_not_create_missing_paths_or_initialize_empty_files() {
        let db = TempDb::new();
        assert!(Store::open_existing(&db.file).is_err());
        assert!(!db.file.exists());
        let missing_parent_save = db.dir.join("missing-parent").join("save.db");
        assert!(Store::open_existing(&missing_parent_save).is_err());
        assert!(!missing_parent_save.parent().unwrap().exists());

        fs::write(&db.file, []).unwrap();
        assert_eq!(
            Store::open_existing(&db.file).err().unwrap().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(fs::metadata(&db.file).unwrap().len(), 0);

        fs::write(&db.file, b"not a sqlite database").unwrap();
        assert_eq!(
            Store::open_existing(&db.file).err().unwrap().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(fs::read(&db.file).unwrap(), b"not a sqlite database");
    }

    #[test]
    fn saved_failed_cleanup_rolls_back_malformed_batch_and_reopens_after_restart() {
        let db = TempDb::new();
        let source = lifecycle_db(&db);
        let character_id;
        {
            let mut store = Store::open(&db.file).unwrap();
            character_id = store.create_character(1, character(0, "miao")).unwrap().id;
            let events = db.dir.join("failed.csv");
            fs::write(
                &events,
                "0,accept,0,0,true\n3600,tick,0,0,false\n3601,kill,10000000,10025000,false\n",
            )
            .unwrap();
            store
                .replay_saved_quest(1, character_id, &source, &events, 2, 1)
                .unwrap();
        }
        {
            let mut store = Store::open(&db.file).unwrap();
            assert_eq!(
                lifecycle_row(&store.connection, character_id),
                Some((1, 1, 3, 1, Some(3600), 3601))
            );

            let malformed = db.dir.join("malformed-cleanup.csv");
            fs::write(
                &malformed,
                "3602,cleanup,0,0,false\n3603,unknown,0,0,false\n",
            )
            .unwrap();
            let error = store
                .replay_saved_quest(1, character_id, &source, &malformed, 99, 99)
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!(
                lifecycle_row(&store.connection, character_id),
                Some((1, 1, 3, 1, Some(3600), 3601))
            );

            store
                .connection
                .execute_batch(
                    "CREATE TRIGGER reject_cleanup_update BEFORE UPDATE ON quest_lifecycle
                     BEGIN SELECT RAISE(ABORT, 'private cleanup trigger text'); END;",
                )
                .unwrap();
            let cleanup = db.dir.join("cleanup.csv");
            fs::write(&cleanup, "3604,cleanup,0,0,false\n").unwrap();
            let error = store
                .replay_saved_quest(1, character_id, &source, &cleanup, 0, 0)
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Other);
            assert!(!error.to_string().contains("private cleanup trigger text"));
            assert_eq!(
                lifecycle_row(&store.connection, character_id),
                Some((1, 1, 3, 1, Some(3600), 3601))
            );
            store
                .connection
                .execute_batch("DROP TRIGGER reject_cleanup_update;")
                .unwrap();
            store
                .replay_saved_quest(1, character_id, &source, &cleanup, 0, 0)
                .unwrap();
        }
        {
            let mut store = Store::open(&db.file).unwrap();
            assert_eq!(
                lifecycle_row(&store.connection, character_id),
                Some((1, 0, 0, 0, None, 3604))
            );
            let malformed = db.dir.join("malformed-reaccept.csv");
            fs::write(&malformed, "3605,accept,0,0,true\n3606,unknown,0,0,false\n").unwrap();
            assert_eq!(
                store
                    .replay_saved_quest(1, character_id, &source, &malformed, 99, 99)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidData
            );
            assert_eq!(
                lifecycle_row(&store.connection, character_id),
                Some((1, 0, 0, 0, None, 3604))
            );
        }
        {
            let mut store = Store::open(&db.file).unwrap();
            let events = db.dir.join("reaccept.csv");
            fs::write(&events, "3605,accept,0,0,true\n").unwrap();
            store
                .replay_saved_quest(1, character_id, &source, &events, 99, 99)
                .unwrap();
        }
        let store = Store::open(&db.file).unwrap();
        assert_eq!(
            lifecycle_row(&store.connection, character_id),
            Some((0, 0, 1, 0, Some(7205), 3605))
        );
    }

    #[test]
    fn saved_quest_batch_errors_and_write_failures_preserve_previous_state() {
        let db = TempDb::new();
        let source = lifecycle_db(&db);
        let mut store = Store::open(&db.file).unwrap();
        let character_id = store.create_character(1, character(0, "miao")).unwrap().id;
        let valid = db.dir.join("valid.csv");
        fs::write(&valid, "0,accept,0,0,true\n").unwrap();
        store
            .replay_saved_quest(1, character_id, &source, &valid, 2, 1)
            .unwrap();
        let before = lifecycle_row(&store.connection, character_id);

        let malformed = db.dir.join("malformed.csv");
        fs::write(
            &malformed,
            "1,kill,10000000,10025000,false\n2,unknown,0,0,false\n",
        )
        .unwrap();
        let error = store
            .replay_saved_quest(1, character_id, &source, &malformed, 99, 99)
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(!error.to_string().contains("unknown"));
        assert_eq!(lifecycle_row(&store.connection, character_id), before);

        store
            .connection
            .execute_batch(
                "CREATE TRIGGER reject_quest_update BEFORE UPDATE ON quest_lifecycle
             BEGIN SELECT RAISE(ABORT, 'private trigger text'); END;",
            )
            .unwrap();
        let next = db.dir.join("next.csv");
        fs::write(&next, "1,kill,10000000,10025000,false\n").unwrap();
        let error = store
            .replay_saved_quest(1, character_id, &source, &next, 0, 0)
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert!(!error.to_string().contains("private trigger text"));
        assert_eq!(lifecycle_row(&store.connection, character_id), before);
    }

    #[test]
    fn saved_quest_requires_character_ownership_and_does_not_initialize_on_refusal() {
        let db = TempDb::new();
        let source = lifecycle_db(&db);
        let mut store = Store::open(&db.file).unwrap();
        let character_id = store.create_character(1, character(0, "miao")).unwrap().id;
        let events = db.dir.join("events.csv");
        fs::write(&events, "0,accept,0,0,true\n").unwrap();
        let error = store
            .replay_saved_quest(2, character_id, &source, &events, 2, 1)
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(lifecycle_row(&store.connection, character_id), None);
    }

    #[test]
    fn carried_gold_is_explicit_bounded_owned_and_idempotent() {
        let db = TempDb::new();
        let (id, other) = {
            let mut store = Store::open(&db.file).unwrap();
            let id = store.create_character(1, character(0, "miao")).unwrap().id;
            let other = store.create_character(1, character(1, "mimi")).unwrap().id;
            let third = store.create_character(1, character(2, "momo")).unwrap().id;
            store.initialize_gold(1, id, 0).unwrap();
            store.initialize_gold(1, other, 99_999_999).unwrap();
            store.initialize_gold(1, id, 0).unwrap();
            assert_eq!(
                store.initialize_gold(1, id, 1).unwrap_err().kind(),
                io::ErrorKind::AlreadyExists
            );
            assert_eq!(
                store.initialize_gold(2, id, 4).unwrap_err().kind(),
                io::ErrorKind::NotFound
            );
            assert_eq!(
                store.initialize_gold(1, u32::MAX, 4).unwrap_err().kind(),
                io::ErrorKind::NotFound
            );
            assert_eq!(
                store.initialize_gold(0, id, 4).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
            assert_eq!(
                store.initialize_gold(1, 0, 4).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
            assert_eq!(
                store
                    .initialize_gold(1, other, 100_000_000)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
            assert_eq!(
                store
                    .connection
                    .query_row("SELECT COUNT(*) FROM character_gold", [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                2
            );
            (id, third)
        };
        let mut store = Store::open_existing(&db.file).unwrap();
        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT gold FROM character_gold WHERE character_id=?1",
                    [id],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert!(store.initialize_gold(1, id, 0).is_ok());
        assert_eq!(
            store.initialize_gold(1, id, 1).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        store.connection.execute_batch("CREATE TRIGGER reject_gold BEFORE INSERT ON character_gold BEGIN SELECT RAISE(ABORT,'private'); END;").unwrap();
        assert_eq!(
            store.initialize_gold(1, other, 23).unwrap_err().kind(),
            io::ErrorKind::Other
        );
        assert_eq!(
            store
                .connection
                .query_row("SELECT COUNT(*) FROM character_gold", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
        store.connection.execute_batch("DROP TRIGGER reject_gold; CREATE TRIGGER ignore_gold BEFORE INSERT ON character_gold BEGIN SELECT RAISE(IGNORE); END;").unwrap();
        assert_eq!(
            store.initialize_gold(1, other, 23).unwrap_err().kind(),
            io::ErrorKind::Other
        );
        assert_eq!(
            store
                .connection
                .query_row("SELECT COUNT(*) FROM character_gold", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
    }

    fn reward_ready_store() -> (TempDb, Store, u32) {
        let db = TempDb::new();
        let mut store = Store::open(&db.file).unwrap();
        let id = store.create_character(1, character(0, "miao")).unwrap().id;
        store.initialize_novice_progress(1, id).unwrap();
        store
            .initialize_novice_base_stats(1, id, NOVICE_BASE_STATS)
            .unwrap();
        store
            .initialize_starter_inventory(1, id, STARTER_ITEMS)
            .unwrap();
        store.apply_novice_exp(1, id, 0, 0, true).unwrap();
        (db, store, id)
    }

    fn completed_reward_store(gold: u32, ep: u32, fame: u32) -> (TempDb, Store, u32) {
        let (db, mut store, id) = reward_ready_store();
        store
            .initialize_reward_context(1, id, ep, 23, true)
            .unwrap();
        store.initialize_gold(1, id, gold).unwrap();
        store.initialize_reward_inventory(1, id, true).unwrap();
        store
            .connection
            .execute(
                "INSERT INTO quest_lifecycle VALUES(?1,7,?2,2,5,3600,12)",
                (id, fame),
            )
            .unwrap();
        (db, store, id)
    }

    fn full_table_projection(connection: &Connection) -> Vec<String> {
        use rusqlite::types::Value;
        let mut names = connection.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap();
        let names: Vec<String> = names
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        names
            .into_iter()
            .map(|name| {
                let mut stmt = connection
                    .prepare(&format!("SELECT * FROM \"{name}\" ORDER BY rowid"))
                    .unwrap();
                let count = stmt.column_count();
                let rows = stmt
                    .query_map([], |row| {
                        (0..count)
                            .map(|i| row.get::<_, Value>(i).map(|v| format!("{v:?}")))
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
                format!("{name}:{rows:?}")
            })
            .collect()
    }

    #[test]
    fn completed_turn_in_is_atomic_bounded_repeatable_only_for_a_new_completion() {
        let (db, mut store, id) = completed_reward_store(777, 29, 1);
        store
            .connection
            .execute(
                "UPDATE character_novice_progress SET base_exp=40,job_exp=0 WHERE character_id=?1",
                [id],
            )
            .unwrap();
        store
            .connection
            .execute(
                "UPDATE character_novice_status SET hp=1,mp=1,sp=1 WHERE character_id=?1",
                [id],
            )
            .unwrap();
        store.turn_in_completed_quest(1, id, 11_000_435).unwrap();
        assert_eq!(store.connection.query_row("SELECT base_level,job_level,base_exp,job_exp FROM character_novice_progress WHERE character_id=?1",[id],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?))).unwrap(),(2,1,95,55));
        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT gold FROM character_gold WHERE character_id=?1",
                    [id],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            1_257
        );
        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT ep,cp FROM character_reward_context WHERE character_id=?1",
                    [id],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
                )
                .unwrap(),
            (30, 23)
        );
        assert_eq!(store.connection.query_row("SELECT hp=max_hp AND mp=max_mp AND sp=max_sp FROM character_novice_status WHERE character_id=?1",[id],|r|r.get::<_,bool>(0)).unwrap(),true);
        assert_eq!(
            lifecycle_row(&store.connection, id),
            Some((7, 2, 0, 0, None, 12))
        );
        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT instance_slot,item_id FROM character_reward_item WHERE character_id=?1",
                    [id],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
                )
                .unwrap(),
            (6, 60_060_000)
        );
        let after = full_table_projection(&store.connection);
        assert!(store.turn_in_completed_quest(1, id, 11_000_435).is_err());
        assert_eq!(full_table_projection(&store.connection), after);

        let source = lifecycle_db(&db);
        let events = db.dir.join("second-completion.csv");
        fs::write(&events,"12,accept,0,0,true\n13,kill,10000000,10025000,false\n14,kill,10000000,10025000,false\n15,kill,10000000,10025000,false\n16,kill,10000000,10025000,false\n17,kill,10000000,10025000,false\n").unwrap();
        store
            .replay_saved_quest(1, id, &source, &events, 7, 2)
            .unwrap();
        store.turn_in_completed_quest(1, id, 11_000_435).unwrap();
        assert_eq!(store.connection.query_row("SELECT group_concat(instance_slot,',') FROM character_reward_item WHERE character_id=?1",[id],|r|r.get::<_,String>(0)).unwrap(),"6,7");
        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT base_exp,job_exp FROM character_novice_progress WHERE character_id=?1",
                    [id],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
                )
                .unwrap(),
            (150, 110)
        );
        drop(store);
        let reopened = Store::open_existing(&db.file).unwrap();
        assert_eq!(
            reopened
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM character_reward_item WHERE character_id=?1",
                    [id],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            2
        );
        let (_db, mut cap, id) = completed_reward_store(99_999_519, 0, i32::MAX as u32 - 1);
        cap.turn_in_completed_quest(1, id, 11_000_435).unwrap();
        assert_eq!(
            cap.connection
                .query_row(
                    "SELECT gold FROM character_gold WHERE character_id=?1",
                    [id],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            99_999_999
        );
    }

    #[test]
    fn completed_turn_in_rejects_caps_and_missing_reward_context_unchanged() {
        for fame in [i32::MAX as u32, i32::MAX as u32 + 1, u32::MAX] {
            let (_db, mut s, id) = completed_reward_store(0, 0, fame);
            let before = full_table_projection(&s.connection);
            assert!(s.turn_in_completed_quest(1, id, 11_000_435).is_err());
            assert_eq!(full_table_projection(&s.connection), before);
        }
        let (_db, mut s, id) = completed_reward_store(99_999_520, 0, 0);
        let before = full_table_projection(&s.connection);
        assert!(s.turn_in_completed_quest(1, id, 11_000_435).is_err());
        assert_eq!(full_table_projection(&s.connection), before);
        for missing in [
            "gold",
            "context",
            "inventory",
            "status",
            "progress",
            "stats",
            "starter",
            "quest",
        ] {
            let (_db, mut s, id) = completed_reward_store(0, 0, 0);
            match missing {
                "gold" => {
                    s.connection
                        .execute("DELETE FROM character_gold WHERE character_id=?1", [id])
                        .unwrap();
                }
                "context" => {
                    s.connection
                        .execute(
                            "DELETE FROM character_reward_context WHERE character_id=?1",
                            [id],
                        )
                        .unwrap();
                }
                "inventory" => {
                    s.connection
                        .execute(
                            "DELETE FROM character_reward_inventory WHERE character_id=?1",
                            [id],
                        )
                        .unwrap();
                }
                "status" => {
                    s.connection
                        .execute(
                            "DELETE FROM character_novice_status WHERE character_id=?1",
                            [id],
                        )
                        .unwrap();
                }
                "progress" => {
                    s.connection
                        .execute(
                            "DELETE FROM character_novice_progress WHERE character_id=?1",
                            [id],
                        )
                        .unwrap();
                }
                "stats" => {
                    s.connection.execute("UPDATE character_novice_progress SET base_str=NULL WHERE character_id=?1", [id]).unwrap();
                }
                "starter" => {
                    s.connection.execute("DELETE FROM character_starter_item WHERE character_id=?1 AND instance_slot=1", [id]).unwrap();
                }
                _ => {
                    s.connection
                        .execute("DELETE FROM quest_lifecycle WHERE character_id=?1", [id])
                        .unwrap();
                }
            }
            let before = full_table_projection(&s.connection);
            assert!(
                s.turn_in_completed_quest(1, id, 11_000_435).is_err(),
                "accepted missing {missing}"
            );
            assert_eq!(
                full_table_projection(&s.connection),
                before,
                "missing {missing}"
            );
        }
    }

    #[test]
    fn completed_turn_in_handles_job_only_crossing_no_crossing_and_refusals() {
        let (_db, mut job_only, id) = completed_reward_store(100, 12, 3);
        job_only
            .connection
            .execute(
                "UPDATE character_novice_progress SET base_exp=0,job_exp=1 WHERE character_id=?1",
                [id],
            )
            .unwrap();
        job_only
            .connection
            .execute(
                "UPDATE character_novice_status SET hp=2,mp=2,sp=2 WHERE character_id=?1",
                [id],
            )
            .unwrap();
        job_only.turn_in_completed_quest(1, id, 11_000_435).unwrap();
        assert_eq!(job_only.connection.query_row("SELECT base_level,job_level,base_exp,job_exp FROM character_novice_progress WHERE character_id=?1",[id],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?))).unwrap(),(1,2,55,56));
        assert_eq!(
            job_only
                .connection
                .query_row(
                    "SELECT ep FROM character_reward_context WHERE character_id=?1",
                    [id],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            13
        );
        assert_eq!(job_only.connection.query_row("SELECT hp=max_hp AND mp=max_mp AND sp=max_sp FROM character_novice_status WHERE character_id=?1",[id],|r|r.get::<_,bool>(0)).unwrap(),true);

        let (_db, mut even, even_id) = completed_reward_store(100, 12, 3);
        even.connection
            .execute(
                "UPDATE character_novice_status SET hp=2,mp=2,sp=2 WHERE character_id=?1",
                [even_id],
            )
            .unwrap();
        even.turn_in_completed_quest(1, even_id, 11_000_435)
            .unwrap();
        assert_eq!(even.connection.query_row("SELECT base_level,job_level FROM character_novice_progress WHERE character_id=?1",[even_id],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?))).unwrap(),(1,1));
        assert_eq!(
            even.connection
                .query_row(
                    "SELECT hp,mp,sp FROM character_novice_status WHERE character_id=?1",
                    [even_id],
                    |r| Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?
                    ))
                )
                .unwrap(),
            (2, 2, 2)
        );

        let (_db, mut valid, id) = completed_reward_store(0, 0, 0);
        let before = full_table_projection(&valid.connection);
        assert!(valid.turn_in_completed_quest(2, id, 11_000_435).is_err());
        assert!(valid.turn_in_completed_quest(1, id, 11_000_434).is_err());
        assert_eq!(full_table_projection(&valid.connection), before);
        valid.turn_in_completed_quest(1, id, 11_000_435).unwrap();
        for (base, job) in [(101, 0), (0, 101), (101, 101)] {
            let (_db, mut exp_overflow, id) = completed_reward_store(0, 0, 0);
            exp_overflow
                .apply_novice_exp(1, id, base, job, false)
                .unwrap();
            let before = full_table_projection(&exp_overflow.connection);
            assert!(exp_overflow
                .turn_in_completed_quest(1, id, 11_000_435)
                .is_err());
            assert_eq!(full_table_projection(&exp_overflow.connection), before);
        }
    }

    #[test]
    fn completed_turn_in_rejects_duplicate_imported_quest_without_mutation() {
        let (db, store, id) = completed_reward_store(777, 7, 8);
        store.connection.execute_batch("PRAGMA foreign_keys=OFF; ALTER TABLE quest_lifecycle RENAME TO old_quest; CREATE TABLE quest_lifecycle AS SELECT * FROM old_quest; DROP TABLE old_quest;").unwrap();
        drop(store);
        let mut store = Store::open_existing(&db.file).unwrap();
        store
            .connection
            .execute_batch("INSERT INTO quest_lifecycle SELECT * FROM quest_lifecycle;")
            .unwrap();
        let before = full_table_projection(&store.connection);
        assert!(store.turn_in_completed_quest(1, id, 11_000_435).is_err());
        assert_eq!(full_table_projection(&store.connection), before);
        drop(store);
        let bytes = fs::read(&db.file).unwrap();
        assert!(Store::open_existing(&db.file).is_err());
        assert_eq!(fs::read(&db.file).unwrap(), bytes);
    }

    #[test]
    fn completed_turn_in_abort_and_ignore_at_each_write_roll_back_all_projections() {
        let stages = [
            ("character_novice_status", "UPDATE"),
            ("character_novice_progress", "UPDATE"),
            ("character_gold", "UPDATE"),
            ("character_reward_context", "UPDATE"),
            ("quest_lifecycle", "UPDATE OF fame"),
            ("character_reward_item", "INSERT"),
            ("character_reward_inventory", "UPDATE"),
            ("quest_lifecycle", "UPDATE OF status"),
        ];
        for (index, (table, operation)) in stages.into_iter().enumerate() {
            for ignore in [false, true] {
                let (_db, mut s, id) = completed_reward_store(400, 4, 7);
                s.connection.execute("UPDATE character_novice_progress SET base_exp=1,job_exp=1 WHERE character_id=?1",[id]).unwrap();
                let action = if ignore { "IGNORE" } else { "ABORT" };
                let raise = if ignore {
                    "RAISE(IGNORE)"
                } else {
                    "RAISE(ABORT,'private')"
                };
                let trigger=format!("CREATE TRIGGER reject_turn_in BEFORE {operation} ON {table} BEGIN UPDATE character_gold SET gold=0 WHERE character_id={id}; SELECT {raise}; END;");
                s.connection.execute_batch(&trigger).unwrap();
                let before = full_table_projection(&s.connection);
                assert!(
                    s.turn_in_completed_quest(1, id, 11_000_435).is_err(),
                    "stage {index} {action}"
                );
                assert_eq!(
                    full_table_projection(&s.connection),
                    before,
                    "stage {index} {action}"
                );
            }
        }
    }

    #[test]
    fn reward_context_requires_explicit_no_ring_and_valid_owned_novice_prerequisites() {
        let (_db, mut store, id) = reward_ready_store();
        assert_eq!(
            store
                .initialize_reward_context(1, id, 0, 0, false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            store
                .initialize_reward_context(0, id, 0, 0, true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            store
                .initialize_reward_context(1, 0, 0, 0, true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            store
                .initialize_reward_context(2, id, 0, 0, true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(
            store
                .initialize_reward_context(1, u32::MAX, 0, 0, true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(
            store
                .initialize_reward_context(1, id, 31, 0, true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            store
                .initialize_reward_context(1, id, 30, 100_000_000, true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        store.initialize_reward_context(1, id, 0, 0, true).unwrap();
        let row: (i64, i64, i64) = store
            .connection
            .query_row(
                "SELECT ep,cp,ringless FROM character_reward_context WHERE character_id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(row, (0, 0, 1));
        let (_max_db, mut max_store, max_id) = reward_ready_store();
        max_store
            .initialize_reward_context(1, max_id, NOVICE_MAX_EP, 99_999_999, true)
            .unwrap();
        let max_row: (i64, i64, i64) = max_store
            .connection
            .query_row(
                "SELECT ep,cp,ringless FROM character_reward_context WHERE character_id=?1",
                [max_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(max_row, (30, 99_999_999, 1));

        let mut missing = Store::in_memory().unwrap();
        let no_context_id = missing
            .create_character(1, character(0, "mimi"))
            .unwrap()
            .id;
        assert_eq!(
            missing
                .initialize_reward_context(1, no_context_id, 0, 0, true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        missing
            .initialize_novice_progress(1, no_context_id)
            .unwrap();
        missing
            .initialize_novice_base_stats(1, no_context_id, NOVICE_BASE_STATS)
            .unwrap();
        missing
            .initialize_starter_inventory(1, no_context_id, STARTER_ITEMS)
            .unwrap();
        assert_eq!(
            missing
                .initialize_reward_context(1, no_context_id, 0, 0, true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn reward_context_repeat_conflict_reopen_and_exp_levelup_preserve_ep() {
        let (db, mut store, id) = reward_ready_store();
        store
            .initialize_reward_context(1, id, 17, 23, true)
            .unwrap();
        store
            .initialize_reward_context(1, id, 17, 23, true)
            .unwrap();
        assert_eq!(
            store
                .initialize_reward_context(1, id, 18, 23, true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        store
            .connection
            .execute(
                "UPDATE character_novice_status SET hp=42 WHERE character_id=?1",
                [id],
            )
            .unwrap();
        store.apply_novice_exp(1, id, 56, 0, false).unwrap();
        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT ep FROM character_reward_context WHERE character_id=?1",
                    [id],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            17
        );
        drop(store);
        let mut reopened = Store::open_existing(&db.file).unwrap();
        reopened
            .initialize_reward_context(1, id, 17, 23, true)
            .unwrap();
        let preserved: (i64,i64,i64,i64) = reopened.connection.query_row("SELECT r.ep,r.cp,r.ringless,s.hp FROM character_reward_context r JOIN character_novice_status s USING(character_id) WHERE r.character_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
        assert_eq!(preserved, (17, 23, 1, 88));
    }

    #[test]
    fn reward_context_aborted_and_ignored_inserts_leave_context_absent() {
        let (_db, mut store, id) = reward_ready_store();
        for trigger in [
            "CREATE TRIGGER reject_reward BEFORE INSERT ON character_reward_context BEGIN SELECT RAISE(ABORT,'private'); END;",
            "DROP TRIGGER reject_reward; CREATE TRIGGER ignore_reward BEFORE INSERT ON character_reward_context BEGIN SELECT RAISE(IGNORE); END;",
        ] {
            store.connection.execute_batch(trigger).unwrap();
            assert_eq!(
                store.initialize_reward_context(1, id, 12, 34, true).unwrap_err().kind(),
                io::ErrorKind::Other
            );
            assert_eq!(
                store.connection.query_row("SELECT COUNT(*) FROM character_reward_context WHERE character_id=?1", [id], |r| r.get::<_,i64>(0)).unwrap(),
                0
            );
        }
    }

    #[test]
    fn reward_inventory_declaration_requires_full_context_and_preserves_imported_items() {
        let (db, mut store, id) = reward_ready_store();
        assert_eq!(
            store
                .initialize_reward_inventory(1, id, false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            store
                .initialize_reward_inventory(2, id, true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(
            store
                .initialize_reward_inventory(1, id, true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        store.initialize_reward_context(1, id, 3, 4, true).unwrap();
        assert_eq!(
            store
                .initialize_reward_inventory(1, id, true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        store.initialize_gold(1, id, 10).unwrap();
        store.initialize_reward_inventory(1, id, true).unwrap();
        store.connection.execute("INSERT INTO character_reward_item(character_id,instance_slot,container,item_id,count,durability,identified,pict_id,default_profile) VALUES(?1,6,2,60060000,1,0,1,0,1)",[id]).unwrap();
        store
            .connection
            .execute(
                "UPDATE character_reward_inventory SET next_slot=7 WHERE character_id=?1",
                [id],
            )
            .unwrap();
        store.initialize_reward_inventory(1, id, true).unwrap();
        let got:(i64,i64)=store.connection.query_row("SELECT next_slot,(SELECT COUNT(*) FROM character_reward_item WHERE character_id=?1) FROM character_reward_inventory WHERE character_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert_eq!(got, (7, 1));
        drop(store);
        let mut reopened = Store::open_existing(&db.file).unwrap();
        reopened.initialize_reward_inventory(1, id, true).unwrap();
        assert_eq!(reopened.connection.query_row("SELECT next_slot,(SELECT COUNT(*) FROM character_reward_item WHERE character_id=?1) FROM character_reward_inventory WHERE character_id=?1",[id],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?))).unwrap(),(7,1));
    }

    #[test]
    fn reward_inventory_insert_abort_and_ignore_leave_other_state_unchanged() {
        let (_db, mut store, id) = reward_ready_store();
        store.initialize_reward_context(1, id, 2, 3, true).unwrap();
        store.initialize_gold(1, id, 44).unwrap();
        store
            .connection
            .execute(
                "INSERT INTO quest_lifecycle VALUES(?1,7,8,2,5,3600,0)",
                [id],
            )
            .unwrap();
        store.connection.execute_batch("CREATE TRIGGER reject_inventory BEFORE INSERT ON character_reward_inventory BEGIN UPDATE character_gold SET gold=0; UPDATE quest_lifecycle SET points=0; SELECT RAISE(ABORT,'private'); END;").unwrap();
        assert_eq!(
            store
                .initialize_reward_inventory(1, id, true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other
        );
        let state:(i64,i64,i64)=store.connection.query_row("SELECT g.gold,q.points,(SELECT COUNT(*) FROM character_reward_inventory) FROM character_gold g JOIN quest_lifecycle q USING(character_id) WHERE g.character_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        assert_eq!(state, (44, 7, 0));
        store.connection.execute_batch("DROP TRIGGER reject_inventory; CREATE TRIGGER ignore_inventory BEFORE INSERT ON character_reward_inventory BEGIN UPDATE character_gold SET gold=0; UPDATE quest_lifecycle SET points=0; SELECT RAISE(IGNORE); END;").unwrap();
        assert_eq!(
            store
                .initialize_reward_inventory(1, id, true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other
        );
        let state:(i64,i64,i64)=store.connection.query_row("SELECT g.gold,q.points,(SELECT COUNT(*) FROM character_reward_inventory) FROM character_gold g JOIN quest_lifecycle q USING(character_id) WHERE g.character_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        assert_eq!(state, (44, 7, 0));
    }

    #[test]
    fn v9_migration_adds_empty_reward_inventory_and_rejects_malformed_reward_rows() {
        let (db, mut store, id) = reward_ready_store();
        store.initialize_reward_context(1, id, 1, 2, true).unwrap();
        store.initialize_gold(1, id, 3).unwrap();
        drop(store);
        let conn = Connection::open(&db.file).unwrap();
        conn.execute_batch("DROP TABLE character_reward_item; DROP TABLE character_reward_inventory; PRAGMA user_version=9;").unwrap();
        drop(conn);
        let store = Store::open_existing(&db.file).unwrap();
        assert_eq!(
            store
                .connection
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            10
        );
        assert_eq!(
            store
                .connection
                .query_row("SELECT COUNT(*) FROM character_reward_inventory", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(store);

        let mut mutations = Vec::new();
        for column in [
            "character_id",
            "instance_slot",
            "container",
            "item_id",
            "count",
            "durability",
            "identified",
            "pict_id",
            "default_profile",
        ] {
            for value in ["1.0", "'1'", "NULL"] {
                mutations.push((column, value));
            }
        }
        for (column, value) in mutations {
            let (bad, mut s, cid) = reward_ready_store();
            s.initialize_reward_context(1, cid, 1, 2, true).unwrap();
            s.initialize_gold(1, cid, 3).unwrap();
            s.initialize_reward_inventory(1, cid, true).unwrap();
            s.connection.execute_batch("PRAGMA foreign_keys=OFF; DROP TABLE character_reward_item; CREATE TABLE character_reward_item(character_id,instance_slot,container,item_id,count,durability,identified,pict_id,default_profile); INSERT INTO character_reward_item VALUES(1,6,2,60060000,1,0,1,0,1); UPDATE character_reward_inventory SET next_slot=7;").unwrap();
            drop(s);
            assert!(
                Store::open_existing(&bad.file).is_ok(),
                "valid unconstrained reward import rejected"
            );
            let conn = Connection::open(&bad.file).unwrap();
            conn.execute(
                &format!("UPDATE character_reward_item SET {column}={value}"),
                [],
            )
            .unwrap();
            drop(conn);
            let before = fs::read(&bad.file).unwrap();
            assert_eq!(
                Store::open_existing(&bad.file).err().unwrap().kind(),
                io::ErrorKind::InvalidData,
                "accepted {column}={value}"
            );
            assert_eq!(fs::read(&bad.file).unwrap(), before);
        }
        for mutation in [
            "UPDATE character_reward_item SET character_id=999",
            "UPDATE character_reward_inventory SET next_slot=8",
            "DELETE FROM character_reward_inventory",
            "INSERT INTO character_reward_item SELECT * FROM character_reward_item",
        ] {
            let (bad, mut s, cid) = reward_ready_store();
            s.initialize_reward_context(1, cid, 1, 2, true).unwrap();
            s.initialize_gold(1, cid, 3).unwrap();
            s.initialize_reward_inventory(1, cid, true).unwrap();
            s.connection.execute_batch("PRAGMA foreign_keys=OFF; DROP TABLE character_reward_item; CREATE TABLE character_reward_item(character_id,instance_slot,container,item_id,count,durability,identified,pict_id,default_profile); INSERT INTO character_reward_item VALUES(1,6,2,60060000,1,0,1,0,1); UPDATE character_reward_inventory SET next_slot=7;").unwrap();
            s.connection.execute_batch(mutation).unwrap();
            drop(s);
            let before = fs::read(&bad.file).unwrap();
            assert_eq!(
                Store::open_existing(&bad.file).err().unwrap().kind(),
                io::ErrorKind::InvalidData
            );
            assert_eq!(fs::read(&bad.file).unwrap(), before);
        }
    }

    #[test]
    fn inventory_context_imports_reject_types_gaps_and_missing_prerequisites() {
        let mut mutations = Vec::new();
        for (column, value) in [("character_id", 1), ("next_slot", 7), ("starter_only", 1)] {
            for raw in [format!("{value}.0"), format!("'{value}'"), "NULL".into()] {
                mutations.push(format!(
                    "UPDATE character_reward_inventory SET {column}={raw}"
                ));
            }
        }
        mutations.extend([
            "UPDATE character_reward_inventory SET character_id=0",
            "UPDATE character_reward_inventory SET character_id=4294967296",
            "UPDATE character_reward_inventory SET next_slot=5",
            "UPDATE character_reward_inventory SET next_slot=6",
            "UPDATE character_reward_inventory SET next_slot=8",
            "UPDATE character_reward_inventory SET next_slot=4294967296",
            "UPDATE character_reward_inventory SET starter_only=0",
            "INSERT INTO character_reward_inventory SELECT * FROM character_reward_inventory",
            "INSERT INTO character_reward_inventory VALUES(999,6,1)",
            "DELETE FROM character_gold",
            "DELETE FROM character_reward_context",
            "DELETE FROM character_novice_status",
            "UPDATE character_reward_item SET instance_slot=7; UPDATE character_reward_inventory SET next_slot=8",
            "UPDATE character_reward_item SET instance_slot=5",
            "UPDATE character_reward_item SET item_id=60060001",
            "UPDATE character_reward_item SET container=16",
            "UPDATE character_reward_item SET count=2",
            "UPDATE character_reward_item SET durability=1",
            "UPDATE character_reward_item SET identified=0",
            "UPDATE character_reward_item SET pict_id=1",
            "UPDATE character_reward_item SET default_profile=2",
        ].map(str::to_owned));
        for mutation in mutations {
            let (db, mut store, id) = reward_ready_store();
            store.initialize_gold(1, id, 777).unwrap();
            store.initialize_reward_context(1, id, 7, 12, true).unwrap();
            store.initialize_reward_inventory(1, id, true).unwrap();
            store.connection.execute_batch("PRAGMA foreign_keys=OFF;
                DROP TABLE character_reward_inventory;
                CREATE TABLE character_reward_inventory(character_id,next_slot,starter_only);
                INSERT INTO character_reward_inventory VALUES(1,7,1);
                DROP TABLE character_reward_item;
                CREATE TABLE character_reward_item(character_id,instance_slot,container,item_id,count,durability,identified,pict_id,default_profile);
                INSERT INTO character_reward_item VALUES(1,6,2,60060000,1,0,1,0,1);").unwrap();
            drop(store);
            assert!(
                Store::open_existing(&db.file).is_ok(),
                "valid unconstrained inventory rejected"
            );
            let conn = Connection::open(&db.file).unwrap();
            conn.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
            conn.execute_batch(&mutation).unwrap();
            drop(conn);
            let before = fs::read(&db.file).unwrap();
            assert!(
                Store::open_existing(&db.file).is_err(),
                "accepted {mutation}"
            );
            assert_eq!(
                fs::read(&db.file).unwrap(),
                before,
                "mutated refused inventory"
            );
        }
    }

    #[test]
    fn v8_migration_adds_empty_reward_context_and_import_validation_is_strict() {
        let (db, mut store, id) = reward_ready_store();
        store.initialize_novice_progress(1, id).unwrap();
        store
            .connection
            .execute_batch("DROP TABLE IF EXISTS character_reward_item; DROP TABLE IF EXISTS character_reward_inventory; DROP TABLE character_reward_context; PRAGMA user_version=8;")
            .unwrap();
        drop(store);
        let migrated = Store::open_existing(&db.file).unwrap();
        assert_eq!(
            migrated
                .connection
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            10
        );
        assert_eq!(
            migrated
                .connection
                .query_row("SELECT COUNT(*) FROM character_reward_context", [], |r| r
                    .get::<_, i64>(
                    0
                ))
                .unwrap(),
            0
        );
        drop(migrated);

        let mut cases = Vec::new();
        for col in ["character_id", "ep", "cp", "ringless"] {
            for raw in ["1.0", "'1'", "NULL"] {
                cases.push((col, raw));
            }
        }
        for (column, raw) in cases {
            let (bad, mut s, cid) = reward_ready_store();
            s.initialize_reward_context(1, cid, 1, 2, true).unwrap();
            s.connection.execute_batch("DROP TABLE character_reward_context; CREATE TABLE character_reward_context(character_id,ep,cp,ringless);").unwrap();
            s.connection
                .execute(
                    "INSERT INTO character_reward_context VALUES(?1,1,2,1)",
                    [cid],
                )
                .unwrap();
            drop(s);
            assert!(
                Store::open_existing(&bad.file).is_ok(),
                "valid unconstrained import rejected"
            );
            let conn = Connection::open(&bad.file).unwrap();
            conn.execute(
                &format!("UPDATE character_reward_context SET {column}={raw}"),
                [],
            )
            .unwrap();
            drop(conn);
            let before = fs::read(&bad.file).unwrap();
            assert_eq!(
                Store::open_existing(&bad.file).err().unwrap().kind(),
                io::ErrorKind::InvalidData,
                "accepted {column}={raw}"
            );
            assert_eq!(fs::read(&bad.file).unwrap(), before);
        }
        for (column, raw) in [
            ("ep", "-1"),
            ("ep", "31"),
            ("cp", "-1"),
            ("cp", "100000000"),
            ("character_id", "0"),
            ("character_id", "-1"),
            ("character_id", "4294967296"),
            ("ringless", "2"),
        ] {
            let (bad, mut s, cid) = reward_ready_store();
            s.initialize_reward_context(1, cid, 1, 2, true).unwrap();
            drop(s);
            let c = Connection::open(&bad.file).unwrap();
            c.execute_batch("PRAGMA foreign_keys=OFF; PRAGMA ignore_check_constraints=ON;")
                .unwrap();
            c.execute(
                &format!("UPDATE character_reward_context SET {column}={raw}"),
                [],
            )
            .unwrap();
            drop(c);
            let before = fs::read(&bad.file).unwrap();
            assert_eq!(
                Store::open_existing(&bad.file).err().unwrap().kind(),
                io::ErrorKind::InvalidData
            );
            assert_eq!(fs::read(&bad.file).unwrap(), before);
        }
        for mutation in [
            "INSERT INTO character_reward_context VALUES(999,1,2,1)",
            "UPDATE character_reward_context SET ringless=0",
            "INSERT INTO character_reward_context VALUES(1,1,2,1)",
        ] {
            let (bad, mut s, cid) = reward_ready_store();
            s.initialize_reward_context(1, cid, 1, 2, true).unwrap();
            s.connection.execute_batch("DROP TABLE character_reward_context; CREATE TABLE character_reward_context(character_id,ep,cp,ringless);").unwrap();
            s.connection
                .execute(
                    "INSERT INTO character_reward_context VALUES(?1,1,2,1)",
                    [cid],
                )
                .unwrap();
            s.connection.execute_batch(mutation).unwrap();
            drop(s);
            let before = fs::read(&bad.file).unwrap();
            assert_eq!(
                Store::open_existing(&bad.file).err().unwrap().kind(),
                io::ErrorKind::InvalidData
            );
            assert_eq!(fs::read(&bad.file).unwrap(), before);
        }
        let (bad, mut s, cid) = reward_ready_store();
        s.initialize_reward_context(1, cid, 1, 2, true).unwrap();
        s.connection
            .execute(
                "DELETE FROM character_novice_status WHERE character_id=?1",
                [cid],
            )
            .unwrap();
        drop(s);
        let before = fs::read(&bad.file).unwrap();
        assert_eq!(
            Store::open_existing(&bad.file).err().unwrap().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(fs::read(&bad.file).unwrap(), before);
    }

    #[test]
    fn v7_migration_adds_no_gold_and_malformed_gold_imports_are_refused_unchanged() {
        let db = TempDb::new();
        let id;
        {
            let mut store = Store::open(&db.file).unwrap();
            id = store.create_character(1, character(0, "miao")).unwrap().id;
            store.initialize_novice_progress(1, id).unwrap();
            store.apply_novice_exp(1, id, 17, 23, false).unwrap();
            store
                .connection
                .execute(
                    "INSERT INTO quest_lifecycle VALUES(?1,2,4,1,1,3600,12)",
                    [id],
                )
                .unwrap();
            store
                .connection
                .execute_batch("DROP TABLE IF EXISTS character_reward_item; DROP TABLE IF EXISTS character_reward_inventory; DROP TABLE character_reward_context; DROP TABLE character_gold; PRAGMA user_version=7;")
                .unwrap();
        }
        let store = Store::open_existing(&db.file).unwrap();
        assert_eq!(
            store
                .connection
                .query_row("SELECT COUNT(*) FROM character_gold", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT base_exp,job_exp FROM character_novice_progress WHERE character_id=?1",
                    [id],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
                )
                .unwrap(),
            (17, 23)
        );
        assert_eq!(
            lifecycle_row(&store.connection, id),
            Some((2, 4, 1, 1, Some(3600), 12))
        );

        for raw in [
            "1,1.5",
            "1,'2'",
            "1,NULL",
            "'1',2",
            "1.5,2",
            "NULL,2",
            "-1,2",
            "0,2",
            "1,-1",
            "1,100000000",
            "4294967296,2",
            "99,2",
        ] {
            let bad = TempDb::new();
            let mut store = Store::open(&bad.file).unwrap();
            let cid = store.create_character(1, character(0, "miao")).unwrap().id;
            assert_eq!(cid, 1);
            store
                .connection
                .execute_batch(
                    "DROP TABLE character_gold; CREATE TABLE character_gold(character_id,gold);",
                )
                .unwrap();
            store.initialize_gold(1, cid, 2).unwrap();
            drop(store);
            let store = Store::open_existing(&bad.file).unwrap();
            store
                .connection
                .execute_batch(&format!(
                    "DELETE FROM character_gold; INSERT INTO character_gold VALUES({raw});"
                ))
                .unwrap();
            drop(store);
            let before = fs::read(&bad.file).unwrap();
            assert_eq!(
                Store::open_existing(&bad.file).err().unwrap().kind(),
                io::ErrorKind::InvalidData,
                "accepted {raw}"
            );
            assert_eq!(fs::read(&bad.file).unwrap(), before);
        }
        let duplicate = TempDb::new();
        let mut store = Store::open(&duplicate.file).unwrap();
        let cid = store.create_character(1, character(0, "miao")).unwrap().id;
        store.connection.execute_batch(&format!("DROP TABLE character_gold; CREATE TABLE character_gold(character_id,gold); INSERT INTO character_gold VALUES({cid},2),({cid},3);" )).unwrap();
        drop(store);
        let before = fs::read(&duplicate.file).unwrap();
        assert_eq!(
            Store::open_existing(&duplicate.file).err().unwrap().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(fs::read(&duplicate.file).unwrap(), before);
    }

    #[test]
    fn position_update_requires_character_ownership() {
        let mut store = Store::in_memory().unwrap();
        let character = store.create_character(1, character(0, "miao")).unwrap();
        let error = store.update_position(2, character.id, 1, 2, 3).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(
            (
                store.characters(1).unwrap()[0].x,
                store.characters(1).unwrap()[0].y
            ),
            (0, 0)
        );
    }

    #[test]
    fn validation_and_conflicts_do_not_mutate_existing_characters() {
        let mut store = Store::in_memory().unwrap();
        assert!(matches!(
            store.create_character(1, character(4, "miao")),
            Err(CreateError::InvalidSlot)
        ));
        assert!(matches!(
            store.create_character(1, character(0, "abc def")),
            Err(CreateError::BadName)
        ));
        assert!(matches!(
            store.create_character(2, character(0, "miao")),
            Err(CreateError::AccountNotFound)
        ));
        store.create_character(1, character(0, "miao")).unwrap();
        assert!(matches!(
            store.create_character(1, character(0, "other")),
            Err(CreateError::SlotOccupied)
        ));
        assert!(matches!(
            store.create_character(1, character(1, "MIAO")),
            Err(CreateError::NameConflict)
        ));
        assert_eq!(store.characters(1).unwrap().len(), 1);
    }

    #[test]
    fn corrupt_and_unknown_saves_are_refused_without_reset() {
        let corrupt = TempDb::new();
        fs::write(&corrupt.file, b"not a sqlite database").unwrap();
        assert_eq!(
            Store::open(&corrupt.file).err().unwrap().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(fs::read(&corrupt.file).unwrap(), b"not a sqlite database");

        let unknown = TempDb::new();
        let connection = Connection::open(&unknown.file).unwrap();
        connection
            .execute_batch("CREATE TABLE keep_me (value TEXT); PRAGMA user_version = 99;")
            .unwrap();
        drop(connection);
        assert!(Store::open(&unknown.file).is_err());
        let connection = Connection::open(&unknown.file).unwrap();
        let exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='keep_me')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(exists);
    }

    #[test]
    fn validates_name_length_in_utf16_units_and_appearance() {
        let mut store = Store::in_memory().unwrap();
        assert!(matches!(
            store.create_character(1, character(0, "ab")),
            Err(CreateError::NameTooShort)
        ));
        assert!(matches!(
            store.create_character(1, character(0, "123456789")),
            Err(CreateError::NameTooLong)
        ));
        let mut invalid = character(0, "valid");
        invalid.race = 4;
        assert!(matches!(
            store.create_character(1, invalid),
            Err(CreateError::InvalidAppearance)
        ));
        store.create_character(1, character(0, "😀😀")).unwrap(); // four UTF-16 units
    }
}
