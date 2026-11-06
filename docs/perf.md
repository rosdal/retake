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

## Route

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

Open until the runs are in. Today: HEVC, QP 16/18, `preset = "speed"`, `priority = "normal"`, `fps = "auto"`.

## Results

None yet.
