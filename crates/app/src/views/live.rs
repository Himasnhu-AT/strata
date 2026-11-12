//! What the window keeps per open file between frames, and the jobs that fill it: loading (and
//! decompressing `.gz`), indexing, following, the filtered rows, the search matches and the
//! histogram.
//!
//! Commands cannot start jobs in ashlar 0.2, so the view reconciles each frame: it compares what
//! the document and view state ask for (a filter, a search, follow) with what it last started,
//! and starts or cancels jobs. Jobs write into a [`Derived`] behind a mutex and wake the window
//! when they report progress or finish; an idle window starts nothing and draws nothing.
//!
//! Small files (up to [`INLINE_BYTES`]) run their jobs on the UI thread at once (`.here()`), so a
//! file opened from a script or a scene is complete on the next frame; bigger ones run on workers.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use ashlar::jobs::{JobId, Progress};
use ashlar::prelude::*;
use ashlar::ui::wake::{self, Reason};
use strata_core::filter::{CmpOp, Field, Literal};
use strata_core::index::IndexState;
use strata_core::limits::CHUNK_LINES;
use strata_core::{FileChange, FileId, Filter, FormatKind, Histogram, LevelCounts, LogFile, Query};

use crate::commands::S;
use crate::doc::{Live, OpenFile};

/// Files up to this size run their jobs inline.
pub const INLINE_BYTES: u64 = 2 * 1024 * 1024;
/// Bytes indexed per step of the index job (progress and cancel between steps).
pub const INDEX_STEP: u64 = 8 * 1024 * 1024;
/// Most search matches kept for next / previous (the counter still counts them all).
pub const MATCHES_MAX: usize = 1_000_000;
/// The largest `.gz` decompressed (the temp file).
pub const GZ_MAX: u64 = 64 * 1024 * 1024 * 1024;
/// Wake reason of the follow watcher.
pub const FOLLOW: Reason = Reason("follow");

/// Lines in chunks of [`CHUNK_LINES`], appended by a job while the table reads: a snapshot is a
/// cheap clone of the chunk list.
#[derive(Clone, Debug, Default)]
pub struct Lines {
    chunks: Vec<Arc<[u64]>>,
    len: usize,
}

impl Lines {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The `i`-th line number.
    pub fn get(&self, i: usize) -> Option<u64> {
        self.chunks.get(i / CHUNK_LINES).and_then(|c| c.get(i % CHUNK_LINES)).copied()
    }

    /// The index of the first entry `>= line` (entries are sorted).
    pub fn lower_bound(&self, line: u64) -> usize {
        let c = self.chunks.partition_point(|c| c.last().is_some_and(|l| *l < line));
        let Some(chunk) = self.chunks.get(c) else { return self.len };
        c * CHUNK_LINES + chunk.partition_point(|l| *l < line)
    }

    /// The index of `line`, when it is an entry.
    pub fn position(&self, line: u64) -> Option<usize> {
        let i = self.lower_bound(line);
        (self.get(i) == Some(line)).then_some(i)
    }

    /// Append `more` (whole chunks only, except the very last call of a job).
    fn extend(&mut self, more: &mut Vec<u64>, flush: bool) {
        // a partial last chunk (an earlier flush) is filled up first, so chunks stay uniform
        if self.chunks.last().is_some_and(|c| c.len() < CHUNK_LINES)
            && !more.is_empty()
            && let Some(last) = self.chunks.pop()
        {
            self.len -= last.len();
            more.splice(0..0, last.iter().copied());
        }
        while more.len() >= CHUNK_LINES || (flush && !more.is_empty()) {
            let take = more.len().min(CHUNK_LINES);
            let chunk: Vec<u64> = more.drain(..take).collect();
            self.len += chunk.len();
            self.chunks.push(chunk.into());
            if take < CHUNK_LINES {
                break;
            }
        }
    }
}

