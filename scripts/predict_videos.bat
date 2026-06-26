@echo off
rem ============================================================================
rem  predict_videos.bat - run predict_videos.ps1 with sensible defaults.
rem
rem  Usage:
rem    - Double-click            : uses the default results file below.
rem    - Drag a JSON onto this   : that JSON is used as the results file.
rem    - predict_videos.bat foo.json [cpu cuda vulkan]
rem ============================================================================
setlocal
set "SCRIPT_DIR=%~dp0"
set "PS_SCRIPT=%SCRIPT_DIR%predict_videos.ps1"

rem ---- Default parameters (edit to taste) ----
set "RESULTS=%~1"
if "%RESULTS%"=="" set "RESULTS=%SCRIPT_DIR%..\result.json"
set "BACKEND=%~2"
if "%BACKEND%"=="" set "BACKEND=cpu"

echo Results : %RESULTS%
echo Backend : %BACKEND%
echo.

powershell -NoProfile -ExecutionPolicy Bypass -File "%PS_SCRIPT%" -Results "%RESULTS%" -Backend "%BACKEND%"

echo.
pause
endlocal
