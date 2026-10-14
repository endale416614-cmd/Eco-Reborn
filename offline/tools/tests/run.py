"""Run the offline static audit suite; the optional bot uses its isolated gated runner."""
import argparse
import csv
import hashlib
import importlib
import json
import shutil
import sys
import uuid
from pathlib import Path

TOOLS = Path(__file__).resolve().parent
ROOT = TOOLS.parents[2]
DEFAULT_OUTPUT = ROOT / "offline/docs/audit"
BEGIN = "<!-- ECO-RUN-SUMMARY:BEGIN -->"
END = "<!-- ECO-RUN-SUMMARY:END -->"
sys.path.insert(0, str(TOOLS))

import audit
import checker
import triage


class SourceChangedError(RuntimeError):
    pass


def source_files(root):
    """Files consumed by the audit, checker, triage, and optional isolated bot."""
    root = Path(root)
    files = []
    for folder, suffixes in ((root / "offline/src", {".rs"}),
                             (root / "offline/reports", {".txt", ".zip"}),
                             (root / "research/SagaECO/SagaScript", {".cs"})):
        if folder.is_dir():
            files.extend(p for p in folder.rglob("*") if p.is_file() and (suffixes is None or p.suffix.lower() in suffixes))
    tools = root / "offline/tools/tests"
    files.extend(p for p in tools.glob("*.py") if p.is_file())
    for folder in (root / "research/SagaECO/Configuration/DB/Spawns", root / "research/SagaECO/Configuration/DB/Quests"):
        if folder.is_dir():
            files.extend(p for p in folder.rglob("*.xml") if p.is_file())
    db = root / "research/SagaECO/Configuration/DB"
    files.extend(p for p in (db / "monster.csv", db / "warp.csv") if p.is_file())
    files.extend(p for p in (db / "SkillList.xml", db / "Dungeon/Dungeons.xml") if p.is_file())
    files.extend(p for p in (root / "offline/Cargo.toml", root / "offline/Cargo.lock", root / "offline/data/20013003.map") if p.is_file())
    signatures = root / "offline/tools/tests/signatures.json"
    if signatures.is_file():
        files.append(signatures)
    return sorted(set(files), key=lambda p: p.relative_to(root).as_posix().casefold())


def snapshot_hash(root):
    root = Path(root)
    digest = hashlib.sha256()
    for path in source_files(root):
        digest.update(path.relative_to(root).as_posix().encode("utf-8"))
        digest.update(b"\0")
        with path.open("rb") as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(chunk)
        digest.update(b"\0")
    return digest.hexdigest()


def checker_rows(path):
    with Path(path).open(encoding="utf-8-sig", newline="") as stream:
        return list(csv.DictReader(stream))


def finding_lists(rows, limit=10):
    proven, leads = [], []
    for row in rows:
        item = {key: row.get(key, "") for key in ("source", "record_or_line", "identifier", "message", "evidence")}
        if row.get("classification") in {"proven_fault", "source_data_mismatch"} or row.get("cause") == "original-data":
            proven.append(item)
        elif row.get("classification") == "unresolved_reference" or row.get("level") == "review":
            leads.append(item)
    return proven[:limit], leads[:limit]


def audit_summary_block(checker_summary, proven, leads, comparison):
    lines = [BEGIN, "", "## Consolidated static-tool run", "",
             f"- Checker counts: {checker_summary.get('source_data_mismatch', 0)} proven source-data mismatches; "
             f"{checker_summary.get('unresolved_reference', 0)} uncertain reference leads.",
             "- " + comparison_text(comparison),
             "- Reports: [SCORE](SCORE.md) · [TRIAGE](triage.json) · [CHECKER](CHECKER.md) · [checker evidence](checker.csv)",
             "", "### Top proven source-data findings", ""]
    lines.extend(f"- `{r['source']}` {r['record_or_line']}: {r['message']}" for r in proven)
    if not proven:
        lines.append("- None.")
    lines += ["", "### Uncertain review leads", ""]
    lines.extend(f"- `{r['source']}` {r['record_or_line']}: {r['message']}" for r in leads)
    if not leads:
        lines.append("- None.")
    lines += ["", END]
    return "\n".join(lines)


