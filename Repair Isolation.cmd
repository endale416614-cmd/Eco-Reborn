@echo off
rem Re-points Eco Reborn's firewall isolation rules at this folder (needed after moving or renaming it).
rem Removes the old rules (including pre-rebrand "ECO Offline" ones), then re-creates them. Asks for UAC.
setlocal
set "SCRIPT=%~dp0offline\Set-RebornIsolation.ps1"
net session >nul 2>&1
if errorlevel 1 (
    powershell -NoProfile -Command "Start-Process -FilePath '%~f0' -Verb RunAs"
    exit /b
)
powershell -NoProfile -File "%SCRIPT%" Disable || goto failed
powershell -NoProfile -File "%SCRIPT%" Enable || goto failed
echo.
echo Isolation repaired. You can close this window and start Eco Reborn.
pause
exit /b 0
:failed
echo.
echo Isolation repair failed. Nothing else was changed; see the message above.
pause
exit /b 1
