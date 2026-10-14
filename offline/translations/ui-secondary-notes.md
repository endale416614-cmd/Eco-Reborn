# Secondary UI translation batch

`en/ui-secondary-0001.jsonl` contains 78 draft entries sourced from current-client `table/command.csv` and `table/emotion.csv`. IDs retain the catalog's physical record/field form (`table/<file>:<line>:<column>`), and source values are copied from `catalog.jsonl` without modifying the extracted Japanese files. The shared checker reports 78 translations, 200,312 catalog source candidates, and no code-review flags. All entries remain drafts and have not been installed or checked in the client.

The batch includes five voice-command display names (`command.csv`, field 2), emotion names (`emotion.csv`, field 3), and the table's unlearned-emote display comments (`emotion.csv`, field 8). Japanese command syntax in `emotion.csv` field 6 was intentionally excluded as executable command text. Proper names in the Guardian Spirit comments are phonetic renderings; the official English episode title and character spellings were not established by these files and are marked in each affected entry.

Source files reviewed but not translated in this batch:

- `defaultmacro.csv` is XML containing configured slash-command macros and shortcut bindings; it has no Japanese player-facing labels in its values. Command strings were left intact.
- `window_param.csv` is XML layout configuration. Its text nodes are numeric layout values/identifiers, with no Japanese text nodes found.
- `shortcut_menu.csv` is CP932 and contains Japanese labels only in comment lines; its actual data rows are numeric menu ordering/group references. Those comments are useful as a glossary, but they are not runtime strings, so they were not treated as visible-text translations. A future source-linked mapping should establish which runtime string IDs those comments describe before translating them as actual UI labels.

`emotion.csv` also contains two full-width OK labels that the CJK-only candidate catalog omits. They are not included in this checker-verified batch; add them only after the catalog workflow supports non-CJK UI text or a separately validated source record. The extracted source and original archives remain unchanged.
