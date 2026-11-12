//! Numbers, sizes and times as the window shows them.

use strata_core::Timestamp;

/// `1284113` → `1,284,113`.
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `2_040_109_465` → `1.9 GB`; `412 MB`, `64 KB`, `12 B`.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit + 1 < UNITS.len() {
        v /= 1024.0;
        unit += 1;
    }
    let u = UNITS.get(unit).copied().unwrap_or("B");
    if unit == 0 || v >= 10.0 { format!("{v:.0} {u}") } else { format!("{v:.1} {u}") }
}

/// A count for a chip: exact up to 999,999, then `1.21M`.
pub fn short_count(n: u64) -> String {
    if n < 1_000_000 {
        thousands(n)
    } else if n < 1_000_000_000 {
        format!("{:.2}M", n as f64 / 1e6)
    } else {
        format!("{:.2}B", n as f64 / 1e9)
    }
}

/// `14:02:17.481` (UTC).
pub fn time_of_day(t: Timestamp) -> String {
    let full = t.to_rfc3339();
    full.get(11..23).map(str::to_string).unwrap_or(full)
}

/// `14:02` (UTC), for the histogram axis.
pub fn hour_minute(t: Timestamp) -> String {
    let full = t.to_rfc3339();
    full.get(11..16).map(str::to_string).unwrap_or(full)
}

/// `14:02:17` (UTC).
pub fn hms(t: Timestamp) -> String {
    let full = t.to_rfc3339();
    full.get(11..19).map(str::to_string).unwrap_or(full)
}

/// The path with the home folder written `~`, and without Windows' `\\?\` prefix (which
/// `canonicalize` adds and people never write).
pub fn tilde(path: &std::path::Path) -> String {
    let shown = path.display().to_string();
    let shown = match shown.strip_prefix(r"\\?\") {
        Some(rest) => rest.strip_prefix(r"UNC\").map_or_else(|| rest.to_string(), |unc| format!(r"\\{unc}")),
        None => shown,
    };
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        let home = home.to_string_lossy().to_string();
        if !home.is_empty()
            && let Some(rest) = shown.strip_prefix(&home)
        {
            return format!("~{rest}");
        }
    }
    shown
}

/// A shortcut written the Mac way (`⌘⇧O`) as this platform writes it (`Ctrl+Shift+O`).
pub fn keys(mac: &str) -> String {
    if cfg!(target_os = "macos") {
        return mac.to_string();
    }
    let mut out = String::new();
    for c in mac.chars() {
        match c {
            '⌘' => out.push_str("Ctrl+"),
            '⇧' => out.push_str("Shift+"),
            '⌥' => out.push_str("Alt+"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilde_drops_the_verbatim_prefix() {
        let p = |s: &str| tilde(std::path::Path::new(s));
        assert_eq!(p(r"\\?\Z:\logs\app.log"), r"Z:\logs\app.log");
        assert_eq!(p(r"\\?\UNC\server\share\app.log"), r"\\server\share\app.log");
        assert_eq!(p("/var/log/app.log"), "/var/log/app.log");
    }

    #[test]
    fn numbers_read_well() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(1_284_113), "1,284,113");
        assert_eq!(short_count(64_210), "64,210");
        assert_eq!(short_count(1_210_000), "1.21M");
        assert_eq!(bytes(12), "12 B");
        assert_eq!(bytes(65_624), "64 KB");
        assert_eq!(bytes(2_040_109_465), "1.9 GB");
        assert_eq!(bytes(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn times_are_cut_from_rfc3339() {
        let t = Timestamp(1_794_355_337_481_000);
        assert_eq!(time_of_day(t), t.to_rfc3339().get(11..23).unwrap_or_default());
        assert_eq!(hour_minute(Timestamp(0)), "00:00");
        assert_eq!(hms(Timestamp(0)), "00:00:00");
    }
}
