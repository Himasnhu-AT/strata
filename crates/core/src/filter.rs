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
//!
//! # Details
//!
//! - Inside `/regex/`, `\/` is a slash; every other `\x` reaches the regex as written. Inside
//!   quotes, `\x` is `x` (so `\"` and `\\`).
//! - A field a line does not have fails every comparison except `!=` (`a != b` is `not a = b`).
//! - Text equality and ordering are case-sensitive byte comparisons; `~` ignores case.
//! - The round trip holds for filters the parser produces and for any filter after
//!   [`Filter::normalize`] (no `All` inside a group, groups of two or more, literals typed the
//!   way the parser types them, finite numbers). A group nested in a group of the same kind is
//!   printed in parentheses so it reads back as written.

use std::borrow::Cow;
use std::cell::RefCell;
use std::cmp::Ordering;
use std::rc::Rc;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::index::ChunkSummary;
use crate::search::{Matcher, Query, build_regex};
use crate::{Error, FieldId, FieldNames, Level, Parsed, Result, Timestamp};

/// Longest filter text, in bytes.
pub const FILTER_MAX_BYTES: usize = 64 * 1024;
/// Deepest nesting of parentheses and `not`.
pub const FILTER_MAX_DEPTH: usize = 64;

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

impl Field {
    /// The name as written in the grammar.
    pub fn as_str(&self) -> &str {
        match self {
            Field::Level => "level",
            Field::Time => "time",
            Field::Message => "message",
            Field::Line => "line",
            Field::Named(n) => n,
        }
    }
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

    /// Whether `ord` (left compared to right) satisfies this operator (`false` for `~`, `=~`).
    fn holds(self, ord: Ordering) -> bool {
        match self {
            CmpOp::Eq => ord == Ordering::Equal,
            CmpOp::Ne => ord != Ordering::Equal,
            CmpOp::Lt => ord == Ordering::Less,
            CmpOp::Le => ord != Ordering::Greater,
            CmpOp::Gt => ord == Ordering::Greater,
            CmpOp::Ge => ord != Ordering::Less,
            CmpOp::Contains | CmpOp::Matches => false,
        }
    }

    fn holds_f64(self, a: f64, b: f64) -> bool {
        a.partial_cmp(&b).map_or(self == CmpOp::Ne, |o| self.holds(o))
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

impl Literal {
    /// The value as text (what `~` and `=~` look for).
    fn text(&self) -> String {
        match self {
            Literal::Text(s) => s.clone(),
            Literal::Number(n) => format_number(*n),
            Literal::Level(l) => l.as_str().to_string(),
            Literal::Time(t) => format_time(*t),
        }
    }
}

impl Filter {
    /// Parse the text form (see the module docs). Empty or blank text is [`Filter::All`].
    pub fn parse(text: &str) -> Result<Filter> {
        if text.trim().is_empty() {
            return Ok(Filter::All);
        }
        if text.len() > FILTER_MAX_BYTES {
            return Err(bad(FILTER_MAX_BYTES, format!("the filter is over {} KB", FILTER_MAX_BYTES / 1024)));
        }
        let mut r = Reader { s: text, pos: 0, depth: 0 };
        let f = r.or()?;
        r.skip_ws();
        match r.peek() {
            None => Ok(f),
            Some(')') => Err(bad(r.pos, "unexpected ')'")),
            Some(c) => Err(bad(r.pos, format!("unexpected '{c}'"))),
        }
    }

    /// The canonical text form; `Filter::parse` reads it back to an equal filter.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        write(self, &mut out, false);
        out
    }

    /// Whether a line passes. `line` is its bytes, `parsed` its split form, `number` its 0-based
    /// line number. Compiles the filter (cached per thread); prefer [`Filter::compile`] in loops.
    /// A filter that does not compile (a bad regex) passes nothing.
    pub fn matches(&self, line: &[u8], parsed: &Parsed, names: &FieldNames, number: u64) -> bool {
        thread_local! {
            static CACHE: RefCell<Vec<(Filter, Rc<CompiledFilter>)>> = const { RefCell::new(Vec::new()) };
        }
        let compiled = CACHE.with(|c| {
            let mut cache = c.borrow_mut();
            if let Some((_, f)) = cache.iter().find(|(f, _)| f == self) {
                return Some(Rc::clone(f));
            }
            let f = Rc::new(self.compile().ok()?);
            if cache.len() >= 8 {
                cache.remove(0);
            }
            cache.push((self.clone(), Rc::clone(&f)));
            Some(f)
        });
        compiled.is_some_and(|f| f.matches(line, parsed, names, number))
    }

    /// Compile the text and regex terms once, for evaluating many lines.
    pub fn compile(&self) -> Result<CompiledFilter> {
        Ok(CompiledFilter { node: compile(self)?, parse: self.needs_parse(), source: self.clone() })
    }

    /// Whether any line of a chunk could pass, from its summary alone (`true` when unsure). Lets
    /// time and level filters skip whole chunks.
    pub fn chunk_may_match(&self, chunk: &ChunkSummary) -> bool {
        match self {
            Filter::And { all } => all.iter().all(|f| f.chunk_may_match(chunk)),
            Filter::Or { any } => any.iter().any(|f| f.chunk_may_match(chunk)),
            Filter::Cmp { field: Field::Line, op, value: Literal::Number(n) } => {
                let lo = chunk.first_line.saturating_add(1) as f64;
                let hi = chunk.first_line.saturating_add(u64::from(chunk.lines)) as f64;
                range_may(*op, lo, hi, *n)
            }
            _ if !chunk.summarized => true,
            Filter::Cmp { field: Field::Level, op, value: Literal::Level(l) } if !matches!(op, CmpOp::Contains | CmpOp::Matches) => {
                Level::ALL.iter().any(|x| chunk.levels.get(*x) > 0 && op.holds(x.cmp(l))) || (*op == CmpOp::Ne && chunk.levels.none > 0)
            }
            Filter::Cmp { field: Field::Time, op, value: Literal::Time(t) } if !matches!(op, CmpOp::Contains | CmpOp::Matches) => {
                match (chunk.min_time, chunk.max_time) {
                    (Some(lo), Some(hi)) => range_may(*op, lo, hi, *t),
                    _ => *op == CmpOp::Ne,
                }
            }
            _ => true,
        }
    }

    /// Whether this filter needs lines parsed (`false`: text and regex terms only).
    pub fn needs_parse(&self) -> bool {
        match self {
            Filter::All | Filter::Text { .. } | Filter::Regex { .. } => false,
            Filter::Cmp { field, .. } => *field != Field::Line,
            Filter::Not { inner } => inner.needs_parse(),
            Filter::And { all } => all.iter().any(Filter::needs_parse),
            Filter::Or { any } => any.iter().any(Filter::needs_parse),
        }
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

    /// The same filter in normal form: nested `And`s and `Or`s flattened, `All` removed from
    /// groups (an `Or` with `All` is `All`), one-item groups unwrapped, `\/` in regexes written
    /// `/`. Normal filters survive [`Filter::to_text`] and [`Filter::parse`] unchanged.
    pub fn normalize(self) -> Filter {
        match self {
            Filter::And { all } => {
                let mut out = Vec::new();
                for f in all {
                    match f.normalize() {
                        Filter::All => {}
                        Filter::And { all } => out.extend(all),
                        f => out.push(f),
                    }
                }
                match out.len() {
                    0 => Filter::All,
                    1 => out.pop().unwrap_or_default(),
                    _ => Filter::And { all: out },
                }
            }
            Filter::Or { any } => {
                let mut out = Vec::new();
                for f in any {
                    match f.normalize() {
                        Filter::All => return Filter::All,
                        Filter::Or { any } => out.extend(any),
                        f => out.push(f),
                    }
                }
                if out.len() == 1 { out.pop().unwrap_or_default() } else { Filter::Or { any: out } }
            }
            Filter::Not { inner } => match inner.normalize() {
                Filter::All => Filter::Not { inner: Box::new(Filter::Text { text: String::new() }) },
                f => Filter::Not { inner: Box::new(f) },
            },
            Filter::Regex { pattern, ignore_case } => Filter::Regex { pattern: unescape_slashes(&pattern), ignore_case },
            f => f,
        }
    }
}

/// Whether some value in `lo..=hi` could satisfy `value op n`.
fn range_may<T: PartialOrd>(op: CmpOp, lo: T, hi: T, n: T) -> bool {
    match op {
        CmpOp::Eq => lo <= n && n <= hi,
        CmpOp::Lt => lo < n,
        CmpOp::Le => lo <= n,
        CmpOp::Gt => hi > n,
        CmpOp::Ge => hi >= n,
        CmpOp::Ne | CmpOp::Contains | CmpOp::Matches => true,
    }
}

fn bad(at: usize, message: impl Into<String>) -> Error {
    Error::BadFilter { at, message: message.into() }
}

// ---------------------------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------------------------

fn is_delim(c: char) -> bool {
    c.is_whitespace() || matches!(c, '(' | ')' | '"' | '=' | '!' | '<' | '>' | '~')
}

fn is_keyword(s: &str) -> bool {
    matches!(s, "and" | "or" | "not")
}

fn is_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '@')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '@' | '-'))
}

