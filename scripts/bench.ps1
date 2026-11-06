<#
.SYNOPSIS
  Runs the v0.4 performance comparison: the game's frame times with Retake off (baseline)
  and with each Retake config, plus GPU, CPU and disk cost, then prints a table with % deltas
  and the gate verdict. You play the route; everything else is scripted.

.DESCRIPTION
  For each config and run: writes a small config.toml next to retake.exe (only the keys that
  config changes; Retake fills in the rest from its defaults), starts Retake, waits for its
  ffmpeg, asks you to start the route, then captures -Seconds of frame times with PresentMon
  while sampling perf counters, measures the buffer's bitrate and stops Retake.
  Your own config.toml is put back at the end, also on Ctrl+C.

  Results go to -Out: env.json, <config>-<n>.csv (PresentMon), <config>-<n>.counters.json,
  <config>-<n>.buffer.json and summary.md. -Report <folder> rebuilds summary.md from them.

  Needs third_party/presentmon/PresentMon.exe (scripts/fetch-presentmon.ps1) and an account
  in "Performance Log Users" or an elevated shell. See docs/perf.md.

.EXAMPLE
  ./scripts/bench.ps1 -Install C:\Tools\Retake
.EXAMPLE
  ./scripts/bench.ps1 -Install C:\Tools\Retake -Configs baseline,default,hevc-240 -Runs 3
.EXAMPLE
  ./scripts/bench.ps1 -Report bench\2026-11-07_201500
#>
[CmdletBinding(DefaultParameterSetName = 'Run')]
param(
    [Parameter(ParameterSetName = 'Run', Mandatory)][string]$Install,
    [Parameter(ParameterSetName = 'Run')][string[]]$Configs = @('baseline', 'default', 'hevc-120', 'hevc-balanced'),
    [Parameter(ParameterSetName = 'Run')][ValidateRange(1, 20)][int]$Runs = 3,
    [Parameter(ParameterSetName = 'Run')][ValidateRange(5, 600)][int]$Seconds = 60,
    [Parameter(ParameterSetName = 'Run')][string]$Process = 'RainbowSix.exe',
    [Parameter(ParameterSetName = 'Run')][string]$Out,
    [Parameter(ParameterSetName = 'Report', Mandatory)][string]$Report
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'perf-stats.ps1')
$inv = [Globalization.CultureInfo]::InvariantCulture

# Config overlays: [section] -> key -> TOML value. $null = Retake not running.
$Overlays = [ordered]@{
    'baseline'      = $null
    'default'       = @{}
    'hevc-120'      = @{ capture = @{ fps = '120' } }
    'hevc-240'      = @{ capture = @{ fps = '240' } }
    'hevc-balanced' = @{ capture = @{ fps = '120'; preset = '"balanced"' } }
    'hevc-qp18-20'  = @{ capture = @{ fps = '120'; qp_i = '18'; qp_p = '20' } }
    'hevc-low-prio' = @{ capture = @{ fps = '120'; priority = '"below_normal"' } }
}

