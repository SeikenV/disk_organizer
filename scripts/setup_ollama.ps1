# Setup Ollama for disk_organizer
# One-time script: installs dependencies, downloads the model, verifies it works.
#
# Usage:
#   Double-click setup_ollama.bat
#   powershell -File setup_ollama.ps1 -Model qwen3.5:0.8b -Quant q5_k_m
#
# Download strategy:
#   aria2:   curl.exe via gh.idayer.com proxy (2.4 MB, ~3s)   ← Verified OK
#   Ollama:  irm https://ollama.com/install.ps1 | iex          ← Official, Cloudflare CDN
#   Model:   aria2c via hf-mirror + modelscope (1.1 GB)       ← Non-GitHub CDN

param(
    [string]$Model = "qwen3.5:0.8b",
    [string]$Quant = "q4_k_m"
)

$ErrorActionPreference = "Continue"
$OllamaUrl = "http://localhost:11434"
$TempDir   = Join-Path $env:TEMP "ollama_import"
$Proxy     = "https://gh.idayer.com"

# Model GGUF mirrors (non-GitHub CDN, fast in China)
$GgufFile   = "qwen3.5-0.8b-instruct-$Quant.gguf"
$HfRepo     = "Qwen/Qwen3.5-0.8B-Instruct-GGUF"
$GgufMirrors = @(
    "https://hf-mirror.com/$HfRepo/resolve/main/$GgufFile",
    "https://modelscope.cn/models/$HfRepo/resolve/master/$GgufFile",
    "https://huggingface.co/$HfRepo/resolve/main/$GgufFile"
)

# ============================================================================
Write-Host ""
Write-Host "==============================================" -ForegroundColor Cyan
Write-Host "  disk_organizer: Ollama Setup" -ForegroundColor Cyan
Write-Host "  Model: $Model   |   Quant: $Quant" -ForegroundColor DarkGray
Write-Host "==============================================" -ForegroundColor Cyan
Write-Host ""

# ============================================================================
# Helper: download a file with curl.exe (Windows built-in)
# ============================================================================
function Invoke-CurlDownload {
    param([string]$Url, [string]$OutPath, [int]$TimeoutSec = 120)
    $curl = Get-Command curl.exe -ErrorAction SilentlyContinue
    if (-not $curl) {
        try {
            Invoke-WebRequest -Uri $Url -OutFile $OutPath -UseBasicParsing -TimeoutSec $TimeoutSec
            return (Test-Path $OutPath) -and ((Get-Item $OutPath).Length -gt 1024)
        } catch { return $false }
    }
    & curl.exe -L -o $OutPath --connect-timeout 10 --max-time $TimeoutSec `
        --retry 3 --retry-delay 5 -C - -sS $Url 2>&1 | Out-Null
    return ($LASTEXITCODE -eq 0) -and (Test-Path $OutPath) -and ((Get-Item $OutPath).Length -gt 1024)
}

# ============================================================================
# Helper: download with aria2 (multi-source, multi-thread)
# ============================================================================
function Invoke-AriaDownload {
    param([string[]]$Urls, [string]$OutDir, [string]$OutFile)
    $outPath = Join-Path $OutDir $OutFile
    $aria2c  = Get-Command aria2c -ErrorAction SilentlyContinue
    if ($aria2c) {
        Write-Host "    aria2c: $($Urls.Count) sources, 16x parallel" -ForegroundColor DarkGray
        $urlArgs = $Urls | ForEach-Object { "`"$_`"" }
        & aria2c --console-log-level=warn --max-connection-per-server=8 --split=16 `
            --min-split-size=1M --continue=true --max-tries=5 --retry-wait=3 `
            --check-certificate=false --dir=$OutDir --out=$OutFile $urlArgs
        if ($LASTEXITCODE -eq 0 -and (Test-Path $outPath)) { return $true }
    }
    foreach ($url in $Urls) {
        Write-Host "    curl: $(([uri]$url).Host)" -ForegroundColor DarkGray
        if (Invoke-CurlDownload -Url $url -OutPath $outPath -TimeoutSec 600) { return $true }
    }
    return $false
}

# ============================================================================
# Step 1: aria2 (small, fast via proxy)
# ============================================================================
Write-Host "[1/4] aria2" -ForegroundColor White
$Aria2Dir = Join-Path "$env:LOCALAPPDATA" "aria2"
$Aria2Exe  = Join-Path $Aria2Dir "aria2c.exe"

$found = Get-Command aria2c -ErrorAction SilentlyContinue
if (-not $found -and (Test-Path $Aria2Exe)) {
    $env:Path = "$Aria2Dir;$env:Path"
    $found = Get-Command aria2c -ErrorAction SilentlyContinue
}

