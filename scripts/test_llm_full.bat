@echo off
setlocal enabledelayedexpansion
set LOGFILE=%~dp0last_run_llm.log

cd /d "%~dp0.."

echo.
echo ================================================================
echo   disk_organizer: Full test (MFT scan + LLM enrichment)
echo ================================================================
echo.
echo Step 1: Scan NTFS MFT for C: (needs Administrator!)
echo Step 2: Classify + LLM enrichment (no admin needed)
echo ================================================================
echo.
echo Log: %LOGFILE%
echo.

REM Check admin before MFT scan
net session >nul 2>&1
if %ERRORLEVEL% neq 0 (
    echo ================================================================
    echo [WARNING] NOT running as Administrator!
    echo MFT scan REQUIRES admin privileges - it will fail.
    echo.
    echo Options:
    echo   1. Close, right-click this .bat, "Run as Administrator"
    echo   2. Or: if you already have scan.snapshot.json,
    echo      skip MFT scan and only run LLM enrichment:
    echo      double-click test_llm.ps1 instead
    echo ================================================================
    echo.
    choice /C 12 /N /M "Choose [1=Exit, 2=Try anyway (will fail)]: "
    if !ERRORLEVEL! equ 1 exit /b 1
)

REM Initialize log in UTF-8 via PowerShell
powershell -NoProfile -Command "'======================================================', ' disk_organizer: Full test started at %DATE% %TIME%', '======================================================', '' | Out-File '%LOGFILE%' -Encoding utf8"

echo.
echo [1/2] Running MFT scan...

powershell -NoProfile -Command "'[%DATE% %TIME%] Starting MFT scan...' | Out-File '%LOGFILE%' -Encoding utf8 -Append"

powershell -ExecutionPolicy Bypass -File "%~dp0tee_run.ps1" -Script "%~dp0scan_mft.ps1" -LogFile "%LOGFILE%"
if %ERRORLEVEL% neq 0 (
    echo.
    echo ================================================================
    echo [ERROR] MFT scan failed (code %ERRORLEVEL%).
    echo ================================================================
    echo.
    echo Common causes:
    echo   - Not running as Administrator
    echo   - C: drive not accessible
    echo.
    echo See details in: %LOGFILE%
    echo ================================================================
    powershell -NoProfile -Command "'[%DATE% %TIME%] MFT scan failed with code %ERRORLEVEL%' | Out-File '%LOGFILE%' -Encoding utf8 -Append"
    pause
    exit /b %ERRORLEVEL%
)

powershell -NoProfile -Command "'[%DATE% %TIME%] MFT scan OK' | Out-File '%LOGFILE%' -Encoding utf8 -Append"

echo.
echo [2/2] Running LLM enrichment...

powershell -NoProfile -Command "'[%DATE% %TIME%] Starting LLM enrichment...' | Out-File '%LOGFILE%' -Encoding utf8 -Append"

powershell -ExecutionPolicy Bypass -File "%~dp0tee_run.ps1" -Script "%~dp0test_llm.ps1" -LogFile "%LOGFILE%"
if %ERRORLEVEL% neq 0 (
    echo.
    echo ================================================================
    echo [ERROR] LLM test failed (code %ERRORLEVEL%).
    echo ================================================================
    echo.
    echo See details in: %LOGFILE%
    echo ================================================================
    powershell -NoProfile -Command "'[%DATE% %TIME%] LLM test failed with code %ERRORLEVEL%' | Out-File '%LOGFILE%' -Encoding utf8 -Append"
    pause
    exit /b %ERRORLEVEL%
)

echo.
echo ================================================================
echo   Full test complete!
echo   Results: analysis_result.json
echo   Run log: %LOGFILE%
echo ================================================================
echo.
powershell -NoProfile -Command "'[%DATE% %TIME%] Full test complete' | Out-File '%LOGFILE%' -Encoding utf8 -Append"
pause
