//! Redaction: every line of log text an MCP result carries passes through [`Redaction::redact`]
//! first (docs/PLAN.md, Privacy). Owned by the `formats` stream.
//!
//! A secret is replaced with `[redacted:<kind>]`, `<kind>` being [`RedactKind::as_str`] (or a
//! custom pattern's name, cut to [`CUSTOM_NAME_BYTES`]).
//!
//! The built-in patterns are hand-written scanners (linear, no regex backtracking, no
//! allocation unless something matches); custom patterns use the `regex` crate (linear time too)
//! with compiled programs cached. Patterns run in [`RedactKind::BUILTIN`] order, then the custom
//! ones; a match that overlaps an earlier one is dropped, so a JWT inside a `Bearer` header is
//! one redaction. Every built-in can be turned off on its own ([`Redaction::set`]); the IP
//! address patterns start off.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use serde::{Deserialize, Serialize};

/// A custom pattern's name is cut to this many bytes in `[redacted:<name>]`.
pub const CUSTOM_NAME_BYTES: usize = 32;

/// What a pattern finds.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedactKind {
    /// `Bearer <token>` (the token is replaced, `Bearer ` kept).
    Bearer,
    /// An `Authorization:` header value.
    Authorization,
    /// `AKIA…` access key ids (and the other AWS id prefixes); secret keys are caught as
    /// [`RedactKind::SecretField`] or [`RedactKind::Base64Secret`].
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
    /// API keys with a known prefix: `sk-…`, `sk_live_…`, `ghp_…`, `github_pat_…`, `xoxb-…`,
    /// `AIza…`, `glpat-…`, `npm_…`, `hf_…`, `SG.…`.
    ApiKey,
    /// 13–19 digits (spaces or dashes allowed between them) starting 2–6 that pass the Luhn check.
    CreditCard,
    /// The value of a `password=`, `secret:`, `"api_key": "…"`, `token=` … field.
    SecretField,
    /// Dotted-quad IPv4 addresses. Off unless turned on.
    Ipv4,
    /// IPv6 addresses. Off unless turned on.
    Ipv6,
}

impl RedactKind {
    /// The built-in kinds, in the order they are applied.
    pub const BUILTIN: [RedactKind; 13] = [
        RedactKind::PrivateKey,
        RedactKind::Jwt,
        RedactKind::Bearer,
        RedactKind::Authorization,
        RedactKind::AwsKey,
        RedactKind::ApiKey,
        RedactKind::SecretField,
        RedactKind::CreditCard,
        RedactKind::Email,
        RedactKind::Ipv6,
        RedactKind::Ipv4,
        RedactKind::HexSecret,
        RedactKind::Base64Secret,
    ];

    /// The `<kind>` in `[redacted:<kind>]`: `bearer`, `authorization`, `aws-key`, `private-key`,
    /// `jwt`, `email`, `hex-secret`, `base64-secret`, `api-key`, `credit-card`, `secret-field`,
    /// `ipv4`, `ipv6`, or the custom name.
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
            RedactKind::ApiKey => "api-key",
            RedactKind::CreditCard => "credit-card",
            RedactKind::SecretField => "secret-field",
            RedactKind::Ipv4 => "ipv4",
            RedactKind::Ipv6 => "ipv6",
        }
    }

    /// Whether a fresh [`Redaction`] applies this kind (everything but the IP addresses).
    pub fn on_by_default(&self) -> bool {
        !matches!(self, RedactKind::Ipv4 | RedactKind::Ipv6)
    }
}

/// One pattern: a kind and the regex that finds it (Rust `regex` syntax).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pattern {
    pub kind: RedactKind,
    pub regex: String,
}

impl Pattern {
    /// Whether `regex` compiles (within the size limits redaction uses); the error text if not.
    /// For Settings ▸ Redaction to check a pattern as it is typed.
    pub fn check(&self) -> Result<(), String> {
        build_regex(&self.regex).map(|_| ())
    }
}

/// The redaction filter: built-in patterns plus the person's own, or off (per file, with a
/// warning in the UI).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Redaction {
    pub enabled: bool,
    /// Patterns added in Settings ▸ Redaction (the built-ins are always included when enabled).
    pub custom: Vec<Pattern>,
    /// Built-in kinds turned off. Default: the IP addresses.
    #[serde(default = "default_disabled")]
    pub disabled: Vec<RedactKind>,
}

fn default_disabled() -> Vec<RedactKind> {
    RedactKind::BUILTIN.into_iter().filter(|k| !k.on_by_default()).collect()
}

impl Default for Redaction {
    fn default() -> Redaction {
        Redaction { enabled: true, custom: Vec::new(), disabled: default_disabled() }
    }
}

/// One match: byte range and which pattern (index into BUILTIN, then custom).
#[derive(Clone, Copy)]
struct Hit {
    start: usize,
    end: usize,
    pattern: usize,
}

impl Redaction {
    /// Redaction turned off.
    pub fn off() -> Redaction {
        Redaction { enabled: false, custom: Vec::new(), disabled: default_disabled() }
    }

