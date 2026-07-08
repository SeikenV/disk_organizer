# Test LLM enrichment from existing snapshot (no admin needed)
param(
    [int]$Top = 1000,
    [int]$MinMb = 100,
    [int]$Samples = 30,
    [string]$OutJson = "analysis_result.json"
)

$ErrorActionPreference = "Continue"
Set-Location $PSScriptRoot\..
$GlobalStart = Get-Date

function Elapsed {
    return [math]::Round(((Get-Date) - $GlobalStart).TotalSeconds, 1)
}

# ---- Pre-flight: check that llama.cpp and models are set up ----
$LlamaDir    = "tools\llamacpp"
$ModelDir    = "tools\models"
$TextModel   = Join-Path $ModelDir "Qwen3.5-0.8B-UD-Q4_K_XL.gguf"

$hasBackend  = $false
$backendList = @()
if (Test-Path $LlamaDir) {
    foreach ($b in @("cpu", "cuda", "vulkan")) {
        $exe = Join-Path $LlamaDir "$b\llama-server.exe"
        if (Test-Path $exe) {
            $hasBackend = $true
            $backendList += $b
        }
    }
}

$hasModel = Test-Path $TextModel

if (-not $hasBackend -or -not $hasModel) {
    Write-Host ""
    Write-Host "================================================================" -ForegroundColor Red
    Write-Host "  [ERROR] LLM environment not ready!" -ForegroundColor Red
    Write-Host "================================================================" -ForegroundColor Red
    Write-Host ""
    if (-not $hasBackend) {
        Write-Host "  Missing: llama-server binary (none found under $LlamaDir\)" -ForegroundColor Yellow
    } else {
        Write-Host "  [OK] Backends found: $($backendList -join ', ')" -ForegroundColor Green
    }
    if (-not $hasModel) {
        Write-Host "  Missing: $TextModel" -ForegroundColor Yellow
    } else {
        Write-Host "  [OK] Text model found" -ForegroundColor Green
    }
    Write-Host ""
    Write-Host "  Please run first:" -ForegroundColor White
    Write-Host "    .\scripts\setup_llama_cpp.bat" -ForegroundColor Cyan
    Write-Host ""
    Write-Host "================================================================" -ForegroundColor Red
    exit 1
}

if ($backendList.Count -gt 0) {
    Write-Host "[PREFLIGHT] Backends: $($backendList -join ', ')  |  Model: $TextModel" -ForegroundColor DarkGray
}

# Always rebuild before running.
Write-Host "[TIMING] $(Elapsed)s - Starting build..." -ForegroundColor DarkGray
$BuildStart = Get-Date
Write-Host "[BUILD] Compiling release binary..." -ForegroundColor DarkGray
cargo build --release
if ($LASTEXITCODE -ne 0) {
    Write-Host "[ERROR] Build failed." -ForegroundColor Red
    exit 1
}
$BuildTime = [math]::Round(((Get-Date) - $BuildStart).TotalSeconds, 1)
Write-Host "[TIMING] $(Elapsed)s - Build complete ($BuildTime s)" -ForegroundColor DarkGray
Write-Host "[BUILD] OK" -ForegroundColor Green
Write-Host ""

$Exe = "target\release\disk_organizer.exe"
$Snapshot = "scan.snapshot.json"

if (-not (Test-Path $Snapshot)) {
    Write-Host ""
    Write-Host "[ERROR] No snapshot found: $Snapshot" -ForegroundColor Red
    Write-Host ""
    Write-Host "  Two-step remedy:" -ForegroundColor White
    Write-Host "    1. Run setup if you haven't yet:" -ForegroundColor White
    Write-Host "       .\scripts\setup_llama_cpp.bat" -ForegroundColor Cyan
    Write-Host "    2. Run full test as Administrator:" -ForegroundColor White
    Write-Host "       .\scripts\test_llm_full.bat" -ForegroundColor Cyan
    Write-Host ""
    exit 1
}

Write-Host ""
Write-Host "================================================================" -ForegroundColor Cyan
Write-Host "  disk_organizer: LLM enrichment test" -ForegroundColor Cyan
Write-Host "  Snapshot: $Snapshot" -ForegroundColor DarkGray
Write-Host "  Top: $Top  |  Min: ${MinMb}MB  |  Samples: $Samples" -ForegroundColor DarkGray
Write-Host "  Output: $OutJson" -ForegroundColor DarkGray
Write-Host "================================================================" -ForegroundColor Cyan
Write-Host ""

# stdout = JSON, stderr = progress. Redirect stdout to file.
Write-Host "[TIMING] $(Elapsed)s - Starting LLM enrichment..." -ForegroundColor DarkGray
$LlmStart = Get-Date
& $Exe --debug --from-snapshot $Snapshot --top $Top --min-size-mb $MinMb --llm --llm-samples $Samples > $OutJson

