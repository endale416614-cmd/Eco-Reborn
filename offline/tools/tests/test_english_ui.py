import importlib.util
from pathlib import Path
import struct
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("build_english_ui", ROOT / "tools" / "Build-EnglishUi.py")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class EnglishUiBuilderTests(unittest.TestCase):
    def test_current_batches_patch_only_exact_mapped_cells(self):
        if not (ROOT / "translations" / "source-ja" / "table").is_dir():
            self.skipTest("needs translations/source-ja, exported from your own client")
        rows = MODULE.load_translations()
        self.assertEqual(len(rows), sum(
            1 for path in (ROOT / "translations" / "en").glob("ui*.jsonl")
            for line in path.read_text(encoding="utf-8").splitlines() if line.strip()
        ))
        grouped = {}
        for row in rows:
            grouped.setdefault(row["member"], []).append(row)
        for member, member_rows in grouped.items():
            original = (ROOT / "translations" / "source-ja" / "table" / member).read_bytes()
            patched, touched = MODULE.patch_csv(original, member_rows)
            self.assertEqual(len(touched), len(member_rows))
            self.assertNotEqual(patched, original)
            cells_before = MODULE.csv_cells(original)
            cells_after = MODULE.csv_cells(patched)
            selected = {(row["record"] - 1, row["field"] - 1): row for row in member_rows}
            self.assertEqual(len(cells_before), len(cells_after))
            for ri, (before_row, after_row) in enumerate(zip(cells_before, cells_after)):
                self.assertEqual(len(before_row), len(after_row))
                for fi, (before, after) in enumerate(zip(before_row, after_row)):
                    before_text = before[2].decode("cp932")
                    after_text = after[2].decode("cp932")
                    if (ri, fi) in selected:
                        self.assertEqual(before_text, selected[ri, fi]["source"])
                        self.assertEqual(after_text, selected[ri, fi]["english"])
                    else:
                        self.assertEqual(before[2:], after[2:])

    def test_csv_replacement_preserves_record_and_quote_format(self):
        original = b'key,"Japanese, text"\r\nother,plain\n'
        rows = [{"id": "table/strtable.csv:1:2", "record": 1, "field": 2,
                 "source": "Japanese, text", "english": "English, text"}]
        patched, _ = MODULE.patch_csv(original, rows)
        self.assertEqual(patched, b'key,"English, text"\r\nother,plain\n')

    def test_archive_appends_raw_replacement_and_changes_only_target_record(self):
        names = struct.pack("<I", 3) + b"first.bin\0strtable.csv\0third.bin\0\0"
        old_target = b"old"
        first = b"compressed-bytes"
        third = b"leave-this-compressed"
        dat = names + old_target + first + third
        records = [(0, len(names), len(names)),
                   (len(names) + len(old_target), 0x80000000 | len(first), 100),
                   (len(names), len(old_target), len(old_target)),
                   (len(names) + len(old_target) + len(first), 0x80000000 | len(third), 99)]
        hed = b"".join(struct.pack("<III", *record) for record in records)
        end_marker = struct.pack("<III", 0, 0, 0)
        rebuilt_hed, rebuilt_dat = MODULE.make_archives(
            hed + end_marker, dat, {1: b"first replacement", 2: b"new English"}
        )
        rebuilt_records = MODULE.parse_hed(rebuilt_hed)
        self.assertEqual(rebuilt_records[0], records[0])
        self.assertEqual(rebuilt_records[1], (len(dat), len(b"first replacement"), len(b"first replacement")))
        self.assertEqual(rebuilt_records[2], (len(dat) + len(b"first replacement"), len(b"new English"), len(b"new English")))
        self.assertEqual(rebuilt_records[3], records[3])
        self.assertEqual(rebuilt_dat[:len(dat)], dat)
        self.assertEqual(rebuilt_dat[len(dat):], b"first replacementnew English")
        self.assertEqual(rebuilt_hed[-12:], end_marker)


if __name__ == "__main__":
    unittest.main()
