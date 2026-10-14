import importlib.util
import json
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location("eco_run", Path(__file__).with_name("run.py"))
eco_run = importlib.util.module_from_spec(spec)
spec.loader.exec_module(eco_run)


class RunTests(unittest.TestCase):
    def test_orchestration_order_and_bot_nonzero(self):
        order = []
        modules = {
            "triage": SimpleNamespace(triage=lambda paths: (order.append("triage") or {"reports": [], "errors": []})),
            "audit": SimpleNamespace(run=lambda root, out: (order.append("audit") or {})),
            "checker": SimpleNamespace(run=lambda root, out: (order.append("checker") or {})),
            "bot": SimpleNamespace(run=lambda args: (order.append("bot") or 23)),
            "scoresheet": SimpleNamespace(run=lambda root, out, bot_path=None: (order.append("scoresheet") or {})),
        }
        result = eco_run.run_steps("root", "stage", modules, bot_enabled=True, runs=3)
        self.assertEqual(order, ["triage", "audit", "checker", "bot", "scoresheet"])
        self.assertEqual(result[-1], 23)
        self.assertEqual(eco_run.exit_code([], True, result[-1]), 23)
        self.assertEqual(eco_run.exit_code([{"error": "bad report"}], True, result[-1]), 23)
        self.assertEqual(eco_run.exit_code([{"error": "bad report"}], False, 0), 1)

    def test_comparison_and_serialized_summary_stay_stable(self):
        previous = {"source_snapshot_sha256": "same", "comparison": {"changed": True, "from": "old", "to": "same"}}
        comparison = eco_run.next_comparison(previous, "same")
        self.assertEqual(comparison, previous["comparison"])
        changed = eco_run.next_comparison(previous, "new", {"checker.findings": 3})
        self.assertEqual(changed, {"changed": True, "from": "same", "to": "new", "deltas": {}})
        previous["metrics"] = {"checker.findings": 2}
        self.assertEqual(eco_run.next_comparison(previous, "new", {"checker.findings": 3})["deltas"], {"checker.findings": 1})
        args = ("same", comparison, {}, {"source_data_mismatch": 2}, {}, {"signature_counts": {"unknown": 1}, "errors": []}, [], [], "not_run")
        first = json.dumps(eco_run.build_summary(*args), sort_keys=True)
        second = json.dumps(eco_run.build_summary(*args), sort_keys=True)
        self.assertEqual(first, second)

    def test_only_proven_rows_are_separated_from_review_leads(self):
        proven, leads = eco_run.finding_lists([
            {"level": "error", "classification": "proven_fault", "source": "a.xml", "message": "bad reference"},
            {"level": "review", "classification": "unresolved_reference", "source": "x.cs", "message": "partial inventory"},
        ])
        self.assertEqual([row["source"] for row in proven], ["a.xml"])
        self.assertEqual([row["source"] for row in leads], ["x.cs"])

    def test_audit_section_replaces_in_place_deterministically(self):
        block = eco_run.audit_summary_block({"source_data_mismatch": 1, "unresolved_reference": 1}, [], [],
                                            {"changed": True, "deltas": {"checker.findings": -1}})
        base = "# Audit\n\nmanual text\n"
        first = eco_run.replace_audit_block(base, block)
        second = eco_run.replace_audit_block(first, block)
        self.assertEqual(first, second)
        self.assertIn("[SCORE](SCORE.md)", first)
        self.assertIn("[TRIAGE](triage.json)", first)
        self.assertIn("checker.findings -1", first)
        self.assertEqual(first.count(eco_run.BEGIN), 1)

    def test_snapshot_scope_skips_build_outputs(self):
        files = eco_run.source_files(eco_run.ROOT)
        names = {path.relative_to(eco_run.ROOT).as_posix() for path in files}
        self.assertIn("offline/tools/tests/run.py", names)
        self.assertIn("offline/tools/tests/signatures.json", names)
        self.assertIn("research/SagaECO/Configuration/DB/SkillList.xml", names)
        self.assertIn("research/SagaECO/Configuration/DB/Dungeon/Dungeons.xml", names)
        self.assertFalse(any("/.work/" in name or "bot-result.json" in name for name in names))

    def test_existing_manual_scorecard_is_copied_before_scoring(self):
        order = []
        modules = {
            "triage": SimpleNamespace(triage=lambda paths: (order.append("triage") or {"reports": [], "errors": []})),
            "audit": SimpleNamespace(run=lambda root, out: (order.append("audit") or {})),
            "checker": SimpleNamespace(run=lambda root, out: (order.append("checker") or {})),
            "bot": SimpleNamespace(run=Mock()),
            "scoresheet": SimpleNamespace(run=lambda root, out, bot_path=None: (order.append("scoresheet") or {})),
        }
        original_is_file = Path.is_file
        with patch.object(Path, "is_file", autospec=True,
                          side_effect=lambda self: str(self) == "manual.csv" or original_is_file(self)), \
                patch.object(eco_run.shutil, "copyfile", side_effect=lambda *args: order.append("copy")) as copy:
            eco_run.run_steps("root", "stage", modules, prior_scorecard="manual.csv")
        copy.assert_called_once_with("manual.csv", Path("stage") / "scoresheet.csv")
        self.assertEqual(order, ["triage", "audit", "checker", "copy", "scoresheet"])


if __name__ == "__main__":
    unittest.main()
