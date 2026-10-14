"""Deterministic read-only inventory of the checked-in SagaECO content."""
from __future__ import annotations

import argparse
import csv
import re
import xml.etree.ElementTree as ET
from collections import Counter
from pathlib import Path


DB_REL = Path("research/SagaECO/Configuration/DB")


def _xml_files(path: Path):
    return sorted(path.rglob("*.xml"), key=lambda p: p.as_posix().casefold()) if path.is_dir() else []


def _int(text):
    try:
        value = int((text or "").strip(), 10)
        return value if 0 <= value <= 0xFFFFFFFF else None
    except ValueError:
        return None


def _csv_write(path: Path, header, rows):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", newline="", encoding="utf-8") as f:
        writer = csv.writer(f, lineterminator="\n")
        writer.writerow(header)
        writer.writerows(rows)


def _systems(root: Path):
    """Report prototype areas with exact source evidence; names are descriptive."""
    candidates = {
        "quest replay and lifecycle": ("offline/src/quest.rs", r"pub fn replay|pub\(crate\) fn apply_lifecycle"),
        "content reference audit": ("offline/src/content.rs", r"pub fn audit"),
        "save persistence": ("offline/src/store.rs", r"pub struct Store|fn open_existing|CREATE TABLE"),
        "map and terrain handling": ("offline/src/map.rs", r"pub|fn "),
        "character progression": ("offline/src/progression.rs", r"pub|fn "),
        "network protocol": ("offline/src/protocol.rs", r"pub|fn "),
    }
    results = []
    for name, (rel, pattern) in candidates.items():
        file = root / rel
        if not file.is_file():
            continue
        for number, line in enumerate(file.read_text(encoding="utf-8", errors="replace").splitlines(), 1):
            if re.search(pattern, line):
                results.append((name, f"{rel}:{number}", line.strip()[:180]))
                break
    return results


