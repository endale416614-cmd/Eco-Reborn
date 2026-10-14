"""Local debug tool for the Eco Reborn offline server: status, packet trace timeline, opcode names, session reports.

Read-only. Packet traces are private to this PC and must never be shared or uploaded.
"""
import argparse
import csv
import html
import json
import re
import sqlite3
import statistics
import subprocess
import sys
import time
from collections import Counter, defaultdict
from pathlib import Path

HERE = Path(__file__).resolve().parent
OFFLINE = HERE.parents[1]
WORKSPACE = OFFLINE.parent
OPCODES_JSON = HERE / "opcodes.json"
TRACE_DIR = OFFLINE / "research" / "local-trace"
REPORT_DIR = OFFLINE / "reports"
SAVE_DB = OFFLINE / "saves" / "world.sqlite"
MAPNAME_CSV = OFFLINE / "translations" / "source-ja" / "table" / "mapname.csv"
ISOLATION_PS1 = OFFLINE / "Set-RebornIsolation.ps1"
PORTS = {17831: "map", 17832: "login", 17833: "world"}
SESSION_WINDOW_S = 120
REPLY_WINDOW_MS = 1000
KEEPALIVE_OPCODES = {0x000A, 0x000B, 0x0032, 0x0033}
EPOCH_MS_MIN = 10 ** 12
ROLES = ("map", "login", "world")

# Names for opcodes the Rust server uses that the reference trees miss or name differently.
# Applies to every role; a role entry in HAND_ROLE wins over these.
HAND = {
    "client": {
        0x0001: "KeyExchangeInit",
        0x11F8: "CharaMove",
        0x11FE: "RequestMove (map loaded)",
        0x13C7: "FinishMapMove",
        0x0FA5: "BattleStatus",
        0x121B: "ChatMotion",
        0x05E6: "NpcEventStart",
        0x05E8: "CurrentEvent",
    },
    "server": {
        0xFFFF: "FirstAck",
        0x1B67: "LoginFinished / ChangeMapFinish",
        0x11F9: "CharaMoveEcho",
        0x0FA6: "BattleStatus",
        0x121C: "ChatMotion",
        0x05DC: "EventStart",
        0x05DD: "EventEnd",
        0x03F7: "NpcMessage",
        0x03F8: "NpcMessageStart",
        0x03F9: "NpcMessageEnd",
    },
}
# Map-only: 0x001F/0x0020 are Logout there but authentication on gate/login, and the
# ping pairs are map traffic (0x0032/0x0033 carry the server address on login).
HAND_ROLE = {
    "map": {
        "client": {0x001F: "Logout", 0x000A: "ConnectionCheck", 0x0032: "ConnectionCheck2"},
        "server": {0x0020: "Logout", 0x000B: "ConnectionCheckAck", 0x0033: "ConnectionCheck2Ack"},
    },
}

CMD_RE = re.compile(r"commandTable\.Add\(\(ushort\)(0x[0-9a-fA-F]+|\d+),\s*(?:\(Packet\))?\s*new (\w+)\(")
CLASS_RE = re.compile(r"\bclass\s+(\w+)")
ID_RE = re.compile(r"\bthis\.ID\s*=\s*\(ushort\)(0x[0-9a-fA-F]+|\d+)\s*;")
TRACE_NAME_RE = re.compile(r"^trace-(\d+)-(\d+)(?:-([A-Za-z]+))?\.log$")
TRACE_LINE_RE = re.compile(r"^\s*(\d+) (C>S|S>C) ([0-9a-fA-F]{4}) len=(\d+) ([0-9a-fA-F]*)(\.\.\.)?\s*$")
STAGE_RE = re.compile(r"^\+(\d+)s stage=(\w+) outcome=(\w+)(?: category=(\w+))?")
EVENT_RE = re.compile(r"^protocol event=(\w+)(.*)$")


# ---------------------------------------------------------------- opcode names

def key_of(opcode):
    return f"{opcode:04x}"


def to_int(text):
    return int(text, 16) if text.lower().startswith("0x") else int(text)


