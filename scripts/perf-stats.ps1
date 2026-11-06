# Pure helpers for the v0.4 performance gate. Dot-source it: . scripts/perf-stats.ps1
# Metric definitions follow CapFrameX's defaults (see docs/perf.md):
#   frame time = MsBetweenPresents; average FPS = frames / total seconds;
#   1% low = 1000 / 99th-percentile frame time (nearest rank); 0.1% low = same at 99.9th;
#   spikes = frames longer than 2x the median frame time.

# Gate thresholds, in percent loss against the baseline.
$script:GateAvgLossPct = 3
$script:GateLow1LossPct = 5

function Read-FrameTimes {
    <# MsBetweenPresents of every frame in a PresentMon CSV, optionally for one process.
       Skips NA and values <= 0 (PresentMon's first frame of a swap chain). #>
    param([Parameter(Mandatory)][string]$Path, [string]$Process)
    $rows = Import-Csv -Path $Path
    if ($Process) { $rows = $rows | Where-Object { $_.Application -eq $Process } }
    $out = [Collections.Generic.List[double]]::new()
    foreach ($r in $rows) {
        $v = 0.0
        if ([double]::TryParse($r.MsBetweenPresents, [Globalization.NumberStyles]::Float,
                [Globalization.CultureInfo]::InvariantCulture, [ref]$v) -and $v -gt 0) {
            $out.Add($v)
        }
    }
    , $out.ToArray()
}

function Get-Percentile([double[]]$Sorted, [double]$P) {
    $i = [math]::Ceiling($P * $Sorted.Length) - 1
    $Sorted[[math]::Max(0, [math]::Min($i, $Sorted.Length - 1))]
}

function Get-FrameStats {
    <# Average FPS, 1% and 0.1% lows, spikes and median of one run's frame times (ms). #>
    param([Parameter(Mandatory)][double[]]$FrameTimes)
    $n = $FrameTimes.Length
    if ($n -eq 0) { throw 'no frames' }
    $sorted = [double[]]$FrameTimes.Clone()
    [Array]::Sort($sorted)
    $sum = 0.0
    foreach ($f in $FrameTimes) { $sum += $f }
    $median = if ($n % 2) { $sorted[($n - 1) / 2] } else { ($sorted[$n / 2 - 1] + $sorted[$n / 2]) / 2 }
    $spikes = 0
    foreach ($f in $FrameTimes) { if ($f -gt 2 * $median) { $spikes++ } }
    [pscustomobject]@{
        Frames   = $n
        Seconds  = $sum / 1000
        AvgFps   = $n / ($sum / 1000)
        Low1     = 1000 / (Get-Percentile $sorted 0.99)
        Low01    = 1000 / (Get-Percentile $sorted 0.999)
        Spikes   = $spikes
        MedianMs = $median
    }
}

function Get-RunMean {
    <# The field-by-field mean of several Get-FrameStats results. #>
    param([Parameter(Mandatory)][object[]]$Stats)
    $o = [ordered]@{}
    foreach ($k in 'Frames', 'Seconds', 'AvgFps', 'Low1', 'Low01', 'Spikes', 'MedianMs') {
        $o[$k] = ($Stats | Measure-Object -Property $k -Average).Average
    }
    [pscustomobject]$o
}

function Get-Delta {
    <# Percent change from Baseline to Value. #>
    param([Parameter(Mandatory)][double]$Baseline, [Parameter(Mandatory)][double]$Value)
    ($Value - $Baseline) / $Baseline * 100
}

function Test-Gate {
    <# Whether Config's mean stays within the gate against Baseline's mean. #>
    param([Parameter(Mandatory)]$Baseline, [Parameter(Mandatory)]$Config)
    $avg = - (Get-Delta $Baseline.AvgFps $Config.AvgFps)
    $low = - (Get-Delta $Baseline.Low1 $Config.Low1)
    [pscustomobject]@{
        AvgLossPct  = $avg
        Low1LossPct = $low
        # A hair of tolerance so a loss of exactly 3.0% computed in floating point passes.
        Pass        = ($avg -le $script:GateAvgLossPct + 1e-9) -and ($low -le $script:GateLow1LossPct + 1e-9)
    }
}

function Get-BufferRate {
    <# Bitrate of the segments listed in a ring buffer's index.csv (name,start,end), from
       their sizes on disk. Segments that are missing are skipped. #>
    param([Parameter(Mandatory)][string]$IndexCsv)
    $dir = Split-Path -Parent $IndexCsv
    $bytes = 0L; $secs = 0.0; $count = 0
    foreach ($line in Get-Content $IndexCsv) {
        $f = $line.Split(',')
        if ($f.Length -lt 3) { continue }
        $path = Join-Path $dir $f[0]
        if (-not (Test-Path $path)) { continue }
        $inv = [Globalization.CultureInfo]::InvariantCulture
        $bytes += (Get-Item $path).Length
        $secs += [double]::Parse($f[2], $inv) - [double]::Parse($f[1], $inv)
        $count++
    }
    if ($secs -le 0) { return [pscustomobject]@{ Mbps = 0; GBPerHour = 0; Segments = 0 } }
    $bps = $bytes / $secs
    [pscustomobject]@{ Mbps = $bps * 8 / 1e6; GBPerHour = $bps * 3600 / 1e9; Segments = $count }
}

function Format-ResultTable {
    <# Markdown table of per-config means. Each row: Config, Runs, AvgFps, Low1, Low01,
       Spikes, Gate (a Test-Gate result, or $null for the baseline). #>
    param([Parameter(Mandatory)][object[]]$Rows)
    $f = { param($v) ([double]$v).ToString('0.0', [Globalization.CultureInfo]::InvariantCulture) }
    $p = { param($v) ([double]$v).ToString('+0.0;-0.0;0.0', [Globalization.CultureInfo]::InvariantCulture) + '%' }
    $lines = @(
        '| Config | Runs | Avg FPS | Δ avg | 1% low | Δ 1% | 0.1% low | Spikes | Gate |',
        '| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |'
    )
    foreach ($r in $Rows) {
        if ($r.Gate) {
            $da = & $p (- $r.Gate.AvgLossPct); $dl = & $p (- $r.Gate.Low1LossPct)
            $gate = if ($r.Gate.Pass) { 'pass' } else { 'FAIL' }
        } else { $da = '—'; $dl = '—'; $gate = '—' }
        $lines += "| $($r.Config) | $($r.Runs) | $(& $f $r.AvgFps) | $da | $(& $f $r.Low1) | $dl | $(& $f $r.Low01) | $(& $f $r.Spikes) | $gate |"
    }
    $lines -join "`n"
}
