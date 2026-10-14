#!/usr/bin/env python3
"""Build a staged, reversible English table-CSV archive patch.

Only source-verified cells from ui-*.jsonl are changed.  The modified archive
member is stored as an uncompressed HED block; all other DAT block bytes are
copied verbatim.  Outputs are restricted to offline/translations/build/.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import sys
import tempfile


ROOT = Path(__file__).resolve().parents[1]
BUILD_ROOT = (ROOT / "translations" / "build").resolve()
MANIFEST = ROOT / "translations" / "source-archives.json"
TRANSLATION_DIR = ROOT / "translations" / "en"
HED_RECORD = struct.Struct("<III")
ASCII_SPECIAL = b',"\r\n'
TOKENS = re.compile(r"\$[A-Za-z*] | %(?:%|[-+#0 ]*(?:\d+|\*)?(?:\.(?:\d+|\*))?(?:hh|h|ll|l|j|z|t|L)?[diuoxXfFeEgGaAcsp]) | \{[^{}]*\}", re.X)


def fail(message: str) -> ValueError:
    return ValueError(message)


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def read_manifest() -> dict[str, str]:
    doc = json.loads(MANIFEST.read_text(encoding="utf-8"))
    return {item["path"]: item["sha256"] for item in doc["files"]}


def load_translations(files: list[Path] | None = None) -> list[dict[str, str]]:
    paths = sorted(files if files is not None else TRANSLATION_DIR.glob("ui*.jsonl"))
    if not paths:
        raise fail("no UI translation files")
    result = []
    seen = set()
    for path in paths:
        for line in path.read_text(encoding="utf-8").splitlines():
            row = json.loads(line)
            if not isinstance(row, dict) or row.get("status") not in ("draft", "reviewed"):
                raise fail("invalid translation row")
            ident, source, english = (row.get(key) for key in ("id", "source", "english"))
            match = re.fullmatch(r"table/([A-Za-z0-9_.-]+\.csv):(\d+):(\d+)", ident or "")
            if (not match or not isinstance(source, str) or not isinstance(english, str)
                    or not english.strip()):
                raise fail("translation ID or text is invalid")
            if ident in seen:
                raise fail("duplicate translation ID")
            seen.add(ident)
            if int(match.group(2)) < 1 or int(match.group(3)) < 1:
                raise fail("translation record and field indexes must be positive")
            if TOKENS.findall(source) != TOKENS.findall(english):
                raise fail("translation changes ordered control or printf tokens")
            result.append({"id": ident, "member": match.group(1),
                           "record": int(match.group(2)), "field": int(match.group(3)), "source": source,
                           "english": english})
    return result


def csv_cells(raw: bytes) -> list[list[tuple[int, int, bytes, bool]]]:
    """Return per-record cell content spans and decoded CP932 field bytes.

    This bounded lexer supports RFC-style doubled quotes and tolerates spaces
    after a closing quote (the known strtable boundary). It never normalizes
    the original bytes. CRLF/LF/CR record delimiters are preserved by spans.
    """
    records = []
    cells = []
    i = 0
    n = len(raw)
    while i < n:
        quoted = raw[i:i + 1] == b'"'
        if quoted:
            content_start = i + 1
            i += 1
            decoded = bytearray()
            while i < n:
                if raw[i] == 34:
                    if i + 1 < n and raw[i + 1] == 34:
                        decoded.append(34)
                        i += 2
                    else:
                        content_end = i
                        after = i + 1
                        while after < n and raw[after] in (32, 9):
                            after += 1
                        if after == n or raw[after] in (44, 10, 13):
                            # Keep trailing whitespace in the value as CPython's
                            # permissive reader does, but outside the byte span.
                            decoded.extend(raw[i + 1:after])
                            i = after
                            break
                        # The source contains permissively parsed quote bytes in
                        # this field. Treat this quote as data, like csv.reader.
                        decoded.append(34)
                        i += 1
                else:
                    decoded.append(raw[i])
                    i += 1
            else:
                raise fail("unterminated quoted CSV cell")
            cells.append((content_start, content_end, bytes(decoded), True))
            if i < n and raw[i] == 44:
                i += 1
                continue
            if i < n and raw[i] in (10, 13):
                if raw[i:i + 2] == b"\r\n":
                    i += 2
                else:
                    i += 1
                records.append(cells)
                cells = []
                continue
            if i == n:
                records.append(cells)
                cells = []
                break
            raise fail("unexpected byte after quoted CSV cell")
        else:
            content_start = i
            while i < n and raw[i] not in (44, 10, 13):
                i += 1
            content_end = i
            cells.append((content_start, content_end, raw[content_start:content_end], False))
            if i < n and raw[i] == 44:
                i += 1
                continue
            if i < n:
                if raw[i:i + 2] == b"\r\n":
                    i += 2
                else:
                    i += 1
            records.append(cells)
            cells = []
    if cells:
        records.append(cells)
    return records


def patch_csv(raw: bytes, translations: list[dict[str, str]]) -> tuple[bytes, set[tuple[int, int]]]:
    records = csv_cells(raw)
    replacements: list[tuple[int, int, bytes]] = []
    touched = set()
    for row in translations:
        ri, fi = row["record"] - 1, row["field"] - 1
        if ri < 0 or fi < 0 or ri >= len(records) or fi >= len(records[ri]):
            raise fail("translation location is outside its source CSV")
        start, end, old, was_quoted = records[ri][fi]
        try:
            source = old.decode("cp932", errors="strict")
            encoded_cell = raw[start:end].replace(b'""', b'"') if was_quoted else raw[start:end]
            english = row["english"].encode("cp932", errors="strict")
        except UnicodeError as exc:
            raise fail("source or English text is not strict CP932") from exc
        if encoded_cell != old:
            raise fail("refusing to replace a quoted cell with trailing formatting bytes")
        if source != row["source"]:
            raise fail("translation source does not match the exact CSV cell")
        if was_quoted:
            body = english.replace(b'"', b'""')
            replacements.append((start, end, body))
        elif any(byte in english for byte in ASCII_SPECIAL):
            replacements.append((start, end, b'"' + english.replace(b'"', b'""') + b'"'))
        else:
            replacements.append((start, end, english))
        touched.add((ri, fi))
    output = raw
    for start, end, value in sorted(replacements, reverse=True):
        output = output[:start] + value + output[end:]
    return output, touched


def parse_hed(data: bytes) -> list[tuple[int, int, int]]:
    if len(data) % HED_RECORD.size:
        raise fail("HED has a partial record")
    records = [HED_RECORD.unpack_from(data, i) for i in range(0, len(data), HED_RECORD.size)]
    if records and records[-1] == (0, 0, 0):
        records.pop()
    return records


def make_archives(hed: bytes, dat: bytes, replacements: dict[int, bytes]) -> tuple[bytes, bytes]:
    records = parse_hed(hed)
    if not records:
        raise fail("empty HED")
    if any(ordinal < 1 or ordinal >= len(records) for ordinal in replacements):
        raise fail("target ordinal is outside the HED records")
    for record in records:
        offset, packed_word, unpacked = record
        packed_len = packed_word & 0x7FFFFFFF
        end = offset + packed_len
        if end > len(dat):
            raise fail("HED block lies outside DAT")

    # HED ordinal order is independent of DAT offsets; blocks may be placed out
    # of order or share bytes. Appending replacements leaves every old range intact.
    new_dat = bytearray(dat)
    new_records = list(records)
    for ordinal, replacement in sorted(replacements.items()):
        new_offset = len(new_dat)
        new_dat.extend(replacement)
        new_records[ordinal] = (new_offset, len(replacement), len(replacement))
    new_hed = b"".join(HED_RECORD.pack(*entry) for entry in new_records)
    if len(hed) != len(new_hed):
        new_hed += hed[len(new_hed):]
    return new_hed, bytes(new_dat)


def build(client_root: Path, output_dir: Path, translation_files: list[Path] | None = None) -> dict[str, str]:
    client_root = client_root.resolve()
    output_dir = output_dir.resolve()
    if not output_dir.is_relative_to(BUILD_ROOT):
        raise fail("output must be beneath offline/translations/build")
    if output_dir.exists():
        raise fail("output directory already exists")
    expected = read_manifest()
    source_paths = {name: client_root / name.removeprefix("eco_jp/") for name in expected}
    for name in ("eco_jp/data/xls/table.hed", "eco_jp/data/xls/table.dat"):
        if sha256(source_paths[name].read_bytes()) != expected[name]:
            raise fail("source archive hash differs from the verified manifest")
    hed_path, dat_path = source_paths["eco_jp/data/xls/table.hed"], source_paths["eco_jp/data/xls/table.dat"]
    hed, dat = hed_path.read_bytes(), dat_path.read_bytes()
    # The current Rust reader is the authority for decompression and archive
    # names, so use its existing exporter before changing any member.
    export_exe = ROOT / "target" / "export-client-text.exe"
    if not export_exe.is_file():
        raise fail("existing verified Export-ClientText executable is required")
    translations = load_translations(translation_files)
    BUILD_ROOT.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".source-", dir=BUILD_ROOT) as temporary:
        exported = Path(temporary) / "source-export"
        run_export(export_exe, client_root, exported)
        with (exported / "manifest.tsv").open(encoding="utf-8", newline="") as stream:
            manifest_rows = list(csv.DictReader(stream, delimiter="\t"))
        names = {row["name"]: int(row["ordinal"]) for row in manifest_rows if row["archive"] == "table"}
        if len(names) != sum(row["archive"] == "table" for row in manifest_rows):
            raise fail("source export has duplicate table member names")
        grouped: dict[str, list[dict[str, str]]] = {}
        for row in translations:
            grouped.setdefault(row["member"], []).append(row)
        patched_members = {}
        touched_count = 0
        for member, rows in grouped.items():
            if member not in names:
                raise fail("translation member is missing from the table archive")
            source_raw = (exported / "table" / member).read_bytes()
            patched, touched = patch_csv(source_raw, rows)
            if not touched or len(touched) != len(rows):
                raise fail("not every translation mapped to one exact cell")
            patched_members[member] = patched
            touched_count += len(touched)
        if not touched_count or touched_count != len(translations):
            raise fail("not every translation maps to a unique source cell")
        target_ordinals = {names[member]: data for member, data in patched_members.items()}
        patched_members_for_verify = patched_members
    if not touched_count or touched_count != len(translations):
        raise fail("not every translation mapped to one exact cell")
    new_hed, new_dat = make_archives(hed, dat, target_ordinals)
    # Decode both archive versions with the existing Rust reader and compare
    # every extracted member. No codec reimplementation is used here.
    with tempfile.TemporaryDirectory(prefix=".roundtrip-", dir=BUILD_ROOT) as temporary:
        temp_root = Path(temporary)
        candidate = temp_root / "candidate"
        xls = candidate / "data" / "xls"
        xls.mkdir(parents=True)
        for name in ("table.hed", "table.dat", "text.hed", "text.dat"):
            shutil.copy2(client_root / "data" / "xls" / name, xls / name)
        (xls / "table.hed").write_bytes(new_hed)
        (xls / "table.dat").write_bytes(new_dat)
        before, after = temp_root / "before", temp_root / "after"
        run_export(export_exe, client_root, before)
        run_export(export_exe, candidate, after)
        for area in ("table", "text"):
            original_files = {p.relative_to(before / area): p.read_bytes()
                              for p in (before / area).iterdir() if p.is_file()}
            rebuilt_files = {p.relative_to(after / area): p.read_bytes()
                             for p in (after / area).iterdir() if p.is_file()}
            if original_files.keys() != rebuilt_files.keys():
                raise fail("round-trip changed archive entry names")
            for relative, original in original_files.items():
                expected = patched_members_for_verify.get(relative.as_posix(), original) if area == "table" else original
                if rebuilt_files[relative] != expected:
                    raise fail("round-trip changed an unselected archive member")
    output_dir.parent.mkdir(parents=True, exist_ok=True)
    output_dir.mkdir()
    (output_dir / "table.hed").write_bytes(new_hed)
    (output_dir / "table.dat").write_bytes(new_dat)
    (output_dir / "source.sha256.json").write_text(json.dumps({
        "table.hed": sha256(hed), "table.dat": sha256(dat),
        "translations": len(translations), "changed_cells": touched_count,
    }, indent=2) + "\n", encoding="utf-8")
    return {"archive_entries": str(len(parse_hed(hed)) - 1), "changed_cells": str(touched_count),
            "changed_members": str(len(patched_members_for_verify))}


def run_export(executable: Path, client_root: Path, output: Path) -> None:
    result = subprocess.run([str(executable), str(client_root), str(output)],
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    if result.returncode != 0:
        raise fail("verified Rust exporter rejected source or rebuilt archive")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("client_root", type=Path, nargs="?", default=ROOT.parent / "eco_jp")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--translations", type=Path, action="append")
    args = parser.parse_args()
    try:
        summary = build(args.client_root, args.output, args.translations)
    except Exception as exc:
        print(f"Build failed: {exc}", file=sys.stderr)
        return 1
    print("Staged table archive: " + ", ".join(f"{k}={v}" for k, v in summary.items()))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
