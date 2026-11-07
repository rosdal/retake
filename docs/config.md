# Configuration

Every key is optional; anything left out takes the default shown. `config.toml` sits next to `retake.exe` and is written with these defaults, as comments, on first run. Unknown keys are errors, so a typo fails loudly instead of being ignored. Relative paths are resolved against the config's folder.

## [game]

| Key | Type | Default | What it does |
|---|---|---|---|
| `builtin` | bool | true | Record the games in Retake's built-in list with no setup (Siege, CS2, Valorant, League, Apex, Fortnite, Overwatch, Call of Duty, PUBG, Marvel Rivals, The Finals, Deadlock, Dota 2, Rocket League, Xonotic) |
| `processes` | list of exe names | `[]` | More games to record, with the default profile (case-insensitive) |
| `any_fullscreen` | bool | false | Record any app that covers a whole monitor, except Explorer, browsers and video players |

## [games.\<id\>]

Per-game changes. `<id>` is a built-in game (`siege`, `cs2`, `valorant`, `lol`, `apex`, `fortnite`, `overwatch`, `cod`, `pubg`, `marvel-rivals`, `the-finals`, `deadlock`, `dota2`, `rocket-league`, `xonotic`) or a name of your own for a game Retake doesn't know ("Add game…" in Settings writes one). Every key is optional.

| Key | Type | What it does |
|---|---|---|
| `name` | string | Display name (your own games) |
| `exes` | list of exe names | Exe names for this game |
| `autoclip` | bool | Automatic clips on or off for this game. Kill-feed games (Valorant, Apex, Fortnite, …) are off until turned on: their layouts aren't checked against real matches yet |
| `strict` | bool | Strict safety for this game. Riot Vanguard games (Valorant, League) are strict by default; turning it off warns at start |
| `look` | path | A `.cube` look for this game's clips in montages |
| `feed` | `[x, y, w, h]` | Kill-feed region as fractions of the frame |
| `hud` | list of `[x, y, w, h]` | HUD areas left out of grading measurements |
| `crop_center` | 0–1 | Centre of the 9:16 crop (ultrawide, off-centre HUDs) |
| `fps` | 60, 120, 144, 165, 240 | Capture frame rate for this game (instead of `[capture] fps`) |
| `preset` | `"speed"`, `"balanced"`, `"quality"` | Encoder preset for this game |

How automatic clips find kills, per game: Siege reads its Match Replays after the match; CS2 uses Valve's Game State Integration (Retake writes `gamestate_integration_retake.cfg` into CS2's cfg folder while CS2 is closed); League uses Riot's Live Client Data API; other kill-feed games scan the kill feed in Retake's own recording after the match.

## [capture]

| Key | Type | Default | Range | What it does |
|---|---|---|---|---|
| `output` | int or `"auto"` | `"auto"` | monitor index | Monitor to record; auto = the one the game's window is on, followed if it moves |
| `fps` | int or `"auto"` | `"auto"` | 60, 120, 144, 165, 240 | Capture rate; auto = the fastest of these the monitor shows |
| `encoder` | `"amf"`, `"qsv"`, `"nvenc"`, `"x264"`, `"auto"` | `"auto"` | | Hardware encoder; auto = from the GPU maker. `x264` records on the CPU (60 fps at most) for testing on a PC without a hardware encoder |
| `codec` | `"hevc"` | `"hevc"` | | `"av1"` is refused until clips can be saved from it |
| `qp_i`, `qp_p` | int | 16, 18 | 0–51, `qp_i` ≤ `qp_p` | Constant QP; lower is better and bigger |
| `preset` | `"speed"`, `"balanced"`, `"quality"` | `"speed"` | | Encoder speed/quality trade-off |
| `priority` | `"normal"`, `"below_normal"` | `"normal"` | | CPU priority of the recording ffmpeg |
| `buffer_dir` | path or `"auto"` | `"auto"` | | Ring buffer folder; auto = `Retake\buffer` on the fastest local drive with 20 GB free |
| `buffer_secs` | int | 60 | ≥ pre_roll + post_roll + 4 | Length of the ring buffer |

## [audio]