def update_audit_block(path, block):
    path = Path(path)
    text = path.read_text(encoding="utf-8")
    path.write_text(replace_audit_block(text, block), encoding="utf-8", newline="\n")


def replace_audit_block(text, block):
    start = text.find(BEGIN)
    stop = text.find(END, start + len(BEGIN)) if start >= 0 else -1
    if start >= 0 and stop >= 0:
        text = text[:start].rstrip() + "\n\n" + block + text[stop + len(END):]
    else:
        text = text.rstrip() + "\n\n" + block + "\n"
    return text


def clean_result(value):
    if isinstance(value, dict):
        return {k: clean_result(v) for k, v in value.items() if k not in {"output", "output_path"}}
    if isinstance(value, list):
        return [clean_result(v) for v in value]
    return value


def next_comparison(previous, current_hash, metrics=None):
    if previous and previous.get("source_snapshot_sha256") != current_hash:
        old = previous.get("metrics", {})
        current = metrics or {}
        deltas = {key: current[key] - old[key] for key in sorted(current.keys() & old.keys())
                  if isinstance(current[key], (int, float)) and isinstance(old[key], (int, float))}
        return {"changed": True, "from": previous.get("source_snapshot_sha256"), "to": current_hash, "deltas": deltas}
    if previous:
        return previous.get("comparison", {"changed": False, "from": None, "to": current_hash, "deltas": {}})
    return {"changed": False, "from": None, "to": current_hash, "deltas": {}}


def exit_code(triage_errors, bot_enabled, bot_code):
    if bot_enabled and bot_code:
        return bot_code
    return 1 if triage_errors else 0


def numeric_metrics(*groups):
    metrics = {}
    def visit(prefix, value):
        if isinstance(value, dict):
            for key, child in value.items():
                if key not in {"output", "output_path"}:
                    visit(f"{prefix}.{key}" if prefix else key, child)
        elif isinstance(value, (int, float)) and not isinstance(value, bool):
            metrics[prefix] = value
    for name, group in groups:
        visit(name, clean_result(group))
    return dict(sorted(metrics.items()))


def comparison_text(comparison):
    deltas = comparison.get("deltas", {})
    if not comparison.get("changed"):
        return "No previous source snapshot is available for a numeric comparison." if not deltas else "No source changes since the last scan; showing the last changed-scan comparison."
    if not deltas:
        return "Source changed since the previous scan; no comparable numeric counts were available."
    return "Changes since the previous source snapshot: " + "; ".join(f"{key} {value:+g}" for key, value in deltas.items()) + "."


def run_steps(root, stage, modules, bot_enabled=False, runs=3, prior_bot=None, prior_scorecard=None):
    triage_result = modules["triage"].triage([Path(root) / "offline/reports"])
    audit_result = modules["audit"].run(root, stage)
    checker_result = modules["checker"].run(root, stage)
    bot_code = modules["bot"].run(["--output", str(Path(stage) / "bot.json"), "--runs", str(runs)]) if bot_enabled else 0
    if not bot_enabled and prior_bot and Path(prior_bot).is_file():
        shutil.copyfile(prior_bot, Path(stage) / "bot.json")
    if prior_scorecard and Path(prior_scorecard).is_file():
        shutil.copyfile(prior_scorecard, Path(stage) / "scoresheet.csv")
    score_result = modules["scoresheet"].run(root, stage, bot_path=Path(stage) / "bot.json")
    return triage_result, audit_result, checker_result, score_result, bot_code


def build_summary(snapshot, comparison, audit_result, checker_result, score_result, triage_result, proven, leads, bot_status):
    return {
        "source_snapshot_sha256": snapshot,
        "comparison": comparison,
        "audit": clean_result(audit_result),
        "checker": clean_result(checker_result),
        "scoresheet": clean_result(score_result),
        "triage": {"signature_counts": triage_result.get("signature_counts", {}), "errors": len(triage_result.get("errors", []))},
        "proven_findings": proven,
        "uncertain_leads": leads,
        "bot": bot_status,
    }


