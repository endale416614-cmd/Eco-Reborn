//! Headless end-to-end test across the local Gate, Login, and Map listeners.
use crate::protocol::{
    aes_key_from_shared, decrypt_frame, encrypt_frame, make_packet, PRIME_DECIMAL,
};
use crate::store::Store;
use crate::{serve, ServerRole};
use num_bigint::BigUint;
use num_traits::Num;
use sha1::{Digest, Sha1};
use std::cell::RefCell;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

static NEXT_TEMP: AtomicUsize = AtomicUsize::new(0);

struct TempSave {
    dir: PathBuf,
    file: PathBuf,
}

impl TempSave {
    fn new() -> Self {
        loop {
            let n = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("eco-e2e-{}-{n}", std::process::id()));
            match fs::create_dir(&dir) {
                Ok(()) => {
                    return Self {
                        file: dir.join("test.sqlite"),
                        dir,
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("could not create test save directory: {error}"),
            }
        }
    }
}

impl Drop for TempSave {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn headless_three_service_flow() {
    // Bind all requested fixed endpoints before starting threads. A busy port fails this
    // test safely; it never probes, stops, or kills the process that owns the port.
    let gate_listener = bind_loopback(17831);
    let login_listener = bind_loopback(17832);
    let map_listener = bind_loopback(17833);
    let save = TempSave::new();
    drop(Store::open(&save.file).expect("create isolated test save"));

    let gate_path = save.file.clone();
    let gate = thread::spawn(move || serve_clients(gate_listener, &gate_path, ServerRole::Gate, 1));
    let login_path = save.file.clone();
    let login =
        thread::spawn(move || serve_clients(login_listener, &login_path, ServerRole::Login, 1));
    let map_path = save.file.clone();
    let map = thread::spawn(move || serve_clients(map_listener, &map_path, ServerRole::Map, 2));

    let mut gate_client = Client::connect("127.0.0.1:17831".parse().unwrap());
    let gate_init = gate_client.request_n(0x0001, &[0x55], 3);
    assert_eq!(opcodes(&gate_init), [0xffff, 0x001e, 0x0002]);
    let salts = pair(&gate_init[1][4..]);
    let auth = gate_client.request_n(0x001f, &login_payload("test", &password_hash(salts)), 1);
    assert_eq!(opcodes(&auth), [0x0020]);
    assert_eq!(&auth[0][4..8], &[0, 0, 0, 0]);
    assert_eq!(
        gate_client.request(0x002f, &[]),
        [make_packet(0x0030, &0i32.to_be_bytes())]
    );
    let server_list = gate_client.request_n(0x0031, &[], 4);
    assert_eq!(opcodes(&server_list), [0x0032, 0x0033, 0x0034, 0x0036]);
    assert_eq!(&server_list[0][4..], &1u32.to_be_bytes());
    assert!(server_list[1]
        .windows(15)
        .any(|bytes| bytes == b"127.0.0.1:17832"));
    gate_client.assert_eof();
    gate_client.close();
    assert!(gate.join().unwrap().is_ok());

    let mut login_client = Client::connect("127.0.0.1:17832".parse().unwrap());
    let login_init = login_client.request_n(0x0001, &[0x12, 0x34], 3);
    assert_eq!(opcodes(&login_init), [0xffff, 0x001e, 0x0002]);
    let login_salts = pair(&login_init[1][4..]);
    let auth = login_client.request_n(
        0x001f,
        &login_payload("test", &password_hash(login_salts)),
        3,
    );
    assert_eq!(opcodes(&auth), [0x0020, 0x0028, 0x0029]);
    assert_eq!(&auth[0][4..8], &[0, 0, 0, 0]);

    let mut create = vec![0];
    put_tstr(&mut create, "OffE2E");
    create.extend_from_slice(&[0, 1]);
    create.extend_from_slice(&0x1234u16.to_be_bytes());
    create.push(51);
    create.extend_from_slice(&3u16.to_be_bytes());
    assert_eq!(
        login_client.request(0x00a0, &create),
        [make_packet(0x00a1, &0u32.to_be_bytes())]
    );
    let created = Store::open(&save.file)
        .expect("reopen save after character creation")
        .characters(1)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(created.name, "OffE2E");
    assert_eq!(created.id, 1, "character ID is the durable save identity");
    let map_id = login_client.request(0x00a7, &[0]);
    assert_eq!(map_id, [make_packet(0x00a8, &created.map_id.to_be_bytes())]);
    let redirect = login_client.request(0x0032, &created.map_id.to_be_bytes());
    assert_eq!(opcodes(&redirect), [0x0033]);
    assert!(redirect[0].windows(9).any(|bytes| bytes == b"127.0.0.1"));
    assert_eq!(
        &redirect[0][redirect[0].len() - 4..],
        &17833u32.to_be_bytes()
    );
    assert_eq!(
        login_client.request_n(0x002a, &0u32.to_be_bytes(), 2),
        [make_packet(0x002b, &[]), make_packet(0x00dd, &[1, 1, 0])]
    );
    login_client.close();
    assert!(login.join().unwrap().is_ok());

