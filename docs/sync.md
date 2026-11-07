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
