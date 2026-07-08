# setup_llama_cpp.ps1  -  Set up llama.cpp for disk_organizer.
#
# Replaces the old setup_ollama.ps1. This script:
#   1. Detects llama-server.exe in PATH or common install locations.
#   2. If found outside PATH, adds its directory to the user PATH.
#   3. If not found, downloads a pinned llama.cpp release into the project
#      tools/llamacpp/ layout so engine defaults work.
#   4. Copies the text and vision GGUF models from Downloads into tools/models/.
#   5. Generates per-backend llama-server preset INI files.
#
# Usage:
#   .\scripts\setup_llama_cpp.ps1                 # install cpu + cuda + vulkan backends
#   .\scripts\setup_llama_cpp.ps1 -CpuOnly        # cpu only
#   .\scripts\setup_llama_cpp.ps1 -SkipCuda       # cpu + vulkan
#   .\scripts\setup_llama_cpp.ps1 -SkipVulkan     # cpu + cuda

[CmdletBinding()]
param(
    [string]$Version = "b9754",
    [switch]$CpuOnly,
    [switch]$SkipCuda,
    [switch]$SkipVulkan
)

$ErrorActionPreference = "Stop"

$ScriptDir   = Split-Path -Parent $MyInvocation.MyCommand.Path
$ProjectRoot = Split-Path -Parent $ScriptDir
$LlamaDir    = Join-Path $ProjectRoot "tools\llamacpp"
$ModelDir    = Join-Path $ProjectRoot "tools\models"
$BaseUrl     = "https://github.com/ggml-org/llama.cpp/releases/download/$Version"

# Models expected in Downloads
$Models = @(
    @{ Name = "Qwen3.5-0.8B text model";     File = "Qwen3.5-0.8B-UD-Q4_K_XL.gguf" },
    @{ Name = "SmolVLM2-500M vision model";   File = "SmolVLM2-500M-Video-Instruct-Q8_0.gguf" },
    @{ Name = "SmolVLM2 mmproj projector";    File = "mmproj-SmolVLM2-500M-Video-Instruct-Q8_0.gguf" }
)

Write-Host ""
Write-Host "==============================================" -ForegroundColor Cyan
Write-Host "  disk_organizer: llama.cpp Setup" -ForegroundColor Cyan
Write-Host "==============================================" -ForegroundColor Cyan
Write-Host ""

# ============================================================================
# Helpers
# ============================================================================

function Update-SessionPath {
    $machine = [Environment]::GetEnvironmentVariable("Path", "Machine")
    $user    = [Environment]::GetEnvironmentVariable("Path", "User")
    $env:Path = ($machine, $user, $env:Path) -join ";"
}

