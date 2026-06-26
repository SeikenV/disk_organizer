@echo off
rem ============================================================================
rem  predict_videos.bat - run predict_videos.ps1 with sensible defaults.
rem
rem  The INPUT is an existing enrichment-result JSON (the engine's item array).
rem  You generate it FIRST, e.g.:
rem    disk_organizer.exe C --llm --backend cpu ^| Out-File -Encoding utf8 analysis_result.json
rem  This script only READS that file; the predictions report is written separately.
rem
rem  Usage:
rem    - Double-click            : reads the default input file below.
rem    - Drag a JSON onto this   : that JSON is read as the input.
rem    - predict_videos.bat foo.json [cpu cuda vulkan]
rem ============================================================================
setlocal
set "SCRIPT_DIR=%~dp0"
set "PS_SCRIPT=%SCRIPT_DIR%predict_videos.ps1"

rem ---- Default parameters (edit to taste) ----
rem INPUT_JSON: existing enrichment-result file to read (NOT created here).
set "INPUT_JSON=%~1"
if "%INPUT_JSON%"=="" set "INPUT_JSON=%SCRIPT_DIR%..\analysis_result.json"
set "BACKEND=%~2"
if "%BACKEND%"=="" set "BACKEND=cpu"

echo Input JSON : %INPUT_JSON%
echo Backend    : %BACKEND%
echo.

powershell -NoProfile -ExecutionPolicy Bypass -File "%PS_SCRIPT%" -EnrichResult "%INPUT_JSON%" -Backend "%BACKEND%"

echo.
pause
endlocal