def add_candidate(table, opcode, name, source):
    table.setdefault(key_of(opcode), []).append((name, source))


def collapse(table):
    """First candidate is the name, the other distinct names become aliases."""
    out = {}
    for key in sorted(table):
        (name, source), rest = table[key][0], table[key][1:]
        aliases = []
        for other, _ in rest:
            if other != name and other not in aliases:
                aliases.append(other)
        out[key] = {"name": name, "aliases": aliases, "source": source}
    return out


def read_text(path):
    return path.read_text(encoding="utf-8-sig", errors="replace")


def scan_command_table(path, rel, table):
    if not path.is_file():
        return
    for match in CMD_RE.finditer(read_text(path)):
        if match[2] != "Packet":
            add_candidate(table, to_int(match[1]), match[2], rel)


def scan_server_packets(folder, base, table):
    for path in sorted(folder.rglob("*.cs")):
        text = read_text(path)
        cls = CLASS_RE.search(text)
        if not cls:
            continue
        rel = path.relative_to(base).as_posix()
        for match in ID_RE.finditer(text):
            add_candidate(table, to_int(match[1]), cls[1], rel)


def scan_interface(path, base, table):
    if not path.is_file():
        return
    rel = path.relative_to(base).as_posix()
    for row in json.loads(read_text(path)):
        add_candidate(table, int(row["ID"], 16), row["InterfaceName"], rel)


def rebuild(research):
    """Regenerate the opcode tables from the SagaECO and EcoServerEmulator source trees."""
    saga, emu = research / "SagaECO", research / "EcoServerEmulator"
    if not saga.is_dir():
        raise SystemExit(f"missing reference tree: {saga}")
    map_client, login_client, world_client = {}, {}, {}
    map_server, login_server = {}, {}
    scan_interface(emu / "MapServer" / "MapServerInterface.json", research, map_client)
    scan_command_table(saga / "SagaMap" / "Manager" / "MapClientManager.cs", "SagaECO/SagaMap/Manager/MapClientManager.cs", map_client)
    scan_interface(emu / "LoginServer" / "LoginServerInterface.json", research, login_client)
    scan_command_table(saga / "SagaLogin" / "Manager" / "LoginClientManager.cs", "SagaECO/SagaLogin/Manager/LoginClientManager.cs", login_client)
    scan_interface(emu / "WorldServer" / "WorldServerInterface.json", research, world_client)
    scan_server_packets(saga / "SagaMap" / "Packets" / "Server", research, map_server)
    scan_server_packets(saga / "SagaLogin" / "Packets" / "Server", research, login_server)
    return {
        "client": collapse(map_client),
        "server": collapse(map_server),
        "role_overrides": {
            "login": {"client": collapse(login_client), "server": collapse(login_server)},
            "world": {"client": collapse(world_client), "server": {}},
        },
    }


class Opcodes:
    """Name lookup: hand overrides, then the role's own table, then the map table as a fallback."""

    def __init__(self, data=None):
        self.data = data or {"client": {}, "server": {}, "role_overrides": {}}

    @classmethod
    def load(cls, path=OPCODES_JSON):
        try:
            return cls(json.loads(path.read_text(encoding="utf-8")))
        except (OSError, ValueError):
            return cls()

    def tables(self, role, direction):
        own = self.data.get("role_overrides", {}).get(role, {}).get(direction, {})
        return own, self.data.get(direction, {})

    def lookup(self, role, direction, opcode):
        """Return (info, exact) or (None, False). A fallback hit comes from the map table for another role.

        A file with no known role (old traces, the Rust default "server") is tried against every role.
        """
        if role not in ROLES:
            for each in ROLES:
                info, exact = self.lookup(each, direction, opcode)
                if info and exact:
                    return info, True
            return None, False
        key = key_of(opcode)
        own, base = self.tables(role, direction)
        found, exact = own.get(key), True
        if found is None:
            found, exact = base.get(key), role == "map"
        hand = HAND_ROLE.get(role, {}).get(direction, {}).get(opcode) or HAND[direction].get(opcode)
        if hand:
            aliases = [n for n in ([found["name"]] + found["aliases"] if found else []) if n != hand]
            return {"name": hand, "aliases": aliases, "source": "hand override"}, True
        return (found, exact) if found else (None, False)

    def name(self, role, direction, opcode):
        if role not in ROLES:
            names = []
            for each in ROLES:
                info, exact = self.lookup(each, direction, opcode)
                if info and exact and info["name"] not in names:
                    names.append(info["name"])
            return "|".join(names) or "?"
        info, exact = self.lookup(role, direction, opcode)
        if info is None:
            return "?"
        return info["name"] if exact else info["name"] + "?"

    def counts(self):
        roles = self.data.get("role_overrides", {})
        return {
            "client": len(self.data.get("client", {})),
            "server": len(self.data.get("server", {})),
            **{f"{r}.{d}": len(roles.get(r, {}).get(d, {})) for r in roles for d in ("client", "server")},
        }


