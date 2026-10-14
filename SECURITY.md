# Security Policy

## Supported versions

Only the latest commit on `main` receives fixes.

## How the game is meant to be isolated

All servers listen on `127.0.0.1` only. `Repair Isolation.cmd` adds Windows Firewall rules that block the game client
(`eco.exe`, `eco_launch.exe`) and the server from reaching the internet, and the launcher refuses to start when those rules are missing
or `eco_jp/server.lst` points anywhere other than `127.0.0.1:17831`. Packet traces are off unless you turn them on, and
stay on your PC.

A way to reach the servers from another computer, to make the client connect elsewhere while the launcher reports the
game as isolated, or to make the launcher or tools write outside the project and client folders counts as a security
problem.

## Reporting a vulnerability

Please report it privately through this repository's **Security** tab (**Report a vulnerability**), not as a public
issue. Include the steps to reproduce and the session report from `offline/reports/` if there is one.
