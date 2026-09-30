//! What a file's header says: capture time, place, camera body, lens, exposure, size, orientation and
//! where its embedded thumbnail sits.
//!
//! The index lane (lane A) fills a [`HeaderMetadata`] from a read bounded in bytes, never the image
//! data; organizing reads it for events and moments, developing a pick (lane C) stores it in the
//! catalog's `capture` table, and the Info panel (lane D) shows it. Every field is optional: a file
//! records what its camera wrote and nothing is guessed.
use crate::Error;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::fmt;

const DAY_MS: i64 = 86_400_000;

/// A calendar day on the camera's own clock: days since 1970-01-01. Serialized as `YYYY-MM-DD`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LocalDay(pub i32);

impl LocalDay {
    /// The day of a civil date, or none outside years 1 to 9999 or for a date that does not exist.
    pub fn from_ymd(year: i32, month: u32, day: u32) -> Option<Self> {
        if !(1..=9999).contains(&year) || !(1..=12).contains(&month) || day == 0 {
            return None;
        }
        if day > days_in_month(year, month) {
            return None;
        }
        Some(Self(days_from_civil(year, month, day)))
    }

    /// Year, month (1–12) and day (1–31).
    pub fn ymd(self) -> (i32, u32, u32) {
        civil_from_days(self.0)
    }

    /// The month this day is in.
    pub fn month(self) -> Month {
        let (year, month, _) = self.ymd();
        Month { year, month }
    }

    fn parse(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
            return None;
        }
        Self::from_ymd(
            text[0..4].parse().ok()?,
            text[5..7].parse().ok()?,
            text[8..10].parse().ok()?,
        )
    }
}

impl fmt::Display for LocalDay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (year, month, day) = self.ymd();
        write!(f, "{year:04}-{month:02}-{day:02}")
    }
}

impl Serialize for LocalDay {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for LocalDay {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text)
            .ok_or_else(|| de::Error::custom(format!("{text} is not a YYYY-MM-DD date")))
    }
}

/// A calendar month, for listing events by month. Serialized as `YYYY-MM`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Month {
    pub year: i32,
    /// 1 to 12.
    pub month: u32,
}

impl fmt::Display for Month {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}", self.year, self.month)
    }
}

impl Serialize for Month {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Month {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        LocalDay::parse(&format!("{text}-01"))
            .map(LocalDay::month)
            .ok_or_else(|| de::Error::custom(format!("{text} is not a YYYY-MM month")))
    }
}

/// When a frame was taken, as its camera recorded it: the camera's wall clock to the millisecond,
/// the offset from UTC when the camera wrote one (EXIF `OffsetTimeOriginal`), and the text itself.
///
/// Camera clocks drift, lack a time zone or were never set. Moments are per body and events use a
/// 3-hour gap, so neither needs more than [`Self::instant_ms`]: exact with an offset, and the local
/// wall time read as if it were UTC without one.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureTime {
    /// The camera's wall-clock time in milliseconds since 1970-01-01T00:00:00 on that clock
    /// (`DateTimeOriginal` and `SubSecTimeOriginal`), whatever its time zone.
    pub local_ms: i64,
    /// Minutes east of UTC, when the camera recorded them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_minutes: Option<i16>,
    /// What the camera wrote, in ISO 8601 form: `YYYY-MM-DDTHH:MM:SS`, the subsecond digits as
    /// written after a `.`, and the offset as written (`+02:00`). The catalog stores it as
    /// `capture.local_text` and search matches it.
    pub text: String,
}

