"""Run ECO's supported headless protocol flow in an isolated copied source tree."""
import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import time
import uuid
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
TEST_SOURCE = Path("src/integration_tests.rs")
LIMITATIONS = [
    "Combat, quests, tutorial, and dungeon flows have no implemented network path and are blocked.",
    "Gate/login/map advertised address bytes are asserted, but the test directly connects to ephemeral listeners; redirect-driven client routing is not exercised.",
]


def adapt_test_source(source):
    """Use ephemeral listener addresses in the isolated test copy; preserve advertised bytes."""
    replacements = [
        ("let gate_listener = bind_loopback(17831);", "let gate_listener = bind_loopback(0);\n    let gate_addr = gate_listener.local_addr().unwrap();"),
        ("let login_listener = bind_loopback(17832);", "let login_listener = bind_loopback(0);\n    let login_addr = login_listener.local_addr().unwrap();"),
        ("let map_listener = bind_loopback(17833);", "let map_listener = bind_loopback(0);\n    let map_addr = map_listener.local_addr().unwrap();"),
        ('Client::connect("127.0.0.1:17831".parse().unwrap())', "Client::connect(gate_addr)"),
        ('Client::connect("127.0.0.1:17832".parse().unwrap())', "Client::connect(login_addr)"),
        ('Client::connect("127.0.0.1:17833".parse().unwrap())', "Client::connect(map_addr)"),
    ]
    for old, new in replacements:
        count = source.count(old)
        expected = 2 if old.startswith('Client::connect("127.0.0.1:17833') else 1
        if count != expected:
            raise ValueError(f"expected {expected} occurrences of test adaptation anchor; found {count}")
        source = source.replace(old, new)
    return source


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def copy_project(destination):
    destination.mkdir(parents=True)
    source_files = [ROOT / "Cargo.toml", ROOT / "Cargo.lock"]
    source_files.extend(sorted((ROOT / "src").glob("*.rs")))
    source_files.append(ROOT / "data/20013003.map")
    source_hashes = {path.relative_to(ROOT.parent).as_posix(): sha256(path) for path in source_files}
    for original_file in source_files:
        copied_file = destination / original_file.relative_to(ROOT)
        copied_file.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(original_file, copied_file)
    copied_raw_hashes = {name: sha256(destination / Path(name).relative_to("offline")) for name in source_hashes}
    changed_copy = [name for name, digest in source_hashes.items() if copied_raw_hashes[name] != digest]
    if changed_copy:
        raise ValueError("copied source changed during snapshot: " + ", ".join(changed_copy))
    original = ROOT / TEST_SOURCE
    copied = destination / TEST_SOURCE
    original_hash = sha256(original)
    adapted = adapt_test_source(copied.read_text(encoding="utf-8"))
    copied.write_text(adapted, encoding="utf-8", newline="\n")
    changed = [name for name, digest in source_hashes.items() if sha256(ROOT.parent / name) != digest]
    if changed:
        raise ValueError("source changed while snapshotting: " + ", ".join(changed))
    return {"source_sha256": source_hashes, "integration_test_sha256": original_hash,
            "adapted_test_sha256": sha256(copied),
            "adaptations": ["copied integration test binds three 127.0.0.1:0 listeners and directly connects to their assigned addresses"]}


def safe_output(stdout, stderr):
    kept = []
    for line in (stdout + "\n" + stderr).splitlines():
        if line.startswith("test integration_tests::headless_three_service_flow ") or re.search(r"test result: .*\b[0-9]+ passed; [0-9]+ failed", line):
            kept.append(line[-500:])
    return kept[-3:]


def kill_owned_tree(process):
    if os.name == "nt":
        result = subprocess.run(["taskkill", "/PID", str(process.pid), "/T", "/F"], capture_output=True, timeout=10)
        if result.returncode and process.poll() is None:
            raise RuntimeError("owned process tree termination failed")
    else:
        process.kill()
    try:
        return process.communicate(timeout=5)
    except subprocess.TimeoutExpired:
        if os.name == "nt":
            result = subprocess.run(["taskkill", "/PID", str(process.pid), "/T", "/F"], capture_output=True, timeout=10)
            if result.returncode and process.poll() is None:
                raise RuntimeError("owned process tree termination failed")
        else:
            process.kill()
        return process.communicate(timeout=5)


def classify_run(return_code, output):
    test_ok = "test integration_tests::headless_three_service_flow ... ok" in output
    passed = test_ok and any(re.search(r"test result: .*\b1 passed; 0 failed", line) for line in output)
    if return_code in (101, 102):
        return "gate_or_build_failure"
    return "passed" if return_code == 0 and passed else "failed"


