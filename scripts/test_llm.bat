@echo off
setlocal
cd /d "%~dp0.."

echo.
echo ================================================================
echo   disk_organizer: LLM enrichment (no admin needed)
echo ================================================================
echo.

powershell -ExecutionPolicy Bypass -File "scripts\test_llm.ps1" %*

if %ERRORLEVEL% neq 0 (
    echo.
    echo [ERROR] LLM enrichment failed (code %ERRORLEVEL%).
    echo.
    pause
    exit /b %ERRORLEVEL%
)

echo.
echo Done! Results saved to analysis_result.json
echo.
pause
