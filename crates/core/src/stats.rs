//! File statistics and field summaries: `log.stats`, `log.fields`, the column chooser. Owned by
//! the `search` stream.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::filter::parse_time_value;
use crate::limits::{FIELDS_MAX, cut};
use crate::search::{LineSource, cut_line, for_each_line, jobs, run_parallel};
use crate::{FieldId, LevelCounts, LogFile, Result, Timestamp};

/// Files with at most this many lines have every line's fields counted; bigger ones are sampled
/// (the first lines of every chunk, about this many in all) and say `exact: false`. Line, level
/// and time figures are always exact.
pub const STATS_EXACT_LINES: u64 = 1_000_000;
/// Distinct values remembered per field; values past it still count in `lines`.
const DISTINCT_CAP: usize = 10_000;
/// A value is kept cut to this many bytes.
const VALUE_MAX_BYTES: usize = 256;
/// Most distinct field names tracked.
const FIELDS_CAP: usize = 1024;

/// The type a field's values mostly have.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    #[default]
    String,
    Number,
    Bool,
    Time,
    /// A JSON object or array.
    Object,
    /// Values of several types.
    Mixed,
}

impl FieldType {
    /// `string`, `number`, `bool`, `time`, `object`, `mixed`.
    pub fn as_str(self) -> &'static str {
        match self {
            FieldType::String => "string",
            FieldType::Number => "number",
            FieldType::Bool => "bool",
            FieldType::Time => "time",
            FieldType::Object => "object",
            FieldType::Mixed => "mixed",
        }
    }
}

/// One parsed field over the lines looked at.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FieldSummary {
    pub name: String,
    pub ty: FieldType,
    /// Lines that have it.
    pub lines: u64,
    /// The most common values with their counts, most common first (capped by the caller).
    pub top: Vec<(String, u64)>,
    /// Distinct values seen (a lower bound once the sketch overflows).
    pub distinct: u64,
}

/// What `log.stats` reports.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileStats {
    pub lines: u64,
    pub bytes: u64,
    pub first_time: Option<Timestamp>,
    pub last_time: Option<Timestamp>,
    pub levels: LevelCounts,
    /// Most frequent fields, most lines first.
    pub fields: Vec<FieldSummary>,
    /// Whether every line was looked at (`false`: a sample, for files still indexing or huge).
    pub exact: bool,
}

/// Statistics of `file`, optionally for lines whose time is in `from..to`. Fields are summarised
/// with at most `top_fields` fields and `top_values` values each.
pub fn stats(
    file: &LogFile,
    from: Option<Timestamp>,
    to: Option<Timestamp>,
    top_fields: usize,
    top_values: usize,
    cancel: &dyn Fn() -> bool,
) -> Result<FileStats> {
    stats_in(file, from, to, top_fields, top_values, cancel)
}

/// [`stats`] over any [`LineSource`]. With a time range, lines without a time are left out.
pub fn stats_in<S: LineSource>(
    src: &S,
    from: Option<Timestamp>,
    to: Option<Timestamp>,
    top_fields: usize,
    top_values: usize,
    cancel: &dyn Fn() -> bool,
) -> Result<FileStats> {
    let range = (from.is_some() || to.is_some()).then_some((from, to));
    let part = collect(src, range, cancel)?;
    Ok(FileStats {
        lines: part.lines,
        bytes: src.byte_len(),
        first_time: part.lo,
        last_time: part.hi,
        levels: part.levels,
        exact: !part.sampled,
        fields: summaries(src, part.fields, top_fields, top_values),
    })
}

/// Every parsed field of `file` with its type and `top_values` most common values.
pub fn fields(file: &LogFile, top_values: usize, cancel: &dyn Fn() -> bool) -> Result<Vec<FieldSummary>> {
    fields_in(file, top_values, cancel)
}

/// [`fields`] over any [`LineSource`] (at most [`FIELDS_MAX`], most lines first).
pub fn fields_in<S: LineSource>(src: &S, top_values: usize, cancel: &dyn Fn() -> bool) -> Result<Vec<FieldSummary>> {
    let part = collect(src, None, cancel)?;
    Ok(summaries(src, part.fields, FIELDS_MAX, top_values))
}

/// One field's counts.
#[derive(Default)]
struct Acc {
    lines: u64,
    values: HashMap<String, u64>,
    /// Values per [`FieldType`], indexed like `TYPES`.
    types: [u64; 6],
}

const TYPES: [FieldType; 6] = [FieldType::String, FieldType::Number, FieldType::Bool, FieldType::Time, FieldType::Object, FieldType::Mixed];

