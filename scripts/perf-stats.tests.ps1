# Plain-assert tests for perf-stats.ps1 and bench.ps1 -Report. Exits 1 on any failure.
# Run: pwsh -NoProfile -File scripts/perf-stats.tests.ps1
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'perf-stats.ps1')

$script:failed = 0
$script:passed = 0
function Assert-Equal($name, $want, $got, $tol = 0) {
    $ok = if ($want -is [bool] -or $want -is [string]) { $want -eq $got }
          else { [math]::Abs([double]$want - [double]$got) -le $tol }
    if ($ok) { $script:passed++ }
    else { $script:failed++; Write-Host "FAIL $name`: want $want, got $got" }
}

function New-TempDir {
    $d = Join-Path ([IO.Path]::GetTempPath()) ('retake-perf-test-' + [guid]::NewGuid())
    New-Item -ItemType Directory $d | Out-Null
    $d
}

# Frame stats: 1000 frames at 5 ms and 10 at 20 ms.
$ft = [double[]](@(@(5.0) * 1000) + @(@(20.0) * 10))
$s = Get-FrameStats -FrameTimes $ft
Assert-Equal 'frames' 1010 $s.Frames
Assert-Equal 'seconds' 5.2 $s.Seconds 1e-9
Assert-Equal 'avg fps' (1010 / 5.2) $s.AvgFps 1e-6
Assert-Equal 'low1' 200 $s.Low1 1e-9      # rank ceil(0.99*1010) = 1000 -> 5 ms
Assert-Equal 'low01' 50 $s.Low01 1e-9     # rank ceil(0.999*1010) = 1009 -> 20 ms
Assert-Equal 'spikes' 10 $s.Spikes        # 20 ms > 2 x 5 ms median
Assert-Equal 'median' 5 $s.MedianMs 1e-9
Assert-Equal 'even median' 2.5 (Get-FrameStats -FrameTimes ([double[]](1, 2, 3, 4))).MedianMs 1e-9

# Deltas and the gate.
Assert-Equal 'delta' -2.5 (Get-Delta -Baseline 200 -Value 195) 1e-9
$g = Test-Gate -Baseline ([pscustomobject]@{ AvgFps = 200; Low1 = 150 }) `
    -Config ([pscustomobject]@{ AvgFps = 195; Low1 = 141 })
Assert-Equal 'avg loss' 2.5 $g.AvgLossPct 1e-9
Assert-Equal 'low loss' 6 $g.Low1LossPct 1e-9
Assert-Equal 'gate fails on lows' $false $g.Pass
$g = Test-Gate -Baseline ([pscustomobject]@{ AvgFps = 200; Low1 = 150 }) `
    -Config ([pscustomobject]@{ AvgFps = 194; Low1 = 142.5 })
Assert-Equal 'gate on the edge' $true $g.Pass

# Run mean.
$m = Get-RunMean -Stats @(
    [pscustomobject]@{ Frames = 10; Seconds = 1; AvgFps = 100; Low1 = 80; Low01 = 60; Spikes = 2; MedianMs = 10 },
    [pscustomobject]@{ Frames = 20; Seconds = 1; AvgFps = 200; Low1 = 90; Low01 = 70; Spikes = 4; MedianMs = 5 })
Assert-Equal 'mean fps' 150 $m.AvgFps 1e-9
Assert-Equal 'mean low1' 85 $m.Low1 1e-9
Assert-Equal 'mean spikes' 3 $m.Spikes 1e-9

# PresentMon CSV: the 0 and the NA are skipped, another app is filtered out.
$dir = New-TempDir
$csv = Join-Path $dir 'run.csv'
@'
Application,ProcessID,SwapChainAddress,MsBetweenPresents,MsInPresentAPI
RainbowSix.exe,10,0x1,0,0.1
RainbowSix.exe,10,0x1,4.5,0.1
dwm.exe,20,0x2,16.6,0.1
RainbowSix.exe,10,0x1,NA,0.1
RainbowSix.exe,10,0x1,5.5,0.1
'@ | Set-Content -Path $csv
$all = Read-FrameTimes -Path $csv
Assert-Equal 'all apps' 3 $all.Count
$r6 = Read-FrameTimes -Path $csv -Process 'RainbowSix.exe'
Assert-Equal 'one app' 2 $r6.Count
Assert-Equal 'one app sum' 10 ($r6 | Measure-Object -Sum).Sum 1e-9

# Buffer rate: two 2 s segments of 1,000,000 bytes = 4 Mbit/s = 1.8 GB/hour.
$buf = New-TempDir
"seg_00.ts,0.000000,2.000000`nseg_01.ts,2.000000,4.000000" | Set-Content (Join-Path $buf 'index.csv')
foreach ($n in 'seg_00.ts', 'seg_01.ts') {
    [IO.File]::WriteAllBytes((Join-Path $buf $n), [byte[]]::new(1000000))
}
$b = Get-BufferRate -IndexCsv (Join-Path $buf 'index.csv')
Assert-Equal 'segments' 2 $b.Segments
Assert-Equal 'mbps' 4 $b.Mbps 1e-9
Assert-Equal 'gb/h' 1.8 $b.GBPerHour 1e-9

# Table.
$t = Format-ResultTable -Rows @(
    [pscustomobject]@{ Config = 'baseline'; Runs = 3; AvgFps = 200; Low1 = 150; Low01 = 100; Spikes = 1; Gate = $null },
    [pscustomobject]@{ Config = 'hevc-speed'; Runs = 3; AvgFps = 194; Low1 = 142.5; Low01 = 90; Spikes = 2; Gate = $g })
Assert-Equal 'table header' $true ($t -match '\| Config \| Runs \| Avg FPS \| Δ avg \| 1% low \| Δ 1% \| 0.1% low \| Spikes \| Gate \|')
Assert-Equal 'table row' $true ($t -match '\| hevc-speed \| 3 \| 194\.0 \| -3\.0% \| 142\.5 \| -5\.0% \| 90\.0 \| 2\.0 \| pass \|')

# bench.ps1 -Report over a fixture folder (only once bench.ps1 exists).
$bench = Join-Path $PSScriptRoot 'bench.ps1'
if (Test-Path $bench) {
    $res = New-TempDir
    $hdr = 'Application,ProcessID,MsBetweenPresents'
    foreach ($n in 1, 2) {
        ($hdr, (@('Game.exe,1,5') * 100)) | ForEach-Object { $_ } | Set-Content (Join-Path $res "baseline-$n.csv")
        ($hdr, (@('Game.exe,1,5.5') * 100)) | ForEach-Object { $_ } | Set-Content (Join-Path $res "hevc-speed-$n.csv")
    }
    & $bench -Report $res | Out-Null
    $summary = Get-Content (Join-Path $res 'summary.md') -Raw
    Assert-Equal 'report row' $true ($summary -match '\| hevc-speed \| 2 \| 181\.8 \|')
    Assert-Equal 'report gate' $true ($summary -match '\| hevc-speed \|.*\| FAIL \|')
    Assert-Equal 'report baseline' $true ($summary -match '\| baseline \| 2 \| 200\.0 \|')
}

Write-Host "$script:passed passed, $script:failed failed"
if ($script:failed) { exit 1 }