/// Results of the jobs of one file.
#[derive(Debug, Default)]
pub struct Derived {
    /// Lines passing the filters; `None` while no filter is on.
    pub rows: Option<Lines>,
    /// Lines the filter job has looked at.
    pub rows_scanned: u64,
    pub rows_done: bool,
    /// The filter could not be read (shown on the table).
    pub rows_error: Option<String>,
    /// Matching lines of the search (at most [`MATCHES_MAX`]).
    pub matches: Lines,
    /// Every match found so far (the counter).
    pub match_total: u64,
    pub search_scanned: u64,
    pub search_done: bool,
    pub search_error: Option<String>,
    pub hist: Option<Histogram>,
    /// Loading failed: the reason (the table shows it).
    pub load_error: Option<String>,
    /// The followed file changed in a way the index cannot follow (replaced, removed).
    pub change: Option<FileChange>,
    /// The index job's progress, 0..=1 (`None`: not indexing).
    pub progress: Option<f32>,
}

/// What the search job looks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchSpec {
    pub query: Query,
}

impl SearchSpec {
    /// The search in view state (`view_keys::SEARCH`), when it is for `file`.
    pub fn from_view(v: Option<&Value>, file: FileId) -> Option<SearchSpec> {
        let v = v?;
        let for_file = v.get("file").and_then(Value::as_u64).is_none_or(|f| f == file.0);
        let text = v.get("query").and_then(Value::as_str).filter(|q| !q.is_empty())?;
        let flag = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
        for_file.then(|| SearchSpec {
            query: Query { text: text.to_string(), regex: flag("regex"), case_sensitive: flag("case"), whole_word: flag("whole_word") },
        })
    }
}

/// The filters of `f` as one filter: enabled chips, hidden levels and the time range. `Ok(None)`:
/// nothing is filtered. `Err`: a chip that does not parse (its text and the reason).
pub fn combined_filter(f: &OpenFile) -> Result<Option<Filter>, String> {
    let mut all = Filter::All;
    let mut any = false;
    for chip in f.filters.iter().filter(|c| c.enabled && !c.text.trim().is_empty()) {
        let parsed = Filter::parse(&chip.text).map_err(|e| format!("`{}`: {e}", chip.text))?;
        all = all.and(parsed);
        any = true;
    }
    for level in &f.hidden_levels {
        let is = Filter::Cmp { field: Field::Level, op: CmpOp::Eq, value: Literal::Level(*level) };
        all = all.and(Filter::Not { inner: Box::new(is) });
        any = true;
    }
    if let Some(t) = f.time_from {
        all = all.and(Filter::Cmp { field: Field::Time, op: CmpOp::Ge, value: Literal::Time(t) });
        any = true;
    }
    if let Some(t) = f.time_to {
        all = all.and(Filter::Cmp { field: Field::Time, op: CmpOp::Le, value: Literal::Time(t) });
        any = true;
    }
    Ok(any.then_some(all))
}

/// Everything the window keeps for one open file.
pub struct FileRt {
    pub id: FileId,
    pub derived: Arc<Mutex<Derived>>,
    load_job: Option<JobId>,
    index_job: Option<JobId>,
    /// Chunks summarized so far (for the level counts), bumped by the index job.
    summarized: Arc<AtomicU64>,
    watcher: Option<strata_core::follow::Watcher>,
    watching: Option<PathBuf>,
    poked: Arc<AtomicBool>,
    rows_key: Option<String>,
    rows_job: Option<JobId>,
    rows_gen: Arc<AtomicU64>,
    search_key: Option<SearchSpec>,
    search_job: Option<JobId>,
    search_gen: Arc<AtomicU64>,
    hist_key: Option<(String, u64)>,
    hist_job: Option<JobId>,
    /// The `LogFile` the derived results were computed on (a reload starts over).
    file_ptr: usize,
    levels_cache: Option<((u64, u64), LevelCounts)>,
}