    /// The built-in patterns, in the order they are applied. The regexes describe what each
    /// scanner finds (for Settings); the scanners also check what a regex cannot (Luhn, a JWT
    /// header, mixed case).
    pub fn builtin() -> Vec<Pattern> {
        RedactKind::BUILTIN
            .into_iter()
            .map(|kind| {
                let regex = match kind {
                    RedactKind::PrivateKey => r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?(-----END [A-Z ]*PRIVATE KEY-----|$)",
                    RedactKind::Jwt => r"\beyJ[A-Za-z0-9_-]{7,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{8,}",
                    RedactKind::Bearer => r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{8,}",
                    RedactKind::Authorization => r#"(?i)\bauthorization"?\s*[:=]\s*"?([A-Za-z]{1,12} )?[^\s",;&]+"#,
                    RedactKind::AwsKey => r"\b(AKIA|ASIA|AGPA|AIDA|AROA|AIPA|ANPA|ANVA|ABIA|ACCA)[A-Z0-9]{16}\b",
                    RedactKind::ApiKey => r"\b(sk-[A-Za-z0-9_-]{20,}|[sr]k_(live|test)_[A-Za-z0-9]{16,}|gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{22,}|xox[abprs]-[A-Za-z0-9-]{10,}|AIza[A-Za-z0-9_-]{35}|glpat-[A-Za-z0-9_-]{20,}|npm_[A-Za-z0-9]{36,}|hf_[A-Za-z0-9]{30,}|SG\.[A-Za-z0-9_.-]{22,})",
                    RedactKind::SecretField => {
                        r#"(?i)[A-Za-z0-9_.-]*(password|passwd|passphrase|secret|token|api_?key|api-key|access_key|private_key|credentials?)"?\s*[:=]\s*("[^"]*"|'[^']*'|[^\s&,;"'})]+)"#
                    }
                    RedactKind::CreditCard => r"\b[2-6](?:[ -]?\d){12,18}\b",
                    RedactKind::Email => r"[A-Za-z0-9._%+-]{1,64}@[A-Za-z0-9.-]+\.[A-Za-z]{2,}",
                    RedactKind::Ipv6 => r"[0-9A-Fa-f:]*:[0-9A-Fa-f:.]*:[0-9A-Fa-f:.]*",
                    RedactKind::Ipv4 => r"\b(25[0-5]|2[0-4]\d|1?\d?\d)(\.(25[0-5]|2[0-4]\d|1?\d?\d)){3}\b",
                    RedactKind::HexSecret => r"\b[0-9A-Fa-f]{32,}\b",
                    _ => r"[A-Za-z0-9+/_-]{32,}={0,2}",
                };
                Pattern { kind, regex: regex.to_string() }
            })
            .collect()
    }

    /// Whether `kind` is applied: redaction is on and the kind is not in `disabled`.
    pub fn is_on(&self, kind: &RedactKind) -> bool {
        self.enabled && !self.disabled.contains(kind)
    }

    /// Turn one built-in kind on or off.
    pub fn set(&mut self, kind: RedactKind, on: bool) {
        self.disabled.retain(|k| *k != kind);
        if !on {
            self.disabled.push(kind);
        }
    }

    /// `text` with every secret replaced by `[redacted:<kind>]`; borrowed when nothing matched.
    pub fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        self.redact_counted(text).0
    }

    /// Like [`Redaction::redact`], also counting replacements per kind (the agent panel shows
    /// "2 secrets redacted").
    pub fn redact_counted<'a>(&self, text: &'a str) -> (Cow<'a, str>, Vec<(RedactKind, u32)>) {
        if !self.enabled || text.is_empty() {
            return (Cow::Borrowed(text), Vec::new());
        }
        let b = text.as_bytes();
        let mut found = Found { covered: Vec::new(), hits: Vec::new(), len: b.len() };
        // Text redacted before stays as it is: redacting twice changes nothing.
        for s in memchr::memmem::find_iter(b, b"[redacted:") {
            if let Some(close) = b.get(s..).and_then(|t| memchr::memchr(b']', t)) {
                found.protect(s, s + close + 1);
            }
        }
        for (p, kind) in RedactKind::BUILTIN.iter().enumerate() {
            if self.is_on(kind) {
                scan(kind, b, &mut |s, e| found.add(s, e, p));
            }
        }
        let base = RedactKind::BUILTIN.len();
        for (j, pat) in self.custom.iter().enumerate() {
            if let Some(re) = cached_regex(&pat.regex) {
                for m in re.find_iter(text) {
                    found.add(m.start(), m.end(), base + j);
                }
            }
        }
        if found.hits.is_empty() {
            return (Cow::Borrowed(text), Vec::new());
        }
        found.hits.sort_by_key(|h| h.start);
        let mut out = String::with_capacity(text.len() + 16 * found.hits.len());
        let mut counts: Vec<(RedactKind, u32)> = Vec::new();
        let mut last = 0;
        for h in &found.hits {
            let (Some(before), Some(_)) = (text.get(last..h.start), text.get(h.start..h.end)) else {
                continue;
            };
            let kind = match RedactKind::BUILTIN.get(h.pattern) {
                Some(k) => k.clone(),
                None => match h.pattern.checked_sub(base).and_then(|j| self.custom.get(j)) {
                    Some(p) => p.kind.clone(),
                    None => continue,
                },
            };
            out.push_str(before);
            out.push_str("[redacted:");
            out.push_str(crate::limits::cut(kind.as_str(), CUSTOM_NAME_BYTES).0);
            out.push(']');
            last = h.end;
            match counts.iter_mut().find(|(k, _)| *k == kind) {
                Some((_, n)) => *n = n.saturating_add(1),
                None => counts.push((kind, 1)),
            }
        }
        out.push_str(text.get(last..).unwrap_or_default());
        (Cow::Owned(out), counts)
    }
}

