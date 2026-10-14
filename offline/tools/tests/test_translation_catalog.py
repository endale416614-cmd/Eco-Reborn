import csv
import importlib.util
import json
import shutil
import unittest
from pathlib import Path

TOOL = Path(__file__).resolve().parents[1] / "Translation-Catalog.py"
spec = importlib.util.spec_from_file_location("translation_catalog", TOOL)
tool = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tool)


class TranslationCatalogTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(__file__).resolve().parent / "_translation_catalog_fixture"
        shutil.rmtree(self.root, ignore_errors=True)
        self.root.mkdir()
        self.source = self.root / "source-ja"
        (self.source / "table").mkdir(parents=True)
        (self.source / "text").mkdir()
        self.catalog = self.root / "catalog.jsonl"
        self.inventory = self.root / "inventory.json"
        self.translations = self.root / "translations.jsonl"

    def tearDown(self):
        shutil.rmtree(self.root, ignore_errors=True)

    def build(self):
        return tool.build(self.source, self.catalog, self.inventory)

    def write_translations(self, rows):
        self.translations.write_text("".join(json.dumps(x, ensure_ascii=False) + "\n" for x in rows), encoding="utf-8")

    def test_candidates_logical_records_comments_hashes_and_text_inventory(self):
        table = self.source / "table/a.csv"
        table.write_bytes('#説明,除外\r\n1,日本語\r\n2,"行\n継続"\r\n3,文\r\n//comment,除外\r\n'.encode("cp932"))
        (self.source / "text/dialog.txt").write_bytes("会話".encode("cp932"))
        self.assertEqual(self.build(), 3)
        rows = [json.loads(x) for x in self.catalog.read_text(encoding="utf-8").splitlines()]
        self.assertEqual([r["id"] for r in rows], ["table/a.csv:2:2", "table/a.csv:3:2", "table/a.csv:4:2"])
        self.assertEqual([r["source"] for r in rows], ["日本語", "行\n継続", "文"])
        inv = json.loads(self.inventory.read_text(encoding="utf-8"))
        self.assertEqual(len(inv["files"]), 2)
        self.assertEqual(inv["candidate_count"], 3)
        with self.assertRaises(FileExistsError):
            self.build()

    def test_bad_encoding_and_malformed_csv_are_inventory_errors(self):
        (self.source / "table/bad.csv").write_bytes(b"\x81\x30")
        (self.source / "table/quote.csv").write_bytes(b'1,"\x82\xa0\n')
        self.assertEqual(self.build(), 0)
        inv = json.loads(self.inventory.read_text(encoding="utf-8"))
        self.assertEqual([f["errors"] for f in inv["files"]], [["UnicodeDecodeError"], ["Error"]])

    def test_bom_encodings_and_xml_csv_are_inventoried_without_xml_candidates(self):
        (self.source / "table/window_param.csv").write_bytes('<?xml version="1.0"?><root>日本語</root>'.encode("utf-8-sig"))
        (self.source / "text/dialog.txt").write_bytes("日本語".encode("utf-16"))
        self.assertEqual(self.build(), 0)
        files = {f["file"]: f for f in json.loads(self.inventory.read_text(encoding="utf-8"))["files"]}
        self.assertEqual(files["table/window_param.csv"]["encoding"], "utf-8-sig")
        self.assertEqual(files["table/window_param.csv"]["format"], "xml")
        self.assertEqual(files["text/dialog.txt"]["encoding"], "utf-16le")

    def test_missing_or_empty_source_is_rejected(self):
        with self.assertRaisesRegex(FileNotFoundError, "directories"):
            tool.build(self.root / "missing", self.catalog, self.inventory)
        with self.assertRaisesRegex(ValueError, "no files"):
            tool.build(self.source, self.catalog, self.inventory)

    def catalog_one(self, source):
        self.catalog.write_text(json.dumps({"id": "table/a.csv:1:2", "source": source}) + "\n", encoding="utf-8")

    def test_checker_validates_source_duplicates_tokens_and_encoding(self):
        source = "こんにちは $R $r %02d {name} %% %lld"
        self.catalog_one(source)
        base = {"id": "table/a.csv:1:2", "source": source, "english": "Hello $R $r %02d {name} %% %lld", "status": "draft"}
        self.write_translations([base])
        self.assertEqual(tool.check(self.catalog, self.translations)["translated"], 1)
        for row, message in [
            ({**base, "source": "違う"}, "source mismatch"),
            ({**base, "english": "Hello %02d {name} %%"}, "token mismatch"),
            ({**base, "english": "Hello $R $r %02d {name} %% %lld €"}, "CP932-encodable"),
        ]:
            self.write_translations([row])
            with self.assertRaisesRegex(ValueError, message):
                tool.check(self.catalog, self.translations)
        self.write_translations([{**base, "english": "Hello $R $r %lld %02d {name} %%"}])
        with self.assertRaisesRegex(ValueError, "token mismatch"):
            tool.check(self.catalog, self.translations)
        self.write_translations([base, base])
        with self.assertRaisesRegex(ValueError, "duplicate translation"):
            tool.check(self.catalog, self.translations)

    def test_unknown_code_mixed_with_known_code_is_flagged_for_review(self):
        source = "日本語 $R $Q"
        self.catalog_one(source)
        self.write_translations([{"id": "table/a.csv:1:2", "source": source,
                                  "english": "Hello $R $Q", "status": "draft"}])
        self.assertEqual(tool.check(self.catalog, self.translations)["needs_code_review"], [1])


if __name__ == "__main__":
    unittest.main()
