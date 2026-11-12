//! Property tests: parsing, detection, times and redaction never panic on any input, spans stay
//! inside the line, and redaction never grows text without bound.

use proptest::prelude::*;
use strata_core::formats::{self, FieldNames, FormatKind, MAX_FIELDS_PER_LINE};
use strata_core::limits::MAX_LINE_BYTES;
use strata_core::time;
use strata_core::{Parsed, RedactKind, Redaction};

/// Bytes biased towards log-ish syntax so the parsers get past their first checks.
fn logish() -> impl Strategy<Value = Vec<u8>> {
    let pieces = prop_oneof![
        Just(b"{".to_vec()),
        Just(b"}".to_vec()),
        Just(b"[".to_vec()),
        Just(b"]".to_vec()),
        Just(b"\"".to_vec()),
        Just(b"\\".to_vec()),
        Just(b":".to_vec()),
        Just(b",".to_vec()),
        Just(b"=".to_vec()),
        Just(b" ".to_vec()),
        Just(b"<14>1 ".to_vec()),
        Just(b"\"level\":".to_vec()),
        Just(b"\"msg\":\"".to_vec()),
        Just(b"time=".to_vec()),
        Just(b"2026-11-12T07:41:03.118Z".to_vec()),
        Just(b"12/Nov/2026:07:41:03 +0000".to_vec()),
        Just(b"Nov 12 07:41:03 ".to_vec()),
        Just(b"1794469263118".to_vec()),
        Just(b"ERROR".to_vec()),
        Just(b"\\u00".to_vec()),
        Just(b"\xff\xfe".to_vec()),
        Just(b"password=".to_vec()),
        Just(b"Bearer ".to_vec()),
        Just(b"eyJ".to_vec()),
        Just(b"@".to_vec()),
        Just(b".".to_vec()),
        Just(b"-----BEGIN PRIVATE KEY-----".to_vec()),
        proptest::collection::vec(any::<u8>(), 0..8),
        "[a-zA-Z0-9]{1,40}".prop_map(String::into_bytes),
    ];
    proptest::collection::vec(pieces, 0..60).prop_map(|v| v.concat())
}

fn check_parsed(line: &[u8], p: &Parsed) {
    let len = line.len().min(MAX_LINE_BYTES) as u32;
    assert!(p.message.start <= p.message.end && p.message.end <= len, "message {:?} of {len}", p.message);
    assert!(p.fields.len() <= MAX_FIELDS_PER_LINE);
    for (_, s) in &p.fields {
        assert!(s.start <= s.end && s.end <= len, "field {s:?} of {len}");
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn parsers_never_panic_and_spans_fit(line in logish()) {
        let names = FieldNames::default();
        let mut reused = Parsed::default();
        for kind in FormatKind::ALL {
            let p = formats::parser(kind);
            let a = p.parse(&line, &names);
            check_parsed(&line, &a);
            p.parse_into(&line, &names, &mut reused);
            prop_assert_eq!(&a, &reused);
            for (_, s) in &a.fields {
                let _ = p.field_text(&line, *s);
            }
            let _ = p.field_text(&line, a.message);
        }
        let _ = formats::detect_scored(&line);
    }

    #[test]
    fn random_bytes_never_panic(line in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let names = FieldNames::default();
        for kind in FormatKind::ALL {
            check_parsed(&line, &formats::parser(kind).parse(&line, &names));
        }
        let d = formats::detect_scored(&line);
        prop_assert!((0.0..=1.0).contains(&d.confidence));
        let _ = time::scan_timestamp(&line);
        let _ = time::epoch_number(&line);
        let s = String::from_utf8_lossy(&line);
        let _ = time::parse_time(&s);
        let _ = time::parse_duration(&s);
    }

    #[test]
    fn redaction_never_panics_or_grows_unbounded(text in logish().prop_map(|b| String::from_utf8_lossy(&b).into_owned())) {
        let mut all = Redaction::default();
        all.set(RedactKind::Ipv4, true);
        all.set(RedactKind::Ipv6, true);
        for red in [Redaction::default(), all] {
            let (out, counts) = red.redact_counted(&text);
            prop_assert!(out.len() <= text.len() * 4 + 32, "{} -> {}", text.len(), out.len());
            let n: u32 = counts.iter().map(|(_, n)| *n).sum();
            prop_assert_eq!(n == 0, out == text.as_str());
            // Redacting again keeps every earlier replacement intact.
            let again = red.redact(&out);
            let marks = |s: &str| s.matches("[redacted:").count();
            prop_assert!(marks(&again) >= marks(&out));
            for piece in out.split("[redacted:").skip(1) {
                let kind = piece.split(']').next().unwrap_or_default();
                let mark = format!("[redacted:{kind}]");
                let kept = again.contains(&mark);
                prop_assert!(kept, "{} lost from {}", mark, again);
            }
        }
    }

    #[test]
    fn redaction_with_custom_patterns(text in "[a-z0-9 =@.]{0,200}", pat in "[a-z0-9.*+?|()\\[\\]]{0,12}") {
        let red = Redaction {
            custom: vec![strata_core::redact::Pattern { kind: RedactKind::Custom("c".repeat(80)), regex: pat }],
            ..Redaction::default()
        };
        let out = red.redact(&text);
        prop_assert!(out.len() <= text.len() * 43 + 64);
    }

    #[test]
    fn durations_round_trip(v in any::<i64>()) {
        prop_assume!(v != i64::MIN);
        prop_assert_eq!(time::parse_duration(&time::format_duration(v)).ok(), Some(v));
    }

    #[test]
    fn rfc3339_round_trips(micros in -62_135_596_800_000_000i64..253_402_300_799_000_000) {
        let ts = strata_core::Timestamp(micros / 1000 * 1000);
        prop_assert_eq!(time::parse_time(&ts.to_rfc3339()).ok(), Some(ts));
        let (t, n) = time::scan_timestamp(ts.to_rfc3339().as_bytes()).unwrap_or_default();
        if micros >= 0 {
            prop_assert_eq!((t, n), (ts, 24));
        }
    }
}

#[test]
fn enormous_and_hostile_lines() {
    let names = FieldNames::default();
    let huge = vec![b'a'; 4 * MAX_LINE_BYTES];
    let deep_obj = format!("{}{}", "{\"a\":".repeat(100_000), "}".repeat(100_000));
    let deep_arr = format!("{{\"a\":{}}}", "[".repeat(1_000_000));
    let many_fields: String = format!("{{{}\"z\":1}}", (0..20_000).map(|i| format!("\"k{i}\":{i},")).collect::<String>());
    let many_kv: String = (0..20_000).map(|i| format!("k{i}=v ")).collect();
    let quotes = "\"\\".repeat(100_000);
    for line in [huge, deep_obj.into_bytes(), deep_arr.into_bytes(), many_fields.into_bytes(), many_kv.into_bytes(), quotes.into_bytes()] {
        for kind in FormatKind::ALL {
            check_parsed(&line, &formats::parser(kind).parse(&line, &names));
        }
        let _ = formats::detect(&line);
        let text = String::from_utf8_lossy(&line);
        let out = Redaction::default().redact(&text);
        assert!(out.len() <= text.len() * 4 + 32);
    }
    assert!(names.all().len() <= formats::MAX_FIELD_NAMES);
}