/// `-?digits(.digits)?([eE][+-]?digits)?`: what an unquoted value must look like to be a number.
fn is_number(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = usize::from(b.first() == Some(&b'-'));
    let digits = |i: &mut usize| {
        let start = *i;
        while b.get(*i).is_some_and(u8::is_ascii_digit) {
            *i += 1;
        }
        *i > start
    };
    if !digits(&mut i) {
        return false;
    }
    if b.get(i) == Some(&b'.') {
        i += 1;
        if !digits(&mut i) {
            return false;
        }
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        if !digits(&mut i) {
            return false;
        }
    }
    i == b.len()
}

struct Reader<'a> {
    s: &'a str,
    pos: usize,
    depth: usize,
}

impl<'a> Reader<'a> {
    fn rest(&self) -> &'a str {
        self.s.get(self.pos..).unwrap_or_default()
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.bump();
        }
    }

    fn at_group_end(&self) -> bool {
        matches!(self.peek(), None | Some(')'))
    }

    /// The keyword at the cursor, if a whole word.
    fn keyword(&self) -> Option<&'static str> {
        ["and", "or", "not"].into_iter().find(|kw| self.rest().strip_prefix(kw).is_some_and(|after| after.chars().next().is_none_or(is_delim)))
    }

    fn eat_keyword(&mut self, kw: &'static str) -> bool {
        if self.keyword() == Some(kw) {
            self.pos += kw.len();
            true
        } else {
            false
        }
    }

    fn bare(&mut self) -> &'a str {
        let start = self.pos;
        while self.peek().is_some_and(|c| !is_delim(c)) {
            self.bump();
        }
        self.s.get(start..self.pos).unwrap_or_default()
    }

    fn or(&mut self) -> Result<Filter> {
        let mut any = vec![self.and()?];
        loop {
            self.skip_ws();
            let at = self.pos;
            if !self.eat_keyword("or") {
                break;
            }
            self.skip_ws();
            if self.at_group_end() || self.keyword().is_some_and(|k| k != "not") {
                return Err(bad(at, "expected a term after 'or'"));
            }
            any.push(self.and()?);
        }
        Ok(if any.len() == 1 { any.pop().unwrap_or_default() } else { Filter::Or { any } })
    }

    fn and(&mut self) -> Result<Filter> {
        let mut all = vec![self.unary()?];
        loop {
            self.skip_ws();
            if self.at_group_end() || self.keyword() == Some("or") {
                break;
            }
            let at = self.pos;
            if self.eat_keyword("and") {
                self.skip_ws();
                if self.at_group_end() || self.keyword().is_some_and(|k| k != "not") {
                    return Err(bad(at, "expected a term after 'and'"));
                }
            }
            all.push(self.unary()?);
        }
        Ok(if all.len() == 1 { all.pop().unwrap_or_default() } else { Filter::And { all } })
    }

    fn unary(&mut self) -> Result<Filter> {
        self.skip_ws();
        self.depth += 1;
        let r = if self.depth > FILTER_MAX_DEPTH { Err(bad(self.pos, "the filter is nested too deeply")) } else { self.unary_inner() };
        self.depth -= 1;
        r
    }

    fn unary_inner(&mut self) -> Result<Filter> {
        let start = self.pos;
        match self.peek() {
            None => Err(bad(start, "expected a term")),
            Some('-') => {
                self.bump();
                Ok(Filter::Not { inner: Box::new(self.unary()?) })
            }
            Some('(') => {
                self.bump();
                self.skip_ws();
                if self.peek() == Some(')') {
                    return Err(bad(start, "empty parentheses"));
                }
                let f = self.or()?;
                self.skip_ws();
                if self.peek() == Some(')') {
                    self.bump();
                    Ok(f)
                } else {
                    Err(bad(start, "this '(' is never closed"))
                }
            }
            Some(')') => Err(bad(start, "unexpected ')'")),
            Some('"') => {
                let text = self.quoted()?;
                let after = self.pos;
                self.skip_ws();
                if self.op().is_some() {
                    return Err(bad(start, "a field name can't be quoted"));
                }
                self.pos = after;
                Ok(Filter::Text { text })
            }
            Some('/') => self.regex(),
            Some('!') => Err(bad(start, "use 'not' or '-' to negate, '!=' to compare")),
            Some(c) if is_delim(c) => Err(bad(start, format!("expected a term before '{c}'"))),
            Some(_) => {
                if self.eat_keyword("not") {
                    return Ok(Filter::Not { inner: Box::new(self.unary()?) });
                }
                if let Some(kw) = self.keyword() {
                    return Err(bad(start, format!("expected a term before '{kw}'")));
                }
                let word = self.bare();
                let after = self.pos;
                self.skip_ws();
                match self.op() {
                    Some(op) => self.cmp(word, start, op),
                    None => {
                        self.pos = after;
                        Ok(Filter::Text { text: word.to_string() })
                    }
                }
            }
        }
    }

    fn op(&mut self) -> Option<CmpOp> {
        const OPS: [(&str, CmpOp); 8] = [
            ("=~", CmpOp::Matches),
            ("!=", CmpOp::Ne),
            ("<=", CmpOp::Le),
            (">=", CmpOp::Ge),
            ("=", CmpOp::Eq),
            ("<", CmpOp::Lt),
            (">", CmpOp::Gt),
            ("~", CmpOp::Contains),
        ];
        let (text, op) = OPS.into_iter().find(|(t, _)| self.rest().starts_with(t))?;
        self.pos += text.len();
        Some(op)
    }

    fn cmp(&mut self, word: &str, start: usize, op: CmpOp) -> Result<Filter> {
        let field = match word {
            "level" => Field::Level,
            "time" => Field::Time,
            "message" => Field::Message,
            "line" => Field::Line,
            name if is_name(name) => Field::Named(name.to_string()),
            _ => return Err(bad(start, format!("'{word}' is not a field name (letters, digits, _ . @ -)"))),
        };
        self.skip_ws();
        let vstart = self.pos;
        let (raw, quoted) = match self.peek() {
            Some('"') => (self.quoted()?, true),
            Some(c) if !is_delim(c) => {
                let w = self.bare();
                if is_keyword(w) {
                    return Err(bad(vstart, format!("expected a value after '{}', found '{w}' (quote it to mean the word)", op.as_str())));
                }
                (w.to_string(), false)
            }
            _ => return Err(bad(vstart, format!("expected a value after '{}'", op.as_str()))),
        };
        let value = literal(&field, op, raw, quoted).map_err(|m| bad(vstart, m))?;
        Ok(Filter::Cmp { field, op, value })
    }

    fn quoted(&mut self) -> Result<String> {
        let start = self.pos;
        self.bump();
        let mut out = String::new();
        loop {
            match self.bump() {
                None => return Err(bad(start, "this quote is never closed")),
                Some('"') => return Ok(out),
                Some('\\') => match self.bump() {
                    Some(c) => out.push(c),
                    None => return Err(bad(start, "this quote is never closed")),
                },
                Some(c) => out.push(c),
            }
        }
    }

    fn regex(&mut self) -> Result<Filter> {
        let start = self.pos;
        self.bump();
        let mut pattern = String::new();
        loop {
            match self.bump() {
                None => return Err(bad(start, "this regex is never closed (end it with '/')")),
                Some('/') => break,
                Some('\\') => match self.bump() {
                    Some('/') => pattern.push('/'),
                    Some(c) => {
                        pattern.push('\\');
                        pattern.push(c);
                    }
                    None => return Err(bad(start, "this regex is never closed (end it with '/')")),
                },
                Some(c) => pattern.push(c),
            }
        }
        let mut ignore_case = false;
        if self.peek() == Some('i') && self.rest().get(1..).is_some_and(|r| r.chars().next().is_none_or(is_delim)) {
            self.bump();
            ignore_case = true;
        }
        if self.peek().is_some_and(|c| !is_delim(c)) {
            return Err(bad(self.pos, "expected a space after the regex (its only flag is i)"));
        }
        build_regex(&pattern, ignore_case).map_err(|e| bad(start, format!("bad regex: {e}")))?;
        Ok(Filter::Regex { pattern, ignore_case })
    }
}

