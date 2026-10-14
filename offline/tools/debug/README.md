# Eco Reborn debug tool

`python offline/tools/debug/eco_debug.py <command>` (Python 3.12, standard library only). Everything is read-only; it never starts or stops a server.

| Command | What it shows |
| --- | --- |
| `status` | Running eco / launcher / `eco-reborn` processes with their role, listening state of ports 17831-17833, firewall isolation (`Set-RebornIsolation.ps1 Status`), characters in `saves/world.sqlite` (opened read-only), the newest session report summary, and whether packet tracing is on. |
| `trace` | Merged packet timeline of the newest session in `research/local-trace/`, with opcode names, then a summary: packet counts per role, client opcodes with no server packet within 1 s, opcodes missing from `opcodes.json`, reply latency per client opcode and the largest gaps. |
| `opcodes [hex ...]` | Name lookup per role and direction. `--rebuild` regenerates `opcodes.json` from `research/SagaECO` and `research/EcoServerEmulator`. |
| `report [path]` | A session report (default newest): stages with outcome, event counts per stream, `UNKNOWN_OPCODE` opcodes with names. |

`trace` options: `--files f1 f2`, `--all`, `--since MINUTES`, `--window SEC` (files within this many seconds of the newest stamp are one session, default 120), `--role map|login|world`, `--opcode 05e6,0fa5`, `--no-hide-keepalive`, `--full`, `--summary-only`, `--follow` (live tail, Ctrl+C to stop), `--html out.html`.

Old trace files (no role in the name, milliseconds since process start) are read too; their packets show names from every role (`Logout|UserLogin`). Names ending in `?` come from the map table as a fallback for another role.

## Packet traces are private

Trace files contain raw plaintext packets, including login names and password hashes. They stay on this PC: never commit, share or upload them, or HTML files produced by `trace --html`. Delete old traces when you are done.

## Enable tracing

Create the empty file `offline/research/local-trace/ENABLED`, then start the server. Each server process writes `trace-<unixsec>-<pid>-<role>.log`. Remove the file to turn tracing off.

## Tests

`python -m unittest discover -s offline/tools/debug -p "test_*.py" -v`