function ConvertTo-OverlayToml($overlay, [string]$proc) {
    $sections = [ordered]@{
        game  = [ordered]@{ processes = "[`"$proc`"]" }
        clips = [ordered]@{ sound_cue = 'false' }
    }
    foreach ($s in $overlay.Keys) {
        if (-not $sections.Contains($s)) { $sections[$s] = [ordered]@{} }
        foreach ($k in $overlay[$s].Keys) { $sections[$s][$k] = $overlay[$s][$k] }
    }
    $lines = @('# Written by scripts/bench.ps1; your own config.toml is restored afterwards.')
    foreach ($s in $sections.Keys) {
        $lines += '', "[$s]"
        foreach ($k in $sections[$s].Keys) { $lines += "$k = $($sections[$s][$k])" }
    }
    $lines -join "`n"
}

function Get-Mean($items, [string]$prop) {
    $v = @($items | Where-Object { $null -ne $_.$prop } | ForEach-Object { [double]$_.$prop })
    if ($v.Count) { ($v | Measure-Object -Average).Average } else { $null }
}

function Invoke-Report([string]$dir) {
    $order = @()
    $envFile = Join-Path $dir 'env.json'
    if (Test-Path $envFile) { $order = @((Get-Content $envFile -Raw | ConvertFrom-Json).Configs) }
    $groups = [ordered]@{}
    foreach ($f in Get-ChildItem -Path $dir -Filter '*.csv' | Sort-Object Name) {
        if ($f.Name -notmatch '^(?<cfg>.+)-(?<n>\d+)\.csv$') { continue }
        $cfg = $Matches.cfg
        if (-not $groups.Contains($cfg)) { $groups[$cfg] = @() }
        $groups[$cfg] += $f
    }
    if (-not $groups.Contains('baseline')) { throw "no baseline-<n>.csv in $dir" }
    $names = @('baseline') + @($order | Where-Object { $_ -ne 'baseline' -and $groups.Contains($_) }) +
        @($groups.Keys | Where-Object { $_ -ne 'baseline' -and $order -notcontains $_ })

    $means = @{}
    $rows = @()
    $res = @()
    foreach ($cfg in $names) {
        $stats = @($groups[$cfg] | ForEach-Object { Get-FrameStats -FrameTimes (Read-FrameTimes -Path $_.FullName) })
        $means[$cfg] = Get-RunMean -Stats $stats
        $gate = if ($cfg -eq 'baseline') { $null } else { Test-Gate -Baseline $means.baseline -Config $means[$cfg] }
        $m = $means[$cfg]
        $rows += [pscustomobject]@{
            Config = $cfg; Runs = $stats.Count; AvgFps = $m.AvgFps; Low1 = $m.Low1
            Low01 = $m.Low01; Spikes = $m.Spikes; Gate = $gate
        }
        $counters = @($groups[$cfg] | ForEach-Object {
                $p = [IO.Path]::ChangeExtension($_.FullName, '.counters.json')
                if (Test-Path $p) { Get-Content $p -Raw | ConvertFrom-Json } })
        $buffers = @($groups[$cfg] | ForEach-Object {
                $p = [IO.Path]::ChangeExtension($_.FullName, '.buffer.json')
                if (Test-Path $p) { Get-Content $p -Raw | ConvertFrom-Json } })
        if ($counters.Count -or $buffers.Count) {
            $res += [pscustomobject]@{
                Config = $cfg
                Game3d = Get-Mean $counters 'Game3dPct'; Ff3d = Get-Mean $counters 'Ffmpeg3dPct'
                FfVideo = Get-Mean $counters 'FfmpegVideoPct'; FfVram = Get-Mean $counters 'FfmpegVramMB'
                FfCpu = Get-Mean $counters 'FfmpegCpuPct'; RtCpu = Get-Mean $counters 'RetakeCpuPct'
                Mbps = Get-Mean $buffers 'Mbps'; GBh = Get-Mean $buffers 'GBPerHour'
            }
        }
    }
    $f = { param($v) if ($null -eq $v) { '—' } else { ([double]$v).ToString('0.0', $inv) } }
    $md = @("Gate: average FPS loss ≤ $script:GateAvgLossPct%, 1% low loss ≤ $script:GateLow1LossPct%, on the mean of each config's runs.", '',
        (Format-ResultTable -Rows $rows))
    if ($res.Count) {
        $md += '', '| Config | Game 3D % | ffmpeg 3D % | ffmpeg video % | ffmpeg VRAM MB | ffmpeg CPU % | retake CPU % | Mbit/s | GB/hour |',
        '| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |'
        foreach ($r in $res) {
            $md += "| $($r.Config) | $(& $f $r.Game3d) | $(& $f $r.Ff3d) | $(& $f $r.FfVideo) | $(& $f $r.FfVram) | $(& $f $r.FfCpu) | $(& $f $r.RtCpu) | $(& $f $r.Mbps) | $(& $f $r.GBh) |"
        }
    }
    $text = $md -join "`n"
    Set-Content -Path (Join-Path $dir 'summary.md') -Value $text -Encoding utf8
    $text
}

if ($PSCmdlet.ParameterSetName -eq 'Report') {
    Invoke-Report (Resolve-Path $Report).Path
    return
}

# ---- Run mode ----
# `pwsh -File bench.ps1 -Configs a,b` passes "a,b" as one string, so split on commas.
$Configs = @($Configs | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ })
foreach ($c in $Configs) {
    if (-not $Overlays.Contains($c)) { throw "unknown config '$c'; known: $($Overlays.Keys -join ', ')" }
}
if ($Configs -notcontains 'baseline') { throw 'the configs must include baseline' }
$root = Split-Path -Parent $PSScriptRoot
$presentMon = Join-Path $root 'third_party/presentmon/PresentMon.exe'
if (-not (Test-Path $presentMon)) { throw "PresentMon not found; run ./scripts/fetch-presentmon.ps1 first" }
$Install = (Resolve-Path $Install).Path
$retakeExe = Join-Path $Install 'retake.exe'
if (-not (Test-Path $retakeExe)) { throw "no retake.exe in $Install" }
$running = @(Get-Process -Name retake -ErrorAction SilentlyContinue)
if ($running) {
    throw "Retake is already running ($(($running | ForEach-Object { $_.Path }) -join ', ')). Quit it first; the bench starts its own."
}
if (-not $Out) { $Out = Join-Path $root ('bench/' + (Get-Date -Format 'yyyy-MM-dd_HHmmss')) }
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path
$procName = [IO.Path]::GetFileNameWithoutExtension($Process)
$cpus = [Environment]::ProcessorCount

function Get-BenchEnvironment {
    $cv = Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion'
    $ram = @(Get-CimInstance Win32_PhysicalMemory)
    $ffmpeg = @($env:RETAKE_FFMPEG, (Join-Path $Install 'ffmpeg\ffmpeg.exe'),
        (Join-Path $root 'third_party\ffmpeg\bin\ffmpeg.exe')) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
    $pmVersion = (Get-Content (Join-Path $root 'third_party/presentmon.lock') | Where-Object { $_ -match '^version=' }) -replace '^version=', ''
    $siege = Read-Host "Game build (from the game's menu; Enter to skip)"
    [ordered]@{
        Date       = (Get-Date).ToString('yyyy-MM-dd HH:mm', $inv)
        Cpu        = (Get-CimInstance Win32_Processor | Select-Object -First 1).Name.Trim()
        RamGB      = [math]::Round(($ram | Measure-Object Capacity -Sum).Sum / 1GB)
        RamMTs     = ($ram | Select-Object -First 1).ConfiguredClockSpeed
        Gpus       = @(Get-CimInstance Win32_VideoController | ForEach-Object { "$($_.Name) (driver $($_.DriverVersion))" })
        Windows    = "$($cv.ProductName) $($cv.DisplayVersion) build $($cv.CurrentBuild).$($cv.UBR)"
        Ffmpeg     = if ($ffmpeg) { (& $ffmpeg -hide_banner -version | Select-Object -First 1) } else { 'not found' }
        PresentMon = $pmVersion
        Game       = $Process
        GameBuild  = $siege
        Configs    = $Configs
        Runs       = $Runs
        Seconds    = $Seconds
    }
}

function Wait-Ffmpeg([int]$parent) {
    $deadline = (Get-Date).AddSeconds(30)
    while ((Get-Date) -lt $deadline) {
        $ff = Get-CimInstance Win32_Process -Filter "ParentProcessId=$parent AND Name='ffmpeg.exe'" |
            Where-Object { $_.CommandLine -match 'ddagrab' } | Select-Object -First 1
        if ($ff) { return $ff }
        Start-Sleep -Milliseconds 500
    }
    throw "Retake did not start recording within 30 s. Is $Process running? See Retake's log in $env:LOCALAPPDATA\Retake\logs."
}

function Start-CounterJob([int[]]$gamePids, [int]$ffPid, [int]$seconds) {
    $paths = @()
    foreach ($p in $gamePids) { $paths += "\GPU Engine(pid_${p}_*)\Utilization Percentage" }
    if ($ffPid) {
        $paths += "\GPU Engine(pid_${ffPid}_*)\Utilization Percentage", "\GPU Process Memory(pid_${ffPid}_*)\Dedicated Usage"
    }
    Start-ThreadJob -ArgumentList $paths, $seconds -ScriptBlock {
        param($paths, $seconds)
        $out = @()
        foreach ($set in Get-Counter -Counter $paths -SampleInterval 1 -MaxSamples $seconds -ErrorAction SilentlyContinue) {
            $out += , @($set.CounterSamples | ForEach-Object {
                    [pscustomobject]@{ Path = $_.Path; Instance = $_.InstanceName; Value = $_.CookedValue } })
        }
        , $out
    }
}

function Measure-Counters($samples, [int[]]$gamePids, [int]$ffPid) {
    $game3d = @(); $ff3d = @(); $ffVideo = @(); $ffVram = 0.0
    foreach ($set in $samples) {
        $g = 0.0; $f3 = 0.0; $fv = 0.0
        foreach ($s in $set) {
            if ($s.Instance -notmatch '^pid_(?<pid>\d+)_') { continue }
            $p = [int]$Matches.pid
            if ($s.Path -match 'gpu process memory') {
                if ($p -eq $ffPid) { $ffVram = [math]::Max($ffVram, $s.Value / 1MB) }
                continue
            }
            $engine = ($s.Instance -split 'engtype_')[-1]
            if ($gamePids -contains $p -and $engine -eq '3d') { $g += $s.Value }
            if ($p -eq $ffPid -and $engine -eq '3d') { $f3 += $s.Value }
            if ($p -eq $ffPid -and $engine -match 'video codec|videoencode|encode') { $fv += $s.Value }
        }
        $game3d += $g; $ff3d += $f3; $ffVideo += $fv
    }
    $avg = { param($a) if ($a.Count) { ($a | Measure-Object -Average).Average } else { $null } }
    [ordered]@{
        Samples        = @($samples).Count
        Game3dPct      = & $avg $game3d
        Ffmpeg3dPct    = if ($ffPid) { & $avg $ff3d } else { $null }
        FfmpegVideoPct = if ($ffPid) { & $avg $ffVideo } else { $null }
        FfmpegVramMB   = if ($ffPid) { $ffVram } else { $null }
    }
}

function Get-CpuSeconds([int]$id) {
    $p = Get-Process -Id $id -ErrorAction SilentlyContinue
    if ($p) { $p.TotalProcessorTime.TotalSeconds } else { $null }
}

$configPath = Join-Path $Install 'config.toml'
$backup = Join-Path $Install 'config.toml.bench-backup'
$hadConfig = Test-Path $configPath
if (Test-Path $backup) { throw "$backup exists from an earlier bench; put it back as config.toml or delete it first" }
if ($hadConfig) { Copy-Item $configPath $backup }

$envInfo = Get-BenchEnvironment
$envInfo | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $Out 'env.json') -Encoding utf8
Write-Host "Results go to $Out"
if (-not (Get-Process -Name $procName -ErrorAction SilentlyContinue)) {
    Read-Host "$Process isn't running. Start it, get into the custom game, then press Enter"
}

