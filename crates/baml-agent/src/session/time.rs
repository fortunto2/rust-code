//! Time utilities: ISO timestamps, UUID v7 extraction, UTF-8 safe truncation.

/// ISO 8601 timestamp from system clock (no chrono dependency).
pub(crate) fn now_iso() -> String {
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let h = time_of_day / 3600;
    let m = (time_of_day % 3600) / 60;
    let s = time_of_day % 60;
    let ms = dur.subsec_millis();
    let (y, mo, d) = days_to_ymd(days);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", y, mo, d, h, m, s, ms)
}

fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    let mut y = 1970;
    loop {
        let year_days = if is_leap(y) { 366 } else { 365 };
        if days < year_days { break; }
        days -= year_days;
        y += 1;
    }
    let leap = is_leap(y);
    let months = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut mo = 1;
    for &ml in &months {
        if days < ml { break; }
        days -= ml;
        mo += 1;
    }
    (y, mo, days + 1)
}

fn is_leap(y: u64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

/// Extract unix timestamp (seconds) from a UUID v7 string.
pub(crate) fn uuid_v7_timestamp(uuid_str: &str) -> Option<u64> {
    let uuid = uuid::Uuid::parse_str(uuid_str).ok()?;
    let (secs, _nanos) = uuid.get_timestamp()?.to_unix();
    Some(secs)
}

/// UTF-8 safe string truncation (never panics on multibyte chars).
pub(crate) fn truncate_str(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

/// Truncate topic for display, respecting UTF-8 boundaries.
pub(crate) fn truncate_topic(s: &str) -> String {
    if s.len() <= 120 {
        s.to_string()
    } else {
        let truncated = truncate_str(s, 117);
        format!("{}...", truncated)
    }
}