/// Accepted matches, non-overlapping (a bitset of covered bytes).
struct Found {
    covered: Vec<u64>,
    hits: Vec<Hit>,
    len: usize,
}

impl Found {
    /// Accept `start..end` for `pattern` unless it overlaps an accepted or protected range.
    fn add(&mut self, start: usize, end: usize, pattern: usize) {
        if self.claim(start, end) {
            self.hits.push(Hit { start, end, pattern });
        }
    }

    /// Mark `start..end` as not to be redacted.
    fn protect(&mut self, start: usize, end: usize) {
        self.claim(start, end);
    }

    fn claim(&mut self, start: usize, end: usize) -> bool {
        if start >= end || end > self.len {
            return false;
        }
        if self.covered.is_empty() {
            self.covered = vec![0; self.len.div_ceil(64)];
        }
        if (start..end).any(|i| self.covered.get(i / 64).is_some_and(|w| w & (1 << (i % 64)) != 0)) {
            return false;
        }
        for i in start..end {
            if let Some(w) = self.covered.get_mut(i / 64) {
                *w |= 1 << (i % 64);
            }
        }
        true
    }
}

fn build_regex(src: &str) -> Result<regex::Regex, String> {
    regex::RegexBuilder::new(src).size_limit(1 << 20).dfa_size_limit(1 << 20).build().map_err(|e| e.to_string())
}

/// Compiled custom patterns, by source (failures cached as `None`).
fn cached_regex(src: &str) -> Option<Arc<regex::Regex>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<Arc<regex::Regex>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Mutex::default);
    let mut map = cache.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(r) = map.get(src) {
        return r.clone();
    }
    if map.len() >= 256 {
        map.clear();
    }
    let r = build_regex(src).ok().map(Arc::new);
    map.insert(src.to_string(), r.clone());
    r
}

// ---- scanners: each calls `hit(start, end)` for every match, left to right ----

fn at(b: &[u8], i: usize) -> u8 {
    b.get(i).copied().unwrap_or(0)
}

fn word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// `i` starts a word: the byte before is not alphanumeric or `_`.
fn starts_word(b: &[u8], i: usize) -> bool {
    i == 0 || !word(at(b, i - 1))
}

fn run(b: &[u8], mut i: usize, ok: impl Fn(u8) -> bool) -> usize {
    while i < b.len() && ok(at(b, i)) {
        i += 1;
    }
    i
}

fn b64url(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'-'
}

fn scan(kind: &RedactKind, b: &[u8], hit: &mut dyn FnMut(usize, usize)) {
    match kind {
        RedactKind::PrivateKey => private_keys(b, hit),
        RedactKind::Jwt => jwts(b, hit),
        RedactKind::Bearer => bearers(b, hit),
        RedactKind::Authorization => authorizations(b, hit),
        RedactKind::AwsKey => aws_keys(b, hit),
        RedactKind::ApiKey => api_keys(b, hit),
        RedactKind::SecretField => secret_fields(b, hit),
        RedactKind::CreditCard => credit_cards(b, hit),
        RedactKind::Email => emails(b, hit),
        RedactKind::Ipv6 => ipv6s(b, hit),
        RedactKind::Ipv4 => ipv4s(b, hit),
        RedactKind::HexSecret => hex_secrets(b, hit),
        RedactKind::Base64Secret => base64_secrets(b, hit),
        RedactKind::Custom(_) => {}
    }
}

fn private_keys(b: &[u8], hit: &mut dyn FnMut(usize, usize)) {
    let begin = memchr::memmem::Finder::new(b"-----BEGIN ");
    let mut from = 0;
    while let Some(rel) = b.get(from..).and_then(|s| begin.find(s)) {
        let s = from + rel;
        let header_end = b.get(s..).and_then(|t| memchr::memmem::find(t.get(..120.min(t.len())).unwrap_or_default(), b"KEY-----")).map(|k| s + k + 8);
        let Some(he) = header_end else {
            from = s + 1;
            continue;
        };
        if !b.get(s..he).is_some_and(|h| memchr::memmem::find(h, b"PRIVATE").is_some()) {
            from = he;
            continue;
        }
        let end = b
            .get(he..)
            .and_then(|t| memchr::memmem::find(t, b"-----END "))
            .and_then(|e| {
                let e = he + e;
                b.get(e..).and_then(|t| memchr::memmem::find(t.get(..120.min(t.len())).unwrap_or_default(), b"KEY-----")).map(|k| e + k + 8)
            })
            .unwrap_or(b.len());
        hit(s, end);
        from = end.max(s + 1);
    }
}