function Test-DirInPath {
    param([string]$Dir)
    $Dir = (Resolve-Path $Dir -ErrorAction SilentlyContinue).Path
    if (-not $Dir) { return $false }
    $machineParts = ([Environment]::GetEnvironmentVariable("Path", "Machine") -split ";") | ForEach-Object { $_.TrimEnd('\') }
    $userParts    = ([Environment]::GetEnvironmentVariable("Path", "User") -split ";")    | ForEach-Object { $_.TrimEnd('\') }
    return ($machineParts -contains $Dir) -or ($userParts -contains $Dir)
}

function Add-DirToUserPath {
    param([string]$Dir)
    $Dir = (Resolve-Path $Dir).Path
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $parts = $userPath -split ";" |
        ForEach-Object { $_.Trim() } |
        Where-Object { $_ -and ($_ -ne $Dir) }
    $newPath = ($parts + $Dir) -join ";"
    [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
    Update-SessionPath
    Write-Host "  [OK] Added to user PATH: $Dir" -ForegroundColor Green
}

function Find-LlamaServers {
    $found = @()

    # 1. PATH
    $cmd = Get-Command llama-server.exe -ErrorAction SilentlyContinue
    if ($cmd) {
        $found += [PSCustomObject]@{
            Backend = "path"
            Path    = $cmd.Source
            Dir     = Split-Path -Parent $cmd.Source
        }
    }

    # 2. Project-managed tools/llamacpp/<backend>/
    foreach ($b in @("cpu", "cuda", "vulkan")) {
        $p = Join-Path $LlamaDir "$b\llama-server.exe"
        if (Test-Path $p) {
            $found += [PSCustomObject]@{
                Backend = $b
                Path    = $p
                Dir     = Join-Path $LlamaDir $b
            }
        }
    }

    # 3. System install locations: %LOCALAPPDATA%\llama.cpp\<backend>\ and %PROGRAMFILES%\llama.cpp\<backend>\
    foreach ($base in @($env:LOCALAPPDATA, $env:PROGRAMFILES, ${env:ProgramFiles(x86)})) {
        if (-not $base) { continue }
        foreach ($b in @("cpu", "cuda", "vulkan")) {
            $p = Join-Path $base "llama.cpp\$b\llama-server.exe"
            if (Test-Path $p) {
                $found += [PSCustomObject]@{
                    Backend = $b
                    Path    = $p
                    Dir     = Join-Path $base "llama.cpp\$b"
                }
            }
        }
    }

    return $found
}

function Install-Backend {
    param([string]$Backend, [string]$Zip)
    $dest = Join-Path $LlamaDir $Backend
    New-Item -ItemType Directory -Path $dest -Force | Out-Null
    $url = "$BaseUrl/$Zip"
    $zipPath = Join-Path $env:TEMP $Zip
    $tmp = Join-Path $env:TEMP "disk_org_$([System.IO.Path]::GetFileNameWithoutExtension($Zip))"

    Write-Host "  [..] Downloading $Zip ..." -ForegroundColor Yellow
    try {
        Invoke-WebRequest -Uri $url -OutFile $zipPath -ErrorAction Stop
    } catch {
        Write-Host "  [ERROR] Download failed: $url" -ForegroundColor Red
        Write-Host "  Browse releases: https://github.com/ggml-org/llama.cpp/releases/tag/$Version" -ForegroundColor Yellow
        throw
    }

    if (Test-Path $tmp) { Remove-Item -Recurse -Force $tmp }
    Expand-Archive -Path $zipPath -DestinationPath $tmp -Force

    # The archive layout varies (flat or build\bin\). Anchor on the directory
    # that actually holds llama-server.exe; for runtime-only zips fall back to
    # the archive root so DLLs still land next to the server.
    $srvr = Get-ChildItem -Path $tmp -Recurse -Filter "llama-server.exe" | Select-Object -First 1
    $srcDir = if ($srvr) { $srvr.Directory.FullName } else { $tmp }
    Copy-Item -Path (Join-Path $srcDir "*") -Destination $dest -Recurse -Force

    Remove-Item -Force $zipPath
    Remove-Item -Recurse -Force $tmp
}

function Install-Model {
    param([string]$File)
    $src = Join-Path $env:USERPROFILE "Downloads\$File"
    $dst = Join-Path $ModelDir $File

    if (-not (Test-Path $src)) {
        Write-Host "  [WARN] Not found in Downloads: $File" -ForegroundColor Yellow
        return $false
    }

    if (Test-Path $dst) {
        $srcLen = (Get-Item $src).Length
        $dstLen = (Get-Item $dst).Length
        if ($srcLen -eq $dstLen) {
            Write-Host "  [OK] Already present: $File" -ForegroundColor Gray
            return $true
        }
        Write-Host "  [..] Size differs, re-copying: $File" -ForegroundColor Yellow
    } else {
        Write-Host "  [..] Copying $File ..." -ForegroundColor Yellow
    }

    Copy-Item -Path $src -Destination $dst -Force
    Write-Host "  [OK] Copied $File -> tools\models\" -ForegroundColor Green
    return $true
}

# ============================================================================
# Helper: detect NVIDIA GPUs via nvidia-smi.
# Returns an array of PSCustomObject with Index and Name, or $null if none.
# ============================================================================
function Get-NvidiaGpuInfo {
    $candidates = @(
        (Get-Command nvidia-smi.exe -ErrorAction SilentlyContinue).Source
        "$env:SystemRoot\System32\nvidia-smi.exe"
        "$env:SystemRoot\SysWOW64\nvidia-smi.exe"
    )
    $smi = $candidates | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
    if (-not $smi) { return $null }

    try {
        $output = & $smi --query-gpu=index,name --format=csv,noheader 2>$null
        if ($LASTEXITCODE -ne 0) { return $null }

        $gpus = @()
        foreach ($line in $output) {
            $parts = $line -split ","
            if ($parts.Count -ge 2) {
                $gpus += [PSCustomObject]@{
                    Index = [int]($parts[0].Trim())
                    Name  = $parts[1].Trim()
                }
            }
        }
        return $gpus
    } catch {
        return $null
    }
}

# ============================================================================
# Helper: write a llama-server --models-preset INI file.
# Uses nvidia-smi to populate device / main-gpu when available.
# ============================================================================
function Write-LlamaServerConfig {
    param(
        [string]$Path,
        [string]$TextModelPath,
        [string]$VisionModelPath,
        [string]$MmprojPath,
        [string]$Backend = "cuda"
    )

    $gpus = Get-NvidiaGpuInfo
    $hasNvidia = ($gpus -and $gpus.Count -gt 0)
    $gpuIndex = if ($hasNvidia) { $gpus[0].Index } else { 0 }
    $threads  = [Math]::Min([Environment]::ProcessorCount, 8)

    $lines = @()
    $lines += "; llama-server preset configuration for disk_organizer"
    $lines += "; Generated by scripts/setup_llama_cpp.ps1"
    $lines += ";"
    $lines += "; Usage with llama-server router mode:"
    $lines += ";   llama-server --models-preset $Path"
    $lines += ";"
    $lines += "; NOTE: The [*] section provides global defaults.  Each backend should"
    $lines += ";       use a separate INI because device is backend-specific:"
    $lines += ";         CUDA  builds accept device = CUDA<N>"
    $lines += ";         Vulkan builds accept device = Vulkan<N>"
    $lines += ";         CPU   builds do NOT accept device at all (will crash)"
    $lines += ";"
    $lines += "version = 1"
    $lines += ""
    $lines += "[*]"
    $lines += "port = 8080"
    $lines += "parallel = 4"
    $lines += "c = 16384"
    $lines += "threads = $threads"

    if ($Backend -eq "cuda") {
        if ($hasNvidia) {
            $lines += "n-gpu-layers = 99"
            $lines += "main-gpu = $gpuIndex"
            $lines += "device = CUDA$gpuIndex"
            $lines += "; Detected GPU: $($gpus[0].Name) (nvidia-smi index $gpuIndex)"
        } else {
            $lines += "n-gpu-layers = 0"
            $lines += "; No NVIDIA GPU detected via nvidia-smi; CPU-only defaults"
        }
    } elseif ($Backend -eq "vulkan") {
        $lines += "n-gpu-layers = 99"
        $lines += "device = Vulkan0"
        $lines += "; Vulkan device 0 (see llama-server --list-devices for full list)"
    } elseif ($Backend -eq "cpu") {
        $lines += "n-gpu-layers = 0"
        $lines += "; CPU-only build  -  no device or main-gpu keys"
    }

    $lines += ""
    $lines += "[disk_organizer_text]"
    $lines += "model = $TextModelPath"
    $lines += ""
    $lines += "[disk_organizer_vision]"
    $lines += "model = $VisionModelPath"
    $lines += "mmproj = $MmprojPath"

    Set-Content -Path $Path -Value ($lines -join "`r`n") -Encoding utf8 -NoNewline
}

# ============================================================================
# Step 1: Detect / install llama.cpp
# ============================================================================
Write-Host "[1/4] Detecting / installing llama.cpp backends ..." -ForegroundColor White
Update-SessionPath

# 1. Report any existing installations and ensure they're on PATH.
$servers = Find-LlamaServers
if ($servers.Count -gt 0) {
    Write-Host "  [OK] Found $($servers.Count) llama-server.exe instance(s):" -ForegroundColor Green
    foreach ($s in $servers) {
        Write-Host "       [$($s.Backend)] $($s.Path)" -ForegroundColor Gray
        if (-not (Test-DirInPath -Dir $s.Dir)) {
            Add-DirToUserPath -Dir $s.Dir
        } else {
            Write-Host "       already in PATH" -ForegroundColor DarkGray
        }
    }
}

# 2. Ensure desired backends exist under project tools/llamacpp/.
#    Default is all three so the daemon can pick the best one at runtime.
$desiredBackends = [System.Collections.ArrayList]@("cpu")
if (-not $SkipVulkan) { $desiredBackends.Add("vulkan") | Out-Null }
if (-not $SkipCuda)   { $desiredBackends.Add("cuda")   | Out-Null }
if ($CpuOnly)         { $desiredBackends = @("cpu") }

New-Item -ItemType Directory -Path $LlamaDir -Force | Out-Null

foreach ($b in $desiredBackends) {
    $backendExe = Join-Path $LlamaDir "$b\llama-server.exe"
    if (Test-Path $backendExe) {
        Write-Host "  [OK] Project backend present: $b" -ForegroundColor Gray
    } else {
        Write-Host "  [..] Downloading $b backend ..." -ForegroundColor Yellow
        switch ($b) {
            "cpu"    { Install-Backend -Backend "cpu"    -Zip "llama-$Version-bin-win-cpu-x64.zip" }
            "cuda"   {
                Install-Backend -Backend "cuda"   -Zip "llama-$Version-bin-win-cuda-12.4-x64.zip"
                Install-Backend -Backend "cuda"   -Zip "cudart-llama-bin-win-cuda-12.4-x64.zip"
            }
            "vulkan" { Install-Backend -Backend "vulkan" -Zip "llama-$Version-bin-win-vulkan-x64.zip" }
        }
    }

    $backendDir = Join-Path $LlamaDir $b
    if (-not (Test-DirInPath -Dir $backendDir)) {
        Add-DirToUserPath -Dir $backendDir
    }
}

Write-Host "  [OK] Desired backends: $($desiredBackends -join ', ')" -ForegroundColor Green

# ============================================================================
# Step 2: Load models from Downloads
# ============================================================================
Write-Host ""
Write-Host "[2/4] Loading models from Downloads ..." -ForegroundColor White
New-Item -ItemType Directory -Path $ModelDir -Force | Out-Null

$allOk = $true
foreach ($m in $Models) {
    $ok = Install-Model -File $m.File
    if (-not $ok) {
        Write-Host "  [ERROR] Could not load: $($m.Name)" -ForegroundColor Red
        $allOk = $false
    }
}

if (-not $allOk) {
    Write-Host ""
    Write-Host "  Some models are missing. Please place them in:" -ForegroundColor Yellow
    Write-Host "    $env:USERPROFILE\Downloads\" -ForegroundColor White
    Write-Host "  Expected files:" -ForegroundColor Yellow
    foreach ($m in $Models) {
        Write-Host "    $($m.File)" -ForegroundColor White
    }
}

# ============================================================================
# Step 3: Write per-backend llama-server preset INIs
# ============================================================================
Write-Host ""
Write-Host "[3/4] Writing per-backend llama-server preset INIs ..." -ForegroundColor White

$textModelPath   = Join-Path $ModelDir "Qwen3.5-0.8B-UD-Q4_K_XL.gguf"
$visionModelPath = Join-Path $ModelDir "SmolVLM2-500M-Video-Instruct-Q8_0.gguf"
$mmprojPath      = Join-Path $ModelDir "mmproj-SmolVLM2-500M-Video-Instruct-Q8_0.gguf"

if ((Test-Path $textModelPath) -and (Test-Path $visionModelPath) -and (Test-Path $mmprojPath)) {
    $gpus = Get-NvidiaGpuInfo
    $hasNvidia = ($gpus -and $gpus.Count -gt 0)

    # Always generate CPU INI (no device param)
    $cpuIni = Join-Path $LlamaDir "models-cpu.ini"
    Write-LlamaServerConfig `
        -Path $cpuIni `
        -TextModelPath (Resolve-Path $textModelPath).Path `
        -VisionModelPath (Resolve-Path $visionModelPath).Path `
        -MmprojPath (Resolve-Path $mmprojPath).Path `
        -Backend "cpu"
    Write-Host "  [OK] Wrote $cpuIni" -ForegroundColor Green

    # CUDA INI (only if we have NVIDIA GPU or CUDA backend installed)
    if ($hasNvidia -or (Test-Path (Join-Path $LlamaDir "cuda\llama-server.exe"))) {
        $cudaIni = Join-Path $LlamaDir "models-cuda.ini"
        Write-LlamaServerConfig `
            -Path $cudaIni `
            -TextModelPath (Resolve-Path $textModelPath).Path `
            -VisionModelPath (Resolve-Path $visionModelPath).Path `
            -MmprojPath (Resolve-Path $mmprojPath).Path `
            -Backend "cuda"
        if ($hasNvidia) {
            Write-Host "  [OK] Wrote $cudaIni (GPU: $($gpus[0].Name))" -ForegroundColor Green
        } else {
            Write-Host "  [OK] Wrote $cudaIni (no NVIDIA GPU detected; n-gpu-layers=0)" -ForegroundColor Green
        }
    }

    # Vulkan INI
    if (Test-Path (Join-Path $LlamaDir "vulkan\llama-server.exe")) {
        $vulkanIni = Join-Path $LlamaDir "models-vulkan.ini"
        Write-LlamaServerConfig `
            -Path $vulkanIni `
            -TextModelPath (Resolve-Path $textModelPath).Path `
            -VisionModelPath (Resolve-Path $visionModelPath).Path `
            -MmprojPath (Resolve-Path $mmprojPath).Path `
            -Backend "vulkan"
        Write-Host "  [OK] Wrote $vulkanIni" -ForegroundColor Green
    }
} else {
    Write-Host "  [WARN] Models not all present; skipping INI generation." -ForegroundColor Yellow
}

# ============================================================================
# Step 4: Verify
# ============================================================================
Write-Host ""
Write-Host "[4/4] Verify ..." -ForegroundColor White
Update-SessionPath
$cmd = Get-Command llama-server.exe -ErrorAction SilentlyContinue
if ($cmd) {
    Write-Host "  [OK] llama-server.exe is now on PATH: $($cmd.Source)" -ForegroundColor Green
} else {
    Write-Host "  [WARN] llama-server.exe still not on PATH. Restart your terminal." -ForegroundColor Yellow
}

if (Test-Path $textModelPath) {
    Write-Host "  [OK] Text model ready" -ForegroundColor Green
}
if ((Test-Path $visionModelPath) -and (Test-Path $mmprojPath)) {
    Write-Host "  [OK] Vision model + mmproj ready" -ForegroundColor Green
}
$iniFiles = Get-ChildItem -Path $LlamaDir -Filter "models-*.ini" | ForEach-Object { $_.Name }
if ($iniFiles) {
    Write-Host "  [OK] Preset INIs ready: $($iniFiles -join ', ')" -ForegroundColor Green
}

Write-Host ""
Write-Host "==============================================" -ForegroundColor Cyan
Write-Host "  Setup complete!" -ForegroundColor Cyan
Write-Host "  Run: cargo run -p disk_organizer -- C --llm" -ForegroundColor White
Write-Host "==============================================" -ForegroundColor Cyan
