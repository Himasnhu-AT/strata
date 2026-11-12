//! Filters: a small expression tree with one text form, shared by the filter bar, the CLI and MCP
//! (`view.filter`, `log.first`, `log.search`'s `filter`). Owned by the `search` stream.
//!
//! # Text grammar (frozen)
//!
//! ```text
//! filter  := or
//! or      := and ( "or" and )*
//! and     := unary ( "and"? unary )*          -- two terms side by side mean "and"
//! unary   := ( "not" | "-" ) unary | "(" filter ")" | term
//! term    := field op value                   -- service = api, status >= 500
//!          | "/" regex "/" "i"?               -- the line matches a regex (i: ignore case)
//!          | string                           -- the line contains this text, ignoring case
//! field   := "level" | "time" | "message" | "line" | name
//! name    := [A-Za-z_@][A-Za-z0-9_.@-]*       -- a parsed field (JSON paths with dots: http.status)
//! op      := "=" | "!=" | "<" | "<=" | ">" | ">=" | "~" (contains, ignoring case) | "=~" (regex)
//! value   := string | number
//! string  := '"' ( [^"\\] | '\\' any )* '"' | bare
//! bare    := [^\s()"=!<>~]+  that is not a keyword (and, or, not)
//! ```
//!
//! - Keywords are lowercase; `AND`/`OR`/`NOT` are words to search for.
//! - `level` compares by severity: `level >= warn` is warn, error and fatal.
//! - `time` values use [`crate::time::parse_time`]; `time >= 2026-11-12T14:02:00Z`.
//! - `line` is 1-based, like everything a person sees.
//! - Numbers compare numerically when both sides parse as numbers, otherwise as text.
//! - [`Filter::to_text`] prints the canonical form: lowercase keywords, single spaces, `and`
//!   written out, strings quoted only when needed, parentheses only where precedence needs them.
//!   `Filter::parse(&f.to_text()) == Ok(f)` for every filter.

use serde::{Deserialize, Serialize};

use crate::index::ChunkSummary;
use crate::{Error, FieldNames, Level, Parsed, Result, Timestamp};

/// A filter expression.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Filter {
    /// Every line (the empty filter).
    #[default]
    All,
    /// `field op value`.
    Cmp {
        field: Field,
        op: CmpOp,
        value: Literal,
    },
    /// A bare or quoted string: the line contains it, ignoring case.
    Text {
        text: String,
    },
    /// `/regex/` or `/regex/i`.
    Regex {
        pattern: String,
        ignore_case: bool,
    },
    Not {
        inner: Box<Filter>,
    },
    And {
        all: Vec<Filter>,
    },
    Or {
        any: Vec<Filter>,
    },
}

/// The left side of a comparison.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "name")]
pub enum Field {
    Level,
    Time,
    Message,
    /// The 1-based line number.
    Line,
    /// A parsed field by name.
    Named(String),
}

/// A comparison operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// `~`: contains, ignoring case.
    Contains,
    /// `=~`: matches a regex.
    Matches,
}

impl CmpOp {
    /// The operator as written in the grammar.
    pub fn as_str(self) -> &'static str {
        match self {
            CmpOp::Eq => "=",
            CmpOp::Ne => "!=",
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
            CmpOp::Gt => ">",
            CmpOp::Ge => ">=",
            CmpOp::Contains => "~",
            CmpOp::Matches => "=~",
        }
    }
}

/// The right side of a comparison, typed by what the field expects.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum Literal {
    Text(String),
    Number(f64),
    Level(Level),
    Time(Timestamp),
}

impl Filter {
    /// Parse the text form (see the module docs). Empty or blank text is [`Filter::All`].
    pub fn parse(text: &str) -> Result<Filter> {
        if text.trim().is_empty() {
            return Ok(Filter::All);
        }
        Err(Error::BadFilter { at: 0, message: "filters are not built yet".into() })
    }

    /// The canonical text form; `Filter::parse` reads it back to an equal filter.
    pub fn to_text(&self) -> String {
        String::new()
    }

    /// Whether a line passes. `line` is its bytes, `parsed` its split form, `number` its 0-based
    /// line number.
    pub fn matches(&self, line: &[u8], parsed: &Parsed, names: &FieldNames, number: u64) -> bool {
        let _ = (line, parsed, names, number);
        matches!(self, Filter::All)
    }

    /// Whether any line of a chunk could pass, from its summary alone (`true` when unsure). Lets
    /// time and level filters skip whole chunks.
    pub fn chunk_may_match(&self, chunk: &ChunkSummary) -> bool {
        let _ = chunk;
        true
    }

    /// Whether this filter needs lines parsed (`false`: text and regex terms only).
    pub fn needs_parse(&self) -> bool {
        false
    }

    /// `self and other`, flattening nested `And`s; `All` disappears.
    pub fn and(self, other: Filter) -> Filter {
        match (self, other) {
            (Filter::All, f) | (f, Filter::All) => f,
            (Filter::And { mut all }, Filter::And { all: more }) => {
                all.extend(more);
                Filter::And { all }
            }
            (Filter::And { mut all }, f) => {
                all.push(f);
                Filter::And { all }
            }
            (f, Filter::And { all }) => {
                let mut v = vec![f];
                v.extend(all);
                Filter::And { all: v }
            }
            (a, b) => Filter::And { all: vec![a, b] },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_is_all_and_and_flattens() {
        assert_eq!(Filter::parse("  "), Ok(Filter::All));
        let t = |s: &str| Filter::Text { text: s.into() };
        assert_eq!(Filter::All.and(t("a")), t("a"));
        assert_eq!(t("a").and(t("b")).and(t("c")), Filter::And { all: vec![t("a"), t("b"), t("c")] });
    }
}
