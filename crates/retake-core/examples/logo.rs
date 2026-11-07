//! Writes the app icon and a status-dot preview: `cargo run -p retake-core --example logo <dir>`.
fn main() {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| ".".into()));
    std::fs::write(dir.join("retake.ico"), retake_core::logo::app_ico()).unwrap();
    use retake_core::logo::*;
    let imgs: Vec<(u32, Vec<u8>)> = [IDLE, ACCENT, WARNING, ERROR]
        .iter()
        .map(|c| (32, rgba(32, *c, true)))
        .collect();
    std::fs::write(dir.join("tray.ico"), ico(&imgs)).unwrap();
}
