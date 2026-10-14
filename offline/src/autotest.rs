//! Autonomous scripted test.
//!
//! Starts its own world/login/map servers (this executable) on spare loopback ports with
//! a throwaway copy of the save, then plays a scenario over the real encrypted protocol
//! the way the game client would. Each step is checked and timed; nothing touches the
//! player's save or the running game.

use crate::protocol::{
    aes_key_from_shared, decrypt_frame, encrypt_frame, make_packet, INIT_PACKET, PRIME_DECIMAL,
};
use crate::store::Store;
use crate::terrain::Maps;
use num_bigint::BigUint;
use num_traits::Num;
use sha1::{Digest, Sha1};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const ACCOUNT_ID: u32 = 1;
const DEFAULT_ACCOUNT: &str = "test";
const DEFAULT_TOKEN: &str = "96e79218965eb72c92a549dd5a330112";
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_ENTRY_PACKETS: usize = 64;

pub struct Options {
    pub scenario: PathBuf,
    pub save: Option<PathBuf>,
    pub trace: bool,
    pub keep: bool,
    pub keep_going: bool,
}

pub fn usage() -> &'static str {
    "usage: eco-reborn autotest <scenario.eco> [--save <world.sqlite>] [--trace] [--keep] [--keep-going]"
}

pub fn parse_options(args: &[String]) -> io::Result<Options> {
    let mut options = Options {
        scenario: PathBuf::new(),
        save: None,
        trace: false,
        keep: false,
        keep_going: false,
    };
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--save" => {
                options.save = Some(PathBuf::from(rest.next().ok_or_else(|| fail(usage()))?))
            }
            "--trace" => options.trace = true,
            "--keep" => options.keep = true,
            "--keep-going" => options.keep_going = true,
            _ if options.scenario.as_os_str().is_empty() && !arg.starts_with("--") => {
                options.scenario = PathBuf::from(arg)
            }
            _ => return Err(fail(usage())),
        }
    }
    if options.scenario.as_os_str().is_empty() {
        return Err(fail(usage()));
    }
    Ok(options)
}

/// Runs a scenario; Ok(true) when every step passed and the servers stayed up.
pub fn run(options: &Options) -> io::Result<bool> {
    // Only the spawned servers trace; this client's packets would be logged backwards.
    crate::trace::disable();
    let text = fs::read_to_string(&options.scenario)?;
    let steps = parse(&text)?;
    let offline = offline_root()?;
    let workdir = make_workdir()?;
    let save = workdir.join("world.sqlite");
    match &options.save {
        Some(source) => {
            fs::copy(source, &save)?;
        }
        None => drop(Store::open(&save)?),
    }
    let map_files = map_files(&offline.join("data"))?;
    let maps = Maps::load(&map_files.iter().map(PathBuf::as_path).collect::<Vec<_>>())?;

    let ports = Ports::pick()?;
    let child_dir = if options.trace {
        offline.clone()
    } else {
        workdir.clone()
    };
    let mut servers = Servers::start(&ports, &save, &map_files, &child_dir, &workdir)?;

    println!("Eco Reborn autotest: {}", options.scenario.display());
    println!(
        "servers world:{} login:{} map:{}  save:{}{}",
        ports.world,
        ports.login,
        ports.map,
        if options.save.is_some() {
            "copy"
        } else {
            "fresh"
        },
        if options.trace { "  trace:on" } else { "" }
    );
    let mut session = Session::new(ports, save.clone(), maps);
    let mut tally = Tally::default();
    let completed = execute(&steps, &mut session, &mut servers, options, &mut tally, 0);
    session.close_all();
    let alive = servers.all_alive();
    if !alive {
        println!(
            "[FAIL] a server process exited during the run (logs in {})",
            workdir.display()
        );
        tally.failed += 1;
    }
    servers.stop();

    let report = tally.summary(&options.scenario);
    print!("{report}");
    let reports = offline.join("reports");
    if reports.is_dir() {
        let name = format!("autotest-{}.txt", now_secs());
        let _ = fs::write(reports.join(&name), &report);
        println!("report: offline/reports/{name}");
    }
    if options.keep || tally.failed > 0 {
        println!("work folder kept: {}", workdir.display());
    } else {
        let _ = fs::remove_dir_all(&workdir);
    }
    Ok(completed && tally.failed == 0)
}

