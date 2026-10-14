import contextlib
import importlib.util
import io
import json
import os
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location("eco_debug", Path(__file__).with_name("eco_debug.py"))
eco_debug = importlib.util.module_from_spec(spec)
spec.loader.exec_module(eco_debug)

PING = "0006000a00000000"
PONG = "0006000b00000000"

OPCODES = eco_debug.Opcodes({
    "client": {"05e6": {"name": "CSMG_NPC_EVENT_START", "aliases": [], "source": "x"},
               "001f": {"name": "CSMG_LOGOUT", "aliases": [], "source": "x"},
               "11f8": {"name": "CSMG_PLAYER_MOVE", "aliases": [], "source": "x"}},
    "server": {"0020": {"name": "SSMG_LOGOUT", "aliases": [], "source": "x"}},
    "role_overrides": {"login": {"client": {"001f": {"name": "UserLogin", "aliases": ["CSMG_LOGIN"], "source": "y"}},
                                 "server": {"0020": {"name": "SSMG_LOGIN_ACK", "aliases": [], "source": "y"}}}},
})


def line(ms, direction, opcode, hex_bytes):
    return f"{ms} {direction} {opcode} len={len(hex_bytes) // 2} {hex_bytes}\n"


def write(folder, name, lines, mtime=None):
    path = Path(folder) / name
    path.write_text("".join(lines), encoding="ascii")
    if mtime is not None:
        os.utime(path, (mtime, mtime))
    return path


