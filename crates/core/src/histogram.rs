//! Lines per time bucket, per level: the strip above the table and `log.histogram`. Built per
//! chunk from the index summaries where no filter needs the lines. Owned by the `search` stream.
//!
//! Each chunk is counted on its own (in parallel) and the per-chunk counts are added up. A chunk
//! whose summary puts all its lines in one bucket is counted from the summary without reading it
//! (when there is no filter); its untimed lines (stack traces and the like) then count in that
//! bucket rather than in [`Histogram::untimed`].

use crate::limits::{HISTOGRAM_AUTO, HISTOGRAM_MAX};
use crate::search::{Job, LineSource, compile_filter, cut_line, for_each_line, jobs, run_parallel};
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

const S: i64 = Timestamp::MICROS_PER_SECOND;
const MIN: i64 = 60 * S;
const HOUR: i64 = 60 * MIN;
const DAY: i64 = 24 * HOUR;
/// The widths [`auto_width`] picks from; past the last, whole multiples of a year.
const NICE: [i64; 24] = [
    S,
    2 * S,
    5 * S,
    10 * S,
    15 * S,
    30 * S,
    MIN,
    2 * MIN,
    5 * MIN,
    10 * MIN,
    15 * MIN,
    30 * MIN,
    HOUR,
    3 * HOUR,
    6 * HOUR,
    12 * HOUR,
    DAY,
    2 * DAY,
    7 * DAY,
    14 * DAY,
    30 * DAY,
    90 * DAY,
    180 * DAY,
    365 * DAY,
];

/// `a / b` rounded up, for `a >= 0`, `b > 0`.
fn ceil_div(a: i64, b: i64) -> i64 {
    let b = b.max(1);
    a / b + i64::from(a % b != 0)
}

/// A "nice" bucket width (1, 2, 5, 10, 15, 30 s, 1, 2, 5, 10, 15, 30 min, 1, 3, 6, 12 h, 1 d…) that
/// splits `span` microseconds into about `target` buckets.
pub fn auto_width(span: i64, target: usize) -> i64 {
    let target = i64::try_from(target.max(1)).unwrap_or(i64::MAX);
    let span = span.max(0);
    // The smallest nice width with at most `target` buckets of it covering the span.
    if let Some(w) = NICE.into_iter().find(|w| ceil_div(span, *w) <= target) {
        return w;
    }
    let year = 365 * DAY;
    let years = ceil_div(ceil_div(span, target), year).max(1);
    years.saturating_mul(year)
}

/// Build the histogram of `file` with buckets `width` wide ([`auto_width`] when `None`), counting
/// only lines that pass `filter`. At most [`crate::limits::HISTOGRAM_MAX`] buckets: a width that
/// would make more is widened.
pub fn histogram(file: &LogFile, width: Option<i64>, filter: Option<&Filter>, cancel: &dyn Fn() -> bool) -> Result<Histogram> {
    histogram_in(file, width, filter, cancel)
}

/// The earliest and latest time of the lines in `jobs`: `Some(None)` when no line has one, `None`
/// when the summaries don't say (a chunk is not summarized).
fn span_from_summaries(jobs: &[Job]) -> Option<Option<(Timestamp, Timestamp)>> {
    let mut span: Option<(Timestamp, Timestamp)> = None;
    for job in jobs {
        let s = job.whole_chunk()?;
        if let (Some(lo), Some(hi)) = (s.min_time, s.max_time) {
            span = Some(span.map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi))));
        }
    }
    Some(span)
}

/// The earliest and latest time, by parsing every line.
fn span_by_scanning<S: LineSource>(src: &S, jobs: &[Job], cancel: &dyn Fn() -> bool) -> Result<Option<(Timestamp, Timestamp)>> {
    let mut span: Option<(Timestamp, Timestamp)> = None;
    run_parallel(
        jobs.len(),
        |i, _| {
            let job = jobs.get(i)?;
            let mut part: Option<(Timestamp, Timestamp)> = None;
            src.with_lines(job.lines.start, job.lines.end, |buf| {
                for_each_line(buf, job.lines.end - job.lines.start, |_, line| {
                    if let Some(t) = src.parse_line(cut_line(line)).time {
                        part = Some(part.map_or((t, t), |(a, b)| (a.min(t), b.max(t))));
                    }
                    true
                })
            });
            part
        },
        cancel,
        &mut |part: Option<(Timestamp, Timestamp)>| {
            if let Some((lo, hi)) = part {
                span = Some(span.map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi))));
            }
            true
        },
    )?;
    Ok(span)
}

