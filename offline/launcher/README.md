# Eco Reborn launcher

`Eco Reborn.exe` runs `offline/Start-EcoReborn.ps1` with Windows PowerShell 5.1 and shows a small start-up window
until the game is up. The script does the real work: isolation checks, starting the three servers, starting
`eco.exe`, writing the session report and shutting everything down when the game exits.

Keep the executable in the repository root, next to the `offline` and `eco_jp` folders. It logs in with the built-in
local account, so there is no login screen. A second launch while the game is running is refused. Close the game
itself to end the session.

Build and test from the repository root:

```powershell
powershell.exe -NoProfile -File offline/launcher/build.ps1 -Check
powershell.exe -NoProfile -File offline/tests/Test-DesktopLauncher.ps1
```

The build uses the .NET Framework C# compiler that comes with Windows and takes its icon from `eco_jp/eco.exe`. It
also builds `offline/target/eco-window.exe`, which handles borderless display mode.

`offline/launcher-settings.txt` picks the display mode: `display = borderless` (default) or `display = windowed`.

The lifecycle test runs three fake sessions under `offline/target`, from paths with spaces and an apostrophe. It never
starts the game or touches the save. Close Eco Reborn before running it.
