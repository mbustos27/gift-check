//! UTC timestamps as RFC 3339 text ("2026-11-15T16:00:00Z"), without a date library.

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

pub fn now_utc() -> String {
    format_utc(now())
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (i64::from(m) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn format_utc(t: i64) -> String {
    let (y, m, d) = civil_from_days(t.div_euclid(86_400));
    let s = t.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", s / 3600, s % 3600 / 60, s % 60)
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        _ => 28,
    }
}

fn num(s: &str) -> Option<i64> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok()).flatten()
}

/// "2026-11-15T16:00:00Z", "2026-11-15T16:00:00.123Z" or with a "+01:00" offset → Unix seconds.
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20 || !s.is_ascii() || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let (y, mo, d) = (num(&s[0..4])?, num(&s[5..7])? as u32, num(&s[8..10])? as u32);
    let (h, mi, sec) = (num(&s[11..13])?, num(&s[14..16])?, num(&s[17..19])?);
    if !(1..=12).contains(&mo) || d == 0 || d > days_in_month(y, mo) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut rest = &s[19..];
    if let Some(frac) = rest.strip_prefix('.') {
        let n = frac.bytes().take_while(u8::is_ascii_digit).count();
        if n == 0 || n > 9 {
            return None;
        }
        rest = &frac[n..];
    }
    let offset = match rest {
        "Z" | "z" => 0,
        _ if rest.len() == 6 && rest.as_bytes()[3] == b':' => {
            let sign = match rest.as_bytes()[0] {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let (oh, om) = (num(&rest[1..3])?, num(&rest[4..6])?);
            if oh > 23 || om > 59 {
                return None;
            }
            sign * (oh * 3600 + om * 60)
        }
        _ => return None,
    };
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + sec - offset)
}

/// An IMF-fixdate as used by `Retry-After`: "Sun, 06 Nov 1994 08:49:37 GMT".
pub fn parse_http_date(s: &str) -> Option<i64> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() != 6 || parts[5] != "GMT" {
        return None;
    }
    let d = num(parts[1])? as u32;
    let months = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let m = months.iter().position(|x| x.eq_ignore_ascii_case(parts[2]))? as u32 + 1;
    let y = num(parts[3])?;
    let hms: Vec<&str> = parts[4].split(':').collect();
    if hms.len() != 3 || d == 0 || d > days_in_month(y, m) {
        return None;
    }
    let (h, mi, sec) = (num(hms[0])?, num(hms[1])?, num(hms[2])?);
    if h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    Some(days_from_civil(y, m, d) * 86_400 + h * 3600 + mi * 60 + sec)
}

/// The UTC time `hours` hours ago (negative: ahead), as RFC 3339 text. Tests use it so their
/// captures stay inside the accepted age window whatever day they run.
pub fn hours_ago_utc(hours: i64) -> String {
    format_utc(now() - hours * 3600)
}

/// "45 s", "12 min", "3 h", "2 d" for a number of seconds.
pub fn ago(secs: i64) -> String {
    let s = secs.max(0);
    match s {
        0..=59 => format!("{s} s"),
        60..=3599 => format!("{} min", s / 60),
        3600..=172_799 => format!("{} h", s / 3600),
        _ => format!("{} d", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_known_dates() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        let t = parse_rfc3339("2026-11-15T16:00:00Z").unwrap();
        assert_eq!(format_utc(t), "2026-11-15T16:00:00Z");
        assert_eq!(parse_rfc3339("2026-11-15T17:00:00.250+01:00"), Some(t));
        assert_eq!(parse_rfc3339("2024-02-29T00:00:00Z").map(format_utc).as_deref(), Some("2024-02-29T00:00:00Z"));
        for bad in ["2026-02-30T00:00:00Z", "2026-11-15 16:00:00", "2026-11-15T16:00:00", "2026-13-01T00:00:00Z", "yesterday"] {
            assert_eq!(parse_rfc3339(bad), None, "{bad}");
        }
    }

    #[test]
    fn http_dates() {
        assert_eq!(parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT").map(format_utc).as_deref(), Some("1994-11-06T08:49:37Z"));
        assert_eq!(parse_http_date("06 Nov 1994"), None);
    }
}
