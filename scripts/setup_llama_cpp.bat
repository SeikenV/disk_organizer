@echo off
setlocal
set LOGFILE=%~dp0last_run_setup.log

cd /d "%~dp0"

echo.
echo ============================================
echo   llama.cpp Setup for disk_organizer
echo ============================================
echo.
echo This will:
echo   - detect llama-server.exe on your system
echo   - add it to PATH if needed
echo   - copy models from Downloads into tools\models
echo.
echo Log: %LOGFILE%
echo.

REM Initialize log in UTF-8 via PowerShell
powershell -NoProfile -Command "'============================================', ' llama.cpp Setup started at %DATE% %TIME%', '============================================', '' | Out-File '%LOGFILE%' -Encoding utf8"

REM Run ps1: show on screen AND capture to UTF-8 log via tee_run helper
powershell -ExecutionPolicy Bypass -File "%~dp0tee_run.ps1" -Script "%~dp0setup_llama_cpp.ps1" -LogFile "%LOGFILE%"
if %ERRORLEVEL% equ 0 (
    echo.
    echo ============================================
    echo   Setup complete! You can now use:
    echo     cargo run -p disk_organizer -- C --llm
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
