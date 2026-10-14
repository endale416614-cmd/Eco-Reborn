//! Source-backed map-entry packets using 2017 EcoServerEmulator fields and the
//! current client's verified 0x01ff tail read order where the two differ.
//!
//! `bootstrap` serializes the actor and the server's initial status notifications.
//! It does not synthesize inventory items or map geometry. The prototype's store
//! keeps only character creation fields and position; the remaining values below
//! are explicit source defaults or the source's level-one race formulas.

use crate::protocol::make_packet as packet;
use crate::store::Character;
use std::io;

/// This single-player map session reserves the source allocator's first PC actor ID.
/// It is intentionally separate from the durable local character primary key.
// shortcut: one player per map view; replace with a shared actor registry before co-op/multiple players.
pub(crate) const PLAYER_ACTOR_ID: u32 = 16;

pub fn bootstrap(character: &Character) -> io::Result<Vec<Vec<u8>>> {
    let stats = initial_stats(character.race)?;
    let actor = actor_packet(character, &stats, PLAYER_ACTOR_ID);
    let mut packets = Vec::with_capacity(14);

    // ChangeMap(01FD, MapMove=0) startup notifications, in source order.
    packets.push(packet(0x1f72, &[0])); // ShowTinyIcon default
    packets.push(packet(0x026a, &0u64.to_be_bytes())); // DailyDungeonCleared default
    packets.push(move_speed(PLAYER_ACTOR_ID, 10)); // lock until client requests movement
    let mut mode = Vec::with_capacity(12);
    put_u32(&mut mode, PLAYER_ACTOR_ID);
    put_u32(&mut mode, 2);
    put_u32(&mut mode, 0);
    packets.push(packet(0x0fa7, &mode));
    packets.push(packet(0x1a5f, &0u32.to_be_bytes())); // RightClickSettings.ALLOW_ALL

    // Source ItemInfoExt(2) is only the inventory-end marker, not a fabricated item.
    packets.push(packet(0x0203, &[0x02, 0x00]));
    packets.push(actor);
    packets.push(vital_packet(0x0221, PLAYER_ACTOR_ID, &stats));
    packets.push(vital_packet(0x021c, PLAYER_ACTOR_ID, &stats));

    let mut buffs = vec![0; 52];
    buffs[..4].copy_from_slice(&PLAYER_ACTOR_ID.to_be_bytes());
    packets.push(packet(0x157c, &buffs));
    packets.push(detailed_status_one(&stats));
    packets.push(detailed_status_two());
    packets.push(elements());
    packets.push(capacity());
    packets.push(packet(
        0x1b67,
        &[PLAYER_ACTOR_ID.to_be_bytes(), 0u32.to_be_bytes()].concat(),
    ));
    Ok(packets)
}

/// Source-based candidate map-load speed response: actor ID and speed 420.
/// tarathep SagaECO initializes ActorPC.Speed to 420; this is not proven universal
/// across the target's protocol version. The upstream 13C7 FinishMapMove handler
/// itself is a no-op; dispatch decides when this response is appropriate.
pub fn loaded(actor_id: u32) -> Vec<Vec<u8>> {
    vec![move_speed(actor_id, 420)]
}

/// SagaECO SSMG_SKILL_CHANGE_BATTLE_STATUS (0FA6) echo for a 0FA5 stance toggle.
pub fn battle_status(actor_id: u32, status: u8) -> Vec<u8> {
    let mut data = actor_id.to_be_bytes().to_vec();
    data.push(status);
    packet(0x0fa6, &data)
}

/// SagaECO SSMG_CHAT_MOTION (121C) echo for a 121B emote request.
pub fn motion(actor_id: u32, motion: u16, looping: u8) -> Vec<u8> {
    let mut data = actor_id.to_be_bytes().to_vec();
    put_u16(&mut data, motion);
    data.extend_from_slice(&[looping, 0]);
    packet(0x121c, &data)
}

/// SagaECO SSMG_CHAT_PUBLIC (03E9): a chat line from an actor; the text is NUL-terminated.
pub fn chat(actor_id: u32, text: &str) -> Vec<u8> {
    let mut data = actor_id.to_be_bytes().to_vec();
    put_tstr(&mut data, text);
    packet(0x03e9, &data)
}

