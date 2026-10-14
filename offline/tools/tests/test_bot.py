import unittest
from unittest.mock import patch
from pathlib import Path
import shutil
import uuid
import os
import subprocess
import sys
import time

import bot


class AdaptationTests(unittest.TestCase):
    def setUp(self):
        self.source = (bot.ROOT / bot.TEST_SOURCE).read_text(encoding="utf-8")

    def test_adaptation_uses_ephemeral_addresses_and_keeps_advertised_ports(self):
        adapted = bot.adapt_test_source(self.source)
        self.assertIn("bind_loopback(0)", adapted)
        self.assertIn("Client::connect(map_addr)", adapted)
        self.assertIn('"127.0.0.1:17832"', adapted)
        self.assertIn("&17833u32.to_be_bytes()", adapted)
        self.assertNotIn('Client::connect("127.0.0.1:17831"', adapted)

    def test_adaptation_refuses_unrecognized_source_shape(self):
        with self.assertRaises(ValueError):
            bot.adapt_test_source(self.source.replace("bind_loopback(17831)", "bind_loopback(1234)"))

    def test_exit_zero_without_one_real_test_is_failure(self):
        self.assertEqual(bot.classify_run(0, []), "failed")
        self.assertEqual(bot.classify_run(0, ["test result: ok. 0 passed; 0 failed; 0 ignored"]), "failed")
        self.assertEqual(bot.classify_run(1, ["test result: FAILED. 0 passed; 1 failed"]), "failed")
        valid = ["test integration_tests::headless_three_service_flow ... ok", "test result: ok. 1 passed; 0 failed; 0 ignored"]
        self.assertEqual(bot.classify_run(0, valid), "passed")
        self.assertEqual(bot.classify_run(0, [valid[1]]), "failed")
        self.assertEqual(bot.classify_run(101, []), "gate_or_build_failure")

    def test_timeout_cleanup_targets_only_owned_pid_tree(self):
        class Owned:
            pid = 24680
            def poll(self):
                return None
            def communicate(self, timeout=None):
                return "", ""
        with patch.object(bot.os, "name", "nt"), patch.object(bot.subprocess, "run", return_value=type("Result", (), {"returncode": 0})()) as run:
            bot.kill_owned_tree(Owned())
        self.assertEqual(run.call_args.args[0], ["taskkill", "/PID", "24680", "/T", "/F"])

    def test_timeout_tree_kill_failure_is_reported(self):
        class Owned:
            pid = 24681
            def poll(self):
                return None
        failure = type("Result", (), {"returncode": 1})()
        with patch.object(bot.os, "name", "nt"), patch.object(bot.subprocess, "run", return_value=failure) as run:
            with self.assertRaises(RuntimeError):
                bot.kill_owned_tree(Owned())
        self.assertEqual(run.call_args.args[0], ["taskkill", "/PID", "24681", "/T", "/F"])

    @unittest.skipUnless(os.name == "nt", "Windows owned-tree behavior")
    def test_timeout_kills_owned_parent_and_child(self):
        work = bot.ROOT / "tools/tests/.work" / f"pid-{uuid.uuid4().hex}.txt"
        child_script = "import time; time.sleep(30)"
        parent_script = "import pathlib,subprocess,sys,time; p=subprocess.Popen([sys.executable,'-c',sys.argv[1]]); pathlib.Path(sys.argv[2]).write_text(str(p.pid)); time.sleep(30)"
        process = subprocess.Popen([sys.executable, "-c", parent_script, child_script, str(work)])
        try:
            deadline = time.monotonic() + 5
            while not work.exists() and time.monotonic() < deadline:
                time.sleep(0.05)
            self.assertTrue(work.exists())
            child_pid = int(work.read_text())
            bot.kill_owned_tree(process)
            self.assertIsNotNone(process.poll())
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                listed = subprocess.run(["tasklist", "/FI", f"PID eq {child_pid}", "/NH"], capture_output=True, text=True)
                if str(child_pid) not in listed.stdout:
                    break
                time.sleep(0.1)
            self.assertNotIn(str(child_pid), listed.stdout)
        finally:
            if process.poll() is None:
                bot.kill_owned_tree(process)
            if work.exists():
                child_pid = work.read_text().strip()
                subprocess.run(["taskkill", "/PID", child_pid, "/T", "/F"], capture_output=True)
                work.unlink(missing_ok=True)

    def test_snapshot_is_source_hashed_and_contains_only_declared_inputs(self):
        work = bot.ROOT / "tools/tests/.work" / f"unit-{uuid.uuid4().hex}"
        work.mkdir(parents=True)
        try:
            copied = work / "offline"
            manifest = bot.copy_project(copied)
            self.assertIn("offline/src/integration_tests.rs", manifest["source_sha256"])
            self.assertIn("offline/Cargo.lock", manifest["source_sha256"])
            self.assertIn("offline/data/20013003.map", manifest["source_sha256"])
            self.assertFalse((copied / "reports").exists())
            self.assertFalse((copied / "saves").exists())
        finally:
            shutil.rmtree(work)


if __name__ == "__main__":
    unittest.main()
