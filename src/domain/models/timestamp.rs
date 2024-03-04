use chrono::NaiveDateTime;
use serde_derive::{Deserialize, Serialize};
use std::fmt::Display;

#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(chrono::NaiveDateTime);
impl Timestamp {
    pub fn now() -> Self {
        Self(chrono::Utc::now().naive_utc())
    }
    pub fn from_unix_secs(secs: i64) -> Self {
        Self::from_unix_nanos(secs * 1_000_000_000)
    }
    pub fn from_unix_millis(millis: i64) -> Self {
        Self::from_unix_nanos(millis * 1_000_000)
    }
    pub fn from_unix_nanos(nanos: i64) -> Self {
        let secs = nanos / 1_000_000_000;
        let subsec_nanos = (nanos / 1_000_000_000) as u32;
        let dt = chrono::NaiveDateTime::from_timestamp_opt(secs, subsec_nanos).unwrap();
        Self(dt)
    }

    pub fn unix_secs(&self) -> i64 {
        self.0.timestamp()
    }
    pub fn unix_millis(&self) -> i64 {
        self.0.timestamp_millis()
    }
    pub fn unix_nanos(&self) -> i64 {
        self.0
            .timestamp_nanos_opt()
            .unwrap_or_else(|| self.0.timestamp_millis() * 1000)
    }
}

impl Display for Timestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("{}", self.0.format("%Y-%m-%d %H:%M:%S")))
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
