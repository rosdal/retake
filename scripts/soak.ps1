<#
.SYNOPSIS
  Long unattended soak with the avsync stand-in game: Retake records for -Hours, saves a clip
  at each mark in -SaveAtMinutes, measures each clip with `retake sync-check`, and collects
  the --diag memory and handle lines. Results go to -Out as soak.md.

.EXAMPLE
  ./scripts/soak.ps1 -Install D:\retake-test\app -Hours 3
#>
param(
    [Parameter(Mandatory)][string]$Install,
    [double]$Hours = 3,
    [int[]]$SaveAtMinutes = @(1, 30, 60, 120, 180),
    [string]$Crop = '200:200:220:270',
    [int[]]$WindowAt = @(-1800, 300),
    [string]$Out
)
$ErrorActionPreference = 'Stop'
$Install = (Resolve-Path $Install).Path
$retake = Join-Path $Install 'retake.exe'
$avsync = Join-Path $Install 'avsync.exe'
foreach ($f in $retake, $avsync) { if (-not (Test-Path $f)) { throw "missing $f" } }
if (Get-Process retake -ErrorAction SilentlyContinue) { throw 'Retake is already running; quit it first' }
if (-not $Out) { $Out = Join-Path (Split-Path -Parent $PSScriptRoot) ('bench/soak-' + (Get-Date -Format 'yyyy-MM-dd_HHmmss')) }
New-Item -ItemType Directory -Force $Out | Out-Null
$logDir = Join-Path $env:LOCALAPPDATA 'Retake\logs'
$started = Get-Date

$av = Start-Process $avsync -ArgumentList $WindowAt -PassThru
Start-Sleep 1
$rt = Start-Process $retake -ArgumentList '--diag' -PassThru
$rows = @()
try {
    foreach ($m in $SaveAtMinutes | Where-Object { $_ -le $Hours * 60 }) {
        $at = $started.AddMinutes($m)
        $wait = ($at - (Get-Date)).TotalSeconds
        if ($wait -gt 0) { Start-Sleep -Seconds ([int]$wait) }
        if ($rt.HasExited) { throw "Retake exited after $([int]((Get-Date) - $started).TotalMinutes) min" }
        $before = Get-Date
        Start-Process $retake -ArgumentList '--save' -Wait
        Start-Sleep 12
        $clip = Get-ChildItem -Recurse -Filter *.mkv -Path (Join-Path $Install '..\clips\raw'), 'D:\Retake\clips\raw' -ErrorAction SilentlyContinue |
            Where-Object { $_.LastWriteTime -ge $before -and $_.Name -notmatch '_graded' } | Sort-Object LastWriteTime | Select-Object -Last 1
        if (-not $clip) { $rows += "| $m | no clip | | | |"; continue }
        $res = & $retake sync-check $clip.FullName --crop $Crop 2>&1 | Out-String
        $sum = ($res -split "`n" | Where-Object { $_ -match '^pairs' }) -replace '\s+', ' '
        $rows += "| $m | $($clip.Name) | $sum |"
        Write-Host "$m min: $sum"
    }
}
finally {
    Stop-Process -Id $rt.Id -ErrorAction SilentlyContinue
    Stop-Process -Id $av.Id -ErrorAction SilentlyContinue
}

$diag = Get-ChildItem $logDir | Sort-Object LastWriteTime | Select-Object -Last 2 | ForEach-Object { Get-Content $_.FullName } |
    Where-Object { $_ -match ' diag ' -and [datetime]::Parse($_.Substring(0, 27)).ToLocalTime() -ge $started } |
    ForEach-Object {
        $t = [datetime]::Parse($_.Substring(0, 27)).ToLocalTime()
        $kv = @{}; foreach ($m in [regex]::Matches($_, '(\w+)=(Some\()?(\d+)')) { $kv[$m.Groups[1].Value] = $m.Groups[3].Value }
        "| $([int]($t - $started).TotalMinutes) | $($kv.retake_mb) | $($kv.retake_handles) | $($kv.ffmpeg_mb) | $($kv.ffmpeg_handles) |"
    }
$md = @(
    "# Soak $($started.ToString('yyyy-MM-dd HH:mm')), $Hours h, avsync stand-in", '',
    '| Minute | Clip | Sync |', '| ---: | --- | --- |') + $rows + @('',
    '| Minute | retake MB | retake handles | ffmpeg MB | ffmpeg handles |', '| ---: | ---: | ---: | ---: | ---: |') + $diag
$md -join "`n" | Set-Content (Join-Path $Out 'soak.md') -Encoding utf8
$md -join "`n"