if ($found) {
    Write-Host "  [OK] Already installed" -ForegroundColor Green
} else {
    Write-Host "  [..] Downloading via proxy (~2.4 MB)..." -ForegroundColor Yellow
    $zipFile = "aria2-1.37.0-win-64bit-build1.zip"
    $zipUrl  = "$Proxy/https://github.com/aria2/aria2/releases/download/release-1.37.0/$zipFile"
    $zipPath = Join-Path (New-Item -Force -ItemType Directory -Path $TempDir) $zipFile

    if (Invoke-CurlDownload -Url $zipUrl -OutPath $zipPath -TimeoutSec 30) {
        $extract = Join-Path $TempDir "aria2_extract"
        Expand-Archive -Path $zipPath -DestinationPath $extract -Force
        $bin = Get-ChildItem $extract -Recurse -Filter "aria2c.exe" | Select-Object -First 1
        if ($bin) {
            New-Item -Force -ItemType Directory -Path $Aria2Dir | Out-Null
            Copy-Item $bin.FullName $Aria2Dir -Force
            $up = [Environment]::GetEnvironmentVariable("Path","User")
            if ($up -notlike "*$Aria2Dir*") {
                [Environment]::SetEnvironmentVariable("Path","$up;$Aria2Dir","User")
            }
            $env:Path = "$Aria2Dir;$env:Path"
            Write-Host "  [OK] Installed to $Aria2Dir" -ForegroundColor Green
        }
        Remove-Item -Recurse -Force $extract, $zipPath -ErrorAction SilentlyContinue
    } else {
        Write-Host "  [WARN] Download failed — no parallel download available" -ForegroundColor Yellow
    }
}

# ============================================================================
# Helper: refresh PATH from machine + user registry (so newly installed
# programs are findable in the current session).
# ============================================================================
function Update-SessionPath {
    $machine = [Environment]::GetEnvironmentVariable("Path", "Machine")
    $user    = [Environment]::GetEnvironmentVariable("Path", "User")
    $env:Path = ($machine, $user, $env:Path) -join ";"
}

# ============================================================================
# Helper: locate ollama.exe on disk, checking PATH and common install dirs.
# ============================================================================
function Find-OllamaExe {
    $cmd = Get-Command ollama -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }

    $hints = @(
        "$env:LOCALAPPDATA\Programs\Ollama\ollama.exe",
        "$env:PROGRAMFILES\Ollama\ollama.exe",
        "${env:ProgramFiles(x86)}\Ollama\ollama.exe"
    )
    foreach ($p in $hints) {
        if (Test-Path $p) { return $p }
    }
    return $null
}

# ============================================================================
# Step 2: Ollama — official install script (irm https://ollama.com/install.ps1 | iex)
# ============================================================================
Write-Host ""
Write-Host "[2/4] Ollama" -ForegroundColor White

$ollamaExe = Find-OllamaExe