// ---------------------------------------------------------------- scenario

enum Step {
    Command {
        line: usize,
        words: Vec<String>,
    },
    Repeat {
        line: usize,
        count: usize,
        body: Vec<Step>,
    },
}

fn parse(text: &str) -> io::Result<Vec<Step>> {
    let mut stack: Vec<(usize, usize, Vec<Step>)> = vec![(0, 1, Vec::new())];
    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        let words: Vec<String> = raw
            .split('#')
            .next()
            .unwrap_or("")
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        match words.first().map(String::as_str) {
            None => {}
            Some("repeat") => {
                let count = words
                    .get(1)
                    .and_then(|n| n.parse().ok())
                    .filter(|n| *n > 0)
                    .ok_or_else(|| fail(&format!("line {line}: repeat needs a positive count")))?;
                stack.push((line, count, Vec::new()));
            }
            Some("end") => {
                if stack.len() == 1 {
                    return Err(fail(&format!("line {line}: end without repeat")));
                }
                let (start, count, body) = stack.pop().unwrap();
                stack.last_mut().unwrap().2.push(Step::Repeat {
                    line: start,
                    count,
                    body,
                });
            }
            Some(_) => stack
                .last_mut()
                .unwrap()
                .2
                .push(Step::Command { line, words }),
        }
    }
    if stack.len() != 1 {
        return Err(fail(&format!(
            "line {}: repeat without end",
            stack.last().unwrap().0
        )));
    }
    Ok(stack.pop().unwrap().2)
}

fn execute(
    steps: &[Step],
    session: &mut Session,
    servers: &mut Servers,
    options: &Options,
    tally: &mut Tally,
    depth: usize,
) -> bool {
    for step in steps {
        match step {
            Step::Repeat { line, count, body } => {
                println!(
                    "{:indent$}repeat {count} (line {line})",
                    "",
                    indent = depth * 2
                );
                for _ in 0..*count {
                    if !execute(body, session, servers, options, tally, depth + 1) {
                        return false;
                    }
                }
            }
            Step::Command { line, words } => {
                let started = Instant::now();
                let result = session.step(words, servers);
                let ms = started.elapsed().as_secs_f64() * 1000.0;
                let text = words.join(" ");
                match result {
                    Ok(note) => {
                        tally.pass(&words[0], ms);
                        // Repeated steps stay quiet unless they fail, so soak runs stay readable.
                        if depth == 0 {
                            println!("[ ok ] L{line:<3} {text:<34} {ms:>8.1} ms{note}");
                        }
                    }
                    Err(error) => {
                        tally.failed += 1;
                        println!("[FAIL] L{line:<3} {text:<34} {ms:>8.1} ms  {error}");
                        if !servers.all_alive() {
                            println!("       a server process has exited");
                        }
                        if !options.keep_going {
                            return false;
                        }
                    }
                }
            }
        }
    }
    true
}

#[derive(Default)]
struct Tally {
    passed: usize,
    failed: usize,
    timings: BTreeMap<String, Vec<f64>>,
}

impl Tally {
    fn pass(&mut self, command: &str, ms: f64) {
        self.passed += 1;
        self.timings.entry(command.to_owned()).or_default().push(ms);
    }

    fn summary(&self, scenario: &Path) -> String {
        let mut out = format!(
            "\n{} : {} step(s) passed, {} failed  [{}]\n",
            if self.failed == 0 { "PASS" } else { "FAIL" },
            self.passed,
            self.failed,
            scenario.display()
        );
        out.push_str("command          count    avg ms    max ms\n");
        for (command, values) in &self.timings {
            let avg = values.iter().sum::<f64>() / values.len() as f64;
            let max = values.iter().cloned().fold(0.0, f64::max);
            out.push_str(&format!(
                "{command:<14} {:>7} {avg:>9.2} {max:>9.2}\n",
                values.len()
            ));
        }
        out
    }
}

// ---------------------------------------------------------------- session

struct Session {
    ports: Ports,
    save: PathBuf,
    maps: Maps,
    account: String,
    token: String,
    slot: u8,
    login: Option<Client>,
    map: Option<Client>,
    current_map: Option<u32>,
    spawn: Option<(u32, u8, u8, u8)>,
}

