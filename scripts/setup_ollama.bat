@echo off
setlocal
set LOGFILE=%~dp0last_run_setup.log

cd /d "%~dp0"

echo.
echo ============================================
echo   Ollama Setup for disk_organizer
echo ============================================
echo.
echo This will install Ollama and pull the model.
echo (~1.5 GB download, one-time only)
echo.
echo Log: %LOGFILE%
echo.

REM Initialize log in UTF-8 via PowerShell
powershell -NoProfile -Command "'============================================', ' Ollama Setup started at %DATE% %TIME%', '============================================', '' | Out-File '%LOGFILE%' -Encoding utf8"

REM Run ps1: show on screen AND capture to UTF-8 log via tee_run helper
powershell -ExecutionPolicy Bypass -File "%~dp0tee_run.ps1" -Script "%~dp0setup_ollama.ps1" -LogFile "%LOGFILE%"
if %ERRORLEVEL% equ 0 (
    echo.
    echo ============================================
    echo   Setup complete! You can now use:
    echo     cargo run --release -- C --llm
    echo   Or double-click: test_llm_full.bat
    echo ============================================
    powershell -NoProfile -Command "'[%DATE% %TIME%] Setup OK' | Out-File '%LOGFILE%' -Encoding utf8 -Append"
) else (
    echo.
    echo ============================================
    echo [ERROR] Setup failed (code %ERRORLEVEL%).
    echo Full log saved to: %LOGFILE%
    echo ============================================
    powershell -NoProfile -Command "'[%DATE% %TIME%] Setup failed with code %ERRORLEVEL%' | Out-File '%LOGFILE%' -Encoding utf8 -Append"
)
echo.
pause
