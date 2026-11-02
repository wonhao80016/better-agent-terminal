use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the epoch for `YYYY-MM-DDTHH:MM:SS[.fff][Z|±HH:MM]`; a missing offset is read as UTC.
pub(crate) fn parse_iso_millis(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    let digits = |start: usize, len: usize| -> Option<i64> {
        let slice = bytes.get(start..start + len)?;
        slice.iter().try_fold(0i64, |acc, &b| {
            b.is_ascii_digit().then(|| acc * 10 + i64::from(b - b'0'))
        })
    };
    let year = digits(0, 4)?;
    let month = digits(5, 2)?;
    let day = digits(8, 2)?;
    if bytes.get(4) != Some(&b'-') || bytes.get(7) != Some(&b'-') {
        return None;
    }
    let (mut hour, mut minute, mut second, mut millis) = (0, 0, 0, 0);
    let mut index = 10;
    if matches!(bytes.get(10), Some(b'T' | b't' | b' ')) {
        hour = digits(11, 2)?;
        minute = digits(14, 2)?;
        if bytes.get(13) != Some(&b':') {
            return None;
        }
        index = 16;
        if bytes.get(16) == Some(&b':') {
            second = digits(17, 2)?;
            index = 19;
            if bytes.get(19) == Some(&b'.') {
                index = 20;
                let mut scale = 100;
                while let Some(b) = bytes.get(index).filter(|b| b.is_ascii_digit()) {
                    millis += i64::from(b - b'0') * scale;
                    scale /= 10;
                    index += 1;
                }
            }
        }
    }
    let offset_minutes = match bytes.get(index) {
        None | Some(b'Z' | b'z') => 0,
        Some(sign @ (b'+' | b'-')) => {
            let hours = digits(index + 1, 2)?;
            let minutes_at = if bytes.get(index + 3) == Some(&b':') {
                index + 4
            } else {
                index + 3
            };
            let minutes = digits(minutes_at, 2).unwrap_or(0);
            let total = hours * 60 + minutes;
            if *sign == b'+' {
                total
            } else {
                -total
            }
        }
        Some(_) => return None,
    };
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some((((days * 24 + hour) * 60 + minute - offset_minutes) * 60 + second) * 1000 + millis)
}

/// Unparseable or absent timestamps sort as the oldest.
pub(crate) fn time_value(text: Option<&str>) -> i64 {
    text.and_then(parse_iso_millis).unwrap_or(0)
}

pub(crate) fn now_iso_utc() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    format_iso_utc(millis)
}

pub(crate) fn format_iso_utc(millis: i64) -> String {
    let days = millis.div_euclid(86_400_000);
    let rest = millis.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3_600_000,
        rest / 60_000 % 60,
        rest / 1000 % 60,
        rest % 1000
    )
}

// Howard Hinnant's days_from_civil / civil_from_days for the proleptic Gregorian calendar.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_forms() {
        assert_eq!(parse_iso_millis("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso_millis("1970-01-01T00:00:01.5Z"), Some(1500));
        assert_eq!(
            parse_iso_millis("2026-10-03T10:00:00.000Z"),
            Some(1_791_021_600_000)
        );
        assert_eq!(
            parse_iso_millis("2026-10-03T18:00:00+08:00"),
            parse_iso_millis("2026-10-03T10:00:00Z")
        );
        assert_eq!(
            parse_iso_millis("2026-10-03T10:00:00.123456Z"),
            Some(1_791_021_600_123)
        );
        assert_eq!(parse_iso_millis("not a date"), None);
        assert_eq!(time_value(None), 0);
    }

    #[test]
    fn formats_round_trip() {
        assert_eq!(format_iso_utc(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            format_iso_utc(1_791_021_600_123),
            "2026-10-03T10:00:00.123Z"
        );
        assert_eq!(format_iso_utc(951_782_400_000), "2000-02-29T00:00:00.000Z");
    }
}
