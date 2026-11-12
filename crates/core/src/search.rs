//! Text and regex search over the line index: multi-threaded over chunks, streaming matches to a
//! callback. Owned by the `search` stream.
//!
//! # How a search runs
//!
//! The lines to search are cut into jobs of [`CHUNK_LINES`] lines (the index's chunks). Worker
//! threads take jobs in order and search each chunk's bytes as one buffer (no per-line calls), so
//! a literal runs at `memchr::memmem` speed and a regex at the speed of its lazy DFA. The calling
//! thread receives the results, puts them back in line order and hands them to the callback; it
//! also polls `cancel` and stops the workers. Workers never get more than a few chunks ahead of
//! the delivered ones, so memory stays bounded however many lines match.
//!
//! A match never spans lines: when a regex matches across a newline in the buffer, the line where
//! the match starts is searched again on its own.
//!
//! # Matching rules
//!
//! - Text is case-insensitive unless [`Query::case_sensitive`] (Unicode simple case folding).
//! - Whole word: the match is not preceded or followed by an ASCII word character
//!   (`[A-Za-z0-9_]`). For a regex the pattern is wrapped in ASCII `\b` boundaries.
//! - Regexes use the `regex` crate (linear time, no backtracking): `^`/`$` match at line starts
//!   and ends, `.` never matches a newline. Compiled programs are capped at [`REGEX_SIZE_LIMIT`],
//!   so a pathological pattern fails to compile instead of eating memory.
//! - An empty query matches every line (with no spans): with a filter it lists the lines that pass.

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, PoisonError, mpsc};
use std::time::Duration;

use memchr::memmem;
use regex::bytes::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::filter::CompiledFilter;
use crate::formats::{self, FieldNames, FormatKind, Parser};
use crate::index::{ChunkSummary, LineIndex};
use crate::limits::{CHUNK_LINES, MAX_LINE_BYTES};
use crate::{Error, Filter, LevelCounts, LineNo, LogFile, Parsed, Result, Span, Timestamp};

/// Largest compiled regex program, in bytes. A pattern over it is a [`Error::BadQuery`].
pub const REGEX_SIZE_LIMIT: usize = 4 << 20;
/// Largest lazy-DFA cache per regex, in bytes (over it the regex falls back to a slower engine,
/// never to more memory).
pub const REGEX_DFA_LIMIT: usize = 8 << 20;
/// Longest query text, in bytes.
pub const QUERY_MAX_BYTES: usize = 64 * 1024;
/// Most spans reported for one line (a hostile line can match thousands of times).
pub const SPANS_MAX: usize = 1024;
/// Deepest regex nesting.
const REGEX_NEST_LIMIT: u32 = 64;
/// Most worker threads one search uses.
const MAX_THREADS: usize = 16;

/// What to look for. `case_sensitive: false` is the default (the toolbar's case toggle off).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Query {
    pub text: String,
    pub regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
}

impl Query {
    /// A plain, case-insensitive text query.
    pub fn text(text: impl Into<String>) -> Query {
        Query { text: text.into(), ..Query::default() }
    }

    /// Compile into a [`Matcher`]; fails on a bad regex ([`Error::BadQuery`]).
    pub fn compile(&self) -> Result<Matcher> {
        if self.text.len() > QUERY_MAX_BYTES {
            return Err(Error::BadQuery { message: format!("the search text is over {} KB", QUERY_MAX_BYTES / 1024) });
        }
        let ignore_case = !self.case_sensitive;
        let bad = |message| Error::BadQuery { message };
        let (kind, word) = if self.text.is_empty() {
            (Kind::All, false)
        } else if self.regex {
            // Compile the bare pattern first so a syntax error points at the person's text.
            let mut re = build_regex(&self.text, ignore_case).map_err(bad)?;
            if self.whole_word {
                re = build_regex(&format!(r"(?-u:\b)(?:{})(?-u:\b)", self.text), ignore_case).map_err(bad)?;
            }
            (Kind::Regex(re), false)
        } else if self.text.contains('\n') {
            (Kind::Never, false)
        } else if !ignore_case || !self.text.chars().any(char::is_alphabetic) {
            (Kind::Literal(Box::new(memmem::Finder::new(self.text.as_bytes()).into_owned())), self.whole_word)
        } else {
            (Kind::Regex(build_regex(&regex::escape(&self.text), true).map_err(bad)?), self.whole_word)
        };
        Ok(Matcher { query: self.clone(), kind, word })
    }
}

/// Compile `pattern` with the search settings (line anchors, CRLF-aware, size limits). The error
/// is one line of text.
pub(crate) fn build_regex(pattern: &str, ignore_case: bool) -> std::result::Result<Regex, String> {
    RegexBuilder::new(pattern)
        .case_insensitive(ignore_case)
        .multi_line(true)
        .crlf(true)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_DFA_LIMIT)
        .nest_limit(REGEX_NEST_LIMIT)
        .build()
        .map_err(|e| regex_error_text(&e))
}

/// The regex crate's multi-line error as one line (`unclosed group`).
fn regex_error_text(e: &regex::Error) -> String {
    match e {
        regex::Error::Syntax(text) => {
            let last = text.lines().rev().find_map(|l| l.trim().strip_prefix("error:")).map(str::trim);
            last.unwrap_or("invalid regex").to_string()
        }
        regex::Error::CompiledTooBig(limit) => format!("the regex is too large (over {limit} bytes compiled)"),
        _ => "invalid regex".to_string(),
    }
}

