<#
.SYNOPSIS
  opencode-api-proxy installer for Windows.

.EXAMPLE
  iwr https://raw.githubusercontent.com/alvaro-co/opencode-api-proxy/main/install.ps1 -useb | iex

.NOTES
  Options (when running as a file):
    .\install.ps1 [-Dir <DIR>] [-Tag <TAG>]
#>
param(
    [string]$Dir = "$env:LOCALAPPDATA\Programs\opencode-api-proxy",
    [string]$Tag = "latest"
)

$ErrorActionPreference = "Stop"
$Repo = "alvaro-co/opencode-api-proxy"
$Bin = "opencode-api-proxy.exe"

function Info($m) { Write-Host "==> $m" }

switch ($env:PROCESSOR_ARCHITECTURE) {
    "AMD64" { $Target = "x86_64-pc-windows-msvc" }
    "ARM64" { $Target = "aarch64-pc-windows-msvc" }
    default { throw "unsupported architecture: $($env:PROCESSOR_ARCHITECTURE)" }
}

$Asset = "opencode-api-proxy-$Target.zip"
if ($Tag -eq "latest") {
    $Url = "https://github.com/$Repo/releases/latest/download/$Asset"
} else {
    $Url = "https://github.com/$Repo/releases/download/$Tag/$Asset"
}

Info "downloading $Asset"
$Tmp = Join-Path ([IO.Path]::GetTempPath()) ("ocproxy-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $Tmp | Out-Null
try {
    Invoke-WebRequest -Uri $Url -OutFile (Join-Path $Tmp $Asset) -UseBasicParsing
    Expand-Archive -Path (Join-Path $Tmp $Asset) -DestinationPath $Tmp -Force

    $Src = Get-ChildItem -Path $Tmp -Filter $Bin -Recurse | Select-Object -First 1
    if (-not $Src) { throw "binary not found in archive" }

    New-Item -ItemType Directory -Force -Path $Dir | Out-Null
    Copy-Item -Force $Src.FullName (Join-Path $Dir $Bin)

    if ($env:PATH -notlike "*$Dir*") {
        $UserPath = [Environment]::GetEnvironmentVariable("PATH", "User")
        [Environment]::SetEnvironmentVariable("PATH", "$UserPath;$Dir", "User")
        Info "added $Dir to user PATH"
    }

    $Version = & (Join-Path $Dir $Bin) --version
    Info "installed: $Dir\$Bin ($($Version.Split(' ')[-1]))"
    Write-Host ""
    Write-Host "run it:"
    Write-Host "  `"$Dir\$Bin`""
    Write-Host ""
    Write-Host "quick test:"
    Write-Host "  curl http://127.0.0.1:6446/health"
} finally {
    Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
}