def cmd_opcodes(args):
    if args.rebuild:
        data = rebuild(Path(args.research))
        OPCODES_JSON.write_text(json.dumps(data, indent=1, ensure_ascii=False, sort_keys=True) + "\n", encoding="utf-8")
        print(f"wrote {OPCODES_JSON}")
        for name, count in Opcodes(data).counts().items():
            print(f"  {name}: {count}")
        return 0
    opcodes = Opcodes.load()
    if not args.hex:
        for name, count in opcodes.counts().items():
            print(f"{name}: {count}")
        return 0
    for text in args.hex:
        try:
            opcode = int(text, 16)
        except ValueError:
            print(f"{text}: not a hex opcode")
            continue
        shown = 0
        for role in ROLES:
            for direction, arrow in (("client", "C>S"), ("server", "S>C")):
                info, exact = opcodes.lookup(role, direction, opcode)
                # fallback hits just repeat the map row, so show only role-specific ones
                if info is None or (role != "map" and not exact):
                    continue
                if role != "map" and info == opcodes.lookup("map", direction, opcode)[0]:
                    continue
                alias = f"  aliases: {', '.join(info['aliases'])}" if info["aliases"] else ""
                print(f"{key_of(opcode)} {role:<5} {arrow} {info['name']}{alias}  [{info['source']}]")
                shown += 1
        if not shown:
            print(f"{key_of(opcode)} not in opcodes.json")
    return 0


# ---------------------------------------------------------------- trace

class TraceFile:
    def __init__(self, path, stamp, pid, role):
        self.path, self.stamp, self.pid, self.role = Path(path), stamp, pid, role

    @classmethod
    def from_path(cls, path):
        path = Path(path)
        match = TRACE_NAME_RE.match(path.name)
        if not match:
            return cls(path, 0, 0, "?")
        return cls(path, int(match[1]), int(match[2]), (match[3] or "?").lower())


class Packet:
    __slots__ = ("t", "role", "dir", "opcode", "length", "hex", "truncated", "src")

    def __init__(self, t, role, dir_, opcode, length, hex_, truncated, src):
        self.t, self.role, self.dir, self.opcode = t, role, dir_, opcode
        self.length, self.hex, self.truncated, self.src = length, hex_, truncated, src

    @property
    def payload_hex(self):
        return self.hex[8:]


def parse_line(line, tf):
    """One trace line -> Packet; old files count ms from process start, so offset them by the file stamp."""
    match = TRACE_LINE_RE.match(line)
    if not match:
        return None
    ms = int(match[1])
    if ms < EPOCH_MS_MIN:
        ms += tf.stamp * 1000
    return Packet(ms, tf.role, match[2], int(match[3], 16), int(match[4]), match[5].lower(), bool(match[6]), tf.path.name)


def parse_text(text, tf):
    packets, bad = [], 0
    for line in text.splitlines():
        if not line.strip():
            continue
        packet = parse_line(line, tf)
        if packet:
            packets.append(packet)
        else:
            bad += 1
    return packets, bad


