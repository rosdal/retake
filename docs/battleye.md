# BattlEye

Before v1.0 is tagged, BattlEye support gets a short description of Retake with the signed release and its hashes; their FAQ invites players to ask. Their reply goes at the bottom of this page.

**Status:** not sent yet. Retake's maintainer sends it from their own address once a signed release exists; nobody else should send it on their behalf.

## Draft

> **Subject:** Third-party game recorder "Retake": request for review before release
>
> Hello,
>
> I'm releasing a small open-source game recorder for Windows called Retake (MIT licence, source: https://github.com/rosdal/retake). Many of its users will play Rainbow Six Siege, and before the 1.0 release I'd like to make sure it can't be mistaken for cheating software.
>
> What it does, and doesn't:
>
> - It records the screen with Windows Desktop Duplication (the composited desktop), the same way screenshot and streaming tools do. No DLL injection, no DirectX or Present hooks, no "game capture".
> - It never opens Siege's process. It finds the process id by name from the system process list (`CreateToolhelp32Snapshot`) and uses it only for Windows' per-process audio loopback (`ActivateAudioInterfaceAsync` with `PROCESS_LOOPBACK`). A "strict" setting avoids even that and records the whole output device.
> - No kernel driver, no admin rights, no memory reading or writing, no input simulation (hotkeys use `RegisterHotKey`), and nothing is drawn over the game.
> - While Siege runs it only records. Everything else, including reading Siege's Match Replay files (with the open-source r6-dissect parser) to find the player's kills, happens after the game has closed.
> - Plain signed binaries with version information: `retake.exe`, `r6-dissect.exe` and `ffmpeg.exe`. SHA-256 hashes are listed below.
>
> [release link and SHA-256 hashes]
>
> Could you confirm that running it alongside Siege is acceptable, or tell me what to change?
>
> Thank you,
> [name]

## Reply

(none yet)
