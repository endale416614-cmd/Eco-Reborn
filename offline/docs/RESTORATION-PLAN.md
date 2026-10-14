# Eco Reborn server restoration plan

Goal: the offline server behaves like the live game did for a solo player. NPCs talk and run
their quests, monsters spawn and fight back, shops and the cash shop work, characters level up,
learn skills, and everything persists. The cash shop lists every available item for free.

Written 2026-10-14 from the current code (Rust server in `offline/src`, ~10 client opcodes handled)
and the reference sources in `research/` (SagaECO: 127 map-client opcodes, 2,015 NPC scripts,
full game data; the 2017 EcoServerEmulator for this client's packet layouts).

## Where we are

| Area | Today |
|---|---|
| Login, character list, enter map, walk, save position | Works (Up Town + GM test room) |
| Character creation | Works; occupied slots marked like SagaECO |
| NPC talk / portals | Answer "not available"; client got stuck after it once (needs a trace) |
| Items, inventory, equipment, gold | Not sent (empty inventory marker only) |
| Monsters, combat, EXP, levels | None |
| Quests, shops, cash shop, skills, jobs | None |
| Maps | 2 loaded; no map changes |

## Source material (all local)

| Data | Where | Size |
|---|---|---|
| Cash shop | `research/SagaECO/Configuration/DB/ECOShop.xml` | 39 categories, ~16,000 entries |
| NPC shops | `ShopDB.xml`, `ChipShop.xml` | per-shop goods, buy/sell rates |
| Monsters | `monster.csv` (1,393), `Spawns/` (242 files, 4,421 spawns), `MobAI.xml` | stats, placement, AI |
| Quests | `Quests/` (76 files, 682 quests) + client `quest.csv` | hunt/collect/delivery/rewards |
| Items, skills, jobs | `item*.csv`, `SkillDB.csv`, `SkillList.xml`, `exp.xml` | |
| Warps/portals | `warp.csv`, `MapInfo.zip` (747 maps) | |
| NPC behaviour | `SagaScript/` 2,015 C# scripts (Traditional Chinese text) | 96% use only 40 API functions |
| Packet layouts | SagaECO `Packets/`, EcoServerEmulator, PlutoProject (Saga17) notes | verify against this client |

## How we build it

1. **Rust stays the server.** SagaECO (C#) is the behaviour and data reference; packet layouts are
   checked against the 2017 emulator and, when they differ, against this client (packet trace +
   the client's reaction). We do not run or copy the C# server wholesale.
2. **Data-driven, not hand-coded.** Game data is loaded from the reference files at start-up
   (copied into `offline/data/` with notices). Behaviour is a fixed set of systems in Rust.
3. **NPC scripts become data.** A converter turns each C# script into an `EcoScript` file
   (dialogue, choices, conditions, item/quest/warp actions); a small Rust interpreter runs it.
   The same pass writes the English text. Scripts using functions outside the supported set are
   listed and handled by hand or by adding the function.
4. **Every system lands with an autotest scenario** (`offline/tests/scenarios/*.eco`, new commands
   added to `eco-reborn autotest` as needed) and one real-client check with the packet trace on.
5. **Saves migrate forward** (existing versioned schema in `store.rs`); a backup is taken before any
   migration, and existing characters keep working at every step.

## Phases

Each phase ends playable and committed. Order is chosen so later systems have what they need.

### Phase 0 - Foundations (before any content)
- Opcode coverage report: run the real client through common actions with tracing on; list
  every client request we don't answer (`eco_debug.py trace`), mapped to SagaECO handlers.
- NPC talk unstick: fix the event/message packet layouts so the dialogue opens and closes
  cleanly (blocks portals today).
- Multi-map + warps: load all 747 maps lazily; handle portal events from `warp.csv` and the map
  change packets so the player can leave Up Town.
- Inventory core: item instances, bag/equipment/warehouse containers, gold, persisted; send the
  real inventory on map entry; equip/unequip, move, drop, use (`09C4/09E2/09E7`).
- Character stats: SagaECO `StatusFactory` formulas (HP/MP/SP, ATK/DEF, speed), level/job
  level, status points; sent through the existing status packets.

### Phase 1 - NPCs, shops and the cash shop
- **EcoScript interpreter** with the 40 top functions: Say, Select, Wait, PlaySound, ShowEffect,
  CountItem/GiveItem/TakeItem/CheckInventory, Warp, OpenShopBuy/Sell, HandleQuest, Navigate,
  Synthese, InputBox, GiveRandomTreasure, Heal, SetHomePoint, NPCShow/Hide/Motion, LearnSkill,
  ChangePlayerJob, bank/warehouse, plus per-character flags and variables.
- Script converter: C# subset (if/else, switch on Select, returns, flag masks, local vars) to
  EcoScript; Up Town (133 scripts) first, then towns, then fields and dungeons.
- NPC shops: `ShopDB.xml` buy/sell (`0614/0616/0617`), prices from item data.
- Cash shop (ECO Shop, "VSHOP"): categories and items from `ECOShop.xml`, every entry priced
  0 points; buying (`064A/0654/0641`) puts the item in the bag. Entries whose item ID is missing
  from this client's item tables are dropped and listed. Shop points always shown as available.
- **Chip shop** (DEM) from `ChipShop.xml`.

### Phase 2 - Monsters and combat
- Spawns from `Spawns/` for the current map: actor appear/disappear packets, movement.
- AI from `MobAI.xml`: idle/wander, aggro, chase, attack, leash; respawn timers.
- Player attack (`attack` + skill cast `1387`), damage/hit/crit formulas from SagaECO,
  death and revive at the home point.
- EXP and job EXP (`exp.xml`), level-up, drops (`monster.csv`), item pickup (`07E4`).

### Phase 3 - Quests
- Quest window and board (`1965/1991` ...), quest data from `Quests/` + client `quest.csv`.
- Hunt (kill counts from Phase 2), collect, delivery, timed quests; rewards (EXP, items, gold,
  quest points); persistence (existing quest lifecycle code in `quest.rs` is the starting point).
- Story/Epic chains run through EcoScript (`HandleQuest`, flags).

### Phase 4 - Jobs and skills
- Skill list, skill points, learning (`LearnSkill`), passive/active skill effects from `SkillDB`,
  buffs/debuffs, job change NPCs, second jobs, rebirth if data allows.

### Phase 5 - The rest of the world
- Dungeons (`Dungeon/`, `Dungeons/`), treasure, synthesis/crafting (`SyntheseDB.xml`), pets and
  partners, marionettes, flying garden/castle, golem shops, stamps, theater, fishing.
- Multiplayer-only systems (party, ring, trade, chat channels) answered harmlessly; real support
  comes with the wLAN co-op goal (see `VISION.md`).

## English text

- Client tables: about 6,000 interface strings and item names reviewed. The rest, including the
  ~28,000 long descriptions, are translated from the Japanese in batches with the
  `Build-EnglishUi.py` pipeline.
- NPC dialogue: written in English by the script converter (source text is Traditional Chinese
  in SagaECO; the Japanese client tables are used for names so they match the UI).
- Server messages (system chat, errors) in English from the start.

## Cash shop: all free

- Source: every `<Item>` in `ECOShop.xml` across all 39 categories.
- Price: 0 points for all entries (the file already marks many "Free"; the rest are overridden).
- Filter: drop entries whose item ID is not in the client's `item*.csv` (would show as blank),
  report them in `offline/docs/audit/cash-shop.md`.
- Limits: none per item; stack sizes follow item data.

## Verification

- Autotest scenarios per system (examples: `shop-buy.eco`, `cash-shop.eco`, `hunt-quest.eco`,
  `level-up.eco`, `warp-chain.eco`) and a long soak that mixes them.
- Real client: one traced session per phase covering the new actions; `eco_debug.py trace`
  must show no unanswered requests for them.
- Content coverage: extend `docs/audit/scoresheet.csv` so each NPC script, quest, shop, spawn
  map and dungeon has a status (converted / tested in autotest / played in the client).

## Effort and order of work

Rough size, most to least: script converter + interpreter, monsters/combat, inventory/items,
quests, skills, multi-map/warps, shops + cash shop. Shops and the cash shop are the quickest
visible win once inventory exists, so Phase 0 inventory and Phase 1 cash shop come first, then
NPC scripts town by town, then combat.

## Design decisions

1. **Rates boosted for solo play.** Defaults: base and job EXP x3, item drop rate x2, gold x2,
   quest rewards x2. They live in one settings file (`offline/data/rates.txt`, plain `key = value`)
   so they can be tuned without rebuilding.
2. **Cash shop items are permanent.** Time limits on cash-shop items are removed when bought.
3. **NPC dialogue is machine-translated** to English by the script converter; batches are
   spot-checked, not hand-reviewed line by line.
4. **Later content is recreated where possible.** Client content newer than SagaECO's data (items,
   NPCs, quests, maps present in the client tables but missing server data) is rebuilt from the
   client tables and sensible defaults, and listed in the audit so gaps stay visible.

## Test commands (chat)

Typed in normal chat with `/` (or `!`), handled by `src/commands.rs`; toggles reset on relog.

| Command | Effect | Status |
|---|---|---|
| `/help` | Lists the commands | done |
| `/godmode [on\|off]` | No damage taken, maximum damage dealt | toggle done; takes effect with combat (phase 2) |
| `/killall` | Kills every monster on the map | answers "no monsters yet" until phase 2 |
| `/unstuck` | Teleports to the nearest town safe point | done within the current map; cross-map "nearest town" with map changes (phase 0) |
| `/freebies [on\|off]` | Every purchase is free (NPC shops, cash shop, chip shop) | toggle done; takes effect with shops (phase 1) |

Autotest coverage: `tests/scenarios/commands.eco`.