fn lock(d: &Mutex<Derived>) -> MutexGuard<'_, Derived> {
    d.lock().unwrap_or_else(PoisonError::into_inner)
}

fn running(jobs: &Jobs<S>, id: Option<JobId>) -> bool {
    id.is_some_and(|id| jobs.status(id).is_some_and(|s| !s.is_finished()))
}

fn done_apply() -> Result<ashlar::jobs::Apply<S>, String> {
    Ok(apply(|_s: &mut S| Ok(Value::Null)))
}

impl FileRt {
    pub fn new(id: FileId) -> FileRt {
        FileRt {
            id,
            derived: Arc::default(),
            load_job: None,
            index_job: None,
            summarized: Arc::default(),
            watcher: None,
            watching: None,
            poked: Arc::default(),
            rows_key: None,
            rows_job: None,
            rows_gen: Arc::default(),
            search_key: None,
            search_job: None,
            search_gen: Arc::default(),
            hist_key: None,
            hist_job: None,
            file_ptr: 0,
            levels_cache: None,
        }
    }

    pub fn derived(&self) -> MutexGuard<'_, Derived> {
        lock(&self.derived)
    }

    /// Whether the index job is running.
    pub fn indexing(&self, jobs: &Jobs<S>) -> bool {
        running(jobs, self.index_job) || running(jobs, self.load_job)
    }

    /// Cancel every job of this file (closed, or Esc on indexing with `index_only`).
    pub fn cancel(&mut self, jobs: &mut Jobs<S>, index_only: bool) {
        for id in [self.load_job, self.index_job].into_iter().flatten() {
            jobs.cancel(id);
        }
        if !index_only {
            for id in [self.rows_job, self.search_job, self.hist_job].into_iter().flatten() {
                jobs.cancel(id);
            }
            self.watcher = None;
        }
    }

    /// Level counts of the summarized chunks (the level chips).
    pub fn levels(&mut self, file: &LogFile) -> LevelCounts {
        let key = (file.lines(), self.summarized.load(Ordering::Relaxed));
        if let Some((k, v)) = &self.levels_cache
            && *k == key
        {
            return *v;
        }
        let counts = file.with_index(|ix| {
            let mut all = LevelCounts::default();
            for c in (0..ix.chunks()).filter_map(|i| ix.chunk(i)).filter(|c| c.summarized) {
                all.merge(&c.levels);
            }
            all
        });
        self.levels_cache = Some((key, counts));
        counts
    }

    /// Start or stop what `f` and the view state ask for. `hold`: start nothing (a scene).
    #[allow(clippy::too_many_lines)]
    pub fn reconcile(&mut self, cx: &mut Cx<'_, crate::StrataDoc>, f: &OpenFile, kind: Option<FormatKind>, search: Option<SearchSpec>, hold: bool) {
        let live = cx.doc().live.clone();
        let Some(file) = live.get(self.id) else {
            if !hold && !running(cx.jobs, self.load_job) && lock(&self.derived).load_error.is_none() {
                self.start_load(cx, f, live);
            }
            return;
        };
        let ptr = Arc::as_ptr(&file) as usize;
        if ptr != self.file_ptr {
            // a new LogFile (opened, reloaded): start over
            self.cancel(cx.jobs, false);
            self.file_ptr = ptr;
            *lock(&self.derived) = Derived::default();
            self.rows_key = None;
            self.search_key = None;
            self.hist_key = None;
            self.summarized.store(0, Ordering::Relaxed);
            self.levels_cache = None;
            self.watching = None;
        }
        if hold {
            return;
        }
        // index
        if matches!(file.index_state(), IndexState::Idle | IndexState::Running { .. }) && !running(cx.jobs, self.index_job) {
            self.start_index(cx, &file);
        }
        // follow
        self.follow(cx, f, &file);
        let indexing = running(cx.jobs, self.index_job);
        let lines = file.lines();
        // filtered rows
        let filter = combined_filter(f);
        let rows_key = format!("{:?}|{:?}", filter, kind);
        if self.rows_key.as_deref() != Some(&rows_key) {
            if let Some(id) = self.rows_job.take() {
                cx.jobs.cancel(id);
            }
            self.rows_gen.fetch_add(1, Ordering::Relaxed);
            self.rows_key = Some(rows_key);
            let mut d = lock(&self.derived);
            d.rows_scanned = 0;
            d.rows_done = false;
            match &filter {
                Ok(Some(_)) => {
                    d.rows = Some(Lines::default());
                    d.rows_error = None;
                }
                Ok(None) => {
                    d.rows = None;
                    d.rows_error = None;
                }
                Err(e) => {
                    d.rows = Some(Lines::default());
                    d.rows_error = Some(e.clone());
                    d.rows_done = true;
                }
            }
        }
        if let Ok(Some(filter)) = &filter {
            let behind = lock(&self.derived).rows_scanned < lines;
            if behind && !running(cx.jobs, self.rows_job) {
                self.start_rows(cx, &file, filter.clone(), kind);
            }
        }
        // search
        if self.search_key != search {
            if let Some(id) = self.search_job.take() {
                cx.jobs.cancel(id);
            }
            self.search_gen.fetch_add(1, Ordering::Relaxed);
            self.search_key = search.clone();
            let mut d = lock(&self.derived);
            d.matches = Lines::default();
            d.match_total = 0;
            d.search_scanned = 0;
            d.search_done = search.is_none();
            d.search_error = None;
        }
        if let Some(spec) = &search {
            let behind = lock(&self.derived).search_scanned < lines;
            if behind && !running(cx.jobs, self.search_job) {
                self.start_search(cx, &file, spec.clone(), filter.clone().ok().flatten());
            }
        }
        // histogram: once the index is done (and again when the file grows or the filter changes)
        let hist_key = (self.rows_key.clone().unwrap_or_default(), if indexing { u64::MAX } else { lines });
        if !indexing && self.hist_key.as_ref() != Some(&hist_key) && !running(cx.jobs, self.hist_job) {
            self.hist_key = Some(hist_key);
            self.start_hist(cx, &file, filter.ok().flatten());
        }
    }

    fn start_load(&mut self, cx: &mut Cx<'_, crate::StrataDoc>, f: &OpenFile, live: Live) {
        let (id, path) = (self.id, f.path.clone());
        let derived = Arc::clone(&self.derived);
        let gz_dir = cx.session().path().and_then(Path::parent).map(Path::to_path_buf).unwrap_or_else(std::env::temp_dir).join(".strata-gz");
        let gz_dir = if gz_dir.parent().is_some_and(|p| p.metadata().is_ok_and(|m| !m.permissions().readonly())) {
            gz_dir
        } else {
            std::env::temp_dir().join("strata-gz")
        };
        let small = std::fs::metadata(&path).is_ok_and(|m| m.len() <= INLINE_BYTES);
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let job = cx.jobs.job("strata.open", format!("Opening {name}"));
        let job = if small { job.here() } else { job };
        self.load_job = Some(job.spawn((id, path, live, derived, gz_dir), |(id, path, live, derived, gz_dir), p| {
            match load(id, &path, &gz_dir, p) {
                Ok(file) => live.insert(Arc::new(file)),
                Err(strata_core::Error::Cancelled) => {}
                Err(e) => lock(&derived).load_error = Some(e.to_string()),
            }
            done_apply()
        }));
    }

    fn start_index(&mut self, cx: &mut Cx<'_, crate::StrataDoc>, file: &Arc<LogFile>) {
        let small = file.len() <= INLINE_BYTES;
        let name = file.path().file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let job = cx.jobs.job("strata.index", format!("Indexing {name}"));
        let job = if small { job.here() } else { job };
        let input = (Arc::clone(file), Arc::clone(&self.summarized), Arc::clone(&self.derived));
        self.index_job = Some(job.spawn(input, |(file, summarized, derived), p| {
            let cancel = || p.is_cancelled();
            let r = file.index_all(INDEX_STEP, &cancel, &mut |done, total| {
                lock(&derived).progress = Some(if total == 0 { 1.0 } else { done as f32 / total as f32 });
                summarize(&file, &summarized, false, &cancel);
                p.steps(done, total);
            });
            summarize(&file, &summarized, true, &cancel);
            lock(&derived).progress = None;
            match r {
                Ok(_) | Err(strata_core::Error::Cancelled) => done_apply(),
                Err(e) => Err(e.to_string()),
            }
        }));
    }

    fn follow(&mut self, cx: &mut Cx<'_, crate::StrataDoc>, f: &OpenFile, file: &Arc<LogFile>) {
        if !f.follow {
            self.watcher = None;
            self.watching = None;
            return;
        }
        if self.watching.as_deref() != Some(f.path.as_path()) {
            self.watching = Some(f.path.clone());
            let poked = Arc::clone(&self.poked);
            let waker = wake::waker(cx.ctx(), FOLLOW);
            self.watcher = strata_core::follow::Watcher::watch(&f.path, move || {
                poked.store(true, Ordering::Relaxed);
                waker.wake();
            })
            .ok();
            // catch up with what was written while not following
            self.poked.store(true, Ordering::Relaxed);
        }
        if !self.poked.load(Ordering::Relaxed) || running(cx.jobs, self.index_job) {
            return;
        }
        self.poked.store(false, Ordering::Relaxed);
        let job = cx.jobs.job("strata.follow", "Reading new lines");
        let job = if file.len() <= INLINE_BYTES { job.here() } else { job };
        let input = (Arc::clone(file), Arc::clone(&self.summarized), Arc::clone(&self.derived));
        self.index_job = Some(job.spawn(input, |(file, summarized, derived), p| {
            let cancel = || p.is_cancelled();
            match file.refresh() {
                Ok(FileChange::Appended { .. } | FileChange::Truncated) => {
                    let _ = file.index_all(INDEX_STEP, &cancel, &mut |_, _| {});
                    // the last chunk grew: count it again
                    let last = file.with_index(|ix| ix.chunks()).saturating_sub(1);
                    summarized.store(summarized.load(Ordering::Relaxed).min(last as u64), Ordering::Relaxed);
                    summarize(&file, &summarized, true, &cancel);
                }
                Ok(c @ (FileChange::Replaced | FileChange::Removed)) => lock(&derived).change = Some(c),
                Ok(FileChange::Unchanged) => {}
                Err(e) => return Err(e.to_string()),
            }
            done_apply()
        }));
    }

    fn start_rows(&mut self, cx: &mut Cx<'_, crate::StrataDoc>, file: &Arc<LogFile>, filter: Filter, kind: Option<FormatKind>) {
        let job = cx.jobs.job("strata.filter", "Filtering");
        let job = if file.len() <= INLINE_BYTES { job.here() } else { job };
        let gen_now = self.rows_gen.load(Ordering::Relaxed);
        let input = (Arc::clone(file), Arc::clone(&self.derived), Arc::clone(&self.rows_gen), filter, kind);
        self.rows_job = Some(job.spawn(input, move |(file, derived, gen_of, filter, kind), p| {
            let parser = strata_core::formats::parser(kind.unwrap_or_else(|| file.format()));
            let names = file.names();
            let total = file.lines();
            let mut at = lock(&derived).rows_scanned;
            let mut found = Vec::new();
            let current = || gen_of.load(Ordering::Relaxed) == gen_now;
            while at < total {
                if p.is_cancelled() || !current() {
                    return done_apply();
                }
                let chunk = usize::try_from(at / CHUNK_LINES as u64).unwrap_or(usize::MAX);
                let summary = file.with_index(|ix| ix.chunk(chunk));
                let chunk_end = summary.as_ref().map_or(total, |c| c.first_line + u64::from(c.lines)).min(total);
                if summary.as_ref().is_none_or(|c| filter.chunk_may_match(c)) {
                    let mut n = at;
                    while n < chunk_end {
                        let batch = file.lines_from(n, 4096, strata_core::limits::MAX_LINE_BYTES);
                        if batch.is_empty() {
                            break;
                        }
                        for line in &batch {
                            let parsed = parser.parse(&line.bytes, names);
                            if filter.matches(&line.bytes, &parsed, names, line.number) {
                                found.push(line.number);
                            }
                        }
                        n += batch.len() as u64;
                    }
                }
                at = chunk_end.max(at + 1);
                let mut d = lock(&derived);
                if current()
                    && let Some(rows) = d.rows.as_mut()
                {
                    found.truncate(virtual_rows_cap().saturating_sub(rows.len()));
                    rows.extend(&mut found, at >= total);
                }
                d.rows_scanned = at;
                d.rows_done = at >= total;
                drop(d);
                p.steps(at, total);
            }
            done_apply()
        }));
    }

    fn start_search(&mut self, cx: &mut Cx<'_, crate::StrataDoc>, file: &Arc<LogFile>, spec: SearchSpec, filter: Option<Filter>) {
        let job = cx.jobs.job("strata.search", format!("Searching for {}", spec.query.text));
        let job = if file.len() <= INLINE_BYTES { job.here() } else { job };
        let gen_now = self.search_gen.load(Ordering::Relaxed);
        let input = (Arc::clone(file), Arc::clone(&self.derived), Arc::clone(&self.search_gen), spec, filter);
        self.search_job = Some(job.spawn(input, move |(file, derived, gen_of, spec, filter), p| {
            let matcher = match spec.query.compile() {
                Ok(m) => m,
                Err(e) => {
                    lock(&derived).search_error = Some(e.to_string());
                    return done_apply();
                }
            };
            let total = file.lines();
            let from = lock(&derived).search_scanned;
            let current = || gen_of.load(Ordering::Relaxed) == gen_now;
            let mut found = Vec::new();
            let cancel = || p.is_cancelled() || !current();
            let r = strata_core::search::search(&file, &matcher, filter.as_ref(), Some(from..total), true, &cancel, &mut |m| {
                found.push(m.line);
                if found.len() >= 4096 {
                    let mut d = lock(&derived);
                    if current() && d.matches.len() < MATCHES_MAX {
                        d.matches.extend(&mut found, false);
                    }
                    p.steps(m.line.saturating_sub(from), total.saturating_sub(from));
                }
                true
            });
            let mut d = lock(&derived);
            if !current() {
                return done_apply();
            }
            match r {
                Ok(summary) => {
                    if d.matches.len() < MATCHES_MAX {
                        d.matches.extend(&mut found, true);
                    }
                    d.match_total = d.match_total.saturating_add(summary.matched);
                    d.search_scanned = total;
                    d.search_done = true;
                }
                Err(strata_core::Error::Cancelled) => {}
                Err(e) => {
                    d.search_error = Some(e.to_string());
                    d.search_scanned = total;
                    d.search_done = true;
                }
            }
            done_apply()
        }));
    }

    fn start_hist(&mut self, cx: &mut Cx<'_, crate::StrataDoc>, file: &Arc<LogFile>, filter: Option<Filter>) {
        let job = cx.jobs.job("strata.histogram", "Time histogram");
        let job = if file.len() <= INLINE_BYTES { job.here() } else { job };
        let input = (Arc::clone(file), Arc::clone(&self.derived), filter);
        self.hist_job = Some(job.spawn(input, |(file, derived, filter), p| {
            let cancel = || p.is_cancelled();
            // about 120 buckets over the file's span, down to 1 ms (the core's `auto` stops at 1 s)
            let span = file.with_index(|ix| {
                let (mut lo, mut hi) = (i64::MAX, i64::MIN);
                for c in (0..ix.chunks()).filter_map(|i| ix.chunk(i)) {
                    if let (Some(a), Some(b)) = (c.min_time, c.max_time) {
                        lo = lo.min(a.0);
                        hi = hi.max(b.0);
                    }
                }
                hi.checked_sub(lo).filter(|s| *s >= 0)
            });
            let width = span.map(|s| (s / strata_core::limits::HISTOGRAM_AUTO as i64).max(1000));
            if let Ok(h) = strata_core::histogram::histogram(&file, width, filter.as_ref(), &cancel) {
                lock(&derived).hist = Some(h);
            }
            done_apply()
        }));
    }
}