/// System text in the chat log: SagaECO SendSystemMessage uses actor ID 0xFFFFFFFF.
pub fn system_message(text: &str) -> Vec<u8> {
    chat(u32::MAX, text)
}

/// SagaECO SSMG_ACTOR_MOVE (11F9) with MoveType.WARP (14): instant move within the map.
pub fn teleport(actor_id: u32, world_x: i16, world_y: i16, direction: u16) -> Vec<u8> {
    let mut data = actor_id.to_be_bytes().to_vec();
    data.extend_from_slice(&world_x.to_be_bytes());
    data.extend_from_slice(&world_y.to_be_bytes());
    put_u16(&mut data, direction);
    put_u16(&mut data, 14);
    packet(0x11f9, &data)
}

/// SagaECO SSMG_LOGOUT (0020), echoing the client's 001F result byte.
/// SagaECO SSMG_PLAYER_STATS_PRE_CALC (0259): derived stats for the status window's
/// proposed attribute values (request 0258).
pub fn stats_pre_calc(derived: &crate::stats::DerivedStats) -> Vec<u8> {
    packet(0x0259, &crate::stats::stats_pre_calc_payload(derived))
}

/// SagaECO SSMG_PLAYER_LEVEL (023A): base/job levels and unspent status/skill points.
pub fn player_level(level: u8, job_level: u8, bonus_points: u16, skill_points: u16) -> Vec<u8> {
    let mut data = vec![level, job_level, 0, 0, 0]; // 2nd jobs (X/T) and joint job unused
    put_u16(&mut data, bonus_points);
    put_u16(&mut data, skill_points);
    put_u16(&mut data, 0);
    put_u16(&mut data, 0);
    packet(0x023a, &data)
}

/// SagaECO SSMG_PLAYER_JOB (0244): job ID; the joint-job field stays 0 without one.
pub fn player_job(job: u32) -> Vec<u8> {
    let mut data = job.to_be_bytes().to_vec();
    data.extend_from_slice(&0u32.to_be_bytes());
    packet(0x0244, &data)
}

/// SagaECO SSMG_PLAYER_EXP (0235), Saga10+ layout: bar fill, WRP, e-coin, raw EXP values.
pub fn player_exp(exp_fill: u32, job_exp_fill: u32, exp: u64, job_exp: u64) -> Vec<u8> {
    let mut data = exp_fill.to_be_bytes().to_vec();
    data.extend_from_slice(&job_exp_fill.to_be_bytes());
    data.extend_from_slice(&0i32.to_be_bytes()); // WRP
    data.extend_from_slice(&0u32.to_be_bytes()); // e-coin
    put_u64(&mut data, exp);
    put_u64(&mut data, job_exp);
    packet(0x0235, &data)
}

pub fn logout(result: u8) -> Vec<u8> {
    packet(0x0020, &[result])
}

