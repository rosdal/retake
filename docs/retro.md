# Retrospective (v0.1 to v1.6)

Written 2026-11-07, at the end of the run that built v0.5 to v1.6. Real numbers against the roadmap's targets, what worked, and what turned out harder than planned.

## Numbers against the targets

| Target | Result | Where |
|---|---|---|
| ≤ 3% average FPS loss, ≤ 5% on 1% lows | −0.4% / −0.5% at 240 fps on the Xonotic stand-in route (Siege itself not benchmarked: it is never launched by automation because of its anti-cheat) | `docs/perf.md` |
| Clip appears ~3 s after the hotkey | Stream copy of the ring buffer, no re-encode | v0.3 |
| A/V in sync ±8 ms after 3 h | Not provable on the stand-in: offsets measured between +22 and +45 ms with no steady drift, dominated by the stand-in's own loopback latency (~+46 ms) | `docs/sync.md`, #27 |
| Grade a clip in ~15 s | ~60 s for a 22 s 1440p clip (keyframe analysis, GPU decode, 16-bit LUT, CAS) | `docs/perf.md`, #29 |
| 25 s montage in under 3 min | 164 s, with motion-compensated interpolation | v1.3 |
| Service RAM with the window closed < 30 MB | 24 MB working set while recording (ffmpeg runs separately) | v1.6 |
| Window RAM open < 120 MB | 30–44 MB working set (CPU rendering) | v1.6 |

## What worked

- **Pure logic in `retake-core`, thin Windows glue.** 229 core tests cover config, ffmpeg arguments, the segment ring, sync maths, grading, the arranger, song analysis, the renderer's filter graph, game profiles, event parsers, the control protocol and the updater. Most bugs showed up there first.
- **Stream copy everywhere it can be.** Saving, auto-clips and trims never re-encode, so they're fast and lossless.
- **Doing nothing during a match.** Grading, exports, auto-clip analysis, thumbnails, update checks: all wait for the quiet gate. The perf gate passed with a lot of room as a result.
- **Official sources for kills.** Siege's replays (after the match), CS2's Game State Integration and League's Live Client API need no screen detection and touch nothing in the game.

## Harder than expected

- **Audio.** Audio inside the recording ffmpeg throttled capture to about 70 fps, because ffmpeg keeps its outputs within 100 ms of each other. Splitting audio into its own ffmpeg, stamping video with wall-clock time and placing audio by capture time fixed it, but took most of v0.5 and v0.6.
- **Overlays.** Discord's overlay on a known game forces composition and cost 55–80% of the frame rate in the bench. The bench renames the stand-in so Discord doesn't recognise it; the docs tell users to keep overlays off.
- **Beat tracking.** Getting tempo right without octave errors and off-beat locking needed flux below 3 kHz with a kick weight, a smoothed comb autocorrelation and a DP tracker.
- **The styled window's renderer.** iced 0.13's CPU renderer has three problems the window works around: canvas text makes NaN damage rectangles (labels are widgets instead), some fractional-size rectangles are dropped (shapes are snapped to pixels), and damage tracking misses canvas changes (each frame repaints fully). A fourth, canvases misplaced at display scaling above 100%, is fixed in a vendored, patched `iced_tiny_skia` (#23).
- **Things automation can't do here.** Siege can't be launched by automation, there's no NVIDIA or Intel GPU, no Resolve, and running downloaded installers or toolchains locally was blocked, so r6-dissect and the installer are built and tested in CI.

## Not done, and why

Everything that needs a person, a game with an anti-cheat, or hardware that isn't here is filed under the `post-1.0` label: code signing (#17), kill-feed layouts per game (#18), NVIDIA/Intel perf (#19), CS2/League live tests (#20), RIFE (#21), the Resolve import test (#22), keyboard focus (#24), in-place reload (#25), per-game capture settings (#26), real-hardware sync (#27), golden-frame tests (#28), faster grading (#29). The 20-match field test, the friend tests, the posted montages and the BattlEye email are yours.
