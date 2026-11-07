# Reliability

What Retake does when something goes wrong, and when each case was last tested. Every row either recovers on its own or shows a tray error with one clear log line. Stand-in game = `examples/avsync.rs` (see `docs/sync.md`); automated tests never launch Siege.

| Event | Expected behaviour | How it's handled | Tested |
|---|---|---|---|
| Game crashes | Stop recording; the buffer stays saveable so F9 can still catch the crash moment | The watcher sees the process gone and stops after 5 s; segments stay until the next session starts | — |
| ffmpeg crashes | Restart with backoff; audio reconnects to the new pipes | Watcher backoff 1/2/5 s (Failed after 5 restarts in 10 min); each run creates fresh pipes and a new pump | — |
| Mic unplugged | Mic track continues as silence; tray warning; reattach when it's back | The capture thread ends; the pump keeps writing silence, turns the tray yellow and restarts the capture every 2 s | — |
| Default output device changes | Process loopback follows; device loopback and the default mic follow too | Process loopback is a virtual device Windows re-routes; for `device` and the default mic the pump compares the default device id every 2 s and restarts the capture | — |
| Disk below 2 GB free | Stop recording, tray error, don't fill the disk | `DiskGuard` on every poll: below 2 GB the session stops at once and the icon shows Failed; above 3 GB recording may start again | — |
| Clips drive missing | Saves fail with a tray error; the buffer keeps running | The save job fails, a toast says why, the recorder is untouched | — |
| PC sleeps / resumes | Stop on suspend, restart on resume if the game still runs | `WM_POWERBROADCAST`: `PBT_APMSUSPEND` stops at once; `PBT_APMRESUMEAUTOMATIC` lets the watcher start again | — |
| Display mode change | ddagrab errors → the pipeline restarts | ffmpeg exits on the duplication error; the watcher restarts it with backoff | — |
| Two F9 during a save | Queue; never two concat jobs at once | One saver thread works through presses in order | — |