| Key | Type | Default | What it does |
|---|---|---|---|
| `game` | bool | true | Game audio track |
| `game_source` | `"process"`, `"device"` | `"process"` | Only the game's own sound (Windows 10 2004+), or everything the speakers play |
| `mic` | bool | true | Microphone track |
| `mic_device` | string | `"default"` | Microphone by its Windows name |
| `mic_gate_db` | number | -50 | Noise gate threshold in dBFS; 0 turns it off |
| `comms` | bool | false | Discord as a third track (only when Discord runs) |

## [clips]

| Key | Type | Default | What it does |
|---|---|---|---|
| `out_dir` | path or `"auto"` | `"auto"` | Where clips go (`raw/`, `graded/`, `tmp/`) |
| `pre_roll`, `post_roll` | int seconds | 15, 3 | Kept before and after the trigger |
| `hotkey`, `hotkey_full` | key | `"F9"`, `"Ctrl+F9"` | Standard clip and full-buffer clip |
| `sound_cue` | bool | true | Short sound on save |
| `max_raw_gb` | number | 0 | Delete the oldest raw clips that have a graded version past this size; 0 = off |

## [autoclip]

| Key | Type | Default | What it does |
|---|---|---|---|
| `enabled` | bool | true | Clip the player's kills automatically after the match |
| `clip` | `"every"`, `"2k"`, `"3k"`, `"aces"`, `"off"` | `"every"` | Which plays are clipped |

## [grade]

| Key | Type | Default | Range | What it does |
|---|---|---|---|---|
| `mode` | `"on"`, `"off"` | `"on"` | | Grade every saved clip after the game closes |
| `output` | `"save_as"`, `"overwrite"` | `"save_as"` | | Keep the raw clip, or replace it once the graded one is checked |
| `suffix` | string | `"_graded"` | | `clip.mkv` → `clip_graded.mkv` |
| `look` | `"builtin"` or `.cube` path | `"builtin"` | | The look LUT |
| `look_strength` | number | 1.0 | 0–1 | How much of the look |
| `auto_exposure`, `exposure_amount` | bool, number | true, 0.5 | 0–1 | Levels towards black 0.03, white 0.95, median 0.42 (dark clips stay dark) |
| `auto_white_bal`, `wb_amount` | bool, number | true, 0.3 | 0–1 | Neutralise the midtone cast, capped at ±6% |
| `contrast` | number | 1.12 | 1.0–1.3 | S-curve around mid-grey |
| `vibrance` | number | 0.15 | 0–1 | Boosts muted colours more than saturated ones |
| `sharpen` | number | 0.35 | 0–1 | CAS strength; 0 = off |
| `hud_mask` | bool | true | | Ignore HUD areas when measuring |
| `hud_protect` | bool | false | | Blend half of the ungraded image back into the HUD areas; keeps UI colours but can leave a visible box over scenery |
| `bit_depth` | int | 10 | 8 or 10 | HEVC Main10 or Main |

## [export]

| Key | Type | Default | What it does |
|---|---|---|---|
| `when` | `"after_game"`, `"manual"` | `"after_game"` | Exports run 30 s after the game closes, or only from the tray |
| `presets` | list | `["edit"]` | `edit` (graded HEVC as MP4), `edit-smooth` (DNxHR HQ), `share` (1080p60 H.264, audio mixed), `vertical` (1080×1920 centre crop) |

## [motion_blur]

| Key | Type | Default | Range | What it does |
|---|---|---|---|---|
| `enabled` | bool | false | | Blend high-fps frames into each output frame |
| `apply_to` | list | `["share", "vertical"]` | never `edit` | Which exports get blur |
| `output_fps` | int | 60 | 24–120 | Frame rate after blending |
| `amount` | number | 1.0 | 0.1–3 | 0.2–0.3 low, 0.5 medium, 1.0 normal, more is heavy |
| `weighting` | `"equal"`, `"gaussian_sym"`, `"pyramid"` | `"gaussian_sym"` | | How blended frames are weighted |

## [update]

| Key | Type | Default | What it does |
|---|---|---|---|
| `check` | bool | true | Look for a new release once a day (only while no game runs). Installed copies download it, check its SHA-256 against the release (and its signature when Retake is signed) and install it the next time Retake starts; portable copies only say it exists |