fn jwts(b: &[u8], hit: &mut dyn FnMut(usize, usize)) {
    for s in memchr::memmem::find_iter(b, b"eyJ") {
        if s > 0 && b64url(at(b, s - 1)) {
            continue;
        }
        let e1 = run(b, s, b64url);
        if e1 - s < 10 || at(b, e1) != b'.' {
            continue;
        }
        let e2 = run(b, e1 + 1, b64url);
        if e2 - e1 - 1 < 10 || at(b, e2) != b'.' {
            continue;
        }
        let e3 = run(b, e2 + 1, b64url);
        if e3 - e2 - 1 < 8 {
            continue;
        }
        hit(s, e3);
    }
}

fn token_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'~' | b'+' | b'/' | b'=' | b'-')
}

fn bearers(b: &[u8], hit: &mut dyn FnMut(usize, usize)) {
    let mut i = 0;
    while let Some(rel) = b.get(i..).and_then(|t| memchr::memchr2(b'b', b'B', t)) {
        let s = i + rel;
        i = s + 1;
        if !starts_word(b, s) || !b.get(s..s + 6).is_some_and(|w| w.eq_ignore_ascii_case(b"bearer")) {
            continue;
        }
        let ts = run(b, s + 6, |c| c == b' ' || c == b'\t');
        if ts == s + 6 {
            continue;
        }
        let te = run(b, ts, token_char);
        if te - ts >= 8 {
            hit(ts, te);
            i = te;
        }
    }
}

fn authorizations(b: &[u8], hit: &mut dyn FnMut(usize, usize)) {
    let mut i = 0;
    while let Some(rel) = b.get(i..).and_then(|t| memchr::memchr2(b'a', b'A', t)) {
        let s = i + rel;
        i = s + 1;
        if !starts_word(b, s) || !b.get(s..s + 13).is_some_and(|w| w.eq_ignore_ascii_case(b"authorization")) {
            continue;
        }
        let mut j = s + 13;
        if at(b, j) == b'"' {
            j += 1;
        }
        j = run(b, j, |c| c == b' ');
        if !matches!(at(b, j), b':' | b'=') {
            continue;
        }
        j = run(b, j + 1, |c| c == b' ');
        if at(b, j) == b'"' {
            j += 1;
        }
        let stop = |c: u8| !(c.is_ascii_whitespace() || matches!(c, b'"' | b',' | b';' | b'&'));
        let vs = j;
        let mut ve = run(b, vs, stop);
        let scheme = b.get(vs..ve).unwrap_or_default();
        if !scheme.is_empty() && scheme.len() <= 12 && scheme.iter().all(u8::is_ascii_alphabetic) && at(b, ve) == b' ' && stop(at(b, ve + 1)) {
            ve = run(b, ve + 1, stop);
        }
        if ve > vs {
            hit(vs, ve);
            i = ve;
        }
    }
}

const AWS_PREFIXES: [&[u8; 4]; 10] = [b"AKIA", b"ASIA", b"AGPA", b"AIDA", b"AROA", b"AIPA", b"ANPA", b"ANVA", b"ABIA", b"ACCA"];

fn aws_keys(b: &[u8], hit: &mut dyn FnMut(usize, usize)) {
    for s in memchr::memchr_iter(b'A', b) {
        if !starts_word(b, s) || !AWS_PREFIXES.iter().any(|p| b.get(s..s + 4) == Some(&p[..])) {
            continue;
        }
        let e = run(b, s + 4, |c| c.is_ascii_uppercase() || c.is_ascii_digit());
        if e - s == 20 && !word(at(b, e)) {
            hit(s, e);
        }
    }
}

/// (prefix, least characters after it, the characters allowed after it).
const API_PREFIXES: [(&[u8], usize, fn(u8) -> bool); 20] = [
    (b"sk-", 20, b64url),
    (b"sk_live_", 16, alnum),
    (b"sk_test_", 16, alnum),
    (b"rk_live_", 16, alnum),
    (b"rk_test_", 16, alnum),
    (b"ghp_", 36, alnum),
    (b"gho_", 36, alnum),
    (b"ghu_", 36, alnum),
    (b"ghs_", 36, alnum),
    (b"ghr_", 36, alnum),
    (b"github_pat_", 22, word),
    (b"xoxa-", 10, alnum_dash),
    (b"xoxb-", 10, alnum_dash),
    (b"xoxp-", 10, alnum_dash),
    (b"xoxr-", 10, alnum_dash),
    (b"AIza", 35, b64url),
    (b"glpat-", 20, b64url),
    (b"npm_", 36, alnum),
    (b"hf_", 30, alnum),
    (b"SG.", 22, sendgrid),
];

