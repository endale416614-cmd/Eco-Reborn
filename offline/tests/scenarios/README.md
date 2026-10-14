# Autotest scenarios

`eco-reborn autotest <scenario.eco>` starts its own world/login/map servers on spare loopback ports
with a throwaway save, then plays the scenario over the real encrypted protocol. It never touches
`saves/world.sqlite`, the game client or a running game. Double-click `Run Autotests.cmd` in the
Eco Reborn folder to run every scenario here.

```
eco-reborn autotest tests/scenarios/smoke.eco
eco-reborn autotest tests/scenarios/my-character.eco --save saves/world.sqlite   # copy of your save
```

Options: `--save <file>` test a copy of that save, `--trace` write packet traces (needs
`research/local-trace/ENABLED`), `--keep` keep the work folder with server logs, `--keep-going`
continue after a failed step. Exit code 0 = pass, 1 = a step failed, 2 = could not run.
A text report goes to `offline/reports/autotest-<time>.txt`.

## Commands (one per line, `#` starts a comment)

| Command | What it does and checks |
|---|---|
| `account <name> <token>` | Log in as another account (default `test`) |
| `login` | Gate auth + server list, then login-server auth (character list) |
| `create <name> [race] [sex]` | Create a character in the current slot; expects success |
| `select <slot>` | Select slot, map redirect, friend init; closes the login connection |
| `place <map> <x> <y> [dir]` | Move the slot's character in the save (tile must be walkable) |
| `enter` | Map server auth + map entry; records the spawn from 01FF |
| `relog` | Close the map connection and `enter` again |
| `loaded` | Map loaded (11FE); expects the speed unlock |
| `move <x> <y> [deg]` | Walk to a tile; expects the 11F9 echo |
| `expect-spawn <x> <y> [map]` | Last spawn position equals this |
| `expect-saved <x> <y> [map]` | Position stored in the save equals this |
| `npc <event id>` | Talk/portal event; expects the full event open/message/close sequence |
| `emote <motion> [loop]`, `stance <n>` | Expect the 121C / 0FA6 echo |
| `chat <text>` | Send a chat line or test command; prints the reply text (and notes a teleport) |
| `logout` | Expects 0020, then closes the map connection |
| `send <op> [hex payload] [op,op]` | Raw packet; optionally check reply opcodes |
| `wait <ms>`, `servers-alive` | Pause; check no server process exited |
| `repeat <n>` ... `end` | Repeat a block (soak tests); repeated steps print only on failure |

Every step is timed; the summary table shows count, average and worst time per command, which is
the quickest way to spot slowdowns over a long soak run.
