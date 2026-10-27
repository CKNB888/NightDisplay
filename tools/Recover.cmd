@echo off
"%~dp0NightDisplay.exe" --action emergency
if errorlevel 1 pause