def run(argv=None, *, gate_path=None):
    # Optional machine resource gate (rescheck-style "run --kind heavy -- <command>" wrapper).
    if gate_path is None and os.environ.get("ECO_RESOURCE_GATE"):
        gate_path = Path(os.environ["ECO_RESOURCE_GATE"])
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, help="write machine-readable JSON report")
    parser.add_argument("--runs", type=int, default=3, help="consecutive successful runs required (1-10)")
    parser.add_argument("--timeout", type=int, default=300, help="per-run timeout in seconds")
    args = parser.parse_args(argv)
    output = Path(args.output).expanduser().resolve()
    report = {"runner": "eco-headless-bot", "scenario_id": "scenario:protocol-flow",
              "status": "failed", "runs_requested": args.runs, "source_sha256": {},
              "runs": [], "coverage": {"supported": ["encrypted handshake", "login", "character create/select", "map redirect bytes", "map enter", "movement", "persistence"],
              "blocked": LIMITATIONS}, "build": {}}
    if not 1 <= args.runs <= 10 or args.timeout < 1:
        report["error"] = "runs must be 1..10 and timeout must be positive"
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        return 2
    if gate_path is not None and not gate_path.is_file():
        report["status"] = "unavailable"
        report["error"] = "resource gate not found"
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        return 2

    work_parent = Path(__file__).resolve().parent / ".work"
    temp = work_parent / f"eco-bot-{uuid.uuid4().hex}"
    try:
        work_parent.mkdir(exist_ok=True)
        temp.mkdir()
    except OSError as error:
        report["status"] = "unavailable"
        report["error"] = "isolated workspace could not be created: " + type(error).__name__
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        return 2
    project = temp / "offline"
    try:
        report["build"]["provenance"] = copy_project(project)
        report["source_sha256"] = report["build"]["provenance"]["source_sha256"]
    except (OSError, ValueError) as error:
        report["error"] = "isolated source preparation failed: " + type(error).__name__
        exit_code = 2
    else:
        target = temp / "cargo-target"
        report["build"]["command"] = ["cargo", "test", "--offline", "--locked", "-j", "4", "integration_tests::headless_three_service_flow", "--", "--exact"]
        exit_code = 0
        for index in range(1, args.runs + 1):
            environment = os.environ.copy()
            environment["CARGO_TARGET_DIR"] = str(target)
            command = ["cargo", "test", "--offline", "--locked", "-j", "4", "integration_tests::headless_three_service_flow", "--", "--exact"]
            started = time.monotonic()
            process = None
            try:
                gated = [sys.executable, str(gate_path), "run", "--kind", "heavy", "--wait", "600", "--"] if gate_path else []
                process = subprocess.Popen([*gated, *command],
                                           cwd=project, env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                stdout, stderr = process.communicate(timeout=args.timeout)
                summary = safe_output(stdout, stderr)
                state = classify_run(process.returncode, summary)
                row = {"run": index, "status": state, "exit_code": process.returncode,
                       "duration_seconds": round(time.monotonic() - started, 3), "evidence": summary}
                if process.returncode in (101, 102):
                    row["failure_class"] = "resource_gate_or_build_failure"
            except subprocess.TimeoutExpired as error:
                if process is not None:
                    try:
                        stdout, stderr = kill_owned_tree(process)
                    except (OSError, subprocess.TimeoutExpired, RuntimeError):
                        stdout, stderr = "", ""
                        row_cleanup_failed = True
                    else:
                        row_cleanup_failed = False
                else:
                    stdout, stderr = "", ""
                    row_cleanup_failed = False
                row = {"run": index, "status": "timeout", "duration_seconds": round(time.monotonic() - started, 3),
                       "evidence": safe_output(stdout, stderr)}
                if row_cleanup_failed:
                    row["cleanup"] = "owned process termination failed; verify no runner child remains"
            except OSError as error:
                row = {"run": index, "status": "unavailable", "error": type(error).__name__}
            report["runs"].append(row)
            if row["status"] != "passed":
                exit_code = row.get("exit_code", 2) or 1
                break
        report["status"] = "passed" if len(report["runs"]) == args.runs and all(r["status"] == "passed" for r in report["runs"]) else "failed"
        report["build"].update({"cargo_lock_sha256": sha256(project / "Cargo.lock"), "target_dir_isolated": True,
                                "isolated_work_dir": str(temp)})

    try:
        shutil.rmtree(temp)
        report["build"].pop("isolated_work_dir", None)
    except OSError:
        report["build"]["cleanup"] = "isolated build folder retained"

    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    return exit_code


if __name__ == "__main__":
    raise SystemExit(run())
