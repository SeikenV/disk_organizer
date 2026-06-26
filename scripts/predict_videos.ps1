# predict_videos.ps1 — predict the content of every video in a disk_organizer
# enrichment result, then merge the vision guess next to the text classification.
#
# The engine does the heavy lifting in ONE process: `--describe-videos-from`
# filters the video files out of the item JSON and describes them all against a
# single llama-server (one model load), printing a JSON array of guesses. This
# script just runs that, joins each guess back to its text fields by path, and
# writes a combined report.
#
# Parameters:
#   -EnrichResult  INPUT  (required): path to an existing enrichment-result JSON
#                  — the engine's item array. You produce this FIRST; the script
#                  only READS it. (Alias: -Results.)
#   -Out           OUTPUT (optional): report path the script CREATES. Defaults to
#                  predictions_<timestamp>.json in the project root.
#
# Usage:
#   # 1. Produce the enrichment result (the engine writes this file):
#   target\release\disk_organizer.exe C --llm --backend cpu | Out-File -Encoding utf8 result.json
#   # 2. Predict the videos found IN that file:
#   ./scripts/predict_videos.ps1 -EnrichResult result.json
#   ./scripts/predict_videos.ps1 -EnrichResult result.json -ExtraArgs '--vlm-model-path','tools/models/SmolVLM2-2.2B-...gguf'

[CmdletBinding()]
param(
    [Parameter(Mandatory)] [Alias('Results')] [string]$EnrichResult,
    [string]$Backend = "cpu",
    [string]$Out,
    [string[]]$ExtraArgs = @()
)

$ErrorActionPreference = "Stop"
$ScriptDir   = Split-Path -Parent $MyInvocation.MyCommand.Path
$ProjectRoot = Split-Path -Parent $ScriptDir
$exe = Join-Path $ProjectRoot "target\release\disk_organizer.exe"

if (-not (Test-Path $exe)) {
    Write-Host "Building release binary ..." -ForegroundColor Yellow
    & cargo build -p disk_organizer --release
}
if (-not (Test-Path $EnrichResult)) { throw "input enrichment-result file not found: $EnrichResult (you must generate it first; see script header)" }

# Text classification per path (for the join).
$textByPath = @{}
foreach ($it in (Get-Content $EnrichResult -Raw | ConvertFrom-Json)) {
    $textByPath[[string]$it.path] = $it
}

# One engine call: describe every video in the result against one llama-server.
Write-Host "Predicting video contents (backend=$Backend, one shared server) ..." -ForegroundColor Cyan
$visionJson = & $exe --describe-videos-from $EnrichResult --backend $Backend @ExtraArgs 2>$null
if ($LASTEXITCODE -ne 0 -or -not $visionJson) {
    throw "describe-videos-from failed (exit $LASTEXITCODE)"
}
$vision = @(($visionJson | Out-String) | ConvertFrom-Json)
if ($vision.Count -eq 0) {
    Write-Host "No video files found in $Results." -ForegroundColor Yellow
    return
}

# Merge vision guess with the text classification by path.
$report = foreach ($g in $vision) {
    $t = $textByPath[[string]$g.path]
    [pscustomobject][ordered]@{
        path              = $g.path
        size_mb           = if ($t -and $t.physical_size) { [math]::Round($t.physical_size / 1MB, 1) } else { $null }
        text_category     = if ($t) { $t.category } else { $null }
        text_risk         = if ($t) { $t.risk } else { $null }
        vision_summary    = $g.summary
        vision_category   = $g.category
        vision_confidence = $g.confidence
        error             = $g.error
    }
}

if (-not $Out) {
    $Out = Join-Path $ProjectRoot ("predictions_{0}.json" -f (Get-Date -Format "yyyy-MM-dd_HH-mm-ss"))
}
$report | ConvertTo-Json -Depth 5 | Set-Content -Path $Out -Encoding UTF8

Write-Host ""
Write-Host "=== Video content predictions ($($report.Count)) ===" -ForegroundColor Cyan
$report | Format-Table -AutoSize -Wrap path, size_mb, vision_category, vision_confidence, vision_summary
Write-Host "Full report: $Out" -ForegroundColor Green
