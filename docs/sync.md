# A/V sync

Retake's video and audio count from one origin per recording run. Video frames are stamped with ffmpeg's wall clock (`setpts=RTCTIME-origin`) and keep those timestamps. Audio packets are placed by their WASAPI capture time, converted to the same clock, with silence wherever nothing arrived. So nothing accumulates: the offset after three hours should be the offset after three seconds.

## Measuring

`retake.exe sync-check <clip.mkv> [--crop w:h:x:y] [--track N]` finds bright flashes in a region of the video (default: the centre fifth of the frame) and loud onsets in an audio track (default: track 0, Game). It pairs each flash with the nearest sound within 300 ms and prints every offset, the median and the worst. Positive = the sound comes after the picture. It exits 0 when the median is within ±8 ms (one frame at 120 fps) and no pair is off by more than 50 ms.

### In Siege

1. Custom game, any map, a weapon with a visible muzzle flash. Face a dark wall.
2. Single-tap the wall 10 times, about a second apart.
3. Press F9, then run `retake.exe sync-check <the clip>`.

The game's own audio latency (the time between the game making a sound and Windows mixing it) is part of what this measures. It is the same delay you hear while playing.

### With the stand-in

`examples/avsync.rs` is a window that turns white with a 1 kHz click once a second. It is the test game for the soak below, where no person is needed.

```powershell
cargo build --release; cargo build --example avsync
# config.toml next to retake.exe: [game] processes = ["avsync.exe"]
target\debug\examples\avsync.exe -1800 300        # window at x, y on screen
target\release\retake.exe --diag
target\release\retake.exe --save                   # whenever a clip is wanted
target\release\retake.exe sync-check <clip> --crop 200:200:<x>:<y>
```

The stand-in paints with GDI. On a 75 Hz monitor captured at 60 fps its own painting jitters by about ±25 ms, so single pairs scatter. The median is what to compare across clips.

## Results

Newest first.

| Date | Setup | After | Pairs | Median | Worst | Notes |
|---|---|---|---|---|---|---|
| 2026-11-07 | 3 h soak, avsync stand-in, 2560×1440 240 fps AMF, process loopback | 1 min | 19 | +45.4 ms | 47.5 ms | |
| 2026-11-07 | same | 30 min | 20 | +29.2 ms | 35.8 ms | |
| 2026-11-07 | same | 60 min | 20 | +22.5 ms | 26.2 ms | builds and a montage render ran on the PC around here |
| 2026-11-07 | same | 120 min | 20 | +32.6 ms | 34.3 ms | |
| 2026-11-07 | same | 180 min | 18 | +46.9 ms | 48.0 ms | |

### Reading the 3-hour soak

- **No drift.** The offset ends where it started (+45 ms → +47 ms after 3 h). In between it dipped to +22 ms while the PC was busy compiling and rendering, and came back. A clock drift would grow steadily in one direction; this doesn't.
- **The constant is the stand-in, not Retake.** The avsync window's own audio path (loopback latency, measured at about +46 ms) is in every number, so the ±8 ms target can't be judged with this test game. Measured against a reference without that latency it would show Retake's own error; see #27.
- **So no drift control is added.** Restarting the pipeline every `max_session_minutes` would only be worth it if drift grew; it doesn't.
- **Resources stayed flat** (`--diag`, every 5 min): `retake.exe` 17 → 22–25 MB and 291 handles from minute 5 to 180; ffmpeg 67–83 MB and 1169–1172 handles. No leaks.