/// Type the value of `field op raw`.
fn literal(field: &Field, op: CmpOp, raw: String, quoted: bool) -> std::result::Result<Literal, String> {
    match op {
        CmpOp::Contains => return Ok(Literal::Text(raw)),
        CmpOp::Matches => {
            build_regex(&raw, false).map_err(|e| format!("bad regex: {e}"))?;
            return Ok(Literal::Text(raw));
        }
        _ => {}
    }
    match field {
        Field::Level => {
            Level::parse(&raw).map(Literal::Level).ok_or_else(|| format!("unknown level '{raw}' (trace, debug, info, warn, error or fatal)"))
        }
        Field::Time => parse_time_value(&raw).map(Literal::Time).ok_or_else(|| format!("not a time: '{raw}' (e.g. 2026-11-12T14:02:00Z)")),
        Field::Line => match raw.parse::<f64>() {
            Ok(n) if is_number(&raw) => Ok(Literal::Number(n)),
            _ => Err(format!("line needs a number, not '{raw}'")),
        },
        Field::Message | Field::Named(_) => match raw.parse::<f64>() {
            Ok(n) if !quoted && is_number(&raw) => Ok(Literal::Number(n)),
            _ => Ok(Literal::Text(raw)),
        },
    }
}

// ---------------------------------------------------------------------------------------------
// Printing
// ---------------------------------------------------------------------------------------------

