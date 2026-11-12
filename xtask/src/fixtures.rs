//! Deterministic log fixtures for tests and benchmarks: the same `--size`, `--format` and `--seed`
//! always give the same bytes (a SplitMix64 generator, no system randomness, no clock).
//!
//! Lines look like a small service mesh's logs: five services, levels mostly info with some debug,
//! warnings and rare errors in bursts, request ids, latencies, HTTP statuses, a stack trace now and
//! then (plain format, the default), and a few secrets for the redaction tests (bearer tokens, emails, AWS key
//! ids). Times start at 2026-11-12T00:00:00Z and advance 0–40 ms a line.

use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use strata_core::Timestamp;

/// SplitMix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// 0..n (n > 0).
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        let i = usize::try_from(self.below(items.len() as u64)).unwrap_or(0);
        items.get(i).copied().unwrap_or_default()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Fmt {
    Jsonl,
    Logfmt,
    Plain,
    Syslog,
    Access,
}

const SERVICES: &[&str] = &["api", "payments", "auth", "search", "worker"];
const PATHS: &[&str] = &["/v1/orders", "/v1/orders/{id}", "/v1/pay", "/v1/login", "/v1/search", "/healthz", "/v1/users/{id}"];
const METHODS: &[&str] = &["GET", "GET", "GET", "POST", "PUT", "DELETE"];
const INFO: &[&str] = &["request handled", "cache hit", "cache miss", "job finished", "connection opened", "user signed in", "order created"];
const DEBUG: &[&str] = &["db query", "retry scheduled", "config reloaded", "span exported", "pool stats"];
const WARN: &[&str] = &["slow request", "retrying upstream", "queue depth high", "deprecated header", "token near expiry"];
const ERROR: &[&str] = &["upstream timeout", "connection refused", "payment declined", "deadlock detected", "invalid signature"];

struct Event {
    time: Timestamp,
    level: &'static str,
    service: &'static str,
    msg: &'static str,
    request: u64,
    method: &'static str,
    path: String,
    status: u16,
    latency_ms: u64,
    secret: Option<String>,
    trace: bool,
}

fn event(r: &mut Rng, time: Timestamp, burst: &mut u32) -> Event {
    if *burst == 0 && r.below(4000) == 0 {
        *burst = 5 + u32::try_from(r.below(40)).unwrap_or(0);
    }
    let roll = r.below(1000);
    let (level, msgs) = if *burst > 0 {
        *burst -= 1;
        if roll < 600 { ("error", ERROR) } else { ("warn", WARN) }
    } else if roll < 2 {
        ("error", ERROR)
    } else if roll < 60 {
        ("warn", WARN)
    } else if roll < 200 {
        ("debug", DEBUG)
    } else {
        ("info", INFO)
    };
    let id = r.below(100_000);
    let path = r.pick(PATHS).replace("{id}", &id.to_string());
    let status = match level {
        "error" => [500, 502, 503, 504][usize::try_from(r.below(4)).unwrap_or(0)],
        "warn" => [200, 404, 429][usize::try_from(r.below(3)).unwrap_or(0)],
        _ => [200, 200, 200, 201, 204, 304][usize::try_from(r.below(6)).unwrap_or(0)],
    };
    let secret = match r.below(5000) {
        0 => Some(format!("Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ1{:08x}In0.c2lnbmF0dXJl", r.next() as u32)),
        1 => Some(format!("user{}@example.com", r.below(10_000))),
        2 => Some(format!("AKIA{:016X}", r.next())),
        _ => None,
    };
    Event {
        time,
        level,
        service: r.pick(SERVICES),
        msg: r.pick(msgs),
        request: r.next() & 0xffff_ffff,
        method: r.pick(METHODS),
        path,
        status,
        latency_ms: if level == "error" { 1000 + r.below(29_000) } else { r.below(400) },
        secret,
        trace: level == "error" && r.below(3) == 0,
    }
}

