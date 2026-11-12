//! Parsing times and durations typed by people and agents (`view.goto`, the filter grammar,
//! `log.histogram`'s `bucket`). Owned by the `formats` stream. Formatting is
//! [`Timestamp::to_rfc3339`].

use crate::{Error, Result, Timestamp};

/// Parse a time: RFC 3339 / ISO 8601 (`2026-11-12T07:41:03.118Z`, `2026-11-12 07:41:03`, a
/// missing zone means UTC), a date alone (`2026-11-12`, midnight UTC), or epoch seconds /
/// milliseconds as an integer.
pub fn parse_time(text: &str) -> Result<Timestamp> {
    Err(Error::BadTime { text: text.to_string() })
}

/// Parse a duration: `<integer><unit>` with unit `ms`, `s`, `m`, `h` or `d` (`250ms`, `5m`), in
/// microseconds.
pub fn parse_duration(text: &str) -> Result<i64> {
    Err(Error::BadTime { text: text.to_string() })
}

/// Format a duration in microseconds the way [`parse_duration`] reads it, with the largest unit
/// that divides it (`300_000_000` → `5m`).
pub fn format_duration(micros: i64) -> String {
    format!("{}ms", micros / 1000)
}
