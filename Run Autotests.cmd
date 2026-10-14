@echo off
rem Runs every Eco Reborn autotest scenario against throwaway servers and saves.
setlocal
cd /d "%~dp0offline"
set FAILED=0
for %%S in (tests\scenarios\*.eco) do (
    if /i not "%%~nS"=="my-character" (
        target\release\eco-reborn.exe autotest "%%S" || set FAILED=1
    )
)
if exist saves\world.sqlite (
    target\release\eco-reborn.exe autotest tests\scenarios\my-character.eco --save saves\world.sqlite || set FAILED=1
)
echo.
if "%FAILED%"=="0" (echo All autotests passed.) else (echo Some autotests FAILED - see the output above.)
pause
exit /b %FAILED%