    let mut map_client = Client::connect("127.0.0.1:17833".parse().unwrap());
    let map_salts = map_client.map_init();
    let map_auth = map_client.request(0x0010, &login_payload("test", &password_hash(map_salts)));
    assert_eq!(opcodes(&map_auth), [0x0011]);
    let entry = map_client.request_n(0x01fd, &[0, 0, 0, 0, 0, 0], 15);
    assert_eq!(
        opcodes(&entry),
        [
            0x1f72, 0x026a, 0x1239, 0x0fa7, 0x1a5f, 0x0203, 0x01ff, 0x0221, 0x021c, 0x157c, 0x0212,
            0x0217, 0x0223, 0x0230, 0x1b67,
        ]
    );
    let actor = entry
        .iter()
        .find(|packet| opcode(packet) == 0x01ff)
        .unwrap();
    assert_eq!(&actor[4..8], &16u32.to_be_bytes()); // Session actor ID
    assert_eq!(&actor[8..12], &created.id.to_be_bytes()); // Durable character ID
    assert_eq!(&actor[44..47], &[63, 58, 0]);
    for opcode in [0x1239u16, 0x0fa7, 0x0221, 0x021c, 0x157c, 0x1b67] {
        let actor_addressed = entry
            .iter()
            .find(|packet| self::opcode(packet) == opcode)
            .unwrap();
        assert_eq!(
            &actor_addressed[4..8],
            &16u32.to_be_bytes(),
            "opcode {opcode:04x}"
        );
    }
    assert_eq!(
        map_client.request(0x11fe, &[]),
        [make_packet(0x1239, &[0, 0, 0, 16, 1, 164])]
    );
    let movement = [0xff, 0xce, 0, 150, 0, 90, 0, 7];
    let moved = map_client.request(0x11f8, &movement);
    assert_eq!(opcodes(&moved), [0x11f9]);
    assert_eq!(&moved[0][4..8], &16u32.to_be_bytes());
    assert_eq!(&moved[0][8..], &movement);
    map_client.close();

    let mut relogin = Client::connect("127.0.0.1:17833".parse().unwrap());
    let relogin_salts = relogin.map_init();
    assert_eq!(
        opcodes(&relogin.request(
            0x0010,
            &login_payload("test", &password_hash(relogin_salts))
        )),
        [0x0011]
    );
    let relogged_entry = relogin.request_n(0x01fd, &[0, 0, 0, 0, 0, 0], 15);
    let relogged_actor = relogged_entry
        .iter()
        .find(|packet| opcode(packet) == 0x01ff)
        .unwrap();
    assert_eq!(&relogged_actor[4..8], &16u32.to_be_bytes());
    assert_eq!(&relogged_actor[8..12], &created.id.to_be_bytes());
    assert_eq!(&relogged_actor[44..47], &[63, 62, 2]);
    relogin.close();
    assert!(map.join().unwrap().is_ok());

    let persisted = Store::open(&save.file)
        .expect("reopen save after map movement")
        .characters(1)
        .unwrap()
        .remove(0);
    assert_eq!(
        persisted.id, 1,
        "wire actor ID must not replace durable identity"
    );
    assert_eq!((persisted.x, persisted.y, persisted.direction), (63, 62, 2));
}

fn bind_loopback(port: u16) -> TcpListener {
    TcpListener::bind(("127.0.0.1", port))
        .unwrap_or_else(|error| panic!("cannot bind test listener on 127.0.0.1:{port}: {error}"))
}

fn serve_clients(
    listener: TcpListener,
    save: &Path,
    role: ServerRole,
    count: usize,
) -> io::Result<()> {
    let maps = if role == ServerRole::Map {
        Some(Rc::new(crate::terrain::Maps::load(&[Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/20013003.map"
        ))])?))
    } else {
        None
    };
    for _ in 0..count {
        let (stream, _) = listener.accept()?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
        let store = Rc::new(RefCell::new(Store::open(save)?));
        serve(stream, store, role, maps.clone())?;
    }
    Ok(())
}

