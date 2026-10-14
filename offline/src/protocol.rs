use crate::diagnostics::{
    event, failure, friend_init_id_shape, protocol_failure, unsupported, Event, Failure,
    ProtocolStage,
};
use crate::store::{Character, CreateError, NewCharacter, Store};
use crate::terrain::Maps;
use aes::Aes128;
use cipher::{generic_array::GenericArray, BlockDecrypt, BlockEncrypt, KeyInit};
use num_bigint::BigUint;
use num_traits::Num;
use rand::{rngs::OsRng, RngCore};
use sha1::{Digest, Sha1};
use std::{
    cell::RefCell,
    io,
    rc::Rc,
    time::{Duration, Instant},
};

/// Movement is kept in memory and written at most this often. Logout, re-login, map
/// entry and disconnect always write it, so only a killed server can lose these seconds.
const POSITION_SAVE_INTERVAL: Duration = Duration::from_secs(5);

pub const MAX_PACKET_SIZE: usize = 64 * 1024;
pub(super) const INIT_PACKET: [u8; 8] = [0, 0, 0, 0, 0, 0, 0, 0x10];
pub(super) const PRIME_DECIMAL: &str = "175012832246148469004952309893923119007504294868274830650101802243580016468616226644476369579140157420542034349400995694097261371077961674039236035533383172308367706779425637041402045013194820474112524204508905916696893254410707373670063475235242589213472899328698912258375583335003993274863729669402122894589";
const FIRST_ACK_PAYLOAD: [u8; 20] = [
    0xe8, 0x6a, 0x6a, 0xca, 0xdc, 0xe8, 0x06, 0x05, 0x2b, 0x29, 0xf8, 0x96, 0x2f, 0x86, 0x7c, 0xab,
    0x2a, 0x57, 0xad, 0x30,
];
const LOGIN_SERVER_NAME: &str = "Clover";
const LOGIN_SERVER_ADDRESS: &str = "127.0.0.1:17832";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerRole {
    Gate,
    Login,
    Map,
}

pub struct Connection {
    phase: Phase,
    server_private: BigUint,
    aes_key: Option<[u8; 16]>,
    store: Option<Rc<RefCell<Store>>>,
    account_id: Option<u32>,
    salts: Option<(u32, u32)>,
    role: ServerRole,
    should_close: bool,
    character: Option<Character>,
    selected_character_id: Option<u32>,
    map_loaded: bool,
    maps: Option<Rc<Maps>>,
    position_dirty: bool,
    position_saved_at: Option<Instant>,
    cheats: crate::commands::Cheats,
}

enum Phase {
    Initial,
    ClientKey,
    Ready,
}

impl Drop for Connection {
    // Disconnects, including a crashed or closed client, still keep the last position.
    fn drop(&mut self) {
        let _ = self.save_position();
    }
}

impl Connection {
    pub fn new() -> Self {
        let mut secret = [0u8; 64];
        OsRng.fill_bytes(&mut secret);
        secret[63] = 0;
        Self {
            phase: Phase::Initial,
            server_private: BigUint::from_bytes_le(&secret),
            aes_key: None,
            store: None,
            account_id: None,
            salts: None,
            role: ServerRole::Login,
            should_close: false,
            character: None,
            selected_character_id: None,
            map_loaded: false,
            maps: None,
            position_dirty: false,
            position_saved_at: None,
            cheats: crate::commands::Cheats::default(),
        }
    }

    #[cfg(test)]
    pub fn with_store(store: Rc<RefCell<Store>>) -> Self {
        Self::with_role(store, ServerRole::Login, None)
    }

    pub fn with_role(store: Rc<RefCell<Store>>, role: ServerRole, maps: Option<Rc<Maps>>) -> Self {
        let mut connection = Self::new();
        connection.store = Some(store);
        connection.role = role;
        connection.maps = maps;
        connection
    }

    pub fn should_close(&self) -> bool {
        self.should_close
    }

    pub fn next_input_size(&self, buffered: &[u8]) -> io::Result<Option<usize>> {
        (|| match &self.phase {
            Phase::Initial => Ok((buffered.len() >= 8).then_some(8)),
            Phase::ClientKey => Ok((buffered.len() >= 260).then_some(260)),
            Phase::Ready => {
                if buffered.len() < 8 {
                    return Ok(None);
                }
                let encrypted_len = u32::from_be_bytes(buffered[..4].try_into().unwrap()) as usize;
                let original_len = u32::from_be_bytes(buffered[4..8].try_into().unwrap()) as usize;
                if encrypted_len == 0
                    || encrypted_len % 16 != 0
                    || encrypted_len > MAX_PACKET_SIZE
                    || original_len < 4
                    || original_len > encrypted_len
                    || !padding_accepted(original_len, encrypted_len)
                {
                    return Err(invalid("invalid encrypted frame lengths"));
                }
                Ok(Some(8 + encrypted_len))
            }
        })()
        .map_err(|error| {
            protocol_failure(ProtocolStage::FrameHeader, None);
            error
        })
    }

    pub fn receive(&mut self, frame: &[u8]) -> io::Result<Option<Vec<u8>>> {
        match &mut self.phase {
            Phase::Initial => {
                if frame != INIT_PACKET {
                    protocol_failure(ProtocolStage::Receive, None);
                    return Err(invalid("expected 8-byte key-exchange init"));
                }
                let reply = server_key_packet(&self.server_private);
                self.phase = Phase::ClientKey;
                Ok(Some(reply))
            }
            Phase::ClientKey => {
                if frame.len() != 260 {
                    protocol_failure(ProtocolStage::Receive, None);
                    return Err(invalid("client key packet must be 260 bytes"));
                }
                let key = derive_aes_key(frame, &self.server_private).map_err(|error| {
                    protocol_failure(ProtocolStage::Receive, None);
                    error
                })?;
                self.aes_key = Some(key);
                self.phase = Phase::Ready;
                event(Event::HandshakeComplete);
                Ok(None)
            }
            Phase::Ready => {
                let (plaintext, original_len) =
                    decrypt_frame(frame, self.aes_key.as_ref().unwrap())?;
                if plaintext.len() != original_len {
                    protocol_failure(ProtocolStage::InnerPacket, None);
                    return Err(invalid("decrypted packet length mismatch"));
                }
                let packets = split_inner_packets(&plaintext).map_err(|error| {
                    protocol_failure(ProtocolStage::InnerPacket, None);
                    error
                })?;
                if packets.len() > 1 {
                    event(Event::BatchedFrameAccepted);
                }
                let mut replies = None;
                for packet in packets {
                    let opcode = u16::from_be_bytes([packet[2], packet[3]]);
                    let response = self.dispatch_packet(packet).map_err(|error| {
                        protocol_failure(ProtocolStage::Dispatch, Some(opcode));
                        error
                    })?;
                    if let Some(response) = response {
                        replies.get_or_insert_with(Vec::new).extend(response);
                    }
                    if self.should_close {
                        break;
                    }
                }
                Ok(replies)
            }
        }
    }

    fn dispatch_packet(&mut self, packet: &[u8]) -> io::Result<Option<Vec<u8>>> {
        crate::trace::packet("C>S", packet);
        let opcode = u16::from_be_bytes([packet[2], packet[3]]);
        let payload = &packet[4..];
        match opcode {
            0x0001 if self.role != ServerRole::Map => {
                let mut replies = Vec::new();
                replies.extend(encrypt_frame(
                    &make_packet(0xffff, &FIRST_ACK_PAYLOAD),
                    self.aes_key.as_ref().unwrap(),
                )?);
                let mut salts = [0u8; 8];
                OsRng.fill_bytes(&mut salts);
                salts[0] = 0;
                salts[4] = 0;
                self.salts = Some((
                    u32::from_be_bytes(salts[..4].try_into().unwrap()),
                    u32::from_be_bytes(salts[4..].try_into().unwrap()),
                ));
                replies.extend(encrypt_frame(
                    &make_packet(0x001e, &salts),
                    self.aes_key.as_ref().unwrap(),
                )?);
                replies.extend(encrypt_frame(
                    &make_packet(0x0002, payload),
                    self.aes_key.as_ref().unwrap(),
                )?);
                Ok(Some(replies))
            }
            0x000a if self.role == ServerRole::Map => self.map_connection_init(payload),
            0x000a => Ok(Some(encrypt_frame(
                &make_packet(0x000b, payload),
                self.aes_key.as_ref().unwrap(),
            )?)),
            0x001f if self.role != ServerRole::Map => self.login(payload),
            0x0010 if self.role == ServerRole::Map => self.login(payload),
            0x002f => {
                if self.role != ServerRole::Gate {
                    unsupported(opcode);
                    return Ok(None);
                }
                event(Event::LoginStatusSent);
                let status: i32 = if self.account_id.is_some() { 0 } else { -1 };
                Ok(Some(encrypt_frame(
                    &make_packet(0x0030, &status.to_be_bytes()),
                    self.aes_key.as_ref().unwrap(),
                )?))
            }
            0x0031 if self.role == ServerRole::Gate => self.request_server_list(payload),
            0x00a0 if self.role == ServerRole::Login => self.create_character(payload),
            0x00a7 if self.role == ServerRole::Login => self.request_map_id(payload),
            0x002a if self.role == ServerRole::Login => self.friend_mail_ready(payload),
            0x0032 if self.role == ServerRole::Login => self.request_map_server(payload),
            0x0032 if self.role == ServerRole::Map => {
                self.send_packets(&[make_packet(0x0033, &[])])
            }
            0x01fd if self.role == ServerRole::Map => self.enter_map(payload),
            0x11fe if self.role == ServerRole::Map => self.map_loaded(payload),
            0x11f8 if self.role == ServerRole::Map => self.move_player(payload),
            // The reference receives this notification without sending a reply.
            // 2017 emulator notes list level, EXP and job among the packets sent once the map
            // has finished loading (13C7); without them the client shows level 0.
            // shortcut: level 1 novice until saved progress is read (restoration plan phase 0).
            0x13c7 if self.role == ServerRole::Map && self.character.is_some() => self
                .send_packets(&[
                    crate::map::player_level(1, 1, 0, 0),
                    crate::map::player_job(0),
                    crate::map::player_exp(0, 0, 0, 0),
                ]),
            0x03e8 if self.role == ServerRole::Map && self.map_loaded => self.chat(payload),
            0x0258 if self.role == ServerRole::Map && self.character.is_some() => {
                self.stats_preview(payload)
            }
            0x0fa5 if self.role == ServerRole::Map && self.map_loaded => {
                let status = *payload
                    .first()
                    .ok_or_else(|| invalid("battle status requires a status byte"))?;
                self.send_packets(&[crate::map::battle_status(
                    crate::map::PLAYER_ACTOR_ID,
                    status,
                )])
            }
            0x121b if self.role == ServerRole::Map && self.map_loaded => {
                if payload.len() < 3 {
                    return Err(invalid("motion requires a motion and loop flag"));
                }
                let motion = u16::from_be_bytes([payload[0], payload[1]]);
                self.send_packets(&[crate::map::motion(
                    crate::map::PLAYER_ACTOR_ID,
                    motion,
                    payload[2],
                )])
            }
            0x05e6 if self.role == ServerRole::Map && self.map_loaded => {
                if payload.len() < 4 {
                    return Err(invalid("event start requires an event ID"));
                }
                let event_id = u32::from_be_bytes(payload[..4].try_into().unwrap());
                self.send_packets(&crate::map::event_unavailable(event_id))
            }
            0x001f if self.role == ServerRole::Map && self.character.is_some() => {
                let result = payload.first().copied().unwrap_or(0);
                self.save_position()?;
                self.send_packets(&[crate::map::logout(result)])
            }
            other => {
                unsupported(other);
                Ok(None)
            }
        }
    }