$retake = $null
try {
    foreach ($cfg in $Configs) {
        $overlay = $Overlays[$cfg]
        for ($n = 1; $n -le $Runs; $n++) {
            $stem = Join-Path $Out "$cfg-$n"
            $ff = $null
            if ($null -ne $overlay) {
                ConvertTo-OverlayToml $overlay $Process | Set-Content -Path $configPath -Encoding utf8
                $retake = Start-Process -FilePath $retakeExe -WorkingDirectory $Install -PassThru
                $ff = Wait-Ffmpeg $retake.Id
                Write-Host "Retake is recording ($cfg; ffmpeg pid $($ff.ProcessId))."
            }
            Read-Host "Run $n/$Runs of '$cfg': go to the route's start, press Enter, then play the route"
            $gamePids = @(Get-Process -Name $procName -ErrorAction SilentlyContinue | ForEach-Object Id)
            if (-not $gamePids) { throw "$Process is not running" }
            $ffPid = if ($ff) { [int]$ff.ProcessId } else { 0 }
            $cpu0 = @{ ff = if ($ffPid) { Get-CpuSeconds $ffPid }; rt = if ($retake) { Get-CpuSeconds $retake.Id } }
            $t0 = Get-Date
            $job = Start-CounterJob $gamePids $ffPid $Seconds
            & $presentMon --process_name $Process --timed $Seconds --terminate_after_timed `
                --no_console_stats --stop_existing_session --output_file "$stem.csv" | Out-Null
            if ($LASTEXITCODE) { throw "PresentMon exited with $LASTEXITCODE" }
            $samples = Receive-Job -Job $job -Wait -AutoRemoveJob
            $wall = ((Get-Date) - $t0).TotalSeconds
            $m = Measure-Counters $samples $gamePids $ffPid
            $cpu = { param($id, $before) $after = Get-CpuSeconds $id
                if ($null -ne $before -and $null -ne $after) { ($after - $before) / ($wall * $cpus) * 100 } }
            $m.FfmpegCpuPct = if ($ffPid) { & $cpu $ffPid $cpu0.ff } else { $null }
            $m.RetakeCpuPct = if ($retake) { & $cpu $retake.Id $cpu0.rt } else { $null }
            $m | ConvertTo-Json | Set-Content "$stem.counters.json" -Encoding utf8
            if ($ff -and $ff.CommandLine -match '-segment_list\s+"?(?<idx>[^"]+?index\.csv)') {
                Get-BufferRate -IndexCsv $Matches.idx | ConvertTo-Json | Set-Content "$stem.buffer.json" -Encoding utf8
            }
            if (Test-Path "$stem.csv") {
                $s = Get-FrameStats -FrameTimes (Read-FrameTimes -Path "$stem.csv")
                Write-Host ("  {0}: {1:0.0} fps avg, {2:0.0} 1% low, {3} frames" -f $cfg, $s.AvgFps, $s.Low1, $s.Frames)
            } else { Write-Warning "PresentMon wrote no $stem.csv (did $Process present any frames?)" }
            if ($retake) {
                Stop-Process -Id $retake.Id -ErrorAction SilentlyContinue
                $retake.WaitForExit(10000) | Out-Null
                $retake = $null
            }
        }
    }
}
finally {
    if ($retake) { Stop-Process -Id $retake.Id -ErrorAction SilentlyContinue; $retake.WaitForExit(10000) | Out-Null }
    if ($hadConfig) { Move-Item -Force $backup $configPath }
    elseif (Test-Path $configPath) { Remove-Item $configPath }
    Write-Host "config.toml restored."
}

Write-Host ''
Invoke-Report $Out
