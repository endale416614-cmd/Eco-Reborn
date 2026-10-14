# English translations

English text for the Japanese client's tables. Every row points at one exact cell of the original client data, so
translations can be checked against their source and rebuilt at any time. The client archives are never edited in
place: `tools/Build-EnglishUi.py` writes a new table pair and `tools/Install-English.ps1` installs it with a backup.

## Files

| Path | What |
|---|---|
| `en/*.jsonl` | translation batches: interface (`ui-*`) and item names (`items-*`) |
| `reviewed-batches.json` | the batches that are reviewed and installed, pinned by SHA-256 |
| `font-overrides.jsonl` | font settings (Yu Gothic instead of the Japanese fonts), not translations |
| `overrides.jsonl` | Eco Reborn's own labels (display mode options) |
| `excluded-config.jsonl`, `advanced-exclusions.jsonl` | cells that look like text but are configuration |
| `pending-*-fragments.jsonl` | fragments held back until their surrounding name or count is known |
| `inventory.json` | every source file in the client tables with its hash, encoding and parse status |
| `source-archives.json` | SHA-256 of the original `table`/`text` archives the builder accepts |
| `REVIEW_CONVENTIONS.md` | terminology and review rules |

Each row looks like this:

```json
{"id": "table/strtable.csv:1486:2", "source": "ウィンドウ", "english": "Borderless Window", "status": "reviewed"}
```

`id` is `<archive member>:<physical line>:<field>`. `source` must match the client cell exactly.

## Rules

- Keep IDs and source text exact. Keep control codes (`$r` and friends), printf placeholders, brace tokens and their
  order.
- English must be encodable in CP932 (the client's code page).
- Don't translate configuration keys, resource paths, slash commands or font names.
- Uncertain terms get a note and stay `draft`.
- `strtable.csv` line 419 has a space after a closing quote; strict CSV parsing stops there, so later rows of that
  file are not cataloged yet.

## Commands

```powershell
rustc --edition=2021 offline/tools/Export-ClientText.rs -o offline/target/export-client-text.exe
offline/target/export-client-text.exe eco_jp offline/translations/source-ja
python offline/tools/Translation-Catalog.py build --source offline/translations/source-ja --catalog offline/translations/catalog.jsonl --inventory offline/translations/build/inventory.json
python offline/tools/Translation-Catalog.py check --catalog offline/translations/catalog.jsonl --translations offline/translations/en/ui-0001.jsonl
python -X utf8 -m unittest discover -s offline/tools/tests -p test_translation_catalog.py -v
```

The export and catalog come from your own client and are not stored in the repository. The tools refuse to overwrite
existing output. Run `check` on every new batch before adding it to `reviewed-batches.json`.

## Not covered yet

Long item descriptions, quest text, NPC dialogue (it comes from the server scripts, not the client tables), XML
tables, text inside images and strings inside `eco.exe`.
