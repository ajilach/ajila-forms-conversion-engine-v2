//! Shared utility functions used across multiple modules.

// ============================================================================
// Timestamps
// ============================================================================
//
// Core has no `chrono`/`time` dependency and must stay usable from wasm, so
// timestamps are formatted from the raw epoch seconds.

/// Current UTC time as `(seconds since the epoch, sub-second milliseconds)`.
pub fn unix_now() -> (u64, u32) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::time::SystemTime;
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
        (now.as_secs(), now.subsec_millis())
    }
    #[cfg(target_arch = "wasm32")]
    {
        let ms = js_sys::Date::now() as u64;
        (ms / 1000, (ms % 1000) as u32)
    }
}

/// Convert days since 1970-01-01 to `(year, month, day)`.
///
/// Algorithm from <http://howardhinnant.github.io/date_algorithms.html>.
pub fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Split `(secs, millis)` into `(year, month, day, hour, minute, second, milli)`.
fn split_utc(secs: u64, millis: u32) -> (u64, u64, u64, u64, u64, u64, u32) {
    let (year, month, day) = days_to_ymd(secs / 86400);
    let day_secs = secs % 86400;
    (
        year,
        month,
        day,
        day_secs / 3600,
        (day_secs % 3600) / 60,
        day_secs % 60,
        millis,
    )
}

/// Format an epoch instant as ISO 8601, e.g. `2026-02-16T12:00:00.000+00:00`.
pub fn iso_timestamp(secs: u64, millis: u32) -> String {
    let (y, mo, d, h, mi, s, ms) = split_utc(secs, millis);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{ms:03}+00:00")
}

/// Current UTC time as an ISO 8601 string.
pub fn iso_now() -> String {
    let (secs, millis) = unix_now();
    iso_timestamp(secs, millis)
}

/// Escape HTML special characters in a string.
///
/// Replaces `&`, `<`, `>`, `"`, and `'` with their HTML entity equivalents.
pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