impl Session {
    fn new(ports: Ports, save: PathBuf, maps: Maps) -> Self {
        Self {
            ports,
            save,
            maps,
            account: DEFAULT_ACCOUNT.into(),
            token: DEFAULT_TOKEN.into(),
            slot: 0,
            login: None,
            map: None,
            current_map: None,
            spawn: None,
        }
    }

    fn step(&mut self, words: &[String], servers: &mut Servers) -> io::Result<String> {
        let arg = |i: usize| -> io::Result<&str> {
            words
                .get(i)
                .map(String::as_str)
                .ok_or_else(|| fail(&format!("{} needs more arguments", words[0])))
        };
        let num = |i: usize| -> io::Result<u32> { parse_number(arg(i)?) };
        match words[0].as_str() {
            "account" => {
                self.account = arg(1)?.into();
                self.token = arg(2)?.into();
                Ok(String::new())
            }
            "login" => self.login_flow(),
            "create" => {
                let race = words
                    .get(2)
                    .map(|w| parse_number(w))
                    .transpose()?
                    .unwrap_or(0);
                let sex = words
                    .get(3)
                    .map(|w| parse_number(w))
                    .transpose()?
                    .unwrap_or(1);
                self.create(arg(1)?, race as u8, sex as u8)
            }
            "select" => {
                self.slot = num(1)? as u8;
                self.select()
            }
            "place" => {
                let dir = words
                    .get(4)
                    .map(|w| parse_number(w))
                    .transpose()?
                    .unwrap_or(0);
                self.place(num(1)?, num(2)? as u8, num(3)? as u8, dir as u8)
            }
            "enter" => self.enter(),
            "relog" => {
                self.close_map();
                self.enter()
            }
            "loaded" => self.loaded(),
            "move" => {
                let dir = words
                    .get(3)
                    .map(|w| parse_number(w))
                    .transpose()?
                    .unwrap_or(0);
                self.walk(num(1)? as u8, num(2)? as u8, dir as u16)
            }
            "expect-spawn" => self.expect_spawn(num(1)? as u8, num(2)? as u8, words.get(3)),
            "expect-saved" => self.expect_saved(num(1)? as u8, num(2)? as u8, words.get(3)),
            "npc" => self.npc(num(1)?),
            "emote" => {
                let looping = words
                    .get(2)
                    .map(|w| parse_number(w))
                    .transpose()?
                    .unwrap_or(0);
                self.simple(
                    0x121b,
                    &[&(num(1)? as u16).to_be_bytes()[..], &[looping as u8]].concat(),
                    &[0x121c],
                )
            }
            "stance" => self.simple(0x0fa5, &[num(1)? as u8], &[0x0fa6]),
            "chat" => self.chat(&words[1..].join(" ")),
            "logout" => {
                let note = self.simple(0x001f, &[0], &[0x0020])?;
                self.close_map();
                Ok(note)
            }
            "send" => {
                let opcode = u16::from_str_radix(arg(1)?, 16).map_err(|_| fail("bad opcode"))?;
                let payload = parse_hex(words.get(2).map(String::as_str).unwrap_or(""))?;
                let expect: Vec<u16> = match words.get(3) {
                    Some(list) => list
                        .split(',')
                        .map(|op| u16::from_str_radix(op, 16).map_err(|_| fail("bad opcode")))
                        .collect::<io::Result<_>>()?,
                    None => Vec::new(),
                };
                self.simple(opcode, &payload, &expect)
            }
            "wait" => {
                std::thread::sleep(Duration::from_millis(num(1)? as u64));
                Ok(String::new())
            }
            "servers-alive" => {
                if servers.all_alive() {
                    Ok(String::new())
                } else {
                    Err(fail("a server process has exited"))
                }
            }
            other => Err(fail(&format!("unknown command '{other}'"))),
        }
    }

