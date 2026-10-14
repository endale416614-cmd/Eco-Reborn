//! Player stat formulas ported from SagaECO `SagaMap/PC/StatusFactory.cs`
//! (`CalcStatus` and the pieces it calls: `CalcStatsRev`, `CalcPayV`,
//! `CalcHPMPSP`, `CalcStats`), plus the `SSMG_PLAYER_STATS_PRE_CALC` (0x0259) body.
//!
//! The result is the bare character: job level bonuses and base stats only.
//! Equipment, skill, marionette, chip, pet, possession and buff terms are all zero in
//! this port; they are added to the same sums when those systems exist. Integer
//! division, `(ushort)` truncation and the f32 job factors widened to f64 follow the
//! C# exactly because the client prints these numbers.
#![allow(dead_code)] // equipment, skill and range fields are read once those systems land

/// Base attributes as stored on the character.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BaseStats {
    pub str_: u16,
    pub dex: u16,
    pub int: u16,
    pub vit: u16,
    pub agi: u16,
    pub mag: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CharacterBasics {
    /// 0 Emil, 1 Titania, 2 Dominion, 3 DEM.
    pub race: u8,
    pub level: u8,
    /// Level of the current job. For the joint jobs (Breeder, Gardener) this is the
    /// joint job level, which is what SagaECO reads in that case.
    pub job_level: u8,
    /// SagaECO `PC_JOB` value, 0 = Novice, 1041/1042 = Breeder/Gardener (joint jobs).
    pub job: u16,
    pub base: BaseStats,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DerivedStats {
    /// Job level bonus added to the base attributes (`*_rev`), `CalcStatsRev`.
    pub job_bonus: BaseStats,
    pub max_hp: u32,
    pub max_mp: u32,
    pub max_sp: u32,
    pub max_ep: u32,
    pub min_atk1: u16,
    pub min_atk2: u16,
    pub min_atk3: u16,
    pub max_atk1: u16,
    pub max_atk2: u16,
    pub max_atk3: u16,
    pub min_matk: u16,
    pub max_matk: u16,
    pub def: u16,
    pub def_add: i16,
    pub mdef: u16,
    pub mdef_add: i16,
    pub hit_melee: u16,
    pub hit_ranged: u16,
    /// SagaECO never computes these four for a player; they stay 0.
    pub hit_magic: u16,
    pub hit_critical: u16,
    pub avoid_melee: u16,
    pub avoid_ranged: u16,
    pub avoid_magic: u16,
    pub avoid_critical: u16,
    pub aspd: i16,
    pub cspd: i16,
    pub speed: u16,
    pub range: u32,
    /// `Inventory.MaxPayload[BODY]` (weight capacity) and `MaxVolume[BODY]`.
    pub payload: u32,
    pub volume: u32,
}

const BASE_SPEED: u16 = 420;
const BASE_RANGE: u32 = 1;
const BASE_MAX_EP: u32 = 30;
const JOINT_BREEDER: u16 = 1041;
const JOINT_GARDENER: u16 = 1042;

/// `Configuration.PayloadRate` / `VolumeRate`: SagaMap.xml says 100, read as 100 / 100f.
const PAYLOAD_RATE: f32 = 1.0;
const VOLUME_RATE: f32 = 1.0;

/// Per job: id, HP, MP, SP, payload, volume factors (`HPJobFactor` .. `VolumeJobFactor`).
/// Jobs not listed use 1.0 everywhere, like the C# `default` branches.
#[rustfmt::skip]
const JOB_FACTORS: [(u16, [f32; 5]); 39] = [
    (0,    [1.0,  1.0,  1.0,  0.7, 0.85]), // Novice
    (1,    [1.8,  1.05, 1.1,  1.0, 1.0]),  // Swordman
    (3,    [3.05, 1.25, 1.75, 1.0, 1.0]),  // Blademaster
    (5,    [2.9,  1.25, 1.8,  1.0, 1.0]),  // Bounty Hunter
    (11,   [1.65, 1.05, 1.15, 1.0, 1.0]),  // Fencer
    (13,   [3.3,  1.3,  1.5,  1.0, 1.0]),  // Knight
    (15,   [3.0,  1.25, 1.7,  1.0, 1.0]),  // Dark Stalker
    (21,   [1.45, 1.05, 1.2,  1.0, 1.0]),  // Scout
    (23,   [2.45, 1.3,  1.7,  1.0, 1.0]),  // Assassin
    (25,   [2.5,  1.25, 1.8,  1.0, 1.0]),  // Command
    (31,   [1.35, 1.1,  1.15, 1.0, 1.0]),  // Archer
    (33,   [2.25, 1.4,  1.6,  1.0, 1.0]),  // Striker
    (35,   [2.15, 1.25, 2.3,  1.0, 1.0]),  // Gunner
    (41,   [1.1,  1.2,  1.05, 0.8, 1.0]),  // Wizard
    (43,   [1.85, 2.35, 1.25, 0.8, 1.0]),  // Sorcerer
    (45,   [1.95, 2.3,  1.25, 0.8, 1.0]),  // Sage
    (51,   [1.05, 1.15, 1.1,  0.8, 1.0]),  // Shaman
    (53,   [1.8,  2.4,  1.2,  0.8, 1.0]),  // Elementer
    (55,   [1.85, 2.35, 1.25, 0.8, 1.0]),  // Enchanter
    (61,   [1.15, 1.15, 1.1,  0.8, 1.0]),  // Vates
    (63,   [1.95, 2.2,  1.35, 0.8, 1.0]),  // Druid
    (65,   [2.15, 2.1,  1.25, 0.8, 1.0]),  // Bard
    (71,   [1.3,  1.15, 1.1,  0.8, 1.0]),  // Warlock
    (73,   [2.6,  2.0,  1.4,  0.8, 1.0]),  // Cabalist
    (75,   [2.3,  2.3,  1.35, 0.8, 1.0]),  // Necromancer
    (81,   [1.5,  1.05, 1.15, 1.3, 1.13]), // Tatarabe
    (83,   [3.0,  1.2,  1.6,  1.3, 1.13]), // Blacksmith
    (85,   [2.6,  1.5,  1.9,  1.3, 1.13]), // Machinery
    (91,   [1.4,  1.1,  1.1,  1.3, 1.13]), // Farmasist
    (93,   [2.5,  1.5,  1.8,  1.3, 1.13]), // Alchemist
    (95,   [2.15, 2.1,  1.7,  1.3, 1.13]), // Marionest
    (101,  [1.25, 1.05, 1.15, 1.3, 1.13]), // Ranger
    (103,  [2.8,  1.3,  1.85, 1.3, 1.13]), // Explorer
    (105,  [2.3,  1.5,  2.1,  1.3, 1.13]), // Treasure Hunter
    (111,  [1.2,  1.1,  1.1,  1.3, 1.13]), // Merchant
    (113,  [2.4,  1.3,  1.9,  1.3, 1.13]), // Trader
    (115,  [2.4,  1.9,  1.7,  1.3, 1.13]), // Gambler
    (1041, [3.0,  1.2,  1.6,  1.3, 1.13]), // Breeder
    (1042, [3.0,  1.2,  1.6,  1.3, 1.13]), // Gardener
];

/// Per job: id, whether the job level is offset by 30 (expert jobs), and the
/// STR, DEX, INT, VIT, AGI, MAG growth per job level (`CalcStatsRev`).
#[rustfmt::skip]
const JOB_GROWTH: [(u16, i32, [f32; 6]); 37] = [
    (0,   0,  [0.07, 0.07, 0.07, 0.07, 0.07, 0.07]), // Novice
    (1,   0,  [0.26, 0.15, 0.03, 0.15, 0.19, 0.02]), // Swordman
    (3,   30, [0.3,  0.24, 0.04, 0.2,  0.2,  0.02]), // Blademaster
    (5,   30, [0.23, 0.23, 0.12, 0.15, 0.25, 0.02]), // Bounty Hunter
    (11,  0,  [0.21, 0.18, 0.05, 0.18, 0.16, 0.02]), // Fencer
    (13,  30, [0.17, 0.2,  0.08, 0.32, 0.14, 0.09]), // Knight
    (15,  30, [0.18, 0.23, 0.08, 0.24, 0.17, 0.1]),  // Dark Stalker
    (21,  0,  [0.2,  0.2,  0.05, 0.07, 0.26, 0.02]), // Scout
    (23,  30, [0.21, 0.22, 0.14, 0.11, 0.3,  0.02]), // Assassin
    (25,  30, [0.22, 0.24, 0.16, 0.12, 0.23, 0.03]), // Command
    (31,  0,  [0.15, 0.12, 0.26, 0.1,  0.15, 0.02]), // Archer
    (33,  30, [0.21, 0.14, 0.3,  0.12, 0.21, 0.02]), // Striker
    (35,  30, [0.2,  0.16, 0.3,  0.12, 0.16, 0.06]), // Gunner
    (41,  0,  [0.02, 0.1,  0.21, 0.07, 0.1,  0.3]),  // Wizard
    (43,  30, [0.03, 0.16, 0.24, 0.15, 0.12, 0.3]),  // Sorcerer
    (45,  30, [0.06, 0.24, 0.24, 0.06, 0.14, 0.26]), // Sage
    (51,  0,  [0.02, 0.13, 0.18, 0.06, 0.13, 0.28]), // Shaman
    (53,  30, [0.03, 0.25, 0.2,  0.08, 0.15, 0.29]), // Elementer
    (55,  30, [0.05, 0.27, 0.25, 0.06, 0.1,  0.27]), // Enchanter
    (61,  0,  [0.05, 0.11, 0.25, 0.02, 0.11, 0.26]), // Vates
    (63,  30, [0.12, 0.16, 0.26, 0.06, 0.12, 0.28]), // Druid
    (65,  30, [0.08, 0.2,  0.22, 0.1,  0.2,  0.2]),  // Bard
    (71,  0,  [0.14, 0.15, 0.13, 0.08, 0.12, 0.18]), // Warlock
    (73,  30, [0.21, 0.17, 0.14, 0.14, 0.09, 0.25]), // Cabalist
    (75,  30, [0.14, 0.13, 0.18, 0.06, 0.22, 0.27]), // Necromancer
    (81,  0,  [0.15, 0.25, 0.06, 0.18, 0.14, 0.02]), // Tatarabe
    (83,  30, [0.19, 0.28, 0.12, 0.23, 0.06, 0.12]), // Blacksmith
    (85,  30, [0.16, 0.23, 0.21, 0.16, 0.2,  0.04]), // Machinery
    (91,  0,  [0.15, 0.18, 0.17, 0.2,  0.08, 0.02]), // Farmasist
    (93,  30, [0.16, 0.25, 0.24, 0.21, 0.08, 0.06]), // Alchemist
    (95,  30, [0.14, 0.17, 0.22, 0.08, 0.21, 0.18]), // Marionest
    (101, 0,  [0.1,  0.24, 0.13, 0.08, 0.22, 0.03]), // Ranger
    (103, 30, [0.14, 0.27, 0.16, 0.12, 0.26, 0.05]), // Explorer
    (105, 30, [0.08, 0.16, 0.16, 0.25, 0.27, 0.08]), // Treasure Hunter
    (111, 0,  [0.12, 0.21, 0.25, 0.05, 0.15, 0.02]), // Merchant
    (113, 30, [0.15, 0.26, 0.3,  0.06, 0.2,  0.03]), // Trader
    (115, 30, [0.1,  0.2,  0.21, 0.14, 0.26, 0.09]), // Gambler
];

/// SagaECO `checkPositive`: truncating cast of a positive value, 0 otherwise.
fn check_positive(value: f64) -> u16 {
    if value > 0.0 {
        value as u16
    } else {
        0
    }
}

fn job_factors(job: u16) -> [f32; 5] {
    JOB_FACTORS
        .iter()
        .find(|(id, _)| *id == job)
        .map_or([1.0; 5], |(_, factors)| *factors)
}

fn payload_race_factor(race: u8) -> f32 {
    match race {
        0 => 1.3,
        1 => 0.9,
        2 => 1.1,
        _ => 1.0,
    }
}

/// `CalcStatsRev`: attribute bonus from the job level.
fn job_bonus(basics: &CharacterBasics) -> BaseStats {
    let level = basics.job_level as i32;
    let grow = |base: f64, per_level: f64| (base + per_level) as u16;
    match basics.job {
        JOINT_BREEDER => BaseStats {
            str_: grow(3.0, (level + 3) as f64 * 0.143),
            dex: grow(6.0, (level + 1) as f64 * 0.25),
            int: grow(1.0, level as f64 * 0.04),
            vit: grow(6.0, (level + 1) as f64 * 0.25),
            agi: grow(7.0, level as f64 * 0.28),
            mag: grow(1.0, level as f64 * 0.04),
        },
        JOINT_GARDENER => BaseStats {
            str_: grow(3.0, (level + 2) as f64 * 0.125),
            dex: grow(6.0, (level + 1) as f64 * 0.25),
            int: grow(6.0, (level + 1) as f64 * 0.25),
            vit: grow(5.0, (level + 2) as f64 * 0.25),
            agi: grow(3.0, (level - 1) as f64 * 0.125),
            mag: grow(0.0, level as f64 * 0.04),
        },
        job => match JOB_GROWTH.iter().find(|(id, _, _)| *id == job) {
            Some((_, offset, rates)) => {
                let scaled = (level + offset) as f64;
                let rev = |rate: f32| (scaled * rate as f64) as u16;
                BaseStats {
                    str_: rev(rates[0]),
                    dex: rev(rates[1]),
                    int: rev(rates[2]),
                    vit: rev(rates[3]),
                    agi: rev(rates[4]),
                    mag: rev(rates[5]),
                }
            }
            None => BaseStats::default(),
        },
    }
}

/// Computes every status value for a character without equipment, skills or buffs.
pub fn derive(basics: &CharacterBasics) -> DerivedStats {
    let bonus = job_bonus(basics);
    let level = basics.level as i32;
    let total = |base: u16, rev: u16| base as i32 + rev as i32;
    let s = total(basics.base.str_, bonus.str_);
    let d = total(basics.base.dex, bonus.dex);
    let i = total(basics.base.int, bonus.int);
    let v = total(basics.base.vit, bonus.vit);
    let a = total(basics.base.agi, bonus.agi);
    let m = total(basics.base.mag, bonus.mag);
    let [hp_factor, mp_factor, sp_factor, payload_factor, volume_factor] = job_factors(basics.job);

    // CalcATKRate: DEX based because no bow/gun is equipped; stored as float.
    let atk_rate = (d as f64 * 1.5 / 160.0) as f32;
    let atk_base = check_positive((s + s / 9 * (s / 9)) as f64 * (1.0 + atk_rate as f64));
    let atk_max = check_positive((s + (s + 14) / 5 * ((s + 14) / 5)) as f64);
    let matk_min = check_positive(
        (m + (m + 9) / 8 * ((m + 9) / 8)) as f64 * (1.0 + i as f64 * 1.2f32 as f64 / 320.0),
    );
    let matk_max = check_positive((m + (m + 17) / 6 * ((m + 17) / 6)) as f64);

    let hit_melee = check_positive((d + d / 10 * 11 + level + 3) as f64);
    let hit_ranged = check_positive((i + i / 10 * 11 + level + 3) as f64);
    let def = check_positive((v / 3 + v / 9 * 2) as f64);
    let mdef = check_positive((i / 3 + v / 4) as f64);
    let avoid_melee = check_positive((a + (a + 18) / 9 * ((a + 18) / 9) + level / 3 - 1) as f64);
    let avoid_ranged = check_positive((i * 5 / 3 + a + level / 3 + 3) as f64);
    let aspd = a * 3 + (a + 63) / 9 * ((a + 63) / 9) + 129;
    let cspd = d * 3 + (d + 63) / 9 * ((d + 63) / 9) + 129;

    let max_hp = ((v * 3 + v / 5 * (v / 5) + level * 2 + level / 5 * (level / 5) + 50) as f64
        * hp_factor as f64) as u32;
    let max_mp = ((m * 3 + level + level / 9 * (level / 9) + 30) as f64 * mp_factor as f64) as u32;
    let max_sp = ((i + v + level + level / 9 * (level / 9) + 20) as f64 * sp_factor as f64) as u32;

    let payload = ((s * 2 / 3 + v / 3 + 400) as f64
        * payload_race_factor(basics.race) as f64
        * PAYLOAD_RATE as f64
        * payload_factor as f64
        * 10.0) as u32;
    let volume =
        ((d / 5 + i / 10 + 200) as f64 * volume_factor as f64 * VOLUME_RATE as f64 * 10.0) as u32;

    DerivedStats {
        job_bonus: bonus,
        max_hp,
        max_mp,
        max_sp,
        max_ep: BASE_MAX_EP,
        min_atk1: atk_base,
        min_atk2: atk_base,
        min_atk3: atk_base,
        max_atk1: atk_max,
        max_atk2: atk_max,
        max_atk3: atk_max,
        min_matk: matk_min,
        max_matk: matk_max,
        def,
        def_add: 0,
        mdef,
        mdef_add: 0,
        hit_melee,
        hit_ranged,
        hit_magic: 0,
        hit_critical: 0,
        avoid_melee: avoid_melee.min(400),
        avoid_ranged: avoid_ranged.min(400),
        avoid_magic: 0,
        avoid_critical: 0,
        aspd: aspd.min(800) as i16,
        cspd: cspd.min(800) as i16,
        speed: BASE_SPEED,
        range: BASE_RANGE,
        payload,
        volume,
    }
}

/// Body of `SSMG_PLAYER_STATS_PRE_CALC` (opcode 0x0259) after the 2-byte opcode.
///
/// Field offsets are the C# `data` indexes minus 2. The packet carries the heal rates
/// (never filled by `OnStatsPreCalc`, so 0), the max HP/MP/SP cut to 16 bits and the
/// summed capacity and payload cut to 16 bits, exactly as the C# casts do.
pub fn stats_pre_calc_payload(derived: &DerivedStats) -> Vec<u8> {
    let mut data = [0u8; 74];
    let mut put = |offset: usize, value: u16| {
        data[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
    };
    put(3, derived.speed);
    put(5, derived.min_atk1);
    put(7, derived.min_atk2);
    put(9, derived.min_atk3);
    put(11, derived.max_atk1);
    put(13, derived.max_atk2);
    put(15, derived.max_atk3);
    put(17, derived.min_matk);
    put(19, derived.max_matk);
    put(21, derived.def);
    put(23, derived.def_add as u16);
    put(25, derived.mdef);
    put(27, derived.mdef_add as u16);
    put(29, derived.hit_melee);
    put(31, derived.hit_ranged);
    put(33, derived.hit_magic);
    put(35, derived.hit_critical);
    put(37, derived.avoid_melee);
    put(39, derived.avoid_ranged);
    put(41, derived.avoid_magic);
    put(43, derived.avoid_critical);
    put(51, derived.aspd as u16);
    put(53, derived.cspd as u16);
    put(64, derived.max_hp as u16);
    put(66, derived.max_mp as u16);
    put(68, derived.max_sp as u16);
    put(70, derived.volume as u16);
    put(72, derived.payload as u16);
    data[2] = 30;
    data[63] = 3;
    data[2..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RACES: [(u8, [u16; 6]); 4] = [
        (0, [8, 3, 3, 10, 4, 3]), // Emil
        (1, [6, 3, 6, 4, 2, 10]), // Titania
        (2, [10, 5, 1, 1, 5, 8]), // Dominion
        (3, [5, 5, 5, 5, 5, 5]),  // DEM
    ];

    fn novice(race: u8) -> DerivedStats {
        let (_, stats) = RACES[race as usize];
        derive(&CharacterBasics {
            race,
            level: 1,
            job_level: 1,
            job: 0,
            base: BaseStats {
                str_: stats[0],
                dex: stats[1],
                int: stats[2],
                vit: stats[3],
                agi: stats[4],
                mag: stats[5],
            },
        })
    }

    #[test]
    fn emil_novice_matches_hand_computed_formulas() {
        // STR 8 DEX 3 INT 3 VIT 10 AGI 4 MAG 3, level 1; novice job level 1 adds
        // (ushort)(1 * 0.07) = 0 to every attribute.
        // ATK rate 3 * 1.5 / 160 = 0.028125, so min ATK = (8 + 0) * 1.028125 -> 8.
        // max ATK = 8 + ((8 + 14) / 5)^2 = 8 + 16 = 24.
        // MATK min = (3 + ((3 + 9) / 8)^2) * (1 + 3 * 1.2 / 320) = 4 * 1.01125 -> 4,
        // max = 3 + ((3 + 17) / 6)^2 = 12.
        // HIT = DEX + DEX / 10 * 11 + level + 3 = 7 (melee) and the same with INT (ranged).
        // DEF = 10 / 3 + 10 / 9 * 2 = 5, MDEF = 3 / 3 + 10 / 4 = 3.
        // AVOID melee = 4 + ((4 + 18) / 9)^2 + 1 / 3 - 1 = 7, ranged = 3 * 5 / 3 + 4 + 0 + 3 = 12.
        // ASPD = 4 * 3 + ((4 + 63) / 9)^2 + 129 = 190, CSPD = 3 * 3 + ((3 + 63) / 9)^2 + 129 = 187.
        // HP = (10 * 3 + 2^2 + 1 * 2 + 0 + 50) * 1.0 = 86, MP = 3 * 3 + 1 + 0 + 30 = 40,
        // SP = 3 + 10 + 1 + 0 + 20 = 34.
        // Payload = (8 * 2 / 3 + 10 / 3 + 400) * 1.3 * 0.7 * 10 = 408 * 0.91 * 10 -> 3712.
        // Volume = (3 / 5 + 3 / 10 + 200) * 0.85 * 10 = 1700.
        let stats = novice(0);
        assert_eq!(stats.job_bonus, BaseStats::default());
        assert_eq!((stats.min_atk1, stats.max_atk1), (8, 24));
        assert_eq!((stats.min_atk3, stats.max_atk2), (8, 24));
        assert_eq!((stats.min_matk, stats.max_matk), (4, 12));
        assert_eq!((stats.hit_melee, stats.hit_ranged), (7, 7));
        assert_eq!((stats.def, stats.mdef), (5, 3));
        assert_eq!((stats.avoid_melee, stats.avoid_ranged), (7, 12));
        assert_eq!((stats.aspd, stats.cspd), (190, 187));
        assert_eq!((stats.max_hp, stats.max_mp, stats.max_sp), (86, 40, 34));
        assert_eq!(stats.max_ep, 30);
        assert_eq!((stats.payload, stats.volume), (3712, 1700));
        assert_eq!((stats.speed, stats.range), (420, 1));
        assert_eq!((stats.def_add, stats.mdef_add), (0, 0));
        assert_eq!(
            (
                stats.hit_magic,
                stats.hit_critical,
                stats.avoid_magic,
                stats.avoid_critical
            ),
            (0, 0, 0, 0)
        );
    }

    #[test]
    fn other_races_match_hand_computed_formulas() {
        let titania = novice(1);
        assert_eq!((titania.min_atk1, titania.max_atk1), (6, 22));
        assert_eq!((titania.min_matk, titania.max_matk), (14, 26));
        assert_eq!((titania.hit_melee, titania.hit_ranged), (7, 10));
        assert_eq!((titania.def, titania.mdef), (1, 3));
        assert_eq!((titania.avoid_melee, titania.avoid_ranged), (5, 15));
        assert_eq!((titania.aspd, titania.cspd), (184, 187));
        assert_eq!(
            (titania.max_hp, titania.max_mp, titania.max_sp),
            (64, 61, 31)
        );
        assert_eq!((titania.payload, titania.volume), (2551, 1700));

        let dominion = novice(2);
        assert_eq!((dominion.min_atk1, dominion.max_atk1), (11, 26));
        assert_eq!((dominion.min_matk, dominion.max_matk), (12, 24));
        assert_eq!((dominion.hit_melee, dominion.hit_ranged), (9, 5));
        assert_eq!((dominion.def, dominion.mdef), (0, 0));
        assert_eq!((dominion.avoid_melee, dominion.avoid_ranged), (8, 9));
        assert_eq!((dominion.aspd, dominion.cspd), (193, 193));
        assert_eq!(
            (dominion.max_hp, dominion.max_mp, dominion.max_sp),
            (55, 55, 23)
        );
        assert_eq!((dominion.payload, dominion.volume), (3126, 1708));

        let dem = novice(3);
        assert_eq!((dem.min_atk1, dem.max_atk1), (5, 14));
        assert_eq!((dem.min_matk, dem.max_matk), (6, 14));
        assert_eq!((dem.hit_melee, dem.hit_ranged), (9, 9));
        assert_eq!((dem.def, dem.mdef), (1, 2));
        assert_eq!((dem.avoid_melee, dem.avoid_ranged), (8, 16));
        assert_eq!((dem.aspd, dem.cspd), (193, 193));
        assert_eq!((dem.max_hp, dem.max_mp, dem.max_sp), (68, 46, 31));
        assert_eq!((dem.payload, dem.volume), (2827, 1708));
    }

    #[test]
    fn level_one_max_hp_matches_map_bootstrap() {
        // offline/src/map.rs initial_stats: Emil 86, Titania 64, Dominion 55, DEM 68.
        let hp: Vec<u32> = (0..4).map(|race| novice(race).max_hp).collect();
        assert_eq!(hp, [86, 64, 55, 68]);
    }

    #[test]
    fn job_level_bonus_and_job_factors_apply() {
        // Swordman job level 10: STR (10 * 0.26) = 2, DEX 1, INT 0, VIT 1, AGI 1, MAG 0.
        let base = BaseStats {
            str_: 8,
            dex: 3,
            int: 3,
            vit: 10,
            agi: 4,
            mag: 3,
        };
        let swordman = derive(&CharacterBasics {
            race: 0,
            level: 10,
            job_level: 10,
            job: 1,
            base,
        });
        assert_eq!(
            swordman.job_bonus,
            BaseStats {
                str_: 2,
                dex: 1,
                int: 0,
                vit: 1,
                agi: 1,
                mag: 0
            }
        );
        // HP = (11 * 3 + (11 / 5)^2 + 20 + (10 / 5)^2 + 50) * 1.8 = 111 * 1.8 -> 199.
        assert_eq!(swordman.max_hp, 199);
        // Expert jobs offset the job level by 30: Blademaster level 1 -> 31 * 0.3 = 9.
        let blademaster = derive(&CharacterBasics {
            race: 0,
            level: 1,
            job_level: 1,
            job: 3,
            base,
        });
        assert_eq!(blademaster.job_bonus.str_, 9);
    }

    #[test]
    fn pre_calc_payload_layout() {
        let stats = novice(0);
        let payload = stats_pre_calc_payload(&stats);
        assert_eq!(payload.len(), 72);
        // C# data offset minus 2 for every field.
        assert_eq!(payload[0], 30); // PutByte(30, 2)
        assert_eq!(payload[61], 3); // PutByte(3, 63)
        assert_eq!(u16::from_be_bytes([payload[1], payload[2]]), 420); // Speed
        assert_eq!(u16::from_be_bytes([payload[3], payload[4]]), 8); // ATK1 min
        assert_eq!(u16::from_be_bytes([payload[9], payload[10]]), 24); // ATK1 max
        assert_eq!(u16::from_be_bytes([payload[15], payload[16]]), 4); // MATK min
        assert_eq!(u16::from_be_bytes([payload[17], payload[18]]), 12); // MATK max
        assert_eq!(u16::from_be_bytes([payload[19], payload[20]]), 5); // DEF
        assert_eq!(u16::from_be_bytes([payload[23], payload[24]]), 3); // MDEF
        assert_eq!(u16::from_be_bytes([payload[27], payload[28]]), 7); // HIT melee
        assert_eq!(u16::from_be_bytes([payload[35], payload[36]]), 7); // AVOID melee
        assert_eq!(u16::from_be_bytes([payload[37], payload[38]]), 12); // AVOID ranged
        assert_eq!(i16::from_be_bytes([payload[49], payload[50]]), 190); // ASPD
        assert_eq!(i16::from_be_bytes([payload[51], payload[52]]), 187); // CSPD
        assert_eq!(u16::from_be_bytes([payload[62], payload[63]]), 86); // HP
        assert_eq!(u16::from_be_bytes([payload[64], payload[65]]), 40); // MP
        assert_eq!(u16::from_be_bytes([payload[66], payload[67]]), 34); // SP
        assert_eq!(u16::from_be_bytes([payload[68], payload[69]]), 1700); // capacity
        assert_eq!(u16::from_be_bytes([payload[70], payload[71]]), 3712); // payload
                                                                          // Heal rates and the gap before HP are never written.
        assert!(payload[43..49].iter().all(|byte| *byte == 0));
        assert!(payload[53..61].iter().all(|byte| *byte == 0));
    }
}
