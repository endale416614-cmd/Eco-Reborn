import importlib.util
import io
import json
import unittest
import zipfile
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("triage", Path(__file__).with_name("triage.py"))
triage = importlib.util.module_from_spec(spec)
spec.loader.exec_module(triage)


def make_zip(files):
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for name, data in files.items():
            archive.writestr(name, data)
    buffer.seek(0)
    return buffer


class TriageTests(unittest.TestCase):
    def test_real_zip_triage_sorts_unknown_first_and_summarizes_privately(self):
        secret = "Alice password=SECRET TOKEN C:\\Users\\Alice"
        archive = make_zip({
            "z-unknown.txt": f"unrecognized {secret}\n".encode(),
            "b-opcode.txt": b"protocol event=UNKNOWN_OPCODE opcode=0x002a\n",
            "a-login.txt": b"protocol event=LOGIN_REJECTED\n",
        })
        with patch.object(triage, "inputs", return_value=[Path("private-name.zip")]), \
                patch.object(triage, "read_source", side_effect=lambda _: triage.read_zip(archive)):
            result = triage.triage(["ignored"])
        self.assertEqual([r["matches"][0]["id"] for r in result["reports"]], ["unknown", "login_rejected", "unknown_opcode"])
        self.assertEqual(result["signature_counts"], {"login_rejected": 1, "unknown": 1, "unknown_opcode": 1})
        opcode = result["reports"][-1]
        self.assertEqual(opcode["matches"][0]["evidence"], [{"line": 1, "field": "protocol event=UNKNOWN_OPCODE opcode=0x002a"}])
        rendered = json.dumps(result)
        for marker in ("Alice", "SECRET", "TOKEN", "private-name.zip"):
            self.assertNotIn(marker, rendered)
        self.assertRegex(opcode["source_id"], r"^[0-9a-f]{64}$")

    def test_report_and_archive_bounds(self):
        with self.assertRaises(zipfile.BadZipFile):
            triage.read_zip(io.BytesIO(b"not a zip"))
        with self.assertRaises(ValueError):
            triage.read_zip(make_zip({"large.txt": b"x" * (triage.MAX_REPORT + 1)}))
        with self.assertRaises(ValueError):
            triage.read_zip(make_zip({f"{n}.txt": b"x" * triage.MAX_REPORT for n in range(9)}))

    def test_parser_ignores_malformed_fields_and_keeps_panic_error_known(self):
        parsed = triage.parse([
            "protocol event=PANIC", "protocol event=STARTUP_FAILED kind=NOT_FOUND",
            "protocol event=LOGIN_ACCEPTED username=Alice password=SECRET", "stage=unknown",
        ], "report-001")
        self.assertEqual([e["event"] for e in parsed["events"]], ["PANIC", "STARTUP_FAILED"])
        self.assertNotIn("Alice", json.dumps(parsed))
        self.assertNotIn("SECRET", json.dumps(parsed))


if __name__ == "__main__":
    unittest.main()
