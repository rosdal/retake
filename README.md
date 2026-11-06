# Retake

Retake is a headless Windows tray app that keeps the last 60 seconds of Rainbow Six Siege gameplay in a rolling on-disk buffer, encoded on the GPU's dedicated AMF encoder, and saves clips on a hotkey or a detected kill. Clips are then graded with a LUT and sharpened for montage editing. See the [design doc](https://claude.ai/artifact/YP9zEmY8jT51rQMtT6Hz4s).

## Development

```powershell
./scripts/fetch-ffmpeg.ps1   # downloads the pinned ffmpeg into third_party/ffmpeg/
cargo test
cargo run -- --console
```
