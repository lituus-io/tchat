use std::fmt;

/// Timestamp in microseconds since Unix epoch.
///
/// Google Chat uses microsecond-precision timestamps internally.
/// This is a `Copy` newtype — 8 bytes, trivially comparable and hashable.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(pub u64);

impl Timestamp {
    pub const ZERO: Self = Self(0);
    pub const MAX: Self = Self(u64::MAX);

    /// Create from seconds since epoch (e.g., Unix time).
    pub fn from_secs(secs: u64) -> Self {
        Self(secs.saturating_mul(1_000_000))
    }

    /// Create from milliseconds since epoch.
    pub fn from_millis(millis: u64) -> Self {
        Self(millis.saturating_mul(1_000))
    }

    /// Seconds since epoch (truncated).
    pub fn as_secs(self) -> u64 {
        self.0 / 1_000_000
    }

    /// Milliseconds since epoch (truncated).
    pub fn as_millis(self) -> u64 {
        self.0 / 1_000
    }
}

impl fmt::Debug for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Ts({})", self.0)
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Simple HH:MM display for TUI. Full formatting would use chrono.
        let secs = self.as_secs();
        let hours = (secs % 86400) / 3600;
        let minutes = (secs % 3600) / 60;
        write!(f, "{hours:02}:{minutes:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_secs_roundtrips() {
        let ts = Timestamp::from_secs(1_700_000_000);
        assert_eq!(ts.as_secs(), 1_700_000_000);
    }

    #[test]
    fn from_millis_roundtrips() {
        let ts = Timestamp::from_millis(1_700_000_000_000);
        assert_eq!(ts.as_millis(), 1_700_000_000_000);
        assert_eq!(ts.as_secs(), 1_700_000_000);
    }

    #[test]
    fn ordering_is_chronological() {
        let earlier = Timestamp(100);
        let later = Timestamp(200);
        assert!(earlier < later);
    }

    #[test]
    fn timestamp_is_copy() {
        fn assert_copy<T: Copy>() {}
        assert_copy::<Timestamp>();
    }

    #[test]
    fn display_formats_as_hh_mm() {
        // 10:30 UTC = 10*3600 + 30*60 = 37800 seconds
        let ts = Timestamp::from_secs(37800);
        assert_eq!(format!("{ts}"), "10:30");
    }
}