/// The most rows the table holds ([`ashlar::ui::virtual_list::MAX_ROWS`]): a filter keeps at
/// most this many lines.
pub fn virtual_rows_cap() -> usize {
    ashlar::ui::virtual_list::MAX_ROWS
}

/// Summarize the chunks the index finished (the last, partial one too when `last`).
fn summarize(file: &LogFile, done: &AtomicU64, last: bool, cancel: &dyn Fn() -> bool) {
    let (chunks, lines) = file.with_index(|ix| (ix.chunks(), ix.lines()));
    let full = usize::try_from(lines / CHUNK_LINES as u64).unwrap_or(usize::MAX).min(chunks);
    let upto = if last { chunks } else { full };
    let mut i = usize::try_from(done.load(Ordering::Relaxed)).unwrap_or(usize::MAX);
    while i < upto {
        if cancel() || file.summarize_chunk(i, cancel).is_err() {
            return;
        }
        i += 1;
        done.store(i as u64, Ordering::Relaxed);
    }
}

/// Open `path` as file `id`; a gzip file is decompressed into `gz_dir` first.
fn load(id: FileId, path: &Path, gz_dir: &Path, p: &Progress) -> strata_core::Result<LogFile> {
    let mut head = [0u8; 2];
    let gz = std::fs::File::open(path).and_then(|mut f| std::io::Read::read_exact(&mut f, &mut head)).is_ok() && strata_core::gz::is_gzip(&head);
    if !gz {
        return LogFile::open(id, path);
    }
    std::fs::create_dir_all(gz_dir).map_err(|e| strata_core::Error::io(gz_dir, &e))?;
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "log".into());
    let dst = gz_dir.join(format!("{}-{stem}", id.0));
    p.message("Decompressing");
    strata_core::gz::decompress(path, &dst, GZ_MAX, &mut |read, total| p.steps(read, total))?;
    LogFile::open(id, &dst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_index_across_chunks() {
        let mut l = Lines::default();
        let mut more: Vec<u64> = (0..(CHUNK_LINES as u64 * 2 + 5)).map(|n| n * 2).collect();
        l.extend(&mut more, false);
        assert_eq!(l.len(), CHUNK_LINES * 2);
        assert_eq!(more.len(), 5);
        l.extend(&mut more, true);
        assert_eq!(l.len(), CHUNK_LINES * 2 + 5);
        assert_eq!(l.get(0), Some(0));
        assert_eq!(l.get(CHUNK_LINES), Some(CHUNK_LINES as u64 * 2));
        assert_eq!(l.get(l.len()), None);
        assert_eq!(l.lower_bound(3), 2);
        assert_eq!(l.position(4), Some(2));
        assert_eq!(l.position(5), None);
        assert_eq!(l.lower_bound(u64::MAX), l.len());
    }

    #[test]
    fn filters_combine_levels_time_and_chips() {
        let mut f = OpenFile::default();
        assert_eq!(combined_filter(&f), Ok(None));
        f.hidden_levels = vec![strata_core::Level::Debug];
        f.time_from = Some(strata_core::Timestamp(5));
        let Ok(Some(Filter::And { all })) = combined_filter(&f) else { panic!("not an and") };
        assert_eq!(all.len(), 2);
    }
}
