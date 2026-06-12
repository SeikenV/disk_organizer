@echo off
setlocal

:: Check if running as admin
net session >nul 2>&1
if %ERRORLEVEL% neq 0 (
    echo Requesting Administrator privileges...
    :: Relaunch self as admin
    powershell -Command "Start-Process -FilePath '%~f0' -Verb RunAs -WorkingDirectory '%~dp0..'"
    exit /b
)

:: Now running as admin
cd /d "%~dp0.."

echo.
echo ================================================================
echo   disk_organizer: MFT scan (Administrator)
echo ================================================================
echo.

powershell -ExecutionPolicy Bypass -File "scripts\scan_mft.ps1" %*

echo.
echo Done. Next: run scripts\test_llm.bat for LLM enrichment.
echo.
pause
