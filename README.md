# Eco Reborn

An offline, single-player edition of Emil Chronicle Online (ECO) for Windows. A local server written in Rust runs next
to the final Japanese game client, so the game can be played again after the official service closed on 31 August
2017. Everything runs on your own PC: the servers listen on `127.0.0.1` only and the client is firewalled off from the
internet.

> **Early development.** You can log in, create characters, enter Up Town and walk around, and your position and
> characters are saved. NPCs, shops, monsters, combat and quests are not in yet. See [Roadmap](#roadmap).

## Game client

Eco Reborn needs the last official Japanese client (2017-08-31, `eco.exe` version 1.239.0.1). This project does not
include it.

### [Download the Japanese ECO client (eco_jp.zip, Google Drive)](https://drive.google.com/file/d/18NU7MRoc79DAIjFVyVUb_q6cjzYEtdbz/view?usp=sharing)

This is the client archive shared by the [EcoServerEmulator](https://github.com/cm-MMK-2/EcoServerEmulator) project.
Extract it so the folder is called `eco_jp` and sits next to the `offline` folder of this repository.

Other clients are not supported. The Nekogame community client (v935) has the same game build, but its `eco.exe` is
modified to expect a different start switch and its tables are already partly translated, so use the archive above.

## Setup

There is no packaged release yet, so for now you build it yourself.

Requirements: Windows 10/11 x64, [Rust](https://rustup.rs/) (stable), Windows PowerShell 5.1 and the .NET Framework 4
C# compiler (both come with Windows), and Python 3.10+ for the English patch and the tools.

1. `git clone https://github.com/endale416614-cmd/Eco-Reborn.git` and put the extracted `eco_jp` folder inside it.
2. Point the client at the local server: replace the contents of `eco_jp/server.lst` with
   `Eco Reborn,127.0.0.1:17831,` (one line; keep a copy of the original).
3. Build the server: `cargo build --release --manifest-path offline/Cargo.toml`
4. Build the launcher: `powershell -NoProfile -File offline/launcher/build.ps1`. This creates `Eco Reborn.exe` in
   the repository root.
5. Run `Repair Isolation.cmd` once and approve the Windows prompt. It adds the firewall rules that keep the client and
   server off the internet. The launcher will not start without them. Run it again if you move the folder.
6. Start the game with `Eco Reborn.exe`.

The launcher starts the three local servers, starts the game, and shuts everything down when you quit. Your save is
`offline/saves/world.sqlite`. Each session writes a plain-text report to `offline/reports/`; attach the newest one
when you report a bug.

### English text

The client is Japanese. `offline/translations/en/` holds about 6,000 reviewed English interface strings and item
names. With the game closed, run:

```powershell
powershell -NoProfile -File offline/tools/Install-English.ps1
```

It checks that the client tables are the unmodified originals, builds the English tables and installs them. The
originals are copied to `offline/backups/english-<time>/` first; copy them back to undo. Long item descriptions and NPC
dialogue are still Japanese.

## Vision

Make ECO fully playable again, in English, with your characters and progress kept on your own PC. In order of
priority:

1. Playable: NPCs, shops, monsters, quests, skills and jobs, then dungeons and the rest of the world.
2. English: the whole game readable in English, interface first, then items, quests and NPC dialogue.
3. Quality of life: solo-friendly rates, a free cash shop, small fixes shaped by player feedback.
4. Co-op over your home network, not an MMORPG: your character is a save on your PC that can visit a friend's game
   and come back with what it earned.
5. Modding: stable data formats that players can change and extend.

Own your game. Own your character. Keep your progress. The full plan is in [VISION.md](VISION.md).

## What works now

- One-click launcher with firewall isolation checks, session reports and clean shutdown.
- Login, character list, character creation (race, gender, appearance, starter clothes).
- Up Town and the starting map: map entry, walking (blocked tiles are refused), position saved on logout.
- Base stats, HP/MP/SP and the stat preview from SagaECO's formulas; level, job and EXP display.
- Chat test commands: `/help`, `/unstuck`, `/godmode`, `/killall`, `/freebies`.
- About 6,000 English interface strings and item names.

## Roadmap

The work is split into phases in [offline/docs/RESTORATION-PLAN.md](offline/docs/RESTORATION-PLAN.md):

| Phase | Content |
|---|---|
| 0 | Foundations: inventory and items, NPC talk, portals and map changes, character stats |
| 1 | NPC scripts, NPC shops, the cash shop (every item free) |
| 2 | Monsters, combat, EXP and drops |
| 3 | Quests |
| 4 | Jobs and skills |
| 5 | Dungeons, crafting, pets, flying garden and the rest of the world |

## Repository layout

| Path | What |
|---|---|
| `offline/src/` | the server (Rust): world, login and map services, save store, autotest runner |
| `offline/launcher/` | the one-click launcher (C#, WinForms) |
| `offline/Start-EcoReborn.ps1` | service start-up, isolation checks, reports and cleanup used by the launcher |
| `offline/Set-RebornIsolation.ps1` | firewall isolation rules (`Repair Isolation.cmd` runs it) |
| `offline/data/` | map data from SagaECO (GPL-3.0, see `offline/data/NOTICE.txt`) |
| `offline/translations/` | English translations and their review notes |
| `offline/tests/` | autotest scenarios and PowerShell tests |
| `offline/tools/` | English patch builder, text export, debug and audit tools |

## Testing

- `cargo test --release --manifest-path offline/Cargo.toml` runs the Rust tests. Some of them read SagaECO's data:
  clone [tarathep/SagaECO](https://github.com/tarathep/SagaECO) at commit `ab51ddc` into `research/SagaECO` to run
  them.
- `Run Autotests.cmd` plays scripted sessions (`offline/tests/scenarios/*.eco`) against throwaway servers and saves.
  See [offline/tests/scenarios/README.md](offline/tests/scenarios/README.md).
- `python offline/tools/debug/eco_debug.py status` shows the running services, isolation state and the newest report.

## Credits

- Protocol framing, handshake and encryption are adapted from
  [cm-MMK-2/EcoServerEmulator](https://github.com/cm-MMK-2/EcoServerEmulator) (Apache-2.0).
- Game rules, formulas and data are checked against [tarathep/SagaECO](https://github.com/tarathep/SagaECO)
  (GPL-3.0). No SagaECO code is copied; its map files ship under their own license in `offline/data/`.

Details are in [offline/NOTICE](offline/NOTICE).

## License

The code in this repository is licensed under the Apache License 2.0 (see [LICENSE](LICENSE)). The map files in
`offline/data/` are licensed under the GNU General Public License v3.0 (see `offline/data/COPYING`).

## Disclaimer

This is a non-commercial fan preservation project. It is not affiliated with, endorsed or sponsored by GungHo Online
Entertainment, HEADLOCK or BROCCOLI. Emil Chronicle Online and its content belong to their respective owners. Use a
game client you obtained yourself.
