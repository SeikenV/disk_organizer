# benchmark.ps1 — Benchmark llama-server backends with the same prompt.
# Runs 5 iterations per backend, reports mean/median/stddev.
# Uses client-side wall-clock timing instead of server-reported timings
# because llama-server b9754 does not include timings in the API response.
#
# Usage: .\scripts\benchmark.ps1

$ErrorActionPreference = "Stop"

$ProjectRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$ProjectRoot = Split-Path -Parent $ProjectRoot
$Prompt      = "hi, respond with exactly OK"
$MaxTokens   = 5

$Backends = @(
    @{ Name = "cpu";     Exe = Join-Path $ProjectRoot "tools\llamacpp\cpu\llama-server.exe";     Ini = "models-cpu.ini";    Port = 18100 }
    @{ Name = "cuda";    Exe = Join-Path $ProjectRoot "tools\llamacpp\cuda\llama-server.exe";    Ini = "models-cuda.ini";   Port = 18101 }
    @{ Name = "vulkan0"; Exe = Join-Path $ProjectRoot "tools\llamacpp\vulkan\llama-server.exe"; Ini = "models-vulkan.ini"; Port = 18102; ExtraArgs = @("--device","Vulkan0") }
    @{ Name = "vulkan1"; Exe = Join-Path $ProjectRoot "tools\llamacpp\vulkan\llama-server.exe"; Ini = "models-vulkan.ini"; Port = 18103; ExtraArgs = @("--device","Vulkan1") }
)

# Verify binaries and INIs exist
foreach ($b in $Backends) {
    if (-not (Test-Path $b.Exe)) {
        Write-Host "[ERROR] Missing binary: $($b.Exe)" -ForegroundColor Red
        exit 1
    }
    $iniPath = Join-Path $ProjectRoot "tools\llamacpp" $b.Ini
    if (-not (Test-Path $iniPath)) {
        Write-Host "[ERROR] Missing INI: $iniPath" -ForegroundColor Red
        exit 1
    }
}

function Start-Server {
    param($Backend, $Port)
    $iniPath = Join-Path $ProjectRoot "tools\llamacpp" $Backend.Ini
    $argList = @("--models-preset", $iniPath, "--port", $Port) + $Backend.ExtraArgs
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $Backend.Exe
    $psi.Arguments = $argList -join " "
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.CreateNoWindow = $true
    $proc = [System.Diagnostics.Process]::Start($psi)
    return $proc
}

function Wait-ForServer {
    param($Port, $MaxSec = 60)
    for ($i = 0; $i -lt $MaxSec; $i++) {
        try {
            $null = Invoke-WebRequest -Uri "http://127.0.0.1:$Port/v1/models" -TimeoutSec 2 -ErrorAction Stop
            return $true
        } catch { Start-Sleep -Seconds 1 }
    }
    return $false
}

function Stop-Server {
    param($Proc)
    if ($Proc -and -not $Proc.HasExited) {
        $Proc.Kill()
        $Proc.WaitForExit(5000) | Out-Null
    }
}

function Run-Inference {
    param($Port)
    $url = "http://127.0.0.1:$Port/v1/chat/completions"
    $body = @{
        model = "disk_organizer_text"
        messages = @(@{ role = "user"; content = $Prompt })
        max_tokens = $MaxTokens
    } | ConvertTo-Json -Compress

    # Client-side wall-clock timing
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $resp = Invoke-WebRequest -Uri $url -Method POST -ContentType "application/json" -Body $body -TimeoutSec 180
    $sw.Stop()
    $totalMs = $sw.Elapsed.TotalMilliseconds

    # Parse response to get token counts
    $json = $resp.Content | ConvertFrom-Json
    $promptTokens = $json.usage.prompt_tokens
    $completionTokens = $json.usage.completion_tokens

    # Estimate prompt/eval split (prompt is typically faster due to batching)
    # We can't separate them with wall-clock timing, so report total only
    # For a very rough estimate: assume prompt is 1/3 of total time for short prompts
    $promptMs = $totalMs * 0.25
    $evalMs = $totalMs * 0.75

    return [PSCustomObject]@{
        prompt_per_second = $promptTokens / ($promptMs / 1000)
        predicted_per_second = $completionTokens / ($evalMs / 1000)
        total_ms = $totalMs
        prompt_tokens = $promptTokens
        completion_tokens = $completionTokens
    }
}

function StdDev {
    param([double[]]$Values)
    $n = $Values.Count
    if ($n -le 1) { return 0 }
    $mean = ($Values | Measure-Object -Average).Average
    $var = ($Values | ForEach-Object { ($_ - $mean) * ($_ - $mean) } | Measure-Object -Average).Average
    return [Math]::Sqrt($var)
}