    fn login(&mut self, payload: &[u8]) -> io::Result<Option<Vec<u8>>> {
        self.save_position()?;
        self.account_id = None;
        self.character = None;
        self.selected_character_id = None;
        self.map_loaded = false;
        let (username, cursor) = read_tstr(payload, 0)?;
        let (password, cursor) = read_tstr(payload, cursor)?;
        let mac_len = *payload
            .get(cursor)
            .ok_or_else(|| invalid("missing login MAC length"))? as usize;
        let sso_offset = cursor
            .checked_add(1 + mac_len)
            .ok_or_else(|| invalid("login length overflow"))?;
        if payload.len() != sso_offset + 4 {
            return Err(invalid("invalid login payload length"));
        }
        let Some(store) = &self.store else {
            return Err(invalid("login requires configured local store"));
        };
        let result = match store
            .borrow()
            .account(&username)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?
        {
            None => 0xffff_fffeu32,
            Some(account) => {
                let Some((front, back)) = self.salts else {
                    return Err(invalid("login received before salt exchange"));
                };
                let candidate = salted_password_hash(front, &account.password_token, back);
                if password != candidate {
                    0xffff_fffd
                } else {
                    event(Event::LoginAccepted);
                    self.account_id = Some(account.id);
                    let mut replies = encrypt_frame(
                        &self.login_reply(0, account.id)?,
                        self.aes_key.as_ref().unwrap(),
                    )?;
                    if self.role == ServerRole::Login {
                        let chars = store
                            .borrow()
                            .characters(account.id)
                            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
                        replies.extend(encrypt_frame(
                            &make_packet(0x0028, &character_selection(&chars)),
                            self.aes_key.as_ref().unwrap(),
                        )?);
                        replies.extend(encrypt_frame(
                            &make_packet(0x0029, &equipment_selection(&chars)),
                            self.aes_key.as_ref().unwrap(),
                        )?);
                        event(Event::CharacterListSent);
                    }
                    return Ok(Some(replies));
                }
            }
        };
        event(Event::LoginRejected);
        Ok(Some(encrypt_frame(
            &self.login_reply(result, 0)?,
            self.aes_key.as_ref().unwrap(),
        )?))
    }

