<#
.SYNOPSIS
  bench.ps1 run hook for Xonotic (open source, no anti-cheat), run as xonotic-bench.exe:
  restarts the game replaying
  demos/the-big-keybench in real time, uncapped, borderless on the primary monitor, so every
  run sees the same frames. Set XONOTIC_DIR (default D:\bench-games\Xonotic).
#>
$ErrorActionPreference = 'Stop'
$dir = if ($env:XONOTIC_DIR) { $env:XONOTIC_DIR } else { 'D:\bench-games\Xonotic' }
$src = Join-Path $dir 'xonotic.exe'
if (-not (Test-Path $src)) { throw "no xonotic.exe in $dir" }
# A copy under a name Discord doesn't know: Discord's overlay is a topmost window over any game
# it recognises, which takes the game off independent flip and makes every capture expensive.
# Run the bench with -Process xonotic-bench.exe.
$exe = Join-Path $dir 'xonotic-bench.exe'
if (-not (Test-Path $exe) -or (Get-Item $exe).Length -ne (Get-Item $src).Length) { Copy-Item $src $exe -Force }
Get-Process xonotic-bench, xonotic -ErrorAction SilentlyContinue | ForEach-Object { $_.Kill(); $_.WaitForExit(5000) | Out-Null }
$user = Join-Path (Split-Path $dir) 'xonotic-user'
$args = @('-userdir', $user, '+vid_fullscreen', '1', '+vid_desktopfullscreen', '1', '+vid_vsync', '0',
    '+cl_maxfps', '0', '+snd_volume', '0.1', '+playdemo', 'demos/the-big-keybench')
$p = Start-Process $exe -WorkingDirectory $dir -ArgumentList $args -PassThru
$deadline = (Get-Date).AddSeconds(30)
do { Start-Sleep -Milliseconds 500; $p.Refresh() } until ($p.MainWindowHandle -ne 0 -or (Get-Date) -gt $deadline)
if ($p.MainWindowHandle -eq 0) { throw 'Xonotic opened no window within 30 s' }