fn quote(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
}

/// Whether `s` reads back as one bare word.
fn bare_ok(s: &str) -> bool {
    !s.is_empty() && !is_keyword(s) && !s.chars().any(is_delim)
}

fn write_text_term(s: &str, out: &mut String) {
    if bare_ok(s) && !s.starts_with('-') && !s.starts_with('/') {
        out.push_str(s);
    } else {
        quote(s, out);
    }
}

fn write_value(v: &Literal, out: &mut String) {
    match v {
        Literal::Text(s) => {
            if bare_ok(s) && !is_number(s) {
                out.push_str(s);
            } else {
                quote(s, out);
            }
        }
        Literal::Number(n) if n.is_nan() => quote("NaN", out),
        Literal::Number(n) => out.push_str(&format_number(*n)),
        Literal::Level(l) => out.push_str(l.as_str()),
        Literal::Time(t) => out.push_str(&format_time(*t)),
    }
}

fn write_regex(pattern: &str, ignore_case: bool, out: &mut String) {
    out.push('/');
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                out.push('\\');
                out.push(chars.next().unwrap_or('\\'));
            }
            '/' => out.push_str("\\/"),
            c => out.push(c),
        }
    }
    out.push('/');
    if ignore_case {
        out.push('i');
    }
}

fn write_group(f: &Filter, out: &mut String) {
    out.push('(');
    write(f, out, true);
    out.push(')');
}

/// Print `f`; `nested`: inside another expression (an `All` there must print as something).
fn write(f: &Filter, out: &mut String, nested: bool) {
    match f {
        Filter::All => {
            if nested {
                out.push_str("\"\"");
            }
        }
        Filter::Text { text } => write_text_term(text, out),
        Filter::Regex { pattern, ignore_case } => write_regex(pattern, *ignore_case, out),
        Filter::Cmp { field, op, value } => {
            out.push_str(field.as_str());
            out.push(' ');
            out.push_str(op.as_str());
            out.push(' ');
            write_value(value, out);
        }
        Filter::Not { inner } => match inner.as_ref() {
            Filter::Text { .. } | Filter::Regex { .. } | Filter::All => {
                out.push('-');
                write(inner, out, true);
            }
            Filter::And { .. } | Filter::Or { .. } => {
                out.push_str("not ");
                write_group(inner, out);
            }
            _ => {
                out.push_str("not ");
                write(inner, out, true);
            }
        },
        Filter::And { all } => {
            if all.is_empty() {
                out.push_str("\"\"");
            }
            for (i, f) in all.iter().enumerate() {
                if i > 0 {
                    out.push_str(" and ");
                }
                if matches!(f, Filter::And { .. } | Filter::Or { .. }) {
                    write_group(f, out);
                } else {
                    write(f, out, true);
                }
            }
        }
        Filter::Or { any } => {
            if any.is_empty() {
                out.push_str("-\"\"");
            }
            for (i, f) in any.iter().enumerate() {
                if i > 0 {
                    out.push_str(" or ");
                }
                if matches!(f, Filter::Or { .. }) {
                    write_group(f, out);
                } else {
                    write(f, out, true);
                }
            }
        }
    }
}

