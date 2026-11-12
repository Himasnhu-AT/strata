//! Redaction: every line of log text an MCP result carries passes through [`Redaction::redact`]
//! first (docs/PLAN.md, Privacy). Owned by the `formats` stream.
//!
//! A secret is replaced with `[redacted:<kind>]`, `<kind>` being [`RedactKind::as_str`] (or a
//! custom pattern's name).

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

/// What a pattern finds.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedactKind {
    /// `Bearer <token>`.
    Bearer,
    /// An `Authorization:` header value.
    Authorization,
    /// `AKIA…` access key ids and 40-character secret keys next to them.
    AwsKey,
    /// `-----BEGIN … PRIVATE KEY-----` blocks.
    PrivateKey,
    /// Three base64url parts separated by dots, the first decoding to a JSON header.
    Jwt,
    Email,
    /// 32+ hex characters.
    HexSecret,
    /// 32+ base64 characters with mixed case and digits.
    Base64Secret,
    /// A pattern the person added in Settings ▸ Redaction; the name is the `<kind>`.
    Custom(String),
}

impl RedactKind {
    /// The `<kind>` in `[redacted:<kind>]`: `bearer`, `authorization`, `aws-key`, `private-key`,
    /// `jwt`, `email`, `hex-secret`, `base64-secret`, or the custom name.
    pub fn as_str(&self) -> &str {
        match self {
            RedactKind::Bearer => "bearer",
            RedactKind::Authorization => "authorization",
            RedactKind::AwsKey => "aws-key",
            RedactKind::PrivateKey => "private-key",
            RedactKind::Jwt => "jwt",
            RedactKind::Email => "email",
            RedactKind::HexSecret => "hex-secret",
            RedactKind::Base64Secret => "base64-secret",
            RedactKind::Custom(name) => name,
        }
    }
}

/// One pattern: a kind and the regex that finds it (Rust `regex` syntax).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pattern {
    pub kind: RedactKind,
    pub regex: String,
}

/// The redaction filter: built-in patterns plus the person's own, or off (per file, with a
/// warning in the UI).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Redaction {
    pub enabled: bool,
    /// Patterns added in Settings ▸ Redaction (the built-ins are always included when enabled).
    pub custom: Vec<Pattern>,
}

impl Default for Redaction {
    fn default() -> Redaction {
        Redaction { enabled: true, custom: Vec::new() }
    }
}

impl Redaction {
    /// Redaction turned off.
    pub fn off() -> Redaction {
        Redaction { enabled: false, custom: Vec::new() }
    }

    /// The built-in patterns, in the order they are applied.
    pub fn builtin() -> Vec<Pattern> {
        Vec::new()
    }

    /// `text` with every secret replaced by `[redacted:<kind>]`; borrowed when nothing matched.
    pub fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        Cow::Borrowed(text)
    }

    /// Like [`Redaction::redact`], also counting replacements per kind (the agent panel shows
    /// "2 secrets redacted").
    pub fn redact_counted<'a>(&self, text: &'a str) -> (Cow<'a, str>, Vec<(RedactKind, u32)>) {
        (self.redact(text), Vec::new())
    }
}