class TraceTests(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self._tmp.name)
        self.addCleanup(self._tmp.cleanup)

    def parse(self, text, name):
        return eco_debug.parse_text(text, eco_debug.TraceFile.from_path(self.dir / name))[0]

    def test_filename_with_and_without_role(self):
        new = eco_debug.TraceFile.from_path("trace-1791927565-13676-map.log")
        self.assertEqual((new.stamp, new.pid, new.role), (1791927565, 13676, "map"))
        old = eco_debug.TraceFile.from_path("trace-1791926627-2792.log")
        self.assertEqual((old.stamp, old.pid, old.role), (1791926627, 2792, "?"))

    def test_old_style_ms_since_process_start(self):
        packets = self.parse(line(18593, "C>S", "000a", PING), "trace-1791926627-2792.log")
        self.assertEqual(packets[0].t, 1791926627 * 1000 + 18593)
        self.assertEqual((packets[0].dir, packets[0].opcode, packets[0].length, packets[0].role), ("C>S", 0x000A, 8, "?"))

    def test_new_style_wall_clock_ms(self):
        ms = 1791927565768
        packets = self.parse(line(ms, "S>C", "ffff", "0016ffff" + "aa" * 20), "trace-1791927565-1-map.log")
        self.assertEqual(packets[0].t, ms)
        self.assertEqual(packets[0].role, "map")
        self.assertEqual(packets[0].payload_hex, "aa" * 20)

    def test_truncated_and_bad_lines(self):
        text = "5 S>C 0001 len=600 0003000155...\nnot a packet\n"
        packets, bad = eco_debug.parse_text(text, eco_debug.TraceFile.from_path(self.dir / "trace-100-1-map.log"))
        self.assertEqual((len(packets), bad), (1, 1))
        self.assertTrue(packets[0].truncated)

    def test_session_grouping_uses_newest_stamp_window(self):
        for name in ("trace-1000-1-map.log", "trace-5000-2-login.log", "trace-5100-3-world.log", "trace-5130-4-map.log"):
            write(self.dir, name, [line(5000, "C>S", "0001", "00020001")])
        files = eco_debug.list_trace_files(self.dir)
        names = [f.path.name for f in eco_debug.select_files(files)]
        self.assertEqual(names, ["trace-5100-3-world.log", "trace-5130-4-map.log"])
        self.assertEqual(len(eco_debug.select_files(files, window=200)), 3)
        self.assertEqual(len(eco_debug.select_files(files, everything=True)), 4)

    def test_since_minutes_uses_mtime(self):
        write(self.dir, "trace-1000-1-map.log", [line(1, "C>S", "0001", "00020001")], mtime=1_000_000)
        write(self.dir, "trace-2000-2-map.log", [line(1, "C>S", "0001", "00020001")], mtime=1_000_900)
        files = eco_debug.list_trace_files(self.dir)
        picked = eco_debug.select_files(files, since_minutes=5, now=1_001_000)
        self.assertEqual([f.path.name for f in picked], ["trace-2000-2-map.log"])

    def test_merge_orders_by_time_across_files(self):
        base = 1791927565000
        write(self.dir, "trace-1791927565-1-login.log", [line(base + 30, "C>S", "001f", "00020" + "01f"), line(base + 50, "S>C", "0020", "0002" + "0020")])
        write(self.dir, "trace-1791927565-2-map.log", [line(base + 10, "C>S", "0001", "00020001"), line(base + 40, "S>C", "0002", "00020002")])
        packets, bad = eco_debug.load_packets(eco_debug.list_trace_files(self.dir))
        self.assertEqual(bad, 0)
        self.assertEqual([(p.role, p.t - base) for p in packets], [("map", 10), ("login", 30), ("map", 40), ("login", 50)])

    def test_keepalive_detection(self):
        tf = eco_debug.TraceFile.from_path(self.dir / "trace-100-1-map.log")
        ping, pong, other, real_000a = eco_debug.parse_text(
            line(1, "C>S", "000a", PING) + line(2, "S>C", "000b", PONG) + line(3, "C>S", "11f8", "000611f800000000")
            + line(4, "C>S", "000a", "000a000a" + "01" * 6), tf)[0]
        self.assertTrue(eco_debug.is_keepalive(ping))
        self.assertTrue(eco_debug.is_keepalive(pong))
        self.assertFalse(eco_debug.is_keepalive(other))
        self.assertFalse(eco_debug.is_keepalive(real_000a))

    def test_unanswered_and_latency(self):
        base = 1791927565000
        text = (
            line(base, "C>S", "05e6", "000605e600000001")           # answered after 40 ms
            + line(base + 40, "S>C", "05dc", "000405dc")
            + line(base + 100, "C>S", "11f8", "000611f800000002")    # only a pong follows: unanswered
            + line(base + 120, "S>C", "000b", PONG)
            + line(base + 3000, "C>S", "05e6", "000605e600000003")  # unanswered, trace continues past the window
            + line(base + 5000, "S>C", "0fa6", "000a0fa600000000")
        )
        packets = self.parse(text, "trace-1791927565-1-map.log")
        result = eco_debug.analyze(packets, OPCODES)
        self.assertEqual(dict(result["unanswered"]), {("map", 0x11F8): 1, ("map", 0x05E6): 1})
        self.assertEqual(result["latency"][("map", 0x05E6)][0], 40)
        self.assertEqual(result["roles"]["map"]["keepalive"], 1)
        self.assertEqual(len(result["unanswered_packets"]), 2)

    def test_reply_in_same_millisecond_as_next_request_is_not_its_answer(self):
        base = 1791927740799
        text = (
            line(base, "C>S", "11f8", "000a11f8015efb82005a0007")
            + line(base + 12, "S>C", "11f9", "000e11f900000010015efb82")
            + line(base + 12, "C>S", "11f8", "000a11f800fafb82010e0007")  # same ms as the reply above
            + line(base + 24, "S>C", "11f9", "000e11f90000001000fafb82")
        )
        packets = self.parse(text, "trace-1791927740-1-map.log")
        result = eco_debug.analyze(packets, OPCODES)
        self.assertEqual(result["latency"][("map", 0x11F8)], [12, 12])

    def test_packet_at_end_of_trace_is_not_judged(self):
        base = 1791927565000
        packets = self.parse(line(base, "S>C", "0001", "00020001") + line(base + 200, "C>S", "05e6", "000605e600000001"),
                             "trace-1791927565-1-map.log")
        self.assertEqual(eco_debug.analyze(packets, OPCODES)["unanswered"], {})

    def test_unknown_opcodes_counted(self):
        packets = self.parse(line(1791927565000, "C>S", "7777", "00067777"), "trace-1791927565-1-map.log")
        self.assertEqual(dict(eco_debug.analyze(packets, OPCODES)["unknown"]), {("map", "C>S", 0x7777): 1})

    def test_trace_command_end_to_end(self):
        base = 1791927565000
        write(self.dir, "trace-1791927565-1-map.log", [
            line(base, "C>S", "05e6", "000605e600000001"), line(base + 5, "S>C", "05dc", "000405dc"),
            line(base + 10, "C>S", "000a", PING), line(base + 11, "S>C", "000b", PONG)])
        out = self.dir / "out.html"
        args = eco_debug.build_parser().parse_args(["trace", "--dir", str(self.dir), "--html", str(out)])
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(eco_debug.cmd_trace(args), 0)
        page = out.read_text(encoding="utf-8")
        self.assertIn("NpcEventStart", page)
        self.assertIn("EventStart", page)
        self.assertNotIn("ConnectionCheck", page)
        self.assertNotIn("<script", page)

    def test_follower_reads_only_new_complete_lines(self):
        path = self.dir / "trace-1791927565-1-map.log"
        path.write_text(line(1791927565000, "C>S", "0001", "00020001"), encoding="ascii")
        follower = eco_debug.Follower(self.dir)
        self.assertEqual(len(follower.poll()), 1)
        self.assertEqual(follower.poll(), [])
        with open(path, "a", encoding="ascii") as handle:
            handle.write("1791927565010 S>C 0002 len=4 0002")  # half-written line
        self.assertEqual(follower.poll(), [])
        with open(path, "a", encoding="ascii") as handle:
            handle.write("0002\n")
        self.assertEqual([p.opcode for p in follower.poll()], [2])


