//! Caps that are part of the contract (docs/PLAN.md §3: "limits are part of the contract").
//! Owned by the lead. MCP results apply them and say how many items they left out.

/// Lines in one index chunk.
pub const CHUNK_LINES: usize = 65_536;
/// Bytes of a file read to detect its format.
pub const DETECT_BYTES: usize = 64 * 1024;
/// A line longer than this is shown and parsed cut (never loaded whole).
pub const MAX_LINE_BYTES: usize = 64 * 1024;
/// Line text in an MCP result is cut to this many bytes (on a char boundary).
pub const MCP_TEXT_BYTES: usize = 2 * 1024;

/// `log.search`: default and largest `limit`.
pub const SEARCH_DEFAULT: u64 = 50;
pub const SEARCH_MAX: u64 = 500;
/// `log.lines`: largest `count`.
pub const LINES_MAX: u64 = 500;
/// `log.around`: largest `before` / `after`, and the default of each.
pub const AROUND_MAX: u64 = 200;
pub const AROUND_DEFAULT: u64 = 10;
/// `view.highlight`: most lines in one highlight.
pub const HIGHLIGHT_MAX: u64 = 500;
/// `log.fields`: most common values listed per field, and most fields listed.
pub const FIELD_VALUES: usize = 20;
pub const FIELDS_MAX: usize = 200;
/// `log.stats`: top fields and top values per field.
pub const STATS_TOP_FIELDS: usize = 10;
pub const STATS_TOP_VALUES: usize = 5;
/// `log.histogram`: most buckets returned; `auto` aims for [`HISTOGRAM_AUTO`].
pub const HISTOGRAM_MAX: usize = 1000;
pub const HISTOGRAM_AUTO: usize = 120;
/// `log.files` / `bookmark.list`: most entries returned.
pub const LIST_MAX: usize = 500;
/// `agent.note`: longest note text in bytes.
pub const NOTE_MAX_BYTES: usize = 8 * 1024;

/// Cut `text` to at most `max` bytes on a char boundary; returns the text and whether it was cut.
pub fn cut(text: &str, max: usize) -> (&str, bool) {
    if text.len() <= max {
        return (text, false);
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text.get(..end).unwrap_or_default(), true)
}

#[cfg(test)]
mod tests {
    #[test]
    fn cut_keeps_char_boundaries() {
        assert_eq!(super::cut("héllo", 2), ("h", true));
        assert_eq!(super::cut("abc", 5), ("abc", false));
    }
}
