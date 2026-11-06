<div align="center">
# Retake
 
**A lightweight game recorder that clips your kills, 3Ks and aces, automatically or with one key.**
 
Install it, play, and your best plays are already saved as clean, graded clips. Turn them into a montage later if you want to.
 
[Features](#features) · [How it works](#how-it-works) · [Anti-cheat](#anti-cheat-safety) · [Roadmap](#roadmap) · [Development](#development)
 
</div>

> [!NOTE]
> Retake is in early development. This README describes what Retake is being built to do.
 
---

## Why Retake
 
Most clippers make you choose: lightweight but manual (ShadowPlay, Adrenalin, OBS), or automatic but heavy, ad-supported and watermarked on the free tier. Retake aims for both:
 
- **Zero setup.** Install it and play. Monitor, encoder, frame rate, storage and audio are picked automatically.
- **Clips first.** Every kill, 2K, 3K, 4K or ace becomes its own finished clip you can post straight away. Montages are an optional extra you start yourself.
- **Practically free while you play.** Video goes straight from the screen to your GPU's hardware encoder. While a game runs, Retake only records; everything else waits until you close the game.
- **Clips you can actually edit.** High frame rate, near-lossless video, separate audio tracks, colour-graded.
- **No account, no cloud, no ads, no watermark.** Everything stays on your PC.

## Features
 
| | |
| --- | --- |
| **Instant replay** | Press `F9` to save the last 60 seconds. Saving is a file copy, not a re-encode, so your game never stutters. |
| **Automatic clips** | After each match, every kill is clipped. Multi-kills become **one clip from your first kill to your last**, named for the play: `_2K`, `_3K`, `_4K`, `_ACE`. Choose every kill, 2K and up, 3K and up, or aces only. |
| **Smart manual clips** | Pressed `F9` a bit late on a 3K? Retake extends the clip back to your first kill of the round. |
| **Automatic grading** | Each clip gets its exposure and colour corrected and a clean, punchy look. Raw clips are kept unless you choose to overwrite them. |
| **Edit-ready** | 120–240 fps, near-lossless HEVC or AV1, game / mic / Discord on separate tracks, kill markers for your editor. |
| **Montages (optional)** | Pick a song and some clips; Retake puts your best play on the drop, lands every kill on a beat, and adds speed ramps, slow-mo and motion blur. Exports 9:16 for TikTok or 16:9 for YouTube, plus a DaVinci Resolve timeline. Never automatic; your clips stay untouched. |
| **Any game, any GPU** | Works with any game. AMD (AMF), NVIDIA (NVENC) and Intel (QSV) hardware encoders. |
| **Simple interface** | One window and a tray icon. No terminal, no config files to edit. |

## How it works
 
```
While you play                    After you close the game
-----------------                 -----------------------------------------------
Screen ─▶ GPU encoder ─▶ buffer   Game data ─┐
Game + mic audio ──────▶ buffer               ├─▶ your plays ─▶ cut ─▶ grade ─▶ clips
                                  Recording ──┘                               │
                                                        (optional) montage ◀──┘
```

1. **Recording.** Retake starts when it sees a known game. It captures the screen with Windows Desktop Duplication and encodes on the GPU into a rolling buffer on disk. Game audio, mic and (optionally) Discord are captured as separate tracks.
2. **Finding your plays.** `F9` saves the buffer any time. Automatic clips are found after the match, using the safest source each game offers:

   | Source | Used for |
   | --- | --- |
   | The game's own event API | Counter-Strike 2 (Valve's Game State Integration), League of Legends (Riot's Live Client Data API) |
   | The game's own replay files | Rainbow Six Siege (Match Replay files, parsed with [r6-dissect](https://github.com/redraskal/r6-dissect)) |
   | Detection on Retake's own recording | Games with a kill feed and no API or replays; models ship with Retake |
3. **Clipping.** Your kills are grouped per round, so a 3K is one clip from 15 s before the first kill to 3 s after the last.
4. **Grading.** Each clip is measured, corrected and given the look, baked into one 3D LUT and rendered in 10-bit on the GPU.
5. **Montages, if you want one.** The montage builder reads a song's beats and drop, arranges the clips you picked and renders the whole timeline in one GPU pass.

## Anti-cheat safety
 
Retake is designed to stay in the same category as OBS display capture and Discord screen share:
 
- **Never touches a game's process.** No injection, no hooks, no memory reading, no overlay, no input automation, no kernel driver.
- **Records the screen only while you play.** Replay parsing, kill detection and grading run after the game has closed.
- **Strict safety mode.** One switch makes Retake avoid even targeting the game's process for audio.
- **Ordinary, signed binaries.** No packing or obfuscation; hashes published with every release.
- **Guarded in CI.** Builds fail if code uses process-memory or input-injection APIs.
No third-party tool can be guaranteed safe with every anti-cheat. If you're unsure, use Strict safety mode.
 
## Requirements
 
- Windows 10 version 2004 or later, or Windows 11
- A GPU with a hardware video encoder: AMD Radeon (RX 5000 series or newer recommended), NVIDIA GeForce (GTX 16 / RTX series) or Intel Arc / recent Intel iGPU
- Games running in borderless or windowed fullscreen, with Windows HDR off for the game
- For automatic Rainbow Six Siege clips: Match Replay enabled in Siege (Retake tells you if it isn't)
- Free disk space for the buffer and your clips (an SSD is strongly recommended)

## Install
 
Not released yet. When it is: download the installer from [Releases](../../releases), run it, and start a game. There is nothing to configure.

## Development
 
Requires the stable Rust toolchain (`x86_64-pc-windows-msvc`) on Windows.
 
```powershell
./scripts/fetch-ffmpeg.ps1   # downloads the pinned ffmpeg into third_party/ffmpeg/
cargo test
cargo run -- --console       # runs with logs in the terminal
```
 
The workspace is split into `retake` (the app), `retake-core` (pure, tested logic), `retake-audio` (WASAPI capture) and `retake-detect` (play detection).
 
## FAQ
 
**Do I have to make montages?**
No. Retake is a clipper first: every play is saved as its own clip. The montage builder is there if you want it.
 
**Why not just use Medal, Outplayed or AMD's recorder?**
They're good tools. Retake is for people who want automatic clips without an account, ads, a watermark or a heavy background app, and who want footage that holds up in a real editor.
 
**Does Retake upload anything?**
No. Clips stay on your PC unless you share them yourself.
 
**Will it hurt my FPS?**
Encoding runs on the GPU's dedicated video engine, and nothing else runs while you play. The target is under 3% average FPS loss; measured results will be published per release.
 
**Can I get banned for using it?**
Retake is built to avoid everything anti-cheat systems look for; see [Anti-cheat safety](#anti-cheat-safety). No third-party tool can promise zero risk.
 
## Acknowledgements
 
- [FFmpeg](https://ffmpeg.org) for capture, encoding and filtering
- [r6-dissect](https://github.com/redraskal/r6-dissect) for Rainbow Six Siege replay parsing
- [native-windows-gui](https://github.com/gabdube/native-windows-gui) for the interface
- [blur](https://github.com/f0e/blur) and the frame-blending community for motion-blur techniques
## License
 
To be decided before the first release. Bundled third-party components keep their own licences; see `THIRD_PARTY_LICENSES.txt` in each release.