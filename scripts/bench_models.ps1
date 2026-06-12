# Benchmark all available Ollama models for enrichment speed & quality
param(
    [int]$Top = 1000,
    [int]$MinMb = 100,
    [int]$Samples = 30
)

$ErrorActionPreference = "Continue"
Set-Location $PSScriptRoot\..

$Timestamp = Get-Date -Format "yyyy-MM-dd_HH-mm-ss"
$LogRoot = "logs\$Timestamp"
New-Item -ItemType Directory -Path $LogRoot -Force | Out-Null

$GlobalStart = Get-Date
function Elapsed { [math]::Round(((Get-Date) - $GlobalStart).TotalSeconds, 1) }

# ---- Models to test ----
$Models = @(
    "qwen3.5:0.8b",
    "qwen3:0.6b",
    "reaperdoesntrun/Qwen3-0.6B-Distilled:latest",
    "radenadri/Qwen3.5-0.8B-Claude-4.6-Opus-Reasoning-Distilled-GGUF:latest"
)

# ---- Build once ----
Write-Host ""
Write-Host "============================================================" -ForegroundColor Cyan
Write-Host "  MODEL BENCHMARK — $($Models.Count) models" -ForegroundColor Cyan
Write-Host "  Snapshot: scan.snapshot.json" -ForegroundColor DarkGray
Write-Host "  Top: $Top  |  Min: ${MinMb}MB  |  Samples: $Samples" -ForegroundColor DarkGray
Write-Host "  Log dir: $LogRoot" -ForegroundColor DarkGray
Write-Host "============================================================" -ForegroundColor Cyan
Write-Host ""

Write-Host "[BUILD] Compiling..." -ForegroundColor DarkGray
$BuildStart = Get-Date
cargo build --release
if ($LASTEXITCODE -ne 0) { Write-Host "[ERROR] Build failed." -ForegroundColor Red; exit 1 }
$BuildTime = [math]::Round(((Get-Date) - $BuildStart).TotalSeconds, 1)
Write-Host "[BUILD] OK ($BuildTime s)" -ForegroundColor Green
Write-Host ""

$Exe = "target\release\disk_organizer.exe"
$Snapshot = "scan.snapshot.json"

if (-not (Test-Path $Snapshot)) {
    Write-Host "[ERROR] No snapshot found." -ForegroundColor Red; exit 1
}

# ---- Benchmark struct ----
$Results = @()

foreach ($model in $Models) {
    $shortName = $model -replace '[\\/:*?"<>|]', '_'
    $modelDir = Join-Path $LogRoot $shortName
    New-Item -ItemType Directory -Path $modelDir -Force | Out-Null
    $outJson = Join-Path $modelDir "result.json"

    Write-Host ""
    Write-Host "============================================================" -ForegroundColor Cyan
    Write-Host "  [$($Results.Count + 1)/$($Models.Count)]  Model: $model" -ForegroundColor White
    Write-Host "============================================================" -ForegroundColor Cyan
    Write-Host ""

    $modelStart = Get-Date

    $stderrLog = Join-Path $modelDir "stderr.log"
    & $Exe --debug --from-snapshot $Snapshot --top $Top --min-size-mb $MinMb --llm --llm-samples $Samples --llm-model $model > $outJson 2>$stderrLog

    $modelTime = [math]::Round(((Get-Date) - $modelStart).TotalSeconds, 1)
    $exitCode = $LASTEXITCODE

    Write-Host ""

    if ($exitCode -ne 0) {
        Write-Host "[FAIL] Exit code: $exitCode" -ForegroundColor Red
        $Results += [PSCustomObject]@{
            Model = $model
            Success = $false
            DurationSec = $modelTime
            ItemCount = 0
            AvgReqMs = 0
            SrtMs = 0
            PeakCwnd = 0
            Retries = 0
            WastePct = 0
            LatMinSec = 0
            LatAvgSec = 0
            LatMaxSec = 0
        }
        continue
    }

    # ---- Parse stderr for metrics ----
    $stderr = Get-Content $stderrLog -Raw -ErrorAction SilentlyContinue

    $items = 0
    $failed = 0
    $avgReqMs = 0
    $srtMs = 0
    $peakCwnd = 0
    $retries = 0
    $wastePct = 0
    $latMinSec = 0
    $latAvgSec = 0
    $latMaxSec = 0

    if ($stderr -match '(\d+)/(\d+)\s+succeeded,\s+(\d+)\s+failed') {
        $items = [int]$Matches[1]
    }
    if ($stderr -match 'Avg/req:\s+(\d+)\s*ms') {
        $avgReqMs = [int]$Matches[1]
    }
    if ($stderr -match 'SRTT:\s+(\d+)\s*ms') {
        $srtMs = [int]$Matches[1]
    }
    if ($stderr -match 'peak\s+cwnd:\s+(\d+)') {
        $peakCwnd = [int]$Matches[1]
    }
    if ($stderr -match 'requests=(\d+)\s+retries=(\d+)') {
        $retries = [int]$Matches[2]
    }
    if ($stderr -match 'wastage=(\d+)%') {
        $wastePct = [int]$Matches[1]
    }
    if ($stderr -match 'min=([\d.]+)s\s+avg=([\d.]+)s\s+max=([\d.]+)s') {
        $latMinSec = [double]$Matches[1]
        $latAvgSec = [double]$Matches[2]
        $latMaxSec = [double]$Matches[3]
    }

    Write-Host "[OK]  $items items, ${modelTime}s, avg/req ${avgReqMs}ms, SRTT ${srtMs}ms, peak cwnd=$peakCwnd" -ForegroundColor Green

    $Results += [PSCustomObject]@{
        Model = $model
        Success = $true
        DurationSec = $modelTime
        ItemCount = $items
        AvgReqMs = $avgReqMs
        SrtMs = $srtMs
        PeakCwnd = $peakCwnd
        Retries = $retries
        WastePct = $wastePct
        LatMinSec = $latMinSec
        LatAvgSec = $latAvgSec
        LatMaxSec = $latMaxSec
    }
}

