//! The monitors on the primary GPU, as `ddagrab` numbers them.

use anyhow::{Context, Result};
use retake_core::display::OutputInfo;
use windows::Win32::Graphics::Dxgi::Common::DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_NOT_FOUND, IDXGIFactory1, IDXGIOutput6,
};
use windows::Win32::Graphics::Gdi::{DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW};
use windows::core::{Interface, PCWSTR};

/// Every output of adapter 0, the adapter `-init_hw_device d3d11va` uses by default. Each one
/// is logged with its raw DXGI colour space.
pub fn outputs() -> Result<Vec<OutputInfo>> {
    // SAFETY: COM calls on interfaces we own; `mode` and `desc` outlive the calls using them.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().context("CreateDXGIFactory1")?;
        let adapter = match factory.EnumAdapters1(0) {
            Ok(a) => a,
            Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => return Ok(Vec::new()),
            Err(e) => return Err(e).context("EnumAdapters1"),
        };
        let mut list = Vec::new();
        for index in 0.. {
            let output = match adapter.EnumOutputs(index) {
                Ok(o) => o,
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(e).context("EnumOutputs"),
            };
            let desc = output
                .cast::<IDXGIOutput6>()
                .context("IDXGIOutput6")?
                .GetDesc1()
                .context("GetDesc1")?;
            let end = desc
                .DeviceName
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(desc.DeviceName.len());
            let name = String::from_utf16_lossy(&desc.DeviceName[..end]);
            let mut mode = DEVMODEW {
                dmSize: size_of::<DEVMODEW>() as u16,
                ..Default::default()
            };
            let ok = EnumDisplaySettingsW(
                PCWSTR(desc.DeviceName.as_ptr()),
                ENUM_CURRENT_SETTINGS,
                &mut mode,
            )
            .as_bool();
            let (width, height, refresh_hz) = if ok {
                (mode.dmPelsWidth, mode.dmPelsHeight, mode.dmDisplayFrequency)
            } else {
                let r = desc.DesktopCoordinates;
                ((r.right - r.left) as u32, (r.bottom - r.top) as u32, 0)
            };
            tracing::info!(
                index,
                name = %name,
                width,
                height,
                refresh_hz,
                color_space = desc.ColorSpace.0,
                "monitor"
            );
            list.push(OutputInfo {
                index,
                name,
                width,
                height,
                refresh_hz,
                hdr: desc.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020,
            });
        }
        Ok(list)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn enumerates_without_error() {
        // CI runners may have no monitor; this only checks the calls succeed.
        let outs = super::outputs().unwrap();
        for o in &outs {
            assert!(o.width > 0 && o.height > 0, "{o}");
        }
    }
}
