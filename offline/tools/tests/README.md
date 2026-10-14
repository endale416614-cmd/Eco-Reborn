# ECO test tools

Audit and coverage tools for the server. They read the session reports, the SagaECO reference data and the Rust
server, and write a score sheet of what is implemented and tested. Python 3.10+, standard library only.

## Run

From the repository root:

```powershell
python offline/tools/tests/run.py
```

Or double-click `Run-Test-Tools.cmd`. The default run sorts session reports, audits content references, checks
literal script destinations and updates the score sheet. It expects SagaECO in `research/SagaECO` (see the main
README).

To also run the headless network scenario three times:

```powershell
python offline/tools/tests/run.py --bot --runs 3
```

This builds and runs a Rust integration test (login, character creation and selection, map entry, movement, saved
position) in an isolated copy with a temporary save. To run it through a machine resource gate, set
`ECO_RESOURCE_GATE` to a script that takes `run --kind heavy --wait 600 -- <command>`.

## Results

Written to `offline/docs/audit/` (not tracked):

- `AUDIT.md`, `audit.csv`: malformed records and missing monster references.
- `CHECKER.md`, `checker.csv`: static source mismatches and portal review leads.
- `SCORE.md`, `scoresheet.csv`: one row per job/skill, dungeon, quest, system and scenario.
- `triage.json`: report signatures and evidence line numbers; unknown reports first.
- `summary.json`: counts.
- `bot.json`: network test results, when `--bot` was used.

The score sheet's denominator is SagaECO's historical catalog, not an exact list of this client's content. Loading a
record never marks it as played. Rows marked `verified_by=user` survive regeneration.

## What each tool can tell you

| Tool | Shows | Doesn't show |
| --- | --- | --- |
| Report sorter | launcher stages, error categories, protocol events, opcodes | the root cause of a disconnect on its own |
| Audit and score sheet | catalog counts, duplicate/malformed IDs, spawn and hunt-target references | whether a mechanic actually works |
| Script checker | missing monster references, `Warp` destinations missing from the map list | reachability, quest chains, dynamic destinations |
| Headless bot | login, character create/select, map entry, movement, saved position | rendered client behaviour, combat, NPCs |

## Individual checks

```powershell
python offline/tools/tests/triage.py --json
python offline/tools/tests/triage.py path/to/report.txt path/to/diagnostics.zip
python offline/tools/tests/audit.py
python offline/tools/tests/checker.py
python offline/tools/tests/scoresheet.py
python -m unittest discover -s offline/tools/tests -p "test_*.py" -v
```

ZIP reports are read in memory with size limits and never extracted. Triage output leaves out filenames, account
names, paths and free log text.