    fn login_flow(&mut self) -> io::Result<String> {
        let mut gate = Client::connect(self.ports.world)?;
        gate.send(0x0001, &[0x55])?;
        let init = gate.expect(&[0xffff, 0x001e, 0x0002])?;
        let auth = self.auth_payload(pair(&init[1][4..])?);
        gate.send(0x001f, &auth)?;
        let result = gate.expect(&[0x0020])?;
        expect_zero(&result[0], "gate login")?;
        gate.send(0x002f, &[])?;
        gate.expect(&[0x0030])?;
        gate.send(0x0031, &[])?;
        gate.expect(&[0x0032, 0x0033, 0x0034, 0x0036])?;
        gate.close();

        let mut login = Client::connect(self.ports.login)?;
        login.send(0x0001, &[0x12, 0x34])?;
        let init = login.expect(&[0xffff, 0x001e, 0x0002])?;
        let auth = self.auth_payload(pair(&init[1][4..])?);
        login.send(0x001f, &auth)?;
        let replies = login.expect(&[0x0020, 0x0028, 0x0029])?;
        expect_zero(&replies[0], "login")?;
        self.login = Some(login);
        Ok(String::new())
    }

    fn create(&mut self, name: &str, race: u8, sex: u8) -> io::Result<String> {
        let mut payload = vec![self.slot];
        put_tstr(&mut payload, name);
        payload.extend_from_slice(&[race, sex]);
        payload.extend_from_slice(&7u16.to_be_bytes()); // hair
        payload.push(50); // hair colour
        payload.extend_from_slice(&0u16.to_be_bytes()); // face
        let login = self
            .login
            .as_mut()
            .ok_or_else(|| fail("create needs login first"))?;
        login.send(0x00a0, &payload)?;
        let reply = login.expect(&[0x00a1])?;
        expect_zero(&reply[0], "character creation")?;
        Ok(String::new())
    }

    fn select(&mut self) -> io::Result<String> {
        let slot = self.slot;
        let login = self
            .login
            .as_mut()
            .ok_or_else(|| fail("select needs login first"))?;
        login.send(0x00a7, &[slot])?;
        let map = login.expect(&[0x00a8])?;
        let map_id = be_u32(&map[0][4..])?;
        login.send(0x0032, &map_id.to_be_bytes())?;
        login.expect(&[0x0033])?;
        login.send(0x002a, &[0; 4])?;
        login.expect(&[0x002b, 0x00dd])?;
        if let Some(login) = self.login.take() {
            login.close();
        }
        Ok(format!("  map {map_id}"))
    }

    fn place(&mut self, map_id: u32, x: u8, y: u8, dir: u8) -> io::Result<String> {
        let terrain = self
            .maps
            .get(map_id)
            .ok_or_else(|| fail(&format!("map {map_id} is not loaded")))?;
        if !terrain.passable(x, y) {
            return Err(fail("destination tile is not walkable"));
        }
        let character = self.character()?;
        Store::open_existing(&self.save)?
            .place_character(ACCOUNT_ID, character, map_id, x, y, dir)?;
        Ok(String::new())
    }

    fn enter(&mut self) -> io::Result<String> {
        let mut map = Client::connect(self.ports.map)?;
        map.send(0x000a, &[0x12, 0x34])?;
        let init = map.expect(&[0xffff, 0x000b, 0x000f])?;
        let auth = self.auth_payload(pair(&init[2][4..])?);
        map.send(0x0010, &auth)?;
        map.expect(&[0x0011])?;
        map.send(0x01fd, &[0, 0, 0, 0, self.slot, 0])?;
        let packets = map.read_until(0x1b67, MAX_ENTRY_PACKETS)?;
        // The client reports the finished map move (13C7); the server answers with level, job, EXP.
        map.send(0x13c7, &[])?;
        map.expect(&[0x023a, 0x0244, 0x0235])?;
        let actor = packets
            .iter()
            .find(|p| opcode(p) == 0x01ff)
            .ok_or_else(|| fail("no 01FF self actor in the map entry"))?;
        let spawn = parse_actor_position(actor)?;
        self.current_map = Some(spawn.0);
        self.spawn = Some(spawn);
        self.map = Some(map);
        Ok(format!(
            "  {} packets, map {} ({},{})",
            packets.len(),
            spawn.0,
            spawn.1,
            spawn.2
        ))
    }

    fn loaded(&mut self) -> io::Result<String> {
        self.simple(0x11fe, &[], &[0x1239])
    }