def parse_trace_file(tf):
    return parse_text(tf.path.read_text(encoding="ascii", errors="replace"), tf)


def list_trace_files(folder=TRACE_DIR):
    files = [TraceFile.from_path(p) for p in folder.glob("trace-*.log")] if folder.is_dir() else []
    return sorted(files, key=lambda f: (f.stamp, f.pid, f.path.name))


def select_files(files, *, everything=False, since_minutes=None, window=SESSION_WINDOW_S, now=None):
    if not files:
        return []
    if everything:
        return list(files)
    if since_minutes is not None:
        cutoff = (now if now is not None else time.time()) - since_minutes * 60
        return [f for f in files if f.path.stat().st_mtime >= cutoff]
    newest = max(f.stamp for f in files)
    return [f for f in files if f.stamp >= newest - window]


def load_packets(files):
    """Merge several files into one timeline; the sort is stable so same-ms packets keep file order."""
    packets, bad = [], 0
    for tf in files:
        got, skipped = parse_trace_file(tf)
        packets.extend(got)
        bad += skipped
    packets.sort(key=lambda p: p.t)
    return packets, bad


def is_keepalive(packet):
    return packet.opcode in KEEPALIVE_OPCODES and packet.length == 8 and packet.payload_hex == "00000000"


def analyze(packets, opcodes, window_ms=REPLY_WINDOW_MS):
    real = [p for p in packets if not is_keepalive(p)]
    roles = defaultdict(Counter)
    for p in packets:
        roles[p.role]["keepalive" if is_keepalive(p) else p.dir] += 1
    streams = defaultdict(list)
    for p in real:
        streams[p.src].append(p)
    unanswered, latency, unanswered_packets = Counter(), defaultdict(list), []
    for stream in streams.values():
        last = stream[-1].t
        # Match by stream order, not timestamp: a request often lands in the same
        # millisecond as the previous reply, which must not count as its answer.
        next_reply, following = [None] * len(stream), None
        for k in range(len(stream) - 1, -1, -1):
            next_reply[k] = following
            if stream[k].dir == "S>C":
                following = stream[k].t
        for k, p in enumerate(stream):
            if p.dir != "C>S":
                continue
            delay = next_reply[k] - p.t if next_reply[k] is not None else None
            if delay is not None:
                latency[(p.role, p.opcode)].append(delay)
            if (delay is None or delay > window_ms) and p.t + window_ms <= last:
                unanswered[(p.role, p.opcode)] += 1
                unanswered_packets.append(p)
    unknown = Counter()
    for p in packets:
        if opcodes.lookup(p.role, "client" if p.dir == "C>S" else "server", p.opcode)[0] is None:
            unknown[(p.role, p.dir, p.opcode)] += 1
    gaps = sorted(((b.t - a.t, a) for a, b in zip(real, real[1:])), key=lambda g: -g[0])[:5]
    return {"roles": roles, "unanswered": unanswered, "unanswered_packets": unanswered_packets,
            "latency": latency, "unknown": unknown, "gaps": gaps}


def format_packet(p, first_t, opcodes, full=False, marker=""):
    rel = (p.t - first_t) / 1000
    arrow = "-->" if p.dir == "C>S" else "<--"
    shown = p.payload_hex if full else p.payload_hex[:48]
    more = "..." if (not full and len(p.payload_hex) > 48) or p.truncated else ""
    spaced = " ".join(shown[i:i + 2] for i in range(0, len(shown), 2))
    name = opcodes.name(p.role, "client" if p.dir == "C>S" else "server", p.opcode)
    return f"{rel:9.3f} {p.role:<5} {arrow} {p.opcode:04x} {name:<28} len={p.length:<5} {spaced}{more}{marker}"


