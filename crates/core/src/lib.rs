//! Strata's headless core: the file index, log formats, search, filters, the time histogram and
//! redaction. No egui and no ashlar: it runs in tests, the CLI and the MCP server without a window.
//!
//! # Conventions (frozen, see docs/contracts.md)
//!
//! - **Line numbers** are 0-based `u64` everywhere in this crate ([`LineNo`]). Everything a person
//!   or an agent sees (the table, MCP results, the filter grammar's `line` field) is 1-based; the
//!   app converts at the edge.
//! - **Times** are [`Timestamp`]s: microseconds since the Unix epoch, UTC.
//! - **Byte ranges** inside one line are [`Span`]s (`u32` offsets: a line is capped at
//!   [`limits::MAX_LINE_BYTES`] before it is parsed).
//! - **Cancellation**: long functions take `cancel: &dyn Fn() -> bool` and return
//!   [`Error::Cancelled`] soon after it returns `true`. The app passes ashlar's `Progress`.
//! - Nothing here panics on any input: log files are hostile (any bytes, any length, changing
//!   while open).
//!
//! # Ownership (docs/PLAN.md §8)
//!
//! `index` stream: [`mmap`], [`index`], [`follow`], [`gz`], [`file`]. `formats` stream:
//! [`formats`], [`redact`], [`time`]. `search` stream: [`search`], [`filter`], [`histogram`],
//! [`stats`]. The lead: this file, [`types`], [`error`], [`limits`].

pub mod error;
pub mod file;
pub mod filter;
pub mod follow;
pub mod formats;
pub mod gz;
pub mod histogram;
pub mod index;
pub mod limits;
pub mod mmap;
pub mod redact;
pub mod search;
pub mod stats;
pub mod time;
pub mod types;

pub use error::Error;
pub use file::{FileChange, Line, LogFile};
pub use filter::{CmpOp, Field, Filter, Literal};
pub use formats::{FieldNames, FormatKind, Parser};
pub use histogram::{Bucket, Histogram};
pub use index::{ChunkSummary, IndexState, LineIndex};
pub use redact::{RedactKind, Redaction};
pub use search::{Match, Matcher, Query, SearchSummary};
pub use stats::{FieldSummary, FieldType, FileStats};
pub use types::{FieldId, FileId, Level, LevelCounts, LineNo, Parsed, Span, Timestamp};

/// The result type of this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// A cancellation check that never cancels.
pub fn never_cancel() -> bool {
    false
}