fn alnum(c: u8) -> bool {
    c.is_ascii_alphanumeric()
}

fn alnum_dash(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'-'
}

fn sendgrid(c: u8) -> bool {
    b64url(c) || c == b'.'
}

fn api_keys(b: &[u8], hit: &mut dyn FnMut(usize, usize)) {
    let mut i = 0;
    while i < b.len() {
        let c = at(b, i);
        if !matches!(c, b's' | b'r' | b'g' | b'x' | b'A' | b'n' | b'h' | b'S') || !starts_word(b, i) {
            i += 1;
            continue;
        }
        let mut matched = None;
        for (prefix, min, ok) in API_PREFIXES {
            if b.get(i..i + prefix.len()) == Some(prefix) {
                let e = run(b, i + prefix.len(), ok);
                if e - i - prefix.len() >= min {
                    matched = Some(e);
                    break;
                }
            }
        }
        match matched {
            Some(e) => {
                hit(i, e);
                i = e;
            }
            None => i += 1,
        }
    }
}

/// Key names (lowercase) whose values are secrets; a key ending with one counts (`db_password`).
const SECRET_KEYS: [&[u8]; 14] = [
    b"password",
    b"passwd",
    b"passphrase",
    b"secret",
    b"token",
    b"api_key",
    b"apikey",
    b"api-key",
    b"access_key",
    b"private_key",
    b"credential",
    b"credentials",
    b"client_secret",
    b"session_key",
];

fn key_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-')
}

fn secret_fields(b: &[u8], hit: &mut dyn FnMut(usize, usize)) {
    let mut i = 0;
    while let Some(rel) = b.get(i..).and_then(|t| memchr::memchr2(b'=', b':', t)) {
        let sep = i + rel;
        i = sep + 1;
        // The key ends at the separator, or at a quote right before it (JSON), allowing spaces.
        let mut ke = sep;
        while ke > 0 && at(b, ke - 1) == b' ' {
            ke -= 1;
        }
        if ke > 0 && matches!(at(b, ke - 1), b'"' | b'\'') {
            ke -= 1;
        }
        let mut ks = ke;
        while ks > 0 && ke - ks < 64 && key_char(at(b, ks - 1)) {
            ks -= 1;
        }
        let Some(key) = b.get(ks..ke) else { continue };
        if key.is_empty()
            || !SECRET_KEYS.iter().any(|k| key.len() >= k.len() && key.get(key.len() - k.len()..).is_some_and(|t| t.eq_ignore_ascii_case(k)))
        {
            continue;
        }
        if at(b, sep) == b':' && at(b, sep + 1) == b':' {
            continue;
        }
        let mut vs = run(b, sep + 1, |c| c == b' ');
        let q = at(b, vs);
        let ve = if q == b'"' || q == b'\'' {
            vs += 1;
            let mut j = vs;
            while j < b.len() && at(b, j) != q && j - vs < 1024 {
                j += if at(b, j) == b'\\' { 2 } else { 1 };
            }
            j.min(b.len())
        } else {
            run(b, vs, |c| !(c.is_ascii_whitespace() || matches!(c, b'&' | b',' | b';' | b'"' | b'\'' | b'}' | b')')))
        };
        if ve > vs {
            hit(vs, ve);
            i = ve;
        }
    }
}

fn luhn(digits: &[u8]) -> bool {
    let mut sum = 0u32;
    for (k, d) in digits.iter().rev().enumerate() {
        let mut v = u32::from(*d);
        if k % 2 == 1 {
            v *= 2;
            if v > 9 {
                v -= 9;
            }
        }
        sum += v;
    }
    sum.is_multiple_of(10)
}

fn credit_cards(b: &[u8], hit: &mut dyn FnMut(usize, usize)) {
    let mut i = 0;
    while i < b.len() {
        let c = at(b, i);
        if !c.is_ascii_digit() || !starts_word(b, i) {
            i += 1;
            continue;
        }
        let mut digits = [0u8; 19];
        let mut n = 0;
        let mut j = i;
        let mut end = i;
        while j < b.len() {
            let c = at(b, j);
            if c.is_ascii_digit() {
                if n == 19 {
                    n = 20;
                    break;
                }
                if let Some(d) = digits.get_mut(n) {
                    *d = c - b'0';
                }
                n += 1;
                j += 1;
                end = j;
            } else if matches!(c, b' ' | b'-') && at(b, j + 1).is_ascii_digit() && j > i && at(b, j - 1).is_ascii_digit() {
                j += 1;
            } else {
                break;
            }
        }
        let ok = (13..=19).contains(&n) && matches!(c, b'2'..=b'6') && !word(at(b, end)) && luhn(digits.get(..n).unwrap_or_default());
        if ok {
            hit(i, end);
            i = end;
        } else {
            // Skip the whole digit group (a card number never starts mid-number).
            i = run(b, i, |c| c.is_ascii_digit()).max(i + 1);
        }
    }
}

