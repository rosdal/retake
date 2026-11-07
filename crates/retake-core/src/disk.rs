//! Never filling the disk: recording stops when the buffer drive drops below 2 GB free and may
//! start again once it is back above 3 GB. The gap keeps it from flapping around one value.

/// Below this, recording stops.
pub const LOW_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Above this, recording may start again.
pub const OK_BYTES: u64 = 3 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskEvent {
    Low,
    Ok,
}

#[derive(Debug, Default)]
pub struct DiskGuard {
    low: bool,
}

impl DiskGuard {
    pub fn new() -> DiskGuard {
        DiskGuard::default()
    }

    /// Feeds the current free space; returns the change, if any.
    pub fn on_free(&mut self, free: u64) -> Option<DiskEvent> {
        if !self.low && free < LOW_BYTES {
            self.low = true;
            Some(DiskEvent::Low)
        } else if self.low && free > OK_BYTES {
            self.low = false;
            Some(DiskEvent::Ok)
        } else {
            None
        }
    }

    pub fn low(&self) -> bool {
        self.low
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn low_below_two_ok_above_three() {
        let mut g = DiskGuard::new();
        assert_eq!(g.on_free(10 * GB), None);
        assert_eq!(g.on_free(2 * GB - 1), Some(DiskEvent::Low));
        assert!(g.low());
        assert_eq!(g.on_free(GB), None);
        // Between 2 and 3 GB: still low, no flapping.
        assert_eq!(g.on_free(2 * GB + GB / 2), None);
        assert!(g.low());
        assert_eq!(g.on_free(3 * GB + 1), Some(DiskEvent::Ok));
        assert!(!g.low());
        assert_eq!(g.on_free(2 * GB + GB / 2), None);
    }
}