if ($LASTEXITCODE -ne 0) {
    Write-Host ""
    Write-Host "[ERROR] Exit code: $LASTEXITCODE" -ForegroundColor Red
} else {
    $LlmTime = [math]::Round(((Get-Date) - $LlmStart).TotalSeconds, 1)
    $size = [math]::Round((Get-Item $OutJson).Length / 1KB, 1)
    Write-Host "[TIMING] $(Elapsed)s - Enrichment phase complete ($LlmTime s)" -ForegroundColor DarkGray
    Write-Host "[OK] Analysis complete. Results saved to: $OutJson ($size KB)" -ForegroundColor Green

    # ---- Parse results ----
    $items = Get-Content $OutJson | ConvertFrom-Json

    # ---- Summary ----
    Write-Host ""
    Write-Host "======================================================================" -ForegroundColor Cyan
    Write-Host "  SUMMARY REPORT" -ForegroundColor Cyan
    Write-Host "======================================================================" -ForegroundColor Cyan

    $totalItems = $items.Count
    $totalSize = ($items | Measure-Object -Property physical_size -Sum).Sum

    function Format-Bytes($bytes) {
        if ($bytes -ge 1TB) { return "$([math]::Round($bytes / 1TB, 2)) TB" }
        if ($bytes -ge 1GB) { return "$([math]::Round($bytes / 1GB, 2)) GB" }
        if ($bytes -ge 1MB) { return "$([math]::Round($bytes / 1MB, 2)) MB" }
        if ($bytes -ge 1KB) { return "$([math]::Round($bytes / 1KB, 2)) KB" }
        return "$bytes B"
    }

    Write-Host ""
    Write-Host "  Items analyzed : $totalItems" -ForegroundColor White
    Write-Host "  Total space    : $(Format-Bytes $totalSize)" -ForegroundColor White
    Write-Host ""

    # --- Distribution by risk ---
    $byRisk = $items | Group-Object risk | Sort-Object Count -Descending
    Write-Host "  --- By Risk Level ---" -ForegroundColor Yellow
    foreach ($g in $byRisk) {
        $sum = ($g.Group | Measure-Object -Property physical_size -Sum).Sum
        $color = switch ($g.Name) {
            "Safe"    { "Green" }
            "Caution" { "Yellow" }
            "System"  { "Red" }
            "Unknown" { "DarkGray" }
            default   { "White" }
        }
        Write-Host "    $($g.Name): $($g.Count) items, $(Format-Bytes $sum)" -ForegroundColor $color
    }

    # --- Distribution by source ---
    $bySource = $items | Group-Object source | Sort-Object Count -Descending
    Write-Host ""
    Write-Host "  --- By Classification Source ---" -ForegroundColor Yellow
    foreach ($g in $bySource) {
        $sum = ($g.Group | Measure-Object -Property physical_size -Sum).Sum
        Write-Host "    $($g.Name): $($g.Count) items, $(Format-Bytes $sum)" -ForegroundColor White
    }

    # --- Top space consumers ---
    Write-Host ""
    Write-Host "  --- Top 10 Space Consumers ---" -ForegroundColor Yellow
    $items | Sort-Object physical_size -Descending | Select-Object -First 10 | ForEach-Object {
        $sz = Format-Bytes $_.physical_size
        $riskColor = switch ($_.risk) {
            "Safe"    { "Green" }
            "Caution" { "Yellow" }
            "System"  { "Red" }
            default   { "DarkGray" }
        }
        Write-Host "    [$($_.risk)] $sz  $($_.path)" -ForegroundColor $riskColor
    }

    # ---- Cleanup Suggestions ----
    Write-Host ""
    Write-Host "======================================================================" -ForegroundColor Cyan
    Write-Host "  CLEANUP SUGGESTIONS" -ForegroundColor Cyan
    Write-Host "======================================================================" -ForegroundColor Cyan

    # ===== SAFE: can be deleted directly =====
    $safeItems = $items | Where-Object { $_.risk -eq "Safe" } | Sort-Object physical_size -Descending
    $safeTotal = ($safeItems | Measure-Object -Property physical_size -Sum).Sum

    Write-Host ""
    Write-Host "  [SAFE TO DELETE] - These are caches/temp files that will be regenerated." -ForegroundColor Green
    Write-Host "  If you delete all Safe items, you recover: $(Format-Bytes $safeTotal)" -ForegroundColor Green

    if ($safeItems.Count -gt 0) {
        Write-Host ""
        $safeItems | ForEach-Object {
            $sz = Format-Bytes $_.physical_size
            Write-Host "    $sz  $($_.category)  --  $($_.path)" -ForegroundColor DarkGreen
        }
    } else {
        Write-Host "  (none found)" -ForegroundColor DarkGray
    }

    # ===== CAUTION: review before deleting =====
    $cautionItems = $items | Where-Object { $_.risk -eq "Caution" } | Sort-Object physical_size -Descending
    $cautionTotal = ($cautionItems | Measure-Object -Property physical_size -Sum).Sum

    Write-Host ""
    Write-Host "  [REVIEW BEFORE DELETING] - These may be useful; check first." -ForegroundColor Yellow
    Write-Host "  Potential recovery: $(Format-Bytes $cautionTotal)" -ForegroundColor Yellow

    if ($cautionItems.Count -gt 0) {
        Write-Host ""
        $cautionItems | ForEach-Object {
            $sz = Format-Bytes $_.physical_size
            Write-Host "    $sz  $($_.category)  --  $($_.path)" -ForegroundColor DarkYellow
        }
    } else {
        Write-Host "  (none found)" -ForegroundColor DarkGray
    }

    # ===== SYSTEM: do NOT delete =====
    $systemItems = $items | Where-Object { $_.risk -eq "System" }
    $sysTotal = ($systemItems | Measure-Object -Property physical_size -Sum).Sum

    Write-Host ""
    Write-Host "  [DO NOT DELETE] - These are critical system files/apps." -ForegroundColor Red
    Write-Host "  These occupy: $(Format-Bytes $sysTotal)" -ForegroundColor Red
    Write-Host "  Remove through formal uninstall or system settings, not direct deletion." -ForegroundColor Red

    # ===== UNKNOWN: need LLM/manual review =====
    $unknownItems = $items | Where-Object { $_.risk -eq "Unknown" } | Sort-Object physical_size -Descending
    $unkTotal = ($unknownItems | Measure-Object -Property physical_size -Sum).Sum

    if ($unknownItems.Count -gt 0) {
        Write-Host ""
        Write-Host "  [STILL UNKNOWN] - These could not be classified." -ForegroundColor DarkGray
        Write-Host "  Total: $(Format-Bytes $unkTotal) in $($unknownItems.Count) items" -ForegroundColor DarkGray
        Write-Host ""
        $unknownItems | ForEach-Object {
            $sz = Format-Bytes $_.physical_size
            Write-Host "    $sz  $($_.path)" -ForegroundColor DarkGray
        }
    }

    # ===== Quick-delete commands =====
    if ($safeItems.Count -gt 0) {
        Write-Host ""
        Write-Host "======================================================================" -ForegroundColor Cyan
        Write-Host "  QUICK ACTIONS (PowerShell)" -ForegroundColor Cyan
        Write-Host "======================================================================" -ForegroundColor Cyan
        Write-Host ""
        Write-Host "  # Dry-run: list all Safe items              " -ForegroundColor White
        Write-Host '  $items | ? {$_.risk -eq "Safe"} | select path,category,@{n="SizeMB";e={[math]::Round($_.physical_size/1MB,1)}} | ft -AutoSize' -ForegroundColor DarkGray
        Write-Host ""
        Write-Host "  # Calculate total recoverable space         " -ForegroundColor White
        Write-Host '  $safeMB = ($items | ? {$_.risk -eq "Safe"} | Measure-Object physical_size -Sum).Sum / 1MB; "Recoverable: $([math]::Round($safeMB,0)) MB"' -ForegroundColor DarkGray
    }

    Write-Host ""
    Write-Host "======================================================================" -ForegroundColor Cyan
}

# ---- Final timing ----
$TotalTime = [math]::Round(((Get-Date) - $GlobalStart).TotalSeconds, 1)
Write-Host ""
Write-Host "======================================================================" -ForegroundColor Magenta
Write-Host "  SCRIPT TIMING SUMMARY" -ForegroundColor Magenta
Write-Host "======================================================================" -ForegroundColor Magenta
Write-Host "  Phase         Duration  % of total"
Write-Host "  ---           --------  ----------"
$pct = if ($TotalTime -gt 0) { [math]::Round($BuildTime / $TotalTime * 100) } else { 0 }
Write-Host "  Build         ${BuildTime}s    ${pct}%" -ForegroundColor White
$pct = if ($TotalTime -gt 0) { [math]::Round($LlmTime / $TotalTime * 100) } else { 0 }
Write-Host "  LLM enrich    ${LlmTime}s    ${pct}%" -ForegroundColor White
Write-Host "  ---           --------  ----------"
Write-Host "  Total         ${TotalTime}s    100%" -ForegroundColor Cyan
Write-Host "======================================================================" -ForegroundColor Magenta
Write-Host ""