fn email_local(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'%' | b'+' | b'-')
}

fn emails(b: &[u8], hit: &mut dyn FnMut(usize, usize)) {
    let mut last_end = 0;
    for at_sign in memchr::memchr_iter(b'@', b) {
        if at_sign < last_end {
            continue;
        }
        let mut s = at_sign;
        while s > last_end && at_sign - s < 64 && email_local(at(b, s - 1)) {
            s -= 1;
        }
        while s < at_sign && at(b, s) == b'.' {
            s += 1;
        }
        if s == at_sign {
            continue;
        }
        let mut e = run(b, at_sign + 1, |c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-'));
        e = e.min(at_sign + 1 + 253);
        while e > at_sign + 1 && matches!(at(b, e - 1), b'.' | b'-') {
            e -= 1;
        }
        let domain = b.get(at_sign + 1..e).unwrap_or_default();
        let Some(dot) = domain.iter().rposition(|&c| c == b'.') else { continue };
        let tld = domain.get(dot + 1..).unwrap_or_default();
        if dot == 0 || tld.len() < 2 || !tld.iter().all(u8::is_ascii_alphabetic) {
            continue;
        }
        hit(s, e);
        last_end = e;
    }
}

fn ipv4s(b: &[u8], hit: &mut dyn FnMut(usize, usize)) {
    let mut i = 0;
    while i < b.len() {
        if !at(b, i).is_ascii_digit() || (i > 0 && (word(at(b, i - 1)) || at(b, i - 1) == b'.')) {
            i += 1;
            continue;
        }
        let mut j = i;
        let mut ok = true;
        for part in 0..4 {
            let e = run(b, j, |c| c.is_ascii_digit());
            let n = e - j;
            let v = b.get(j..e).and_then(|d| std::str::from_utf8(d).ok()).and_then(|d| d.parse::<u32>().ok());
            if n == 0 || n > 3 || v.is_none_or(|v| v > 255) {
                ok = false;
                break;
            }
            j = e;
            if part < 3 {
                if at(b, j) != b'.' {
                    ok = false;
                    break;
                }
                j += 1;
            }
        }
        let next = at(b, j);
        if ok && !word(next) && !(next == b'.' && at(b, j + 1).is_ascii_digit()) {
            hit(i, j);
            i = j;
        } else {
            i = run(b, i, |c| c.is_ascii_digit() || c == b'.').max(i + 1);
        }
    }
}

fn ipv6_char(c: u8) -> bool {
    c.is_ascii_hexdigit() || c == b':' || c == b'.'
}

fn ipv6s(b: &[u8], hit: &mut dyn FnMut(usize, usize)) {
    let mut i = 0;
    while i < b.len() {
        if !ipv6_char(at(b, i)) || (i > 0 && (word(at(b, i - 1)) || ipv6_char(at(b, i - 1)))) {
            i += 1;
            continue;
        }
        let mut e = run(b, i, ipv6_char);
        let next = e;
        if word(at(b, e)) {
            i = next.max(i + 1);
            continue;
        }
        // Trailing `:` / `.` of prose (`addr fe80::1: down`).
        while (e > i && at(b, e - 1) == b'.') || (e > i + 2 && at(b, e - 1) == b':' && at(b, e - 2) != b':') {
            e -= 1;
        }
        let cand = b.get(i..e).unwrap_or_default();
        let colons = cand.iter().filter(|&&c| c == b':').count();
        if colons >= 2
            && cand.iter().any(u8::is_ascii_hexdigit)
            && cand.len() <= 45
            && let Some(s) = std::str::from_utf8(cand).ok().filter(|s| s.parse::<std::net::Ipv6Addr>().is_ok())
        {
            hit(i, i + s.len());
        }
        i = next.max(i + 1);
    }
}

fn hex_secrets(b: &[u8], hit: &mut dyn FnMut(usize, usize)) {
    let mut i = 0;
    while i < b.len() {
        if !at(b, i).is_ascii_hexdigit() || !starts_word(b, i) {
            i += 1;
            continue;
        }
        let e = run(b, i, |c| c.is_ascii_hexdigit());
        let run_ = b.get(i..e).unwrap_or_default();
        if e - i >= 32 && !word(at(b, e)) && run_.iter().any(u8::is_ascii_digit) && run_.iter().any(u8::is_ascii_alphabetic) {
            hit(i, e);
        }
        i = run(b, e, word).max(i + 1);
    }
}

fn b64(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'+' | b'/' | b'_' | b'-')
}

fn class(c: u8) -> u8 {
    if c.is_ascii_uppercase() {
        0
    } else if c.is_ascii_lowercase() {
        1
    } else if c.is_ascii_digit() {
        2
    } else {
        3
    }
}

