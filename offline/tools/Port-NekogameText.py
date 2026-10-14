#!/usr/bin/env python3
"""Generate translation rows from the Nekogame client's English table text.

Both clients run the same eco.exe (v1.239.0.1). Nekogame v935 ships translated
table CSVs. A Nekogame cell is used for an eco_jp cell only when:
  * the rows share a unique key (first field) in both tables,
  * the eco_jp cell has Japanese and the Nekogame cell has none,
  * the row's other non-Japanese fields mostly match (same content, not a reused ID),
  * control/printf tokens are unchanged and the text is CP932-encodable,
  * the text does not look Indonesian (Nekogame mixes a few in).
Output rows use the Build-EnglishUi.py JSONL format with "origin": "nekogame".
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BUILDER = ROOT / "tools" / "Build-EnglishUi.py"
EXPORTER = ROOT / "target" / "export-client-text.exe"
BASELINE = ROOT / "translations" / "build" / "original-client"

JAPANESE = re.compile(r"[぀-ヿ㐀-鿿ｦ-ﾟ]")
INDONESIAN = re.compile(
    r"\b(kalahkan|mengalahkan|dan|yang|untuk|dengan|tidak|ini|itu|dari|ke|di|"
    r"kumpulkan|bawa|cari|hadiah|antar|serahkan|bicara|temui|pergi|kembali|"
    r"hari|orang|sudah|belum|akan|bisa|harus|saya|kamu|anda|pada|adalah|atau|"
    r"bagian|dapat|lebih|sangat|milik|khusus|ketika|memiliki|jika|seperti|dalam|"
    r"oleh|bila|saat|sebuah|para|juga|tetapi|bahwa|mereka|kita|aku|apa|tak|kali|"
    r"cukup|setiap|semua|ada|sama|mau|lagi|dia|tanpa|telah|tersebut|menjadi|"
    r"serangan|lawan|dipasang|digunakan|memberikan|menambah|meningkatkan|"
    r"menurunkan|memulihkan|terdapat|salah|satu|jenis|hanya|kecil|besar|baru|"
    r"detik|mohon|selama|setelah|memasuki|ruangan|berhasil|menurun|berkurang|berikan|"
    r"membuat|membeli|bertemu|membunuh|berburu|bertahan|terbaik|kemampuan|kerusakan)\b"
    # Indonesian prefix+suffix word shapes (permintaan, pengiriman, berisikan, ...).
    r"|\b(?:me[mn]g?|pe[mnr]g?|ber|ter|ke|di)[a-z]{2,}(?:kan|an|nya)\b|\b[a-z]{3,}nya\b",
    re.I,
)
ENGLISH = re.compile(
    r"\b(the|a|an|of|to|and|is|in|for|with|can|be|you|your|when|this|it|on|at|by|"
    r"from|will|has|are|was|not|or|that|if|all|use|used|item|equip|equipment)\b",
    re.I,
)
WORD = re.compile(r"[A-Za-z]{2,}")


def looks_foreign(text: str) -> bool:
    """Indonesian word, or sentence-length text with no common English word at all."""
    plain = text.replace("$R", " ").replace("$r", " ")
    if INDONESIAN.search(plain):
        return True
    sentence = "$R" in text or "$r" in text or len(WORD.findall(plain)) >= 8
    return sentence and not ENGLISH.search(plain)


MIN_ROW_AGREEMENT = 0.8


def load_builder():
    spec = importlib.util.spec_from_file_location("build_english_ui", BUILDER)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def export(client_root: Path, output: Path) -> Path:
    result = subprocess.run([str(EXPORTER), str(client_root), str(output)],
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    if result.returncode != 0:
        raise SystemExit(f"exporter failed for {client_root}")
    return output / "table"


def decoded_rows(builder, raw: bytes) -> list[list[str]] | None:
    try:
        return [[cell[2].decode("cp932") for cell in record] for record in builder.csv_cells(raw)]
    except (UnicodeError, ValueError):
        return None


def unpatchable_cells(builder, raw: bytes) -> set[tuple[int, int]]:
    """Cells Build-EnglishUi.py refuses: quoted cells with bytes after the closing quote."""
    unsafe = set()
    for ri, record in enumerate(builder.csv_cells(raw)):
        for fi, (start, end, decoded, quoted) in enumerate(record):
            span = raw[start:end].replace(b'""', b'"') if quoted else raw[start:end]
            if span != decoded:
                unsafe.add((ri, fi))
    return unsafe


def keyed(rows: list[list[str]]) -> dict[str, int]:
    seen: dict[str, int] = {}
    duplicate = set()
    for index, row in enumerate(rows):
        if not row or not row[0].strip() or row[0].lstrip().startswith("#"):
            continue
        key = row[0].strip()
        if key in seen:
            duplicate.add(key)
        seen[key] = index
    return {key: index for key, index in seen.items() if key not in duplicate}


def agreement(jp: list[str], neko: list[str]) -> float:
    pairs = [(a, b) for a, b in zip(jp, neko) if not JAPANESE.search(a)]
    if not pairs:
        return 1.0
    return sum(a.strip() == b.strip() for a, b in pairs) / len(pairs)


def port_table(builder, member: str, jp_raw: bytes, neko_raw: bytes, stats: dict,
               rejects: list) -> list[dict]:
    jp_rows, neko_rows = decoded_rows(builder, jp_raw), decoded_rows(builder, neko_raw)
    unsafe = unpatchable_cells(builder, jp_raw) if jp_rows is not None else set()
    if jp_rows is None or neko_rows is None:
        stats["unparsed"] += 1
        return []
    jp_keys, neko_keys = keyed(jp_rows), keyed(neko_rows)
    result = []
    for key, ri in jp_keys.items():
        ni = neko_keys.get(key)
        jp, neko = jp_rows[ri], neko_rows[ni] if ni is not None else None
        japanese_fields = [fi for fi, cell in enumerate(jp) if JAPANESE.search(cell)]
        if not japanese_fields:
            continue
        if neko is None:
            stats["no_matching_row"] += len(japanese_fields)
            continue
        if agreement(jp, neko) < MIN_ROW_AGREEMENT:
            stats["row_differs"] += len(japanese_fields)
            continue
        for fi in japanese_fields:
            if (ri, fi) in unsafe:
                stats["unpatchable"] += 1
                continue
            english = neko[fi] if fi < len(neko) else ""
            if not english.strip() or JAPANESE.search(english) or english == jp[fi]:
                stats["not_translated"] += 1
                continue
            reason = None
            if looks_foreign(english):
                reason = "indonesian"
            elif builder.TOKENS.findall(jp[fi]) != builder.TOKENS.findall(english):
                reason = "token_mismatch"
            if reason:
                stats[reason] += 1
                rejects.append({"id": f"table/{member}:{ri + 1}:{fi + 1}", "reason": reason,
                                "source": jp[fi], "nekogame": english})
                continue
            try:
                english.encode("cp932")
            except UnicodeError:
                stats["not_cp932"] += 1
                continue
            result.append({"id": f"table/{member}:{ri + 1}:{fi + 1}", "source": jp[fi],
                           "english": english, "status": "draft", "origin": "nekogame"})
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--nekogame", type=Path, required=True,
                        help="Nekogame client folder (the one that holds eco.exe and data/)")
    parser.add_argument("--output", type=Path, required=True,
                        help="directory for one nekogame-<table>.jsonl per table")
    args = parser.parse_args()
    if args.output.exists():
        raise SystemExit("output directory already exists")
    builder = load_builder()
    stats = {k: 0 for k in ("unparsed", "no_matching_row", "row_differs", "not_translated",
                            "indonesian", "token_mismatch", "not_cp932", "unpatchable")}
    per_table = {}
    rejects: list = []
    with tempfile.TemporaryDirectory() as temporary:
        jp_dir = export(BASELINE, Path(temporary) / "jp")
        neko_dir = export(args.nekogame, Path(temporary) / "neko")
        args.output.mkdir(parents=True)
        for jp_path in sorted(jp_dir.iterdir()):
            neko_path = neko_dir / jp_path.name
            if not neko_path.is_file():
                continue
            rows = port_table(builder, jp_path.name, jp_path.read_bytes(), neko_path.read_bytes(),
                              stats, rejects)
            if rows:
                per_table[jp_path.name] = len(rows)
                stem = jp_path.name.removesuffix(".csv")
                with (args.output / f"nekogame-{stem}.jsonl").open("w", encoding="utf-8", newline="\n") as out:
                    for row in rows:
                        out.write(json.dumps(row, ensure_ascii=False) + "\n")
    with (args.output / "rejected.jsonl").open("w", encoding="utf-8", newline="\n") as out:
        for row in rejects:
            out.write(json.dumps(row, ensure_ascii=False) + "\n")
    print(json.dumps({"cells": sum(per_table.values()), "tables": per_table, "skipped": stats}, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
