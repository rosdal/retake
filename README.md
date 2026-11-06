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