fn write_line(out: &mut String, f: Fmt, e: &Event, seq: u64) {
    let ts = e.time.to_rfc3339();
    match f {
        Fmt::Jsonl => {
            let _ = write!(
                out,
                r#"{{"ts":"{ts}","level":"{}","service":"{}","msg":"{}","request_id":"r-{:08x}","method":"{}","path":"{}","status":{},"latency_ms":{}"#,
                e.level, e.service, e.msg, e.request, e.method, e.path, e.status, e.latency_ms
            );
            if let Some(s) = &e.secret {
                let _ = write!(out, r#","auth":"{s}""#);
            }
            out.push_str("}\n");
        }
        Fmt::Logfmt => {
            let _ = write!(
                out,
                r#"time={ts} level={} service={} msg="{}" request_id=r-{:08x} method={} path={} status={} latency_ms={}"#,
                e.level, e.service, e.msg, e.request, e.method, e.path, e.status, e.latency_ms
            );
            if let Some(s) = &e.secret {
                let _ = write!(out, r#" auth="{s}""#);
            }
            out.push('\n');
        }
        Fmt::Plain => plain_line(out, e, seq),
        Fmt::Syslog => {
            let pri = match e.level {
                "error" => 11,
                "warn" => 12,
                "debug" => 15,
                _ => 14,
            };
            let _ = writeln!(out, "<{pri}>1 {ts} host-{} {} {} - - {} request_id=r-{:08x}", seq % 7, e.service, 1000 + seq % 50, e.msg, e.request);
        }
        Fmt::Access => {
            let _ = writeln!(
                out,
                r#"10.0.{}.{} - - [{}] "{} {} HTTP/1.1" {} {} "-" "strata-fixture/1.0" {}"#,
                e.request % 256,
                (e.request >> 8) % 256,
                apache_time(&ts),
                e.method,
                e.path,
                e.status,
                200 + e.latency_ms * 3,
                e.latency_ms
            );
        }
    }
}

/// A plain text line as a person would read and write it: `2026-11-12 10:00:01.513 ERROR
/// [payments] upstream timeout after 5s request_id=r-0000beef status=504 latency_ms=5012`, and
/// after some errors a stack trace (Java for the JVM services, Python for the others) whose lines
/// have no time of their own. Extra variety comes from the event's request id and `seq`, not the
/// generator, so the other formats' bytes do not depend on this one.
fn plain_line(out: &mut String, e: &Event, seq: u64) {
    let ts = e.time.to_rfc3339();
    let t = ts.get(..23).unwrap_or(&ts).replace('T', " ");
    let _ = write!(out, "{t} {:<5} [{}] ", e.level.to_uppercase(), e.service);
    let user = format!("u-{}", 1000 + e.request % 9000);
    let http = matches!(e.msg, "request handled" | "slow request");
    match e.msg {
        "request handled" => {
            let _ = write!(out, "{} {} {} in {} ms", e.method, e.path, e.status, e.latency_ms);
        }
        "user signed in" => {
            let _ = write!(out, "user signed in user={user}");
        }
        "order created" => {
            let _ = write!(out, "order created order={} user={user} amount={}.{:02}", 8000 + e.request % 2000, 5 + e.request % 400, e.request % 100);
        }
        "slow request" => {
            let _ = write!(out, "slow request {} {} took {} ms", e.method, e.path, e.latency_ms);
        }
        "upstream timeout" => {
            let _ = write!(out, "upstream timeout after {}s calling ledger", e.latency_ms / 1000);
        }
        "db query" => {
            let _ = write!(out, "db query rows={} ms={}", e.request % 500, e.latency_ms % 40);
        }
        "queue depth high" => {
            let _ = write!(out, "queue depth high depth={}", 500 + e.request % 4000);
        }
        msg => out.push_str(msg),
    }
    let _ = write!(out, " request_id=r-{:08x}", e.request);
    if !http {
        let _ = write!(out, " status={} latency_ms={}", e.status, e.latency_ms);
    }
    if let Some(s) = &e.secret {
        let _ = if s.contains(' ') { write!(out, " auth=\"{s}\"") } else { write!(out, " auth={s}") };
    }
    out.push('\n');
    if !e.trace {
        return;
    }
    let line = 40 + (seq % 260);
    if matches!(e.service, "search" | "worker") {
        let err = match e.msg {
            "upstream timeout" => "TimeoutError",
            "connection refused" => "ConnectionRefusedError",
            "deadlock detected" => "psycopg.errors.DeadlockDetected",
            _ => "RuntimeError",
        };
        let _ = writeln!(out, "Traceback (most recent call last):");
        let _ = writeln!(
            out,
            "  File \"/srv/{0}/app/handlers.py\", line {line}, in handle\n    result = self.client.call(request, timeout=30)",
            e.service
        );
        let _ = writeln!(out, "  File \"/srv/{0}/app/client.py\", line {1}, in call\n    raise {err}(msg)", e.service, 12 + seq % 90);
        let _ = writeln!(out, "{err}: {}", e.msg);
    } else {
        let (class, frame) = match e.msg {
            "upstream timeout" => ("java.net.SocketTimeoutException", "http.LedgerClient.send"),
            "connection refused" => ("java.net.ConnectException", "http.LedgerClient.connect"),
            "payment declined" => ("com.acme.payments.DeclinedException", "card.Authorizer.authorize"),
            "deadlock detected" => ("java.sql.SQLTransactionRollbackException", "db.OrderRepository.save"),
            _ => ("java.security.SignatureException", "auth.TokenVerifier.verify"),
        };
        let file = frame.rsplit('.').nth(1).unwrap_or("Main");
        let _ = writeln!(out, "{class}: {}", e.msg);
        let _ = writeln!(out, "\tat com.acme.{}.{frame}({file}.java:{line})", e.service);
        let _ = writeln!(out, "\tat com.acme.{0}.OrderHandler.handle(OrderHandler.java:{1})", e.service, 60 + seq % 40);
        let _ = writeln!(out, "\tat com.acme.platform.Router.dispatch(Router.java:141)");
        let _ = writeln!(out, "\t... {} more", 18 + seq % 12);
    }
}

/// `2026-11-12T00:00:00.035Z` as Apache writes it: `12/Nov/2026:00:00:00 +0000`.
fn apache_time(rfc3339: &str) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let part = |a: usize, b: usize| rfc3339.get(a..b).unwrap_or("00");
    let month = part(5, 7).parse::<usize>().ok().and_then(|m| MONTHS.get(m.wrapping_sub(1))).copied().unwrap_or("Jan");
    format!("{}/{month}/{}:{} +0000", part(8, 10), part(0, 4), part(11, 19))
}

/// `100M`, `1G`, `512K`, `1000` (bytes; powers of 1024).
fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (num, mult) = match s.char_indices().last() {
        Some((i, 'K' | 'k')) => (s.get(..i), 1u64 << 10),
        Some((i, 'M' | 'm')) => (s.get(..i), 1 << 20),
        Some((i, 'G' | 'g')) => (s.get(..i), 1 << 30),
        _ => (Some(s), 1),
    };
    num.and_then(|n| n.parse::<u64>().ok()).and_then(|n| n.checked_mul(mult)).ok_or_else(|| format!("bad size `{s}` (e.g. 100M, 1G)"))
}

