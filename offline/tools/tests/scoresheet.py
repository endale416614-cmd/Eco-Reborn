"""Historical-catalog implementation scoreboard; source presence is not gameplay verification."""
from __future__ import annotations

import argparse
import csv
import hashlib
import json
import xml.etree.ElementTree as ET
from pathlib import Path

from audit import DB_REL, _csv_write, _int, _xml_files

FIELDS = ["id", "category", "status", "cause", "evidence", "upstream_issue",
          "last_verified_build", "verified_by"]
STATUSES = {"verified", "implemented-unverified", "partial", "missing", "blocked"}


def _evidence(root: Path, file: Path, selector: str) -> str:
    return f"{file.relative_to(root).as_posix()} XPath {selector}"


def _catalog(root: Path):
    db = root / DB_REL
    rows = []
    for required in (db / "SkillList.xml", db / "Dungeon/Dungeons.xml", db / "Quests"):
        if not required.exists():
            raise FileNotFoundError(f"required scoresheet input is missing: {required.name}")
    skill_file = db / "SkillList.xml"
    if skill_file.is_file():
        doc = ET.parse(skill_file).getroot()
        if doc.tag != "SkillList":
            raise ValueError("SkillList.xml has an unexpected root")
        for skills in doc.findall("Skills"):
            job = skills.get("Job")
            for node in skills.findall("SkillID"):
                value = _int(node.text)
                if job and job.upper() != "NOVICE" and value is not None:
                    selector = f"/SkillList/Skills[@Job='{job}']/SkillID[text()='{value}']"
                    ident = f"skill:{job}:{value}"
                    evidence = _evidence(root, skill_file, selector)
                    if not any(row[0] == ident for row in rows):
                        rows.append((ident, "gameplay", "blocked", "emulator", evidence, "", "", ""))

    dungeon_file = db / "Dungeon/Dungeons.xml"
    if dungeon_file.is_file():
        doc = ET.parse(dungeon_file).getroot()
        if doc.tag != "Dungeons":
            raise ValueError("Dungeon/Dungeons.xml has an unexpected root")
        seen_dungeons = set()
        for index, node in enumerate(doc.findall("Dungeon"), 1):
            value = _int(node.findtext("ID"))
            if value is not None and value not in seen_dungeons:
                seen_dungeons.add(value)
                rows.append((f"dungeon:{value}", "gameplay", "blocked", "emulator",
                             _evidence(root, dungeon_file, f"/Dungeons/Dungeon[{index}]/ID"), "", "", ""))

    seen_quests = set()
    for file in _xml_files(db / "Quests"):
        doc = ET.parse(file).getroot()
        if doc.tag != "QuestDB":
            continue
        for index, node in enumerate(doc.findall("Quest"), 1):
            value = _int(node.findtext("ID"))
            if value is not None and value not in seen_quests:
                seen_quests.add(value)
                rows.append((f"quest:{value}", "gameplay", "blocked", "emulator",
                             _evidence(root, file, f"/QuestDB/Quest[{index}]/ID"), "", "", ""))

    for ident, message, rel, needle in [
        ("system:quest-lifecycle-prototype", "Quest lifecycle replay exists in the offline prototype.", "offline/src/quest.rs", "pub fn replay("),
        ("system:content-audit-prototype", "Static content reference audit exists in the offline prototype.", "offline/src/content.rs", "pub fn audit("),
        ("system:save-persistence-prototype", "SQLite save storage exists in the offline prototype.", "offline/src/store.rs", "pub struct Store"),
        ("system:map-prototype", "Map startup packet generation exists in the offline prototype.", "offline/src/map.rs", "pub fn bootstrap("),
        ("system:progression-prototype", "Progression source-table checks exist in the offline prototype.", "offline/src/progression.rs", "pub fn validate_novice_exp_table("),
        ("system:protocol-prototype", "Login/map/world protocol code exists in the offline prototype.", "offline/src/protocol.rs", "pub const MAX_PACKET_SIZE"),
    ]:
        file = root / rel
        if file.is_file():
            line_no = next((i for i, line in enumerate(file.read_text(encoding="utf-8").splitlines(), 1) if needle in line), 0)
            if line_no:
                status, cause, evidence = "implemented-unverified", "emulator", f"{rel}:{line_no} — {message}"
            else:
                status, cause, evidence = "missing", "unknown", f"{rel} — expected source marker not present"
        else:
            status, cause, evidence = "missing", "unknown", f"{rel} — expected source not present in checkout"
        rows.append((ident, "system", status, cause, evidence, "", "", ""))
    rows.append(("scenario:protocol-flow", "protocol", "implemented-unverified", "emulator",
                 "offline/src/integration_tests.rs:53 — headless three-service protocol flow exists; bot evidence may verify only this protocol scenario.", "", "", ""))
    rows.append(("scenario:tutorial-portal", "gameplay-scenario", "blocked", "emulator",
                 "offline/tutorial-portal-inspection.txt — tutorial portal behavior has inspection notes, not connected gameplay verification.", "", "", ""))
    rows.append(("scenario:dungeon-flow", "gameplay-scenario", "blocked", "emulator",
                 _evidence(root, dungeon_file, "/Dungeons/Dungeon/SpawnFile") + " — historical dungeon source exists; runtime flow is not connected.", "", "", ""))
    return sorted(rows, key=lambda row: (row[1], row[0]))


