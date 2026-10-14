mod autotest;
mod commands;
mod content;
mod diagnostics;
#[cfg(test)]
mod integration_tests;
mod map;
mod progression;
mod protocol;
mod quest;
mod quest_rewards;
mod starter_catalog;
mod stats;
mod store;
mod terrain;
mod trace;
mod world;

use diagnostics::{event, failure, protocol_failure, Event, Failure, ProtocolStage};
use protocol::{Connection, ServerRole, MAX_PACKET_SIZE};
use std::cell::RefCell;
use std::env;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;
use store::Store;

fn main() -> std::process::ExitCode {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|arg| arg == "autotest") {
        // Test tooling prints its own readable errors; the server's privacy filter is not needed.
        return match autotest::parse_options(&args[1..]).and_then(|o| autotest::run(&o)) {
            Ok(true) => std::process::ExitCode::SUCCESS,
            Ok(false) => std::process::ExitCode::FAILURE,
            Err(error) => {
                eprintln!("autotest: {error}");
                std::process::ExitCode::from(2)
            }
        };
    }
    std::panic::set_hook(Box::new(|_| event(Event::Panic)));
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            failure(Failure::Startup, &error);
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> io::Result<()> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|arg| arg == "audit-content") {
        if args.len() != 2 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "usage: eco-reborn audit-content <SagaECO/Configuration/DB path>",
            ));
        }
        let report = content::audit(Path::new(&args[1]))
            .map_err(|error| io::Error::new(io::ErrorKind::Other, error.to_string()))?;
        println!("{report}");
        return Ok(());
    }
    if args
        .first()
        .is_some_and(|arg| arg == "inspect-training-actor")
    {
        if args.len() != 3 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "usage: eco-reborn inspect-training-actor <SagaECO/Configuration/DB path> <terrain.map>",
            ));
        }
        println!(
            "{}",
            world::inspect(Path::new(&args[1]), Path::new(&args[2]))?
        );
        return Ok(());
    }
    if args.first().is_some_and(|arg| arg == "replay-hunt-quest") {
        if args.len() != 3 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "usage: eco-reborn replay-hunt-quest <SagaECO/Configuration/DB path> <kills.csv>",
            ));
        }
        println!(
            "{}",
            quest::replay(Path::new(&args[1]), Path::new(&args[2])).map_err(|_| io::Error::new(
                io::ErrorKind::InvalidData,
                "hunt replay input is invalid or unreadable"
            ))?
        );
        return Ok(());
    }
    if args
        .first()
        .is_some_and(|arg| arg == "replay-timed-hunt-quest")
    {
        if args.len() != 3 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "usage: eco-reborn replay-timed-hunt-quest <SagaECO/Configuration/DB path> <events.csv>",
            ));
        }
        println!(
            "{}",
            quest::replay_timed(Path::new(&args[1]), Path::new(&args[2])).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "timed hunt replay input is invalid or unreadable",
                )
            })?
        );
        return Ok(());
    }
    if args
        .first()
        .is_some_and(|arg| arg == "replay-quest-lifecycle")
    {
        if args.len() != 5 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "usage: eco-reborn replay-quest-lifecycle <SagaECO/Configuration/DB path> <events.csv> <initial_points> <initial_fame>",
            ));
        }
        let parse_digits = |value: &str| {
            (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| value.parse::<u32>().ok())
                .flatten()
        };
        let initial_points = parse_digits(&args[3])
            .and_then(|value| u16::try_from(value).ok())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "lifecycle input is invalid")
            })?;
        let initial_fame = parse_digits(&args[4]).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "lifecycle input is invalid")
        })?;
        println!(
            "{}",
            quest::replay_lifecycle(
                Path::new(&args[1]),
                Path::new(&args[2]),
                initial_points,
                initial_fame,
            )
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "lifecycle replay input is invalid or unreadable",
                )
            })?
        );
        return Ok(());
    }
    if args
        .first()
        .is_some_and(|arg| arg == "replay-saved-quest-lifecycle")
    {
        if args.len() != 8 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "usage: eco-reborn replay-saved-quest-lifecycle <SagaECO/Configuration/DB path> <events.csv> <save.sqlite> <account_id> <character_id> <initial_points> <initial_fame>",
            ));
        }
        let parse_digits = |value: &str| {
            (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| value.parse::<u32>().ok())
                .flatten()
        };
        let account_id = parse_digits(&args[4])
            .filter(|value| *value != 0)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "saved replay input is invalid")
            })?;
        let character_id = parse_digits(&args[5])
            .filter(|value| *value != 0)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "saved replay input is invalid")
            })?;
        let initial_points = parse_digits(&args[6])
            .and_then(|value| u16::try_from(value).ok())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "saved replay input is invalid")
            })?;
        let initial_fame = parse_digits(&args[7]).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "saved replay input is invalid")
        })?;
        let save_path = Path::new(&args[3]);
        let mut store = Store::open_existing(save_path).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "saved replay input is invalid or unreadable",
            )
        })?;
        let report = store
            .replay_saved_quest(
                account_id,
                character_id,
                Path::new(&args[1]),
                Path::new(&args[2]),
                initial_points,
                initial_fame,
            )
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "saved replay input is invalid or unreadable",
                )
            })?;
        println!("{report}");
        return Ok(());
    }
    if args
        .first()
        .is_some_and(|arg| arg == "initialize-reward-inventory")
    {
        let invalid = || {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "reward inventory initialization input is invalid or unreadable",
            )
        };
        if args.len() != 7 || args[6] != "--declare-starter-only-inventory" {
            return Err(invalid());
        }
        let parse_id = |value: &str| {
            (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| value.parse::<u32>().ok())
                .flatten()
                .filter(|value| *value > 0)
                .ok_or_else(invalid)
        };
        let account_id = parse_id(&args[4])?;
        let character_id = parse_id(&args[5])?;
        let db = Path::new(&args[1]);
        quest_rewards::validate(db, Path::new(&args[2])).map_err(|_| invalid())?;
        starter_catalog::validate(&db.join("item.csv")).map_err(|_| invalid())?;
        let mut store = Store::open_existing(Path::new(&args[3])).map_err(|_| invalid())?;
        store
            .initialize_reward_inventory(account_id, character_id, true)
            .map_err(|_| invalid())?;
        println!("Reward inventory initialized or preserved.");
        return Ok(());
    }
    if args
        .first()
        .is_some_and(|arg| arg == "turn-in-completed-quest")
    {
        let invalid = || {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "completed quest turn-in input is invalid or unreadable",
            )
        };
        if args.len() != 7 || args[6] != "--declare-npc-event-11000435" {
            return Err(invalid());
        }
        let parse_id = |value: &str| {
            (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| value.parse::<u32>().ok())
                .flatten()
                .filter(|value| *value > 0)
                .ok_or_else(invalid)
        };
        let account_id = parse_id(&args[4])?;
        let character_id = parse_id(&args[5])?;
        let db = Path::new(&args[1]);
        quest_rewards::validate(db, Path::new(&args[2])).map_err(|_| invalid())?;
        progression::validate_novice_exp_table(db).map_err(|_| invalid())?;
        starter_catalog::validate(&db.join("item.csv")).map_err(|_| invalid())?;
        progression::validate_novice_map_flags(db).map_err(|_| invalid())?;
        let mut store = Store::open_existing(Path::new(&args[3])).map_err(|_| invalid())?;
        store
            .turn_in_completed_quest(account_id, character_id, 11000435)
            .map_err(|_| invalid())?;
        println!("Completed quest turned in.");
        return Ok(());
    }
    if args.first().is_some_and(|arg| arg == "initialize-gold") {
        if args.len() != 5 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "usage: eco-reborn initialize-gold <save.sqlite> <account_id> <character_id> <gold>",
            ));
        }
        let parse_ascii_u32 = |value: &str| {
            (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| value.parse::<u32>().ok())
                .flatten()
        };
        let account_id = parse_ascii_u32(&args[2])
            .filter(|value| *value > 0)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "gold initialization input is invalid",
                )
            })?;
        let character_id = parse_ascii_u32(&args[3])
            .filter(|value| *value > 0)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "gold initialization input is invalid",
                )
            })?;
        let gold = parse_ascii_u32(&args[4])
            .filter(|value| *value <= 99_999_999)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "gold initialization input is invalid",
                )
            })?;
        let mut store = Store::open_existing(Path::new(&args[1])).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "gold initialization input is invalid or unreadable",
            )
        })?;
        store
            .initialize_gold(account_id, character_id, gold)
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "gold initialization input is invalid or unreadable",
                )
            })?;
        println!("Carried gold initialized or preserved.");
        return Ok(());
    }
    if args.first().is_some_and(|arg| arg == "place-character") {
        let invalid = || {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "usage: eco-reborn place-character <save.sqlite> <account_id> <character_id> <terrain.map> <x> <y> <direction>",
            )
        };
        if args.len() != 8 {
            return Err(invalid());
        }
        let number = |value: &str| {
            (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| value.parse::<u32>().ok())
                .flatten()
        };
        let account_id = number(&args[2]).filter(|v| *v > 0).ok_or_else(invalid)?;
        let character_id = number(&args[3]).filter(|v| *v > 0).ok_or_else(invalid)?;
        let tile = |value: &str| number(value).and_then(|v| u8::try_from(v).ok());
        let x = tile(&args[5]).ok_or_else(invalid)?;
        let y = tile(&args[6]).ok_or_else(invalid)?;
        let direction = tile(&args[7]).filter(|v| *v < 8).ok_or_else(invalid)?;
        let terrain = terrain::Terrain::load(Path::new(&args[4]))?;
        if !terrain.passable(x, y) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "destination tile is not walkable",
            ));
        }
        let mut store = Store::open_existing(Path::new(&args[1]))?;
        store.place_character(account_id, character_id, terrain.map_id(), x, y, direction)?;
        println!("Character placed on map {} at ({x},{y}).", terrain.map_id());
        return Ok(());
    }
    if args
        .first()
        .is_some_and(|arg| arg == "initialize-reward-context")
    {
        if args.len() != 7 || args[6] != "--declare-no-ring" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "usage: eco-reborn initialize-reward-context <save.sqlite> <account_id> <character_id> <ep> <cp> --declare-no-ring",
            ));
        }
        let parse_ascii_u32 = |value: &str| {
            (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| value.parse::<u32>().ok())
                .flatten()
        };
        let invalid = || {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "reward context initialization input is invalid",
            )
        };
        let account_id = parse_ascii_u32(&args[2])
            .filter(|value| *value > 0)
            .ok_or_else(invalid)?;
        let character_id = parse_ascii_u32(&args[3])
            .filter(|value| *value > 0)
            .ok_or_else(invalid)?;
        let ep = parse_ascii_u32(&args[4])
            .filter(|value| *value <= 30)
            .ok_or_else(invalid)?;
        let cp = parse_ascii_u32(&args[5])
            .filter(|value| *value <= 99_999_999)
            .ok_or_else(invalid)?;
        let mut store = Store::open_existing(Path::new(&args[1])).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "reward context initialization input is invalid or unreadable",
            )
        })?;
        store
            .initialize_reward_context(account_id, character_id, ep, cp, true)
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "reward context initialization input is invalid or unreadable",
                )
            })?;
        println!("Reward context initialized or preserved.");
        return Ok(());
    }
    if args
        .first()
        .is_some_and(|arg| arg == "initialize-novice-progress")
    {
        if args.len() != 4 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "usage: eco-reborn initialize-novice-progress <save.sqlite> <account_id> <character_id>",
            ));
        }
        let parse_id = |value: &str| {
            (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| value.parse::<u32>().ok())
                .flatten()
                .filter(|value| *value > 0)
        };
        let account_id = parse_id(&args[2]).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "saved initialization input is invalid",
            )
        })?;
        let character_id = parse_id(&args[3]).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "saved initialization input is invalid",
            )
        })?;
        let mut store = Store::open_existing(Path::new(&args[1])).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "saved initialization input is invalid or unreadable",
            )
        })?;
        store
            .initialize_novice_progress(account_id, character_id)
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "saved initialization input is invalid or unreadable",
                )
            })?;
        println!("Novice progression initialized or preserved.");
        return Ok(());
    }
    if args
        .first()
        .is_some_and(|arg| arg == "initialize-novice-base-stats")
    {
        if args.len() != 5 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "usage: eco-reborn initialize-novice-base-stats <SagaLogin.xml> <save.sqlite> <account_id> <character_id>"));
        }
        let parse_id = |value: &str| {
            (!value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()))
                .then(|| value.parse::<u32>().ok())
                .flatten()
                .filter(|v| *v > 0)
        };
        let account_id = parse_id(&args[3]).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "novice base stats input is invalid",
            )
        })?;
        let character_id = parse_id(&args[4]).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "novice base stats input is invalid",
            )
        })?;
        let stats = progression::start_stats(Path::new(&args[1])).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "novice base stats input is invalid or unreadable",
            )
        })?;
        let mut store = Store::open_existing(Path::new(&args[2])).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "novice base stats input is invalid or unreadable",
            )
        })?;
        store
            .initialize_novice_base_stats(account_id, character_id, stats)
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "novice base stats input is invalid or unreadable",
                )
            })?;
        println!("Novice base stats initialized or preserved.");
        return Ok(());
    }
    if args
        .first()
        .is_some_and(|arg| arg == "initialize-starter-inventory")
    {
        if args.len() != 6 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "usage: eco-reborn initialize-starter-inventory <SagaLogin.xml> <item.csv> <save.sqlite> <account_id> <character_id>",
            ));
        }
        let parse_id = |value: &str| {
            (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| value.parse::<u32>().ok())
                .flatten()
                .filter(|value| *value > 0)
        };
        let account_id = parse_id(&args[4]).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "starter inventory input is invalid",
            )
        })?;
        let character_id = parse_id(&args[5]).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "starter inventory input is invalid",
            )
        })?;
        let items = progression::start_items(Path::new(&args[1])).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "starter inventory input is invalid or unreadable",
            )
        })?;
        starter_catalog::validate(Path::new(&args[2])).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "starter inventory input is invalid or unreadable",
            )
        })?;
        let mut store = Store::open_existing(Path::new(&args[3])).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "starter inventory input is invalid or unreadable",
            )
        })?;
        store
            .initialize_starter_inventory(account_id, character_id, items)
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "starter inventory input is invalid or unreadable",
                )
            })?;
        println!("Starter inventory initialized or preserved.");
        return Ok(());
    }
    if args.first().is_some_and(|arg| arg == "apply-novice-exp") {
        if !(args.len() == 7 || args.len() == 8 && args[7] == "--declare-unmodified-status") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "usage: eco-reborn apply-novice-exp <SagaECO/Configuration/DB path> <save.sqlite> <account_id> <character_id> <base_exp_award> <job_exp_award> [--declare-unmodified-status]",
            ));
        }
        let parse_ascii_u32 = |value: &str| {
            (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| value.parse::<u32>().ok())
                .flatten()
        };
        let account_id = parse_ascii_u32(&args[3])
            .filter(|value| *value != 0)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "novice EXP input is invalid")
            })?;
        let character_id = parse_ascii_u32(&args[4])
            .filter(|value| *value != 0)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "novice EXP input is invalid")
            })?;
        let base_award = parse_ascii_u32(&args[5]).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "novice EXP input is invalid")
        })?;
        let job_award = parse_ascii_u32(&args[6]).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "novice EXP input is invalid")
        })?;
        progression::validate_novice_exp_table(Path::new(&args[1])).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "novice EXP input is invalid or unreadable",
            )
        })?;
        starter_catalog::validate(&Path::new(&args[1]).join("item.csv")).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "novice EXP input is invalid or unreadable",
            )
        })?;
        progression::validate_novice_map_flags(Path::new(&args[1])).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "novice EXP input is invalid or unreadable",
            )
        })?;
        let mut store = Store::open_existing(Path::new(&args[2])).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "novice EXP input is invalid or unreadable",
            )
        })?;
        let totals = store
            .apply_novice_exp(
                account_id,
                character_id,
                base_award,
                job_award,
                args.len() == 8,
            )
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "novice EXP input is invalid or unreadable",
                )
            })?;
        println!("Base EXP: {}; Job EXP: {}", totals[0], totals[1]);
        return Ok(());
    }
    let (role, default_port) = match args.first().map(String::as_str) {
        Some("serve-world") => (ServerRole::Gate, 17831),
        Some("serve-map") => (ServerRole::Map, 17833),
        None | Some("serve") | Some("serve-login") => (ServerRole::Login, 17832),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unknown server command",
            ))
        }
    };
    trace::set_role(match role {
        ServerRole::Gate => "world",
        ServerRole::Login => "login",
        ServerRole::Map => "map",
    });
    let (port, save_path) = match args.as_slice() {
        [] | [_] => (default_port, "saves/world.sqlite"),
        [_, value, rest @ ..] if role == ServerRole::Map || rest.len() <= 1 => (
            value
                .parse::<u16>()
                .ok()
                .filter(|port| *port != 0)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "port must be 1..65535")
                })?,
            rest.first()
                .map(String::as_str)
                .unwrap_or("saves/world.sqlite"),
        ),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "usage: eco-reborn serve-map [port [save.sqlite [terrain.map ...]]] | serve-world|serve-login [port [save.sqlite]] | audit-content <path> | inspect-training-actor <SagaECO/Configuration/DB path> <terrain.map> | replay-hunt-quest <SagaECO/Configuration/DB path> <kills.csv> | replay-timed-hunt-quest <SagaECO/Configuration/DB path> <events.csv> | replay-quest-lifecycle <SagaECO/Configuration/DB path> <events.csv> <initial_points> <initial_fame> | replay-saved-quest-lifecycle <SagaECO/Configuration/DB path> <events.csv> <save.sqlite> <account_id> <character_id> <initial_points> <initial_fame> | initialize-reward-inventory <SagaECO/Configuration/DB path> <SagaMap.xml> <save.sqlite> <account_id> <character_id> --declare-starter-only-inventory | initialize-gold <save.sqlite> <account_id> <character_id> <gold> | initialize-reward-context <save.sqlite> <account_id> <character_id> <ep> <cp> --declare-no-ring | initialize-novice-progress <save.sqlite> <account_id> <character_id> | initialize-novice-base-stats <SagaLogin.xml> <save.sqlite> <account_id> <character_id> | initialize-starter-inventory <SagaLogin.xml> <item.csv> <save.sqlite> <account_id> <character_id> | apply-novice-exp <SagaECO/Configuration/DB path> <save.sqlite> <account_id> <character_id> <base_exp_award> <job_exp_award> [--declare-unmodified-status]",
            ))
        }
    };

    let maps = if role == ServerRole::Map {
        let paths: Vec<&Path> = if args.len() > 3 {
            args[3..].iter().map(Path::new).collect()
        } else {
            vec![
                Path::new("data/20013003.map"),
                Path::new("data/10023000.map"),
            ]
        };
        Some(Rc::new(terrain::Maps::load(&paths)?))
    } else {
        None
    };
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let store = Rc::new(RefCell::new(Store::open(Path::new(save_path))?));
    event(Event::Listening);
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                event(Event::ClientConnected);
                if let Err(error) = serve(stream, Rc::clone(&store), role, maps.clone()) {
                    failure(Failure::Connection, &error);
                }
                event(Event::ConnectionClosed);
            }
            Err(error) => failure(Failure::Accept, &error),
        }
    }
    Ok(())
}