def run(root: Path, output: Path) -> dict:
    root, output = Path(root), Path(output)
    db = root / DB_REL
    monsters = set()
    monster_rows = 0
    duplicates = 0
    counts = Counter()
    findings = []
    monster_path = db / "monster.csv"
    for required in (monster_path, db / "Spawns", db / "Quests"):
        if not required.exists():
            raise FileNotFoundError(f"required content input is missing: {required}")
    with monster_path.open(encoding="utf-8-sig", newline="") as f:
        for rowno, row in enumerate(csv.reader(f), 1):
            if not row or row[0].lstrip().startswith("#"):
                continue
            monster_rows += 1
            ident = _int(row[0])
            if ident is None:
                findings.append(("invalid_monster_id", "monster.csv", str(rowno), "monster row ID is not a uint32 decimal integer"))
            elif ident in monsters:
                duplicates += 1
                findings.append(("duplicate_monster_id", "monster.csv", str(rowno), f"duplicate monster ID {ident}"))
            else:
                monsters.add(ident)

    spawn_files = _xml_files(db / "Spawns")
    spawn_records = 0
    spawn_ids = set()
    for file in spawn_files:
        rel = file.relative_to(root).as_posix()
        try:
            doc = ET.parse(file).getroot()
        except (ET.ParseError, OSError, UnicodeError):
            findings.append(("unreadable_spawn_xml", rel, "", "XML could not be parsed"))
            continue
        if doc.tag != "Spawns":
            findings.append(("unexpected_spawn_root", rel, "", f"expected Spawns root, found {doc.tag}"))
            continue
        for index, spawn in enumerate(doc.findall("spawn"), 1):
            spawn_records += 1
            ident = _int(spawn.findtext("id"))
            if ident is None:
                findings.append(("invalid_spawn_id", rel, str(index), "spawn id is missing or not a decimal integer"))
            else:
                spawn_ids.add(ident)
                if ident not in monsters:
                    findings.append(("spawn_monster_missing", rel, str(index), f"spawn references monster ID {ident} absent from monster.csv"))

    quest_files = _xml_files(db / "Quests")
    quest_records = hunt_records = 0
    quest_ids = set()
    for file in quest_files:
        rel = file.relative_to(root).as_posix()
        try:
            doc = ET.parse(file).getroot()
        except (ET.ParseError, OSError, UnicodeError):
            findings.append(("unreadable_quest_xml", rel, "", "XML could not be parsed"))
            continue
        if doc.tag != "QuestDB":
            findings.append(("unexpected_quest_root", rel, "", f"expected QuestDB root, found {doc.tag}"))
            continue
        for index, quest in enumerate(doc.findall("Quest"), 1):
            quest_records += 1
            qid = _int(quest.findtext("ID"))
            if qid is None:
                findings.append(("invalid_quest_id", rel, str(index), "quest ID is missing or not a uint32 decimal integer"))
            else:
                if qid in quest_ids:
                    findings.append(("duplicate_quest_id", rel, str(index), f"duplicate quest ID {qid}"))
                quest_ids.add(qid)
            if (quest.findtext("Type") or "").strip().upper() == "HUNT":
                hunt_records += 1
                for tag in ("ObjectID1", "ObjectID2", "ObjectID3"):
                    raw = quest.findtext(tag)
                    target = _int(raw)
                    if target and target not in monsters:
                        findings.append(("hunt_monster_missing", rel, str(index), f"HUNT {tag} references monster ID {target} absent from monster.csv"))

    findings.sort(key=lambda x: (x[0], x[1].casefold(), x[2], x[3]))
    _csv_write(output / "audit.csv", ["finding", "source", "record", "evidence"], findings)
    systems = _systems(root)
    finding_counts = Counter(row[0] for row in findings)
    lines = [
        "# ECO static content audit", "",
        "This report reads checked-in SagaECO content and prototype source only. It does not connect to a server or open live saves.", "",
        "## Content inventory", "",
        f"- Monsters: {monster_rows} CSV rows; {len(monsters)} unique decimal IDs; {duplicates} duplicate IDs.",
        f"- Spawns: {len(spawn_files)} XML files; {spawn_records} top-level spawn records; {len(spawn_ids)} unique monster IDs referenced.",
        f"- Quests: {len(quest_files)} XML files; {quest_records} top-level quest records; {len(quest_ids)} unique valid quest IDs; {hunt_records} HUNT quests.",
        f"- Static findings: {len(findings)}. Full evidence is in `audit.csv`.", "",
        "## Findings by count", "",
    ]
    lines.extend(f"- `{kind}`: {count}" for kind, count in sorted(finding_counts.items(), key=lambda item: (-item[1], item[0])))
    if not finding_counts:
        lines.append("- None.")
    lines += ["", "## Prototype systems evidenced", ""]
    lines.extend(f"- {name}: `{evidence}` — `{snippet}`" for name, evidence, snippet in systems)
    lines += ["", "## Coverage and limits", "",
              "The checked references are spawn monster IDs and HUNT quest target IDs against monster.csv, plus duplicate IDs and unreadable source XML. Other quest objective types, rewards, map geometry, runtime behavior, and dynamic C# control flow are outside this audit.",
              "No live save or server data is loaded by this audit. Static dataset reads are limited to the checked-in Configuration/DB files.",
              "Counts and finding rows use stable path and value ordering; no generation timestamp is included.", ""]
    (output / "AUDIT.md").parent.mkdir(parents=True, exist_ok=True)
    (output / "AUDIT.md").write_text("\n".join(lines), encoding="utf-8", newline="\n")
    return {"monsters": len(monsters), "monster_rows": monster_rows, "spawn_files": len(spawn_files),
            "spawn_records": spawn_records, "quest_files": len(quest_files), "quest_records": quest_records,
            "hunt_quests": hunt_records, "findings": len(findings), "output": str(output)}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[3])
    parser.add_argument("--output", type=Path, default=None)
    args = parser.parse_args()
    result = run(args.root, args.output or args.root / "offline/docs/audit")
    print(result)