def _bot_verified(root: Path, bot_path: Path | None) -> str | None:
    if bot_path is None or not bot_path.is_file():
        return None
    try:
        bot = json.loads(bot_path.read_text(encoding="utf-8"))
        provenance = bot["source_sha256"]
        requested = int(bot["runs_requested"])
        runs = bot["runs"]
        scenario_id = bot.get("scenario_id")
        if (bot.get("status") != "passed" or scenario_id != "scenario:protocol-flow" or requested < 3
                or not isinstance(runs, list) or len(runs) < requested
                or not isinstance(provenance, dict)
                or not all(isinstance(entry, dict) for entry in runs)):
            return None
        for run in runs[:requested]:
            if run.get("status") != "passed" or run.get("exit_code") != 0:
                return None
        offline = root / "offline"
        expected_paths = {Path("offline/Cargo.toml"), Path("offline/Cargo.lock"), Path("offline/data/20013003.map")}
        expected_paths.update(Path("offline/src") / file.name for file in (offline / "src").glob("*.rs"))
        if {Path(k) for k in provenance} != expected_paths or not all(
                isinstance(value, str) and len(value) == 64 and all(c in "0123456789abcdefABCDEF" for c in value)
                for value in provenance.values()):
            return None
        for rel, expected in provenance.items():
            relative = Path(rel)
            if relative.is_absolute() or ".." in relative.parts:
                return None
            source = root / Path(rel)
            if not source.is_file() or _sha256(source) != expected:
                return None
        canonical = json.dumps(provenance, sort_keys=True, separators=(",", ":")).encode("utf-8")
        return hashlib.sha256(canonical).hexdigest()
    except (OSError, ValueError, TypeError, KeyError, json.JSONDecodeError):
        return None