def format_summary(result, first_t, opcodes):
    out = ["", "== summary =="]
    for role in sorted(result["roles"]):
        c = result["roles"][role]
        out.append(f"{role:<6} C>S {c['C>S']:<6} S>C {c['S>C']:<6} keepalive {c['keepalive']}")
    out.append("")
    if result["unanswered"]:
        out.append(f"client opcodes with no server packet within {REPLY_WINDOW_MS} ms:")
        for (role, op), n in sorted(result["unanswered"].items(), key=lambda kv: -kv[1]):
            out.append(f"  {role:<6} {op:04x} {opcodes.name(role, 'client', op):<28} x{n}")
    else:
        out.append("unanswered client opcodes: none")
    out.append("")
    if result["unknown"]:
        out.append("opcodes not in opcodes.json:")
        for (role, dir_, op), n in sorted(result["unknown"].items(), key=lambda kv: (kv[0][0], kv[0][1], kv[0][2])):
            out.append(f"  {role:<6} {dir_} {op:04x} x{n}")
    else:
        out.append("unknown opcodes: none")
    out.append("")
    out.append("reply latency per client opcode (ms to the first following S>C packet):")
    for (role, op), delays in sorted(result["latency"].items()):
        out.append(f"  {role:<6} {op:04x} {opcodes.name(role, 'client', op):<28} n={len(delays):<4} "
                   f"min {min(delays)}  median {statistics.median(delays):g}  max {max(delays)}")
    out.append("")
    out.append("largest gaps between packets:")
    for gap, p in result["gaps"]:
        out.append(f"  {gap / 1000:9.3f} s after {(p.t - first_t) / 1000:.3f} ({p.role} {p.dir} {p.opcode:04x})")
    return out


def wanted(packet, args, opcode_filter):
    if args.role and packet.role != args.role:
        return False
    if args.hide_keepalive and is_keepalive(packet):
        return False
    return not opcode_filter or packet.opcode in opcode_filter


def write_html(path, shown, first_t, opcodes, unanswered_ids, title):
    rows = []
    for p in shown:
        cls = "c" if p.dir == "C>S" else "s"
        if id(p) in unanswered_ids:
            cls += " u"
        name = opcodes.name(p.role, "client" if p.dir == "C>S" else "server", p.opcode)
        data = p.payload_hex[:96] + ("..." if len(p.payload_hex) > 96 or p.truncated else "")
        rows.append(f'<tr class="{cls}"><td>{(p.t - first_t) / 1000:.3f}</td><td>{p.role}</td><td>{p.dir}</td>'
                    f'<td>{p.opcode:04x}</td><td>{html.escape(name)}</td><td>{p.length}</td><td class="h">{data}</td></tr>')
    page = (
        "<!doctype html><meta charset=utf-8><title>" + html.escape(title) + "</title><style>"
        "body{font:13px Consolas,monospace;margin:16px}table{border-collapse:collapse}"
        "td,th{padding:2px 8px;border-bottom:1px solid #ddd;text-align:left}"
        "tr.c{background:#e8f1ff}tr.s{background:#eefbe8}tr.u td{background:#ffd9d9;font-weight:bold}"
        ".h{color:#555;max-width:60em;word-break:break-all}</style>"
        f"<h3>{html.escape(title)}</h3><p>blue = client to server, green = server to client, red = no server packet within {REPLY_WINDOW_MS} ms. "
        "Private trace data: do not share.</p><table><tr><th>t (s)<th>role<th>dir<th>op<th>name<th>len<th>payload</tr>"
        + "".join(rows) + "</table>")
    Path(path).write_text(page, encoding="utf-8")


class Follower:
    """Tails trace files: each poll returns the packets that appeared since the last one."""

    def __init__(self, folder, window=SESSION_WINDOW_S):
        self.folder, self.window = folder, window
        self.offsets, self.tails = {}, {}

    def poll(self):
        fresh = []
        for tf in select_files(list_trace_files(self.folder), window=self.window):
            try:
                with open(tf.path, "rb") as handle:
                    handle.seek(self.offsets.get(tf.path, 0))
                    chunk = handle.read()
            except OSError:
                continue
            self.offsets[tf.path] = self.offsets.get(tf.path, 0) + len(chunk)
            text = self.tails.get(tf.path, "") + chunk.decode("ascii", errors="replace")
            text, _, self.tails[tf.path] = text.rpartition("\n")
            fresh.extend(parse_text(text, tf)[0])
        fresh.sort(key=lambda p: p.t)
        return fresh


