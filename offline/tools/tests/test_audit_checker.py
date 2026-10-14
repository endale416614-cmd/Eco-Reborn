import sys
import unittest
import shutil
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import audit
import checker


class AuditCheckerTests(unittest.TestCase):
    def setUp(self):
        self.root = HERE / "_audit_checker_fixture"
        shutil.rmtree(self.root, ignore_errors=True)
        self.root.mkdir()
        self.db = self.root / "research/SagaECO/Configuration/DB"
        (self.db / "Spawns").mkdir(parents=True)
        (self.db / "Quests").mkdir()
        (self.root / "research/SagaECO/SagaScript/Test").mkdir(parents=True)
        (self.db / "monster.csv").write_text("#ID,name\n100,Known\n", encoding="utf-8")
        (self.db / "warp.csv").write_text("id,map\n0,10001000\n", encoding="utf-8")

    def tearDown(self):
        shutil.rmtree(self.root, ignore_errors=True)

    def fixture(self, broken=True):
        mob_id = "999" if broken else "100"
        quest_target = "998" if broken else "100"
        (self.db / "Spawns/10001000.xml").write_text(
            f"<Spawns><spawn><id>{mob_id}</id></spawn></Spawns>", encoding="utf-8")
        (self.db / "Quests/q.xml").write_text(
            f"<QuestDB><Quest><ID>7</ID><Type>HUNT</Type><ObjectID1>{quest_target}</ObjectID1></Quest></QuestDB>", encoding="utf-8")
        (self.root / "research/SagaECO/SagaScript/Test/portal.cs").write_text(
            "// Warp(pc, 55555555, 1, 2);\nstring s = \"Warp(pc, 44444444, 1, 2);\";\n"
            "string multi = @\"text\nWarp(pc, 33333333, 1, 2);\nend\";\n"
            "/* start\nWarp(pc, 22222222, 1, 2);\nend */\n"
            "Warp(pc, 10001000, 1, 2);\nWarp(pc, (uint)dynamicMap, 1, 2);\n", encoding="utf-8")

    def test_reports_direct_reference_faults_and_dynamic_limit(self):
        self.fixture()
        output = self.root / "out"
        audit_summary = audit.run(self.root, output)
        checker_summary = checker.run(self.root, output)
        self.assertEqual(audit_summary["findings"], 2)
        self.assertEqual(checker_summary["source_data_mismatch"], 2)
        data = (output / "checker.csv").read_text(encoding="utf-8")
        self.assertIn("999", data)
        self.assertIn("998", data)
        self.assertNotIn("dynamicMap", data)
        self.assertNotIn("55555555", data)
        self.assertNotIn("44444444", data)
        self.assertNotIn("33333333", data)
        self.assertNotIn("22222222", data)
        self.assertIn("absent from the supplied monster.csv snapshot", data)
        self.assertEqual(checker_summary["literal_warps"], 1)
        self.assertIn("not proof of runtime failure", (output / "CHECKER.md").read_text(encoding="utf-8"))

    def test_fixed_fixture_has_no_faults_and_reports_are_deterministic(self):
        self.fixture(broken=False)
        output = self.root / "out"
        audit.run(self.root, output)
        checker_summary = checker.run(self.root, output)
        first = {name: (output / name).read_bytes() for name in ("audit.csv", "AUDIT.md", "checker.csv", "CHECKER.md")}
        audit.run(self.root, output)
        checker.run(self.root, output)
        second = {name: (output / name).read_bytes() for name in first}
        self.assertEqual(first, second)
        self.assertEqual(checker_summary["source_data_mismatch"], 0)
        self.assertEqual(checker_summary["unresolved_reference"], 0)
        self.assertIn("No live save or server data is loaded", first["AUDIT.md"].decode())

    def test_missing_bad_ids_and_wrong_xml_roots_are_reported(self):
        (self.db / "monster.csv").write_text("-1,Bad\n4294967296,TooLarge\n", encoding="utf-8")
        (self.db / "Spawns/map.xml").write_text("<WrongRoot><spawn><id>1</id></spawn></WrongRoot>", encoding="utf-8")
        (self.db / "Quests/q.xml").write_text("<QuestDB><Quest><Type>HUNT</Type></Quest></QuestDB>", encoding="utf-8")
        summary = audit.run(self.root, self.root / "out")
        report = (self.root / "out/audit.csv").read_text(encoding="utf-8")
        self.assertEqual(summary["findings"], 4)
        self.assertIn("invalid_monster_id", report)
        self.assertIn("unexpected_spawn_root", report)
        self.assertIn("invalid_quest_id", report)
        self.assertGreater(checker.run(self.root, self.root / "out")["source_data_mismatch"], 0)

    def test_checker_refuses_missing_required_source(self):
        with self.assertRaises(FileNotFoundError):
            checker.run(self.root / "empty", self.root / "out")


if __name__ == "__main__":
    unittest.main()
