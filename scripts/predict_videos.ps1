# predict_videos.ps1 — pull every video out of a disk_organizer enrichment
# result and predict its content with the SmolVLM2 vision model.
#
# Takes the JSON item array the engine prints (text-enrichment result), keeps
# the video files, and runs `--describe-video` on each, merging the vision
# guess (summary/category/confidence) next to the text classification.
#
# Each video spins up its own llama-server (model reload per file) — fine for a
# test/validation harness; slow for large batches.
#
# Usage:
#   # 1. Produce an enrichment result (stdout is the item JSON array):
#   target\release\disk_organizer.exe C --llm --backend cpu > result.json
#   # 2. Predict the videos found in it:
#   ./scripts/predict_videos.ps1 -Results result.json
#   ./scripts/predict_videos.ps1 -Results result.json -Backend cpu -ExtraArgs '--vlm-model-path','tools/models/SmolVLM2-2.2B-...gguf'

[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string]$Results,
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
if (-not (Test-Path $Results)) { throw "results file not found: $Results" }

$videoExt = @('.mp4','.mkv','.mov','.avi','.webm','.m4v','.flv','.wmv','.mpg','.mpeg','.ts','.m2ts')
$items = Get-Content $Results -Raw | ConvertFrom-Json
$videos = @($items | Where-Object {
    -not $_.is_dir -and ($videoExt -contains ([IO.Path]::GetExtension([string]$_.path)).ToLower())
})

if ($videos.Count -eq 0) {
    Write-Host "No video files found in $Results." -ForegroundColor Yellow
    return
}
Write-Host "Found $($videos.Count) video file(s). Predicting content (backend=$Backend) ..." -ForegroundColor Cyan

$report = @()
$i = 0
foreach ($v in $videos) {
    $i++
    $path = [string]$v.path
    $sizeMB = if ($v.physical_size) { [math]::Round($v.physical_size / 1MB, 1) } else { 0 }
    Write-Host ("[{0}/{1}] {2} ({3} MB)" -f $i, $videos.Count, $path, $sizeMB) -ForegroundColor Gray

    $entry = [ordered]@{
        path          = $path
        size_mb       = $sizeMB
        text_category = $v.category
        text_risk     = $v.risk
    }
    if (-not (Test-Path $path)) {
        $entry.error = "file no longer exists"
        $report += [pscustomobject]$entry
        continue
    }
    try {
        $json = & $exe --describe-video $path --backend $Backend @ExtraArgs 2>$null
        if ($LASTEXITCODE -ne 0 -or -not $json) {
            $entry.error = "describe-video failed (exit $LASTEXITCODE)"
        } else {
            $guess = ($json | Out-String) | ConvertFrom-Json
            $entry.vision_summary    = $guess.summary
            $entry.vision_category   = $guess.category
            $entry.vision_confidence = $guess.confidence
        }
    } catch {
        $entry.error = $_.Exception.Message
    }
    $report += [pscustomobject]$entry
}

if (-not $Out) {
    $Out = Join-Path $ProjectRoot ("predictions_{0}.json" -f (Get-Date -Format "yyyy-MM-dd_HH-mm-ss"))
}
$report | ConvertTo-Json -Depth 5 | Set-Content -Path $Out -Encoding UTF8

Write-Host ""
Write-Host "=== Video content predictions ===" -ForegroundColor Cyan
$report | Format-Table -AutoSize -Wrap path, size_mb, vision_category, vision_confidence, vision_summary
Write-Host "Full report: $Out" -ForegroundColor Green