fn value_type(v: &str) -> FieldType {
    let t = v.trim();
    if t == "true" || t == "false" {
        FieldType::Bool
    } else if t.parse::<f64>().is_ok_and(f64::is_finite) {
        FieldType::Number
    } else if t.starts_with('{') || t.starts_with('[') {
        FieldType::Object
    } else if t.len() >= 10 && t.as_bytes().get(4) == Some(&b'-') && parse_time_value(t).is_some() {
        FieldType::Time
    } else {
        FieldType::String
    }
}

impl Acc {
    fn add(&mut self, value: &str) {
        self.lines += 1;
        let ty = value_type(value);
        if let Some(c) = TYPES.iter().position(|t| *t == ty).and_then(|i| self.types.get_mut(i)) {
            *c += 1;
        }
        let v = cut(value, VALUE_MAX_BYTES).0;
        if let Some(c) = self.values.get_mut(v) {
            *c += 1;
        } else if self.values.len() < DISTINCT_CAP {
            self.values.insert(v.to_string(), 1);
        }
    }

    fn merge(&mut self, other: Acc) {
        self.lines = self.lines.saturating_add(other.lines);
        for (a, b) in self.types.iter_mut().zip(other.types) {
            *a = a.saturating_add(b);
        }
        for (v, n) in other.values {
            if let Some(c) = self.values.get_mut(&v) {
                *c = c.saturating_add(n);
            } else if self.values.len() < DISTINCT_CAP {
                self.values.insert(v, n);
            }
        }
    }

    fn summary(self, name: String, top_values: usize) -> FieldSummary {
        let total: u64 = self.types.iter().sum();
        let (best, n) = TYPES.iter().zip(self.types).max_by_key(|(_, n)| *n).map_or((FieldType::String, 0), |(t, n)| (*t, n));
        // one type for at least 90 % of the values, else mixed
        let ty = if total == 0 || n.saturating_mul(10) >= total.saturating_mul(9) { best } else { FieldType::Mixed };
        let distinct = self.values.len() as u64;
        let mut top: Vec<(String, u64)> = self.values.into_iter().collect();
        top.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        top.truncate(top_values);
        FieldSummary { name, ty, lines: self.lines, top, distinct }
    }
}

/// What one chunk (and then the whole file) adds up to.
#[derive(Default)]
struct Part {
    lines: u64,
    levels: LevelCounts,
    lo: Option<Timestamp>,
    hi: Option<Timestamp>,
    fields: HashMap<FieldId, Acc>,
    sampled: bool,
}

impl Part {
    fn time(&mut self, t: Timestamp) {
        self.lo = Some(self.lo.map_or(t, |x| x.min(t)));
        self.hi = Some(self.hi.map_or(t, |x| x.max(t)));
    }

    fn merge(&mut self, other: Part) {
        self.lines = self.lines.saturating_add(other.lines);
        self.levels.merge(&other.levels);
        if let Some(t) = other.lo {
            self.time(t);
        }
        if let Some(t) = other.hi {
            self.time(t);
        }
        for (id, acc) in other.fields {
            if let Some(a) = self.fields.get_mut(&id) {
                a.merge(acc);
            } else if self.fields.len() < FIELDS_CAP {
                self.fields.insert(id, acc);
            }
        }
        self.sampled |= other.sampled;
    }
}

/// One pass over the file (lines whose time is in `range` when given).
fn collect<S: LineSource>(src: &S, range: Option<(Option<Timestamp>, Option<Timestamp>)>, cancel: &dyn Fn() -> bool) -> Result<Part> {
    let jobs = jobs(src, None);
    let sample = if src.line_count() <= STATS_EXACT_LINES { u64::MAX } else { (STATS_EXACT_LINES / jobs.len().max(1) as u64).max(1024) };
    let in_range = |t: Option<Timestamp>| match range {
        None => true,
        Some((from, to)) => t.is_some_and(|t| from.is_none_or(|f| t >= f) && to.is_none_or(|e| t < e)),
    };
    let mut total = Part::default();
    run_parallel(
        jobs.len(),
        |i, _| {
            let mut part = Part::default();
            let Some(job) = jobs.get(i) else { return part };
            let summary = job.summary.as_ref().filter(|s| s.summarized);
            if let (Some((from, to)), Some(s)) = (range, summary) {
                let before = matches!((s.max_time, from), (Some(hi), Some(f)) if hi < f);
                let after = matches!((s.min_time, to), (Some(lo), Some(e)) if lo >= e);
                if before || after || s.min_time.is_none() {
                    return part;
                }
            }
            // Without a range, a summarized chunk gives lines, levels and times directly.
            let whole = job.whole_chunk().filter(|_| range.is_none());
            if let Some(s) = whole {
                part.lines = u64::from(s.lines);
                part.levels = s.levels;
                part.lo = s.min_time;
                part.hi = s.max_time;
            }
            let mut seen: Vec<FieldId> = Vec::new();
            src.with_lines(job.lines.start, job.lines.end, |buf| {
                for_each_line(buf, job.lines.end - job.lines.start, |idx, line| {
                    let with_fields = idx < sample;
                    if whole.is_some() && !with_fields {
                        part.sampled = true;
                        return false;
                    }
                    let line = cut_line(line);
                    let parsed = src.parse_line(line);
                    if whole.is_none() {
                        if !in_range(parsed.time) {
                            return true;
                        }
                        part.lines += 1;
                        part.levels.add(parsed.level);
                        if let Some(t) = parsed.time {
                            part.time(t);
                        }
                    }
                    if !with_fields {
                        part.sampled = true;
                        return true;
                    }
                    seen.clear();
                    for (id, span) in &parsed.fields {
                        if seen.contains(id) {
                            continue;
                        }
                        seen.push(*id);
                        if part.fields.len() >= FIELDS_CAP && !part.fields.contains_key(id) {
                            continue;
                        }
                        part.fields.entry(*id).or_default().add(&src.field_text(line, *span));
                    }
                    true
                })
            });
            part
        },
        cancel,
        &mut |part: Part| {
            total.merge(part);
            true
        },
    )?;
    Ok(total)
}

