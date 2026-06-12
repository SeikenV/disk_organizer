# 安装 Qwopus3.5-0.8B-v3 到 Ollama
# 用法: .\install_qwopus.ps1 [-Quant Q4_K_M|Q5_K_M|Q6_K|Q8_0]
param(
    [string]$Quant = "Q4_K_M"
)

$ErrorActionPreference = "Stop"
$BaseUrl = "https://modelscope.cn/models/Jackrong/Qwopus3.5-0.8B-v3-GGUF/resolve/master"
$FileName = "Qwen3.5-0.8B.$Quant.gguf"
$Url = "$BaseUrl/$FileName"
$OllamaModels = "$env:USERPROFILE\.ollama\models"
$ModelName = "qwopus3.5:0.8b"

Write-Host "=== 安装 Qwopus3.5-0.8B ($Quant) 到 Ollama ===" -ForegroundColor Cyan

# 1. 下载 GGUF
$DestDir = "$PSScriptRoot\..\temp"
New-Item -ItemType Directory -Force -Path $DestDir | Out-Null
$DestFile = "$DestDir\$FileName"

if (Test-Path $DestFile) {
    $existing = (Get-Item $DestFile).Length
    Write-Host "已有文件: $DestFile ($([math]::Round($existing/1MB, 1)) MB)" -ForegroundColor Yellow
    $download = Read-Host "重新下载？(y/n)"
    if ($download -ne 'y') {
        Write-Host "跳过下载，使用已有文件" -ForegroundColor Green
    } else {
        Invoke-WebRequest -Uri $Url -OutFile $DestFile
    }
} else {
    Write-Host "下载 $FileName ..." -ForegroundColor Yellow
    Invoke-WebRequest -Uri $Url -OutFile $DestFile
    Write-Host "下载完成: $([math]::Round((Get-Item $DestFile).Length/1MB, 1)) MB" -ForegroundColor Green
}

# 2. 创建 Modelfile（关闭 reasoning，纯 chat 模式）
$Modelfile = @"
FROM $DestFile
TEMPLATE """<|im_start|>system
{{ .System }}<|im_end|>
<|im_start|>user
{{ .Prompt }}<|im_end|>
<|im_start|>assistant
"""
PARAMETER temperature 0.3
PARAMETER top_p 0.9
"@

$ModelfilePath = "$DestDir\Modelfile.qwopus"
$Modelfile | Set-Content -Path $ModelfilePath -Encoding UTF8
Write-Host "Modelfile 已创建: $ModelfilePath" -ForegroundColor Green

# 3. 创建 Ollama 模型
Write-Host "创建 Ollama 模型 '$ModelName' ..." -ForegroundColor Yellow
ollama create $ModelName -f $ModelfilePath

# 4. 验证
Write-Host "`n=== 验证模型 ===" -ForegroundColor Cyan
ollama list | Select-String "qwopus"

Write-Host "`n=== 测试 ===" -ForegroundColor Cyan
$testResult = & ollama run $ModelName "用一句话回答：1+1等于几？"
Write-Host $testResult

Write-Host "`n安装完成！模型名: $ModelName" -ForegroundColor Green
