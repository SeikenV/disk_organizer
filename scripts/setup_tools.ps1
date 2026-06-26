# setup_tools.ps1 — fetch pinned llama.cpp binaries for disk_organizer.
#
# Lays out project-managed (gitignored) tools the engine resolves by default:
#
#   tools/llamacpp/cpu/llama-server.exe      (always)
#   tools/llamacpp/cuda/llama-server.exe     (-Cuda)
#   tools/llamacpp/vulkan/llama-server.exe   (-Vulkan)
#   tools/models/<model>.gguf                (-ModelSource or auto-detected)
#
# Backend dirs match enrich::backend::Backend::dir_name(). The engine takes
# --tools-dir tools/llamacpp and --llm-model-path tools/models/<model>.gguf.
#
# Usage:
#   ./scripts/setup_tools.ps1                       # CPU only
#   ./scripts/setup_tools.ps1 -Cuda                 # + CUDA 12.4 build + runtime
#   ./scripts/setup_tools.ps1 -Vulkan               # + Vulkan build
#   ./scripts/setup_tools.ps1 -ModelSource C:\path\model.gguf

[CmdletBinding()]
param(
    [string]$Version = "b9754",
    [switch]$Cuda,
    [switch]$Vulkan,
    [ValidateSet("12.4", "13.3")]
    [string]$CudaVer = "12.4",
    [string]$ModelSource,
    # -Video also fetches a pinned ffmpeg/ffprobe and the SmolVLM2-500M
    # vision model (for `--describe-video`). Off by default to keep CPU setup lean.
    [switch]$Video,
    [string]$FfmpegVersion = "8.0.1"
)

$ErrorActionPreference = "Stop"
$ScriptDir   = Split-Path -Parent $MyInvocation.MyCommand.Path
$ProjectRoot = Split-Path -Parent $ScriptDir
$ToolsDir    = Join-Path $ProjectRoot "tools"
$LlamaDir    = Join-Path $ToolsDir "llamacpp"
$ModelDir    = Join-Path $ToolsDir "models"
$BaseUrl     = "https://github.com/ggml-org/llama.cpp/releases/download/$Version"

Write-Host "=== disk_organizer tools setup (llama.cpp $Version) ===" -ForegroundColor Cyan

New-Item -ItemType Directory -Path $LlamaDir, $ModelDir -Force | Out-Null

# Download $zip from the release and merge its contents into tools/llamacpp/$backend
# so that llama-server.exe ends up directly under that backend dir.
function Install-Archive {
    param([string]$Backend, [string]$Zip)

    $dest = Join-Path $LlamaDir $Backend
    New-Item -ItemType Directory -Path $dest -Force | Out-Null
    $url = "$BaseUrl/$Zip"
    $tmp = Join-Path $env:TEMP "disk_org_$([System.IO.Path]::GetFileNameWithoutExtension($Zip))"
    $zipPath = Join-Path $env:TEMP $Zip

    Write-Host "  Downloading $Zip ..." -ForegroundColor Yellow
    try {
        Invoke-WebRequest -Uri $url -OutFile $zipPath -ErrorAction Stop
    } catch {
        Write-Host "  ERROR: download failed: $url" -ForegroundColor Red
        Write-Host "  Browse releases: https://github.com/ggml-org/llama.cpp/releases/tag/$Version" -ForegroundColor Yellow
        throw
    }

    if (Test-Path $tmp) { Remove-Item -Recurse -Force $tmp }
    Expand-Archive -Path $zipPath -DestinationPath $tmp -Force

    # The layout varies by release (flat, or build\bin\). Anchor on the dir that
    # actually holds llama-server.exe; for runtime-only zips (cudart) fall back
    # to the archive root so the DLLs still land next to the server.
    $srvr = Get-ChildItem -Path $tmp -Recurse -Filter "llama-server.exe" | Select-Object -First 1
    $srcDir = if ($srvr) { $srvr.Directory.FullName } else { $tmp }
    Copy-Item -Path (Join-Path $srcDir "*") -Destination $dest -Recurse -Force

    Remove-Item -Force $zipPath
    Remove-Item -Recurse -Force $tmp
}

