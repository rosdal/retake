//! The window's exe gets Retake's icon and a version resource, like retake.exe.

fn main() {
    let v = env!("CARGO_PKG_VERSION");
    let mut n: Vec<u32> = v.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    n.resize(4, 0);
    let commas = n.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let ico = out.join("retake.ico");
    std::fs::write(&ico, retake_core::logo::app_ico()).expect("writing retake.ico");
    let rc = format!(
        r#"1 ICON "{ico}"
1 VERSIONINFO
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
      VALUE "FileDescription", "Retake window"
      VALUE "FileVersion", "{v}"
      VALUE "InternalName", "retake-ui"
      VALUE "LegalCopyright", "MIT licence"
      VALUE "OriginalFilename", "retake-ui.exe"
      VALUE "ProductName", "Retake"
      VALUE "ProductVersion", "{v}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        ico = ico.to_string_lossy().replace('\\', "/")
    );
    let path = out.join("retake-ui.rc");
    std::fs::write(&path, rc).expect("writing retake-ui.rc");
    embed_resource::compile(&path, embed_resource::NONE)
        .manifest_optional()
        .expect("compiling the resources");
}
