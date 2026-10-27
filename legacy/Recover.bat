@echo off
setlocal
powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%LOCALAPPDATA%\NightDisplay\NightDisplay.ps1" -Action Emergency
pause
