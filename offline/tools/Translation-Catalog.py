"""Build and check a source-linked ECO translation catalog (stdlib only)."""
import argparse
import codecs
import csv
import hashlib
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1] / "translations"
SOURCE = ROOT / "source-ja"
CATALOG = ROOT / "catalog.jsonl"
INVENTORY = ROOT / "inventory.json"
TRANSLATIONS = ROOT / "translations.jsonl"
TOKENS = re.compile(r"\$[A-Za-z*] | %(?:%|[-+#0 ]*(?:\d+|\*)?(?:\.(?:\d+|\*))?(?:hh|h|ll|l|j|z|t|L)?[diuoxXfFeEgGaAcsp]) | \{[^{}]*\}", re.X)
SUSPECT = re.compile(r"\$|%|\{[^{}]*\}")
KNOWN_CODES = {"$R", "$P", "$r", "$*"}
CJK = re.compile(r"[\u3040-\u30ff\u3400-\u9fff\uf900-\ufaff\uff66-\uff9f]")


def digest(path):
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()


def decode_source(raw):
    if raw.startswith(codecs.BOM_UTF8):
        return raw.decode("utf-8-sig", errors="strict"), "utf-8-sig"
    if raw.startswith(codecs.BOM_UTF16_LE):
        return raw.decode("utf-16", errors="strict"), "utf-16le"
    if raw.startswith(codecs.BOM_UTF16_BE):
        return raw.decode("utf-16", errors="strict"), "utf-16be"
    return raw.decode("cp932", errors="strict"), "cp932"


def build(source=SOURCE, catalog=CATALOG, inventory=INVENTORY):
    source, catalog, inventory = map(Path, (source, catalog, inventory))
    if catalog.exists() or inventory.exists():
        raise FileExistsError("catalog or inventory already exists")
    if not source.is_dir() or any(not (source / area).is_dir() for area in ("table", "text")):
        raise FileNotFoundError("source-ja table/text directories are required")
    if not any(path.is_file() for area in ("table", "text") for path in (source / area).rglob("*")):
        raise ValueError("source-ja contains no files")
    files = []
    entries = []
    for area in ("table", "text"):
        folder = source / area
        for path in sorted(folder.rglob("*")):
            if not path.is_file():
                continue
            rel = path.relative_to(source).as_posix()
            info = {"file": rel, "sha256": digest(path), "partial": False}
            file_entries = []
            encoding = "unknown"
            try:
                data, encoding = decode_source(path.read_bytes())
                info.update(encoding=encoding, errors=[])
                if area == "table" and data.lstrip("\ufeff\r\n \t").startswith("<?xml"):
                    info.update(format="xml", not_cataloged="XML content in a .csv source", candidate_count=0)
                    files.append(info)
                    continue
                if area == "table" and path.suffix.lower() == ".csv":
                    rows = csv.reader(data.splitlines(keepends=True), strict=True)
                    for record, row in enumerate(rows, 1):
                        if not row or row[0].lstrip().startswith(("#", "//")):
                            continue
                        key = row[0]
                        for field, cell in enumerate(row, 1):
                            if CJK.search(cell):
                                file_entries.append({"id": f"{rel}:{record}:{field}", "source_file": rel,
                                                     "record": record, "field": field, "key": key, "source": cell})
                    entries.extend(file_entries)
            except (UnicodeDecodeError, csv.Error) as e:
                info.update(encoding=encoding, errors=[type(e).__name__], partial=bool(file_entries))
                if isinstance(e, csv.Error):
                    info["error_line"] = rows.line_num
                entries.extend(file_entries)
            info["candidate_count"] = len(file_entries)
            files.append(info)
    catalog.parent.mkdir(parents=True, exist_ok=True)
    inventory.parent.mkdir(parents=True, exist_ok=True)
    with catalog.open("x", encoding="utf-8", newline="\n") as f:
        f.write("".join(json.dumps(e, ensure_ascii=False) + "\n" for e in entries))
    with inventory.open("x", encoding="utf-8") as f:
        f.write(json.dumps({"files": files, "candidate_count": len(entries)}, ensure_ascii=False, indent=2) + "\n")
    return len(entries)


def check(catalog=CATALOG, translations=TRANSLATIONS):
    source = {}
    for line in Path(catalog).read_text(encoding="utf-8").splitlines():
        row = json.loads(line)
        if row["id"] in source:
            raise ValueError("duplicate source id")
        source[row["id"]] = row["source"]
    seen = set()
    review = []
    for line_no, line in enumerate(Path(translations).read_text(encoding="utf-8").splitlines(), 1):
        row = json.loads(line)
        if not isinstance(row, dict):
            raise ValueError(f"invalid translation row at line {line_no}")
        ident, original, english, status = (row.get(k) for k in ("id", "source", "english", "status"))
        if not isinstance(ident, str) or ident not in source:
            raise ValueError(f"unknown source id at translation line {line_no}")
        if ident in seen:
            raise ValueError(f"duplicate translation id at line {line_no}")
        seen.add(ident)
        if original != source[ident]:
            raise ValueError(f"source mismatch at translation line {line_no}")
        if not isinstance(english, str) or not english.strip() or status not in ("draft", "reviewed"):
            raise ValueError(f"invalid translation at line {line_no}")
        if TOKENS.findall(original) != TOKENS.findall(english):
            raise ValueError(f"format/control token mismatch at translation line {line_no}")
        try:
            english.encode("cp932", errors="strict")
        except UnicodeEncodeError:
            raise ValueError(f"translation is not CP932-encodable at line {line_no}") from None
        original_codes = [t for t in TOKENS.findall(original) if t.startswith("$")]
        english_codes = [t for t in TOKENS.findall(english) if t.startswith("$")]
        if (SUSPECT.search(TOKENS.sub("", original)) or SUSPECT.search(TOKENS.sub("", english))
                or any(t not in KNOWN_CODES for t in original_codes + english_codes)):
            review.append(line_no)
    return {"translated": len(seen), "source_count": len(source), "needs_code_review": review}


def main():
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="action", required=True)
    b = sub.add_parser("build")
    b.add_argument("--source", type=Path, default=SOURCE)
    b.add_argument("--catalog", type=Path, default=CATALOG)
    b.add_argument("--inventory", type=Path, default=INVENTORY)
    c = sub.add_parser("check")
    c.add_argument("--catalog", type=Path, default=CATALOG)
    c.add_argument("--translations", type=Path, default=TRANSLATIONS)
    args = parser.parse_args()
    try:
        result = build(args.source, args.catalog, args.inventory) if args.action == "build" else check(args.catalog, args.translations)
        print(json.dumps(result, ensure_ascii=False))
    except (OSError, ValueError, csv.Error) as e:
        print(f"translation catalog {type(e).__name__}: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
