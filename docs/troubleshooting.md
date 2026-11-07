# Troubleshooting

Each entry: what you see, why, what to do. Retake's log is in `%LOCALAPPDATA%\Retake\logs` (tray → Open logs).

## Clips are black

- **Why:** the game runs in exclusive fullscreen, which Desktop Duplication can't always see. Retake warns about this at startup (`WindowMode=0` in Siege's settings).
- **Fix:** set the game to borderless fullscreen.

## Recording costs a lot of FPS

- **Why:** something is drawn over the game, most often Discord's in-game overlay (also Steam's, or a notification). A window over the game takes it off the fast fullscreen path ("independent flip"). Windows then composes every frame, and recording a composed screen is expensive: in the stand-in bench the cost went from about 0% to 55–80%.
- **Check:** PresentMon or `scripts/bench.ps1` shows `PresentMode = Composed: Flip` instead of `Hardware: Independent Flip`.
- **Fix:** turn off Discord's overlay for the game (Discord → Settings → Game Overlay), close other overlays, and play in borderless fullscreen.

## Colours look washed out or too dark

- **Why:** Windows HDR is on. Retake captures SDR and warns when the monitor's colour space is HDR.
- **Fix:** turn HDR off for gaming, or accept that colours will be off.

## No game audio in clips

- **Why:** process loopback needs Windows 10 2004 or later. Without it, Retake falls back to recording everything your speakers play. A game whose audio comes from a helper process is still caught, because the whole process tree is captured.
- **Fix:** update Windows. To capture all system sound on purpose, set `audio.game_source = "device"`.

## The mic track is silent and the tray is yellow

- **Why:** the microphone named in `audio.mic_device` isn't there (unplugged, renamed). Retake records silence on that track and retries every 2 seconds. The log lists the microphones it found.
- **Fix:** plug it back in, or set `mic_device = "default"`.

## Sound and picture are out of sync

- **Why:** almost always the game's own audio latency, which you also hear while playing. Retake itself puts every sound at its capture time.
- **Check:** `retake.exe sync-check <clip>` on a clip of single shots at a wall (`docs/sync.md`). A constant offset of 10–30 ms is normal.

## "Encoder falling behind" in the log

- **Why:** the hardware encoder can't keep up with the capture rate, usually at 240 fps with a heavy game.
- **Fix:** set `capture.fps = 120`, or `capture.preset = "speed"` if you changed it.

## Recording stopped: "disk almost full"

- **Why:** the buffer drive dropped below 2 GB free. Retake stops rather than fill the disk, and starts again above 3 GB.
- **Fix:** free some space, or move `capture.buffer_dir` and `clips.out_dir` to another drive.

## Auto-clips are missing

- **Why:** Siege isn't saving Match Replays, or a new season changed the replay format. Retake shows a message in both cases. Manual F9 always works.
- **Fix:** turn on Match Replay in Siege's settings. After a Siege update, update Retake.

## Windows SmartScreen warns about retake.exe

- **Why:** a new download that few people have run yet. Release builds are signed. A build you made yourself isn't.
- **Fix:** check the SHA-256 against the release notes, then choose "More info → Run anyway".

## Graded clips look wrong

- **Why:** the built-in look assumes Siege's default brightness (around 53–56).
- **Fix:** lower `grade.look_strength`, set `grade.mode = "off"`, or use your own `.cube` with `grade.look`. Raw clips are always kept with `output = "save_as"`.