/// A compiled query.
#[derive(Clone, Debug)]
pub struct Matcher {
    query: Query,
    kind: Kind,
    /// Check ASCII word boundaries around each match by hand (literals).
    word: bool,
}

#[derive(Clone, Debug)]
enum Kind {
    /// The empty query: every line, no spans.
    All,
    /// Can never match one line (a literal with a newline).
    Never,
    Literal(Box<memmem::Finder<'static>>),
    Regex(Regex),
}

impl Matcher {
    /// A matcher that accepts every line (the empty query).
    pub fn all() -> Matcher {
        Matcher { query: Query::default(), kind: Kind::All, word: false }
    }

    pub fn query(&self) -> &Query {
        &self.query
    }

    /// Whether this matcher accepts every line without looking at it.
    pub fn is_all(&self) -> bool {
        matches!(self.kind, Kind::All)
    }

    /// The first match in `hay`.
    pub fn find(&self, hay: &[u8]) -> Option<Range<usize>> {
        self.find_from(hay, 0)
    }

    /// Every match in `hay` (non-overlapping), appended to `out`. Empty matches are skipped and
    /// at most [`SPANS_MAX`] are added.
    pub fn find_all(&self, hay: &[u8], out: &mut Vec<Span>) {
        if matches!(self.kind, Kind::All | Kind::Never) {
            return;
        }
        let mut at = 0;
        let mut added = 0;
        while added < SPANS_MAX {
            let Some(m) = self.find_from(hay, at) else { break };
            if !m.is_empty() {
                let (Ok(start), Ok(end)) = (u32::try_from(m.start), u32::try_from(m.end)) else { break };
                out.push(Span::new(start, end));
                added += 1;
            }
            at = if m.is_empty() { m.end.saturating_add(1) } else { m.end };
            if at > hay.len() {
                break;
            }
        }
    }

    /// The first match starting at or after `at`.
    fn find_from(&self, hay: &[u8], mut at: usize) -> Option<Range<usize>> {
        loop {
            if at > hay.len() {
                return None;
            }
            let m = match &self.kind {
                Kind::All => return Some(at..at),
                Kind::Never => return None,
                Kind::Literal(f) => {
                    let i = f.find(hay.get(at..)?)?;
                    let start = at.checked_add(i)?;
                    start..start.checked_add(f.needle().len())?
                }
                Kind::Regex(re) => {
                    let m = re.find_at(hay, at)?;
                    m.start()..m.end()
                }
            };
            if !self.word || word_bounded(hay, &m) {
                return Some(m);
            }
            at = m.start.checked_add(1)?;
        }
    }
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Whether `m` in `hay` has no ASCII word character right before or after it.
fn word_bounded(hay: &[u8], m: &Range<usize>) -> bool {
    let before = m.start.checked_sub(1).and_then(|i| hay.get(i)).is_some_and(|b| is_word_byte(*b));
    let after = hay.get(m.end).is_some_and(|b| is_word_byte(*b));
    !before && !after
}

/// One matching line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Match {
    pub line: LineNo,
    /// Where the query matched in the line (underlined in the table).
    pub spans: Vec<Span>,
}

/// How a search ended.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchSummary {
    /// Matching lines found (all of them, even past what `on_match` accepted).
    pub matched: u64,
    /// Lines looked at.
    pub scanned: u64,
    /// Whether the whole range was searched (`false`: stopped by `on_match` or cancelled).
    pub complete: bool,
}

// ---------------------------------------------------------------------------------------------
// Line sources
// ---------------------------------------------------------------------------------------------

/// What search, filters, the histogram and stats read lines from: a [`LogFile`] (the app) or a
/// [`MemoryLog`] (tests, benchmarks, small inputs). [`LineSource::with_lines`] is called from
/// several threads at once and must not serialize them.
pub trait LineSource: Sync {
    /// Lines available (indexed so far).
    fn line_count(&self) -> u64;

    /// Bytes in the file.
    fn byte_len(&self) -> u64;

    /// The index summary of chunk `chunk` (lines `chunk * CHUNK_LINES ..`), if known.
    fn chunk_summary(&self, chunk: usize) -> Option<ChunkSummary> {
        let _ = chunk;
        None
    }

    /// Run `f` on the raw bytes of lines `from..to`, separated by `\n` (a trailing newline may or
    /// may not be included). `None` when the range is not available.
    fn with_lines<R>(&self, from: LineNo, to: LineNo, f: impl FnOnce(&[u8]) -> R) -> Option<R>;

    /// Split one line (already cut to [`MAX_LINE_BYTES`]).
    fn parse_line(&self, line: &[u8]) -> Parsed;

    /// Split one line, interning field names in `names` instead of the shared table. Workers
    /// pass a table of their own: the shared one is a lock every parse takes once per field, and
    /// with many threads that lock (not parsing) becomes the limit.
    fn parse_with(&self, line: &[u8], names: &FieldNames) -> Parsed;

    /// The field names [`LineSource::parse_line`] interns.
    fn field_names(&self) -> &FieldNames;