impl CaptureTime {
    /// The capture time EXIF records, or none when `datetime` is absent, blank, zero or malformed:
    /// `datetime` is `DateTimeOriginal` (`YYYY:MM:DD HH:MM:SS`; hyphens are accepted in the date),
    /// `subsec` is `SubSecTimeOriginal` (its digits, of which the first three are milliseconds) and
    /// `offset` is `OffsetTimeOriginal` (`±HH:MM`). A malformed subsecond or offset is dropped and
    /// the rest kept.
    pub fn from_exif(datetime: &str, subsec: Option<&str>, offset: Option<&str>) -> Option<Self> {
        let datetime = datetime.trim_matches(|c: char| c == '\0' || c.is_whitespace());
        let bytes = datetime.as_bytes();
        if bytes.len() != 19
            || !matches!(bytes[4], b':' | b'-')
            || bytes[7] != bytes[4]
            || bytes[10] != b' '
            || bytes[13] != b':'
            || bytes[16] != b':'
        {
            return None;
        }
        let number = |range: std::ops::Range<usize>| -> Option<u32> {
            let text = &datetime[range];
            text.bytes()
                .all(|b| b.is_ascii_digit())
                .then(|| text.parse().ok())?
        };
        let day = LocalDay::from_ymd(number(0..4)? as i32, number(5..7)?, number(8..10)?)?;
        let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
        if hour > 23 || minute > 59 || second > 60 {
            return None;
        }
        let subsec = subsec
            .map(|text| text.trim_matches(|c: char| c == '\0' || c.is_whitespace()))
            .filter(|text| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()));
        let millis = subsec.map_or(0, |digits| {
            let first: String = digits.chars().chain("000".chars()).take(3).collect();
            first.parse::<i64>().unwrap_or(0)
        });
        let offset_text = offset
            .map(|text| text.trim_matches(|c: char| c == '\0' || c.is_whitespace()))
            .filter(|text| parse_offset(text).is_some());
        let local_ms = i64::from(day.0) * DAY_MS
            + (i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second)) * 1000
            + millis;
        let mut text = format!(
            "{day}T{hour:02}:{minute:02}:{second:02}",
            day = day,
            hour = hour,
            minute = minute,
            second = second
        );
        if let Some(digits) = subsec {
            text.push('.');
            text.push_str(digits);
        }
        if let Some(offset) = offset_text {
            text.push_str(offset);
        }
        Some(Self {
            local_ms,
            offset_minutes: offset_text.and_then(parse_offset),
            text,
        })
    }

    /// A sortable instant in milliseconds since the Unix epoch: exact UTC when the offset is known,
    /// and the camera's wall time read as UTC when it is not ([`Self::is_exact`]). Events, moments
    /// and the capture-time sort all order by this.
    pub fn instant_ms(&self) -> i64 {
        match self.offset_minutes {
            Some(offset) => self.local_ms - i64::from(offset) * 60_000,
            None => self.local_ms,
        }
    }

    /// Whether [`Self::instant_ms`] is a true UTC instant.
    pub fn is_exact(&self) -> bool {
        self.offset_minutes.is_some()
    }

    /// The camera-local calendar day: what a view groups days by, whatever the offset.
    pub fn local_day(&self) -> LocalDay {
        LocalDay(self.local_ms.div_euclid(DAY_MS) as i32)
    }
}

/// Minutes east of UTC of an EXIF offset, `+HH:MM` or `-HH:MM`.
fn parse_offset(text: &str) -> Option<i16> {
    let bytes = text.as_bytes();
    if bytes.len() != 6 || !matches!(bytes[0], b'+' | b'-') || bytes[3] != b':' {
        return None;
    }
    let hours: i16 = text[1..3].parse().ok()?;
    let minutes: i16 = text[4..6].parse().ok()?;
    if hours > 14 || minutes > 59 {
        return None;
    }
    let total = hours * 60 + minutes;
    Some(if bytes[0] == b'-' { -total } else { total })
}

/// Days since 1970-01-01 of a proleptic Gregorian date (H. Hinnant's `days_from_civil`).
fn days_from_civil(year: i32, month: u32, day: u32) -> i32 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era =
        year_of_era as u32 * 365 + year_of_era as u32 / 4 - year_of_era as u32 / 100 + day_of_year;
    era * 146_097 + day_of_era as i32 - 719_468
}

/// The date of a day number (H. Hinnant's `civil_from_days`).
fn civil_from_days(days: i32) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = (z - era * 146_097) as u32;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era as i32 + era * 400 + i32::from(month <= 2);
    (year, month, day)
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Where a frame was taken (EXIF GPS): degrees north and east, and metres above sea level when
/// recorded. Kept on the Mac: places are how the catalog is searched, and export's metadata rules
/// decide separately what a file carries.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeoPosition {
    pub lat: f64,
    pub lon: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt_m: Option<f64>,
}

