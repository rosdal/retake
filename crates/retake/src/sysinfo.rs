//! Small facts about the machine, for the startup log line.

use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
use windows::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
};
use windows::core::HSTRING;

const CURRENT_VERSION: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";

fn reg_string(value: &str) -> Option<String> {
    let mut buf = [0u16; 128];
    let mut len = size_of_val(&buf) as u32;
    // SAFETY: buf and len describe a valid writable buffer.
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            &HSTRING::from(CURRENT_VERSION),
            &HSTRING::from(value),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
    };
    rc.is_ok().then(|| {
        let chars = (len as usize / 2).saturating_sub(1);
        String::from_utf16_lossy(&buf[..chars])
    })
}

fn reg_dword(value: &str) -> Option<u32> {
    let mut data = 0u32;
    let mut len = size_of::<u32>() as u32;
    // SAFETY: data and len describe a valid writable u32.
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            &HSTRING::from(CURRENT_VERSION),
            &HSTRING::from(value),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut data).cast()),
            Some(&mut len),
        )
    };
    rc.is_ok().then_some(data)
}

/// e.g. `26300.7171`, or `unknown`.
pub fn windows_build() -> String {
    match (reg_string("CurrentBuildNumber"), reg_dword("UBR")) {
        (Some(b), Some(u)) => format!("{b}.{u}"),
        (Some(b), None) => b,
        _ => "unknown".to_owned(),
    }
}

/// Description of DXGI adapter 0, e.g. `AMD Radeon RX 7900 XTX`, or `unknown`.
pub fn gpu_name() -> String {
    // SAFETY: COM factory creation and adapter enumeration; no raw pointers kept.
    let desc = unsafe {
        CreateDXGIFactory1::<IDXGIFactory1>()
            .and_then(|f| f.EnumAdapters1(0))
            .and_then(|a| a.GetDesc1())
    };
    match desc {
        Ok(d) => {
            let end = d
                .Description
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(d.Description.len());
            String::from_utf16_lossy(&d.Description[..end])
        }
        Err(_) => "unknown".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_build_is_numeric() {
        let b = windows_build();
        assert!(b.split('.').all(|p| p.parse::<u32>().is_ok()), "{b}");
    }

    #[test]
    fn gpu_name_is_not_empty() {
        assert!(!gpu_name().is_empty());
    }
}