/// `\/` written as `/` (the form the parser stores).
fn unescape_slashes(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('/') => out.push('/'),
                Some(n) => {
                    out.push('\\');
                    out.push(n);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// A number as the grammar reads it back exactly (`1e999` for infinity).
fn format_number(n: f64) -> String {
    if n.is_nan() {
        "NaN".to_string()
    } else if n.is_infinite() {
        if n > 0.0 { "1e999".to_string() } else { "-1e999".to_string() }
    } else {
        format!("{n}")
    }
}

// ---------------------------------------------------------------------------------------------
// Times: exact RFC 3339 printing and reading (microseconds, any year)
// ---------------------------------------------------------------------------------------------

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z.saturating_add(719_468);
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// (year, month, day) to days since 1970-01-01.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// `2026-11-12T14:02:00Z`, with `.mmm` or `.uuuuuu` when the time has them; years outside
/// 0000–9999 get a sign (`+12345-…`, `-0001-…`). [`parse_time_value`] reads it back exactly.
pub(crate) fn format_time(t: Timestamp) -> String {
    let secs = t.0.div_euclid(Timestamp::MICROS_PER_SECOND);
    let us = t.0.rem_euclid(Timestamp::MICROS_PER_SECOND);
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let sod = secs.rem_euclid(86_400);
    let year = if (0..=9999).contains(&y) {
        format!("{y:04}")
    } else if y < 0 {
        format!("-{:04}", y.unsigned_abs())
    } else {
        format!("+{y}")
    };
    let frac = if us == 0 {
        String::new()
    } else if us % 1000 == 0 {
        format!(".{:03}", us / 1000)
    } else {
        format!(".{us:06}")
    };
    format!("{year}-{m:02}-{d:02}T{:02}:{:02}:{:02}{frac}Z", sod / 3600, sod % 3600 / 60, sod % 60)
}

/// Read `[+-]YYYY-MM-DDTHH:MM:SS[.fraction]Z` exactly (fractions past microseconds dropped).
fn parse_rfc3339(s: &str) -> Option<Timestamp> {
    let b = s.as_bytes();
    let mut i = 0usize;
    let neg = match b.first() {
        Some(b'-') => {
            i = 1;
            true
        }
        Some(b'+') => {
            i = 1;
            false
        }
        _ => false,
    };
    let num = |i: &mut usize, min: usize, max: usize| -> Option<i64> {
        let start = *i;
        while *i - start < max && b.get(*i).is_some_and(u8::is_ascii_digit) {
            *i += 1;
        }
        if *i - start < min {
            return None;
        }
        s.get(start..*i)?.parse().ok()
    };
    let expect = |i: &mut usize, c: u8| -> Option<()> { (b.get(*i) == Some(&c)).then(|| *i += 1) };
    let year = num(&mut i, 4, 9)?;
    let year = if neg { -year } else { year };
    expect(&mut i, b'-')?;
    let month = u32::try_from(num(&mut i, 2, 2)?).ok()?;
    expect(&mut i, b'-')?;
    let day = u32::try_from(num(&mut i, 2, 2)?).ok()?;
    expect(&mut i, b'T')?;
    let hour = num(&mut i, 2, 2)?;
    expect(&mut i, b':')?;
    let min = num(&mut i, 2, 2)?;
    expect(&mut i, b':')?;
    let sec = num(&mut i, 2, 2)?;
    let mut us = 0i64;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        let digits = s.get(start..i)?;
        if digits.is_empty() || digits.len() > 9 {
            return None;
        }
        let six: String = digits.chars().chain(std::iter::repeat('0')).take(6).collect();
        us = six.parse().ok()?;
    }
    expect(&mut i, b'Z')?;
    if i != b.len() || !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) || hour > 23 || min > 59 || sec > 59 {
        return None;
    }
    let secs = i128::from(days_from_civil(year, month, day)) * 86_400 + i128::from(hour * 3600 + min * 60 + sec);
    i64::try_from(secs * 1_000_000 + i128::from(us)).ok().map(Timestamp)
}

/// A time value in a filter: the exact form [`format_time`] prints, else whatever
/// [`crate::time::parse_time`] reads.
pub(crate) fn parse_time_value(s: &str) -> Option<Timestamp> {
    parse_rfc3339(s).or_else(|| crate::time::parse_time(s).ok())
}

// ---------------------------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------------------------

/// A filter with its regexes and text terms compiled: what search, count and the histogram run
/// per line. Build it with [`Filter::compile`].
#[derive(Clone, Debug)]
pub struct CompiledFilter {
    source: Filter,
    node: Node,
    parse: bool,
}

#[derive(Clone, Debug)]
enum Node {
    All,
    /// A text or regex term on the whole line.
    Line(Box<Matcher>),
    Not(Box<Node>),
    And(Vec<Node>),
    Or(Vec<Node>),
    Cmp(Box<CmpNode>),
}

#[derive(Clone, Debug)]
struct CmpNode {
    field: Key,
    op: CmpOp,
    value: Value,
}

#[derive(Clone, Debug)]
enum Key {
    Level,
    Time,
    Message,
    Line,
    /// A field name and its id once a line has had it.
    Named(String, OnceLock<FieldId>),
}

#[derive(Clone, Debug)]
enum Value {
    Level(Level),
    Time(Timestamp),
    Number(f64),
    /// Text and its numeric value when it has one.
    Text(String, Option<f64>),
    /// `~` and `=~`.
    Find(Box<Matcher>),
}

fn query_error(e: Error) -> Error {
    match e {
        Error::BadQuery { message } => bad(0, message),
        e => e,
    }
}

fn compile(f: &Filter) -> Result<Node> {
    Ok(match f {
        Filter::All => Node::All,
        Filter::Text { text } => Node::Line(Box::new(Query::text(text.clone()).compile().map_err(query_error)?)),
        Filter::Regex { pattern, ignore_case } => {
            let q = Query { text: pattern.clone(), regex: true, case_sensitive: !ignore_case, whole_word: false };
            Node::Line(Box::new(q.compile().map_err(query_error)?))
        }
        Filter::Not { inner } => Node::Not(Box::new(compile(inner)?)),
        Filter::And { all } => Node::And(all.iter().map(compile).collect::<Result<_>>()?),
        Filter::Or { any } => Node::Or(any.iter().map(compile).collect::<Result<_>>()?),
        Filter::Cmp { field, op, value } => {
            let key = match field {
                Field::Level => Key::Level,
                Field::Time => Key::Time,
                Field::Message => Key::Message,
                Field::Line => Key::Line,
                Field::Named(n) => Key::Named(n.clone(), OnceLock::new()),
            };
            let value = match (op, value) {
                (CmpOp::Contains, v) => Value::Find(Box::new(Query::text(v.text()).compile().map_err(query_error)?)),
                (CmpOp::Matches, v) => {
                    let q = Query { text: v.text(), regex: true, case_sensitive: true, whole_word: false };
                    Value::Find(Box::new(q.compile().map_err(query_error)?))
                }
                (_, Literal::Level(l)) => Value::Level(*l),
                (_, Literal::Time(t)) => Value::Time(*t),
                (_, Literal::Number(n)) => Value::Number(*n),
                (_, Literal::Text(s)) => Value::Text(s.clone(), parse_num(s.as_bytes())),
            };
            Node::Cmp(Box::new(CmpNode { field: key, op: *op, value }))
        }
    })
}

