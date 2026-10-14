"""Privacy-preserving sorter for Eco Reborn reports."""
import argparse
import json
import hashlib
import re
import sys
import zipfile
import zlib
from pathlib import Path

MAX_REPORT = 2 * 1024 * 1024
MAX_ARCHIVE_TOTAL = 16 * 1024 * 1024
MAX_ARCHIVE_REPORTS = 64
MAX_ARCHIVE_BYTES = 32 * 1024 * 1024
STAGES = {"wrapper_start", "preflight", "isolation", "map_start", "map_ready", "login_start", "login_ready", "world_start", "world_ready", "client_start", "client_exit", "cleanup"}
OUTCOMES = {"ok", "failed", "refused", "unavailable", "stopped"}
CATEGORIES = {"preflight_refused", "isolation_unavailable", "server_start_failed", "server_not_ready", "client_start_failed", "server_stopped", "cleanup_failed", "other_failure"}
EVENTS = set("LISTENING CLIENT_CONNECTED HANDSHAKE_COMPLETE LOGIN_ACCEPTED LOGIN_REJECTED CHARACTER_LIST_SENT CHARACTER_CREATED CHARACTER_CREATE_REJECTED MAP_ID_SENT LOGIN_STATUS_SENT MAP_UNAVAILABLE SERVER_LIST_SENT MAP_ENTERED MAP_LOADED PLAYER_MOVED MAP_REDIRECT_SENT CONNECTION_CLOSED PANIC".split())
ERRORS = set("IO INVALID_DATA PERMISSION_DENIED NOT_FOUND ADDRESS_IN_USE TIMEOUT CONNECTION_RESET BROKEN_PIPE ALREADY_EXISTS".split())
STAGE_RE = re.compile(r"^\+\d{6,}s stage=([a-z_]+) outcome=([a-z]+)(?: category=([a-z_]+))?$")
EVENT_RE = re.compile(r"^protocol event=([A-Z_]+)(?: kind=([A-Z_]+)| opcode=(0x[0-9a-f]{4}))?$")


def lines_from_bytes(data):
    if len(data) > MAX_REPORT:
        raise ValueError("report exceeds size limit")
    return data.decode("utf-8-sig", errors="strict").splitlines()


def parse(lines, ref, source_id=""):
    stages, events, streams = [], [], set()
    for number, line in enumerate(lines, 1):
        if line.startswith("protocol stream="):
            stream = line.removeprefix("protocol stream=")
            if stream in {"map", "login", "world"}:
                streams.add(stream)
            continue
        m = STAGE_RE.fullmatch(line)
        if m and m[1] in STAGES and m[2] in OUTCOMES and (not m[3] or (m[2] == "failed" and m[3] in CATEGORIES)):
            stages.append({"stage": m[1], "outcome": m[2], "line": number, **({"category": m[3]} if m[3] else {})})
            continue
        m = EVENT_RE.fullmatch(line)
        if not m:
            continue
        event, kind, opcode = m.groups()
        if event in EVENTS and not kind and not opcode:
            events.append({"event": event, "line": number})
        elif event in {"CONNECTION_ERROR", "STARTUP_FAILED", "STORAGE_ERROR", "ACCEPT_ERROR"} and kind in ERRORS and not opcode:
            events.append({"event": event, "kind": kind, "line": number})
        elif event == "UNKNOWN_OPCODE" and opcode and not kind:
            events.append({"event": event, "opcode": opcode, "line": number})
    return {"ref": ref, "source_id": source_id, "stages": stages, "events": events, "streams": sorted(streams)}


def inputs(paths):
    found = []
    for raw in paths:
        path = Path(raw)
        if path.is_dir():
            found.extend(sorted(p for p in path.rglob("*") if p.is_file() and p.suffix.lower() in {".txt", ".zip"}))
        else:
            found.append(path)
    return found


def read_zip(source):
    if isinstance(source, Path):
        archive_size = source.stat().st_size
    else:
        archive_size = len(source.getbuffer())
        source.seek(0)
    if archive_size > MAX_ARCHIVE_BYTES:
        raise ValueError("archive exceeds size limit")
    reports = []
    total = 0
    with zipfile.ZipFile(source) as archive:
        for info in sorted(archive.infolist(), key=lambda i: i.filename):
            if info.is_dir() or Path(info.filename).suffix.lower() != ".txt":
                continue
            if len(reports) >= MAX_ARCHIVE_REPORTS or info.file_size > MAX_REPORT or total + info.file_size > MAX_ARCHIVE_TOTAL:
                raise ValueError("archive exceeds size limits")
            with archive.open(info) as stream:
                data = stream.read(MAX_REPORT + 1)
            if len(data) > MAX_REPORT:
                raise ValueError("report exceeds size limit")
            total += len(data)
            if total > MAX_ARCHIVE_TOTAL:
                raise ValueError("archive exceeds size limits")
            reports.append((lines_from_bytes(data), hashlib.sha256(data).hexdigest()))
    return reports