    fn walk(&mut self, x: u8, y: u8, direction: u16) -> io::Result<String> {
        let map_id = self
            .current_map
            .ok_or_else(|| fail("move needs enter first"))?;
        let (width, height) = self
            .maps
            .get(map_id)
            .ok_or_else(|| fail(&format!("map {map_id} is not loaded")))?
            .dimensions();
        // SagaLib PosX8to16/PosY8to16: tile centre in world units.
        let world_x = (i32::from(x) - i32::from(width) / 2) * 100 + 50;
        let world_y = -(i32::from(y) - i32::from(height) / 2) * 100 - 50;
        let mut payload = (world_x as i16).to_be_bytes().to_vec();
        payload.extend_from_slice(&(world_y as i16).to_be_bytes());
        payload.extend_from_slice(&direction.to_be_bytes());
        payload.extend_from_slice(&7u16.to_be_bytes());
        let map = self
            .map
            .as_mut()
            .ok_or_else(|| fail("move needs enter first"))?;
        map.send(0x11f8, &payload)?;
        let echo = map.expect(&[0x11f9])?;
        if echo[0][8..] != payload[..] {
            return Err(fail("11F9 echo does not repeat the movement"));
        }
        Ok(String::new())
    }

    /// Sends a chat line and returns the text of the 03E9 reply (command output or echo).
    fn chat(&mut self, text: &str) -> io::Result<String> {
        let mut payload = Vec::new();
        put_tstr(&mut payload, text);
        let map = self
            .map
            .as_mut()
            .ok_or_else(|| fail("chat needs enter first"))?;
        map.send(0x03e8, &payload)?;
        let packets = map.read_until(0x03e9, 8)?;
        let reply = packets.last().unwrap();
        let len = *reply.get(8).ok_or_else(|| fail("03E9 too short"))? as usize;
        let message = reply
            .get(9..8 + len)
            .ok_or_else(|| fail("03E9 text truncated"))?;
        let message = String::from_utf8_lossy(message)
            .trim_end_matches('\0')
            .to_string();
        let teleported = packets.iter().any(|p| opcode(p) == 0x11f9);
        Ok(format!(
            "  \"{message}\"{}",
            if teleported { " (teleport)" } else { "" }
        ))
    }

    fn npc(&mut self, event_id: u32) -> io::Result<String> {
        self.simple(
            0x05e6,
            &event_id.to_be_bytes(),
            &[0x05dc, 0x05e8, 0x03f8, 0x03f7, 0x03f9, 0x05dd],
        )
    }

    fn simple(&mut self, opcode: u16, payload: &[u8], expect: &[u16]) -> io::Result<String> {
        let map = self.map.as_mut().ok_or_else(|| fail("needs enter first"))?;
        map.send(opcode, payload)?;
        map.expect(expect)?;
        Ok(String::new())
    }

    fn expect_spawn(&self, x: u8, y: u8, map: Option<&String>) -> io::Result<String> {
        let spawn = self
            .spawn
            .ok_or_else(|| fail("no spawn recorded; enter first"))?;
        check_position("spawn", (spawn.0, spawn.1, spawn.2), x, y, map)
    }

    fn expect_saved(&self, x: u8, y: u8, map: Option<&String>) -> io::Result<String> {
        let character = Store::open_existing(&self.save)?
            .characters(ACCOUNT_ID)?
            .into_iter()
            .find(|c| c.slot == self.slot)
            .ok_or_else(|| fail("no character in the selected slot"))?;
        check_position(
            "saved",
            (character.map_id, character.x, character.y),
            x,
            y,
            map,
        )
    }

    fn character(&self) -> io::Result<u32> {
        Store::open_existing(&self.save)?
            .characters(ACCOUNT_ID)?
            .into_iter()
            .find(|c| c.slot == self.slot)
            .map(|c| c.id)
            .ok_or_else(|| fail("no character in the selected slot"))
    }

    fn auth_payload(&self, (front, back): (u32, u32)) -> Vec<u8> {
        let mut digest = Sha1::new();
        digest.update(format!("{front}{}{back}", self.token).as_bytes());
        let hash = format!("{:x}", digest.finalize());
        let mut payload = Vec::new();
        put_tstr(&mut payload, &self.account);
        put_tstr(&mut payload, &hash);
        payload.extend_from_slice(&[0, 0, 0, 0, 0]);
        payload
    }

