//! Checking the monitor `capture.output` points at.

use std::fmt;

/// One DXGI output (monitor) on the primary GPU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputInfo {
    /// The `ddagrab` `output_idx`.
    pub index: u32,
    /// e.g. `\\.\DISPLAY1`.
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub refresh_hz: u32,
    /// Windows HDR (BT.2020 PQ) is on.
    pub hdr: bool,
    /// The monitor holding the desktop origin (Windows' main display).
    pub primary: bool,
    /// Its `HMONITOR` as a number, to match a window's monitor; 0 = unknown.
    pub monitor: isize,
}

impl fmt::Display for OutputInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let range = if self.hdr { "HDR" } else { "SDR" };
        write!(
            f,
            "{}: {} {}x{} @ {} Hz {range}",
            self.index, self.name, self.width, self.height, self.refresh_hz
        )
    }
}

/// Finds monitor `output`. `Err` if it doesn't exist; otherwise the warnings about it.
pub fn check_output(outputs: &[OutputInfo], output: u32, fps: u32) -> Result<Vec<String>, String> {
    let Some(o) = outputs.iter().find(|o| o.index == output) else {
        if outputs.is_empty() {
            return Err(format!(
                "capture.output = {output}: no monitors found on the primary GPU"
            ));
        }
        let list: Vec<String> = outputs.iter().map(ToString::to_string).collect();
        return Err(format!(
            "capture.output = {output}: no such monitor. Monitors: {}",
            list.join(", ")
        ));
    };
    let mut warnings = Vec::new();
    // Windows rounds fractional modes down (143.98 Hz → 143), so allow 1 Hz; 0 = unknown.
    if o.refresh_hz != 0 && o.refresh_hz + 1 < fps {
        warnings.push(format!(
            "monitor {} ({}) runs at {} Hz, below capture.fps = {fps}: frames will repeat",
            o.index, o.name, o.refresh_hz
        ));
    }
    if o.hdr {
        warnings.push(format!(
            "monitor {} ({}) has Windows HDR on: colours will be off and files larger",
            o.index, o.name
        ));
    }
    Ok(warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(index: u32, hz: u32, hdr: bool) -> OutputInfo {
        OutputInfo {
            index,
            name: format!(r"\.\DISPLAY{}", index + 1),
            width: 2560,
            height: 1440,
            refresh_hz: hz,
            hdr,
            primary: index == 0,
            monitor: 0,
        }
    }

    #[test]
    fn fine_monitor_has_no_warnings() {
        assert_eq!(check_output(&[out(0, 165, false)], 0, 120), Ok(vec![]));
    }

    #[test]
    fn missing_index_lists_monitors() {
        let e = check_output(&[out(0, 165, false), out(1, 60, true)], 2, 120).unwrap_err();
        assert_eq!(
            e,
            r"capture.output = 2: no such monitor. Monitors: 0: \.\DISPLAY1 2560x1440 @ 165 Hz SDR, 1: \.\DISPLAY2 2560x1440 @ 60 Hz HDR"
        );
    }

    #[test]
    fn no_monitors_at_all() {
        assert_eq!(
            check_output(&[], 0, 120).unwrap_err(),
            "capture.output = 0: no monitors found on the primary GPU"
        );
    }

    #[test]
    fn slow_monitor_and_hdr_warn() {
        let w = check_output(&[out(0, 60, true)], 0, 120).unwrap();
        assert_eq!(
            w,
            [
                r"monitor 0 (\.\DISPLAY1) runs at 60 Hz, below capture.fps = 120: frames will repeat",
                r"monitor 0 (\.\DISPLAY1) has Windows HDR on: colours will be off and files larger",
            ]
        );
    }

    #[test]
    fn fractional_modes_rounded_down_are_fine() {
        // Windows reports 143.98 Hz as 143 and 59.94 Hz as 59.
        assert_eq!(check_output(&[out(0, 143, false)], 0, 144), Ok(vec![]));
        assert_eq!(check_output(&[out(0, 59, false)], 0, 60), Ok(vec![]));
        assert_eq!(
            check_output(&[out(0, 142, false)], 0, 144).unwrap().len(),
            1
        );
    }

    #[test]
    fn unknown_refresh_rate_is_not_a_warning() {
        assert_eq!(check_output(&[out(0, 0, false)], 0, 120), Ok(vec![]));
    }

    #[test]
    fn refresh_equal_to_fps_is_fine() {
        assert_eq!(check_output(&[out(0, 120, false)], 0, 120), Ok(vec![]));
    }
}
