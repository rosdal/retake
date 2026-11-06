<#
.SYNOPSIS
  Downloads the ffmpeg build pinned in third_party/ffmpeg.lock, verifies its SHA-256 and
  unpacks it into third_party/ffmpeg/ (bin/ffmpeg.exe, ...).
.PARAMETER Force
  Re-download even if third_party/ffmpeg/ already holds the pinned version.
#>
param([switch]$Force)
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$lockPath = Join-Path $root 'third_party/ffmpeg.lock'
$dest = Join-Path $root 'third_party/ffmpeg'

$lock = @{}
foreach ($line in Get-Content $lockPath) {
    if ($line -match '^\s*([a-z0-9_]+)\s*=\s*(.+?)\s*$') { $lock[$Matches[1]] = $Matches[2] }
}
foreach ($key in 'version', 'url', 'sha256') {
    if (-not $lock[$key]) { throw "ffmpeg.lock is missing '$key'" }
}

$stamp = Join-Path $dest '.version'
if (-not $Force -and (Test-Path $stamp) -and ((Get-Content $stamp -Raw).Trim() -eq $lock.sha256)) {
    Write-Host "ffmpeg $($lock.version) already present in $dest"
    return
}

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("retake-ffmpeg-" + [guid]::NewGuid())
New-Item -ItemType Directory $tmp | Out-Null
try {
    $zip = Join-Path $tmp 'ffmpeg.zip'
    Write-Host "Downloading ffmpeg $($lock.version) ..."
    $ProgressPreference = 'SilentlyContinue'
    Invoke-WebRequest -Uri $lock.url -OutFile $zip -UseBasicParsing

    $actual = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLowerInvariant()
    if ($actual -ne $lock.sha256.ToLowerInvariant()) {
        throw "SHA-256 mismatch: expected $($lock.sha256), got $actual"
    }

    Expand-Archive -Path $zip -DestinationPath (Join-Path $tmp 'x')
    $inner = Get-ChildItem (Join-Path $tmp 'x') -Directory | Select-Object -First 1
    if (-not (Test-Path (Join-Path $inner.FullName 'bin/ffmpeg.exe'))) { throw 'archive has no bin/ffmpeg.exe' }

    if (Test-Path $dest) { Remove-Item -Recurse -Force $dest }
    Move-Item $inner.FullName $dest
    Set-Content -Path $stamp -Value $lock.sha256
    Write-Host "ffmpeg $($lock.version) installed in $dest"
}
finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
