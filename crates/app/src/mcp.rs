//! Building agent results: limits, redaction and the 1-based conversions between strata-core and
//! the `crate::results` types. Owned by the `agent` stream.

use strata_core::{LineNo, Timestamp};

/// A core (0-based) line number as agents see it (1-based).
pub fn line_out(line: LineNo) -> u64 {
    line.saturating_add(1)
}

/// An agent's (1-based) line number as the core's (0-based); `None` for 0.
pub fn line_in(line: u64) -> Option<LineNo> {
    line.checked_sub(1)
}

/// A time as agents see it.
pub fn time_out(t: Option<Timestamp>) -> Option<String> {
    t.map(Timestamp::to_rfc3339)
}

#[cfg(test)]
mod tests {
    #[test]
    fn lines_are_one_based_outside() {
        assert_eq!(super::line_out(0), 1);
        assert_eq!(super::line_in(1), Some(0));
        assert_eq!(super::line_in(0), None);
    }
}
