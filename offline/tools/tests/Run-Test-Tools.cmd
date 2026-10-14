@echo off
setlocal
cd /d "%~dp0\..\..\.." || exit /b 2
python offline\tools\tests\run.py %*
set "RESULT=%ERRORLEVEL%"
echo.
echo Reports: offline\docs\audit
pause
exit /b %RESULT%
