# Setup iGPU inference backend for disk_organizer dual-backend mode.
# This script downloads llama.cpp server and the Qwen3.5 0.8B GGUF model.

$ErrorActionPreference = "Stop"
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$ProjectRoot = Split-Path -Parent $ScriptDir
$ToolsDir = Join-Path $ProjectRoot "tools"
$LlamaDir = Join-Path $ToolsDir "llama.cpp"
$ModelDir = Join-Path $LlamaDir "models"

# ---- Config ----
$LlamaVersion = "b4927"  # check https://github.com/ggerganov/llama.cpp/releases
$LlamaZip = "llama-$LlamaVersion-bin-win-vulkan-x64.zip"  
$LlamaUrl = "https://github.com/ggerganov/llama.cpp/releases/download/$LlamaVersion/$LlamaZip"

$ModelFile = "qwen3.5-0.8b-instruct-q4_k_m.gguf"
$ModelUrl = "https://huggingface.co/bartowski/Qwen3.5-0.8B-Instruct-GGUF/resolve/main/$ModelFile"

$ServerPort = 8080

Write-Host "=== disk_organizer iGPU backend setup ===" -ForegroundColor Cyan

# ---- Ensure directories ----
if (-not (Test-Path $LlamaDir)) { New-Item -ItemType Directory -Path $LlamaDir -Force | Out-Null }
if (-not (Test-Path $ModelDir)) { New-Item -ItemType Directory -Path $ModelDir -Force | Out-Null }

# ---- Download llama.cpp ----
$LlamaExe = Join-Path $LlamaDir "llama-server.exe"
if (-not (Test-Path $LlamaExe)) {
    $ZipPath = Join-Path $LlamaDir $LlamaZip
    Write-Host "[1/2] Downloading llama.cpp ($LlamaVersion Vulkan)..." -ForegroundColor Yellow
    try {
        Invoke-WebRequest -Uri $LlamaUrl -OutFile $ZipPath -ErrorAction Stop
    } catch {
        Write-Host "  ERROR: Failed to download llama.cpp." -ForegroundColor Red
        Write-Host "  Manual download: https://github.com/ggerganov/llama.cpp/releases" -ForegroundColor Yellow
        Write-Host "  Download the Vulkan .zip, extract to: $LlamaDir" -ForegroundColor Yellow
        exit 1
    }
    Write-Host "  Extracting..." -ForegroundColor Gray
    Expand-Archive -Path $ZipPath -DestinationPath $LlamaDir -Force
    # The zip may have a subdirectory; find and move llama-server.exe
    $found = Get-ChildItem -Path $LlamaDir -Recurse -Filter "llama-server.exe" | Select-Object -First 1
    if ($found) {
        Move-Item -Path $found.FullName -Destination $LlamaExe -Force
    }
    Write-Host "  llama-server.exe ready." -ForegroundColor Green
} else {
    Write-Host "[1/2] llama-server.exe found, skipping download." -ForegroundColor Gray
}

# ---- Download GGUF model ----
$ModelPath = Join-Path $ModelDir $ModelFile
if (-not (Test-Path $ModelPath)) {
    Write-Host "[2/2] Downloading $ModelFile (~0.6 GB)..." -ForegroundColor Yellow
    Write-Host "  This may take a few minutes..." -ForegroundColor Gray
    try {
        Invoke-WebRequest -Uri $ModelUrl -OutFile $ModelPath -ErrorAction Stop
        Write-Host "  Model downloaded: $ModelPath" -ForegroundColor Green
    } catch {
        Write-Host "  ERROR: Failed to download model." -ForegroundColor Red
        Write-Host "  Manual download: $ModelUrl" -ForegroundColor Yellow
        Write-Host "  Place it at: $ModelPath" -ForegroundColor Yellow
        exit 1
    }
} else {
    Write-Host "[2/2] Model found, skipping download." -ForegroundColor Gray
}

# ---- Start llama-server (Vulkan / iGPU) ----
Write-Host ""
Write-Host "=== Starting iGPU inference server ===" -ForegroundColor Cyan
Write-Host "  Model     : $ModelPath" -ForegroundColor White
Write-Host "  Backend   : Vulkan (iGPU)" -ForegroundColor White
Write-Host "  Port      : $ServerPort" -ForegroundColor White
Write-Host "  API       : http://localhost:$ServerPort/v1/chat/completions" -ForegroundColor White
Write-Host ""

$Args = @(
    "-m", $ModelPath,
    "-ngl", "99",           # offload ALL layers to GPU (iGPU via Vulkan)
    "--host", "0.0.0.0",
    "--port", "$ServerPort",
    "--ctx-size", "4096",
    "--batch-size", "512",
    "--parallel", "16",     # 0.8B model is light, iGPU can batch more
    "--no-mmap"             # avoid memory-mapped I/O latency on Windows
)

Write-Host "Running: llama-server.exe $($Args -join ' ')" -ForegroundColor Gray
Write-Host ""
Write-Host "Then start disk_organizer with:" -ForegroundColor Cyan
Write-Host "  cargo run --release -- C --top 1000 --min-size-mb 100 --llm --llm-igpu-endpoint http://localhost:8080 --llm-igpu-weight 0.3"
Write-Host ""

& $LlamaExe @Args