class OpcodeTests(unittest.TestCase):
    def test_role_overrides_and_hand_table(self):
        self.assertEqual(OPCODES.name("map", "client", 0x001F), "Logout")
        self.assertEqual(OPCODES.name("login", "client", 0x001F), "UserLogin")
        self.assertEqual(OPCODES.name("login", "server", 0x0020), "SSMG_LOGIN_ACK")
        self.assertEqual(OPCODES.name("map", "client", 0x05E6), "NpcEventStart")
        self.assertEqual(OPCODES.name("map", "server", 0x0033), "ConnectionCheck2Ack")

    def test_fallback_to_map_table_is_marked(self):
        self.assertEqual(OPCODES.name("login", "client", 0x11F8), "CharaMove")  # hand names apply to every role
        self.assertEqual(OPCODES.name("world", "server", 0x0020), "SSMG_LOGOUT?")
        self.assertEqual(OPCODES.name("map", "client", 0x7777), "?")

    def test_unknown_role_tries_every_role(self):
        self.assertEqual(OPCODES.name("?", "client", 0x001F), "Logout|UserLogin")

    def test_alias_keeps_replaced_name(self):
        info, _ = OPCODES.lookup("map", "client", 0x11F8)
        self.assertEqual(info["name"], "CharaMove")
        self.assertIn("CSMG_PLAYER_MOVE", info["aliases"])

    def test_collapse_keeps_first_name_and_aliases(self):
        table = {}
        eco_debug.add_candidate(table, 0x1B, "Emu", "a.json")
        eco_debug.add_candidate(table, 0x1B, "CSMG_X", "b.cs")
        eco_debug.add_candidate(table, 0x1B, "CSMG_X", "c.cs")
        self.assertEqual(eco_debug.collapse(table)["001b"], {"name": "Emu", "aliases": ["CSMG_X"], "source": "a.json"})

    def test_rebuild_from_synthetic_tree(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            manager = root / "SagaECO" / "SagaMap" / "Manager"
            manager.mkdir(parents=True)
            (manager / "MapClientManager.cs").write_text(
                "this.commandTable.Add((ushort)1510, (Packet)new CSMG_NPC_EVENT_START());\n"
                "this.commandTable.Add((ushort)30, new Packet());\n", encoding="utf-8")
            server = root / "SagaECO" / "SagaMap" / "Packets" / "Server"
            server.mkdir(parents=True)
            (server / "SSMG_A.cs").write_text("public class SSMG_A : Packet { A() { this.ID = (ushort)1500; } }", encoding="utf-8")
            (server / "SSMG_B.cs").write_text("public class SSMG_B : Packet { B() { this.ID = (ushort)1500; } }", encoding="utf-8")
            emu = root / "EcoServerEmulator" / "MapServer"
            emu.mkdir(parents=True)
            (emu / "MapServerInterface.json").write_text(json.dumps([{"ID": "05E6", "InterfaceName": "NpcStart"}]), encoding="utf-8-sig")
            data = eco_debug.rebuild(root)
        self.assertEqual(data["client"]["05e6"]["name"], "NpcStart")
        self.assertEqual(data["client"]["05e6"]["aliases"], ["CSMG_NPC_EVENT_START"])
        self.assertNotIn("001e", data["client"])
        self.assertEqual(data["server"]["05dc"]["name"], "SSMG_A")
        self.assertEqual(data["server"]["05dc"]["aliases"], ["SSMG_B"])

    def test_shipped_json_has_core_names(self):
        shipped = eco_debug.Opcodes.load()
        self.assertEqual(shipped.name("map", "client", 0x05E6), "NpcEventStart")
        info, _ = shipped.lookup("map", "client", 0x05E6)
        self.assertIn("CSMG_NPC_EVENT_START", info["aliases"])
        self.assertEqual(shipped.name("login", "client", 0x001F), "UserLogin")


REPORT = """ECO report
+000000s stage=wrapper_start outcome=ok
+000003s stage=map_start outcome=failed category=server_start_failed
protocol stream=map
protocol event=LISTENING
protocol event=UNKNOWN_OPCODE opcode=0x0fa5
protocol event=UNKNOWN_OPCODE opcode=0x0fa5
protocol stream=login
protocol event=LISTENING
protocol event=FRIEND_INIT_ID_SHAPE zero=true
"""


class ReportTests(unittest.TestCase):
    def test_parse_report(self):
        parsed = eco_debug.parse_report(REPORT)
        self.assertEqual([(s["stage"], s["outcome"], s["category"]) for s in parsed["stages"]],
                         [("wrapper_start", "ok", None), ("map_start", "failed", "server_start_failed")])
        self.assertEqual(parsed["stream_order"], ["map", "login"])
        self.assertEqual(parsed["streams"]["map"]["UNKNOWN_OPCODE"], 2)
        self.assertEqual(parsed["streams"]["login"]["FRIEND_INIT_ID_SHAPE"], 1)
        self.assertEqual(parsed["unknown"], [("map", 0x0FA5), ("map", 0x0FA5)])


class StatusHelperTests(unittest.TestCase):
    def test_netstat_listening_ports(self):
        text = ("  TCP    0.0.0.0:17831   0.0.0.0:0   LISTENING   4242\n"
                "  TCP    127.0.0.1:17832   0.0.0.0:0   LISTENING   4243\n"
                "  TCP    127.0.0.1:50000   127.0.0.1:17833   ESTABLISHED   4244\n"
                "  TCP    0.0.0.0:9999   0.0.0.0:0   LISTENING   1\n")
        self.assertEqual(eco_debug.parse_netstat(text), {17831: 4242, 17832: 4243})

    def test_process_role(self):
        self.assertEqual(eco_debug.process_role("eco-reborn.exe serve-world --x"), "world")
        self.assertEqual(eco_debug.process_role("powershell -File Start-EcoReborn.ps1"), "launcher script")
        self.assertEqual(eco_debug.process_role(None), "")

    def test_isolation_classification(self):
        rules = "DisplayName : a\nEnabled : True\n\nDisplayName : b\n"
        self.assertEqual(eco_debug.classify_isolation(0, rules)[0], "ok (2 rules)")
        self.assertEqual(eco_debug.classify_isolation(0, "Offline isolation is disabled (no managed firewall rules found).")[0], "missing")
        self.assertEqual(eco_debug.classify_isolation(1, "Offline isolation is partial or mismatched in ActiveStore.")[0], "partial")


if __name__ == "__main__":
    unittest.main()
