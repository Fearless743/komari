//! Go-compatible `LocalTime`.
//!
//! The Go backend defines `models.LocalTime` as a `time.Time` wrapper whose
//! database representation is the *local wall clock* formatted with
//! `2006-01-02 15:04:05.0000000` (always seven fractional digits, no offset).
//! A zero time is written as SQL `NULL`.
//!
//! Values read back may use other layouts because the Go SQLite driver also
//! writes `time.Time` directly (for example
//! `2026-06-23 01:01:32.488977903+00:00`). The Go `Scan` implementation keeps
//! the date/clock components and re-interprets them in the application
//! timezone, i.e. it effectively discards the offset. This module does the
//! same, which is what makes an existing Komari database readable without any
//! data conversion.

use std::env;
use std::fmt;
use std::str::FromStr;

use chrono::{
    DateTime, FixedOffset, Local, LocalResult, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone,
    Utc,
};
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, Value, ValueRef};
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The exact storage layout used by Go's `LocalTime.Value` (seven fractional
/// digits). Chrono only accepts 3/6/9 digit specifiers, so the fraction is
/// appended manually below.
pub const STORAGE_FORMAT: &str = "%Y-%m-%d %H:%M:%S%.f";

fn zero_time() -> NaiveDateTime {
    // Go's zero `time.Time` is 0001-01-01T00:00:00Z.
    NaiveDate::from_ymd_opt(1, 1, 1)
        .expect("year 1 is a valid date")
        .and_hms_opt(0, 0, 0)
        .expect("midnight is valid")
}

/// Returns `true` when the Go backend would use UTC, i.e. `TZ` is unset or
/// empty. Go caches this value once at startup; Rust reads it on demand so
/// tests and CLIs can configure it before opening a database.
pub fn timezone_is_utc() -> bool {
    env::var("TZ")
        .map(|value| value.trim().is_empty())
        .unwrap_or(true)
}

/// A wall-clock timestamp stored the same way Go's `models.LocalTime` stores it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalTime(NaiveDateTime);

impl LocalTime {
    /// The Go zero time (`0001-01-01 00:00:00`). It is persisted as `NULL`.
    pub fn zero() -> Self {
        Self(zero_time())
    }

    /// Wrap a naive local timestamp.
    pub const fn from_naive(value: NaiveDateTime) -> Self {
        Self(value)
    }

    /// Create from a Unix timestamp, interpreting it in the application zone.
    pub fn from_unix_timestamp(seconds: i64) -> Option<Self> {
        let utc = DateTime::<Utc>::from_timestamp(seconds, 0)?;
        Some(Self(if timezone_is_utc() {
            utc.naive_utc()
        } else {
            utc.with_timezone(&Local).naive_local()
        }))
    }

    /// The wrapped naive wall-clock timestamp.
    pub const fn to_naive(self) -> NaiveDateTime {
        self.0
    }

    /// Current time in the application timezone (Go `models.Now`).
    pub fn now() -> Self {
        Self(if timezone_is_utc() {
            Utc::now().naive_utc()
        } else {
            Local::now().naive_local()
        })
    }

    /// Whether this is Go's zero time.
    pub fn is_zero(self) -> bool {
        self.0 == zero_time()
    }

    /// Unix timestamp in seconds. Returns `None` for the zero time.
    pub fn timestamp(self) -> Option<i64> {
        if self.is_zero() {
            return None;
        }
        Some(if timezone_is_utc() {
            self.0.and_utc().timestamp()
        } else {
            Local
                .from_local_datetime(&self.0)
                .earliest()
                .map(|dt| dt.timestamp())
                .unwrap_or_else(|| self.0.and_utc().timestamp())
        })
    }

    /// Format using the Go storage layout (seven fractional digits,
    /// truncating anything beyond 100 ns just like Go's layout parser).
    pub fn to_storage_string(self) -> String {
        let fraction = self.0.and_utc().timestamp_subsec_nanos() / 100;
        format!("{}.{fraction:07}", self.0.format("%Y-%m-%d %H:%M:%S"))
    }

    /// Format as RFC 3339 with the application timezone offset. Mirrors
    /// `LocalTime.MarshalJSON`, which emits `null` for the zero time.
    pub fn to_rfc3339(self) -> Option<String> {
        if self.is_zero() {
            return None;
        }

        if timezone_is_utc() {
            return Some(self.0.and_utc().to_rfc3339_opts(SecondsFormat::Secs, true));
        }

        let dt = match Local.from_local_datetime(&self.0) {
            LocalResult::Single(dt) => dt,
            LocalResult::Ambiguous(first, _) => first,
            LocalResult::None => {
                // Nonexistent wall clock during a DST jump: fall back to UTC
                // rather than failing serialization.
                return Some(self.0.and_utc().to_rfc3339_opts(SecondsFormat::Secs, true));
            }
        };
        Some(dt.to_rfc3339_opts(SecondsFormat::Secs, true))
    }

