@echo off
rem C96 launcher: GUI (browser window) + LAN RSS server.
rem Args are passed through to start-rss-lan.ps1, e.g.
rem   start-rss-lan.bat -Port 8899 -OpenIndex
rem   start-rss-lan.bat -Headless -Kb D:\kb.jsonl
rem   start-rss-lan.bat -Stop
setlocal
set "PS=pwsh"
where pwsh >nul 2>nul || set "PS=powershell"
"%PS%" -NoProfile -ExecutionPolicy Bypass -File "%~dp0start-rss-lan.ps1" %*
echo.
echo Closing this window does NOT stop C96. To stop the RSS server:
echo   start-rss-lan.bat -Stop
pause
