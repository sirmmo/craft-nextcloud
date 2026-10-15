//! Dates without a date library: civil dates from Unix time, the local offset, HTTP dates.

use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch, now.
pub fn now_ms() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
        Err(_) => 0,
    }
}

/// A date and time of day.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Civil {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

/// Days since 1970-01-01 of a civil date (proleptic Gregorian).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(m);
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date of a day count since 1970-01-01.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, u32::try_from(m).unwrap_or(1), u32::try_from(d).unwrap_or(1))
}

/// The civil date and time of `secs` seconds since the epoch, in UTC.
pub fn civil(secs: i64) -> Civil {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = u32::try_from(rem / 3600).unwrap_or(0);
    let minute = u32::try_from(rem % 3600 / 60).unwrap_or(0);
    let second = u32::try_from(rem % 60).unwrap_or(0);
    Civil { year, month, day, hour, minute, second }
}

/// The computer's offset from UTC in seconds (asked once; 0 when it can't be found).
pub fn local_offset() -> i64 {
    static OFFSET: OnceLock<i64> = OnceLock::new();
    *OFFSET.get_or_init(|| {
        if let Some(v) = std::env::var("CRAFT_NEXTCLOUD_UTC_OFFSET").ok().and_then(|s| s.trim().parse::<i64>().ok()) {
            return v.clamp(-14 * 3600, 14 * 3600);
        }
        if cfg!(windows) {
            return 0;
        }
        crate::system::run_quiet("date", &["+%z"]).and_then(|s| parse_offset(s.trim())).unwrap_or(0)
    })
}

/// `+0200` → 7200.
fn parse_offset(s: &str) -> Option<i64> {
    let (sign, digits) = match s.as_bytes().first()? {
        b'+' => (1, s.get(1..)?),
        b'-' => (-1, s.get(1..)?),
        _ => return None,
    };
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let h: i64 = digits.get(..2)?.parse().ok()?;
    let m: i64 = digits.get(2..)?.parse().ok()?;
    Some(sign * (h * 3600 + m * 60))
}

/// Local civil time of a millisecond timestamp.
pub fn local(ms: i64) -> Civil {
    civil(ms.div_euclid(1000).saturating_add(local_offset()))
}

/// The local day number of a timestamp (for "one per day" retention).
pub fn local_day(ms: i64) -> i64 {
    ms.div_euclid(1000).saturating_add(local_offset()).div_euclid(86_400)
}

/// `2026-10-15 14:03`.
pub fn display(ms: i64) -> String {
    let c = local(ms);
    format!("{:04}-{:02}-{:02} {:02}:{:02}", c.year, c.month, c.day, c.hour, c.minute)
}

/// `2026-10-15 14:03:22`.
pub fn display_seconds(ms: i64) -> String {
    let c = local(ms);
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", c.year, c.month, c.day, c.hour, c.minute, c.second)
}

/// `2026-10-15 14.03` (for file names: no colons).
pub fn file_display(ms: i64) -> String {
    let c = local(ms);
    format!("{:04}-{:02}-{:02} {:02}.{:02}", c.year, c.month, c.day, c.hour, c.minute)
}

/// `20261015-140322-123` (UTC; sorts by time).
pub fn stamp(ms: i64) -> String {
    let c = civil(ms.div_euclid(1000));
    format!("{:04}{:02}{:02}-{:02}{:02}{:02}-{:03}", c.year, c.month, c.day, c.hour, c.minute, c.second, ms.rem_euclid(1000))
}

/// Seconds since the epoch of an HTTP date (`Wed, 14 Oct 2026 18:02:00 GMT`, RFC 7231's
/// preferred form).
pub fn parse_http_date(s: &str) -> Option<i64> {
    let s = s.trim();
    let rest = s.split_once(',').map(|(_, r)| r).unwrap_or(s);
    let mut parts = rest.split_whitespace();
    let day: u32 = parts.next()?.parse().ok()?;
    let month = match parts.next()?.to_ascii_lowercase().as_str() {
        "jan" => 1,
        "feb" => 2,
        "mar" => 3,
        "apr" => 4,
        "may" => 5,
        "jun" => 6,
        "jul" => 7,
        "aug" => 8,
        "sep" => 9,
        "oct" => 10,
        "nov" => 11,
        "dec" => 12,
        _ => return None,
    };
    let year: i64 = parts.next()?.parse().ok()?;
    let mut hms = parts.next()?.split(':');
    let h: i64 = hms.next()?.parse().ok()?;
    let m: i64 = hms.next()?.parse().ok()?;
    let sec: i64 = hms.next()?.parse().ok()?;
    if !(1..=31).contains(&day) || !(0..24).contains(&h) || !(0..60).contains(&m) || !(0..=60).contains(&sec) || !(1..=9999).contains(&year) {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + h * 3600 + m * 60 + sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trips() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        for d in [-1000, -1, 0, 1, 59, 60, 11_016, 20_000, 30_000] {
            let (y, m, dd) = civil_from_days(d);
            assert_eq!(days_from_civil(y, m, dd), d);
        }
        let c = civil(1_792_000_000);
        assert_eq!((c.year, c.month, c.day), (2026, 10, 14));
    }

    #[test]
    fn http_dates() {
        let s = parse_http_date("Wed, 14 Oct 2026 18:02:05 GMT").unwrap_or(0);
        let c = civil(s);
        assert_eq!((c.year, c.month, c.day, c.hour, c.minute, c.second), (2026, 10, 14, 18, 2, 5));
        assert_eq!(parse_http_date("garbage"), None);
        assert_eq!(parse_http_date("Wed, 99 Oct 2026 18:02:05 GMT"), None);
    }

    #[test]
    fn offsets() {
        assert_eq!(parse_offset("+0200"), Some(7200));
        assert_eq!(parse_offset("-0530"), Some(-19_800));
        assert_eq!(parse_offset("0200"), None);
        assert_eq!(stamp(1_000_123), "19700101-001640-123");
    }
}