    fn login_reply(&self, result: u32, account_id: u32) -> io::Result<Vec<u8>> {
        if self.role != ServerRole::Map {
            return Ok(make_packet(0x0020, &login_result(result, account_id)));
        }
        let seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| invalid("system clock precedes protocol epoch"))?
            .as_secs();
        let timestamp = u32::try_from(seconds).map_err(|_| invalid("protocol time overflow"))?;
        let mut payload = result.to_be_bytes().to_vec();
        payload.extend_from_slice(&[1, 0]);
        payload.extend_from_slice(&timestamp.to_be_bytes());
        Ok(make_packet(0x0011, &payload))
    }

    fn send_packets(&self, packets: &[Vec<u8>]) -> io::Result<Option<Vec<u8>>> {
        let key = self
            .aes_key
            .as_ref()
            .ok_or_else(|| invalid("key exchange incomplete"))?;
        let mut replies = Vec::new();
        for packet in packets {
            replies.extend(encrypt_frame(packet, key)?);
        }
        Ok(Some(replies))
    }

    fn map_connection_init(&mut self, payload: &[u8]) -> io::Result<Option<Vec<u8>>> {
        self.save_position()?;
        self.account_id = None;
        self.character = None;
        self.selected_character_id = None;
        self.map_loaded = false;
        let mut salts = [0u8; 8];
        OsRng.fill_bytes(&mut salts);
        salts[0] = 0;
        salts[4] = 0;
        self.salts = Some((
            u32::from_be_bytes(salts[..4].try_into().unwrap()),
            u32::from_be_bytes(salts[4..].try_into().unwrap()),
        ));
        self.send_packets(&[
            make_packet(0xffff, &FIRST_ACK_PAYLOAD),
            make_packet(0x000b, payload),
            make_packet(0x000f, &salts),
        ])
    }

    fn request_map_server(&self, payload: &[u8]) -> io::Result<Option<Vec<u8>>> {
        let account_id = self
            .account_id
            .ok_or_else(|| invalid("map redirect requires authentication"))?;
        let map_id = u32::from_be_bytes(
            payload
                .try_into()
                .map_err(|_| invalid("invalid map redirect request"))?,
        );
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| invalid("map redirect requires store"))?;
        if !store
            .borrow()
            .characters(account_id)?
            .iter()
            .any(|ch| ch.map_id == map_id)
        {
            return Err(invalid("map redirect references unowned character map"));
        }
        let mut reply = vec![1];
        reply.extend(tstr_bytes("127.0.0.1")?);
        reply.extend_from_slice(&17833u32.to_be_bytes());
        event(Event::MapRedirectSent);
        self.send_packets(&[make_packet(0x0033, &reply)])
    }

    fn friend_mail_ready(&self, payload: &[u8]) -> io::Result<Option<Vec<u8>>> {
        let account_id = self
            .account_id
            .ok_or_else(|| invalid("friend initialization requires authentication"))?;
        let character_id = u32::from_be_bytes(payload.try_into().map_err(|_| {
            protocol_failure(ProtocolStage::FriendInitPayload, None);
            invalid("invalid friend initialization request")
        })?);
        let characters = self
            .store
            .as_ref()
            .ok_or_else(|| invalid("friend initialization requires store"))?
            .borrow()
            .characters(account_id)?;
        // Modern GetMailAndFriendList reads but ignores this reference; observed clients send zero.
        // Require a successful 0x00a7 selection for zero; do not infer a documented sentinel.
        let matches_owned_character = if character_id == 0 {
            self.selected_character_id
                .is_some_and(|selected_id| characters.iter().any(|ch| ch.id == selected_id))
        } else {
            characters.iter().any(|ch| ch.id == character_id)
        };
        friend_init_id_shape(
            character_id == 0,
            character_id == account_id,
            characters
                .iter()
                .any(|ch| u32::from(ch.slot) == character_id),
            characters.iter().any(|ch| ch.slot == payload[0]),
            payload[1..].iter().all(|byte| *byte == 0),
        );
        if !matches_owned_character {
            protocol_failure(ProtocolStage::FriendInitOwnership, None);
            return Err(invalid(
                "friend initialization references unowned character",
            ));
        }
        // Source 002A -> 002B, 00DD. 00DD is the player's own friend state (1 = online, as SagaECO
        // SSMG_CHAR_STATUS sends) plus an empty comment; 0 made the client ask to go online.
        self.send_packets(&[make_packet(0x002b, &[]), make_packet(0x00dd, &[1, 1, 0])])
    }

    fn enter_map(&mut self, payload: &[u8]) -> io::Result<Option<Vec<u8>>> {
        self.save_position()?;
        let account_id = self
            .account_id
            .ok_or_else(|| invalid("map entry requires authentication"))?;
        if payload.len() != 6 || payload[..4] != [0; 4] || payload[5] != 0 {
            return Err(invalid("unsupported map entry request"));
        }
        let mut character = self
            .store
            .as_ref()
            .ok_or_else(|| invalid("map entry requires store"))?
            .borrow()
            .characters(account_id)?
            .into_iter()
            .find(|ch| ch.slot == payload[4])
            .ok_or_else(|| invalid("map entry references empty slot"))?;
        // The reference's default (0,0) is blocked in MapInfo. Use its documented
        // P10000499 entrance (63,58), verified walkable, for this prototype placeholder only.
        let terrain = self
            .maps
            .as_ref()
            .and_then(|maps| maps.get(character.map_id))
            .ok_or_else(|| invalid("character map terrain unavailable"))?;
        let placeholder = character.x == 0 && character.y == 0;
        if placeholder {
            character.x = 63;
            character.y = 58;
        }
        if !terrain.passable(character.x, character.y) {
            return Err(invalid("saved position is blocked"));
        }
        if placeholder {
            self.store.as_ref().unwrap().borrow_mut().update_position(
                account_id,
                character.id,
                character.x,
                character.y,
                character.direction,
            )?;
        }
        let replies = self.send_packets(&crate::map::bootstrap(&character)?)?;
        self.character = Some(character);
        self.position_dirty = false;
        self.position_saved_at = Some(Instant::now());
        self.cheats = crate::commands::Cheats::default();
        self.map_loaded = false;
        event(Event::MapEntered);
        Ok(replies)
    }

    fn map_loaded(&mut self, payload: &[u8]) -> io::Result<Option<Vec<u8>>> {
        if !payload.is_empty() {
            return Err(invalid("invalid map-loaded request"));
        }
        let _character = self
            .character
            .as_ref()
            .ok_or_else(|| invalid("map load before character entry"))?;
        let reply = self.send_packets(&crate::map::loaded(crate::map::PLAYER_ACTOR_ID))?;
        self.map_loaded = true;
        event(Event::MapLoaded);
        Ok(reply)
    }

    fn move_player(&mut self, payload: &[u8]) -> io::Result<Option<Vec<u8>>> {
        if !self.map_loaded || payload.len() != 8 {
            return Err(invalid(
                "movement requires loaded map and an eight-byte payload",
            ));
        }
        let character = self
            .character
            .as_ref()
            .ok_or_else(|| invalid("movement before character entry"))?;
        let terrain = self
            .maps
            .as_ref()
            .and_then(|maps| maps.get(character.map_id))
            .ok_or_else(|| invalid("movement map dimensions are not configured"))?;
        let x = i16::from_be_bytes(payload[..2].try_into().unwrap());
        let y = i16::from_be_bytes(payload[2..4].try_into().unwrap());
        let direction = u16::from_be_bytes(payload[4..6].try_into().unwrap());
        let move_type = u16::from_be_bytes(payload[6..8].try_into().unwrap());
        if direction >= 360 || !matches!(move_type, 1 | 6 | 7) {
            return Err(invalid("invalid client movement mode or direction"));
        }
        // 100 world units per tile around the map centre, as SagaLib PosX16to8/PosY16to8.
        let (width, height) = terrain.dimensions();
        let tile_x = (i32::from(x) + i32::from(width) * 50 - 50).div_euclid(100);
        let tile_y = (i32::from(height) * 50 - 50 - i32::from(y)).div_euclid(100);
        if !(0..i32::from(width)).contains(&tile_x) || !(0..i32::from(height)).contains(&tile_y) {
            return Err(invalid("movement outside known map bounds"));
        }
        if !terrain.passable(tile_x as u8, tile_y as u8) {
            return Err(invalid("movement destination is blocked"));
        }
        // shortcut: destination tiles only; swept collision and speed enforcement need map simulation.
        let mut reply = crate::map::PLAYER_ACTOR_ID.to_be_bytes().to_vec();
        reply.extend_from_slice(payload);
        let character = self.character.as_mut().unwrap();
        character.x = tile_x as u8;
        character.y = tile_y as u8;
        character.direction = (direction / 45) as u8;
        self.position_dirty = true;
        if self
            .position_saved_at
            .is_none_or(|saved| saved.elapsed() >= POSITION_SAVE_INTERVAL)
        {
            self.save_position()?;
        }
        event(Event::PlayerMoved);
        self.send_packets(&[make_packet(0x11f9, &reply)])
    }

    /// Status window preview (0258): count byte, then STR/DEX/INT/VIT/AGI/MAG as u16.
    fn stats_preview(&mut self, payload: &[u8]) -> io::Result<Option<Vec<u8>>> {
        if payload.len() < 13 {
            return Err(invalid("stats preview requires six attributes"));
        }
        let at = |i: usize| u16::from_be_bytes([payload[1 + i * 2], payload[2 + i * 2]]);
        let race = self.character.as_ref().map_or(0, |c| c.race);
        // shortcut: level 1 novice until saved progress is read (restoration plan phase 0).
        let derived = crate::stats::derive(&crate::stats::CharacterBasics {
            race,
            level: 1,
            job_level: 1,
            job: 0,
            base: crate::stats::BaseStats {
                str_: at(0),
                dex: at(1),
                int: at(2),
                vit: at(3),
                agi: at(4),
                mag: at(5),
            },
        });
        self.send_packets(&[crate::map::stats_pre_calc(&derived)])
    }

    /// Public chat (03E8): test commands are handled here; other text is echoed to the log.
    fn chat(&mut self, payload: &[u8]) -> io::Result<Option<Vec<u8>>> {
        use crate::commands::{self, Command};
        use crate::map::system_message as system;
        // Length byte counts the trailing NUL.
        let len = *payload
            .first()
            .ok_or_else(|| invalid("chat requires text"))? as usize;
        let text = payload
            .get(1..len.max(1))
            .ok_or_else(|| invalid("chat text is truncated"))?;
        let text = String::from_utf8_lossy(text)
            .trim_end_matches('\0')
            .to_string();
        let replies = match commands::parse(&text) {
            None => vec![crate::map::chat(crate::map::PLAYER_ACTOR_ID, &text)],
            Some(Err(name)) => vec![system(&format!(
                "Unknown command /{name}. {}",
                commands::HELP
            ))],
            Some(Ok(Command::Help)) => vec![system(commands::HELP)],
            Some(Ok(Command::GodMode(toggle))) => {
                self.cheats.god_mode = toggle.unwrap_or(!self.cheats.god_mode);
                vec![system(if self.cheats.god_mode {
                    "God mode ON: you take no damage and deal maximum damage (applies once combat is added)."
                } else {
                    "God mode OFF."
                })]
            }
            Some(Ok(Command::Freebies(toggle))) => {
                self.cheats.freebies = toggle.unwrap_or(!self.cheats.freebies);
                vec![system(if self.cheats.freebies {
                    "Freebies ON: everything you buy is free (applies once shops are added)."
                } else {
                    "Freebies OFF."
                })]
            }
            // shortcut: no monster system yet (restoration plan phase 2).
            Some(Ok(Command::KillAll)) => {
                vec![system("No monsters are spawned on this map yet.")]
            }
            Some(Ok(Command::Unstuck)) => self.unstuck()?,
        };
        self.send_packets(&replies)
    }

    fn unstuck(&mut self) -> io::Result<Vec<Vec<u8>>> {
        let character = self
            .character
            .as_mut()
            .ok_or_else(|| invalid("unstuck before character entry"))?;
        let Some((x, y)) = crate::commands::safe_point(character.map_id) else {
            return Ok(vec![crate::map::system_message(
                "No safe point is known for this map yet.",
            )]);
        };
        let (width, height) = self
            .maps
            .as_ref()
            .and_then(|maps| maps.get(character.map_id))
            .ok_or_else(|| invalid("map terrain unavailable"))?
            .dimensions();
        // SagaLib PosX8to16/PosY8to16: tile centre in world units.
        let world_x = (i32::from(x) - i32::from(width) / 2) * 100 + 50;
        let world_y = -(i32::from(y) - i32::from(height) / 2) * 100 - 50;
        character.x = x;
        character.y = y;
        let direction = u16::from(character.direction) * 45;
        self.position_dirty = true;
        self.save_position()?;
        Ok(vec![
            crate::map::teleport(
                crate::map::PLAYER_ACTOR_ID,
                world_x as i16,
                world_y as i16,
                direction,
            ),
            crate::map::system_message("Teleported to this map's safe point."),
        ])
    }

    /// Writes the in-memory position if it changed since the last write.
    fn save_position(&mut self) -> io::Result<()> {
        if !self.position_dirty {
            return Ok(());
        }
        if let (Some(character), Some(store)) = (self.character.as_ref(), self.store.as_ref()) {
            store.borrow_mut().update_position(
                character.account_id,
                character.id,
                character.x,
                character.y,
                character.direction,
            )?;
        }
        self.position_dirty = false;
        self.position_saved_at = Some(Instant::now());
        Ok(())
    }

    fn create_character(&mut self, payload: &[u8]) -> io::Result<Option<Vec<u8>>> {
        let Some(account_id) = self.account_id else {
            return Err(invalid("character creation requires authentication"));
        };
        let slot = *payload
            .first()
            .ok_or_else(|| invalid("missing character slot"))?;
        let (name, cursor) = read_tstr(payload, 1)?;
        if payload.len() != cursor + 7 {
            return Err(invalid("invalid character creation payload length"));
        }
        let input = NewCharacter {
            slot,
            name,
            race: payload[cursor],
            sex: payload[cursor + 1],
            hair: u16::from_be_bytes([payload[cursor + 2], payload[cursor + 3]]),
            hair_color: payload[cursor + 4],
            face: u16::from_be_bytes([payload[cursor + 5], payload[cursor + 6]]),
        };
        let result: u32 = match self
            .store
            .as_ref()
            .ok_or_else(|| invalid("character creation requires configured local store"))?
            .borrow_mut()
            .create_character(account_id, input)
        {
            Ok(_) => 0,
            Err(CreateError::NameTooShort) => 0xffff_ff9f,
            Err(CreateError::NameConflict) => 0xffff_ff9e,
            Err(CreateError::SlotOccupied) => 0xffff_ff9d,
            Err(CreateError::NameTooLong) => 0xffff_ff9c,
            Err(CreateError::BadName) => 0xffff_ffa0,
            Err(CreateError::Storage(error)) => {
                failure(Failure::Storage, &error);
                0xffff_ff9b
            }
            Err(
                CreateError::InvalidSlot
                | CreateError::InvalidAppearance
                | CreateError::AccountNotFound,
            ) => 0xffff_ff9b,
        };
        event(if result == 0 {
            Event::CharacterCreated
        } else {
            Event::CharacterCreateRejected
        });
        Ok(Some(encrypt_frame(
            &make_packet(0x00a1, &result.to_be_bytes()),
            self.aes_key.as_ref().unwrap(),
        )?))
    }

    fn request_map_id(&mut self, payload: &[u8]) -> io::Result<Option<Vec<u8>>> {
        // A new selection attempt invalidates any prior choice, including a failed lookup.
        self.selected_character_id = None;
        let account_id = self
            .account_id
            .ok_or_else(|| invalid("map lookup requires authentication"))?;
        if payload.len() != 1 {
            return Err(invalid("invalid map lookup payload length"));
        }
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| invalid("map lookup requires configured local store"))?;
        let chars = store
            .borrow()
            .characters(account_id)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        let character = chars
            .iter()
            .find(|character| character.slot == payload[0])
            .ok_or_else(|| invalid("map lookup references empty slot"))?;
        let reply = encrypt_frame(
            &make_packet(0x00a8, &character.map_id.to_be_bytes()),
            self.aes_key.as_ref().unwrap(),
        )?;
        self.selected_character_id = Some(character.id);
        event(Event::MapIdSent);
        Ok(Some(reply))
    }

    fn request_server_list(&mut self, payload: &[u8]) -> io::Result<Option<Vec<u8>>> {
        if self.account_id.is_none() {
            return Err(invalid("server-list request requires authentication"));
        }
        if !payload.is_empty() {
            return Err(invalid("invalid server-list request payload"));
        }
        let mut replies = Vec::new();
        let server_name = tstr_bytes(LOGIN_SERVER_NAME)?;
        let server_address = tstr_bytes(LOGIN_SERVER_ADDRESS)?;
        let mut server_info = server_name;
        server_info.extend_from_slice(&server_address);
        let mut disconnect = 1i32.to_be_bytes().to_vec();
        disconnect.extend_from_slice(&[0, 0, 0]);
        for packet in [
            make_packet(0x0032, &1u32.to_be_bytes()),
            make_packet(0x0033, &server_info),
            make_packet(0x0034, &[]),
            make_packet(0x0036, &disconnect),
        ] {
            replies.extend(encrypt_frame(&packet, self.aes_key.as_ref().unwrap())?);
        }
        self.should_close = true;
        event(Event::ServerListSent);
        Ok(Some(replies))
    }
}

