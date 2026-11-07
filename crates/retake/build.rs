use std::process::Command;

fn main() {
    let hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=RETAKE_GIT_HASH={hash}");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/heads");
    version_resource(&hash);
}

/// A VERSIONINFO resource, so Windows file properties show the version and git hash: a plain,
/// identifiable binary (anti-cheat rule 9).
fn version_resource(hash: &str) {
    let v = env!("CARGO_PKG_VERSION");
    let mut n: Vec<u32> = v.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    n.resize(4, 0);
    let commas = n.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let rc = format!(
        r#"1 VERSIONINFO
FILEVERSION {commas}
PRODUCTVERSION {commas}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Retake"
      VALUE "FileDescription", "Retake game recorder"
      VALUE "FileVersion", "{v} ({hash})"
      VALUE "InternalName", "retake"
      VALUE "LegalCopyright", "MIT licence"
      VALUE "OriginalFilename", "retake.exe"
      VALUE "ProductName", "Retake"
      VALUE "ProductVersion", "{v}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    );
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    // The app icon (Retake's mark), drawn by retake-core.
    let ico = out.join("retake.ico");
    std::fs::write(&ico, retake_core::logo::app_ico()).expect("writing retake.ico");
    let rc = format!(
        "1 ICON \"{}\"\n{rc}",
        ico.to_string_lossy().replace('\\', "/")
    );
    let path = out.join("retake.rc");
    std::fs::write(&path, rc).expect("writing retake.rc");
    embed_resource::compile(&path, embed_resource::NONE)
        .manifest_optional()
        .expect("compiling the version resource");
}