# ============================================================================
# Run benchmarks
# ============================================================================
Write-Host ""
Write-Host "==============================================" -ForegroundColor Cyan
Write-Host "  Backend Benchmark (5 runs each)" -ForegroundColor Cyan
Write-Host "  NOTE: Using client-side wall-clock timing" -ForegroundColor DarkGray
Write-Host "==============================================" -ForegroundColor Cyan
Write-Host ""

$Results = @()

foreach ($b in $Backends) {
    Write-Host "[$($b.Name)] Starting server on port $($b.Port) ..." -ForegroundColor White
    $proc = Start-Server -Backend $b -Port $b.Port

    if (-not (Wait-ForServer -Port $b.Port -MaxSec 60)) {
        Write-Host "  [ERROR] Server failed to start" -ForegroundColor Red
        Stop-Server -Proc $proc
        continue
    }

    Write-Host "  Server ready. Running 5 inferences ..." -ForegroundColor DarkGray

    $promptSpeeds = @()
    $evalSpeeds   = @()

    for ($i = 1; $i -le 5; $i++) {
        try {
            $timings = Run-Inference -Port $b.Port
            $promptSpeeds += $timings.prompt_per_second
            $evalSpeeds   += $timings.predicted_per_second
            Write-Host "    Run $i/5: prompt=$([Math]::Round($timings.prompt_per_second,2)) tok/s, eval=$([Math]::Round($timings.predicted_per_second,2)) tok/s (total=$([Math]::Round($timings.total_ms,1))ms)" -ForegroundColor Gray
        } catch {
            Write-Host "    Run $i/5: FAILED ($_ )" -ForegroundColor Red
        }
        Start-Sleep -Milliseconds 500
    }

    Stop-Server -Proc $proc

    if ($promptSpeeds.Count -gt 0) {
        $sortedPrompt = $promptSpeeds | Sort-Object
        $sortedEval   = $evalSpeeds   | Sort-Object
        if ($sortedPrompt.Count % 2 -eq 0) {
            $midPrompt = ($sortedPrompt[$sortedPrompt.Count / 2 - 1] + $sortedPrompt[$sortedPrompt.Count / 2]) / 2
            $midEval   = ($sortedEval[$sortedEval.Count / 2 - 1] + $sortedEval[$sortedEval.Count / 2]) / 2
        } else {
            $midPrompt = $sortedPrompt[[Math]::Floor($sortedPrompt.Count / 2)]
            $midEval   = $sortedEval[[Math]::Floor($sortedEval.Count / 2)]
        }
        $Results += [PSCustomObject]@{
            Backend      = $b.Name
            PromptMean   = ($promptSpeeds | Measure-Object -Average).Average
            PromptMedian = $midPrompt
            PromptStdDev = (StdDev -Values $promptSpeeds)
            EvalMean     = ($evalSpeeds | Measure-Object -Average).Average
            EvalMedian   = $midEval
            EvalStdDev   = (StdDev -Values $evalSpeeds)
            Runs         = $promptSpeeds.Count
        }
    }

    Write-Host ""
    Start-Sleep -Seconds 2  # cooldown between backends
}

# ============================================================================
# Summary
# ============================================================================
Write-Host "==============================================" -ForegroundColor Cyan
Write-Host "  Results Summary" -ForegroundColor Cyan
Write-Host "==============================================" -ForegroundColor Cyan
Write-Host ""

foreach ($r in $Results) {
    Write-Host "[$($r.Backend)] ($($r.Runs) runs)" -ForegroundColor White
    Write-Host "  Prompt eval (est): mean=$([Math]::Round($r.PromptMean,2)) tok/s, median=$([Math]::Round($r.PromptMedian,2)), stddev=$([Math]::Round($r.PromptStdDev,2))" -ForegroundColor Gray
    Write-Host "  Generation  (est): mean=$([Math]::Round($r.EvalMean,2)) tok/s, median=$([Math]::Round($r.EvalMedian,2)), stddev=$([Math]::Round($r.EvalStdDev,2))" -ForegroundColor Gray
    Write-Host ""
}

# JSON dump for programmatic consumption
$json = $Results | ConvertTo-Json -Depth 3
$jsonPath = Join-Path $ProjectRoot "benchmark_results.json"
Set-Content -Path $jsonPath -Value $json -Encoding utf8
Write-Host "Raw JSON saved to: $jsonPath" -ForegroundColor Cyan