    /// The text of a field value, format escapes resolved.
    fn field_text(&self, line: &[u8], span: Span) -> String {
        String::from_utf8_lossy(span.slice(line).unwrap_or_default()).into_owned()
    }
}

impl LineSource for LogFile {
    fn line_count(&self) -> u64 {
        self.lines()
    }

    fn byte_len(&self) -> u64 {
        self.len()
    }

    fn chunk_summary(&self, chunk: usize) -> Option<ChunkSummary> {
        self.with_index(|ix| ix.chunk(chunk))
    }

    fn with_lines<R>(&self, from: LineNo, to: LineNo, f: impl FnOnce(&[u8]) -> R) -> Option<R> {
        self.with_bytes(from, to, f)
    }

    fn parse_line(&self, line: &[u8]) -> Parsed {
        self.parse(line)
    }

    fn parse_with(&self, line: &[u8], names: &FieldNames) -> Parsed {
        formats::parser(self.format()).parse(line, names)
    }

    fn field_names(&self) -> &FieldNames {
        self.names()
    }

    fn field_text(&self, line: &[u8], span: Span) -> String {
        formats::parser(self.format()).field_text(line, span)
    }
}

/// A log held in memory: bytes plus line starts, parsed with one [`Parser`]. For tests,
/// benchmarks, the CLI reading stdin and other inputs too small to map.
pub struct MemoryLog {
    bytes: Vec<u8>,
    /// Start offset of every line.
    starts: Vec<usize>,
    parser: &'static dyn Parser,
    names: FieldNames,
    summaries: Vec<ChunkSummary>,
}

impl std::fmt::Debug for MemoryLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryLog").field("bytes", &self.bytes.len()).field("lines", &self.starts.len()).finish_non_exhaustive()
    }
}

impl MemoryLog {
    /// Index `bytes` and parse them as `format`.
    pub fn new(bytes: Vec<u8>, format: FormatKind) -> MemoryLog {
        MemoryLog::with_parser(bytes, formats::parser(format))
    }

    /// Index `bytes` and parse them with `parser`.
    pub fn with_parser(bytes: Vec<u8>, parser: &'static dyn Parser) -> MemoryLog {
        let mut starts = Vec::with_capacity(bytes.len() / 128);
        if !bytes.is_empty() {
            starts.push(0);
        }
        for i in memchr::memchr_iter(b'\n', &bytes) {
            let next = i.saturating_add(1);
            if next < bytes.len() {
                starts.push(next);
            }
        }
        MemoryLog { bytes, starts, parser, names: FieldNames::default(), summaries: Vec::new() }
    }

    /// Fill in the per-chunk time and level summaries (what the index does), so filters and the
    /// histogram can skip chunks.
    pub fn summarize(mut self) -> MemoryLog {
        let lines = self.line_count();
        let chunk = CHUNK_LINES as u64;
        let mut out = Vec::new();
        let mut first = 0u64;
        while first < lines {
            let last = first.saturating_add(chunk).min(lines);
            let mut s = ChunkSummary {
                first_line: first,
                lines: u32::try_from(last - first).unwrap_or(u32::MAX),
                summarized: true,
                ..ChunkSummary::default()
            };
            let mut levels = LevelCounts::default();
            let (mut lo, mut hi): (Option<Timestamp>, Option<Timestamp>) = (None, None);
            if let Some(r) = self.range_of(first, last) {
                s.bytes = r.start as u64..r.end as u64;
                let buf = self.bytes.get(r).unwrap_or_default();
                for_each_line(buf, last - first, |_, line| {
                    let p = self.parser.parse(cut_line(line), &self.names);
                    levels.add(p.level);
                    if let Some(t) = p.time {
                        lo = Some(lo.map_or(t, |x| x.min(t)));
                        hi = Some(hi.map_or(t, |x| x.max(t)));
                    }
                    true
                });
            }
            (s.levels, s.min_time, s.max_time) = (levels, lo, hi);
            out.push(s);
            first = last;
        }
        self.summaries = out;
        self
    }

    /// The whole text.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Line `n` without its newline (and `\r`).
    pub fn line(&self, n: LineNo) -> Option<&[u8]> {
        let r = self.range_of(n, n.checked_add(1)?)?;
        self.bytes.get(r).map(strip_cr)
    }

    /// Byte range of lines `from..to`, without the last newline.
    fn range_of(&self, from: LineNo, to: LineNo) -> Option<Range<usize>> {
        if from >= to || to > self.line_count() {
            return None;
        }
        let start = *self.starts.get(usize::try_from(from).ok()?)?;
        let end = match self.starts.get(usize::try_from(to).ok()?) {
            Some(next) => next.saturating_sub(1),
            None => {
                let len = self.bytes.len();
                if self.bytes.last() == Some(&b'\n') { len.saturating_sub(1) } else { len }
            }
        };
        Some(start..end.max(start))
    }
}

impl LineSource for MemoryLog {
    fn line_count(&self) -> u64 {
        self.starts.len() as u64
    }

