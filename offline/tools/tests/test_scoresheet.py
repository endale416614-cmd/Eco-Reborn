import csv
import hashlib
import json
import shutil
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import scoresheet


class ScoresheetTests(unittest.TestCase):
    def setUp(self):
        self.root = HERE / "_scoresheet_fixture"
        shutil.rmtree(self.root, ignore_errors=True)
        self.root.mkdir()
        db = self.root / "research/SagaECO/Configuration/DB"
        (db / "Dungeon").mkdir(parents=True)
        (db / "Quests").mkdir()
        (db / "SkillList.xml").write_text(
            '<SkillList><Skills Job="NOVICE"/><Skills Job="TEST"><SkillID JobLV="1">22</SkillID></Skills></SkillList>', encoding="utf-8")
        (db / "Dungeon/Dungeons.xml").write_text(
            "<Dungeons><Dungeon><ID>4</ID></Dungeon></Dungeons>", encoding="utf-8")
        (db / "Quests/q.xml").write_text(
            "<QuestDB><Quest><ID>5</ID></Quest></QuestDB>", encoding="utf-8")
        src = self.root / "offline/src"
        src.mkdir(parents=True)
        for filename, marker in [("quest.rs", "pub fn replay("), ("content.rs", "pub fn audit("),
                                 ("store.rs", "pub struct Store"), ("map.rs", "pub fn bootstrap("),
                                 ("progression.rs", "pub fn validate_novice_exp_table("),
                                 ("protocol.rs", "pub const MAX_PACKET_SIZE")]:
            (src / filename).write_text(marker, encoding="utf-8")
        self.output = self.root / "out"
        self.output.mkdir()

    def tearDown(self):
        shutil.rmtree(self.root, ignore_errors=True)

    def test_catalog_and_manual_verified_and_custom_rows_survive_deterministically(self):
        path = self.output / "scoresheet.csv"
        fields = scoresheet.FIELDS
        with path.open("w", encoding="utf-8", newline="") as f:
            writer = csv.DictWriter(f, fieldnames=fields, lineterminator="\n")
            writer.writeheader()
            writer.writerow(dict(zip(fields, ["quest:5", "gameplay", "verified", "", "human evidence", "", "build-1", "reviewer"])))
            writer.writerow(dict(zip(fields, ["custom:feature", "gameplay", "partial", "unknown", "manual", "", "", "reviewer"])))
        first_summary = scoresheet.run(self.root, self.output)
        first = path.read_bytes()
        self.assertEqual(first_summary["gameplay_verified"], 1)
        self.assertEqual(first_summary["gameplay_rows"], 3)
        self.assertEqual(first_summary["protocol_verified"], 0)
        scoresheet.run(self.root, self.output)
        self.assertEqual(first, path.read_bytes())
        with path.open(encoding="utf-8", newline="") as f:
            rows = {row["id"]: row for row in csv.DictReader(f)}
        self.assertEqual(rows["quest:5"]["evidence"], "human evidence")
        self.assertEqual(rows["quest:5"]["cause"], "")
        self.assertIn("custom:feature", rows)
        self.assertNotIn("skill:TEST:22:1", rows)
        self.assertFalse(any(ident.startswith("skill:NOVICE:") for ident in rows))

    def test_missing_catalog_inputs_are_not_reported_as_empty_score(self):
        with self.assertRaises(FileNotFoundError):
            scoresheet.run(self.root / "missing", self.output)

    def test_protocol_bot_requires_three_passes_and_matching_source_hash(self):
        source = self.root / "offline/src/protocol.rs"
        source.parent.mkdir(parents=True, exist_ok=True)
        source.write_text("pub const MAX_PACKET_SIZE", encoding="utf-8")
        (self.root / "offline/src/other.rs").write_text("other", encoding="utf-8")
        (self.root / "offline/data").mkdir(parents=True)
        (self.root / "offline/data/20013003.map").write_text("map", encoding="utf-8")
        (self.root / "offline/Cargo.toml").write_text("manifest", encoding="utf-8")
        (self.root / "offline/Cargo.lock").write_text("lock", encoding="utf-8")
        bot = self.root / "bot.json"
        source_paths = [self.root / "offline/Cargo.toml", self.root / "offline/Cargo.lock",
                        self.root / "offline/data/20013003.map", *sorted((self.root / "offline/src").glob("*.rs"))]
        payload = {"status": "passed", "scenario_id": "scenario:protocol-flow", "runs_requested": 3,
                   "runs": [{"status": "passed", "exit_code": 0} for _ in range(3)],
                   "source_sha256": {p.relative_to(self.root).as_posix(): hashlib.sha256(p.read_bytes()).hexdigest() for p in source_paths}}
        bot.write_text(json.dumps(payload), encoding="utf-8")
        summary = scoresheet.run(self.root, self.output, bot)
        self.assertTrue(summary["bot_verified"])
        self.assertEqual(summary["protocol_verified"], 1)
        source.write_text("changed", encoding="utf-8")
        bot.write_text(json.dumps(payload), encoding="utf-8")
        summary = scoresheet.run(self.root, self.output, bot)
        self.assertFalse(summary["bot_verified"])
        self.assertEqual(summary["protocol_verified"], 0)
        payload["source_sha256"] = ["malformed"]
        bot.write_text(json.dumps(payload), encoding="utf-8")
        summary = scoresheet.run(self.root, self.output, bot)
        self.assertFalse(summary["bot_verified"])
        self.assertEqual(summary["protocol_verified"], 0)


if __name__ == "__main__":
    unittest.main()