# ---- Comparison Summary ----
Write-Host ""
Write-Host "======================================================================" -ForegroundColor Magenta
Write-Host "  BENCHMARK COMPLETE — Speed Comparison" -ForegroundColor Magenta
Write-Host "======================================================================" -ForegroundColor Magenta
Write-Host ""

$fmt = "{0,-55} {1,10} {2,10} {3,10} {4,10} {5,10} {6,9} {7,8}"
$fmtHeader = "{0,-55} {1,10} {2,10} {3,10} {4,10} {5,10} {6,9} {7,8}"
Write-Host ($fmtHeader -f "Model", "Time(s)", "AvgReq(ms)", "SRTT(ms)", "LatAvg(s)", "peakCwnd", "Retries", "Waste%") -ForegroundColor Yellow
Write-Host ($fmtHeader -f "-----", "-------", "----------", "--------", "---------", "--------", "-------", "------") -ForegroundColor DarkGray

$sorted = $Results | Sort-Object DurationSec
foreach ($r in $sorted) {
    $status = if ($r.Success) { "Green" } else { "Red" }
    $modelName = if ($r.Model.Length -gt 52) { $r.Model.Substring(0, 49) + "..." } else { $r.Model }
    Write-Host ($fmt -f $modelName, $r.DurationSec, $r.AvgReqMs, $r.SrtMs, $r.LatAvgSec, $r.PeakCwnd, $r.Retries, $r.WastePct) -ForegroundColor $status
}

Write-Host ""
Write-Host "Text quality comparison: see each model's result.json under $LogRoot/<model>/" -ForegroundColor Cyan
Write-Host ""

# ---- Text quality quick compare ----
Write-Host "======================================================================" -ForegroundColor Magenta
Write-Host "  OUTPUT SAMPLE — First 3 items per model" -ForegroundColor Magenta
Write-Host "======================================================================" -ForegroundColor Magenta

foreach ($model in $Models) {
    $shortName = $model -replace '[\\/:*?"<>|]', '_'
    $modelDir = Join-Path $LogRoot $shortName
    $outJson = Join-Path $modelDir "result.json"

    if (-not (Test-Path $outJson)) { continue }

    Write-Host ""
    Write-Host "--- $model ---" -ForegroundColor Cyan

    try {
        $items = Get-Content $outJson -Raw | ConvertFrom-Json
        $riskCounts = $items | Group-Object risk | ForEach-Object { "$($_.Name):$($_.Count)" }
        Write-Host "  Risk distribution: $($riskCounts -join ', ')" -ForegroundColor DarkGray

        $items | Select-Object -First 3 | ForEach-Object {
            $sz = if ($_.physical_size -ge 1MB) { "$([math]::Round($_.physical_size/1MB,1))MB" } elseif ($_.physical_size -ge 1KB) { "$([math]::Round($_.physical_size/1KB,1))KB" } else { "$($_.physical_size)B" }
            $cat = if ($_.category) { $_.category } else { "(none)" }
            $desc = if ($_.description) { $_.description } else { "(none)" }
            Write-Host "  [$($_.risk)] $sz  $cat  $($_.path)" -ForegroundColor DarkGray
            Write-Host "         desc: $desc" -ForegroundColor Gray
        }
    } catch {
        Write-Host "  [ERROR] Failed to parse: $_" -ForegroundColor Red
    }
}

$TotalTime = [math]::Round(((Get-Date) - $GlobalStart).TotalSeconds, 1)
Write-Host ""
Write-Host "======================================================================" -ForegroundColor Magenta
Write-Host "  Total elapsed: ${TotalTime}s" -ForegroundColor Magenta
Write-Host "  Results saved to: $LogRoot" -ForegroundColor Magenta
Write-Host "======================================================================" -ForegroundColor Magenta
Write-Host ""