struct Client {
    stream: TcpStream,
    key: [u8; 16],
}

impl Client {
    fn connect(address: SocketAddr) -> Self {
        let mut stream = TcpStream::connect(address).expect("connect loopback service");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(&crate::protocol::INIT_PACKET).unwrap();
        let server_key = read_exact(&mut stream, 529);
        assert_eq!(&server_key[..8], &[0, 0, 0, 0, 0, 0, 0, 1]);
        let public_hex = std::str::from_utf8(&server_key[273..529]).unwrap();
        let server_public = BigUint::from_str_radix(public_hex, 16).unwrap();
        let prime = BigUint::from_str_radix(PRIME_DECIMAL, 10).unwrap();
        let client_private = BigUint::from(5u8);
        let client_public = BigUint::from(3u8).modpow(&client_private, &prime);
        let mut request = Vec::with_capacity(260);
        request.extend_from_slice(&256u32.to_be_bytes());
        request.extend_from_slice(format!("{client_public:0>256x}").as_bytes());
        stream.write_all(&request).unwrap();
        let key = aes_key_from_shared(&server_public.modpow(&client_private, &prime)).unwrap();
        Self { stream, key }
    }

    fn request(&mut self, opcode: u16, payload: &[u8]) -> Vec<Vec<u8>> {
        self.request_n(opcode, payload, 1)
    }

    fn request_n(&mut self, opcode: u16, payload: &[u8], response_count: usize) -> Vec<Vec<u8>> {
        let frame = encrypt_frame(&make_packet(opcode, payload), &self.key).unwrap();
        self.stream.write_all(&frame).unwrap();
        let mut replies = Vec::with_capacity(response_count);
        for _ in 0..response_count {
            replies.push(self.read_packet());
        }
        replies
    }

    fn map_init(&mut self) -> (u32, u32) {
        let response = self.request_n(0x000a, &[0x12, 0x34], 3);
        assert_eq!(opcodes(&response), [0xffff, 0x000b, 0x000f]);
        pair(&response[2][4..])
    }

    fn read_packet(&mut self) -> Vec<u8> {
        let header = read_exact(&mut self.stream, 8);
        let encrypted_len = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        let mut frame = header;
        frame.extend_from_slice(&read_exact(&mut self.stream, encrypted_len));
        decrypt_frame(&frame, &self.key).unwrap().0
    }

    fn close(self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }

    fn assert_eof(&mut self) {
        let mut byte = [0];
        assert_eq!(
            self.stream.read(&mut byte).expect("read server-list EOF"),
            0
        );
    }
}

fn read_exact(stream: &mut TcpStream, length: usize) -> Vec<u8> {
    let mut bytes = vec![0; length];
    stream
        .read_exact(&mut bytes)
        .expect("read complete protocol bytes");
    bytes
}

fn pair(bytes: &[u8]) -> (u32, u32) {
    (
        u32::from_be_bytes(bytes[..4].try_into().unwrap()),
        u32::from_be_bytes(bytes[4..8].try_into().unwrap()),
    )
}

fn login_payload(username: &str, password: &str) -> Vec<u8> {
    let mut payload = Vec::new();
    put_tstr(&mut payload, username);
    put_tstr(&mut payload, password);
    payload.extend_from_slice(&[0, 0, 0, 0, 0]);
    payload
}

fn put_tstr(payload: &mut Vec<u8>, value: &str) {
    payload.push((value.len() + 1) as u8);
    payload.extend_from_slice(value.as_bytes());
    payload.push(0);
}

fn password_hash((front, back): (u32, u32)) -> String {
    let mut digest = Sha1::new();
    // Independent client-side fixture token matching the seeded demo account.
    digest.update(format!("{front}96e79218965eb72c92a549dd5a330112{back}").as_bytes());
    format!("{:x}", digest.finalize())
}

fn opcode(packet: &[u8]) -> u16 {
    u16::from_be_bytes([packet[2], packet[3]])
}

fn opcodes(packets: &[Vec<u8>]) -> Vec<u16> {
    packets.iter().map(|packet| opcode(packet)).collect()
}
