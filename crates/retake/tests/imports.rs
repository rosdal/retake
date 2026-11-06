//! Anti-cheat guard on what the shipped exe actually links. Scanning dependency sources can't
//! work (the `windows` crate defines every Win32 function), so this reads `retake.exe`'s PE
//! import table and fails if any forbidden function is imported, whoever pulled it in.

use object::LittleEndian as LE;
use object::read::pe::PeFile64;

/// Functions the anti-cheat rules forbid (memory access, injection, hooks, input automation).
const FORBIDDEN: &[&str] = &[
    "ReadProcessMemory",
    "WriteProcessMemory",
    "VirtualAllocEx",
    "VirtualAllocEx2",
    "CreateRemoteThread",
    "CreateRemoteThreadEx",
    "SetWindowsHookExA",
    "SetWindowsHookExW",
    "SendInput",
    "keybd_event",
    "mouse_event",
    "NtReadVirtualMemory",
    "NtWriteVirtualMemory",
];

/// `(dll, function)` for every by-name import of the exe.
fn imports() -> Vec<(String, String)> {
    let data = std::fs::read(env!("CARGO_BIN_EXE_retake")).expect("reading retake.exe");
    let pe = PeFile64::parse(&*data).expect("parsing retake.exe");
    let table = pe
        .import_table()
        .expect("reading the import table")
        .expect("retake.exe has an import table");
    let mut out = Vec::new();
    let mut descriptors = table.descriptors().expect("import descriptors");
    while let Some(d) = descriptors.next().expect("next descriptor") {
        let dll =
            String::from_utf8_lossy(table.name(d.name.get(LE)).expect("dll name")).into_owned();
        let mut thunks = table
            .thunks(d.original_first_thunk.get(LE))
            .expect("import thunks");
        while let Some(thunk) = thunks
            .next::<object::pe::ImageNtHeaders64>()
            .expect("next thunk")
        {
            if let Ok(object::read::pe::Import::Name(_, name)) =
                table.import::<object::pe::ImageNtHeaders64>(thunk)
            {
                out.push((dll.clone(), String::from_utf8_lossy(name).into_owned()));
            }
        }
    }
    out
}

#[test]
fn no_forbidden_imports() {
    let all = imports();
    assert!(
        all.len() >= 20,
        "only {} imports parsed: {all:?}",
        all.len()
    );
    let bad: Vec<_> = all
        .iter()
        .filter(|(_, f)| FORBIDDEN.contains(&f.as_str()))
        .collect();
    assert!(bad.is_empty(), "forbidden imports in retake.exe: {bad:?}");
}