/// A base64 run from `i`: alphanumerics, with `+ / _ -` only between two of them (so
/// `abc-----BEGIN` or `a//b` are two runs).
fn b64_run(b: &[u8], mut i: usize) -> usize {
    while i < b.len() {
        let c = at(b, i);
        if c.is_ascii_alphanumeric() || (b64(c) && at(b, i + 1).is_ascii_alphanumeric()) {
            i += 1;
        } else {
            break;
        }
    }
    i
}

fn base64_secrets(b: &[u8], hit: &mut dyn FnMut(usize, usize)) {
    let mut i = 0;
    while i < b.len() {
        if !at(b, i).is_ascii_alphanumeric() {
            i += 1;
            continue;
        }
        let e = b64_run(b, i);
        let body = b.get(i..e).unwrap_or_default();
        let mut end = e;
        while end < e + 2 && at(b, end) == b'=' {
            end += 1;
        }
        let n = body.len();
        // Not glued to more word characters, and not the tail of a longer run (`x-` before it).
        let free = !word(at(b, end)) && !(i > 0 && b64(at(b, i - 1)) && i > 1 && at(b, i - 2).is_ascii_alphanumeric());
        if n >= 32 && free {
            let (mut up, mut low, mut dig, mut sep, mut changes) = (0, 0, 0, 0, 0);
            for (k, &c) in body.iter().enumerate() {
                match class(c) {
                    0 => up += 1,
                    1 => low += 1,
                    2 => dig += 1,
                    _ => sep += 1,
                }
                if k > 0 && class(c) != class(at(body, k - 1)) {
                    changes += 1;
                }
            }
            // Random text switches class often; identifiers and paths (camelCase, /a/b) do not.
            if up > 0 && low > 0 && dig > 0 && sep * 8 <= n && changes * 5 >= n * 2 {
                hit(i, end);
            }
        }
        i = end.max(i + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(text: &str) -> String {
        Redaction::default().redact(text).into_owned()
    }

    #[test]
    fn builtin_table() {
        let cases: &[(&str, &str)] = &[
            // bearer / authorization / jwt
            ("auth=Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ1MDAwMDAwMDAifQ.c2lnbmF0dXJl ok", "auth=Bearer [redacted:jwt] ok"),
            ("auth=Bearer abc.def-123_456 ok", "auth=Bearer [redacted:bearer] ok"),
            ("Authorization: Basic dXNlcjpwYXNzd29yZA== next", "Authorization: [redacted:authorization] next"),
            (r#"{"authorization":"Token abc123"}"#, r#"{"authorization":"[redacted:authorization]"}"#),
            ("Authorization: Bearer abcdefgh12345678", "Authorization: Bearer [redacted:bearer]"),
            ("token eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.dBjftJeZ4CVPmB92K27u end", "token [redacted:jwt] end"),
            // aws, api keys
            ("key AKIAIOSFODNN7EXAMPLE used", "key [redacted:aws-key] used"),
            ("XAKIAIOSFODNN7EXAMPLE", "XAKIAIOSFODNN7EXAMPLE"),
            ("openai sk-proj-abcdefghijklmnopqrstuvwxyz012345", "openai [redacted:api-key]"),
            ("gh ghp_0123456789abcdefghijklmnopqrstuvwxyz", "gh [redacted:api-key]"),
            ("slack xoxb-1234567890-abcdefghij", "slack [redacted:api-key]"),
            ("stripe sk_live_0123456789abcdef", "stripe [redacted:api-key]"),
            ("google AIzaSyA-0123456789abcdefghijklmnopqrstu", "google [redacted:api-key]"),
            ("task-runner sk-short", "task-runner sk-short"),
            // fields
            ("password=hunter2 user=bob", "password=[redacted:secret-field] user=bob"),
            ("db_password: 's3cr3t pass' x", "db_password: '[redacted:secret-field]' x"),
            (r#"{"client_secret": "abc\"def", "id": 1}"#, r#"{"client_secret": "[redacted:secret-field]", "id": 1}"#),
            ("access_token=abc&x=1", "access_token=[redacted:secret-field]&x=1"),
            ("token near expiry", "token near expiry"),
            ("std::secret::foo", "std::secret::foo"),
            // cards
            ("card 4111 1111 1111 1111 ok", "card [redacted:credit-card] ok"),
            ("card 4111-1111-1111-1111", "card [redacted:credit-card]"),
            ("card 4111111111111112 bad luhn", "card 4111111111111112 bad luhn"),
            ("epoch 1794469263118000", "epoch 1794469263118000"),
            // email
            ("from user42@example.com.", "from [redacted:email]."),
            ("a@b x@y.c @z.com me@host", "a@b x@y.c @z.com me@host"),
            // hex, base64
            ("sha 0123456789abcdef0123456789abcdef!", "sha [redacted:hex-secret]!"),
            ("id deadbeefdeadbeefdeadbeefdeadbeefdeadbeef", "id deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"),
            ("aws wJalrXUtnFEMI/K7MDENG/bPxRfiCY9XAMPLEKEY end", "aws [redacted:base64-secret] end"),
            ("class AbstractSingletonProxyFactoryBean2x", "class AbstractSingletonProxyFactoryBean2x"),
            ("path /v1/users/12345/orders/abcdef/items", "path /v1/users/12345/orders/abcdef/items"),
            // private key
            ("-----BEGIN RSA PRIVATE KEY-----\nMIIabc\n-----END RSA PRIVATE KEY----- tail", "[redacted:private-key] tail"),
            ("-----BEGIN PRIVATE KEY----- MIIcut", "[redacted:private-key]"),
            ("-----BEGIN PUBLIC KEY----- MIIpub", "-----BEGIN PUBLIC KEY----- MIIpub"),
            // IPs are off by default
            ("from 10.0.0.1 and ::1", "from 10.0.0.1 and ::1"),
            ("", ""),
        ];
        for (input, want) in cases {
            assert_eq!(r(input), *want, "{input}");
        }
    }

    #[test]
    fn ips_when_turned_on() {
        let mut red = Redaction::default();
        red.set(RedactKind::Ipv4, true);
        red.set(RedactKind::Ipv6, true);
        let cases: &[(&str, &str)] = &[
            ("from 10.0.0.1 to 255.255.255.255.", "from [redacted:ipv4] to [redacted:ipv4]."),
            ("v1.2.3.4 1.2.3.4.5 256.1.1.1 1.2.3", "v1.2.3.4 1.2.3.4.5 256.1.1.1 1.2.3"),
            ("addr fe80::1: down [::1]:8080 2001:db8::8a2e:370:7334", "addr [redacted:ipv6]: down [[redacted:ipv6]]:8080 [redacted:ipv6]"),
            ("at 07:41:03 std::vector x::y", "at 07:41:03 std::vector x::y"),
        ];
        for (input, want) in cases {
            assert_eq!(red.redact(input), *want, "{input}");
        }
    }

    #[test]
    fn toggles_custom_and_counts() {
        let mut red = Redaction::default();
        red.set(RedactKind::Email, false);
        assert!(!red.is_on(&RedactKind::Email));
        assert_eq!(red.redact("me@example.com"), "me@example.com");
        red.set(RedactKind::Email, true);
        red.custom.push(Pattern { kind: RedactKind::Custom("order".into()), regex: r"ord-\d+".into() });
        red.custom.push(Pattern { kind: RedactKind::Custom("bad".into()), regex: r"(".into() });
        red.custom.push(Pattern { kind: RedactKind::Custom("empty".into()), regex: r"x*".into() });
        let (out, counts) = red.redact_counted("me@example.com ord-1 ord-22 password=x");
        assert_eq!(out, "[redacted:email] [redacted:order] [redacted:order] password=[redacted:secret-field]");
        assert_eq!(counts, vec![(RedactKind::Email, 1), (RedactKind::Custom("order".into()), 2), (RedactKind::SecretField, 1)]);
        assert!(matches!(red.redact("nothing here"), Cow::Borrowed(_)));
        assert!(Pattern { kind: RedactKind::Custom("x".into()), regex: "(".into() }.check().is_err());
        assert!(matches!(Redaction::off().redact("password=x"), Cow::Borrowed("password=x")));
        // A long custom name is cut in the replacement.
        let long = Redaction { custom: vec![Pattern { kind: RedactKind::Custom("n".repeat(100)), regex: "z".into() }], ..Redaction::default() };
        assert_eq!(long.redact("z"), format!("[redacted:{}]", "n".repeat(CUSTOM_NAME_BYTES)));
    }

    #[test]
    fn builtin_patterns_compile_and_serde_defaults() {
        let b = Redaction::builtin();
        assert_eq!(b.len(), RedactKind::BUILTIN.len());
        for p in &b {
            assert!(p.check().is_ok(), "{:?}: {:?}", p.kind, p.check());
        }
        let old: Redaction = serde_json::from_str(r#"{"enabled":true,"custom":[]}"#).unwrap();
        assert_eq!(old, Redaction::default());
        let json = serde_json::to_string(&Redaction::default()).unwrap();
        assert_eq!(serde_json::from_str::<Redaction>(&json).unwrap(), Redaction::default());
    }

    #[test]
    fn fixture_secrets() {
        let lines = [
            r#"{"ts":"2026-11-12T00:00:00.035Z","level":"info","msg":"x","auth":"Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ1deadbeefIn0.c2lnbmF0dXJl"}"#,
            r#"time=2026-11-12T00:00:00.035Z level=info auth="user123@example.com""#,
            "[2026-11-12 00:00:00.035] INFO  api: x (request r-0000beef, 3 ms) auth=AKIA0123456789ABCDEF",
        ];
        for l in lines {
            let out = r(l);
            assert!(out.contains("[redacted:"), "{l} -> {out}");
            assert!(!out.contains("eyJ") && !out.contains("@example") && !out.contains("AKIA"), "{out}");
            assert!(out.contains("2026-11-12") && out.contains("level") || out.contains("INFO"), "{out}");
        }
    }
}