def _sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run(root: Path, output: Path, bot_path: Path | None = None) -> dict:
    root, output = Path(root), Path(output)
    bot_path = Path(bot_path) if bot_path is not None else output / "bot.json"
    generated = _catalog(root)
    prior_path = output / "scoresheet.csv"
    prior = {}
    if prior_path.is_file():
        with prior_path.open(encoding="utf-8-sig", newline="") as f:
            for row in csv.DictReader(f):
                if row.get("id"):
                    prior[row["id"]] = row

    bot_digest = _bot_verified(root, bot_path)
    verified = bot_digest is not None
    merged = []
    for row in generated:
        item = dict(zip(FIELDS, row))
        old = prior.pop(item["id"], None)
        if item["id"] == "scenario:protocol-flow" and verified:
            item.update(status="verified", cause="", last_verified_build=bot_digest, verified_by="bot")
        if old:
            user_evidence = old.get("status") == "verified" and old.get("verified_by", "").lower() != "bot"
            manual_row = bool(old.get("verified_by")) and old.get("verified_by", "").lower() != "bot"
            if user_evidence or manual_row:
                item.update({field: old.get(field, "") for field in FIELDS})
        if item["status"] not in STATUSES:
            item["status"] = "implemented-unverified"
        merged.append([item.get(field, "") for field in FIELDS])
    # Preserve manual rows, including custom IDs and user-entered verification evidence.
    for ident, old in sorted(prior.items()):
        merged.append([old.get(field, "") for field in FIELDS])
    merged.sort(key=lambda row: (row[1], row[0]))
    _csv_write(prior_path, FIELDS, merged)

    catalog_ids = {row[0] for row in generated}
    gameplay = [row for row in merged if row[0] in catalog_ids and row[1] == "gameplay"]
    systems = [row for row in merged if row[0] in catalog_ids and row[1] == "system"]
    protocol = [row for row in merged if row[0] in catalog_ids and row[1] == "protocol"]
    scenarios = [row for row in merged if row[1] == "gameplay-scenario"]
    counts = {status: sum(row[2] == status for row in gameplay) for status in sorted(STATUSES)}
    verified_gameplay = counts["verified"]
    denominator = len(gameplay)
    score = (100.0 * verified_gameplay / denominator) if denominator else 0.0
    catalog_counts = {"skills": sum(row[0].startswith("skill:") for row in generated),
                      "dungeons": sum(row[0].startswith("dungeon:") for row in generated),
                      "quests": sum(row[0].startswith("quest:") for row in generated)}
    summary = ["# ECO implementation scoresheet", "",
               f"Gameplay: {verified_gameplay}/{denominator} verified historical-catalog rows ({score:.2f}%).",
               f"Catalog: {catalog_counts['skills']} skills, {catalog_counts['dungeons']} dungeons, {catalog_counts['quests']} quests ({denominator} gameplay rows).",
               f"Protocol: {sum(row[2] == 'verified' for row in protocol)}/{len(protocol)} verified scenario rows.",
               f"Prototype systems: {sum(row[2] == 'verified' for row in systems)}/{len(systems)} verified.",
               f"Gameplay scenarios: {sum(row[2] == 'verified' for row in scenarios)}/{len(scenarios)} verified.", "",
               "The gameplay denominator is the checked-in historical catalog represented here: non-NOVICE SkillList entries, Dungeon IDs, and Quest IDs. It is not the full game.",
               "Protocol scenarios are counted separately. Static source presence is not gameplay verification.", "",
               "## Gameplay status counts", ""]
    summary.extend(f"- {status}: {counts[status]}" for status in sorted(counts))
    summary += ["", "## Scope", "",
                "Rows use source IDs and XPath or file:line evidence. Quest rows represent quest IDs only; no prerequisite or chain relationships are inferred. Manually maintained rows are retained across regeneration.",
                "The protocol scenario is marked verified only when bot.json reports the supported scenario passing at least three runs and all listed source hashes still match the checkout.", ""]
    (output / "SCORE.md").parent.mkdir(parents=True, exist_ok=True)
    (output / "SCORE.md").write_text("\n".join(summary), encoding="utf-8", newline="\n")
    return {"rows": len(merged), "gameplay_rows": denominator, "gameplay_verified": verified_gameplay,
            "gameplay_percentage": round(score, 2), "protocol_rows": len(protocol),
            "protocol_verified": sum(row[2] == "verified" for row in protocol),
            **catalog_counts,
            "system_rows": len(systems), "gameplay_scenarios": len(scenarios),
            "catalog_rows": len(generated), "bot_verified": verified, "output": str(output)}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[3])
    parser.add_argument("--output", type=Path, default=None)
    parser.add_argument("--bot", type=Path, default=None)
    args = parser.parse_args()
    print(run(args.root, args.output or args.root / "offline/docs/audit", args.bot))
