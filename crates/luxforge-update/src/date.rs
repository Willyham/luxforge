//! A release's publication time, from GitHub's RFC 3339 form to RSS's RFC 822 `pubDate`.

/// `2026-10-12T09:30:05Z` (fractional seconds allowed, UTC only) as
/// `Mon, 12 Oct 2026 09:30:05 +0000`. Anything else is `None`.
pub fn pub_date(rfc3339: &str) -> Option<String> {
    let text = rfc3339.strip_suffix('Z')?;
    let (date, time) = text.split_once('T')?;
    let time = match time.split_once('.') {
        Some((whole, fraction))
            if !fraction.is_empty() && fraction.bytes().all(|b| b.is_ascii_digit()) =>
        {
            whole
        }
        Some(_) => return None,
        None => time,
    };
    let [year, month, day] = fields::<3>(date, '-', &[4, 2, 2])?;
    let [hour, minute, second] = fields::<3>(time, ':', &[2, 2, 2])?;
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => return None,
    };
    if day == 0 || day > days_in_month || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let weekday = WEEKDAYS[days_from_epoch(year, month, day).rem_euclid(7) as usize];
    Some(format!(
        "{weekday}, {day:02} {} {year:04} {hour:02}:{minute:02}:{second:02} +0000",
        MONTHS[month as usize - 1]
    ))
}

/// `N` decimal fields of the given widths separated by `separator`.
fn fields<const N: usize>(text: &str, separator: char, widths: &[usize; N]) -> Option<[i64; N]> {
    let mut values = [0; N];
    let mut parts = text.split(separator);
    for (value, width) in values.iter_mut().zip(widths) {
        let part = parts.next()?;
        if part.len() != *width || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *value = part.parse().ok()?;
    }
    parts.next().is_none().then_some(values)
}

/// Days from 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's `days_from_civil`).
fn days_from_epoch(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_times_become_rss_dates() {
        for (input, expected) in [
            ("1970-01-01T00:00:00Z", "Thu, 01 Jan 1970 00:00:00 +0000"),
            ("2026-10-12T09:30:05Z", "Mon, 12 Oct 2026 09:30:05 +0000"),
            (
                "2024-02-29T23:59:59.123Z",
                "Thu, 29 Feb 2024 23:59:59 +0000",
            ),
            ("2000-03-01T12:00:00Z", "Wed, 01 Mar 2000 12:00:00 +0000"),
            ("1969-12-31T00:00:00Z", "Wed, 31 Dec 1969 00:00:00 +0000"),
        ] {
            assert_eq!(pub_date(input).as_deref(), Some(expected), "{input}");
        }
    }

    #[test]
    fn other_forms_are_refused() {
        for input in [
            "",
            "2026-10-12",
            "2026-10-12T09:30:05",
            "2026-10-12T09:30:05+01:00",
            "2026-10-12 09:30:05Z",
            "2026-13-12T09:30:05Z",
            "2023-02-29T09:30:05Z",
            "2026-10-12T24:00:00Z",
            "2026-10-12T09:30:05.Z",
            "26-10-12T09:30:05Z",
            "2026-10-12T09:30Z",
        ] {
            assert_eq!(pub_date(input), None, "{input}");
        }
    }
}