def read_source(path):
    if path.suffix.lower() == ".zip":
        return read_zip(path)
    if path.suffix.lower() != ".txt":
        raise ValueError("expected .txt or .zip input")
    with path.open("rb") as stream:
        data = stream.read(MAX_REPORT + 1)
    if len(data) > MAX_REPORT:
        raise ValueError("report exceeds size limit")
    return [(lines_from_bytes(data), hashlib.sha256(data).hexdigest())]


def triage(paths):
    catalog_path = Path(__file__).with_name("signatures.json")
    catalog = json.loads(catalog_path.read_text(encoding="utf-8"))
    reports, errors = [], []
    index = 0
    for path in inputs(paths):
        try:
            sources = read_source(path)
            for source, source_id in sources:
                index += 1
                reports.append(parse(source, f"report-{index:03d}", source_id))
        except (OSError, ValueError, UnicodeError, zipfile.BadZipFile, RuntimeError, zlib.error):
            errors.append({"ref": f"input-{len(errors) + 1:03d}", "error": "unreadable or malformed input"})
    out = []
    summary = {}
    for report in reports:
        evidence = {}
        for s in report["stages"]:
            if s.get("category"):
                evidence.setdefault(f"stage category={s['category']}", []).append(s["line"])
        for e in report["events"]:
            key = f"protocol event={e['event']}" + (f" kind={e['kind']}" if e.get("kind") else "") + (f" opcode={e['opcode']}" if e.get("opcode") else "")
            evidence.setdefault(key, []).append(e["line"])
        matches = []
        for item in catalog[:-1]:
            key = item["when"]
            if key.startswith("protocol event=") and key.endswith("*"):
                matched = {k: v for k, v in evidence.items() if k.startswith(key[:-1])}
                if item["id"] == "service_connection_error":
                    matched = {k: v for k, v in matched.items() if "kind=CONNECTION_RESET" not in k}
            else:
                matched = {key: evidence[key]} if key in evidence else {}
            if matched:
                matches.append({"id": item["id"], "explanation": item["explanation"],
                                "evidence": [{"line": n, "field": k} for k, ns in sorted(matched.items()) for n in ns]})
        if not matches:
            matches = [{"id": catalog[-1]["id"], "explanation": catalog[-1]["explanation"], "evidence": []}]
        for match in matches:
            summary[match["id"]] = summary.get(match["id"], 0) + 1
        out.append({"ref": report["ref"], "source_id": report["source_id"], "matches": matches,
                    "stages": report["stages"], "streams": report["streams"],
                    "event_counts": dict(sorted({e["event"]: sum(x["event"] == e["event"] for x in report["events"]) for e in report["events"]}.items()))})
    out.sort(key=lambda r: (r["matches"][0]["id"] != "unknown", r["ref"]))
    return {"reports": out, "signature_counts": dict(sorted(summary.items())), "errors": errors}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("paths", nargs="*", help="report files, directories, or zip archives")
    parser.add_argument("--json", action="store_true", help="emit machine-readable JSON")
    args = parser.parse_args(argv)
    result = triage(args.paths or [Path(__file__).parents[2] / "reports"])
    if args.json:
        print(json.dumps(result, sort_keys=True, indent=2))
    else:
        print("signature counts: " + (", ".join(f"{key}={value}" for key, value in sorted(result["signature_counts"].items())) or "none"))
        for row in result["reports"]:
            print(f"{row['ref']} ({row['source_id']}): " + "; ".join(f"{m['id']} — {m['explanation']}" for m in row["matches"]))
            for match in row["matches"]:
                if match["evidence"]:
                    print("  " + match["id"] + " evidence: " + ", ".join(f"line {e['line']} {e['field']}" for e in match["evidence"]))
            if row["stages"]:
                print("  stages: " + ", ".join(f"{s['stage']}={s['outcome']}" for s in row["stages"]))
        for error in result["errors"]:
            print(f"{error['ref']}: {error['error']}", file=sys.stderr)
    return bool(result["errors"])


if __name__ == "__main__":
    raise SystemExit(main())