    fn byte_len(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn chunk_summary(&self, chunk: usize) -> Option<ChunkSummary> {
        self.summaries.get(chunk).cloned()
    }

    fn with_lines<R>(&self, from: LineNo, to: LineNo, f: impl FnOnce(&[u8]) -> R) -> Option<R> {
        let r = self.range_of(from, to)?;
        Some(f(self.bytes.get(r)?))
    }

    fn parse_line(&self, line: &[u8]) -> Parsed {
        self.parser.parse(line, &self.names)
    }

    fn parse_with(&self, line: &[u8], names: &FieldNames) -> Parsed {
        self.parser.parse(line, names)
    }

    fn field_names(&self) -> &FieldNames {
        &self.names
    }

    fn field_text(&self, line: &[u8], span: Span) -> String {
        self.parser.field_text(line, span)
    }
}

/// `line` without a trailing `\r`.
pub(crate) fn strip_cr(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// `line` cut to [`MAX_LINE_BYTES`] (what parsers and filters see).
pub(crate) fn cut_line(line: &[u8]) -> &[u8] {
    line.get(..MAX_LINE_BYTES).unwrap_or(line)
}

/// Call `f(i, line)` for the first `max` lines of `buf` (0-based `i`, no `\n` or trailing `\r`);
/// stops when `f` returns `false`. Returns the lines visited.
pub(crate) fn for_each_line(buf: &[u8], max: u64, mut f: impl FnMut(u64, &[u8]) -> bool) -> u64 {
    let mut start = 0usize;
    let mut i = 0u64;
    let mut newlines = memchr::memchr_iter(b'\n', buf);
    while i < max && start <= buf.len() {
        // `max` decides whether a newline at the very end starts one more (empty) line.
        let end = newlines.next().unwrap_or(buf.len());
        let line = strip_cr(buf.get(start..end).unwrap_or_default());
        i += 1;
        if !f(i - 1, line) {
            break;
        }
        start = end.saturating_add(1);
    }
    i
}

// ---------------------------------------------------------------------------------------------
// Jobs and the parallel runner
// ---------------------------------------------------------------------------------------------

/// One unit of work: a run of lines inside one index chunk.
#[derive(Clone, Debug)]
pub(crate) struct Job {
    pub lines: Range<LineNo>,
    /// The chunk's summary, when the index has one (`lines` may be part of the chunk).
    pub summary: Option<ChunkSummary>,
}

impl Job {
    /// The summary when the job covers its whole, summarized chunk (so the summary describes
    /// exactly these lines).
    pub fn whole_chunk(&self) -> Option<&ChunkSummary> {
        self.summary.as_ref().filter(|s| s.summarized && s.first_line == self.lines.start && u64::from(s.lines) == self.lines.end - self.lines.start)
    }
}

/// The jobs for `lines` of `src` (every available line when `None`), in line order.
pub(crate) fn jobs<S: LineSource>(src: &S, lines: Option<Range<LineNo>>) -> Vec<Job> {
    let total = src.line_count();
    let r = lines.unwrap_or(0..total);
    let (start, end) = (r.start.min(total), r.end.min(total));
    let mut out = Vec::new();
    let chunk = CHUNK_LINES as u64;
    let mut c = LineIndex::chunk_of(start);
    let mut first = (c as u64).saturating_mul(chunk);
    while start < end && first < end {
        let a = first.max(start);
        let b = first.saturating_add(chunk).min(end);
        if a < b {
            out.push(Job { lines: a..b, summary: src.chunk_summary(c).filter(|s| s.first_line == first) });
        }
        c = c.saturating_add(1);
        first = first.saturating_add(chunk);
    }
    out
}

/// Run `work(i)` for `0..jobs` on worker threads and hand the results to `deliver` in order of
/// `i` on this thread. Stops early when `deliver` returns `false` (`Ok(false)`); `cancel` is
/// polled here, never on a worker (`Err(Cancelled)`). `work` gets a stop flag to check in long
/// loops.
pub(crate) fn run_parallel<T: Send>(
    jobs: usize,
    work: impl Fn(usize, &AtomicBool) -> T + Sync,
    cancel: &dyn Fn() -> bool,
    deliver: &mut dyn FnMut(T) -> bool,
) -> Result<bool> {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, MAX_THREADS).min(jobs);
    let stop = AtomicBool::new(false);
    if threads <= 1 {
        for i in 0..jobs {
            if cancel() {
                return Err(Error::Cancelled);
            }
            if !deliver(work(i, &stop)) {
                return Ok(false);
            }
        }
        return if cancel() { Err(Error::Cancelled) } else { Ok(true) };
    }

    let window = threads.saturating_mul(3);
    let next = AtomicUsize::new(0);
    let delivered = Mutex::new(0usize);
    let moved = Condvar::new();
    let (tx, rx) = mpsc::channel::<(usize, T)>();
    std::thread::scope(|s| {
        for _ in 0..threads {
            let tx = tx.clone();
            let (next, stop, delivered, moved, work) = (&next, &stop, &delivered, &moved, &work);
            s.spawn(move || {
                loop {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= jobs {
                        break;
                    }
                    let mut d = delivered.lock().unwrap_or_else(PoisonError::into_inner);
                    while i >= d.saturating_add(window) && !stop.load(Ordering::Relaxed) {
                        d = moved.wait_timeout(d, Duration::from_millis(50)).unwrap_or_else(PoisonError::into_inner).0;
                    }
                    drop(d);
                    if stop.load(Ordering::Relaxed) || tx.send((i, work(i, stop))).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);

        let mut pending = BTreeMap::new();
        let mut want = 0usize;
        let outcome = loop {
            if cancel() {
                break Err(Error::Cancelled);
            }
            match rx.recv_timeout(Duration::from_millis(15)) {
                Ok((i, r)) => {
                    pending.insert(i, r);
                    let mut go = true;
                    while let Some(r) = pending.remove(&want) {
                        want += 1;
                        *delivered.lock().unwrap_or_else(PoisonError::into_inner) = want;
                        moved.notify_all();
                        if !deliver(r) {
                            go = false;
                            break;
                        }
                    }
                    if !go {
                        break Ok(false);
                    }
                    if want >= jobs {
                        break Ok(true);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break Ok(want >= jobs),
            }
        };
        stop.store(true, Ordering::Relaxed);
        moved.notify_all();
        outcome
    })
}

// ---------------------------------------------------------------------------------------------
// Scanning one chunk
// ---------------------------------------------------------------------------------------------

/// What a chunk scan collects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Want {
    /// Every matching line with its spans.
    Matches,
    /// Only how many lines match.
    Count,
    /// The first matching line (no spans).
    First,
    /// The last matching line (no spans).
    Last,
}

/// The result of one chunk.
#[derive(Debug, Default)]
pub(crate) struct Hits {
    pub matches: Vec<Match>,
    pub matched: u64,
    pub scanned: u64,
    /// `false`: the lines were not available or the scan was stopped.
    pub complete: bool,
}

/// How often (in lines taken) a scan checks the stop flag.
const STOP_EVERY: u64 = 1024;

/// Whether line `number` (bytes `line`) passes `filter`; `names` is the worker's own table.
pub(crate) fn passes<S: LineSource>(src: &S, filter: &CompiledFilter, line: &[u8], number: LineNo, names: &FieldNames) -> bool {
    let cut = cut_line(line);
    if filter.needs_parse() {
        filter.matches(cut, &src.parse_with(cut, names), names, number)
    } else {
        filter.matches(cut, &Parsed::default(), names, number)
    }
}

/// Scan `job` for lines that `matcher` matches and `filter` passes.
pub(crate) fn scan<S: LineSource>(src: &S, job: &Job, matcher: &Matcher, filter: Option<&CompiledFilter>, want: Want, stop: &AtomicBool) -> Hits {
    let n = job.lines.end - job.lines.start;
    let skip = filter.zip(job.summary.as_ref()).is_some_and(|(f, s)| !f.chunk_may_match(s));
    if skip || matches!(matcher.kind, Kind::Never) {
        return Hits { scanned: n, complete: true, ..Hits::default() };
    }
    let first = job.lines.start;
    let names = FieldNames::default();
    src.with_lines(job.lines.start, job.lines.end, |buf| {
        let mut hits = Hits { complete: true, ..Hits::default() };
        // Returns false to stop the scan.
        let take = |idx: u64, line: &[u8], hits: &mut Hits| -> bool {
            if filter.is_some_and(|f| !passes(src, f, line, first + idx, &names)) {
                return true;
            }
            hits.matched += 1;
            match want {
                Want::Count => {}
                Want::Matches => {
                    let mut spans = Vec::new();
                    matcher.find_all(line, &mut spans);
                    hits.matches.push(Match { line: first + idx, spans });
                }
                Want::First => {
                    hits.matches.push(Match { line: first + idx, spans: Vec::new() });
                    return false;
                }
                Want::Last => {
                    hits.matches.clear();
                    hits.matches.push(Match { line: first + idx, spans: Vec::new() });
                }
            }
            if hits.matched.is_multiple_of(STOP_EVERY) && stop.load(Ordering::Relaxed) {
                hits.complete = false;
                return false;
            }
            true
        };

        if matcher.is_all() {
            hits.scanned = for_each_line(buf, n, |idx, line| {
                if idx > 0 && idx.is_multiple_of(STOP_EVERY * 16) && stop.load(Ordering::Relaxed) {
                    hits.complete = false;
                    return false;
                }
                take(idx, line, &mut hits)
            });
            return hits;
        }

        // Search the whole buffer; `pos` is always the start of line `idx`.
        let mut pos = 0usize;
        let mut idx = 0u64;
        let mut stopped = false;
        while idx < n && pos <= buf.len() {
            let Some(m) = matcher.find_from(buf, pos) else { break };
            let seg = buf.get(pos..m.start).unwrap_or_default();
            idx += memchr::memchr_iter(b'\n', seg).count() as u64;
            if idx >= n {
                break;
            }
            let line_start = memchr::memrchr(b'\n', seg).map_or(pos, |i| pos + i + 1);
            let rest = buf.get(line_start..).unwrap_or_default();
            let line_end = memchr::memchr(b'\n', rest).map_or(buf.len(), |i| line_start + i);
            let line = strip_cr(buf.get(line_start..line_end).unwrap_or_default());
            let inside = m.end <= line_start + line.len();
            if (inside || matcher.find(line).is_some()) && !take(idx, line, &mut hits) {
                stopped = true;
                break;
            }
            pos = line_end.saturating_add(1);
            idx += 1;
        }
        hits.scanned = if stopped { idx.saturating_add(1).min(n) } else { n };
        hits
    })
    .unwrap_or_default()
}

// ---------------------------------------------------------------------------------------------
// The public operations
// ---------------------------------------------------------------------------------------------

/// Search `lines` of `file` (the indexed part when `None`) for `matcher`, keeping only lines that
/// pass `filter`. Matches arrive in line order through `on_match`; returning `false` stops
/// delivering but keeps counting when `count_all` is set (MCP's `total`). Checks `cancel` between
/// chunks.
pub fn search(
    file: &LogFile,
    matcher: &Matcher,
    filter: Option<&Filter>,
    lines: Option<Range<LineNo>>,
    count_all: bool,
    cancel: &dyn Fn() -> bool,
    on_match: &mut dyn FnMut(Match) -> bool,
) -> Result<SearchSummary> {
    search_in(file, matcher, filter, lines, count_all, cancel, on_match)
}

/// [`search`] over any [`LineSource`].
pub fn search_in<S: LineSource>(
    src: &S,
    matcher: &Matcher,
    filter: Option<&Filter>,
    lines: Option<Range<LineNo>>,
    count_all: bool,
    cancel: &dyn Fn() -> bool,
    on_match: &mut dyn FnMut(Match) -> bool,
) -> Result<SearchSummary> {
    let filter = compile_filter(filter)?;
    let jobs = jobs(src, lines);
    let collect = AtomicBool::new(true);
    let mut summary = SearchSummary::default();
    let mut delivering = true;
    let mut all_there = true;
    let finished = run_parallel(
        jobs.len(),
        |i, stop| {
            let want = if collect.load(Ordering::Relaxed) { Want::Matches } else { Want::Count };
            jobs.get(i).map(|job| scan(src, job, matcher, filter.as_ref(), want, stop)).unwrap_or_default()
        },
        cancel,
        &mut |hits: Hits| {
            summary.matched = summary.matched.saturating_add(hits.matched);
            summary.scanned = summary.scanned.saturating_add(hits.scanned);
            all_there &= hits.complete;
            if delivering {
                for m in hits.matches {
                    if !on_match(m) {
                        delivering = false;
                        collect.store(false, Ordering::Relaxed);
                        break;
                    }
                }
            }
            delivering || count_all
        },
    )?;
    summary.complete = finished && all_there;
    Ok(summary)
}

/// The filter compiled, `None` when it accepts everything.
pub(crate) fn compile_filter(filter: Option<&Filter>) -> Result<Option<CompiledFilter>> {
    match filter {
        None | Some(Filter::All) => Ok(None),
        Some(f) => Ok(Some(f.compile()?)),
    }
}

/// The next line after `from` (before it when `backwards`) that passes `filter`: jump to error
/// (`level >= error`), next match, `log.first`. Skips chunks with
/// [`Filter::chunk_may_match`].
pub fn find_next(file: &LogFile, filter: &Filter, from: LineNo, backwards: bool, cancel: &dyn Fn() -> bool) -> Result<Option<LineNo>> {
    find_next_in(file, filter, from, backwards, cancel)
}

/// [`find_next`] over any [`LineSource`].
pub fn find_next_in<S: LineSource>(src: &S, filter: &Filter, from: LineNo, backwards: bool, cancel: &dyn Fn() -> bool) -> Result<Option<LineNo>> {
    let compiled = compile_filter(Some(filter))?;
    let matcher = compiled.as_ref().and_then(CompiledFilter::prefilter).unwrap_or_else(Matcher::all);
    let range = if backwards { 0..from } else { from.saturating_add(1)..u64::MAX };
    let mut jobs = jobs(src, Some(range));
    if backwards {
        jobs.reverse();
    }
    let want = if backwards { Want::Last } else { Want::First };
    let mut found = None;
    run_parallel(
        jobs.len(),
        |i, stop| jobs.get(i).map(|job| scan(src, job, &matcher, compiled.as_ref(), want, stop)).unwrap_or_default(),
        cancel,
        &mut |hits: Hits| {
            found = hits.matches.first().map(|m| m.line);
            found.is_none()
        },
    )?;
    Ok(found)
}

/// How many indexed lines pass `filter` (the status bar's and `view.filter`'s visible count).
pub fn count(file: &LogFile, filter: &Filter, cancel: &dyn Fn() -> bool) -> Result<u64> {
    count_in(file, filter, cancel)
}

/// [`count`] over any [`LineSource`].
pub fn count_in<S: LineSource>(src: &S, filter: &Filter, cancel: &dyn Fn() -> bool) -> Result<u64> {
    let Some(compiled) = compile_filter(Some(filter))? else {
        return Ok(src.line_count());
    };
    let matcher = compiled.prefilter().unwrap_or_else(Matcher::all);
    let jobs = jobs(src, None);
    let mut n = 0u64;
    run_parallel(
        jobs.len(),
        |i, stop| jobs.get(i).map(|job| scan(src, job, &matcher, Some(&compiled), Want::Count, stop)).unwrap_or_default(),
        cancel,
        &mut |hits: Hits| {
            n = n.saturating_add(hits.matched);
            true
        },
    )?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::never_cancel;

    fn lines_of(text: &str, q: &Query) -> Vec<(u64, Vec<(u32, u32)>)> {
        let log = MemoryLog::new(text.as_bytes().to_vec(), FormatKind::Plain);
        let m = q.compile().unwrap();
        let mut out = Vec::new();
        let s = search_in(&log, &m, None, None, true, &never_cancel, &mut |m| {
            out.push((m.line, m.spans.iter().map(|s| (s.start, s.end)).collect()));
            true
        })
        .unwrap();
        assert!(s.complete);
        assert_eq!(s.matched, out.len() as u64);
        out
    }

    fn q(text: &str) -> Query {
        Query::text(text)
    }

    #[test]
    fn literal_ignores_case_by_default_and_reports_spans() {
        let text = "Error one\nfine\nan ERROR and an error\r\nlast error";
        assert_eq!(lines_of(text, &q("error")), vec![(0, vec![(0, 5)]), (2, vec![(3, 8), (16, 21)]), (3, vec![(5, 10)])]);
        let cs = Query { case_sensitive: true, ..q("error") };
        assert_eq!(lines_of(text, &cs), vec![(2, vec![(16, 21)]), (3, vec![(5, 10)])]);
        // no letters: the literal fast path even when ignoring case
        assert_eq!(lines_of("a 500\n404\n1500", &q("500")), vec![(0, vec![(2, 5)]), (2, vec![(1, 4)])]);
    }

    #[test]
    fn whole_word_needs_boundaries() {
        let text = "timeout\ntime out\nruntime\ntime_x\ntime-x";
        let w = Query { whole_word: true, ..q("time") };
        assert_eq!(lines_of(text, &w).iter().map(|l| l.0).collect::<Vec<_>>(), vec![1, 4]);
        let rw = Query { whole_word: true, regex: true, ..q("ti.e") };
        assert_eq!(lines_of(text, &rw).iter().map(|l| l.0).collect::<Vec<_>>(), vec![1, 4]);
        // case-sensitive literal fast path, word mode, retries after a failed candidate
        let cw = Query { whole_word: true, case_sensitive: true, ..q("a") };
        assert_eq!(lines_of("aa a\nba", &cw), vec![(0, vec![(3, 4)])]);
    }

    #[test]
    fn regex_never_matches_across_lines() {
        let text = "abc\ndef\nab c d";
        let r = Query { regex: true, ..q(r"c\s+d") };
        assert_eq!(lines_of(text, &r), vec![(2, vec![(3, 6)])]);
        let anchored = Query { regex: true, ..q("^d.f$") };
        assert_eq!(lines_of("abc\r\ndef\r\nxdef", &anchored), vec![(1, vec![(0, 3)])]);
        let neg = Query { regex: true, ..q("[^x]+z") };
        assert_eq!(lines_of("aaa\nbz", &neg), vec![(1, vec![(0, 2)])]);
    }

    #[test]
    fn empty_query_matches_every_line_and_newline_never() {
        assert_eq!(lines_of("a\n\nb\n", &q("")).len(), 3);
        assert_eq!(lines_of("a\n\n", &q("")), vec![(0, vec![]), (1, vec![])]);
        assert!(lines_of("a\nb", &q("a\nb")).is_empty());
        assert!(lines_of("", &q("x")).is_empty());
    }

    #[test]
    fn bad_and_huge_regexes_fail_cleanly() {
        let bad = Query { regex: true, ..q("(a") };
        match bad.compile() {
            Err(Error::BadQuery { message }) => assert!(message.contains("unclosed"), "{message}"),
            other => panic!("{other:?}"),
        }
        let huge = Query { regex: true, ..q(r"\w{1000}{1000}") };
        assert!(matches!(huge.compile(), Err(Error::BadQuery { .. })));
        // The classic catastrophic-backtracking pattern runs in linear time.
        let evil = Query { regex: true, ..q("(a+)+$") };
        let line = format!("{}!", "a".repeat(100_000));
        assert!(lines_of(&line, &evil).is_empty());
    }

    #[test]
    fn many_chunks_stream_in_order_and_stop_early() {
        let n = CHUNK_LINES * 5 + 17;
        let mut text = String::new();
        for i in 0..n {
            text.push_str(if i % 1000 == 7 { "needle here\n" } else { "hay hay hay\n" });
        }
        let log = MemoryLog::new(text.into_bytes(), FormatKind::Plain);
        let m = q("NEEDLE").compile().unwrap();
        let mut got = Vec::new();
        let s = search_in(&log, &m, None, None, true, &never_cancel, &mut |m| {
            got.push(m.line);
            got.len() < 10
        })
        .unwrap();
        assert_eq!(got, (0..10).map(|i| i * 1000 + 7).collect::<Vec<u64>>());
        let expected = (0..n as u64).filter(|i| i % 1000 == 7).count() as u64;
        assert_eq!((s.matched, s.scanned, s.complete), (expected, n as u64, true));

        let mut seen = 0;
        let s = search_in(&log, &m, None, None, false, &never_cancel, &mut |_| {
            seen += 1;
            false
        })
        .unwrap();
        assert!(!s.complete);
        assert_eq!(seen, 1);

        let all: Vec<u64> = lines_of(std::str::from_utf8(log.bytes()).unwrap(), &q("needle")).into_iter().map(|l| l.0).collect();
        assert_eq!(all.len() as u64, expected);
        assert!(all.windows(2).all(|w| w[0] < w[1]));

        // a sub-range across a chunk boundary
        let range = 1500..CHUNK_LINES as u64 + 10;
        let mut sub = Vec::new();
        search_in(&log, &m, None, Some(range.clone()), true, &never_cancel, &mut |m| {
            sub.push(m.line);
            true
        })
        .unwrap();
        assert_eq!(sub, (0..n as u64).filter(|i| i % 1000 == 7 && range.contains(i)).collect::<Vec<_>>());
    }

    #[test]
    fn cancel_returns_cancelled() {
        let text = "x\n".repeat(CHUNK_LINES * 3);
        let log = MemoryLog::new(text.into_bytes(), FormatKind::Plain);
        let m = q("x").compile().unwrap();
        let r = search_in(&log, &m, None, None, true, &|| true, &mut |_| true);
        assert_eq!(r, Err(Error::Cancelled));
    }

    #[test]
    fn find_next_and_count_with_filters() {
        let mut text = String::new();
        let n = CHUNK_LINES * 2 + 5;
        for i in 0..n {
            text.push_str(if i == 3 || i == CHUNK_LINES + 1 || i == n - 2 { "boom error\n" } else { "ok\n" });
        }
        let log = MemoryLog::new(text.into_bytes(), FormatKind::Plain);
        let f = Filter::parse("error").unwrap();
        let last = n as u64 - 2;
        assert_eq!(count_in(&log, &f, &never_cancel), Ok(3));
        assert_eq!(count_in(&log, &Filter::All, &never_cancel), Ok(n as u64));
        assert_eq!(find_next_in(&log, &f, 0, false, &never_cancel), Ok(Some(3)));
        assert_eq!(find_next_in(&log, &f, 3, false, &never_cancel), Ok(Some(CHUNK_LINES as u64 + 1)));
        assert_eq!(find_next_in(&log, &f, last, false, &never_cancel), Ok(None));
        assert_eq!(find_next_in(&log, &f, last, true, &never_cancel), Ok(Some(CHUNK_LINES as u64 + 1)));
        assert_eq!(find_next_in(&log, &f, 3, true, &never_cancel), Ok(None));
        assert_eq!(find_next_in(&log, &f, u64::MAX, true, &never_cancel), Ok(Some(last)));
        let not = Filter::parse("-ok").unwrap();
        assert_eq!(count_in(&log, &not, &never_cancel), Ok(3));
        let lines = Filter::parse("line > 4 and line <= 6").unwrap();
        assert_eq!(count_in(&log, &lines, &never_cancel), Ok(2));
        // search with a filter: the query and the filter both apply
        let m = q("o").compile().unwrap();
        let mut got = Vec::new();
        search_in(&log, &m, Some(&Filter::parse("boom").unwrap()), None, true, &never_cancel, &mut |m| {
            got.push(m.line);
            true
        })
        .unwrap();
        assert_eq!(got, vec![3, CHUNK_LINES as u64 + 1, last]);
    }

    #[test]
    fn hostile_bytes_never_panic() {
        let mut bytes = Vec::new();
        let mut x = 12345u32;
        for _ in 0..200_000 {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            bytes.push((x >> 16) as u8);
        }
        let log = MemoryLog::new(bytes, FormatKind::Plain);
        for (text, regex) in [("a", false), ("\u{fffd}", false), (r"[\x00-\xff]+", true), ("é", false), ("(?i)k", true), ("", false)] {
            for whole_word in [false, true] {
                let m = Query { text: text.into(), regex, whole_word, ..Query::default() }.compile().unwrap();
                let s = search_in(&log, &m, None, None, true, &never_cancel, &mut |_| true).unwrap();
                assert!(s.complete);
            }
        }
        let f = Filter::parse("level >= warn or x = 1 or /a+/").unwrap();
        assert!(count_in(&log, &f, &never_cancel).is_ok());
    }
}

/// Test helpers shared by the search stream's modules: a tiny `key=value` format.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// `t=<rfc3339> level=<word> key=value … rest`: `t` and `level` become the time and level,
    /// other `key=value` words become fields, the whole line is the message.
    pub struct KvParser;

    pub static KV: KvParser = KvParser;

    impl Parser for KvParser {
        fn kind(&self) -> FormatKind {
            FormatKind::Logfmt
        }

        fn parse(&self, line: &[u8], names: &FieldNames) -> Parsed {
            let mut p = Parsed { message: Span::new(0, line.len() as u32), ..Parsed::default() };
            let mut at = 0usize;
            for word in line.split(|b| *b == b' ') {
                let start = at;
                at += word.len() + 1;
                let Some(eq) = word.iter().position(|b| *b == b'=') else { continue };
                let key = String::from_utf8_lossy(&word[..eq]).into_owned();
                let value = &word[eq + 1..];
                let span = Span::new((start + eq + 1) as u32, (start + word.len()) as u32);
                match key.as_str() {
                    "t" => p.time = crate::filter::parse_time_value(&String::from_utf8_lossy(value)),
                    "level" => p.level = crate::Level::parse(&String::from_utf8_lossy(value)),
                    _ => p.fields.push((names.intern(&key), span)),
                }
            }
            p
        }
    }

    /// A summarized in-memory log in the `key=value` format.
    pub fn kv_log(text: &str) -> MemoryLog {
        MemoryLog::with_parser(text.as_bytes().to_vec(), &KV).summarize()
    }
}
