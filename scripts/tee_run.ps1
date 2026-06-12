# Tee a PowerShell script's output to both screen and a UTF-8 log file.
# Usage: powershell -File tee_run.ps1 -Script "path\to\script.ps1" -LogFile "path\to\output.log"
#
# Strategy: redirect ALL streams (*>) to a temp file so no pipeline interaction
# interferes with the target script. After it completes, read + display + log.
param(
    [Parameter(Mandatory=$true)] [string]$Script,
    [Parameter(Mandatory=$true)] [string]$LogFile
)

$ErrorActionPreference = "Continue"

# Ensure log directory exists
$logDir = Split-Path $LogFile -Parent
if ($logDir -and -not (Test-Path $logDir)) {
    New-Item -ItemType Directory -Path $logDir -Force | Out-Null
}

# Temp file for captured output
$tempOut = Join-Path ([System.IO.Path]::GetTempPath()) "tee_run_$([System.IO.Path]::GetRandomFileName()).log"

try {
    # *> redirects ALL streams to file — pure output capture, zero pipeline interference
    & $Script *> $tempOut

    $exitCode = $LASTEXITCODE

    # Read captured output and tee to screen + UTF-8 log
    if (Test-Path $tempOut) {
        Get-Content $tempOut -Encoding utf8 | ForEach-Object {
            $_ | Out-File -FilePath $LogFile -Encoding utf8 -Append
            Write-Host $_
        }
    }
} finally {
    Remove-Item $tempOut -Force -ErrorAction SilentlyContinue
}

exit $exitCode