fn tstr_bytes(value: &str) -> io::Result<Vec<u8>> {
    let bytes = value.as_bytes();
    let length = bytes.len() + 1;
    if length > u8::MAX as usize {
        return Err(invalid("server-list string exceeds protocol limit"));
    }
    let mut result = Vec::with_capacity(length + 1);
    result.push(length as u8);
    result.extend_from_slice(bytes);
    result.push(0);
    Ok(result)
}

fn read_tstr(payload: &[u8], offset: usize) -> io::Result<(String, usize)> {
    let len = *payload
        .get(offset)
        .ok_or_else(|| invalid("missing TSTR length"))? as usize;
    if len == 0 {
        return Err(invalid("TSTR must include its NUL terminator"));
    }
    let end = offset
        .checked_add(1 + len)
        .ok_or_else(|| invalid("TSTR length overflow"))?;
    let bytes = payload
        .get(offset + 1..end)
        .ok_or_else(|| invalid("truncated TSTR"))?;
    if bytes[len - 1] != 0 || bytes[..len - 1].contains(&0) {
        return Err(invalid("invalid TSTR terminator"));
    }
    let value = std::str::from_utf8(&bytes[..len - 1])
        .map_err(|_| invalid("TSTR is not UTF-8"))?
        .to_owned();
    Ok((value, end))
}

fn salted_password_hash(front: u32, token: &str, back: u32) -> String {
    let mut sha = Sha1::new();
    // EcoServerEmulator uses Encoding.ASCII over the concatenated decimal salt/token string.
    let source_text = format!("{front}{token}{back}");
    let source_ascii: Vec<u8> = source_text
        .chars()
        .map(|character| {
            if character.is_ascii() {
                character as u8
            } else {
                b'?'
            }
        })
        .collect();
    sha.update(source_ascii);
    format!("{:x}", sha.finalize())
}

fn login_result(result: u32, user_id: u32) -> Vec<u8> {
    let mut payload = Vec::with_capacity(17);
    payload.extend_from_slice(&result.to_be_bytes());
    payload.extend_from_slice(&user_id.to_be_bytes());
    payload.extend_from_slice(&[0; 9]);
    payload
}

fn character_selection(chars: &[Character]) -> Vec<u8> {
    let by_slot = |slot: usize| chars.iter().find(|c| c.slot as usize == slot);
    let mut out = Vec::new();
    macro_rules! field {
        ($write:expr) => {{
            out.push(4);
            for slot in 0..4 {
                $write(by_slot(slot), &mut out);
            }
        }};
    }
    field!(|c: Option<&Character>, o: &mut Vec<u8>| {
        let name = c.map(|c| c.name.as_bytes()).unwrap_or(&[]);
        o.push(name.len() as u8);
        o.extend_from_slice(name);
    });
    field!(|c: Option<&Character>, o: &mut Vec<u8>| o.push(c.map_or(0, |c| c.race)));
    field!(|_: Option<&Character>, o: &mut Vec<u8>| o.push(0)); // Form
    field!(|c: Option<&Character>, o: &mut Vec<u8>| o.push(c.map_or(0, |c| c.sex)));
    field!(|c: Option<&Character>, o: &mut Vec<u8>| o
        .extend_from_slice(&c.map_or(0, |c| c.hair).to_be_bytes()));
    field!(|c: Option<&Character>, o: &mut Vec<u8>| o.push(c.map_or(0, |c| c.hair_color)));
    field!(|c: Option<&Character>, o: &mut Vec<u8>| o
        .extend_from_slice(&c.map_or(0, |_| 255u16).to_be_bytes())); // DB Wig default for stored characters
                                                                     // SagaECO SSMG_CHAR_DATA SetIfExist: 0xFF marks a slot holding a character, 0 a free slot
                                                                     // the client may offer for creation. The 2017 emulator sent its DB default 0 for both.
    field!(|c: Option<&Character>, o: &mut Vec<u8>| o.push(if c.is_some() { 0xff } else { 0 }));
    field!(|c: Option<&Character>, o: &mut Vec<u8>| o
        .extend_from_slice(&c.map_or(0, |c| c.face).to_be_bytes()));
    for _ in 0..5 {
        field!(|_: Option<&Character>, o: &mut Vec<u8>| o.push(0));
    } // rebirth, ex, wing, wing color, job
    field!(|c: Option<&Character>, o: &mut Vec<u8>| o
        .extend_from_slice(&c.map_or(0, |c| c.map_id).to_be_bytes()));
    for _ in 0..2 {
        // shortcut: level 1 until saved progress is read, as in the map server's 023A.
        field!(|c: Option<&Character>, o: &mut Vec<u8>| o.push(u8::from(c.is_some())));
    } // base/job1 levels
    field!(|c: Option<&Character>, o: &mut Vec<u8>| o
        .extend_from_slice(&c.map_or(0, |_| 5u16).to_be_bytes())); // DB quest default for stored characters
    for _ in 0..3 {
        field!(|_: Option<&Character>, o: &mut Vec<u8>| o.push(0));
    } // job2x/job2t/job3
    out
}

/// Character-select equipment (0029): per slot, 14 equipment image IDs in 01FF order.
fn equipment_selection(chars: &[Character]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 * 57);
    for slot in 0..4 {
        out.push(14);
        let images = chars
            .iter()
            .find(|c| c.slot as usize == slot)
            .map_or([0; 14], |c| crate::map::starter_appearance(c.race, c.sex));
        for image in images {
            out.extend_from_slice(&image.to_be_bytes());
        }
    }
    out
}

fn server_key_packet(private: &BigUint) -> Vec<u8> {
    let prime = BigUint::from_str_radix(PRIME_DECIMAL, 10).unwrap();
    let public = BigUint::from(3u8).modpow(private, &prime);
    let mut packet = vec![0, 0, 0, 0];
    append_sized_ascii_hex(&mut packet, "3");
    append_sized_ascii_hex(&mut packet, &format!("{prime:0>256x}"));
    append_sized_ascii_hex(&mut packet, &format!("{public:0>256x}"));
    packet
}

fn append_sized_ascii_hex(out: &mut Vec<u8>, text: &str) {
    out.extend_from_slice(&(text.len() as u32).to_be_bytes());
    out.extend_from_slice(text.as_bytes());
}

fn derive_aes_key(client_packet: &[u8], server_private: &BigUint) -> io::Result<[u8; 16]> {
    let public_len = u32::from_be_bytes(client_packet[..4].try_into().unwrap()) as usize;
    if public_len != 256 {
        return Err(invalid("invalid client public-key length"));
    }
    let public_hex = std::str::from_utf8(&client_packet[4..])
        .map_err(|_| invalid("client public key is not ASCII hex"))?;
    if public_hex.len() != 256 || !public_hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid(
            "client public key must contain 256 ASCII hex digits",
        ));
    }
    let client_public =
        BigUint::from_str_radix(public_hex, 16).map_err(|_| invalid("bad client public key"))?;
    let prime = BigUint::from_str_radix(PRIME_DECIMAL, 10).unwrap();
    if client_public < BigUint::from(2u8) || client_public > &prime - BigUint::from(2u8) {
        return Err(invalid("client public key is outside the DH group"));
    }
    let shared = client_public.modpow(server_private, &prime);
    aes_key_from_shared(&shared)
}

pub(super) fn aes_key_from_shared(shared: &BigUint) -> io::Result<[u8; 16]> {
    let shared_hex = format!("{shared:0>256x}");
    let mut key_hex = [0u8; 32];
    for (dst, src) in key_hex.iter_mut().zip(shared_hex.bytes().take(32)) {
        *dst = if (b'a'..=b'f').contains(&src) {
            src - 48
        } else {
            src
        };
    }
    let mut key = [0u8; 16];
    for (i, pair) in key_hex.chunks_exact(2).enumerate() {
        key[i] = (hex_value(pair[0])? << 4) | hex_value(pair[1])?;
    }
    Ok(key)
}

fn hex_value(value: u8) -> io::Result<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(invalid("invalid derived key hex")),
    }
}

pub(super) fn encrypt_frame(packet: &[u8], key: &[u8; 16]) -> io::Result<Vec<u8>> {
    if packet.len() < 4 || packet.len() > MAX_PACKET_SIZE || packet.len() > u16::MAX as usize + 2 {
        return Err(invalid("packet length outside supported bounds"));
    }
    let declared = u16::from_be_bytes([packet[0], packet[1]]) as usize + 2;
    if declared != packet.len() {
        return Err(invalid("inner packet length mismatch"));
    }
    crate::trace::packet("S>C", packet);
    let padded = padded_len(packet.len());
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let mut out = Vec::with_capacity(8 + padded);
    out.extend_from_slice(&(padded as u32).to_be_bytes());
    out.extend_from_slice(&(packet.len() as u32).to_be_bytes());
    let mut body = vec![0u8; padded];
    body[..packet.len()].copy_from_slice(packet);
    for block in body.chunks_exact_mut(16) {
        cipher.encrypt_block(GenericArray::from_mut_slice(block));
    }
    out.extend_from_slice(&body);
    Ok(out)
}

pub(super) fn decrypt_frame(frame: &[u8], key: &[u8; 16]) -> io::Result<(Vec<u8>, usize)> {
    if frame.len() < 24 {
        protocol_failure(ProtocolStage::FrameBody, None);
        return Err(invalid("encrypted frame is too short"));
    }
    let encrypted_len = u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize;
    let original_len = u32::from_be_bytes(frame[4..8].try_into().unwrap()) as usize;
    if encrypted_len + 8 != frame.len()
        || encrypted_len % 16 != 0
        || encrypted_len > MAX_PACKET_SIZE
        || original_len < 4
        || original_len > encrypted_len
        || !padding_accepted(original_len, encrypted_len)
    {
        protocol_failure(ProtocolStage::FrameBody, None);
        return Err(invalid("invalid encrypted frame"));
    }
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let mut body = frame[8..].to_vec();
    for block in body.chunks_exact_mut(16) {
        cipher.decrypt_block(GenericArray::from_mut_slice(block));
    }
    if body[original_len..].iter().any(|byte| *byte != 0) {
        protocol_failure(ProtocolStage::FramePadding, None);
        return Err(invalid("nonzero bytes after declared packet"));
    }
    body.truncate(original_len);
    Ok((body, original_len))
}