impl GeoPosition {
    /// A position inside the valid ranges (latitude −90..=90, longitude −180..=180, all finite), or
    /// none: a GPS block of zeros or garbage is not a place.
    pub fn new(lat: f64, lon: f64, alt_m: Option<f64>) -> Option<Self> {
        ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)).then_some(Self {
            lat,
            lon,
            alt_m: alt_m.filter(|alt| alt.is_finite()),
        })
    }

    /// The great-circle distance to `other` in kilometres (haversine on the mean Earth radius),
    /// which the event rule's 25 km jump reads.
    pub fn distance_km(&self, other: &Self) -> f64 {
        const EARTH_KM: f64 = 6_371.008_8;
        let (lat1, lat2) = (self.lat.to_radians(), other.lat.to_radians());
        let dlat = lat2 - lat1;
        let dlon = (other.lon - self.lon).to_radians();
        let a = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
        2.0 * EARTH_KM * a.sqrt().min(1.0).asin()
    }
}

/// One camera body: make, model and body serial (EXIF `Make`, `Model`, `BodySerialNumber`), each
/// trimmed of spaces and NULs. Two bodies of one model are two bodies, which is why moments and the
/// Camera grouping key on the serial too.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CameraBody {
    pub make: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
}

impl CameraBody {
    /// The body's stable key: `make|model|serial`, with nothing after the last `|` when the serial
    /// is not recorded.
    pub fn key(&self) -> BodyKey {
        BodyKey(format!(
            "{}|{}|{}",
            self.make,
            self.model,
            self.serial.as_deref().unwrap_or("")
        ))
    }

    /// The body as a person reads it: the model, led by the make when the model does not already
    /// name it (`NIKON Z 8`, `FUJIFILM X100VI`, `Apple iPhone 15 Pro`).
    pub fn label(&self) -> String {
        let brand = self.make.split_whitespace().next().unwrap_or("");
        if brand.is_empty() || self.model.to_lowercase().contains(&brand.to_lowercase()) {
            self.model.clone()
        } else {
            format!("{brand} {}", self.model)
        }
    }
}

/// A camera body's stable key ([`CameraBody::key`]); the empty key is a frame with no camera
/// recorded, which forms a body of its own.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BodyKey(pub String);

impl BodyKey {
    /// The key of a frame's camera, the empty key when none is recorded.
    pub fn of(camera: Option<&CameraBody>) -> Self {
        camera.map(CameraBody::key).unwrap_or_default()
    }
}

/// A frame's exposure as recorded: shutter time in seconds, f-number, ISO speed, exposure bias in
/// EV, and focal length, as recorded and as its 35 mm equivalent. Each is optional.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Exposure {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_s: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub f_number: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iso: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bias_ev: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focal_mm: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focal_35mm_mm: Option<f32>,
}

impl Exposure {
    /// The exposure value the settings give, at ISO 100: `log2(N² / t) − log2(ISO / 100)`, when the
    /// time, f-number and ISO are all recorded and positive. A frame one stop brighter has an `ev`
    /// one lower.
    ///
    /// The bias is not added: a camera applies its exposure compensation through these same
    /// settings, so adding it would count a bracket's step twice. Bracket detection reads the
    /// settings first and falls back to the recorded bias only when the settings are unknown or
    /// identical across a run ([`crate::organize::metadata_steps`]).
    pub fn ev(&self) -> Option<f32> {
        let (time, f_number, iso) = (self.time_s?, self.f_number?, self.iso?);
        (time > 0.0 && f_number > 0.0 && iso > 0)
            .then(|| (f_number * f_number / time).log2() - (iso as f32 / 100.0).log2())
    }

