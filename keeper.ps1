<#
.SYNOPSIS
  opencode-api-proxy keeper for Windows.
  Auto-installs, keeps the binary updated, and restarts it whenever it dies.

.EXAMPLE
  .\keeper.ps1                          # keyless on port 6446
  .\keeper.ps1 -Args --auth             # generated API key
  Start-Process powershell -ArgumentList "-File keeper.ps1" -WindowStyle Hidden
#>
param(
    [string]$Binary = "$env:LOCALAPPDATA\Programs\opencode-api-proxy\opencode-api-proxy.exe",
    [int]$CheckInterval = 3600,
    [int]$Tick = 10,
    [string[]]$AppArgs = @()
)

$ErrorActionPreference = "Continue"
$Repo = "alvaro-co/opencode-api-proxy"

function Log($level, $msg) {
    Write-Host ("{0} [{1}] {2}" -f (Get-Date -Format "yyyy-MM-dd HH:mm:ss"), $level, $msg)
}

function Latest-Version {
    try {
        $rel = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" -TimeoutSec 30
        return $rel.tag_name.TrimStart("v")
    } catch {
        Log "warn" "could not check latest version: $_"
        return $null
    }
}

function Current-Version {
    if (-not (Test-Path $Binary)) { return "0" }
    $out = & $Binary --version 2>$null
    if ($out) { return ($out.Split(" ")[-1]) } else { return "0" }
}

function Install-Update {
    switch ($env:PROCESSOR_ARCHITECTURE) {
        "AMD64" { $Target = "x86_64-pc-windows-msvc" }
        "ARM64" { $Target = "aarch64-pc-windows-msvc" }
        default { Log "error" "unsupported architecture"; return $false }
    }
    $Asset = "opencode-api-proxy-$Target.zip"
    $Url = "https://github.com/$Repo/releases/latest/download/$Asset"
    $Tmp = Join-Path ([IO.Path]::GetTempPath()) ("ocproxy-" + [guid]::NewGuid().ToString("N"))
    try {
        New-Item -ItemType Directory -Force -Path $Tmp | Out-Null
        Invoke-WebRequest -Uri $Url -OutFile (Join-Path $Tmp $Asset) -UseBasicParsing
        try {
            Invoke-WebRequest -Uri "$Url.sha256" -OutFile (Join-Path $Tmp "checksum") -UseBasicParsing -ErrorAction Stop
            $Want = (((Get-Content (Join-Path $Tmp "checksum") -TotalCount 1) -split '\s+')[0]).ToLower()
            $Got = ((Get-FileHash -Algorithm SHA256 -Path (Join-Path $Tmp $Asset)).Hash).ToLower()
            if ($Got -ne $Want) { throw "checksum mismatch for $Asset" }
            Log "info" "checksum verified"
        } catch {
            if ($_.Exception.Message -match "checksum mismatch") { throw }
            Log "warn" "checksum unavailable, skipping verification"
        }
        Expand-Archive -Path (Join-Path $Tmp $Asset) -DestinationPath $Tmp -Force
        $Src = Get-ChildItem -Path $Tmp -Filter "opencode-api-proxy.exe" -Recurse | Select-Object -First 1
        if (-not $Src) { Log "error" "binary not found in archive"; return $false }

        New-Item -ItemType Directory -Force -Path (Split-Path $Binary) | Out-Null
        Copy-Item -Force $Src.FullName "$Binary.new"
        if (Test-Path $Binary) { Remove-Item -Force $Binary }
        Move-Item -Force "$Binary.new" $Binary
        Log "info" "updated to $(Current-Version)"
        return $true
    } catch {
        Log "error" "update install failed: $_"
        return $false
    } finally {
        Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
    }
}

function Maybe-Update {
    $want = Latest-Version
    if (-not $want) { return $false }
    $have = Current-Version
    if ($want -eq $have) {
        Log "info" "up to date ($have)"
        return $false
    }
    Log "info" "update available: $have -> $want"
    Install-Update
}

if (-not (Test-Path $Binary)) {
    Log "info" "$Binary not found, installing..."
    if (-not (Install-Update)) { exit 1 }
} else {
    Maybe-Update | Out-Null
}

$Backoff = 2
$LastCheck = Get-Date
$RestartNow = $false

while ($true) {
    Log "info" "starting $Binary $($AppArgs -join ' ')"
    $Proc = Start-Process -FilePath $Binary -ArgumentList $AppArgs -PassThru -NoNewWindow

    while (-not $Proc.HasExited) {
        Start-Sleep -Seconds $Tick
        if (((Get-Date) - $LastCheck).TotalSeconds -ge $CheckInterval) {
            $LastCheck = Get-Date
            if (Maybe-Update) {
                Log "info" "restarting with new binary"
                $RestartNow = $true
                if (-not $Proc.HasExited) { Stop-Process -Id $Proc.Id -Force -ErrorAction SilentlyContinue }
                break
            }
        }
    }

    if (-not $Proc.HasExited) { Stop-Process -Id $Proc.Id -Force -ErrorAction SilentlyContinue }
    if ($RestartNow) {
        $RestartNow = $false
        $Backoff = 2
        continue
    }
    $code = $Proc.ExitCode
    Log "warn" "process exited (code $code), restarting in ${Backoff}s"
    Start-Sleep -Seconds $Backoff
    if ($Backoff -lt 30) { $Backoff *= 2 }
}