# Download a version-pinned ffmpeg essentials build (gyan.dev) and copy
# ffmpeg.exe + ffprobe.exe into tools/ffmpeg/ (used by --describe-video).
function Install-Ffmpeg {
    $dest = Join-Path $ToolsDir "ffmpeg"
    New-Item -ItemType Directory -Path $dest -Force | Out-Null
    $zip = "ffmpeg-$FfmpegVersion-essentials_build.zip"
    $url = "https://www.gyan.dev/ffmpeg/builds/packages/$zip"
    $zipPath = Join-Path $env:TEMP $zip
    $tmp = Join-Path $env:TEMP "disk_org_ffmpeg"

    Write-Host "  Downloading $zip ..." -ForegroundColor Yellow
    try {
        Invoke-WebRequest -Uri $url -OutFile $zipPath -ErrorAction Stop
    } catch {
        Write-Host "  ERROR: download failed: $url" -ForegroundColor Red
        Write-Host "  Browse builds: https://www.gyan.dev/ffmpeg/builds/" -ForegroundColor Yellow
        throw
    }

    if (Test-Path $tmp) { Remove-Item -Recurse -Force $tmp }
    Expand-Archive -Path $zipPath -DestinationPath $tmp -Force
    $exe = Get-ChildItem -Path $tmp -Recurse -Filter "ffmpeg.exe" | Select-Object -First 1
    $bin = $exe.Directory.FullName
    Copy-Item -Path (Join-Path $bin "ffmpeg.exe")  -Destination $dest -Force
    Copy-Item -Path (Join-Path $bin "ffprobe.exe") -Destination $dest -Force

    Remove-Item -Force $zipPath
    Remove-Item -Recurse -Force $tmp
}

# Download a SmolVLM2 GGUF (and matching mmproj) into tools/models/ if absent.
function Install-VisionModel {
    $hf = "https://huggingface.co/ggml-org/SmolVLM2-500M-Video-Instruct-GGUF/resolve/main"
    foreach ($f in @(
        "SmolVLM2-500M-Video-Instruct-Q8_0.gguf",
        "mmproj-SmolVLM2-500M-Video-Instruct-Q8_0.gguf"
    )) {
        $d = Join-Path $ModelDir $f
        if (-not (Test-Path $d)) {
            Write-Host "  Downloading $f ..." -ForegroundColor Yellow
            Invoke-WebRequest -Uri "$hf/$f" -OutFile $d -ErrorAction Stop
        } else {
            Write-Host "  Vision model present: $f" -ForegroundColor Gray
        }
    }
}

# ---- CPU (always) ----
Write-Host "[cpu]" -ForegroundColor Cyan
Install-Archive -Backend "cpu" -Zip "llama-$Version-bin-win-cpu-x64.zip"

# ---- CUDA (optional): build + matching runtime DLLs into the same dir ----
if ($Cuda) {
    Write-Host "[cuda $CudaVer]" -ForegroundColor Cyan
    Install-Archive -Backend "cuda" -Zip "llama-$Version-bin-win-cuda-$CudaVer-x64.zip"
    Install-Archive -Backend "cuda" -Zip "cudart-llama-bin-win-cuda-$CudaVer-x64.zip"
}

# ---- Vulkan (optional) ----
if ($Vulkan) {
    Write-Host "[vulkan]" -ForegroundColor Cyan
    Install-Archive -Backend "vulkan" -Zip "llama-$Version-bin-win-vulkan-x64.zip"
}

# ---- Video stack (optional): ffmpeg + SmolVLM2 vision model ----
if ($Video) {
    Write-Host "[ffmpeg $FfmpegVersion]" -ForegroundColor Cyan
    Install-Ffmpeg
    Write-Host "[vision model]" -ForegroundColor Cyan
    Install-VisionModel
}

# ---- Model ----
# Prefer an explicit -ModelSource; otherwise auto-detect the known Downloads file.
$defaultModel = "Qwen3.5-0.8B-UD-Q4_K_XL.gguf"
if (-not $ModelSource) {
    $cand = Join-Path $env:USERPROFILE "Downloads\$defaultModel"
    if (Test-Path $cand) { $ModelSource = $cand }
}
if ($ModelSource -and (Test-Path $ModelSource)) {
    $modelDest = Join-Path $ModelDir (Split-Path -Leaf $ModelSource)
    if (-not (Test-Path $modelDest)) {
        Write-Host "Copying model -> $modelDest" -ForegroundColor Yellow
        Copy-Item -Path $ModelSource -Destination $modelDest -Force
    } else {
        Write-Host "Model already present: $modelDest" -ForegroundColor Gray
    }
} else {
    Write-Host "No model copied. Place a GGUF under $ModelDir or pass --llm-model-path." -ForegroundColor Yellow
}

# ---- Verify ----
Write-Host ""
Write-Host "Installed backends:" -ForegroundColor Cyan
foreach ($b in @("cpu", "cuda", "vulkan")) {
    $exe = Join-Path $LlamaDir "$b\llama-server.exe"
    if (Test-Path $exe) { Write-Host "  [ok]   $exe" -ForegroundColor Green }
}
$ff = Join-Path $ToolsDir "ffmpeg\ffmpeg.exe"
if (Test-Path $ff) { Write-Host "  [ok]   $ff" -ForegroundColor Green }
Write-Host ""
Write-Host "Done. Example runs:" -ForegroundColor Cyan
Write-Host "  cargo run -p disk_organizer -- C --llm --tools-dir tools/llamacpp --backend cpu --llm-model-path tools/models/$defaultModel" -ForegroundColor Gray
if (Test-Path $ff) {
    Write-Host "  cargo run -p disk_organizer -- --describe-video C:\path\to\video.mp4 --backend cpu" -ForegroundColor Gray
}
