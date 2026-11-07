<#
.SYNOPSIS
  Builds the pinned r6-dissect (third_party/r6-dissect.lock) from source with Go and puts
  r6-dissect.exe in third_party/r6-dissect/. Needs Go 1.26+ on PATH and git.
#>
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$lock = Get-Content (Join-Path $root 'third_party/r6-dissect.lock') | Where-Object { $_ -match '^\w+=' } |
    ForEach-Object { $k, $v = $_ -split '=', 2; @{ $k = $v } }
$repo = ($lock | Where-Object { $_.repo }).repo
$commit = ($lock | Where-Object { $_.commit }).commit
$out = Join-Path $root 'third_party/r6-dissect'
$src = Join-Path ([IO.Path]::GetTempPath()) "r6-dissect-$commit"
if (-not (Test-Path $src)) {
    git clone --quiet $repo $src
}
git -C $src checkout --quiet $commit
if ((git -C $src rev-parse HEAD) -ne $commit) { throw "could not check out $commit" }
New-Item -ItemType Directory -Force $out | Out-Null
Push-Location $src
try {
    $env:CGO_ENABLED = '0'
    go build -trimpath -ldflags '-s -w' -o (Join-Path $out 'r6-dissect.exe') .
    if ($LASTEXITCODE) { throw "go build failed ($LASTEXITCODE)" }
}
finally { Pop-Location }
Set-Content (Join-Path $out '.version') $commit
Write-Host "Built r6-dissect $commit into $out"