if ($ollamaExe) {
    Write-Host "  [OK] Already installed: $ollamaExe" -ForegroundColor Green
} else {
    try {
        # Use winget with --disable-interactivity to skip ALL prompts
        # (Y/A/N/H source agreements, package agreements, etc.)
        $winget = Get-Command winget.exe -ErrorAction SilentlyContinue
        if ($winget) {
            Write-Host "  [..] winget install Ollama.Ollama (--disable-interactivity)..." -ForegroundColor Yellow
            Write-Host "       $winget" -ForegroundColor DarkGray
            & $winget install Ollama.Ollama `
                --accept-source-agreements `
                --accept-package-agreements `
                --disable-interactivity
            # winget may return non-zero for "already installed, no upgrade" —
            # check the actual result instead of relying on exit code.
            Update-SessionPath
            $ollamaExe = Find-OllamaExe
            if ($ollamaExe) {
                Write-Host "  [OK] Ollama installed: $ollamaExe" -ForegroundColor Green
            }
        }

        # Fallback: download official installer directly (no winget dependency)
        if (-not $ollamaExe) {
            Write-Host "  [..] Downloading official installer..." -ForegroundColor Yellow
            $setupUrl  = "https://ollama.com/download/OllamaSetup.exe"
            $setupPath = Join-Path $TempDir "OllamaSetup.exe"
            if (Invoke-CurlDownload -Url $setupUrl -OutPath $setupPath -TimeoutSec 120) {
                Write-Host "  [..] Running installer (/S = silent)..." -ForegroundColor Yellow
                $psi = New-Object System.Diagnostics.ProcessStartInfo
                $psi.FileName = $setupPath
                $psi.Arguments = "/S"
                $psi.UseShellExecute = $true
                $psi.WindowStyle = [System.Diagnostics.ProcessWindowStyle]::Hidden
                $proc = [System.Diagnostics.Process]::Start($psi)
                $proc.WaitForExit(120000) | Out-Null
                Remove-Item $setupPath -Force -ErrorAction SilentlyContinue
                Update-SessionPath
                Start-Sleep -Seconds 3
                $ollamaExe = Find-OllamaExe
                if ($ollamaExe) {
                    Write-Host "  [OK] Ollama installed: $ollamaExe" -ForegroundColor Green
                }
            }
        }

        if (-not $ollamaExe) {
            Write-Host ""
            Write-Host "  ========================================" -ForegroundColor Red
            Write-Host "  Ollama could not be installed automatically." -ForegroundColor Red
            Write-Host "  Please install manually:" -ForegroundColor Yellow
            Write-Host "    1. Open: https://ollama.com/download/windows" -ForegroundColor White
            Write-Host "    2. Download and run OllamaSetup.exe" -ForegroundColor White
            Write-Host "    3. Re-run this script" -ForegroundColor White
            Write-Host "  ========================================" -ForegroundColor Red
            Write-Host ""
            exit 1
        }
    } catch {
        Write-Host "  [ERROR] Install failed: $_" -ForegroundColor Red
        exit 1
    }
}

# ============================================================================
# Step 3: Start Ollama server
# ============================================================================
Write-Host ""
Write-Host "[3/4] Start Ollama server" -ForegroundColor White

# Make sure PATH is current (ollama may have been installed just above).
Update-SessionPath

function Wait-OllamaApi {
    param([int]$MaxSec = 60)
    Write-Host "       Waiting for API" -NoNewline -ForegroundColor DarkGray
    for ($i = 0; $i -lt $MaxSec; $i++) {
        Start-Sleep -Seconds 1
        Write-Host "." -NoNewline -ForegroundColor DarkGray
        try {
            $null = Invoke-WebRequest "$OllamaUrl/api/tags" -TimeoutSec 2 -ErrorAction Stop
            Write-Host ""
            return $true
        } catch {}
    }
    Write-Host ""
    return $false
}

try {
    $null = Invoke-WebRequest -Uri "$OllamaUrl/api/tags" -TimeoutSec 3 -ErrorAction Stop
    Write-Host "  [OK] Already running" -ForegroundColor Green
} catch {
    Write-Host "  [..] Starting..." -ForegroundColor Yellow

    $ollamaPath = Find-OllamaExe
    if (-not $ollamaPath) {
        Write-Host "  [ERROR] Cannot find ollama.exe on disk." -ForegroundColor Red
        exit 1
    }

    # Enable iGPU (AMD 780M via Vulkan) so ollama doesn't drop it.
    $env:OLLAMA_IGPU_ENABLE = "1"
    [Environment]::SetEnvironmentVariable("OLLAMA_IGPU_ENABLE", "1", "User")

    # Kill any stale ollama processes first (they may be hung).
    $stale = Get-Process -Name ollama -ErrorAction SilentlyContinue
    if ($stale) {
        Write-Host "       Stopping stale ollama process..." -ForegroundColor DarkGray
        $stale | Stop-Process -Force -ErrorAction SilentlyContinue
        Start-Sleep -Seconds 2
    }

    # ------------------------------------------------------------------
    # Attempt 1: "ollama list" — on Windows this triggers the service
    # just like double-clicking the icon.  Returns immediately, does not
    # block on console handles.
    # ------------------------------------------------------------------
    Write-Host "       $ollamaPath list" -ForegroundColor DarkGray
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $ollamaPath
    $psi.Arguments = "list"
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.EnvironmentVariables["OLLAMA_IGPU_ENABLE"] = "1"
    $proc = [System.Diagnostics.Process]::Start($psi)
    $proc.WaitForExit(15000) | Out-Null   # give it up to 15 s to trigger service

    # GPU discovery can take 10+ s, so wait longer.
    if (Wait-OllamaApi -MaxSec 90) {
        Write-Host "  [OK] Server started" -ForegroundColor Green
    } else {
        # -----------------------------------------------------------------
        # Attempt 2: ollama serve (use ShellExecute to avoid console hang).
        # -----------------------------------------------------------------
        Write-Host "  [..] list didn't trigger; trying serve..." -ForegroundColor Yellow
        Write-Host "       $ollamaPath serve" -ForegroundColor DarkGray

        $psi2 = New-Object System.Diagnostics.ProcessStartInfo
        $psi2.FileName = $ollamaPath
        $psi2.Arguments = "serve"
        $psi2.UseShellExecute = $true       # fully detaches, no console inheritance
        $psi2.WindowStyle = [System.Diagnostics.ProcessWindowStyle]::Hidden
        $psi2.EnvironmentVariables["OLLAMA_IGPU_ENABLE"] = "1"
        $proc2 = [System.Diagnostics.Process]::Start($psi2)

        if (Wait-OllamaApi -MaxSec 90) {
            Write-Host "  [OK] Server started (serve mode)" -ForegroundColor Green
        } else {
            $alive = Get-Process -Name ollama -ErrorAction SilentlyContinue
            if ($alive) { $alive | Stop-Process -Force -ErrorAction SilentlyContinue }
            Write-Host "  [ERROR] Ollama failed to start." -ForegroundColor Red
            Write-Host ""
            Write-Host "  Please start Ollama manually:" -ForegroundColor Yellow
            Write-Host "    - Double-click Ollama in the Start Menu" -ForegroundColor White
            Write-Host "    - Or run: ollama list" -ForegroundColor White
            Write-Host "    - Then re-run this script" -ForegroundColor White
            exit 1
        }
    }
}

# ============================================================================
# Step 4: Download model
# ============================================================================
Write-Host ""
Write-Host "[4/4] Model: $Model" -ForegroundColor White

$existing = (Invoke-RestMethod "$OllamaUrl/api/tags").models.name
if ($existing -contains $Model) {
    Write-Host "  [OK] Already downloaded" -ForegroundColor Green
} else {
    # -----------------------------------------------------------------
    # Attempt 1: ollama pull (simple, official registry).
    # -----------------------------------------------------------------
    Write-Host "  [..] ollama pull $Model" -ForegroundColor Yellow
    Write-Host "       (this may take a few minutes on first download)" -ForegroundColor DarkGray
    ollama pull $Model
    if ($LASTEXITCODE -eq 0) {
        Write-Host "  [OK] Model pulled" -ForegroundColor Green
    } else {
        # -----------------------------------------------------------------
        # Attempt 2: GGUF mirror download + ollama create import.
        # Faster in China via hf-mirror / modelscope CDN.
        # -----------------------------------------------------------------
        Write-Host "  [WARN] ollama pull failed, trying mirror download..." -ForegroundColor Yellow
        Write-Host "         $GgufFile" -ForegroundColor DarkGray

        $ok = $false
        $ggufPath = Join-Path $TempDir $GgufFile

        if (Invoke-AriaDownload -Urls $GgufMirrors -OutDir $TempDir -OutFile $GgufFile) {
            $mb = [math]::Round((Get-Item $ggufPath).Length / 1MB, 1)
            Write-Host "  [OK] Downloaded: $mb MB" -ForegroundColor Green
            $ok = $true
        }

        if ($ok) {
            Write-Host "  [..] Importing into Ollama..." -ForegroundColor Yellow
            $mf = Join-Path $TempDir "Modelfile.txt"
            Set-Content -Path $mf -Encoding utf8 -Value @"
FROM "$ggufPath"

TEMPLATE """{{ if .System }}<|im_start|>system
{{ .System }}<|im_end|>
{{ end }}{{ if .Prompt }}<|im_start|>user
{{ .Prompt }}<|im_end|>
{{ end }}<|im_start|>assistant
"""
PARAMETER stop "<|im_start|>"
PARAMETER stop "<|im_end|>"
PARAMETER temperature 0.1
PARAMETER top_p 0.9
"@
            ollama create $Model -f $mf
            if ($LASTEXITCODE -eq 0) {
                Write-Host "  [OK] Model imported" -ForegroundColor Green
            } else {
                Write-Host "  [ERROR] Import failed." -ForegroundColor Red
                Write-Host "  Try manually: ollama pull $Model" -ForegroundColor Yellow
                exit 1
            }
            Remove-Item $ggufPath, $mf -Force -ErrorAction SilentlyContinue
        } else {
            Write-Host "  [ERROR] All download methods failed." -ForegroundColor Red
            Write-Host "  Try manually: ollama pull $Model" -ForegroundColor Yellow
            exit 1
        }
    }
}

Write-Host ""
Write-Host "==============================================" -ForegroundColor Cyan
Write-Host "  Setup complete!" -ForegroundColor Cyan
Write-Host "  Run: disk_organizer C --llm" -ForegroundColor White
Write-Host "==============================================" -ForegroundColor Cyan
Write-Host ""