def cmd_trace(args):
    opcodes = Opcodes.load()
    opcode_filter = {int(x, 16) for x in args.opcode.split(",") if x} if args.opcode else set()
    folder = Path(args.dir)
    if args.follow:
        return follow(args, opcodes, opcode_filter, folder)
    if args.files:
        files = [TraceFile.from_path(f if Path(f).exists() else folder / f) for f in args.files]
    else:
        files = select_files(list_trace_files(folder), everything=args.all, since_minutes=args.since, window=args.window)
    if not files:
        print(f"no trace files in {folder} (enable tracing: create ENABLED there, see README.md)")
        return 1
    packets, bad = load_packets(files)
    if args.role:
        packets = [p for p in packets if p.role == args.role]
    if not packets:
        print("selected trace files contain no packets")
        return 1
    first_t = packets[0].t
    result = analyze(packets, opcodes)
    unanswered_ids = {id(p) for p in result["unanswered_packets"]}
    shown = [p for p in packets if wanted(p, args, opcode_filter)]
    print(f"{len(files)} file(s): {', '.join(f.path.name for f in files)}" + (f"  ({bad} unparsable lines)" if bad else ""))
    if not args.summary_only:
        for p in shown:
            print(format_packet(p, first_t, opcodes, args.full, "  (no reply)" if id(p) in unanswered_ids else ""))
    for line in format_summary(result, first_t, opcodes):
        print(line)
    if args.html:
        write_html(args.html, shown, first_t, opcodes, unanswered_ids, "Eco Reborn packet trace")
        print(f"\nwrote {args.html}")
    return 0


def follow(args, opcodes, opcode_filter, folder):
    follower = Follower(folder, args.window)
    first_t = None
    print("following newest trace files, Ctrl+C to stop", flush=True)
    try:
        while True:
            for p in follower.poll():
                first_t = p.t if first_t is None else first_t
                if wanted(p, args, opcode_filter):
                    print(format_packet(p, first_t, opcodes, args.full), flush=True)
            time.sleep(0.5)
    except KeyboardInterrupt:
        print()
    return 0


# ---------------------------------------------------------------- session reports

def parse_report(text):
    """Stages, per-stream event counts and unknown opcodes of one session report."""
    stages, streams, unknown, order = [], defaultdict(Counter), [], []
    stream = None
    for line in text.splitlines():
        match = STAGE_RE.match(line)
        if match:
            stages.append({"t": int(match[1]), "stage": match[2], "outcome": match[3], "category": match[4]})
            continue
        if line.startswith("protocol stream="):
            stream = line.split("=", 1)[1].strip()
            if stream not in order:
                order.append(stream)
            continue
        match = EVENT_RE.match(line)
        if match and stream:
            streams[stream][match[1]] += 1
            if match[1] == "UNKNOWN_OPCODE":
                op = re.search(r"opcode=0x([0-9a-fA-F]+)", match[2])
                if op:
                    unknown.append((stream, int(op[1], 16)))
    return {"stages": stages, "streams": streams, "stream_order": order, "unknown": unknown}


def newest_report():
    reports = sorted(REPORT_DIR.glob("session-*.txt"), key=lambda p: p.stat().st_mtime) if REPORT_DIR.is_dir() else []
    return reports[-1] if reports else None


def print_report(path, parsed, opcodes, indent=""):
    print(f"{indent}{path.name}  ({time.strftime('%Y-%m-%d %H:%M:%S', time.localtime(path.stat().st_mtime))})")
    print(f"{indent}stages:")
    for s in parsed["stages"]:
        extra = f" category={s['category']}" if s["category"] else ""
        flag = "" if s["outcome"] == "ok" else "   <-- not ok"
        print(f"{indent}  +{s['t']:>4}s  {s['stage']:<14} {s['outcome']}{extra}{flag}")
    for name in parsed["stream_order"]:
        counts = parsed["streams"][name]
        print(f"{indent}{name} events: " + (", ".join(f"{k} x{n}" for k, n in sorted(counts.items())) or "none"))
    return parsed


