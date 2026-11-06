//! Which processes are running, from a Toolhelp snapshot. This never opens a process (the
//! anti-cheat rules forbid touching Siege's); it only reads the system's process list.

use anyhow::{Context, Result};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};

/// `(pid, exe file name, e.g. `RainbowSix.exe`)` of every running process.
pub fn processes() -> Result<Vec<(u32, String)>> {
    // SAFETY: the snapshot handle is closed below; `entry` is a properly sized out-struct.
    unsafe {
        let snap =
            CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).context("CreateToolhelp32Snapshot")?;
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut names = Vec::new();
        let mut more = Process32FirstW(snap, &mut entry).is_ok();
        while more {
            let end = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..end]);
            names.push((entry.th32ProcessID, name));
            more = Process32NextW(snap, &mut entry).is_ok();
        }
        let _ = CloseHandle(snap);
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn lists_its_own_pid() {
        let me = std::process::id();
        assert!(super::processes().unwrap().iter().any(|(p, _)| *p == me));
    }

    #[test]
    fn sees_its_own_process() {
        let me = std::env::current_exe().unwrap();
        let me = me.file_name().unwrap().to_str().unwrap();
        let names: Vec<String> = super::processes()
            .unwrap()
            .into_iter()
            .map(|(_, n)| n)
            .collect();
        assert!(
            names.iter().any(|n| n.eq_ignore_ascii_case(me)),
            "{me} not in {} names",
            names.len()
        );
    }
}