    fn close_map(&mut self) {
        if let Some(map) = self.map.take() {
            map.close();
        }
    }

    fn close_all(&mut self) {
        self.close_map();
        if let Some(login) = self.login.take() {
            login.close();
        }
    }
}

fn check_position(
    what: &str,
    actual: (u32, u8, u8),
    x: u8,
    y: u8,
    map: Option<&String>,
) -> io::Result<String> {
    let map_ok = match map {
        Some(map) => parse_number(map)? == actual.0,
        None => true,
    };
    if map_ok && (actual.1, actual.2) == (x, y) {
        Ok(String::new())
    } else {
        Err(fail(&format!(
            "{what} position is map {} ({},{})",
            actual.0, actual.1, actual.2
        )))
    }
}

/// Reads map ID and tile position from a 01FF self-actor packet.
fn parse_actor_position(packet: &[u8]) -> io::Result<(u32, u8, u8, u8)> {
    let body = &packet[4..];
    // ServerCharaID, CharaID, Unk1, Unk, then the TSTR name (length includes NUL).
    let name_len = *body.get(13).ok_or_else(|| fail("01FF too short"))? as usize;
    // Race, Form, Sex, Hair(2), HairColor, Wig(2), Extra, Face(2), RebirthLv, Ex, Wing, WingColor.
    let at = 14 + name_len + 3 + 2 + 1 + 2 + 1 + 2 + 4;
    let map_id = be_u32(body.get(at..).ok_or_else(|| fail("01FF too short"))?)?;
    let tail = body
        .get(at + 4..at + 7)
        .ok_or_else(|| fail("01FF too short"))?;
    Ok((map_id, tail[0], tail[1], tail[2]))
}

// ---------------------------------------------------------------- client

struct Client {
    stream: TcpStream,
    key: [u8; 16],
}

impl Client {
    fn connect(port: u16) -> io::Result<Self> {
        let address = SocketAddr::from(([127, 0, 0, 1], port));
        let mut stream = TcpStream::connect_timeout(&address, READ_TIMEOUT)?;
        stream.set_read_timeout(Some(READ_TIMEOUT))?;
        stream.set_write_timeout(Some(READ_TIMEOUT))?;
        stream.write_all(&INIT_PACKET)?;
        let server_key = read_exact(&mut stream, 529)?;
        if server_key[..8] != [0, 0, 0, 0, 0, 0, 0, 1] {
            return Err(fail("unexpected key exchange header"));
        }
        let public_hex = std::str::from_utf8(&server_key[273..529])
            .map_err(|_| fail("server public key is not text"))?;
        let server_public =
            BigUint::from_str_radix(public_hex, 16).map_err(|_| fail("bad server public key"))?;
        let prime = BigUint::from_str_radix(PRIME_DECIMAL, 10).unwrap();
        let client_private = BigUint::from(0x1234_5677u32);
        let client_public = BigUint::from(3u8).modpow(&client_private, &prime);
        let mut request = Vec::with_capacity(260);
        request.extend_from_slice(&256u32.to_be_bytes());
        request.extend_from_slice(format!("{client_public:0>256x}").as_bytes());
        stream.write_all(&request)?;
        let key = aes_key_from_shared(&server_public.modpow(&client_private, &prime))?;
        Ok(Self { stream, key })
    }

    fn send(&mut self, opcode: u16, payload: &[u8]) -> io::Result<()> {
        let frame = encrypt_frame(&make_packet(opcode, payload), &self.key)?;
        self.stream.write_all(&frame)
    }

    fn read_packet(&mut self) -> io::Result<Vec<u8>> {
        let header = read_exact(&mut self.stream, 8)?;
        let encrypted_len = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        let mut frame = header;
        frame.extend_from_slice(&read_exact(&mut self.stream, encrypted_len)?);
        Ok(decrypt_frame(&frame, &self.key)?.0)
    }

