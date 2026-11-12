//! Plain text: the whole line is the message. A level word as the first token (`ERROR: x`,
//! `[WARN] x`) still sets the level, so jump-to-error works on unstructured logs.

use super::{FieldNames, FormatKind, Parser, cut, level_word, strip_word, whole};
use crate::Parsed;

/// The fallback: the whole line is the message.
#[derive(Debug, Default)]
pub struct PlainParser;

impl Parser for PlainParser {
    fn kind(&self) -> FormatKind {
        FormatKind::Plain
    }

    fn parse(&self, line: &[u8], names: &FieldNames) -> Parsed {
        let mut p = Parsed::default();
        self.parse_into(line, names, &mut p);
        p
    }

    fn parse_into(&self, line: &[u8], _names: &FieldNames, out: &mut Parsed) {
        let line = cut(line);
        whole(out, line);
        let end = line.iter().take(16).position(|c| matches!(c, b' ' | b'\t')).unwrap_or(line.len().min(16));
        if end > 0 {
            out.level = level_word(strip_word(line.get(..end).unwrap_or_default()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Level, Span};

    #[test]
    fn whole_line_with_leading_level() {
        let n = FieldNames::default();
        let p = PlainParser.parse(b"ERROR: disk full", &n);
        assert_eq!((p.level, p.message, p.fields.len(), p.time), (Some(Level::Error), Span::new(0, 16), 0, None));
        assert_eq!(PlainParser.parse(b"[warn] x", &n).level, Some(Level::Warn));
        assert_eq!(PlainParser.parse(b"an error happened", &n).level, None);
        assert_eq!(PlainParser.parse(b"", &n), Parsed::default());
    }
}
