<#
.SYNOPSIS
  Writes THIRD_PARTY_LICENSES.txt for a release: ffmpeg (GPL build, with its source link),
  r6-dissect (MIT) and every Rust crate compiled into retake.exe with its licence.
#>
param([Parameter(Mandatory)][string]$Out)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$ff = @{}; Get-Content (Join-Path $root 'third_party/ffmpeg.lock') | Where-Object { $_ -match '^\w+=' } | ForEach-Object { $k, $v = $_ -split '=', 2; $ff[$k] = $v }
$r6 = @{}; Get-Content (Join-Path $root 'third_party/r6-dissect.lock') | Where-Object { $_ -match '^\w+=' } | ForEach-Object { $k, $v = $_ -split '=', 2; $r6[$k] = $v }
$meta = cargo metadata --format-version 1 --manifest-path (Join-Path $root 'Cargo.toml') --filter-platform x86_64-pc-windows-msvc | ConvertFrom-Json
$crates = $meta.packages | Where-Object { $_.source } | Sort-Object name, version | ForEach-Object { "{0} {1} — {2}" -f $_.name, $_.version, ($_.license ?? 'see crate') }
@(
    'Retake is MIT-licensed (see LICENSE). This package also contains other programs and code:',
    '',
    "ffmpeg $($ff.version) ($($ff.flavor)) — GNU General Public License v3 (this build includes GPL components,",
    '  so this package is distributed under the GPL''s terms for ffmpeg). Source code and build details:',
    "  $($ff.url -replace '/releases/download/.*', '')  and  https://ffmpeg.org/download.html",
    '',
    "r6-dissect ($($r6.repo) at $($r6.commit)) — MIT License, Copyright (c) 2022 Benjamin Ryan",
    '',
    'Rust crates compiled into retake.exe:',
    ''
) + $crates | Set-Content -Path $Out -Encoding utf8
Write-Host "Wrote $Out ($($crates.Count) crates)"
