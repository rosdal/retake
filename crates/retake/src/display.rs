//! The monitors on the primary GPU, as `ddagrab` numbers them.

use anyhow::{Context, Result};
use retake_core::display::OutputInfo;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020, DXGI_COLOR_SPACE_TYPE,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_NOT_FOUND, IDXGIFactory1, IDXGIOutput6,
};
use windows::Win32::Graphics::Gdi::{DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW};
use windows::core::{Interface, PCWSTR};

/// Every output of adapter 0, the adapter `-init_hw_device d3d11va` uses by default. Each one
/// is logged with its raw DXGI colour space.
pub fn outputs() -> Result<Vec<OutputInfo>> {
    // SAFETY: COM calls on interfaces we own; `mode` and `device` outlive the calls using them.
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
            // IDXGIOutput6 carries the colour space; without it (old drivers, remote or
            // basic display adapters) fall back to the plain description and assume SDR.
            let (device, rect, color_space) =
                match output.cast::<IDXGIOutput6>().and_then(|o| o.GetDesc1()) {
                    Ok(d) => (d.DeviceName, d.DesktopCoordinates, d.ColorSpace),
                    Err(_) => {
                        let d = output.GetDesc().context("GetDesc")?;
                        (d.DeviceName, d.DesktopCoordinates, DXGI_COLOR_SPACE_TYPE(0))
                    }
                };
            let end = device.iter().position(|&c| c == 0).unwrap_or(device.len());
            let name = String::from_utf16_lossy(&device[..end]);
            let mut mode = DEVMODEW {
                dmSize: size_of::<DEVMODEW>() as u16,
                ..Default::default()
            };
            let ok =
                EnumDisplaySettingsW(PCWSTR(device.as_ptr()), ENUM_CURRENT_SETTINGS, &mut mode)
                    .as_bool();
            let (width, height, refresh_hz) = if ok {
                (mode.dmPelsWidth, mode.dmPelsHeight, mode.dmDisplayFrequency)
            } else {
                // 0 = unknown refresh rate; check_output doesn't warn about it.
                (
                    (rect.right - rect.left) as u32,
                    (rect.bottom - rect.top) as u32,
                    0,
                )
            };
            tracing::info!(
                index,
                name = %name,
                width,
                height,
                refresh_hz,
                color_space = color_space.0,
                "monitor"
            );
            list.push(OutputInfo {
                index,
                name,
                width,
                height,
                refresh_hz,
                hdr: color_space == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020,
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