/// A field value as a finite number.
fn parse_num(v: &[u8]) -> Option<f64> {
    std::str::from_utf8(v).ok()?.trim().parse::<f64>().ok().filter(|x| x.is_finite())
}

impl CompiledFilter {
    /// The filter this was compiled from.
    pub fn filter(&self) -> &Filter {
        &self.source
    }

    /// Whether lines must be parsed for [`CompiledFilter::matches`] (else pass any `Parsed`).
    pub fn needs_parse(&self) -> bool {
        self.parse
    }

    /// See [`Filter::chunk_may_match`].
    pub fn chunk_may_match(&self, chunk: &ChunkSummary) -> bool {
        self.source.chunk_may_match(chunk)
    }

    /// Whether a line passes (see [`Filter::matches`]).
    pub fn matches(&self, line: &[u8], parsed: &Parsed, names: &FieldNames, number: u64) -> bool {
        eval(&self.node, line, parsed, names, number)
    }

    /// A text or regex term every passing line must match: search finds candidates with it
    /// before evaluating the whole filter.
    pub(crate) fn prefilter(&self) -> Option<Matcher> {
        let usable = |n: &Node| match n {
            Node::Line(m) if !m.is_all() => Some(Matcher::clone(m)),
            _ => None,
        };
        match &self.node {
            Node::And(all) => all.iter().find_map(usable),
            n => usable(n),
        }
    }
}

fn eval(n: &Node, line: &[u8], parsed: &Parsed, names: &FieldNames, number: u64) -> bool {
    match n {
        Node::All => true,
        Node::Line(m) => m.find(line).is_some(),
        Node::Not(inner) => !eval(inner, line, parsed, names, number),
        Node::And(all) => all.iter().all(|n| eval(n, line, parsed, names, number)),
        Node::Or(any) => any.iter().any(|n| eval(n, line, parsed, names, number)),
        Node::Cmp(c) => eval_cmp(c, line, parsed, names, number),
    }
}