/// SagaECO's reply for an event with no loaded script: open the event, show one
/// NPC message with the talk motion (131), then close the message and event.
// shortcut: every NPC and portal event gets this notice until event content is ported.
pub fn event_unavailable(event_id: u32) -> Vec<Vec<u8>> {
    let mut current = event_id.to_be_bytes().to_vec();
    current.extend_from_slice(&[0; 4]);
    let mut message = event_id.to_be_bytes().to_vec();
    message.push(1);
    put_tstr(&mut message, "This isn't available offline yet.");
    put_u16(&mut message, 131);
    put_tstr(&mut message, "");
    vec![
        packet(0x05dc, &[]),
        packet(0x05e8, &current),
        packet(0x03f8, &[]),
        packet(0x03f7, &message),
        packet(0x03f9, &[]),
        packet(0x05dd, &[]),
    ]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InitialStats {
    str_: u16,
    dex: u16,
    int: u16,
    vit: u16,
    agi: u16,
    mag: u16,
    max_hp: u32,
    max_mp: u32,
    max_sp: u32,
    max_ep: u32,
}

fn initial_stats(race: u8) -> io::Result<InitialStats> {
    let (str_, dex, int, vit, agi, mag) = match race {
        0 => (8, 3, 3, 10, 4, 3), // Emil
        1 => (6, 3, 6, 4, 2, 10), // Titania
        2 => (10, 5, 1, 1, 5, 8), // Dominion
        3 => (5, 5, 5, 5, 5, 5),  // DEM
        _ => return Err(invalid("unknown race in map bootstrap")),
    };
    // Level 1 novice, no skill/equipment bonuses: SagaECO StatusFactory via stats.rs.
    let derived = crate::stats::derive(&crate::stats::CharacterBasics {
        race,
        level: 1,
        job_level: 1,
        job: 0,
        base: crate::stats::BaseStats {
            str_,
            dex,
            int,
            vit,
            agi,
            mag,
        },
    });
    Ok(InitialStats {
        str_,
        dex,
        int,
        vit,
        agi,
        mag,
        max_hp: derived.max_hp,
        max_mp: derived.max_mp,
        max_sp: derived.max_sp,
        max_ep: derived.max_ep,
    })
}

/// Equipment image IDs (client item tables, third column) of the SagaLogin.xml starter
/// clothes, in CharaEquipment slot order: head, head acc., face, face acc., chest acc.,
/// top, bottom, back, right, left, shoes, socks, pet, effect.
// shortcut: appearance only until the inventory system sends real equipped items (phase 0).
pub(crate) fn starter_appearance(race: u8, sex: u8) -> [u32; 14] {
    // (face accessory, top, bottom, shoes); sex 0 = male, 1 = female.
    let (face_acce, top, bottom, shoes) = match (race, sex) {
        (0, 0) => (0, 50000000, 50010300, 50060150), // Emil: smock, monpe, black sandals
        (0, _) => (0, 50000000, 50010300, 50060100),
        (1, 0) => (0, 50000100, 50010400, 50060150), // Titania: replica frill, trousers
        (1, _) => (0, 50001300, 50012360, 50060100),
        (2, 0) => (0, 50002700, 50010200, 50060150), // Dominion: tank top
        (2, _) => (0, 50001454, 50010100, 50060100),
        _ => (50111700, 50000950, 50068550, 50060350), // DEM jacket set
    };
    let mut slots = [0; 14];
    slots[3] = face_acce;
    slots[5] = top;
    slots[6] = bottom;
    slots[10] = shoes;
    slots
}

fn actor_packet(character: &Character, stats: &InitialStats, actor_id: u32) -> Vec<u8> {
    let mut data = Vec::with_capacity(320);
    // CharaInfo fields follow MapServer/Packets/CharaInfo.cs; the 0x01ff tail
    // below follows the current client's reader where its order differs.
    // ServerCharaID identifies this map-session actor; CharaID remains durable.
    put_u32(&mut data, actor_id);
    put_u32(&mut data, character.id);
    put_u32(&mut data, 1); // Unk1
    data.push(0); // Unk
    put_tstr(&mut data, &character.name);
    data.push(character.race);
    data.push(0); // Form
    data.push(character.sex);
    put_u16(&mut data, character.hair);
    data.push(character.hair_color);
    put_u16(&mut data, 255); // Wig
    data.push(255); // Extra
    put_u16(&mut data, character.face);
    data.extend_from_slice(&[0, 0, 0, 0]); // RebirthLv, Ex, Wing, WingColor
    put_u32(&mut data, character.map_id);
    data.extend_from_slice(&[character.x, character.y, character.direction]);
    put_u32(&mut data, stats.max_hp); // HP starts at MaxHP on intended new-character path
    put_u32(&mut data, stats.max_hp);
    put_u32(&mut data, stats.max_mp);
    put_u32(&mut data, stats.max_mp);
    put_u32(&mut data, stats.max_sp);
    put_u32(&mut data, stats.max_sp);
    put_u32(&mut data, stats.max_ep);
    put_u32(&mut data, stats.max_ep);
    put_u16(&mut data, 9); // Unknown1
    put_u16_array(
        &mut data,
        &[
            stats.str_, stats.dex, stats.int, stats.vit, stats.agi, stats.mag, 0, 0,
        ],
    );
    put_u16_array(&mut data, &[0; 20]); // Unknown2
    put_u16_array(&mut data, &[]); // Unknown3
    put_u16_array(&mut data, &[]); // Unknown4
    put_u32(&mut data, u32::MAX); // HyouiTarget
    data.push(255); // HyouiPart
    put_u64(&mut data, 0); // Gold
    put_u64(&mut data, 0); // Unknown6
    data.push(0); // Unknown7
    put_u32_array(
        &mut data,
        &starter_appearance(character.race, character.sex),
    );
    put_u16_array(&mut data, &[0; 3]); // LeftHandMotion
    put_u16_array(&mut data, &[0; 3]); // RightHandMotion
    put_u16_array(&mut data, &[0; 3]); // RidingMotion
    put_u32(&mut data, 0); // RidePetID
    data.push(0); // RidePetColor
    put_u32(&mut data, 0); // Range
    put_u32(&mut data, 0); // Unknown8
    put_u32(&mut data, 2); // Mode1, matching the preceding CharaModeChange packet
    put_u32(&mut data, 0); // Mode2
                           // The current client's 0x01ff Mode2 reader consumes Unknown9 and Guest as
                           // bytes, then the two u32s, then an anonymous byte (observed at 0x8239fb..0x823ad2).
    data.extend_from_slice(&[0, 0]); // Unknown9, Guest
    put_u32(&mut data, 0); // Unknown10
    put_u32(&mut data, 1); // Unknown11
    data.push(0); // Anonymous byte; semantics remain unknown
    put_u32(&mut data, 0x0d); // Unknown13
    put_u16(&mut data, 0); // ShowTinyIcon
    put_u64(&mut data, 0); // DailyDungeonCleared
    put_u32_array(&mut data, &[0; 4]); // Unknown_
    packet(0x01ff, &data)
}

fn vital_packet(opcode: u16, character_id: u32, stats: &InitialStats) -> Vec<u8> {
    let mut data = Vec::with_capacity(33);
    put_u32(&mut data, character_id);
    data.push(3);
    // Fresh characters start at their source-derived maxima. The upstream 021C
    // builder also uses MaxEp for its last field rather than current Ep.
    put_u64(&mut data, stats.max_hp as u64);
    put_u64(&mut data, stats.max_mp as u64);
    put_u64(&mut data, stats.max_sp as u64);
    put_u32(&mut data, stats.max_ep);
    packet(opcode, &data)
}

fn move_speed(character_id: u32, speed: u16) -> Vec<u8> {
    let mut data = Vec::with_capacity(6);
    put_u32(&mut data, character_id);
    put_u16(&mut data, speed);
    packet(0x1239, &data)
}

fn detailed_status_one(stats: &InitialStats) -> Vec<u8> {
    let mut data = Vec::with_capacity(51);
    let base = [
        stats.str_, stats.dex, stats.int, stats.vit, stats.agi, stats.mag, 0, 0,
    ];
    put_u16_array(&mut data, &base);
    put_u16_array(&mut data, &[0; 8]); // ReviseStatus
    put_u16_array(&mut data, &[0; 8]); // BounusStatus
    packet(0x0212, &data)
}

fn detailed_status_two() -> Vec<u8> {
    let mut data = Vec::with_capacity(39);
    data.push(0x13);
    put_u16(&mut data, 410);
    for _ in 0..18 {
        put_u16(&mut data, 0);
    }
    packet(0x0217, &data)
}

fn elements() -> Vec<u8> {
    let mut data = vec![0; 30];
    data[0] = 7;
    data[15] = 7;
    packet(0x0223, &data)
}

fn capacity() -> Vec<u8> {
    let mut data = Vec::with_capacity(16);
    put_u32(&mut data, 0);
    put_u32(&mut data, 0);
    put_u32(&mut data, 2670);
    put_u32(&mut data, 7110);
    packet(0x0230, &data)
}

fn put_tstr(out: &mut Vec<u8>, value: &str) {
    let bytes = value.as_bytes();
    out.push((bytes.len() + 1) as u8);
    out.extend_from_slice(bytes);
    out.push(0);
}

fn put_u16_array(out: &mut Vec<u8>, values: &[u16]) {
    out.push(values.len() as u8);
    for value in values {
        put_u16(out, *value);
    }
}

fn put_u32_array(out: &mut Vec<u8>, values: &[u32]) {
    out.push(values.len() as u8);
    for value in values {
        put_u32(out, *value);
    }
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct PacketCursor<'a> {
        bytes: &'a [u8],
        offset: usize,
    }

    impl PacketCursor<'_> {
        fn take(&mut self, len: usize) -> &[u8] {
            let start = self.offset;
            self.offset += len;
            &self.bytes[start..self.offset]
        }

        fn u8(&mut self) -> u8 {
            self.take(1)[0]
        }

        fn u16(&mut self) -> u16 {
            u16::from_be_bytes(self.take(2).try_into().unwrap())
        }

        fn u32(&mut self) -> u32 {
            u32::from_be_bytes(self.take(4).try_into().unwrap())
        }

        fn u64(&mut self) -> u64 {
            u64::from_be_bytes(self.take(8).try_into().unwrap())
        }

        fn skip_u16_array(&mut self) {
            let count = self.u8() as usize;
            self.take(count * 2);
        }

        fn skip_u32_array(&mut self) {
            let count = self.u8() as usize;
            self.take(count * 4);
        }

        fn skip_u8_array(&mut self) {
            let count = self.u8() as usize;
            self.take(count);
        }

        fn skip_actor_prefix(&mut self) {
            self.take(12); // ServerCharaID, CharaID, Unk1
            self.take(1); // Unk
            let name_len = self.u8() as usize; // TSTR length includes NUL
            self.take(name_len);
            self.take(3); // Race, Form, Sex
            self.take(2); // Hair
            self.take(1); // HairColor
            self.take(2); // Wig
            self.take(1); // Extra
            self.take(2); // Face
            self.take(4); // RebirthLv, Ex, Wing, WingColor
            self.take(4); // MapID
            self.take(3); // X, Y, Dir
            self.take(8 * 4); // Current/max HP, MP, SP, EP
            self.take(2); // Unknown1
            for _ in 0..3 {
                self.skip_u16_array();
            }
            self.skip_u8_array(); // Fourth variable array uses the native byte-array reader
            self.take(4); // HyouiTarget
            self.take(1); // HyouiPart
            self.take(8); // Gold
            self.take(8); // Unknown6
            self.take(1); // Unknown7
            self.skip_u32_array(); // Equipment
            for _ in 0..3 {
                self.skip_u16_array(); // Left/right/riding motion
            }
            self.take(4); // RidePetID
            self.take(1); // RidePetColor
            self.take(16); // Range, Unknown8, Mode1, Mode2
        }
    }

    fn character(race: u8) -> Character {
        Character {
            id: 0x0102_0304,
            account_id: 1,
            slot: 0,
            name: "miao".into(),
            race,
            sex: 1,
            hair: 0x1234,
            hair_color: 51,
            face: 3,
            map_id: 20_013_003,
            x: 100,
            y: 101,
            direction: 2,
        }
    }

    #[test]
    fn actor_packet_uses_source_field_order_counts_and_endianness() {
        let actor = actor_packet(&character(0), &initial_stats(0).unwrap(), PLAYER_ACTOR_ID);
        assert_eq!(u16::from_be_bytes([actor[2], actor[3]]), 0x01ff);
        assert_eq!(&actor[4..8], &PLAYER_ACTOR_ID.to_be_bytes()); // ServerCharaID
        assert_eq!(&actor[8..12], &0x0102_0304u32.to_be_bytes()); // CharaID
        assert_eq!(&actor[12..16], &1u32.to_be_bytes());
        assert_eq!(actor[16], 0); // Unk
        assert_eq!(&actor[17..23], &[5, b'm', b'i', b'a', b'o', 0]); // TSTR
                                                                     // Race appearance and source array count prefixes follow Name in CharaInfo order.
        assert_eq!(
            &actor[23..34],
            &[0, 0, 1, 0x12, 0x34, 51, 0, 255, 255, 0, 3]
        );
        assert_eq!(actor[79], 8); // Status array count
        assert_eq!(actor[96], 20); // Unknown2 array count
        assert_eq!(*actor.last().unwrap(), 0); // final byte of Unknown_ array
        assert_eq!(actor.len(), 302);
        assert_eq!(&actor[252..256], &2u32.to_be_bytes()); // Mode1
        assert_eq!(&actor[256..260], &0u32.to_be_bytes()); // Mode2
        assert_eq!(
            &actor[80..96],
            &[0, 8, 0, 3, 0, 3, 0, 10, 0, 4, 0, 3, 0, 0, 0, 0]
        );
    }

    #[test]
    fn actor_packet_tail_matches_native_client_mode2_reader_order() {
        for name in ["x", "longer-name"] {
            let mut actor_character = character(0);
            actor_character.name = name.into();
            let actor = actor_packet(
                &actor_character,
                &initial_stats(0).unwrap(),
                PLAYER_ACTOR_ID,
            );
            let mut cursor = PacketCursor {
                bytes: &actor[4..], // skip packet length and opcode
                offset: 0,
            };
            cursor.skip_actor_prefix();

            let unknown9 = cursor.u8();
            let guest = cursor.u8();
            let unknown10 = cursor.u32();
            let unknown11 = cursor.u32();
            let anonymous_byte = cursor.u8();
            let unknown13 = cursor.u32();
            let show_tiny_icon = cursor.u16();
            let daily_dungeon_cleared = cursor.u64();
            let unknown_count = cursor.u8() as usize;
            let unknown_values = (0..unknown_count).map(|_| cursor.u32()).collect::<Vec<_>>();

            assert_eq!((unknown9, guest), (0, 0));
            assert_eq!(unknown10, 0);
            assert_eq!(unknown11, 1);
            assert_eq!(anonymous_byte, 0);
            assert_eq!(unknown13, 13);
            assert_eq!(show_tiny_icon, 0);
            assert_eq!(daily_dungeon_cleared, 0);
            assert_eq!(unknown_values, [0; 4]);
            assert_eq!(cursor.offset, cursor.bytes.len());
        }
    }

    #[test]
    fn bootstrap_separates_session_actor_id_from_saved_character_id() {
        let mut saved = character(0);
        saved.id = 1;
        let packets = bootstrap(&saved).unwrap();
        let payload = |opcode: u16| {
            packets
                .iter()
                .find(|packet| u16::from_be_bytes([packet[2], packet[3]]) == opcode)
                .unwrap()
        };
        assert_eq!(
            saved.id, 1,
            "wire identity must not mutate persisted identity"
        );
        assert_eq!(&payload(0x1239)[4..8], &16u32.to_be_bytes());
        assert_eq!(&payload(0x0fa7)[4..8], &16u32.to_be_bytes());
        let actor = payload(0x01ff);
        assert_eq!(&actor[4..8], &16u32.to_be_bytes());
        assert_eq!(&actor[8..12], &1u32.to_be_bytes());
        for opcode in [0x0221, 0x021c, 0x157c, 0x1b67] {
            assert_eq!(
                &payload(opcode)[4..8],
                &16u32.to_be_bytes(),
                "opcode {opcode:04x}"
            );
        }
    }

    #[test]
    fn bootstrap_emits_source_startup_sequence_and_initial_race_stats() {
        let packets = bootstrap(&character(0)).unwrap();
        let opcodes = packets
            .iter()
            .map(|p| u16::from_be_bytes([p[2], p[3]]))
            .collect::<Vec<_>>();
        assert_eq!(
            opcodes,
            [
                0x1f72, 0x026a, 0x1239, 0x0fa7, 0x1a5f, 0x0203, 0x01ff, 0x0221, 0x021c, 0x157c,
                0x0212, 0x0217, 0x0223, 0x0230, 0x1b67
            ]
        );
        assert_eq!(&packets[2][4..], &[0, 0, 0, 16, 0, 10]);
        assert_eq!(&packets[3][8..12], &2u32.to_be_bytes());
        assert_eq!(&packets[3][12..16], &0u32.to_be_bytes());
        assert_eq!(&packets[6][252..260], &packets[3][8..16]);
        assert_eq!(&packets[7][9..17], &86u64.to_be_bytes());
        assert_eq!(&packets[7][17..25], &40u64.to_be_bytes()); // SagaECO CalcMaxMP, Emil
        assert_eq!(&packets[7][25..33], &34u64.to_be_bytes()); // CalcMaxSP
        assert_eq!(&packets[7][33..37], &30u32.to_be_bytes());
        assert_eq!(
            &packets[10][5..21],
            &packets[6][80..96],
            "01FF actor base stats and 0212 BaseStatus must agree"
        );
    }

    #[test]
    fn race_max_hp_values_follow_source_level_one_formula() {
        assert_eq!(initial_stats(0).unwrap().max_hp, 86);
        assert_eq!(initial_stats(1).unwrap().max_hp, 64);
        assert_eq!(initial_stats(2).unwrap().max_hp, 55);
        assert_eq!(initial_stats(3).unwrap().max_hp, 68);
        assert_eq!(
            initial_stats(4).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn interaction_replies_follow_source_layouts() {
        assert_eq!(battle_status(16, 1), packet(0x0fa6, &[0, 0, 0, 16, 1]));
        assert_eq!(
            motion(16, 0x0083, 1),
            packet(0x121c, &[0, 0, 0, 16, 0, 0x83, 1, 0])
        );
        assert_eq!(logout(0), packet(0x0020, &[0]));
    }

    #[test]
    fn chat_system_message_and_teleport_layouts() {
        assert_eq!(
            chat(16, "hi"),
            packet(0x03e9, &[0, 0, 0, 16, 3, b'h', b'i', 0])
        );
        assert_eq!(&system_message("x")[4..8], &[0xff; 4]);
        assert_eq!(
            teleport(16, 250, -1150, 90),
            packet(0x11f9, &[0, 0, 0, 16, 0, 250, 0xfb, 0x82, 0, 90, 0, 14])
        );
    }

    #[test]
    fn starter_clothes_fill_top_bottom_and_shoes() {
        let emil_female = starter_appearance(0, 1);
        assert_eq!(
            (emil_female[5], emil_female[6], emil_female[10]),
            (50000000, 50010300, 50060100)
        );
        assert_eq!(starter_appearance(3, 1)[3], 50111700, "DEM face accessory");
        assert_eq!(emil_female.iter().filter(|&&id| id != 0).count(), 3);
    }

    #[test]
    fn level_job_and_exp_layouts() {
        assert_eq!(
            player_level(1, 1, 3, 0),
            packet(0x023a, &[1, 1, 0, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0])
        );
        assert_eq!(player_job(0), packet(0x0244, &[0; 8]));
        let exp = player_exp(0, 0, 0, 0);
        assert_eq!(u16::from_be_bytes([exp[2], exp[3]]), 0x0235);
        assert_eq!(exp.len(), 4 + 32);
    }

    #[test]
    fn unavailable_event_opens_shows_and_closes_one_message() {
        let replies = event_unavailable(11_000_155);
        let opcodes = replies
            .iter()
            .map(|p| u16::from_be_bytes([p[2], p[3]]))
            .collect::<Vec<_>>();
        assert_eq!(opcodes, [0x05dc, 0x05e8, 0x03f8, 0x03f7, 0x03f9, 0x05dd]);
        assert_eq!(&replies[1][4..], &[0, 0xa7, 0xd9, 0x5b, 0, 0, 0, 0]);
        let message = &replies[3][4..];
        assert_eq!(&message[..5], &[0, 0xa7, 0xd9, 0x5b, 1]);
        let text = "This isn't available offline yet.";
        assert_eq!(message[5] as usize, text.len() + 1);
        assert_eq!(&message[6..6 + text.len()], text.as_bytes());
        let tail = &message[6 + text.len()..];
        assert_eq!(tail, &[0, 0, 131, 1, 0]);
    }

    #[test]
    fn loaded_unlock_packet_matches_request_move_response() {
        assert_eq!(
            loaded(PLAYER_ACTOR_ID),
            vec![packet(0x1239, &[0, 0, 0, 16, 1, 164])]
        );
    }
}
