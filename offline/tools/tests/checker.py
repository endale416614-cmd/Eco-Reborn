"""Conservative static checks for checked-in content and literal script destinations."""
from __future__ import annotations

import argparse
import csv
import re
import xml.etree.ElementTree as ET
from collections import Counter
from pathlib import Path

from audit import DB_REL, _csv_write, _int, _xml_files

WARP = re.compile(r"\bWarp\s*\(\s*pc\s*,\s*(\d+)\s*,")
ASCII_ID = re.compile(r"^[0-9]+$")


def _code_lines(text):
    """Yield lines with comments and quoted literals blanked while preserving offsets."""
    block = False
    quote = None
    verbatim = False
    for line in text.splitlines() if isinstance(text, str) else text:
        out = list(line)
        i = 0
        escaped = False
        while i < len(line):
            if block:
                out[i] = " "
                if line.startswith("*/", i):
                    if i + 1 < len(out): out[i + 1] = " "
                    block = False
                    i += 2
                else:
                    i += 1
                continue
            if quote:
                out[i] = " "
                if verbatim and line[i] == quote and i + 1 < len(line) and line[i + 1] == quote:
                    out[i + 1] = " "
                    i += 2
                    continue
                if escaped:
                    escaped = False
                elif not verbatim and line[i] == "\\":
                    escaped = True
                elif line[i] == quote:
                    quote = None
                    verbatim = False
                i += 1
                continue
            if line.startswith("//", i):
                out[i:] = " " * (len(out) - i)
                break
            if line.startswith("/*", i):
                out[i:i + 2] = [" ", " "]
                block = True
                i += 2
                continue
            if line[i] in ('"', "'"):
                quote = line[i]
                verbatim = quote == '"' and i > 0 and line[i - 1] == "@"
                out[i] = " "
            i += 1
        if quote and not verbatim:
            quote = None
        yield "".join(out)


