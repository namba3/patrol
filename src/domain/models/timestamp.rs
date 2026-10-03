use chrono::{DateTime, Local, Utc};
use serde_derive::{Deserialize, Serialize};
use std::fmt::Display;

type DT = DateTime<Utc>;

#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(chrono::DateTime<Utc>);
impl Timestamp {
    pub fn now() -> Self {
        Self(chrono::Utc::now())
    }
    pub fn from_unix_secs(secs: i64) -> Self {
        Self::try_from_unix_secs(secs).expect("unix seconds are outside the supported range")
    }

    pub fn try_from_unix_secs(secs: i64) -> Option<Self> {
        DT::from_timestamp(secs, 0).map(Self)
    }

    pub fn from_unix_millis(millis: i64) -> Self {
        Self::try_from_unix_millis(millis)
            .expect("unix milliseconds are outside the supported range")
    }

    pub fn try_from_unix_millis(millis: i64) -> Option<Self> {
        let secs = millis.div_euclid(1_000);
        let subsec_nanos = millis.rem_euclid(1_000) as u32 * 1_000_000;
        DT::from_timestamp(secs, subsec_nanos).map(Self)
    }

    pub fn from_unix_nanos(nanos: i64) -> Self {
        Self::try_from_unix_nanos(nanos).expect("unix nanoseconds are outside the supported range")
    }

    pub fn try_from_unix_nanos(nanos: i64) -> Option<Self> {
        let secs = nanos.div_euclid(1_000_000_000);
        let subsec_nanos = nanos.rem_euclid(1_000_000_000) as u32;
        DT::from_timestamp(secs, subsec_nanos).map(Self)
    }

    pub fn unix_secs(&self) -> i64 {
        self.0.timestamp()
    }
    pub fn unix_millis(&self) -> i64 {
        self.0.timestamp_millis()
    }
    /// Returns Unix nanoseconds, clamping timestamps outside the `i64` nanosecond range.
    pub fn unix_nanos(&self) -> i64 {
        self.0.timestamp_nanos_opt().unwrap_or_else(|| {
            if self.0.timestamp() < 0 {
                i64::MIN
            } else {
                i64::MAX
            }
        })
    }
}

impl Display for Timestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!(
            "{}",
            DateTime::<Local>::from(self.0).format("%Y-%m-%d %H:%M:%S")
        ))
    }
}

impl core::ops::Sub<Duration> for Timestamp {
    type Output = Self;

    fn sub(self, rhs: Duration) -> Self::Output {
        Self(self.0 - chrono::Duration::nanoseconds(rhs.0 as i64))
    }
}
impl core::ops::Add<Duration> for Timestamp {
    type Output = Self;

    fn add(self, rhs: Duration) -> Self::Output {
        Self(self.0 + chrono::Duration::nanoseconds(rhs.0 as i64))
    }
}
impl core::ops::SubAssign<Duration> for Timestamp {
    fn sub_assign(&mut self, rhs: Duration) {
        self.0 -= chrono::Duration::nanoseconds(rhs.0 as i64)
    }
}
impl core::ops::AddAssign<Duration> for Timestamp {
    fn add_assign(&mut self, rhs: Duration) {
        self.0 += chrono::Duration::nanoseconds(rhs.0 as i64)
    }
}

#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Duration(u64);
impl Duration {
    pub const fn from_days(days: u32) -> Self {
        Self(days as u64 * 24 * 60 * 60 * 1_000_000_000)
    }
    pub const fn from_hours(hours: u32) -> Self {
        Self(hours as u64 * 60 * 60 * 1_000_000_000)
    }
    pub const fn from_mins(mins: u32) -> Self {
        Self(mins as u64 * 60 * 1_000_000_000)
    }
    pub const fn from_secs(secs: u64) -> Self {
        Self(secs * 1_000_000_000)
    }
    pub const fn from_millis(millis: u64) -> Self {
        Self(millis * 1_000_000)
    }
    pub const fn from_nanos(nanos: u64) -> Self {
        Self(nanos)
    }
}

#[cfg(test)]
mod tests {
    use super::{Duration, Timestamp};

    #[test]
    fn unix_constructors_preserve_subsecond_precision() {
        let timestamp = Timestamp::from_unix_nanos(1_234_567_890);

        assert_eq!(timestamp.unix_secs(), 1);
        assert_eq!(timestamp.unix_millis(), 1_234);
        assert_eq!(timestamp.unix_nanos(), 1_234_567_890);
        assert_eq!(Timestamp::from_unix_millis(1_234).unix_millis(), 1_234);
    }

    #[test]
    fn unix_nanos_handles_negative_subsecond_timestamps() {
        let timestamp = Timestamp::from_unix_nanos(-1);

        assert_eq!(timestamp.unix_secs(), -1);
        assert_eq!(timestamp.unix_nanos(), -1);
        assert_eq!(Timestamp::from_unix_millis(-1).unix_nanos(), -1_000_000);
    }

    #[test]
    fn unix_seconds_and_millis_do_not_overflow_via_nanos() {
        let seconds = Timestamp::from_unix_secs(10_000_000_000);
        assert_eq!(seconds.unix_secs(), 10_000_000_000);
        assert_eq!(seconds.unix_nanos(), i64::MAX);

        let millis = Timestamp::from_unix_millis(10_000_000_000_000);
        assert_eq!(millis.unix_millis(), 10_000_000_000_000);
        assert_eq!(millis.unix_nanos(), i64::MAX);
    }

    #[test]
    fn checked_unix_constructors_reject_dates_outside_chrono_range() {
        assert!(Timestamp::try_from_unix_secs(i64::MAX).is_none());
        assert!(Timestamp::try_from_unix_millis(i64::MAX).is_none());
        assert_eq!(
            Timestamp::try_from_unix_nanos(i64::MAX)
                .unwrap()
                .unix_nanos(),
            i64::MAX
        );
        assert_eq!(
            Timestamp::try_from_unix_nanos(i64::MIN)
                .unwrap()
                .unix_nanos(),
            i64::MIN
        );
    }

    #[test]
    fn arithmetic_uses_duration_units() {
        let timestamp = Timestamp::from_unix_secs(100);

        assert_eq!(
            (timestamp + Duration::from_millis(250)).unix_nanos(),
            100_250_000_000
        );
        assert_eq!((timestamp - Duration::from_secs(2)).unix_secs(), 98);
    }

    #[test]
    fn serde_round_trip_preserves_timestamp() {
        let timestamp = Timestamp::from_unix_millis(1_700_000_123_456);
        let encoded = serde_json::to_string(&timestamp).unwrap();
        let decoded: Timestamp = serde_json::from_str(&encoded).unwrap();

        assert_eq!(decoded, timestamp);
    }
}
