@echo off
setlocal
cd /d "%~dp0.."

call pnpm run tauri:build:release
if errorlevel 1 goto failed

if not defined CARGO_TARGET_DIR start "" "%CD%\src-tauri\target\local\release\bundle\nsis"
exit /b 0

:failed
echo.
echo Release build failed.
pause
exit /b 1
