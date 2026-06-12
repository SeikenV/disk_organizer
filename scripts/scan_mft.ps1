# Scan NTFS MFT and save snapshot (requires Administrator)
param(
    [string]$Drive = "C",
    [int]$Top = 1000,
    [int]$MinMb = 50,
    [string]$Snapshot = "scan.snapshot.json"
)

$ErrorActionPreference = "Continue"
Set-Location $PSScriptRoot\..

Write-Host "[BUILD] Compiling release binary..." -ForegroundColor DarkGray
cargo build --release
if ($LASTEXITCODE -ne 0) {
    Write-Host "[ERROR] Build failed." -ForegroundColor Red
    exit 1
}
Write-Host "[BUILD] OK" -ForegroundColor Green
Write-Host ""

Write-Host "================================================================" -ForegroundColor Cyan
Write-Host "  disk_organizer: MFT scan" -ForegroundColor Cyan
Write-Host "  Drive: $Drive  |  Top: $Top  |  Min: ${MinMb}MB" -ForegroundColor DarkGray
Write-Host "  Snapshot: $Snapshot" -ForegroundColor DarkGray
Write-Host "  *** REQUIRES ADMINISTRATOR ***" -ForegroundColor Yellow
Write-Host "================================================================" -ForegroundColor Cyan
Write-Host ""

$Exe = "target\release\disk_organizer.exe"

# Scan MFT, save snapshot. No LLM — just raw classification.
& $Exe --debug $Drive --top $Top --min-size-mb $MinMb --save-snapshot $Snapshot | Out-Null

if ($LASTEXITCODE -ne 0) {
    Write-Host ""
    Write-Host "[ERROR] MFT scan failed (exit code: $LASTEXITCODE)" -ForegroundColor Red
    exit 1
}

$size = [math]::Round((Get-Item $Snapshot).Length / 1KB, 1)
Write-Host ""
Write-Host "[OK] Snapshot saved: $Snapshot ($size KB)" -ForegroundColor Green
Write-Host ""
Write-Host "Next: run .\scripts\test_llm.ps1  (no admin needed)" -ForegroundColor White
Write-Host ""