fn eval_cmp(c: &CmpNode, line: &[u8], parsed: &Parsed, names: &FieldNames, number: u64) -> bool {
    let op = c.op;
    let missing = op == CmpOp::Ne;
    match (&c.field, &c.value) {
        (Key::Level, Value::Level(l)) => return parsed.level.map_or(missing, |v| op.holds(v.cmp(l))),
        (Key::Time, Value::Time(t)) => return parsed.time.map_or(missing, |v| op.holds(v.cmp(t))),
        (Key::Line, Value::Number(n)) => return op.holds_f64(number.saturating_add(1) as f64, *n),
        _ => {}
    }
    let text: Option<Cow<'_, [u8]>> = match &c.field {
        Key::Level => parsed.level.map(|l| Cow::Borrowed(l.as_str().as_bytes())),
        Key::Time => parsed.time.map(|t| Cow::Owned(t.to_rfc3339().into_bytes())),
        Key::Line => Some(Cow::Owned(number.saturating_add(1).to_string().into_bytes())),
        Key::Message => parsed.message.slice(line).map(Cow::Borrowed),
        Key::Named(name, id) => {
            let id = match id.get() {
                Some(id) => Some(*id),
                None => names.id(name).inspect(|found| {
                    let _ = id.set(*found);
                }),
            };
            id.and_then(|id| parsed.field(id)).and_then(|s| s.slice(line)).map(Cow::Borrowed)
        }
    };
    let Some(v) = text else { return missing };
    match &c.value {
        Value::Find(m) => m.find(&v).is_some(),
        Value::Number(n) => match parse_num(&v) {
            Some(x) => op.holds_f64(x, *n),
            None => op.holds(v.as_ref().cmp(format_number(*n).as_bytes())),
        },
        Value::Text(s, num) => match (parse_num(&v), num) {
            (Some(a), Some(b)) => op.holds_f64(a, *b),
            _ => op.holds(v.as_ref().cmp(s.as_bytes())),
        },
        Value::Level(l) => op.holds(v.as_ref().cmp(l.as_str().as_bytes())),
        Value::Time(t) => op.holds(v.as_ref().cmp(format_time(*t).as_bytes())),
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::search::testing::kv_log;
    use crate::search::{LineSource, count_in};

    fn t(s: &str) -> Filter {
        Filter::Text { text: s.into() }
    }

    fn cmp(field: Field, op: CmpOp, value: Literal) -> Filter {
        Filter::Cmp { field, op, value }
    }

    fn named(s: &str) -> Field {
        Field::Named(s.into())
    }

    fn err_at(text: &str) -> (usize, String) {
        match Filter::parse(text) {
            Err(Error::BadFilter { at, message }) => (at, message),
            other => panic!("{text:?} parsed: {other:?}"),
        }
    }

    #[test]
    fn empty_text_is_all_and_and_flattens() {
        assert_eq!(Filter::parse("  "), Ok(Filter::All));
        assert_eq!(Filter::All.and(t("a")), t("a"));
        assert_eq!(t("a").and(t("b")).and(t("c")), Filter::And { all: vec![t("a"), t("b"), t("c")] });
    }

    #[test]
    fn contract_examples_parse() {
        assert_eq!(
            Filter::parse("level >= warn and service = api"),
            Ok(Filter::And {
                all: vec![cmp(Field::Level, CmpOp::Ge, Literal::Level(Level::Warn)), cmp(named("service"), CmpOp::Eq, Literal::Text("api".into()))]
            })
        );
        assert_eq!(
            Filter::parse(r#"status >= 500 -"healthz""#),
            Ok(Filter::And { all: vec![cmp(named("status"), CmpOp::Ge, Literal::Number(500.0)), Filter::Not { inner: Box::new(t("healthz")) }] })
        );
        assert_eq!(Filter::parse("time >= 2026-11-12T14:02:00Z"), Ok(cmp(Field::Time, CmpOp::Ge, Literal::Time(Timestamp(1_794_492_120_000_000)))));
        assert_eq!(Filter::parse("/time(d)?out/i"), Ok(Filter::Regex { pattern: "time(d)?out".into(), ignore_case: true }));
    }

    #[test]
    fn precedence_and_implicit_and() {
        let f = Filter::parse("a b or not c and (d or e)").unwrap();
        let want = Filter::Or {
            any: vec![
                Filter::And { all: vec![t("a"), t("b")] },
                Filter::And { all: vec![Filter::Not { inner: Box::new(t("c")) }, Filter::Or { any: vec![t("d"), t("e")] }] },
            ],
        };
        assert_eq!(f, want);
        assert_eq!(f.to_text(), "a and b or -c and (d or e)");
        // upper-case keywords are words
        assert_eq!(Filter::parse("AND"), Ok(t("AND")));
        assert_eq!(Filter::parse("android or norway"), Ok(Filter::Or { any: vec![t("android"), t("norway")] }));
        assert_eq!(Filter::parse("http.status>=500"), Ok(cmp(named("http.status"), CmpOp::Ge, Literal::Number(500.0))));
        assert_eq!(Filter::parse(r#"msg = "500""#), Ok(cmp(named("msg"), CmpOp::Eq, Literal::Text("500".into()))));
        assert_eq!(Filter::parse(r"/a\/b/"), Ok(Filter::Regex { pattern: "a/b".into(), ignore_case: false }));
    }

    #[test]
    fn errors_say_where_and_what() {
        assert_eq!(err_at("level >= loud").0, 9);
        assert!(err_at("level >= loud").1.contains("unknown level"));
        assert_eq!(err_at("a and (b or c").0, 6);
        assert_eq!(err_at("a )").0, 2);
        assert_eq!(err_at("\"abc").0, 0);
        assert_eq!(err_at("x /(/").0, 2);
        assert!(err_at("x /(/").1.starts_with("bad regex"));
        assert_eq!(err_at("a and").0, 2);
        assert_eq!(err_at("a or or b").0, 2);
        assert_eq!(err_at("status >= ").0, 10);
        assert_eq!(err_at("= 5").0, 0);
        assert_eq!(err_at("time > yesterday-ish").0, 7);
        assert_eq!(err_at("line = x").0, 7);
        assert_eq!(err_at("a ! b").0, 2);
        assert_eq!(err_at("\"f\" = 1").0, 0);
        assert_eq!(err_at("()").0, 0);
        assert_eq!(err_at("a = and").0, 4);
        assert!(err_at("/a/x").1.contains("space"));
        assert!(err_at("1x = 2").1.contains("not a field name"));
        let deep = format!("{}a{}", "(".repeat(100), ")".repeat(100));
        assert!(err_at(&deep).1.contains("deeply"));
        let deep_not = format!("{}a", "not ".repeat(100));
        assert!(err_at(&deep_not).1.contains("deeply"));
    }

    #[test]
    fn times_print_and_read_exactly() {
        for us in [
            0,
            1,
            -1,
            1_794_492_120_000_000,
            1_794_492_120_118_000,
            1_794_492_120_118_123,
            i64::MIN,
            i64::MAX,
            -62_135_596_800_000_001,
            253_402_300_800_000_000,
        ] {
            let s = format_time(Timestamp(us));
            assert_eq!(parse_rfc3339(&s), Some(Timestamp(us)), "{s}");
        }
        assert_eq!(format_time(Timestamp(1_794_492_120_118_000)), "2026-11-12T14:02:00.118Z");
        assert_eq!(parse_rfc3339("2026-02-30T00:00:00Z"), None);
        assert_eq!(parse_rfc3339("2026-11-12T14:02:00.1234567891Z"), None);
        assert_eq!(parse_rfc3339("2026-11-12T14:02:00.123456789Z"), Some(Timestamp(1_794_492_120_123_456)));
    }

    #[test]
    fn evaluates_against_parsed_lines() {
        let log = kv_log(concat!(
            "t=2026-11-12T00:00:00Z level=info service=api status=200 hello\n",
            "t=2026-11-12T00:00:01Z level=warn service=db status=503 slow query\n",
            "t=2026-11-12T00:00:02Z level=error service=api status=500 boom\n",
            "no structure at all\n",
            "t=2026-11-12T00:00:03Z level=debug service=API status=abc x\n",
        ));
        let n = |s: &str| count_in(&log, &Filter::parse(s).unwrap(), &crate::never_cancel).unwrap();
        assert_eq!(n("level >= warn"), 2);
        assert_eq!(n("level < info"), 1);
        assert_eq!(n("level != info"), 4);
        assert_eq!(n("service = api"), 2);
        assert_eq!(n("service ~ api"), 3);
        assert_eq!(n("service != api"), 3);
        // "abc" is not a number, so it compares as text: "abc" > "500"
        assert_eq!(n("status >= 500"), 3);
        assert_eq!(n("status > 2e2"), 3);
        assert_eq!(n("status >= 500 and status < 1000"), 2);
        assert_eq!(n("status = abc"), 1);
        assert_eq!(n("time >= 2026-11-12T00:00:01Z and time < 2026-11-12T00:00:03Z"), 2);
        assert_eq!(n("/^no/"), 1);
        assert_eq!(n("/SLOW/i or BOOM"), 2);
        assert_eq!(n("service =~ ^a.i$"), 2);
        assert_eq!(n("line >= 4"), 2);
        assert_eq!(n("not (level >= warn or line = 4)"), 2);
        assert_eq!(n("missing = 1"), 0);
        assert_eq!(n("missing != 1"), 5);
        assert_eq!(n("message ~ query"), 1);

        let f = Filter::parse("service = db").unwrap();
        let line = log_line(&log, 1);
        assert!(f.matches(&line, &log.parse_line(&line), log.field_names(), 1));
        assert!(!f.matches(b"service=api", &log.parse_line(b"service=api"), log.field_names(), 0));
    }

    fn log_line(log: &crate::search::MemoryLog, n: u64) -> Vec<u8> {
        log.line(n).unwrap().to_vec()
    }

    #[test]
    fn chunk_summaries_skip_chunks() {
        let chunk = ChunkSummary {
            first_line: 0,
            lines: 10,
            bytes: 0..100,
            min_time: Some(Timestamp(100)),
            max_time: Some(Timestamp(200)),
            levels: {
                let mut l = crate::LevelCounts::default();
                l.add(Some(Level::Info));
                l.add(Some(Level::Warn));
                l
            },
            summarized: true,
        };
        let may = |s: &str| Filter::parse(s).unwrap().chunk_may_match(&chunk);
        assert!(may("level >= warn"));
        assert!(!may("level >= error"));
        assert!(!may("level = debug or line > 10"));
        assert!(may("line = 10"));
        assert!(!may("level >= error and x"));
        assert!(may("not level >= error"));
        let at = |us: i64| format_time(Timestamp(us));
        assert!(!may(&format!("time > {}", at(200))));
        assert!(may(&format!("time >= {}", at(200))));
        assert!(!may(&format!("time < {}", at(100))));
        assert!(!may(&format!("time = {}", at(201))));
        let unsummarized = ChunkSummary { summarized: false, ..chunk.clone() };
        assert!(Filter::parse("level >= fatal").unwrap().chunk_may_match(&unsummarized));
    }

    // ---- round trip ----------------------------------------------------------------------

    fn arb_level() -> impl Strategy<Value = Level> {
        prop::sample::select(Level::ALL.to_vec())
    }

    fn arb_ord_op() -> impl Strategy<Value = CmpOp> {
        prop::sample::select(vec![CmpOp::Eq, CmpOp::Ne, CmpOp::Lt, CmpOp::Le, CmpOp::Gt, CmpOp::Ge])
    }

    fn arb_regex() -> impl Strategy<Value = String> {
        prop::sample::select(vec![
            "a+",
            "foo|bar",
            r"\d{3}",
            "x/y",
            "^start",
            "end$",
            "(?i)Mixed",
            "[a-z]+/[0-9]",
            r#"say "hi""#,
            r"a\\b",
            "",
            " sp ace ",
        ])
        .prop_map(String::from)
    }

    fn arb_name() -> impl Strategy<Value = String> {
        "[A-Za-z_@][A-Za-z0-9_.@-]{0,6}"
            .prop_filter("not reserved", |s| !matches!(s.as_str(), "and" | "or" | "not" | "level" | "time" | "message" | "line"))
    }

    fn arb_number() -> impl Strategy<Value = f64> {
        prop_oneof![any::<f64>().prop_filter("not NaN", |x| !x.is_nan()), (-1000i32..1000).prop_map(f64::from), Just(f64::INFINITY), Just(-0.0)]
    }

    fn arb_text() -> impl Strategy<Value = String> {
        prop_oneof![
            any::<String>().prop_map(|s| s.chars().take(8).collect()),
            "[a-z0-9 ./\\\\\"()=!<>~-]{0,8}",
            prop::sample::select(vec!["and", "or", "not", "AND", "-x", "/x/", "500", "1e5", "-3", ""]).prop_map(String::from),
        ]
    }

    fn arb_leaf() -> impl Strategy<Value = Filter> {
        let field = prop_oneof![Just(Field::Level), Just(Field::Time), Just(Field::Message), Just(Field::Line), arb_name().prop_map(Field::Named)];
        prop_oneof![
            arb_text().prop_map(|text| Filter::Text { text }),
            (arb_regex(), any::<bool>()).prop_map(|(pattern, ignore_case)| Filter::Regex { pattern, ignore_case }),
            (arb_ord_op(), arb_level()).prop_map(|(op, l)| cmp(Field::Level, op, Literal::Level(l))),
            (arb_ord_op(), any::<i64>()).prop_map(|(op, us)| cmp(Field::Time, op, Literal::Time(Timestamp(us)))),
            (arb_ord_op(), arb_number()).prop_map(|(op, n)| cmp(Field::Line, op, Literal::Number(n))),
            (prop_oneof![Just(Field::Message), arb_name().prop_map(Field::Named)], arb_ord_op(), arb_number()).prop_map(|(f, op, n)| cmp(
                f,
                op,
                Literal::Number(n)
            )),
            (prop_oneof![Just(Field::Message), arb_name().prop_map(Field::Named)], arb_ord_op(), arb_text()).prop_map(|(f, op, s)| cmp(
                f,
                op,
                Literal::Text(s)
            )),
            (field.clone(), arb_text()).prop_map(|(f, s)| cmp(f, CmpOp::Contains, Literal::Text(s))),
            (field, arb_regex()).prop_map(|(f, s)| cmp(f, CmpOp::Matches, Literal::Text(s))),
        ]
    }

    fn arb_filter() -> impl Strategy<Value = Filter> {
        arb_leaf().prop_recursive(4, 24, 4, |inner| {
            prop_oneof![
                inner.clone().prop_map(|f| Filter::Not { inner: Box::new(f) }),
                prop::collection::vec(inner.clone(), 2..4).prop_map(|all| Filter::And { all }),
                prop::collection::vec(inner, 2..4).prop_map(|any| Filter::Or { any }),
            ]
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 2000, ..ProptestConfig::default() })]

        #[test]
        fn round_trip(f in arb_filter()) {
            let text = f.to_text();
            prop_assert_eq!(Filter::parse(&text), Ok(f.clone()), "text: {}", text);
            let norm = f.normalize();
            prop_assert_eq!(Filter::parse(&norm.to_text()), Ok(norm));
        }

        #[test]
        fn parse_never_panics_and_reprints(s in "\\PC{0,40}", t in "[a-z ()\"/=!<>~-]{0,30}") {
            for input in [s, t] {
                if let Ok(f) = Filter::parse(&input) {
                    prop_assert_eq!(Filter::parse(&f.to_text()), Ok(f));
                }
            }
        }
    }
}
