//! Opt-in private packet trace for local debugging.
//!
//! Off unless `research/local-trace/ENABLED` exists under the working directory.
//! Trace files stay on this PC; they are not diagnostics reports and must not be shared.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const DIR: &str = "research/local-trace";
const MAX_HEX_BYTES: usize = 512;

struct Trace {
    file: Mutex<File>,
}

static TRACE: OnceLock<Option<Trace>> = OnceLock::new();
static ROLE: OnceLock<&'static str> = OnceLock::new();

/// Turns tracing off for this process (the autotest client shares the server's codec).
pub fn disable() {
    let _ = TRACE.set(None);
}

/// Names the trace file after the server role; call before the first packet.
pub fn set_role(role: &'static str) {
    let _ = ROLE.set(role);
}

/// Records one plaintext packet: wall-clock ms, direction, opcode, length and leading bytes.
pub fn packet(direction: &str, packet: &[u8]) {
    let Some(trace) = TRACE.get_or_init(open) else {
        return;
    };
    let opcode = match packet {
        [_, _, high, low, ..] => u16::from_be_bytes([*high, *low]),
        _ => 0,
    };
    let shown = &packet[..packet.len().min(MAX_HEX_BYTES)];
    let hex: String = shown.iter().map(|byte| format!("{byte:02x}")).collect();
    let truncated = if shown.len() < packet.len() {
        "..."
    } else {
        ""
    };
    if let Ok(mut file) = trace.file.lock() {
        let _ = writeln!(
            file,
            "{} {direction} {opcode:04x} len={} {hex}{truncated}",
            now_ms(),
            packet.len()
        );
    }
}

fn open() -> Option<Trace> {
    if cfg!(test) {
        return None;
    }
    let dir = Path::new(DIR);
    if !dir.join("ENABLED").is_file() {
        return None;
    }
    let role = ROLE.get().copied().unwrap_or("server");
    let name = format!(
        "trace-{}-{}-{role}.log",
        now_ms() / 1000,
        std::process::id()
    );
    let file = OpenOptions::new()
        .create_new(true)
        .append(true)
        .open(dir.join(name))
        .ok()?;
    Some(Trace {
        file: Mutex::new(file),
    })
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or(0)
}