def load_previous(path):
    try:
        return json.loads(Path(path).read_text(encoding="utf-8"))
    except (OSError, ValueError, UnicodeError):
        return None


def run(root=ROOT, output=DEFAULT_OUTPUT, *, bot_enabled=False, runs=3, modules=None, snapshot=snapshot_hash):
    root, output = Path(root).resolve(), Path(output).resolve()
    bot_code = 0
    try:
        modules = modules or {"audit": audit, "checker": checker, "triage": triage,
                              "scoresheet": importlib.import_module("scoresheet"),
                              "bot": importlib.import_module("bot")}
        output.mkdir(parents=True, exist_ok=True)
        previous = load_previous(output / "summary.json")
        before = snapshot(root)
        stage = output / (".eco-run-" + uuid.uuid4().hex)
        stage.mkdir()
        try:
            triage_result, audit_result, checker_result, score_result, bot_code = run_steps(
                root, stage, modules, bot_enabled, runs, output / "bot.json", output / "scoresheet.csv")
            (stage / "triage.json").write_text(json.dumps(triage_result, ensure_ascii=False, sort_keys=True, indent=2) + "\n", encoding="utf-8")
            rows = checker_rows(stage / "checker.csv")
            proven, leads = finding_lists(rows)
            after = snapshot(root)
            if before != after:
                raise SourceChangedError()
            metrics = numeric_metrics(("audit", audit_result), ("checker", checker_result), ("score", score_result))
            comparison = next_comparison(previous, after, metrics)
            update_audit_block(stage / "AUDIT.md", audit_summary_block(checker_result, proven, leads, comparison))
            bot_status = json.loads((stage / "bot.json").read_text(encoding="utf-8")).get("status") if (stage / "bot.json").is_file() else "not_run"
            summary = build_summary(after, comparison, audit_result, checker_result, score_result, triage_result, proven, leads, bot_status)
            summary["metrics"] = metrics
            (stage / "summary.json").write_text(json.dumps(summary, ensure_ascii=False, sort_keys=True, indent=2) + "\n", encoding="utf-8")
            for artifact in stage.iterdir():
                if artifact.is_file():
                    shutil.copyfile(artifact, output / artifact.name)
        finally:
            if stage.parent.resolve() == output.resolve() and stage.name.startswith(".eco-run-"):
                shutil.rmtree(stage, ignore_errors=True)
    except Exception as error:
        if isinstance(error, SourceChangedError):
            print("ECO report discarded: source files changed during the scan. Rerun after edits finish.", file=sys.stderr)
            return 2
        print("ECO tools could not complete a stable report run (" + type(error).__name__ + ").", file=sys.stderr)
        return 1
    code = exit_code(triage_result.get("errors"), bot_enabled, bot_code)
    proven_count = checker_result.get("source_data_mismatch", 0)
    lead_count = checker_result.get("unresolved_reference", 0)
    try:
        location = output.relative_to(root).as_posix()
    except ValueError:
        location = output.name
    if bot_enabled and bot_code:
        print(f"ECO static reports complete: {proven_count} source-data findings, {lead_count} review leads; bot failed or timed out. Reports: {location}")
    else:
        print(f"ECO tools complete: {proven_count} source-data findings, {lead_count} review leads. Reports: {location}")
    return code


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--bot", action="store_true", help="run the isolated, gated headless bot")
    parser.add_argument("--runs", type=int, default=3, help="successful bot repetitions to require (1-10)")
    args = parser.parse_args(argv)
    if not 1 <= args.runs <= 10:
        parser.error("--runs must be between 1 and 10")
    return run(ROOT, args.output, bot_enabled=args.bot, runs=args.runs)


if __name__ == "__main__":
    raise SystemExit(main())
