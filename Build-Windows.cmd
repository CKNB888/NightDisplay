@echo off
setlocal
cd /d "%~dp0"
where py >nul 2>nul
if errorlevel 1 (
  python -m venv .build-env
) else (
  py -3 -m venv .build-env
)
if errorlevel 1 goto fail
set "ND_PY=%CD%\.build-env\Scripts\python.exe"
"%ND_PY%" -m pip install ".[tray]" pyinstaller
if errorlevel 1 goto fail
if not exist "vendor\vdd\VirtualDisplayDriver\MttVDD.inf" (
  "%ND_PY%" tools\download_vdd.py
  if errorlevel 1 goto fail
)
"%ND_PY%" tools\collect_licenses.py
if errorlevel 1 goto fail
"%ND_PY%" -m PyInstaller --noconfirm --clean --onefile --windowed --name NightDisplay --add-data "nightdisplay/resources;nightdisplay/resources" --add-data "vendor/vdd;vdd" --hidden-import pystray._win32 --hidden-import PIL._tkinter_finder --hidden-import PIL._imagingtk nightdisplay_main.py
if errorlevel 1 goto fail
echo.
echo Built: %CD%\dist\NightDisplay.exe
echo Test the executable on this computer before enabling the daily schedule.
pause
exit /b 0
:fail
echo Build failed. Keep the messages above for troubleshooting.
pause
exit /b 1