/// The `top_fields` fields with the most lines, each with its `top_values` most common values.
fn summaries<S: LineSource>(src: &S, fields: HashMap<FieldId, Acc>, top_fields: usize, top_values: usize) -> Vec<FieldSummary> {
    let names = src.field_names();
    let mut out: Vec<FieldSummary> = fields.into_iter().filter_map(|(id, acc)| Some(acc.summary(names.name(id)?, top_values))).collect();
    out.sort_unstable_by(|a, b| b.lines.cmp(&a.lines).then_with(|| a.name.cmp(&b.name)));
    out.truncate(top_fields);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::format_time;
    use crate::limits::CHUNK_LINES;
    use crate::search::MemoryLog;
    use crate::search::testing::{KV, kv_log};
    use crate::{Level, never_cancel};

    fn sample_text() -> String {
        let mut text = String::new();
        for i in 0..20i64 {
            let level = if i % 5 == 0 { "error" } else { "info" };
            let service = if i % 4 == 0 { "db" } else { "api" };
            text.push_str(&format!(
                "t={} level={level} service={service} status={} ok={} at={}\n",
                format_time(Timestamp::from_secs(i)),
                200 + i % 2,
                i % 2 == 0,
                format_time(Timestamp(0))
            ));
        }
        text.push_str("a line with no fields\n");
        text
    }

    #[test]
    fn counts_levels_times_and_top_values() {
        for log in [kv_log(&sample_text()), MemoryLog::with_parser(sample_text().into_bytes(), &KV)] {
            let s = stats_in(&log, None, None, 10, 2, &never_cancel).unwrap();
            assert_eq!(s.lines, 21);
            assert_eq!(s.levels.get(Level::Error), 4);
            assert_eq!(s.levels.none, 1);
            assert_eq!((s.first_time, s.last_time), (Some(Timestamp::from_secs(0)), Some(Timestamp::from_secs(19))));
            assert!(s.exact);
            let service = s.fields.iter().find(|f| f.name == "service").unwrap();
            assert_eq!(service.top, vec![("api".to_string(), 15), ("db".to_string(), 5)]);
            assert_eq!((service.ty, service.lines, service.distinct), (FieldType::String, 20, 2));
            let ty = |n: &str| s.fields.iter().find(|f| f.name == n).unwrap().ty;
            assert_eq!((ty("status"), ty("ok"), ty("at")), (FieldType::Number, FieldType::Bool, FieldType::Time));

            let r = stats_in(&log, Some(Timestamp::from_secs(5)), Some(Timestamp::from_secs(10)), 1, 1, &never_cancel).unwrap();
            assert_eq!(r.lines, 5);
            assert_eq!(r.levels.get(Level::Error), 1);
            assert_eq!(r.fields.len(), 1);

            let all = fields_in(&log, 1, &never_cancel).unwrap();
            assert_eq!(all.len(), 4);
            assert!(all.iter().all(|f| f.top.len() == 1));
        }
    }

    #[test]
    fn mixed_types_and_sampling_flag() {
        let text: String = (0..10).map(|i| if i < 5 { format!("v={i}\n") } else { format!("v=x{i}\n") }).collect();
        let s = stats_in(&MemoryLog::with_parser(text.into_bytes(), &KV), None, None, 5, 5, &never_cancel).unwrap();
        assert_eq!(s.fields.first().map(|f| f.ty), Some(FieldType::Mixed));

        let big = "v=1\n".repeat(CHUNK_LINES * 16 + 1);
        let s = stats_in(&kv_log(&big), None, None, 5, 5, &never_cancel).unwrap();
        assert_eq!(s.lines, CHUNK_LINES as u64 * 16 + 1);
        assert!(!s.exact);
        assert!(s.fields.first().is_some_and(|f| f.lines < s.lines));
    }
}