    /// Whether two frames share the aperture, ISO and focal length, which lets them form one run up
    /// to the longer matched gap (P5), since bracketing at slower shutter speeds spaces frames
    /// further apart. An unrecorded value matches nothing.
    pub fn settings_match(&self, other: &Self) -> bool {
        fn same<T: PartialEq>(a: Option<T>, b: Option<T>) -> bool {
            matches!((a, b), (Some(a), Some(b)) if a == b)
        }
        same(self.f_number, other.f_number)
            && same(self.iso, other.iso)
            && same(self.focal_mm, other.focal_mm)
    }
}

/// An image's stored size in pixels, before orientation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dimensions {
    pub width: u32,
    pub height: u32,
}

/// An EXIF orientation, 1 to 8: how the stored pixels turn upright.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct ExifOrientation(u8);

impl ExifOrientation {
    /// Upright as stored.
    pub const UPRIGHT: Self = Self(1);

    pub fn new(value: u8) -> Option<Self> {
        (1..=8).contains(&value).then_some(Self(value))
    }

    pub fn get(self) -> u8 {
        self.0
    }

    /// Whether the orientation swaps width and height (5 to 8).
    pub fn transposes(self) -> bool {
        self.0 >= 5
    }
}

impl TryFrom<u8> for ExifOrientation {
    type Error = String;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value).ok_or_else(|| format!("orientation {value} is not 1 to 8"))
    }
}

impl From<ExifOrientation> for u8 {
    fn from(orientation: ExifOrientation) -> Self {
        orientation.0
    }
}

/// How an embedded image's bytes are encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EmbeddedFormat {
    /// A JPEG stream (EXIF IFD1's thumbnail, most RAW previews).
    Jpeg,
    /// An uncompressed strip of 8-bit RGB, three bytes a pixel, row by row, as NEF IFD0 and many
    /// DNGs carry their thumbnail. Its size must be known to read it.
    Rgb8,
}

impl EmbeddedFormat {
    /// The format as the index stores it (`files.thumb_format`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Jpeg => "jpeg",
            Self::Rgb8 => "rgb8",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        [Self::Jpeg, Self::Rgb8]
            .into_iter()
            .find(|format| format.as_str() == value)
    }
}

/// Where an embedded image sits in its file: `len` bytes from byte `offset`, in `format`, with its
/// size when the header records it. The preview lane reads exactly those bytes; nothing else in the
/// file is read to draw a grid cell. An [`EmbeddedFormat::Rgb8`] image always has its size, and its
/// length is exactly three bytes a pixel ([`Self::new`] refuses anything else).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "EmbeddedFields", into = "EmbeddedFields")]
pub struct EmbeddedImage {
    offset: u64,
    len: u32,
    format: EmbeddedFormat,
    width: Option<u32>,
    height: Option<u32>,
}

/// [`EmbeddedImage`]'s fields as they are serialized, checked by [`EmbeddedImage::new`] on the
/// way in.
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedFields {
    offset: u64,
    len: u32,
    format: EmbeddedFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    height: Option<u32>,
}

impl EmbeddedImage {
    /// An embedded image, refused when it is empty, when a size is half given or zero, or when an
    /// RGB strip lacks its size or its length is not three bytes a pixel.
    pub fn new(
        offset: u64,
        len: u32,
        format: EmbeddedFormat,
        width: Option<u32>,
        height: Option<u32>,
    ) -> Result<Self, Error> {
        if len == 0 {
            return Err(Error::validation("an embedded image has no bytes"));
        }
        match (width, height) {
            (Some(0), _) | (_, Some(0)) => {
                return Err(Error::validation("an embedded image has no pixels"));
            }
            (Some(_), None) | (None, Some(_)) => {
                return Err(Error::validation(
                    "an embedded image's size needs both width and height",
                ));
            }
            _ => {}
        }
        if format == EmbeddedFormat::Rgb8 {
            let (Some(width), Some(height)) = (width, height) else {
                return Err(Error::validation("an RGB strip needs its width and height"));
            };
            if u64::from(width) * u64::from(height) * 3 != u64::from(len) {
                return Err(Error::validation(
                    "an RGB strip's length is not three bytes a pixel",
                ));
            }
        }
        Ok(Self {
            offset,
            len,
            format,
            width,
            height,
        })
    }

    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn len(&self) -> u32 {
        self.len
    }