    /// Reads exactly `expected.len()` packets and checks their opcodes in order.
    fn expect(&mut self, expected: &[u16]) -> io::Result<Vec<Vec<u8>>> {
        let mut packets = Vec::with_capacity(expected.len());
        for _ in expected {
            match self.read_packet() {
                Ok(packet) => packets.push(packet),
                Err(error) => {
                    return Err(fail(&format!(
                        "expected {} but got {} then {}",
                        hex_list(expected),
                        hex_list(&opcodes(&packets)),
                        describe(&error)
                    )))
                }
            }
        }
        if opcodes(&packets) != expected {
            return Err(fail(&format!(
                "expected {} but got {}",
                hex_list(expected),
                hex_list(&opcodes(&packets))
            )));
        }
        Ok(packets)
    }

    fn read_until(&mut self, last: u16, limit: usize) -> io::Result<Vec<Vec<u8>>> {
        let mut packets = Vec::new();
        while packets.len() < limit {
            let packet = self.read_packet().map_err(|error| {
                fail(&format!(
                    "waiting for {last:04x}: got {} then {}",
                    hex_list(&opcodes(&packets)),
                    describe(&error)
                ))
            })?;
            let done = opcode(&packet) == last;
            packets.push(packet);
            if done {
                return Ok(packets);
            }
        }
        Err(fail(&format!("no {last:04x} within {limit} packets")))
    }

    fn close(self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

fn describe(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => "timeout".into(),
        io::ErrorKind::UnexpectedEof
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::ConnectionAborted => "server closed the connection".into(),
        _ => error.to_string(),
    }
}

fn read_exact(stream: &mut TcpStream, length: usize) -> io::Result<Vec<u8>> {
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes)?;
    Ok(bytes)
}

// ---------------------------------------------------------------- servers

struct Ports {
    world: u16,
    login: u16,
    map: u16,
}

impl Ports {
    fn pick() -> io::Result<Self> {
        let listeners = [
            TcpListener::bind(("127.0.0.1", 0))?,
            TcpListener::bind(("127.0.0.1", 0))?,
            TcpListener::bind(("127.0.0.1", 0))?,
        ];
        let port = |l: &TcpListener| l.local_addr().map(|a| a.port());
        Ok(Self {
            world: port(&listeners[0])?,
            login: port(&listeners[1])?,
            map: port(&listeners[2])?,
        })
    }
}

struct Servers {
    children: Vec<Child>,
}

impl Servers {
    fn start(
        ports: &Ports,
        save: &Path,
        maps: &[PathBuf],
        cwd: &Path,
        logs: &Path,
    ) -> io::Result<Self> {
        let exe = std::env::current_exe()?;
        let save = save.to_string_lossy().into_owned();
        let mut servers = Self {
            children: Vec::new(),
        };
        let mut map_args = vec!["serve-map".to_owned(), ports.map.to_string(), save.clone()];
        map_args.extend(maps.iter().map(|m| m.to_string_lossy().into_owned()));
        let roles = [
            ("map", map_args, ports.map),
            (
                "login",
                vec!["serve-login".into(), ports.login.to_string(), save.clone()],
                ports.login,
            ),
            (
                "world",
                vec!["serve-world".into(), ports.world.to_string(), save],
                ports.world,
            ),
        ];
        for (name, args, port) in roles {
            let out = fs::File::create(logs.join(format!("{name}.log")))?;
            let err = out.try_clone()?;
            let child = Command::new(&exe)
                .args(&args)
                .current_dir(cwd)
                .stdin(Stdio::null())
                .stdout(out)
                .stderr(err)
                .spawn()?;
            servers.children.push(child);
            if let Err(error) = wait_listening(port) {
                servers.stop();
                return Err(fail(&format!("{name} server did not start: {error}")));
            }
        }
        Ok(servers)
    }

    fn all_alive(&mut self) -> bool {
        self.children
            .iter_mut()
            .all(|child| matches!(child.try_wait(), Ok(None)))
    }

    fn stop(&mut self) {
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.children.clear();
    }
}

impl Drop for Servers {
    fn drop(&mut self) {
        self.stop();
    }
}

fn wait_listening(port: u16) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    loop {
        // The probe connection closes at once; servers treat it like the launcher's health check.
        match TcpStream::connect_timeout(&address, Duration::from_millis(200)) {
            Ok(stream) => {
                drop(stream);
                return Ok(());
            }
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

// ---------------------------------------------------------------- helpers

fn offline_root() -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    exe.ancestors()
        .find(|dir| dir.join("Cargo.toml").is_file() && dir.join("data").is_dir())
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
        .ok_or_else(|| fail("cannot locate the offline folder"))
}

fn map_files(data: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = fs::read_dir(data)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "map"))
        .collect();
    files.sort();
    if files.is_empty() {
        return Err(fail("no .map files in offline/data"));
    }
    Ok(files)
}

