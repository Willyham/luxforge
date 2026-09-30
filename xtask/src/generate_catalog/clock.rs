//! Calendar dates and camera clock times for the generator, in the proleptic Gregorian calendar,
//! with the text forms EXIF and the manifest write. Every camera in the plan keeps local time at
//! the place it shoots, so a capture's local time and its zone's offset give its UTC time.
use std::fmt;

pub const SECOND: i64 = 1000;
pub const MINUTE: i64 = 60 * SECOND;
pub const HOUR: i64 = 60 * MINUTE;
pub const DAY: i64 = 24 * HOUR;

/// A calendar date.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date {
    pub year: i32,
    pub month: u8,
    pub day: u8,
}

impl Date {
    pub const fn new(year: i32, month: u8, day: u8) -> Self {
        Date { year, month, day }
    }

    /// Days since 1970-01-01 (Howard Hinnant's `days_from_civil`).
    pub fn days(self) -> i64 {
        let year = i64::from(self.year) - i64::from(self.month <= 2);
        let era = year.div_euclid(400);
        let year_of_era = year - era * 400;
        let month_from_march = (i64::from(self.month) + 9) % 12;
        let day_of_year = (153 * month_from_march + 2) / 5 + i64::from(self.day) - 1;
        let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
        era * 146_097 + day_of_era - 719_468
    }

    /// The date `days` after 1970-01-01 (Hinnant's `civil_from_days`).
    pub fn from_days(days: i64) -> Self {
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let day_of_era = z - era * 146_097;
        let year_of_era =
            (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let month_from_march = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
        let month = if month_from_march < 10 {
            month_from_march + 3
        } else {
            month_from_march - 9
        };
        let year = year_of_era + era * 400 + i64::from(month <= 2);
        Date::new(year as i32, month as u8, day as u8)
    }

    pub fn add_days(self, days: i64) -> Self {
        Date::from_days(self.days() + days)
    }

    /// `hour:minute` on the date, on a camera's clock.
    pub fn at(self, hour: i64, minute: i64) -> LocalTime {
        LocalTime(self.days() * DAY + hour * HOUR + minute * MINUTE)
    }

    /// The month's English abbreviation, `Sep`.
    pub fn month_name(self) -> &'static str {
        const NAMES: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        NAMES[usize::from(self.month) - 1]
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// A time on a camera's clock: milliseconds since 1970-01-01T00:00 local time, with no zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LocalTime(pub i64);

impl LocalTime {
    pub fn date(self) -> Date {
        Date::from_days(self.0.div_euclid(DAY))
    }

    fn clock(self) -> (i64, i64, i64, i64) {
        let within = self.0.rem_euclid(DAY);
        (
            within / HOUR,
            within % HOUR / MINUTE,
            within % MINUTE / SECOND,
            within % SECOND,
        )
    }

    /// EXIF's `DateTimeOriginal` text, `YYYY:MM:DD HH:MM:SS`.
    pub fn exif(self) -> String {
        let date = self.date();
        let (hour, minute, second, _) = self.clock();
        format!(
            "{:04}:{:02}:{:02} {hour:02}:{minute:02}:{second:02}",
            date.year, date.month, date.day
        )
    }

    /// EXIF's `SubSecTimeOriginal` text: the milliseconds, three digits.
    pub fn subsec(self) -> String {
        format!("{:03}", self.clock().3)
    }

    /// `YYYY-MM-DDTHH:MM:SS.mmm`, with `offset` appended when the camera wrote one.
    pub fn iso(self, offset: Option<i16>) -> String {
        let (hour, minute, second, milli) = self.clock();
        let mut text = format!(
            "{}T{hour:02}:{minute:02}:{second:02}.{milli:03}",
            self.date()
        );
        if let Some(offset) = offset {
            text.push_str(&offset_text(offset));
        }
        text
    }

    /// Milliseconds since the Unix epoch in UTC, for a clock `offset` minutes ahead of UTC.
    pub fn utc(self, offset: i16) -> i64 {
        self.0 - i64::from(offset) * MINUTE
    }
}

impl std::ops::Add<i64> for LocalTime {
    type Output = LocalTime;
    fn add(self, milliseconds: i64) -> LocalTime {
        LocalTime(self.0 + milliseconds)
    }
}

/// EXIF's `OffsetTimeOriginal` text, `+02:00`.
pub fn offset_text(minutes: i16) -> String {
    let sign = if minutes < 0 { '-' } else { '+' };
    let minutes = minutes.unsigned_abs();
    format!("{sign}{:02}:{:02}", minutes / 60, minutes % 60)
}

/// A place's time zone. Summer time is approximated by month (April to October), which is all
/// generated data needs: the offset is a property of the date.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Zone {
    /// Minutes ahead of UTC outside summer time.
    pub standard: i16,
    /// Whether the clocks go forward an hour in summer.
    pub summer_time: bool,
}

const fn zone(standard: i16, summer_time: bool) -> Zone {
    Zone {
        standard,
        summer_time,
    }
}

/// Central European time: +01:00, +02:00 in summer.
pub const CENTRAL: Zone = zone(60, true);
/// Western European time: +00:00, +01:00 in summer.
pub const WESTERN: Zone = zone(0, true);
/// Eastern European time: +02:00, +03:00 in summer.
pub const EASTERN: Zone = zone(120, true);
/// Iceland: +00:00 all year.
pub const ICELAND: Zone = zone(0, false);
/// US Eastern time: −05:00, −04:00 in summer.
pub const US_EASTERN: Zone = zone(-300, true);
/// Japan: +09:00 all year.
pub const JAPAN: Zone = zone(540, false);
/// South Africa: +02:00 all year.
pub const SOUTH_AFRICA: Zone = zone(120, false);

impl Zone {
    /// Minutes ahead of UTC on `date`.
    pub fn offset(self, date: Date) -> i16 {
        let summer = self.summer_time && (4..=10).contains(&date.month);
        self.standard + 60 * i16::from(summer)
    }
}