/// Write `size` bytes (whole lines, stopping at the first line that reaches it) of `f` to `out`.
fn generate(f: Fmt, size: u64, seed: u64, out: &mut dyn Write) -> Result<u64, String> {
    let mut r = Rng(seed);
    let mut time = Timestamp(1_794_441_600_000_000); // 2026-11-12T00:00:00Z
    let (mut written, mut seq, mut burst) = (0u64, 0u64, 0u32);
    let mut buf = String::with_capacity(1 << 16);
    while written < size {
        buf.clear();
        while buf.len() < 60_000 && written + (buf.len() as u64) < size {
            time = Timestamp(time.0.saturating_add(i64::try_from(r.below(40_000)).unwrap_or(0)));
            let e = event(&mut r, time, &mut burst);
            write_line(&mut buf, f, &e, seq);
            seq += 1;
        }
        out.write_all(buf.as_bytes()).map_err(|e| e.to_string())?;
        written += buf.len() as u64;
    }
    Ok(written)
}

pub fn main(args: &[String]) -> Result<(), String> {
    let (mut size, mut fmt, mut seed, mut out) = (100u64 << 20, Fmt::Plain, 42u64, None::<PathBuf>);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().ok_or_else(|| format!("{a} needs a value"));
        match a.as_str() {
            "--size" => size = parse_size(&val()?)?,
            "--seed" => seed = val()?.parse().map_err(|_| "bad --seed".to_string())?,
            "--out" => out = Some(PathBuf::from(val()?)),
            "--format" => {
                fmt = match val()?.as_str() {
                    "jsonl" => Fmt::Jsonl,
                    "logfmt" => Fmt::Logfmt,
                    "plain" => Fmt::Plain,
                    "syslog" => Fmt::Syslog,
                    "access" => Fmt::Access,
                    other => return Err(format!("unknown format `{other}` (plain, jsonl, logfmt, syslog, access)")),
                }
            }
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    let ext = match fmt {
        Fmt::Jsonl => "jsonl",
        _ => "log",
    };
    let name = match fmt {
        Fmt::Jsonl => "jsonl",
        Fmt::Logfmt => "logfmt",
        Fmt::Plain => "plain",
        Fmt::Syslog => "syslog",
        Fmt::Access => "access",
    };
    let path = out.unwrap_or_else(|| super::root().join("benches").join("fixtures").join(format!("{name}-{size}-s{seed}.{ext}")));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let file = File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut w = BufWriter::new(file);
    let n = generate(fmt, size, seed, &mut w)?;
    w.flush().map_err(|e| e.to_string())?;
    println!("{} ({n} bytes)", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_bytes_and_whole_lines() {
        for f in [Fmt::Jsonl, Fmt::Logfmt, Fmt::Plain, Fmt::Syslog, Fmt::Access] {
            let (mut a, mut b) = (Vec::new(), Vec::new());
            generate(f, 200_000, 7, &mut a).unwrap();
            generate(f, 200_000, 7, &mut b).unwrap();
            assert_eq!(a, b);
            assert!(a.len() >= 200_000 && a.ends_with(b"\n"));
            let mut c = Vec::new();
            generate(f, 200_000, 8, &mut c).unwrap();
            assert_ne!(a, c);
        }
    }

    #[test]
    fn access_times_look_like_apache() {
        assert_eq!(apache_time("2026-11-12T07:41:03.118Z"), "12/Nov/2026:07:41:03 +0000");
    }

    #[test]
    fn sizes_parse() {
        assert_eq!(parse_size("100M"), Ok(100 << 20));
        assert_eq!(parse_size("2k"), Ok(2048));
        assert_eq!(parse_size("17"), Ok(17));
        assert!(parse_size("M").is_err() && parse_size("x").is_err());
    }
}
