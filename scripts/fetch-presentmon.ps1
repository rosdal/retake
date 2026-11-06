<#
.SYNOPSIS
  Downloads the PresentMon build pinned in third_party/presentmon.lock, verifies its SHA-256
  and puts it in third_party/presentmon/PresentMon.exe, for scripts/bench.ps1.
.PARAMETER Force
  Re-download even if third_party/presentmon/ already holds the pinned version.
.PARAMETER Lock
  Another lock file to use (for testing the hash check).
#>
param([switch]$Force, [string]$Lock)
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$lockPath = if ($Lock) { $Lock } else { Join-Path $root 'third_party/presentmon.lock' }
$dest = Join-Path $root 'third_party/presentmon'

$pin = @{}
foreach ($line in Get-Content $lockPath) {
    if ($line -match '^\s*([a-z0-9_]+)\s*=\s*(.+?)\s*$') { $pin[$Matches[1]] = $Matches[2] }
}
foreach ($key in 'version', 'url', 'sha256') {
    if (-not $pin[$key]) { throw "presentmon.lock is missing '$key'" }
}

$stamp = Join-Path $dest '.version'
if (-not $Force -and (Test-Path $stamp) -and ((Get-Content $stamp -Raw).Trim() -eq $pin.sha256)) {
    Write-Host "PresentMon $($pin.version) already present in $dest"
    return
}

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("retake-presentmon-" + [guid]::NewGuid())
New-Item -ItemType Directory $tmp | Out-Null
try {
    $exe = Join-Path $tmp 'PresentMon.exe'
    Write-Host "Downloading PresentMon $($pin.version) ..."
    $ProgressPreference = 'SilentlyContinue'
    Invoke-WebRequest -Uri $pin.url -OutFile $exe -UseBasicParsing

    $actual = (Get-FileHash -Algorithm SHA256 $exe).Hash.ToLowerInvariant()
    if ($actual -ne $pin.sha256.ToLowerInvariant()) {
        throw "SHA-256 mismatch: expected $($pin.sha256), got $actual"
    }

    New-Item -ItemType Directory -Force $dest | Out-Null
    Move-Item -Force $exe (Join-Path $dest 'PresentMon.exe')
    Set-Content -Path $stamp -Value $pin.sha256
    Write-Host "PresentMon $($pin.version) installed in $dest"
}
finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