    /// Parse a timestamp from any layout Komari has ever written.
    pub fn parse(value: &str) -> Result<Self, String> {
        let value = value.trim();
        if value.is_empty() {
            return Ok(Self::default());
        }

        // Offset-aware layouts first. The wall clock is deliberately kept.
        if let Ok(dt) = DateTime::parse_from_rfc3339(value) {
            return Ok(Self(dt.naive_local()));
        }
        for layout in [
            "%Y-%m-%d %H:%M:%S%.f%:z",
            "%Y-%m-%dT%H:%M:%S%.f%:z",
            "%Y-%m-%d %H:%M:%S%.f%z",
        ] {
            if let Ok(dt) = DateTime::parse_from_str(value, layout) {
                return Ok(Self(dt.naive_local()));
            }
        }

        // Naive layouts written by Go's `LocalTime.Value`.
        for layout in [
            STORAGE_FORMAT,
            "%Y-%m-%d %H:%M:%S%.f",
            "%Y-%m-%dT%H:%M:%S%.f",
            "%Y-%m-%d %H:%M:%S",
            "%Y-%m-%dT%H:%M:%S",
            "%Y-%m-%d",
        ] {
            if let Ok(dt) = NaiveDateTime::parse_from_str(value, layout) {
                return Ok(Self(dt));
            }
            if let Ok(date) = NaiveDate::parse_from_str(value, layout) {
                return Ok(Self(date.and_hms_opt(0, 0, 0).expect("midnight is valid")));
            }
        }

        Err(format!("unable to parse time string {value:?}"))
    }

    /// Construct from a `DateTime` in any timezone, keeping its wall clock
    /// components (matching Go's `LocalTime.Scan(time.Time)`).
    pub fn from_datetime<Tz: TimeZone>(value: DateTime<Tz>) -> Self {
        Self(value.naive_local())
    }
}

impl Default for LocalTime {
    fn default() -> Self {
        Self(zero_time())
    }
}

impl fmt::Debug for LocalTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_zero() {
            f.write_str("LocalTime(zero)")
        } else {
            write!(f, "LocalTime({})", self.to_storage_string())
        }
    }
}

impl fmt::Display for LocalTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_zero() {
            f.write_str("")
        } else {
            f.write_str(&self.to_storage_string())
        }
    }
}

impl FromStr for LocalTime {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl ToSql for LocalTime {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        if self.is_zero() {
            Ok(ToSqlOutput::Owned(Value::Null))
        } else {
            Ok(ToSqlOutput::Owned(Value::Text(self.to_storage_string())))
        }
    }
}

impl FromSql for LocalTime {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        match value {
            ValueRef::Null => Ok(Self::default()),
            ValueRef::Text(bytes) => {
                let text =
                    std::str::from_utf8(bytes).map_err(|err| FromSqlError::Other(Box::new(err)))?;
                Self::parse(text).map_err(|err| FromSqlError::Other(Box::new(DbTimeError(err))))
            }
            ValueRef::Blob(bytes) => {
                let text =
                    std::str::from_utf8(bytes).map_err(|err| FromSqlError::Other(Box::new(err)))?;
                Self::parse(text).map_err(|err| FromSqlError::Other(Box::new(DbTimeError(err))))
            }
            other => Err(FromSqlError::Other(Box::new(DbTimeError(format!(
                "unsupported SQLite value for LocalTime: {other:?}"
            ))))),
        }
    }
}

#[derive(Debug)]
struct DbTimeError(String);

impl fmt::Display for DbTimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DbTimeError {}

impl Serialize for LocalTime {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.to_rfc3339() {
            Some(value) => serializer.serialize_str(&value),
            None => serializer.serialize_none(),
        }
    }
}

impl<'de> Deserialize<'de> for LocalTime {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct LocalTimeVisitor;

        impl<'de> Visitor<'de> for LocalTimeVisitor {
            type Value = LocalTime;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an RFC 3339 timestamp, a local timestamp string, or null")
            }

            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(LocalTime::default())
            }

            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(LocalTime::default())
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                LocalTime::parse(value).map_err(E::custom)
            }

            fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
                self.visit_str(&value)
            }
        }

        deserializer.deserialize_any(LocalTimeVisitor)
    }
}

/// Parse an offset-less timestamp as an application-local wall clock and return
/// a fixed-offset date-time for callers that need absolute ordering.
pub fn local_to_fixed_offset(value: LocalTime) -> Option<DateTime<FixedOffset>> {
    if value.is_zero() {
        return None;
    }
    if timezone_is_utc() {
        return Some(value.0.and_utc().fixed_offset());
    }
    Local
        .from_local_datetime(&value.0)
        .earliest()
        .map(|dt| dt.fixed_offset())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_round_trips_as_null() {
        let value = LocalTime::default();
        assert!(value.is_zero());
        assert_eq!(value.to_storage_string(), "0001-01-01 00:00:00.0000000");
        assert_eq!(value.to_rfc3339(), None);
    }

    #[test]
    fn parses_go_storage_layout() {
        let value = LocalTime::parse("2026-06-23 01:00:31.6481286").unwrap();
        assert_eq!(
            value.to_naive().to_string(),
            "2026-06-23 01:00:31.648128600"
        );
        assert_eq!(value.to_storage_string(), "2026-06-23 01:00:31.6481286");
    }

    #[test]
    fn parses_driver_layout_with_offset() {
        let value = LocalTime::parse("2026-06-23 01:01:32.488977903+00:00").unwrap();
        assert_eq!(
            value.to_naive().to_string(),
            "2026-06-23 01:01:32.488977903"
        );
    }

    #[test]
    fn parses_all_supported_layouts() {
        for input in [
            "2026-06-23 01:00:31",
            "2026-06-23T01:00:31",
            "2026-06-23",
            "2026-06-23T01:00:31Z",
            "2026-06-23T01:00:31+08:00",
            "2026-06-23 01:00:31.5",
        ] {
            assert!(LocalTime::parse(input).is_ok(), "failed to parse {input}");
        }
    }
}