def cmd_report(args):
    path = Path(args.path) if args.path else newest_report()
    if path is None or not path.is_file():
        print("no session report found")
        return 1
    opcodes = Opcodes.load()
    parsed = parse_report(read_text(path))
    print_report(path, parsed, opcodes)
    if parsed["unknown"]:
        print("UNKNOWN_OPCODE (client opcodes the server did not handle):")
        counts = Counter(parsed["unknown"])
        for (stream, op), n in sorted(counts.items()):
            print(f"  {stream:<6} {op:04x} {opcodes.name(stream, 'client', op):<28} x{n}")
    return 0


# ---------------------------------------------------------------- status

PROCESS_QUERY = (
    "[Console]::OutputEncoding=[Text.Encoding]::UTF8;"
    "$r=@(Get-CimInstance Win32_Process | Where-Object { $_.ProcessId -ne $PID -and ("
    "$_.Name -in 'eco.exe','eco_launch.exe','eco-reborn.exe','Eco Reborn.exe' -or "
    "($_.Name -match '^(powershell|pwsh)\\.exe$' -and $_.CommandLine -match 'Start-EcoReborn')) } | "
    "ForEach-Object { [pscustomobject]@{pid=$_.ProcessId;name=$_.Name;start=$_.CreationDate.ToString('s');"
    "ws=[math]::Round($_.WorkingSetSize/1MB,1);cmd=$_.CommandLine} });"
    "if($r.Count){ConvertTo-Json -InputObject $r -Compress}"
)


def run(cmd, timeout=60):
    try:
        done = subprocess.run(cmd, capture_output=True, timeout=timeout)
    except (OSError, subprocess.SubprocessError) as error:
        return 1, f"{type(error).__name__}: {error}"
    return done.returncode, done.stdout.decode("utf-8", errors="replace")


def process_role(cmd):
    match = re.search(r"serve-(map|login|world)", cmd or "")
    if match:
        return match[1]
    return "launcher script" if "Start-EcoReborn" in (cmd or "") else ""


def list_processes():
    code, out = run(["powershell", "-NoProfile", "-Command", PROCESS_QUERY])
    if not out.strip():
        return []
    try:
        rows = json.loads(out)
    except ValueError:
        return []
    return rows if isinstance(rows, list) else [rows]


def parse_netstat(text, ports=PORTS):
    found = {}
    for line in text.splitlines():
        parts = line.split()
        if len(parts) >= 5 and parts[0] == "TCP" and parts[3] == "LISTENING":
            port = parts[1].rsplit(":", 1)[-1]
            if port.isdigit() and int(port) in ports:
                found[int(port)] = int(parts[4])
    return found


def classify_isolation(code, output):
    first = next((ln.strip() for ln in output.splitlines() if ln.strip()), "(no output)")
    low = output.lower()
    rules = len(re.findall(r"^DisplayName\s*:", output, re.M))
    if "partial" in low or "mismatched" in low:
        return "partial", first
    if "disabled" in low or "no managed" in low or "cannot confirm" in low:
        return "missing", first
    if code == 0 and rules:
        return f"ok ({rules} rules)", first
    return "unknown", first


def load_map_names(path=MAPNAME_CSV):
    names = {}
    try:
        with open(path, encoding="cp932", errors="replace", newline="") as handle:
            for row in csv.reader(handle):
                if len(row) > 1 and row[0].isdigit():
                    names.setdefault(int(row[0]), row[1])
    except OSError:
        pass
    return names


def read_characters(db=SAVE_DB):
    con = sqlite3.connect(f"file:{db.as_posix()}?mode=ro", uri=True, timeout=2)
    try:
        return con.execute("select id, name, map_id, x, y, direction from character order by id").fetchall()
    finally:
        con.close()


