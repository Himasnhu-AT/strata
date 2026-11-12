//! Text and regex search over the line index: multi-threaded over chunks, streaming matches to a
//! callback. Owned by the `search` stream.

use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::{Error, Filter, LineNo, LogFile, Result, Span};

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
        Ok(Matcher { query: self.clone() })
    }
}

/// A compiled query.
#[derive(Clone, Debug)]
pub struct Matcher {
    query: Query,
}

impl Matcher {
    pub fn query(&self) -> &Query {
        &self.query
    }

    /// The first match in `hay`.
    pub fn find(&self, hay: &[u8]) -> Option<Range<usize>> {
        let _ = hay;
        None
    }

    /// Every match in `hay` (non-overlapping), appended to `out`.
    pub fn find_all(&self, hay: &[u8], out: &mut Vec<Span>) {
        let _ = (hay, out);
    }
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
    let _ = (file, matcher, filter, lines, count_all, on_match);
    if cancel() {
        return Err(Error::Cancelled);
    }
    Ok(SearchSummary { complete: true, ..SearchSummary::default() })
}

/// The next line after `from` (before it when `backwards`) that passes `filter`: jump to error
/// (`level >= error`), next match, `log.first`. Skips chunks with
/// [`Filter::chunk_may_match`].
pub fn find_next(file: &LogFile, filter: &Filter, from: LineNo, backwards: bool, cancel: &dyn Fn() -> bool) -> Result<Option<LineNo>> {
    let _ = (file, filter, from, backwards, cancel);
    Ok(None)
}

/// How many indexed lines pass `filter` (the status bar's and `view.filter`'s visible count).
pub fn count(file: &LogFile, filter: &Filter, cancel: &dyn Fn() -> bool) -> Result<u64> {
    let _ = (file, filter, cancel);
    Ok(0)
}