fn serve(
    mut stream: TcpStream,
    store: Rc<RefCell<Store>>,
    role: ServerRole,
    maps: Option<Rc<terrain::Maps>>,
) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(120)))?;
    stream.set_write_timeout(Some(Duration::from_secs(120)))?;
    let mut connection = Connection::with_role(store, role, maps);
    let mut input = [0u8; 4096];
    let mut buffered = Vec::new();

    loop {
        let count = stream.read(&mut input)?;
        if count == 0 {
            return Ok(());
        }
        buffered.extend_from_slice(&input[..count]);

        loop {
            let input_size = connection.next_input_size(&buffered)?;
            match input_size {
                Some(size) if buffered.len() >= size => {
                    let frame: Vec<u8> = buffered.drain(..size).collect();
                    if let Some(reply) = connection.receive(&frame)? {
                        stream.write_all(&reply)?;
                    }
                    if connection.should_close() {
                        return Ok(());
                    }
                }
                Some(_) | None => break,
            }
        }
        if buffered.len() > MAX_PACKET_SIZE + 8 {
            protocol_failure(ProtocolStage::FrameHeader, None);
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "input buffer exceeds frame limit",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{
        aes_key_from_shared, decrypt_frame, encrypt_frame, make_packet, PRIME_DECIMAL,
    };
    use num_bigint::BigUint;
    use num_traits::Num;
    use std::io::Read;
    use std::thread;

    #[test]
    fn tcp_handshake_version_check_and_ping_survive_fragmented_reads() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let store = Rc::new(RefCell::new(Store::in_memory().unwrap()));
            serve(stream, store, ServerRole::Login, None).unwrap();
        });
        let mut client = TcpStream::connect(address).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        client
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();

        let init = crate::protocol::INIT_PACKET;
        client.write_all(&init[..3]).unwrap();
        client.write_all(&init[3..]).unwrap();
        let server_key = read_exact_vec(&mut client, 529);

        let prime = BigUint::from_str_radix(PRIME_DECIMAL, 10).unwrap();
        let client_private = BigUint::from(5u8);
        let client_public = BigUint::from(3u8).modpow(&client_private, &prime);
        let mut client_key = Vec::with_capacity(260);
        client_key.extend_from_slice(&256u32.to_be_bytes());
        client_key.extend_from_slice(format!("{client_public:0>256x}").as_bytes());
        client.write_all(&client_key[..7]).unwrap();
        client.write_all(&client_key[7..]).unwrap();

        let public_hex = std::str::from_utf8(&server_key[273..529]).unwrap();
        let server_public = BigUint::from_str_radix(public_hex, 16).unwrap();
        let shared = server_public.modpow(&client_private, &prime);
        let key = aes_key_from_shared(&shared).unwrap();

        let mut requests = encrypt_frame(&make_packet(0x0001, &[0x55]), &key).unwrap();
        requests.extend(encrypt_frame(&make_packet(0x000a, &[0x12, 0x34]), &key).unwrap());
        client.write_all(&requests).unwrap();

        let mut opcodes = Vec::new();
        for _ in 0..4 {
            let mut header = [0u8; 8];
            client.read_exact(&mut header).unwrap();
            let body_len = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
            let mut frame = header.to_vec();
            frame.extend(read_exact_vec(&mut client, body_len));
            let (packet, _) = decrypt_frame(&frame, &key).unwrap();
            opcodes.push(u16::from_be_bytes([packet[2], packet[3]]));
            if opcodes.len() == 4 {
                assert_eq!(&packet[4..], &[0x12, 0x34]);
            }
        }
        assert_eq!(opcodes, [0xffff, 0x001e, 0x0002, 0x000b]);
        drop(client);
        server.join().unwrap();
    }

    fn read_exact_vec(reader: &mut impl Read, size: usize) -> Vec<u8> {
        let mut data = vec![0u8; size];
        reader.read_exact(&mut data).unwrap();
        data
    }
}