def run(root: Path, output: Path) -> dict:
    root, output = Path(root), Path(output)
    db = root / DB_REL
    findings = []
    monsters = set()
    monster_file = db / "monster.csv"
    for required in (monster_file, db / "Spawns", db / "Quests"):
        if not required.exists():
            raise FileNotFoundError(f"required checker input is missing: {required.name}")
    with monster_file.open(encoding="utf-8-sig", newline="") as f:
        for row in csv.reader(f):
            if row and not row[0].lstrip().startswith("#"):
                value = _int(row[0])
                if value is not None:
                    monsters.add(value)

    for directory in ("Spawns", "Quests"):
        for file in _xml_files(db / directory):
            rel = file.relative_to(root).as_posix()
            try:
                doc = ET.parse(file).getroot()
            except (ET.ParseError, OSError, UnicodeError) as exc:
                findings.append(("error", "source_data_mismatch", "original-data", rel, "", "", "XML parse failure", type(exc).__name__))
                continue
            expected_root = "Spawns" if directory == "Spawns" else "QuestDB"
            if doc.tag != expected_root:
                findings.append(("error", "source_data_mismatch", "original-data", rel, "", "", f"expected {expected_root} root, found {doc.tag}", f"{rel}: root={doc.tag}"))
                continue
            records = doc.findall("spawn") if directory == "Spawns" else doc.findall("Quest")
            for index, record in enumerate(records, 1):
                names = ("id",) if directory == "Spawns" else (("ObjectID1", "ObjectID2", "ObjectID3") if (record.findtext("Type") or "").strip().upper() == "HUNT" else ())
                for field in names:
                    raw = record.findtext(field)
                    ident = _int(raw)
                    if ident is not None and ident != 0 and ident not in monsters:
                        label = "spawn monster ID" if directory == "Spawns" else "HUNT target monster ID"
                        findings.append(("error", "source_data_mismatch", "original-data", rel, f"{directory}/{index}", str(ident),
                                         f"{label} {ident} is absent from the supplied monster.csv snapshot", f"{rel} record {index}, field {field}={ident}; supplied monster.csv contains no matching ID"))

    # Spawn map filenames and warp.csv values form a partial static map inventory.
    # Missing destinations are review leads only because these sources may omit valid maps.
    maps = {int(p.stem) for p in _xml_files(db / "Spawns") if p.stem.isdecimal()}
    warp_csv = db / "warp.csv"
    if warp_csv.is_file():
        # Latin-1 preserves every source byte; only the ASCII numeric map column is consumed.
        with warp_csv.open(encoding="latin-1", newline="") as f:
            for row in csv.reader(f):
                raw = row[1].strip() if len(row) > 1 else ""
                if row and not row[0].lstrip().startswith("#") and ASCII_ID.fullmatch(raw):
                    value = _int(raw)
                    if value is not None and value:
                        maps.add(value)
    scripts = root / "research/SagaECO/SagaScript"
    script_files = sorted(scripts.rglob("*.cs"), key=lambda p: p.as_posix().casefold()) if scripts.is_dir() else []
    literal_warps = 0
    for file in script_files:
        rel = file.relative_to(root).as_posix()
        try:
            script_text = file.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            # Legacy script encodings vary; lexical checks only consume ASCII tokens.
            script_text = file.read_text(encoding="latin-1")
        for line_no, line in enumerate(_code_lines(script_text.splitlines()), 1):
            for match in WARP.finditer(line):
                literal_warps += 1
                dest = int(match.group(1))
                if maps and dest not in maps:
                    findings.append(("review", "unresolved_reference", "unknown", rel, f"line/{line_no}", str(dest),
                                     f"literal Warp destination {dest} is absent from the partial static map inventory",
                                     f"{rel}:{line_no} has a lexical Warp(pc, {dest}, ...) call; inventory uses spawn XML filenames"))

    findings.sort(key=lambda row: (row[0], row[1], row[3].casefold(), row[4], row[5], row[6]))
    _csv_write(output / "checker.csv",
               ["level", "classification", "cause", "source", "record_or_line", "identifier", "message", "evidence"], findings)
    counts = {"source_data_mismatch": sum(row[1] == "source_data_mismatch" for row in findings),
              "unresolved_reference": sum(row[1] == "unresolved_reference" for row in findings)}
    categories = Counter(row[1] for row in findings)
    top = sorted(Counter(row[6] for row in findings).items(), key=lambda item: (-item[1], item[0]))[:10]
    summary = ["# ECO static checker", "", f"- C# files scanned: {len(script_files)}",
               f"- Literal Warp calls scanned after removing comments and string literals: {literal_warps}",
               f"- Static map IDs in partial inventory: {len(maps)}", f"- Findings: {len(findings)}", "",
               "## Classification counts", ""]
    summary.extend(f"- {key}: {value}" for key, value in sorted(categories.items()))
    if not categories: summary.append("- None.")
    summary += ["", "## Most frequent messages", ""]
    summary.extend(f"- {count}: {message}" for message, count in top)
    summary += ["", "## Limits", "",
                "Missing references in this historical content snapshot are source-data mismatches, not proof of runtime failure or an emulator defect. Warp destinations are checked only when written as numeric literals; the map inventory is partial, so unmatched destinations are unresolved review leads. warp.csv is read with lossless Latin-1 decoding and only ASCII decimal values in its map column are used. Comments and quoted literals are stripped before scanning. Dynamic C# values and control flow are not analyzed.", ""]
    (output / "CHECKER.md").parent.mkdir(parents=True, exist_ok=True)
    (output / "CHECKER.md").write_text("\n".join(summary), encoding="utf-8", newline="\n")
    return {"findings": len(findings), **counts, "literal_warps": literal_warps,
            "map_ids_in_partial_inventory": len(maps), "script_files": len(script_files), "output": str(output)}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[3])
    parser.add_argument("--output", type=Path, default=None)
    args = parser.parse_args()
    print(run(args.root, args.output or args.root / "offline/docs/audit"))
