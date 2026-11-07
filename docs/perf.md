# Performance gate (v0.4)

Recording must cost at most **3% average FPS** and **5% on 1% lows** in Siege, compared with Retake not running. This page holds the test route, the setup, how to run the comparison and every result, newest first. Rerun it at v0.6, v0.8 and v1.0.

## Metrics

Frame times come from [PresentMon](https://github.com/GameTechDev/PresentMon) 2.6.0 (`MsBetweenPresents`), the same data CapFrameX uses. Definitions match CapFrameX's defaults:

| Metric | Definition |
|---|---|
| Average FPS | frames ÷ total seconds |
| 1% low | 1000 ÷ the 99th-percentile frame time (nearest rank) |
| 0.1% low | 1000 ÷ the 99.9th-percentile frame time |
| Spikes | frames longer than 2× the run's median frame time |
| Gate | each loss is taken on the mean of a config's runs vs the mean of the baseline runs |

Per run the bench also records, from Windows perf counters: the game's 3D engine use, ffmpeg's 3D and video-codec engine use, ffmpeg's dedicated VRAM, CPU use of `ffmpeg.exe` and `retake.exe` (as a share of all logical CPUs), and the buffer's bitrate and GB per hour.

## Unattended stand-in route (Xonotic)

Siege can't be launched by an automated test (anti-cheat), so the gate is also run against [Xonotic](https://xonotic.org) 0.8.6 (GPL, no anti-cheat), replaying its bundled `demos/the-big-keybench` in real time, uncapped, borderless on the primary monitor. Every run sees the same frames and nobody has to play:

```powershell
# Xonotic unpacked in D:\bench-games\Xonotic (or set XONOTIC_DIR)
./scripts/bench.ps1 -Install C:\path\to\retake-folder -Process xonotic-bench.exe `
    -RunHook scripts\bench-hooks\xonotic.ps1 -NoPrompt
```

The hook runs Xonotic as `xonotic-bench.exe`, a name Discord doesn't recognise. Discord puts its overlay, a topmost window, over any game it knows, and that takes the game off the fast fullscreen path ("Hardware: Independent Flip"). Windows then composes every frame, and recording a composed screen cost 55–80% of the frame rate in this bench, against about 0% at independent flip. The same applies to Siege: keep overlays off.

Xonotic at ~620 fps is CPU-bound (the GPU's 3D engine is about 20% busy), so this route shows the cost of capture and encoding on a busy desktop but doesn't load the GPU the way Siege does. The Siege route below is still the real gate.

## Route (Siege)

Fill this in once and keep it fixed; runs are only comparable on the same route.

1. Map:
2. Operator and loadout:
3. Custom game settings (time of day, bots, etc.):
4. Start point:
5. Path, step by step, through the busiest area:
6. Gunfire (where, how much):
7. End point (about 60 s after the start):

## Locked variables

- Same Siege graphics preset, and the in-game FPS cap off (or above what you reach).
- Adrenalin at defaults; no Discord overlay; the same background apps for every run.
- Warm hardware: play 5 minutes before the first run.
- Siege in borderless fullscreen (Retake warns about exclusive fullscreen).
- Close your own Retake: the bench starts its own copy, with a temporary `config.toml`, and puts yours back afterwards. That config only sets what it tests; everything else is the default (so your own `output`, `buffer_dir` and so on don't apply during the bench).

## How to run

```powershell
./scripts/fetch-presentmon.ps1            # once
# Start Siege, load the custom game, play 5 minutes.
./scripts/bench.ps1 -Install C:\path\to\retake-folder
```

The script asks for the Siege build, then for each config and run says when to go to the route's start; press Enter and play the route. Each capture lasts `-Seconds` (60). Defaults: `-Configs baseline,default,hevc-120,hevc-balanced -Runs 3`. Other configs: `hevc-240`, `hevc-qp18-20`, `hevc-low-prio`.

| Config | What it changes |
|---|---|
| `baseline` | Retake not running |
| `default` | nothing: a new install (`fps = "auto"`, the fastest of 60/120/144/165/240 the monitor shows) |
| `hevc-120` | `fps = 120` |
| `hevc-240` | `fps = 240` |
| `hevc-balanced` | `fps = 120`, `preset = "balanced"` |
| `hevc-qp18-20` | `fps = 120`, QP 18/20 |
| `hevc-low-prio` | `fps = 120`, ffmpeg at below-normal CPU priority |

Results land in `bench/<date_time>/` (git-ignored): raw PresentMon CSVs, counters, `env.json` and `summary.md`. Paste `summary.md` and the environment into **Results** below. `./scripts/bench.ps1 -Report bench/<folder>` rebuilds the summary.

PresentMon needs an elevated shell or membership of *Performance Log Users*.

## Not measured

- **AV1**: refused at config load until clips can be saved from it (the MPEG-TS buffer stores AV1 as an unreadable data stream). Measure it when that's fixed.
- **NVENC and QSV**: no such GPU in the test PC.

## Defaults and why

HEVC, QP 16/18, `preset = "speed"`, `priority = "normal"`, `fps = "auto"` (240 on a 240 Hz+ monitor). Every config passed the gate on the stand-in route with room to spare (−0.4 to −0.5%), so nothing was tuned down: the roadmap's rule is not to over-tune once the gate passes easily, and image quality matters more than the last 0.5%. `preset = "balanced"` cost no more FPS than `speed`; it stays an option. 240 fps is the default where the monitor allows, because it doubles the frames for slow motion and motion blur.

The cost that does grow is disk: at 240 fps the ring writes about 240 Mb/s (≈109 GB/hour) into `buffer/`. That's over the ~50 GB/hour the roadmap flags as input for a RAM ring buffer after v1.0; it doesn't block the gate. On a game at its fps ceiling, Desktop Duplication delivered about 155 new frames per second of the 240 asked for (frames that didn't change aren't duplicated with audio on), so real 240 fps footage needs a game that isn't saturating the GPU's presentation.

Still to measure on the real route: Siege itself (GPU-bound), and AV1 once it can be saved.

## Results

Newest first.

### 2026-11-07, stand-in route (Xonotic demo), audio in its own ffmpeg

RX 7900 XTX (driver 32.0.32015.7029), Ryzen 7 7800X3D, 32 GB DDR5-6000, Windows build 26200.9445, ffmpeg 9.0.2 (gyan.dev full), PresentMon 2.6.0. Game and mic audio on (the default). Presentation: Hardware: Independent Flip in every run.

| Config | Runs | Avg FPS | Δ avg | 1% low | Δ 1% | 0.1% low | Spikes | Gate |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| baseline | 3 | 625.2 | — | 395.9 | — | 303.1 | 51.0 | — |
| default (240 fps) | 3 | 622.5 | -0.4% | 393.8 | -0.5% | 297.5 | 53.3 | pass |
| hevc-120 | 3 | 622.6 | -0.4% | 394.2 | -0.4% | 300.1 | 52.7 | pass |
| hevc-balanced | 3 | 622.3 | -0.5% | 393.3 | -0.7% | 300.9 | 53.0 | pass |

| Config | Game 3D % | ffmpeg 3D % | ffmpeg video % | ffmpeg VRAM MB | ffmpeg CPU % | retake CPU % | Mbit/s | GB/hour |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| baseline | 22.4 | — | — | — | — | — | — | — |
| default (240 fps) | 12.8 | 1.0 | 42.9 | 140.2 | 0.8 | 0.0 | 241.9 | 108.8 |
| hevc-120 | 13.7 | 0.5 | 65.2 | 145.2 | 0.4 | 0.0 | 145.6 | 65.5 |
| hevc-balanced | 13.6 | 0.5 | 63.9 | 145.2 | 0.5 | 0.0 | 143.1 | 64.4 |

Earlier the same night, with audio inside the recording ffmpeg, the FPS deltas were the same (−0.4/−0.5%) but ffmpeg only captured ~70 fps (the audio stalled it). Before that, with Discord's overlay on the game, Retake cost 55–80%; see the stand-in route above.