fn make_workdir() -> io::Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!(
        "eco-reborn-autotest-{}-{}",
        std::process::id(),
        now_secs()
    ));
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn parse_number(text: &str) -> io::Result<u32> {
    let parsed = match text.strip_prefix("0x") {
        Some(hex) => u32::from_str_radix(hex, 16),
        None => text.parse(),
    };
    parsed.map_err(|_| fail(&format!("'{text}' is not a number")))
}

fn parse_hex(text: &str) -> io::Result<Vec<u8>> {
    if text.len() % 2 != 0 {
        return Err(fail("hex payload needs an even number of digits"));
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).map_err(|_| fail("bad hex payload")))
        .collect()
}

fn put_tstr(payload: &mut Vec<u8>, value: &str) {
    payload.push((value.len() + 1) as u8);
    payload.extend_from_slice(value.as_bytes());
    payload.push(0);
}

fn pair(bytes: &[u8]) -> io::Result<(u32, u32)> {
    Ok((be_u32(bytes)?, be_u32(bytes.get(4..).unwrap_or(&[]))?))
}

fn be_u32(bytes: &[u8]) -> io::Result<u32> {
    bytes
        .get(..4)
        .map(|b| u32::from_be_bytes(b.try_into().unwrap()))
        .ok_or_else(|| fail("packet too short"))
}

fn expect_zero(packet: &[u8], what: &str) -> io::Result<()> {
    if be_u32(&packet[4..])? == 0 {
        Ok(())
    } else {
        Err(fail(&format!("{what} was refused")))
    }
}

fn opcode(packet: &[u8]) -> u16 {
    u16::from_be_bytes([packet[2], packet[3]])
}

fn opcodes(packets: &[Vec<u8>]) -> Vec<u16> {
    packets.iter().map(|p| opcode(p)).collect()
}

fn hex_list(ops: &[u16]) -> String {
    if ops.is_empty() {
        return "nothing".into();
    }
    ops.iter()
        .map(|op| format!("{op:04x}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn fail(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Other, message.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenario_parser_nests_repeats_and_skips_comments() {
        let steps = parse(
            "login # comment\n\nrepeat 3\n move 1 2\n repeat 2\n  npc 5\n end\nend\nlogout\n",
        )
        .unwrap();
        assert_eq!(steps.len(), 3);
        match &steps[1] {
            Step::Repeat { count, body, .. } => {
                assert_eq!(*count, 3);
                assert_eq!(body.len(), 2);
                assert!(matches!(&body[1], Step::Repeat { count: 2, .. }));
            }
            _ => panic!("expected repeat"),
        }
        assert!(parse("repeat 2\nlogin\n").is_err());
        assert!(parse("end\n").is_err());
        assert!(parse("repeat 0\nend\n").is_err());
    }

    #[test]
    fn actor_position_follows_the_01ff_layout() {
        let character = crate::store::Character {
            id: 1,
            account_id: 1,
            slot: 0,
            name: "Offline".into(),
            race: 0,
            sex: 1,
            hair: 7,
            hair_color: 50,
            face: 0,
            map_id: 10_023_000,
            x: 130,
            y: 139,
            direction: 4,
        };
        let packets = crate::map::bootstrap(&character).unwrap();
        let actor = packets.iter().find(|p| opcode(p) == 0x01ff).unwrap();
        assert_eq!(
            parse_actor_position(actor).unwrap(),
            (10_023_000, 130, 139, 4)
        );
    }

    #[test]
    fn numbers_and_hex_payloads_parse() {
        assert_eq!(parse_number("0x05e6").unwrap(), 0x05e6);
        assert_eq!(parse_number("11000155").unwrap(), 11_000_155);
        assert!(parse_number("x").is_err());
        assert_eq!(parse_hex("00ff10").unwrap(), [0, 255, 16]);
        assert!(parse_hex("0").is_err());
    }
}