    /// Never true: [`Self::new`] refuses an empty image.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn format(&self) -> EmbeddedFormat {
        self.format
    }

    /// Width and height, when recorded (always for an RGB strip).
    pub fn size(&self) -> Option<Dimensions> {
        Some(Dimensions {
            width: self.width?,
            height: self.height?,
        })
    }
}

impl TryFrom<EmbeddedFields> for EmbeddedImage {
    type Error = String;

    fn try_from(fields: EmbeddedFields) -> Result<Self, Self::Error> {
        Self::new(
            fields.offset,
            fields.len,
            fields.format,
            fields.width,
            fields.height,
        )
        .map_err(|error| error.detail)
    }
}

impl From<EmbeddedImage> for EmbeddedFields {
    fn from(image: EmbeddedImage) -> Self {
        Self {
            offset: image.offset,
            len: image.len,
            format: image.format,
            width: image.width,
            height: image.height,
        }
    }
}

/// Everything organizing, the Info panel and the catalog read from one file's header. Lane A
/// fills it from a bounded header read (`index/`); a field its camera did not write is `None`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HeaderMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture: Option<CaptureTime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<GeoPosition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub camera: Option<CameraBody>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lens: Option<String>,
    pub exposure: Exposure,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<Dimensions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orientation: Option<ExifOrientation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<EmbeddedImage>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_capture_time_reads_subseconds_and_the_offset() {
        let time = CaptureTime::from_exif("2026:09:12 10:15:02", Some("13"), Some("+02:00"))
            .expect("a dated frame");
        assert_eq!(time.text, "2026-09-12T10:15:02.13+02:00");
        assert_eq!(time.offset_minutes, Some(120));
        assert!(time.is_exact());
        assert_eq!(time.local_day().to_string(), "2026-09-12");
        assert_eq!(time.local_ms % 1000, 130, "13 is 130 ms");
        assert_eq!(time.instant_ms(), time.local_ms - 120 * 60_000);
        // 2026-09-12T08:15:02.130Z.
        assert_eq!(time.instant_ms(), 1_789_200_902_130);
        let local = CaptureTime::from_exif("2026:09:12 10:15:02", None, None).unwrap();
        assert!(!local.is_exact());
        assert_eq!(local.instant_ms(), local.local_ms, "local read as UTC");
        assert_eq!(local.text, "2026-09-12T10:15:02");
    }

    #[test]
    fn missing_malformed_and_zero_times_are_undated() {
        for text in [
            "",
            "0000:00:00 00:00:00",
            "    :  :     :  :  ",
            "2026:13:01 00:00:00",
            "2026:02:30 00:00:00",
            "2026:09:12T10:15:02",
            "2026:09:12 24:00:00",
        ] {
            assert_eq!(CaptureTime::from_exif(text, None, None), None, "{text:?}");
        }
        let dropped = CaptureTime::from_exif("2026-09-12 10:15:02\0", Some("x"), Some("CEST"))
            .expect("the date and time stand without their bad parts");
        assert_eq!(dropped.text, "2026-09-12T10:15:02");
        assert_eq!(dropped.offset_minutes, None);
        let west =
            CaptureTime::from_exif("2026:09:12 01:00:00", Some("1234"), Some("-05:30")).unwrap();
        assert_eq!(west.offset_minutes, Some(-330));
        assert_eq!(west.local_ms % 1000, 123);
        assert_eq!(west.text, "2026-09-12T01:00:00.1234-05:30");
    }

    #[test]
    fn days_and_months_round_trip_as_text() {
        for (year, month, day) in [(1970, 1, 1), (2000, 2, 29), (2026, 9, 12), (1, 1, 1)] {
            let local = LocalDay::from_ymd(year, month, day).unwrap();
            assert_eq!(local.ymd(), (year, month, day));
            let json = serde_json::to_value(local).unwrap();
            assert_eq!(serde_json::from_value::<LocalDay>(json).unwrap(), local);
        }
        assert_eq!(LocalDay::from_ymd(1970, 1, 1), Some(LocalDay(0)));
        assert_eq!(LocalDay::from_ymd(2025, 2, 29), None);
        let month = LocalDay::from_ymd(2026, 9, 12).unwrap().month();
        assert_eq!(serde_json::to_value(month).unwrap(), json!("2026-09"));
        assert_eq!(
            serde_json::from_value::<Month>(json!("2026-09")).unwrap(),
            month
        );
    }

    #[test]
    fn exposure_value_follows_the_settings_and_not_the_bias() {
        let base = Exposure {
            time_s: Some(1.0 / 60.0),
            f_number: Some(8.0),
            iso: Some(100),
            bias_ev: Some(0.0),
            focal_mm: Some(35.0),
            ..Exposure::default()
        };
        let brighter = Exposure {
            time_s: Some(1.0 / 15.0),
            bias_ev: Some(2.0),
            ..base
        };
        let step = base.ev().unwrap() - brighter.ev().unwrap();
        assert!((step - 2.0).abs() < 1e-4, "{step}");
        assert!(base.settings_match(&brighter));
        assert_eq!(Exposure::default().ev(), None);
        assert!(!Exposure::default().settings_match(&Exposure::default()));
    }

    #[test]
    fn a_position_measures_distance_and_refuses_garbage() {
        let konstanz = GeoPosition::new(47.660, 9.175, None).unwrap();
        let zurich = GeoPosition::new(47.377, 8.540, Some(408.0)).unwrap();
        let km = konstanz.distance_km(&zurich);
        assert!((55.0..60.0).contains(&km), "{km}");
        assert_eq!(GeoPosition::new(91.0, 0.0, None), None);
        assert_eq!(GeoPosition::new(f64::NAN, 0.0, None), None);
    }

    #[test]
    fn a_body_keys_on_its_serial_and_reads_as_its_model() {
        let body = CameraBody {
            make: "NIKON CORPORATION".into(),
            model: "NIKON Z 8".into(),
            serial: Some("3001234".into()),
        };
        assert_eq!(body.key().0, "NIKON CORPORATION|NIKON Z 8|3001234");
        assert_eq!(body.label(), "NIKON Z 8");
        let other = CameraBody {
            serial: Some("3009999".into()),
            ..body.clone()
        };
        assert_ne!(body.key(), other.key());
        let fuji = CameraBody {
            make: "FUJIFILM".into(),
            model: "X100VI".into(),
            serial: None,
        };
        assert_eq!(fuji.label(), "FUJIFILM X100VI");
        assert_eq!(fuji.key().0, "FUJIFILM|X100VI|");
        assert_eq!(BodyKey::of(None).0, "");
    }

    #[test]
    fn an_rgb_strip_needs_its_size_and_exact_length() {
        assert!(
            EmbeddedImage::new(
                10,
                160 * 120 * 3,
                EmbeddedFormat::Rgb8,
                Some(160),
                Some(120)
            )
            .is_ok()
        );
        assert!(EmbeddedImage::new(10, 100, EmbeddedFormat::Rgb8, None, None).is_err());
        assert!(EmbeddedImage::new(10, 100, EmbeddedFormat::Rgb8, Some(160), Some(120)).is_err());
        assert!(EmbeddedImage::new(10, 0, EmbeddedFormat::Jpeg, None, None).is_err());
        assert!(EmbeddedImage::new(10, 9, EmbeddedFormat::Jpeg, Some(160), None).is_err());
        let jpeg = EmbeddedImage::new(512, 4096, EmbeddedFormat::Jpeg, None, None).unwrap();
        let json = serde_json::to_value(jpeg).unwrap();
        assert_eq!(json, json!({"offset": 512, "len": 4096, "format": "jpeg"}));
        assert_eq!(serde_json::from_value::<EmbeddedImage>(json).unwrap(), jpeg);
        assert!(
            serde_json::from_value::<EmbeddedImage>(
                json!({"offset": 0, "len": 5, "format": "rgb8"})
            )
            .is_err(),
            "deserializing checks what the constructor checks"
        );
    }
}