def cmd_status(args):
    print("== processes ==")
    procs = list_processes()
    if not procs:
        print("  none running")
    names = {}
    for p in procs:
        names[p["pid"]] = p["name"]
        role = process_role(p.get("cmd"))
        print(f"  pid {p['pid']:<7} {p['name']:<16} {role:<16} started {p['start'].replace('T', ' ')}  {p['ws']} MB")
    print("== listening ports ==")
    code, out = run(["netstat", "-ano", "-p", "tcp"])
    listening = parse_netstat(out)
    for port, role in PORTS.items():
        pid = listening.get(port)
        print(f"  {port} ({role}): " + (f"pid {pid} {names.get(pid, '')}".rstrip() if pid else "not listening"))
    print("== firewall isolation ==")
    code, out = run(["powershell", "-NoProfile", "-File", str(ISOLATION_PS1), "Status"])
    verdict, first = classify_isolation(code, out)
    print(f"  {verdict}: {first}")
    print("== save ==")
    try:
        rows = read_characters()
        maps = load_map_names()
        for cid, name, map_id, x, y, direction in rows:
            label = f" {maps[map_id]}" if map_id in maps else ""
            print(f"  #{cid} {name}  map {map_id}{label}  x={x} y={y} dir={direction}")
        if not rows:
            print("  no characters")
    except sqlite3.Error as error:
        print(f"  cannot read {SAVE_DB.name}: {error}")
    print("== latest session report ==")
    path = newest_report()
    if path:
        parsed = parse_report(read_text(path))
        print_report(path, parsed, Opcodes.load(), "  ")
    else:
        print("  none")
    print("== packet trace ==")
    print(f"  enabled: {'yes' if (TRACE_DIR / 'ENABLED').is_file() else 'no'}")
    files = sorted(TRACE_DIR.glob("trace-*.log"), key=lambda p: p.stat().st_mtime, reverse=True) if TRACE_DIR.is_dir() else []
    for f in files[:3]:
        stamp = time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(f.stat().st_mtime))
        print(f"  {f.name}  {f.stat().st_size} bytes  {stamp}")
    if not files:
        print("  no trace files")
    return 0


# ---------------------------------------------------------------- cli

def build_parser():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("status", help="processes, ports, isolation, save, newest report, trace state").set_defaults(func=cmd_status)

    t = sub.add_parser("trace", help="packet timeline and analysis from research/local-trace")
    t.add_argument("--dir", default=str(TRACE_DIR))
    t.add_argument("--files", nargs="+", metavar="FILE")
    t.add_argument("--all", action="store_true", help="every trace file, not just the newest session")
    t.add_argument("--since", type=float, metavar="MINUTES", help="files modified in the last N minutes")
    t.add_argument("--window", type=int, default=SESSION_WINDOW_S, help="seconds between file stamps that still count as one session")
    t.add_argument("--role", choices=ROLES)
    t.add_argument("--opcode", help="comma-separated hex opcodes to show")
    t.add_argument("--hide-keepalive", action=argparse.BooleanOptionalAction, default=True)
    t.add_argument("--full", action="store_true", help="print the whole payload")
    t.add_argument("--summary-only", action="store_true")
    t.add_argument("--follow", action="store_true", help="tail the newest files live")
    t.add_argument("--html", metavar="OUT", help="also write a self-contained HTML timeline")
    t.set_defaults(func=cmd_trace)

    o = sub.add_parser("opcodes", help="look up opcode names")
    o.add_argument("hex", nargs="*")
    o.add_argument("--rebuild", action="store_true", help="regenerate opcodes.json from research/")
    o.add_argument("--research", default=str(WORKSPACE / "research"))
    o.set_defaults(func=cmd_opcodes)

    r = sub.add_parser("report", help="pretty-print a session report (default newest)")
    r.add_argument("path", nargs="?")
    r.set_defaults(func=cmd_report)
    return parser


def main(argv=None):
    if not sys.stdout.isatty():
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    args = build_parser().parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