fn split_inner_packets(plaintext: &[u8]) -> io::Result<Vec<&[u8]>> {
    let mut packets = Vec::new();
    let mut offset = 0usize;
    while offset < plaintext.len() {
        let prefix = plaintext
            .get(offset..offset + 2)
            .ok_or_else(|| invalid("incomplete inner packet prefix"))?;
        let packet_len = u16::from_be_bytes([prefix[0], prefix[1]]) as usize + 2;
        if packet_len < 4 {
            return Err(invalid("inner packet shorter than header"));
        }
        let end = offset
            .checked_add(packet_len)
            .filter(|end| *end <= plaintext.len())
            .ok_or_else(|| invalid("inner packet exceeds plaintext"))?;
        packets.push(&plaintext[offset..end]);
        offset = end;
    }
    if packets.is_empty() {
        return Err(invalid("empty inner packet stream"));
    }
    // SagaLib NetIO.cs:586-617 iterates by each length prefix; MapClient.cs:1359 configures two bytes.
    Ok(packets)
}

pub(super) fn make_packet(opcode: u16, payload: &[u8]) -> Vec<u8> {
    let data_len = payload.len() + 2;
    let mut packet = Vec::with_capacity(data_len + 2);
    packet.extend_from_slice(&(data_len as u16).to_be_bytes());
    packet.extend_from_slice(&opcode.to_be_bytes());
    packet.extend_from_slice(payload);
    packet
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Frames we receive may carry the minimum zero padding (2017 emulator style) or, when the
/// length is already a multiple of 16, one whole extra block: the client pads with
/// `16 - len % 16` like SagaLib NetIO. Rejecting the latter dropped the connection.
fn padding_accepted(original_len: usize, encrypted_len: usize) -> bool {
    encrypted_len % 16 == 0 && encrypted_len >= original_len && encrypted_len - original_len <= 16
}

fn padded_len(length: usize) -> usize {
    length.div_ceil(16) * 16
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn ready_connection() -> (Connection, Rc<RefCell<Store>>, [u8; 16]) {
        let store = Rc::new(RefCell::new(Store::in_memory().unwrap()));
        let key = [0x35; 16];
        let mut connection = Connection::with_store(store.clone());
        connection.phase = Phase::Ready;
        connection.server_private = BigUint::from(1u8);
        connection.aes_key = Some(key);
        connection.salts = Some((12_345, 67_890));
        (connection, store, key)
    }

    fn unpack_replies(bytes: &[u8], key: &[u8; 16]) -> Vec<Vec<u8>> {
        let mut replies = Vec::new();
        let mut offset = 0;
        while offset < bytes.len() {
            let size =
                8 + u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
            replies.push(decrypt_frame(&bytes[offset..offset + size], key).unwrap().0);
            offset += size;
        }
        replies
    }

    fn encrypt_plaintext_frame(packet: &[u8], key: &[u8; 16]) -> Vec<u8> {
        let padded = padded_len(packet.len());
        let cipher = Aes128::new(GenericArray::from_slice(key));
        let mut frame = Vec::with_capacity(8 + padded);
        frame.extend_from_slice(&(padded as u32).to_be_bytes());
        frame.extend_from_slice(&(packet.len() as u32).to_be_bytes());
        let mut body = vec![0; padded];
        body[..packet.len()].copy_from_slice(packet);
        for block in body.chunks_exact_mut(16) {
            cipher.encrypt_block(GenericArray::from_mut_slice(block));
        }
        frame.extend_from_slice(&body);
        frame
    }

    #[test]
    fn accepts_client_full_block_padding_on_aligned_frames() {
        assert!(padding_accepted(16, 16));
        assert!(
            padding_accepted(16, 32),
            "client pads aligned frames with a whole block"
        );
        assert!(padding_accepted(17, 32));
        assert!(!padding_accepted(17, 48));
        assert!(!padding_accepted(16, 48));
        assert!(!padding_accepted(20, 16));

        let (mut connection, _, key) = ready_connection();
        connection.role = ServerRole::Map;
        // 0x000a with a 12-byte payload is exactly 16 bytes; send it in a 32-byte frame.
        let packet = make_packet(0x000a, &[0x12; 12]);
        assert_eq!(packet.len(), 16);
        let cipher = Aes128::new(GenericArray::from_slice(&key));
        let mut frame = 32u32.to_be_bytes().to_vec();
        frame.extend_from_slice(&16u32.to_be_bytes());
        let mut body = vec![0u8; 32];
        body[..16].copy_from_slice(&packet);
        for block in body.chunks_exact_mut(16) {
            cipher.encrypt_block(GenericArray::from_mut_slice(block));
        }
        frame.extend_from_slice(&body);
        assert_eq!(connection.next_input_size(&frame).unwrap(), Some(40));
        assert!(connection.receive(&frame).unwrap().is_some());
    }

    #[test]
    fn dispatches_multiple_length_prefixed_packets_from_one_encrypted_frame() {
        let (mut connection, _, key) = ready_connection();
        connection.role = ServerRole::Map;
        let mut plaintext = make_packet(0x000a, &[1]);
        plaintext.extend_from_slice(&make_packet(0x000a, &[2]));
        let frame = encrypt_plaintext_frame(&plaintext, &key);

        let response = connection.receive(&frame).unwrap().unwrap();
        assert_eq!(crate::diagnostics::take_batched_frame_events(), 1);
        let packets = unpack_replies(&response, &key);
        assert_eq!(
            packets
                .iter()
                .map(|packet| &packet[2..4])
                .collect::<Vec<_>>(),
            [
                &0xffffu16.to_be_bytes()[..],
                &0x000bu16.to_be_bytes()[..],
                &0x000fu16.to_be_bytes()[..],
                &0xffffu16.to_be_bytes()[..],
                &0x000bu16.to_be_bytes()[..],
                &0x000fu16.to_be_bytes()[..]
            ]
        );
        assert_eq!(&packets[1][4..], &[1]);
        assert_eq!(&packets[4][4..], &[2]);

        let (mut single, _, _) = ready_connection();
        single.role = ServerRole::Map;
        single
            .receive(&encrypt_frame(&make_packet(0x000a, &[3]), &key).unwrap())
            .unwrap();
        assert_eq!(crate::diagnostics::take_batched_frame_events(), 0);
    }

    #[test]
    fn malformed_batch_tail_is_rejected_before_first_packet_dispatch() {
        let key = [0x35; 16];
        let first = make_packet(0x000a, &[1]);
        for tail in [&[0u8][..], &[0, 0][..], &[0, 2, 9][..]] {
            let (mut connection, _, _) = ready_connection();
            connection.role = ServerRole::Map;
            connection.account_id = Some(42);
            connection.salts = Some((11, 22));
            let mut plaintext = first.clone();
            plaintext.extend_from_slice(tail);

            assert!(connection
                .receive(&encrypt_plaintext_frame(&plaintext, &key))
                .is_err());
            assert_eq!(connection.account_id, Some(42));
            assert_eq!(connection.salts, Some((11, 22)));
            assert_eq!(crate::diagnostics::take_batched_frame_events(), 0);
            assert_eq!(
                crate::diagnostics::take_protocol_failures(),
                vec![(ProtocolStage::InnerPacket, None)]
            );
        }
    }

    #[test]
    fn batched_dispatch_stops_after_connection_requests_close() {
        let (mut connection, _, key) = ready_connection();
        connection.role = ServerRole::Gate;
        connection.account_id = Some(1);
        let mut plaintext = make_packet(0x0031, &[]);
        plaintext.extend_from_slice(&make_packet(0x002f, &[]));

        let response = connection
            .receive(&encrypt_plaintext_frame(&plaintext, &key))
            .unwrap()
            .unwrap();
        let packets = unpack_replies(&response, &key);
        assert_eq!(
            packets
                .iter()
                .map(|packet| u16::from_be_bytes([packet[2], packet[3]]))
                .collect::<Vec<_>>(),
            [0x0032, 0x0033, 0x0034, 0x0036]
        );
        assert!(connection.should_close());
    }

    fn login_payload(username: &str, password: &str) -> Vec<u8> {
        let mut payload = Vec::new();
        for value in [username, password] {
            payload.push((value.len() + 1) as u8);
            payload.extend_from_slice(value.as_bytes());
            payload.push(0);
        }
        payload.extend_from_slice(&[0, 0, 0, 0, 0]); // empty MAC and SSO
        payload
    }

    #[test]
    fn key_vector_matches_emulator_nibble_mapping() {
        let mut packet = [b'0'; 260];
        packet[..4].copy_from_slice(&256u32.to_be_bytes());
        packet[4..].copy_from_slice(b"1234567890abcdef1234567890abcdef".repeat(8).as_slice());
        let key = derive_aes_key(&packet, &BigUint::from(1u8)).unwrap();
        assert_eq!(
            key,
            [
                0x12, 0x34, 0x56, 0x78, 0x90, 0x12, 0x34, 0x56, 0x12, 0x34, 0x56, 0x78, 0x90, 0x12,
                0x34, 0x56
            ]
        );
    }

    #[test]
    fn encrypted_frame_round_trip_and_header_lengths() {
        let key = [0x35; 16];
        let packet = make_packet(0x000a, &[1, 2, 3]);
        let frame = encrypt_frame(&packet, &key).unwrap();
        assert_eq!(u32::from_be_bytes(frame[..4].try_into().unwrap()), 16);
        assert_eq!(
            u32::from_be_bytes(frame[4..8].try_into().unwrap()),
            packet.len() as u32
        );
        assert_eq!(
            frame,
            [
                0, 0, 0, 16, 0, 0, 0, 7, 0xeb, 0xf0, 0xe5, 0xd4, 0x64, 0x3d, 0xf1, 0x2b, 0x9a,
                0xab, 0x9e, 0x42, 0x15, 0x78, 0x87, 0x47,
            ]
        );
        assert_eq!(decrypt_frame(&frame, &key).unwrap().0, packet);
    }

    #[test]
    fn tcp_frame_size_waits_for_fragmented_header_and_payload() {
        let key = [0x35; 16];
        let frame = encrypt_frame(&make_packet(0x000a, &[]), &key).unwrap();
        let connection = Connection {
            phase: Phase::Ready,
            server_private: BigUint::from(1u8),
            aes_key: Some(key),
            store: None,
            account_id: None,
            salts: None,
            role: ServerRole::Login,
            should_close: false,
            character: None,
            selected_character_id: None,
            map_loaded: false,
            maps: None,
            position_dirty: false,
            position_saved_at: None,
            cheats: crate::commands::Cheats::default(),
        };
        assert_eq!(connection.next_input_size(&frame[..3]).unwrap(), None);
        assert_eq!(
            connection.next_input_size(&frame[..8]).unwrap(),
            Some(frame.len())
        );
        assert_eq!(
            connection
                .next_input_size(&frame[..frame.len() - 1])
                .unwrap(),
            Some(frame.len())
        );
    }

    #[test]
    fn rejects_inconsistent_outer_lengths_and_bad_dh_public_values() {
        let connection = Connection {
            phase: Phase::Ready,
            server_private: BigUint::from(1u8),
            aes_key: Some([0; 16]),
            store: None,
            account_id: None,
            salts: None,
            role: ServerRole::Login,
            should_close: false,
            character: None,
            selected_character_id: None,
            map_loaded: false,
            maps: None,
            position_dirty: false,
            position_saved_at: None,
            cheats: crate::commands::Cheats::default(),
        };
        let mut bad_header = [0u8; 8];
        bad_header[..4].copy_from_slice(&32u32.to_be_bytes());
        bad_header[4..].copy_from_slice(&4u32.to_be_bytes());
        assert!(connection.next_input_size(&bad_header).is_err());
        assert_eq!(
            crate::diagnostics::take_protocol_failures(),
            vec![(ProtocolStage::FrameHeader, None)]
        );

        let mut invalid_client_key = vec![0u8; 260];
        invalid_client_key[..4].copy_from_slice(&1u32.to_be_bytes());
        invalid_client_key[4..].fill(b'0');
        assert!(derive_aes_key(&invalid_client_key, &BigUint::from(1u8)).is_err());

        invalid_client_key[..4].copy_from_slice(&256u32.to_be_bytes());
        invalid_client_key[4..].fill(b'0');
        invalid_client_key[259] = b'1';
        assert!(derive_aes_key(&invalid_client_key, &BigUint::from(1u8)).is_err());
    }

    #[test]
    fn protocol_failure_paths_report_stage_and_only_dispatch_reports_opcode() {
        let mut handshake = Connection::new();
        assert!(handshake.receive(b"bad-init").is_err());
        assert_eq!(
            crate::diagnostics::take_protocol_failures(),
            vec![(ProtocolStage::Receive, None)]
        );

        let (mut connection, _, key) = ready_connection();
        let malformed_login = encrypt_frame(&make_packet(0x001f, &[]), &key).unwrap();
        assert!(connection.receive(&malformed_login).is_err());
        assert_eq!(
            crate::diagnostics::take_protocol_failures(),
            vec![(ProtocolStage::Dispatch, Some(0x001f))]
        );

        let mut malformed_inner = make_packet(0x0032, &[]);
        malformed_inner[0] = 1;
        let cipher = Aes128::new(GenericArray::from_slice(&key));
        let mut malformed_frame = Vec::from(16u32.to_be_bytes());
        malformed_frame.extend_from_slice(&(malformed_inner.len() as u32).to_be_bytes());
        let mut block = [0u8; 16];
        block[..malformed_inner.len()].copy_from_slice(&malformed_inner);
        cipher.encrypt_block(GenericArray::from_mut_slice(&mut block));
        malformed_frame.extend_from_slice(&block);
        assert!(connection.receive(&malformed_frame).is_err());
        assert_eq!(
            crate::diagnostics::take_protocol_failures(),
            vec![(ProtocolStage::InnerPacket, None)]
        );

        let mut padded = [0u8; 16];
        padded[..4].copy_from_slice(&make_packet(0x0032, &[]));
        padded[15] = 1;
        cipher.encrypt_block(GenericArray::from_mut_slice(&mut padded));
        let mut bad_padding_frame = Vec::from(16u32.to_be_bytes());
        bad_padding_frame.extend_from_slice(&4u32.to_be_bytes());
        bad_padding_frame.extend_from_slice(&padded);
        assert!(connection.receive(&bad_padding_frame).is_err());
        assert_eq!(
            crate::diagnostics::take_protocol_failures(),
            vec![(ProtocolStage::FramePadding, None)]
        );

        assert!(connection.receive(&[]).is_err());
        assert_eq!(
            crate::diagnostics::take_protocol_failures(),
            vec![(ProtocolStage::FrameBody, None)]
        );

        connection.account_id = Some(1);
        assert!(connection.friend_mail_ready(&[]).is_err());
        assert_eq!(
            crate::diagnostics::take_protocol_failures(),
            vec![(ProtocolStage::FriendInitPayload, None)]
        );
        assert!(connection.friend_mail_ready(&123u32.to_be_bytes()).is_err());
        assert_eq!(
            crate::diagnostics::take_protocol_failures(),
            vec![(ProtocolStage::FriendInitOwnership, None)]
        );
    }

    #[test]
    fn friend_init_shape_diagnostic_contains_only_boolean_relationships() {
        let (mut connection, store, _) = ready_connection();
        let owned = map_test_character(&store);
        store
            .borrow_mut()
            .create_character(
                1,
                NewCharacter {
                    slot: 1,
                    name: "Second".into(),
                    race: 0,
                    sex: 0,
                    hair: 0,
                    hair_color: 0,
                    face: 0,
                },
            )
            .unwrap();
        connection.account_id = Some(1);

        let owned_bytes = owned.id.to_be_bytes();
        assert!(connection.friend_mail_ready(&owned_bytes).is_ok());
        assert_eq!(
            crate::diagnostics::take_friend_init_shapes(),
            vec![[
                owned.id == 0,
                owned.id == 1,
                owned.id == 0 || owned.id == 1,
                owned_bytes[0] == 0 || owned_bytes[0] == 1,
                owned_bytes[1..].iter().all(|byte| *byte == 0),
            ]]
        );

        for (payload, expected) in [
            (&[0, 0, 0, 0][..], [true, false, true, true, true]),
            (&[1, 0, 0, 0][..], [false, false, false, true, true]),
            (
                &[0xde, 0xad, 0xbe, 0xef][..],
                [false, false, false, false, false],
            ),
        ] {
            assert!(connection.friend_mail_ready(payload).is_err());
            assert_eq!(
                crate::diagnostics::take_friend_init_shapes(),
                vec![expected]
            );
            assert_eq!(
                crate::diagnostics::take_protocol_failures(),
                vec![(ProtocolStage::FriendInitOwnership, None)]
            );
        }
    }

    #[test]
    fn zero_friend_reference_requires_fresh_authenticated_character_selection() {
        let (mut connection, store, key) = ready_connection();
        map_test_character(&store);
        let selected = store
            .borrow_mut()
            .create_character(
                1,
                NewCharacter {
                    slot: 1,
                    name: "Selected".into(),
                    race: 0,
                    sex: 0,
                    hair: 0,
                    hair_color: 0,
                    face: 0,
                },
            )
            .unwrap();
        assert!(connection.request_map_id(&[selected.slot]).is_err());
        assert!(connection.friend_mail_ready(&0u32.to_be_bytes()).is_err());
        connection.account_id = Some(1);
        assert!(connection.friend_mail_ready(&0u32.to_be_bytes()).is_err());
        assert!(connection.friend_mail_ready(&[0, 0, 0]).is_err());
        assert!(connection.request_map_id(&[9]).is_err());
        assert!(connection.friend_mail_ready(&0u32.to_be_bytes()).is_err());

        let response = exchange(&mut connection, &key, 0x00a7, &[selected.slot]).unwrap();
        assert_eq!(
            response,
            [make_packet(0x00a8, &selected.map_id.to_be_bytes())]
        );
        assert_eq!(connection.selected_character_id, Some(selected.id));
        assert_eq!(
            exchange(&mut connection, &key, 0x002a, &0u32.to_be_bytes()).unwrap(),
            [make_packet(0x002b, &[]), make_packet(0x00dd, &[1, 1, 0])]
        );
        assert_eq!(
            exchange(&mut connection, &key, 0x002a, &selected.id.to_be_bytes()).unwrap(),
            [make_packet(0x002b, &[]), make_packet(0x00dd, &[1, 1, 0])]
        );
        connection.selected_character_id = Some(u32::MAX);
        assert!(connection.friend_mail_ready(&0u32.to_be_bytes()).is_err());
        connection.selected_character_id = Some(selected.id);
        connection.account_id = Some(2);
        assert!(connection.friend_mail_ready(&0u32.to_be_bytes()).is_err());
        connection.account_id = Some(1);
        assert!(connection
            .friend_mail_ready(&u32::MAX.to_be_bytes())
            .is_err());

        assert!(connection.request_map_id(&[9]).is_err());
        assert_eq!(connection.selected_character_id, None);
        assert!(connection.friend_mail_ready(&0u32.to_be_bytes()).is_err());

        connection.selected_character_id = Some(selected.id);
        connection.salts = Some((12_345, 67_890));
        let rejected_login = connection
            .login(&login_payload("test", "incorrect"))
            .unwrap();
        assert!(rejected_login.is_some());
        assert_eq!(connection.account_id, None);
        assert_eq!(connection.selected_character_id, None);
        assert!(connection.friend_mail_ready(&0u32.to_be_bytes()).is_err());
    }

    #[test]
    fn handshake_server_key_has_verified_wire_size_and_prefix() {
        let packet = server_key_packet(&BigUint::from(1u8));
        assert_eq!(packet.len(), 529);
        assert_eq!(&packet[..9], &[0, 0, 0, 0, 0, 0, 0, 1, b'3']);
    }

    #[test]
    fn login_authenticates_using_source_salt_hash_and_returns_selection_packets() {
        let (mut connection, _, key) = ready_connection();
        assert_eq!(
            salted_password_hash(12_345, "96e79218965eb72c92a549dd5a330112", 67_890),
            "7c4d1fdb726c8c1c2156e6a0e6b41f2711055820"
        );
        let hash = salted_password_hash(12_345, "96e79218965eb72c92a549dd5a330112", 67_890);
        let success = connection
            .receive(
                &encrypt_frame(&make_packet(0x001f, &login_payload("test", &hash)), &key).unwrap(),
            )
            .unwrap()
            .unwrap();
        let replies = unpack_replies(&success, &key);
        assert_eq!(
            replies
                .iter()
                .map(|p| u16::from_be_bytes([p[2], p[3]]))
                .collect::<Vec<_>>(),
            [0x0020, 0x0028, 0x0029]
        );
        assert_eq!(&replies[0][4..12], &[0, 0, 0, 0, 0, 0, 0, 1]);
        assert_eq!(replies[1].len(), 137);
        assert_eq!(replies[2].len(), 4 + 4 * (1 + 14 * 4));

        let (mut bad, _, bad_key) = ready_connection();
        let failed = bad
            .receive(
                &encrypt_frame(
                    &make_packet(0x001f, &login_payload("test", "wrong")),
                    &bad_key,
                )
                .unwrap(),
            )
            .unwrap()
            .unwrap();
        let packet = decrypt_frame(&failed, &bad_key).unwrap().0;
        assert_eq!(&packet[4..8], &0xffff_fffdu32.to_be_bytes());
        assert_eq!(bad.account_id, None);

        let mut malformed = Vec::new();
        malformed.extend_from_slice(&[3, b'b', b'a', b'd']); // missing terminal NUL
        assert_eq!(
            bad.receive(&encrypt_frame(&make_packet(0x001f, &malformed), &bad_key).unwrap())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );

        let mut create_after_failure = vec![0, 5, b'm', b'i', b'a', b'o', 0, 0, 1, 0, 1, 51, 0, 3];
        assert!(bad
            .receive(&encrypt_frame(&make_packet(0x00a0, &create_after_failure), &bad_key).unwrap())
            .is_err());
        create_after_failure.clear();
    }

    #[test]
    fn authenticated_character_create_and_map_id_survive_relogin() {
        let (mut connection, store, key) = ready_connection();
        connection.account_id = Some(1);
        let mut creation = vec![0];
        creation.extend_from_slice(&[5, b'm', b'i', b'a', b'o', 0, 0, 1, 0, 1, 51, 0, 3]);
        let response = connection
            .receive(&encrypt_frame(&make_packet(0x00a0, &creation), &key).unwrap())
            .unwrap()
            .unwrap();
        let packet = decrypt_frame(&response, &key).unwrap().0;
        assert_eq!(&packet[4..8], &0u32.to_be_bytes());

        let map = connection
            .receive(&encrypt_frame(&make_packet(0x00a7, &[0]), &key).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(
            decrypt_frame(&map, &key).unwrap().0,
            make_packet(0x00a8, &20_013_003u32.to_be_bytes())
        );

        let mut relogin = Connection {
            phase: Phase::Ready,
            server_private: BigUint::from(1u8),
            aes_key: Some(key),
            store: Some(store),
            account_id: None,
            salts: Some((12_345, 67_890)),
            role: ServerRole::Login,
            should_close: false,
            character: None,
            selected_character_id: None,
            map_loaded: false,
            maps: None,
            position_dirty: false,
            position_saved_at: None,
            cheats: crate::commands::Cheats::default(),
        };
        let hash = salted_password_hash(12_345, "96e79218965eb72c92a549dd5a330112", 67_890);
        let response = relogin
            .receive(
                &encrypt_frame(&make_packet(0x001f, &login_payload("test", &hash)), &key).unwrap(),
            )
            .unwrap()
            .unwrap();
        let replies = unpack_replies(&response, &key);
        assert_eq!(&replies[1][6..10], b"miao");
    }

    #[test]
    fn login_status_check_returns_source_signed_status() {
        let (mut connection, _, key) = ready_connection();
        connection.role = ServerRole::Gate;
        let request = encrypt_frame(&make_packet(0x002f, &[]), &key).unwrap();
        let response = connection.receive(&request).unwrap().unwrap();
        assert_eq!(
            decrypt_frame(&response, &key).unwrap().0,
            make_packet(0x0030, &(-1i32).to_be_bytes())
        );

        connection.account_id = Some(1);
        let response = connection.receive(&request).unwrap().unwrap();
        assert_eq!(
            decrypt_frame(&response, &key).unwrap().0,
            make_packet(0x0030, &0i32.to_be_bytes())
        );
    }

    #[test]
    fn gate_server_list_matches_source_packets_and_closes_after_write() {
        let (mut connection, _, key) = ready_connection();
        connection.role = ServerRole::Gate;
        connection.account_id = Some(1);
        let request = encrypt_frame(&make_packet(0x0031, &[]), &key).unwrap();
        let response = connection.receive(&request).unwrap().unwrap();
        let packets = unpack_replies(&response, &key);
        assert_eq!(
            packets
                .iter()
                .map(|p| u16::from_be_bytes([p[2], p[3]]))
                .collect::<Vec<_>>(),
            [0x0032, 0x0033, 0x0034, 0x0036]
        );
        assert_eq!(packets[0], make_packet(0x0032, &1u32.to_be_bytes()));
        assert_eq!(
            packets[1],
            make_packet(
                0x0033,
                &[
                    7, b'C', b'l', b'o', b'v', b'e', b'r', 0, 16, b'1', b'2', b'7', b'.', b'0',
                    b'.', b'0', b'.', b'1', b':', b'1', b'7', b'8', b'3', b'2', 0
                ]
            )
        );
        assert_eq!(packets[2], make_packet(0x0034, &[]));
        assert_eq!(packets[3], make_packet(0x0036, &[0, 0, 0, 1, 0, 0, 0]));
        assert!(connection.should_close());
    }

    #[test]
    fn gate_rejects_server_list_before_authentication() {
        let (mut connection, _, key) = ready_connection();
        connection.role = ServerRole::Gate;
        let request = encrypt_frame(&make_packet(0x0031, &[]), &key).unwrap();
        assert_eq!(
            connection.receive(&request).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn gate_login_does_not_send_character_packets() {
        let (mut connection, _, key) = ready_connection();
        connection.role = ServerRole::Gate;
        let hash = salted_password_hash(12_345, "96e79218965eb72c92a549dd5a330112", 67_890);
        let request =
            encrypt_frame(&make_packet(0x001f, &login_payload("test", &hash)), &key).unwrap();
        let response = connection.receive(&request).unwrap().unwrap();
        let packets = unpack_replies(&response, &key);
        assert_eq!(packets.len(), 1);
        assert_eq!(u16::from_be_bytes([packets[0][2], packets[0][3]]), 0x0020);
        assert!(connection.account_id.is_some());
    }

    fn map_test_character(store: &Rc<RefCell<Store>>) -> Character {
        store
            .borrow_mut()
            .create_character(
                1,
                NewCharacter {
                    slot: 0,
                    name: "Offline".into(),
                    race: 0,
                    sex: 0,
                    hair: 0,
                    hair_color: 0,
                    face: 0,
                },
            )
            .unwrap()
    }

    fn test_maps() -> Rc<Maps> {
        let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data");
        Rc::new(
            Maps::load(&[
                data.join("20013003.map").as_path(),
                data.join("10023000.map").as_path(),
            ])
            .unwrap(),
        )
    }

    fn exchange(
        connection: &mut Connection,
        key: &[u8; 16],
        opcode: u16,
        payload: &[u8],
    ) -> io::Result<Vec<Vec<u8>>> {
        let request = encrypt_frame(&make_packet(opcode, payload), key)?;
        Ok(connection
            .receive(&request)?
            .map(|bytes| unpack_replies(&bytes, key))
            .unwrap_or_default())
    }

    #[test]
    fn map_authentication_entry_movement_and_relogin_preserve_position() {
        let (mut connection, store, key) = ready_connection();
        connection.role = ServerRole::Map;
        connection.maps = Some(test_maps());
        map_test_character(&store);
        assert!(exchange(&mut connection, &key, 0x01fd, &[0; 6]).is_err());
        let init = exchange(&mut connection, &key, 0x000a, &[0x12, 0x34]).unwrap();
        assert_eq!(
            init.iter()
                .map(|p| u16::from_be_bytes([p[2], p[3]]))
                .collect::<Vec<_>>(),
            [0xffff, 0x000b, 0x000f]
        );
        assert_eq!(&init[1][4..], &[0x12, 0x34]);
        let front = u32::from_be_bytes(init[2][4..8].try_into().unwrap());
        let back = u32::from_be_bytes(init[2][8..12].try_into().unwrap());
        let hash = salted_password_hash(front, "96e79218965eb72c92a549dd5a330112", back);
        let auth = exchange(&mut connection, &key, 0x0010, &login_payload("test", &hash)).unwrap();
        assert_eq!(auth.len(), 1);
        assert_eq!(auth[0].len(), 14);
        assert_eq!(&auth[0][2..10], &[0, 0x11, 0, 0, 0, 0, 1, 0]);
        assert!(exchange(&mut connection, &key, 0x01fd, &[0, 0, 0, 0, 3, 0]).is_err());
        let entry = exchange(&mut connection, &key, 0x01fd, &[0; 6]).unwrap();
        let entered = store.borrow().characters(1).unwrap().remove(0);
        assert_eq!(entered.id, 1, "durable character ID remains unchanged");
        assert_eq!((entered.x, entered.y, entered.direction), (63, 58, 0));
        let actor = entry
            .iter()
            .find(|packet| packet[2..4] == [0x01, 0xff])
            .unwrap();
        assert_eq!(&actor[4..8], &crate::map::PLAYER_ACTOR_ID.to_be_bytes());
        assert_eq!(&actor[8..12], &entered.id.to_be_bytes());
        for opcode in [0x1239u16, 0x0fa7, 0x0221, 0x021c, 0x157c, 0x1b67] {
            let actor_addressed = entry
                .iter()
                .find(|packet| u16::from_be_bytes([packet[2], packet[3]]) == opcode)
                .unwrap();
            assert_eq!(
                &actor_addressed[4..8],
                &crate::map::PLAYER_ACTOR_ID.to_be_bytes(),
                "opcode {opcode:04x} uses session actor ID"
            );
        }
        let mut finish = crate::map::PLAYER_ACTOR_ID.to_be_bytes().to_vec();
        finish.extend_from_slice(&[0; 4]);
        assert_eq!(entry.last().unwrap(), &make_packet(0x1b67, &finish));
        let movement = [0xff, 0xce, 0, 150, 0, 90, 0, 7];
        assert!(exchange(&mut connection, &key, 0x11f8, &movement).is_err());
        let ready = exchange(&mut connection, &key, 0x11fe, &[]).unwrap();
        let mut unlock = crate::map::PLAYER_ACTOR_ID.to_be_bytes().to_vec();
        unlock.extend_from_slice(&420u16.to_be_bytes());
        assert_eq!(ready, [make_packet(0x1239, &unlock)]);
        let moved = exchange(&mut connection, &key, 0x11f8, &movement).unwrap();
        let mut broadcast = crate::map::PLAYER_ACTOR_ID.to_be_bytes().to_vec();
        broadcast.extend_from_slice(&movement);
        assert_eq!(moved, [make_packet(0x11f9, &broadcast)]);
        // Moves are batched in memory; logout writes them.
        let unsaved = store.borrow().characters(1).unwrap().remove(0);
        assert_eq!((unsaved.x, unsaved.y, unsaved.direction), (63, 58, 0));
        assert_eq!(
            exchange(&mut connection, &key, 0x001f, &[0]).unwrap(),
            [crate::map::logout(0)]
        );
        let saved = store.borrow().characters(1).unwrap().remove(0);
        assert_eq!((saved.x, saved.y, saved.direction), (63, 62, 2));
        for invalid_move in [
            vec![0; 7],
            vec![0x7f, 0xff, 0, 0, 0, 90, 0, 7],
            vec![0, 0, 0, 0, 1, 104, 0, 7],
            vec![0, 0, 0, 0, 0, 0, 0, 14],
            // Tile (0,0) is in bounds but blocked in the source map.
            vec![0xe7, 0x32, 0x18, 0xce, 0, 0, 0, 7],
        ] {
            assert!(exchange(&mut connection, &key, 0x11f8, &invalid_move).is_err());
        }
        assert_eq!(store.borrow().characters(1).unwrap()[0], saved);
        exchange(&mut connection, &key, 0x0010, &login_payload("test", "bad")).unwrap();
        assert!(connection.character.is_none());
        assert!(exchange(&mut connection, &key, 0x11f8, &movement).is_err());
        exchange(&mut connection, &key, 0x0010, &login_payload("test", &hash)).unwrap();
        exchange(&mut connection, &key, 0x01fd, &[0; 6]).unwrap();
        assert_eq!(connection.character.as_ref().unwrap(), &saved);
    }

    #[test]
    fn up_town_entry_and_movement_use_its_own_dimensions() {
        let (mut connection, store, key) = ready_connection();
        connection.role = ServerRole::Map;
        connection.maps = Some(test_maps());
        let created = map_test_character(&store);
        store
            .borrow_mut()
            .place_character(1, created.id, 10_023_000, 130, 139, 4)
            .unwrap();
        let init = exchange(&mut connection, &key, 0x000a, &[0x12, 0x34]).unwrap();
        let front = u32::from_be_bytes(init[2][4..8].try_into().unwrap());
        let back = u32::from_be_bytes(init[2][8..12].try_into().unwrap());
        let hash = salted_password_hash(front, "96e79218965eb72c92a549dd5a330112", back);
        exchange(&mut connection, &key, 0x0010, &login_payload("test", &hash)).unwrap();
        let entry = exchange(&mut connection, &key, 0x01fd, &[0; 6]).unwrap();
        let entered = connection.character.clone().unwrap();
        assert_eq!(
            (entered.map_id, entered.x, entered.y),
            (10_023_000, 130, 139)
        );
        assert!(entry.iter().any(|packet| packet[2..4] == [0x01, 0xff]));
        exchange(&mut connection, &key, 0x11fe, &[]).unwrap();

        // Tile (131,139) centre on a 256x256 map: x = 3*100+50, y = -(11*100)-50.
        let mut movement = 350i16.to_be_bytes().to_vec();
        movement.extend_from_slice(&(-1150i16).to_be_bytes());
        movement.extend_from_slice(&[0, 90, 0, 7]);
        exchange(&mut connection, &key, 0x11f8, &movement).unwrap();
        let position = |store: &Rc<RefCell<Store>>| {
            let c = store.borrow().characters(1).unwrap().remove(0);
            (c.map_id, c.x, c.y, c.direction)
        };
        assert_eq!(
            position(&store),
            (10_023_000, 130, 139, 4),
            "first move is batched"
        );
        // Once the save interval has passed, the next move writes the position.
        connection.position_saved_at =
            Some(Instant::now().checked_sub(POSITION_SAVE_INTERVAL).unwrap());
        exchange(&mut connection, &key, 0x11f8, &movement).unwrap();
        assert_eq!(position(&store), (10_023_000, 131, 139, 2));

        // (126,126) is inside Up Town but blocked.
        let mut blocked = (-150i16).to_be_bytes().to_vec();
        blocked.extend_from_slice(&150i16.to_be_bytes());
        blocked.extend_from_slice(&[0, 0, 0, 7]);
        assert!(exchange(&mut connection, &key, 0x11f8, &blocked).is_err());

        let actor = crate::map::PLAYER_ACTOR_ID;
        assert_eq!(
            exchange(&mut connection, &key, 0x0fa5, &[1]).unwrap(),
            [crate::map::battle_status(actor, 1)]
        );
        assert_eq!(
            exchange(&mut connection, &key, 0x121b, &[0, 0x83, 1]).unwrap(),
            [crate::map::motion(actor, 0x83, 1)]
        );
        assert_eq!(
            exchange(&mut connection, &key, 0x05e6, &11_000_155u32.to_be_bytes()).unwrap(),
            crate::map::event_unavailable(11_000_155)
        );
        assert!(exchange(&mut connection, &key, 0x05e6, &[0; 3]).is_err());
        assert_eq!(
            exchange(&mut connection, &key, 0x001f, &[0]).unwrap(),
            [crate::map::logout(0)]
        );

        // A disconnect without logout (closed or crashed client) still saves the last move.
        let mut back = 250i16.to_be_bytes().to_vec();
        back.extend_from_slice(&(-1150i16).to_be_bytes());
        back.extend_from_slice(&[1, 14, 0, 7]);
        exchange(&mut connection, &key, 0x11f8, &back).unwrap();
        assert_eq!(position(&store), (10_023_000, 131, 139, 2));
        drop(connection);
        assert_eq!(position(&store), (10_023_000, 130, 139, 6));
    }

    #[test]
    fn placing_a_character_refuses_unknown_owners() {
        let (_, store, _) = ready_connection();
        let created = map_test_character(&store);
        assert_eq!(
            store
                .borrow_mut()
                .place_character(2, created.id, 10_023_000, 130, 139, 0)
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn map_redirect_is_loopback_only_and_requires_an_owned_map() {
        let (mut connection, store, key) = ready_connection();
        let created = map_test_character(&store);
        assert!(exchange(&mut connection, &key, 0x0032, &created.map_id.to_be_bytes()).is_err());
        let hash = salted_password_hash(12_345, "96e79218965eb72c92a549dd5a330112", 67_890);
        exchange(&mut connection, &key, 0x001f, &login_payload("test", &hash)).unwrap();
        assert!(exchange(&mut connection, &key, 0x0032, &0u32.to_be_bytes()).is_err());
        assert!(exchange(&mut connection, &key, 0x0032, &[0; 3]).is_err());
        let reply = exchange(&mut connection, &key, 0x0032, &created.map_id.to_be_bytes()).unwrap();
        assert!(exchange(&mut connection, &key, 0x002a, &0u32.to_be_bytes()).is_err());
        let friends = exchange(&mut connection, &key, 0x002a, &created.id.to_be_bytes()).unwrap();
        assert_eq!(
            friends,
            [make_packet(0x002b, &[]), make_packet(0x00dd, &[1, 1, 0])]
        );
        assert_eq!(
            &reply[0][4..],
            &[1, 10, b'1', b'2', b'7', b'.', b'0', b'.', b'0', b'.', b'1', 0, 0, 0, 0x45, 0xa9]
        );
    }

    #[test]
    fn selection_serialization_matches_source_field_major_layout() {
        let character = Character {
            id: 7,
            account_id: 1,
            slot: 2,
            name: "miao".into(),
            race: 1,
            sex: 0,
            hair: 0x1234,
            hair_color: 5,
            face: 0x6789,
            map_id: 0x0102_0304,
            x: 0,
            y: 0,
            direction: 0,
        };
        let packet = character_selection(&[character]);
        assert_eq!(packet.len(), 137);
        assert_eq!(
            &packet[..11],
            &[4, 0, 0, 4, b'm', b'i', b'a', b'o', 0, 4, 0]
        );
        // Source property sequence after name is race, form, sex, hairstyle, hair color, wig,
        // empty-slot flag, face, five single-byte status fields, then map and levels.
        let mut expected = vec![4, 0, 0, 1, 0]; // race: count + four slots
        expected.extend_from_slice(&[4, 0, 0, 0, 0]); // form
        expected.extend_from_slice(&[4, 0, 0, 0, 0]); // sex
        expected.extend_from_slice(&[4, 0, 0, 0, 0, 0x12, 0x34, 0, 0]); // hairstyle
        expected.extend_from_slice(&[4, 0, 0, 5, 0]); // hair color
        expected.extend_from_slice(&[4, 0, 0, 0, 0, 0, 255, 0, 0]); // wig default
        expected.extend_from_slice(&[4, 0, 0, 0xff, 0]); // occupied-slot flag (SagaECO SetIfExist)
        expected.extend_from_slice(&[4, 0, 0, 0, 0, 0x67, 0x89, 0, 0]); // face
        expected.extend_from_slice(&[4, 0, 0, 0, 0]); // rebirth
        expected.extend_from_slice(&[4, 0, 0, 0, 0]); // ex
        expected.extend_from_slice(&[4, 0, 0, 0, 0]); // wing
        expected.extend_from_slice(&[4, 0, 0, 0, 0]); // wing color
        expected.extend_from_slice(&[4, 0, 0, 0, 0]); // job
        expected.extend_from_slice(&[4, 0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4, 0, 0, 0, 0]); // map
        expected.extend_from_slice(&[4, 0, 0, 1, 0]); // base level 1 for the stored slot
        expected.extend_from_slice(&[4, 0, 0, 1, 0]); // job 1 level 1
        expected.extend_from_slice(&[4, 0, 0, 0, 0, 0, 5, 0, 0]); // quest default
        expected.extend_from_slice(&[4, 0, 0, 0, 0]); // job 2 expert
        expected.extend_from_slice(&[4, 0, 0, 0, 0]); // job 2 technical
        expected.extend_from_slice(&[4, 0, 0, 0, 0]); // job 3
        let actual = &packet[9..];
        let mismatch = actual.iter().zip(&expected).position(|(a, b)| a != b);
        assert!(
            actual == expected,
            "selection differs at {mismatch:?}; lengths {} and {}",
            actual.len(),
            expected.len()
        );
    }
}