/// The bucket width and first bucket start for a span, with at most [`HISTOGRAM_MAX`] buckets.
fn layout(lo: Timestamp, hi: Timestamp, width: Option<i64>) -> (i64, i64, usize) {
    let span = hi.0.saturating_sub(lo.0);
    let mut w = width.map_or_else(|| auto_width(span, HISTOGRAM_AUTO), |w| w.max(1));
    loop {
        let start = lo.0.div_euclid(w).saturating_mul(w);
        let n = hi.0.saturating_sub(start) / w + 1;
        if n <= HISTOGRAM_MAX as i64 || w == i64::MAX {
            return (w, start, usize::try_from(n).unwrap_or(HISTOGRAM_MAX));
        }
        w = w.max(auto_width(span, HISTOGRAM_MAX - 1)).max(w.saturating_mul(2));
    }
}

/// Per-chunk counts.
#[derive(Default)]
struct Part {
    counts: Vec<(usize, LevelCounts)>,
    untimed: u64,
}

/// [`histogram`] over any [`LineSource`].
pub fn histogram_in<S: LineSource>(src: &S, width: Option<i64>, filter: Option<&Filter>, cancel: &dyn Fn() -> bool) -> Result<Histogram> {
    let filter = compile_filter(filter)?;
    let jobs = jobs(src, None);
    let span = match span_from_summaries(&jobs) {
        Some(span) => span,
        None => span_by_scanning(src, &jobs, cancel)?,
    };
    let (w, start, n) = match span {
        Some((lo, hi)) => layout(lo, hi, width),
        None => (width.unwrap_or(S).max(1), 0, 0),
    };
    let bucket_of = |t: Timestamp| -> usize {
        let i = t.0.saturating_sub(start).max(0) / w;
        usize::try_from(i).unwrap_or(usize::MAX).min(n.saturating_sub(1))
    };

    let mut out = Histogram {
        width: w,
        buckets: (0..n)
            .map(|i| Bucket { start: Timestamp(start.saturating_add(w.saturating_mul(i as i64))), counts: LevelCounts::default() })
            .collect(),
        untimed: 0,
    };
    run_parallel(
        jobs.len(),
        |i, _| {
            let Some(job) = jobs.get(i) else { return Part::default() };
            if let Some(f) = &filter
                && let Some(s) = &job.summary
                && !f.chunk_may_match(s)
            {
                return Part::default();
            }
            if filter.is_none()
                && let Some(s) = job.whole_chunk()
            {
                match (s.min_time, s.max_time) {
                    (Some(lo), Some(hi)) if n > 0 && bucket_of(lo) == bucket_of(hi) => {
                        return Part { counts: vec![(bucket_of(lo), s.levels)], untimed: 0 };
                    }
                    (None, None) => return Part { counts: Vec::new(), untimed: u64::from(s.lines) },
                    _ => {}
                }
            }
            let mut part = Part::default();
            let mut local = vec![LevelCounts::default(); n];
            src.with_lines(job.lines.start, job.lines.end, |buf| {
                for_each_line(buf, job.lines.end - job.lines.start, |idx, line| {
                    let cut = cut_line(line);
                    let parsed = src.parse_line(cut);
                    if filter.as_ref().is_some_and(|f| !f.matches(cut, &parsed, src.field_names(), job.lines.start + idx)) {
                        return true;
                    }
                    match parsed.time.filter(|_| n > 0) {
                        Some(t) => {
                            if let Some(c) = local.get_mut(bucket_of(t)) {
                                c.add(parsed.level);
                            }
                        }
                        None => part.untimed += 1,
                    }
                    true
                })
            });
            part.counts = local.into_iter().enumerate().filter(|(_, c)| c.total() > 0).collect();
            part
        },
        cancel,
        &mut |part: Part| {
            for (i, c) in part.counts {
                if let Some(b) = out.buckets.get_mut(i) {
                    b.counts.merge(&c);
                }
            }
            out.untimed = out.untimed.saturating_add(part.untimed);
            true
        },
    )?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::format_time;
    use crate::limits::CHUNK_LINES;
    use crate::search::MemoryLog;
    use crate::search::testing::{KV, kv_log};
    use crate::{Level, never_cancel};

    #[test]
    fn auto_width_picks_nice_widths() {
        assert_eq!(auto_width(0, 120), S);
        assert_eq!(auto_width(60 * S, 120), S);
        assert_eq!(auto_width(10 * MIN, 120), 5 * S);
        assert_eq!(auto_width(DAY, 120), 15 * MIN);
        assert_eq!(auto_width(DAY, 0), DAY);
        assert_eq!(auto_width(i64::MAX, 120) % (365 * DAY), 0);
        assert!(ceil_div(i64::MAX, auto_width(i64::MAX, 120)) <= 120);
        assert_eq!(auto_width(-5, 10), S);
    }

    fn log_with(lines: &[(i64, &str)]) -> String {
        lines.iter().map(|(s, l)| format!("t={} level={l} x\n", format_time(Timestamp::from_secs(*s)))).collect()
    }

    #[test]
    fn counts_per_bucket_and_level() {
        let mut text = log_with(&[(0, "info"), (1, "info"), (10, "error"), (61, "warn")]);
        text.push_str("untimed line\n");
        for summarized in [false, true] {
            let log = if summarized { kv_log(&text) } else { MemoryLog::with_parser(text.clone().into_bytes(), &KV) };
            let h = histogram_in(&log, Some(30 * S), None, &never_cancel).unwrap();
            assert_eq!(h.width, 30 * S);
            assert_eq!(h.buckets.len(), 3);
            assert_eq!(h.buckets[0].counts.get(Level::Info), 2);
            assert_eq!(h.buckets[0].counts.get(Level::Error), 1);
            assert_eq!(h.buckets[2].counts.get(Level::Warn), 1);
            assert_eq!(h.buckets[2].start, Timestamp::from_secs(60));
            assert_eq!(h.untimed, 1);

            let f = Filter::parse("level >= warn").unwrap();
            let h = histogram_in(&log, Some(30 * S), Some(&f), &never_cancel).unwrap();
            assert_eq!(h.buckets.iter().map(|b| b.counts.total()).collect::<Vec<_>>(), vec![1, 0, 1]);
            assert_eq!(h.untimed, 0);
        }
    }

    #[test]
    fn widens_to_the_bucket_cap_and_merges_chunks() {
        // two chunks a day apart; one-second buckets would be 86 401 buckets
        let n = CHUNK_LINES + 10;
        let text: String =
            (0..n).map(|i| format!("t={} level=info\n", format_time(Timestamp::from_secs(if i < CHUNK_LINES { 0 } else { 86_400 })))).collect();
        let log = kv_log(&text);
        let h = histogram_in(&log, Some(S), None, &never_cancel).unwrap();
        assert!(h.buckets.len() <= HISTOGRAM_MAX);
        assert_eq!(h.buckets.iter().map(|b| b.counts.total()).sum::<u64>(), n as u64);
        assert_eq!(h.buckets.first().map(|b| b.counts.total()), Some(CHUNK_LINES as u64));
        let auto = histogram_in(&log, None, None, &never_cancel).unwrap();
        assert_eq!(auto.width, 15 * MIN);
    }

    #[test]
    fn untimed_and_empty_files() {
        let log = MemoryLog::new(b"a\nb\nc".to_vec(), crate::FormatKind::Plain);
        let h = histogram_in(&log, None, None, &never_cancel).unwrap();
        assert_eq!((h.buckets.len(), h.untimed, h.width), (0, 3, S));
        let empty = MemoryLog::new(Vec::new(), crate::FormatKind::Plain);
        assert_eq!(histogram_in(&empty, Some(0), None, &never_cancel).unwrap().width, 1);
        let extremes = log_with(&[(i64::MIN / S, "info"), (i64::MAX / S, "info")]);
        let h = histogram_in(&kv_log(&extremes), None, None, &never_cancel).unwrap();
        assert!(h.buckets.len() <= HISTOGRAM_MAX);
    }
}
