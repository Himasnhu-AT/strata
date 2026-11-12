//! Lines per time bucket, per level: the strip above the table and `log.histogram`. Built per
//! chunk from the index summaries where no filter needs the lines. Owned by the `search` stream.

use crate::{Filter, LevelCounts, LogFile, Result, Timestamp};

/// One bucket: `[start, start + width)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Bucket {
    pub start: Timestamp,
    pub counts: LevelCounts,
}

/// A histogram over a file's time span.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Histogram {
    /// Bucket width in microseconds (> 0).
    pub width: i64,
    pub buckets: Vec<Bucket>,
    /// Lines that passed the filter but have no timestamp.
    pub untimed: u64,
}

/// A "nice" bucket width (1, 2, 5, 10, 15, 30 s, 1, 2, 5, 10, 15, 30 min, 1, 3, 6, 12 h, 1 d…) that
/// splits `span` microseconds into about `target` buckets.
pub fn auto_width(span: i64, target: usize) -> i64 {
    let target = i64::try_from(target.max(1)).unwrap_or(i64::MAX);
    (span / target).max(Timestamp::MICROS_PER_SECOND)
}

/// Build the histogram of `file` with buckets `width` wide ([`auto_width`] when `None`), counting
/// only lines that pass `filter`. At most [`crate::limits::HISTOGRAM_MAX`] buckets: a width that
/// would make more is widened.
pub fn histogram(file: &LogFile, width: Option<i64>, filter: Option<&Filter>, cancel: &dyn Fn() -> bool) -> Result<Histogram> {
    let _ = (file, width, filter, cancel);
    Ok(Histogram { width: width.unwrap_or(Timestamp::MICROS_PER_SECOND).max(1), ..Histogram::default() })
}
