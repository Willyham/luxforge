//! The fields a header keeps: read from IFD0, Exif and GPS IFDs into [`Found`], first found wins,
//! then turned into a [`FileHeader`] once. Each value is validated as it is read, so an invalid one
//! leaves its slot free for a later source (a RW2's embedded JPEG, IFD0's DateTime after
//! DateTimeOriginal).

use super::source::Source;
use super::tiff::{Entry, Ifd, MAX_TEXT_BYTES, Tiff};
use super::{
    CaptureTime, Container, DateTime, Dimensions, FileHeader, GpsPosition, GpsTime,
    HEADER_HEAD_BYTES, MAX_THUMBNAIL_JPEG_BYTES, MAX_THUMBNAIL_SIDE, Rational, SignedRational,
    Thumbnail, ThumbnailFormat, TimeSource,
};

// IFD0.
const MAKE: u16 = 0x010f;
const MODEL: u16 = 0x0110;
const ORIENTATION: u16 = 0x0112;
const DATE_TIME: u16 = 0x0132;
const CAMERA_SERIAL: u16 = 0xc62f;
// The Exif IFD.
const EXPOSURE_TIME: u16 = 0x829a;
const F_NUMBER: u16 = 0x829d;
const PHOTOGRAPHIC_SENSITIVITY: u16 = 0x8827;
const SENSITIVITY_TYPE: u16 = 0x8830;
const STANDARD_OUTPUT_SENSITIVITY: u16 = 0x8831;
const RECOMMENDED_EXPOSURE_INDEX: u16 = 0x8832;
const ISO_SPEED: u16 = 0x8833;
const DATE_TIME_ORIGINAL: u16 = 0x9003;
const DATE_TIME_DIGITIZED: u16 = 0x9004;
const OFFSET_TIME: u16 = 0x9010;
const OFFSET_TIME_ORIGINAL: u16 = 0x9011;
const OFFSET_TIME_DIGITIZED: u16 = 0x9012;
const EXPOSURE_BIAS: u16 = 0x9204;
const FOCAL_LENGTH: u16 = 0x920a;
const SUB_SEC_TIME: u16 = 0x9290;
const SUB_SEC_TIME_ORIGINAL: u16 = 0x9291;
const SUB_SEC_TIME_DIGITIZED: u16 = 0x9292;
const PIXEL_X_DIMENSION: u16 = 0xa002;
const PIXEL_Y_DIMENSION: u16 = 0xa003;
const FOCAL_LENGTH_35MM: u16 = 0xa405;
const BODY_SERIAL: u16 = 0xa431;
const LENS_MAKE: u16 = 0xa433;
const LENS_MODEL: u16 = 0xa434;
// The GPS IFD.
const GPS_LATITUDE_REF: u16 = 0x0001;
const GPS_LATITUDE: u16 = 0x0002;
const GPS_LONGITUDE_REF: u16 = 0x0003;
const GPS_LONGITUDE: u16 = 0x0004;
const GPS_ALTITUDE_REF: u16 = 0x0005;
const GPS_ALTITUDE: u16 = 0x0006;
const GPS_TIME_STAMP: u16 = 0x0007;
const GPS_STATUS: u16 = 0x0009;
const GPS_DATE_STAMP: u16 = 0x001d;

/// Which table an IFD's tags are read as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum IfdKind {
    Primary,
    Exif,
    Gps,
}

/// Where a body serial came from, in order of preference.
#[derive(Clone, Copy)]
pub(super) enum SerialSource {
    Exif,
    Dng,
    MakerNote,
}

/// The sensitivity tags, resolved into one ISO by [`Iso::value`].
#[derive(Default)]
pub(super) struct Iso {
    photographic: Option<u32>,
    sensitivity_type: Option<u32>,
    standard_output: Option<u32>,
    recommended_index: Option<u32>,
    speed: Option<u32>,
    /// A RW2's IFD0 ISO (0x0017).
    pub(super) panasonic: Option<u32>,
}

impl Iso {
    /// PhotographicSensitivity, unless it is 65535 ("at least"), absent or zero; then the tag its
    /// SensitivityType names (ISO speed, then recommended exposure index, then standard output
    /// sensitivity, or any of them in that order when the type is unknown); then a RW2's own.
    fn value(&self) -> Option<u32> {
        let usable = |value: Option<u32>| value.filter(|value| (1..65_535).contains(value));
        let wide = |value: Option<u32>| value.filter(|value| *value > 0);
        if let Some(value) = usable(self.photographic) {
            return Some(value);
        }
        let (speed, index, output) = match self.sensitivity_type {
            Some(1) => (false, false, true),
            Some(2) => (false, true, false),
            Some(3) => (true, false, false),
            Some(4) => (false, true, true),
            Some(5) => (true, false, true),
            Some(6) => (true, true, false),
            _ => (true, true, true),
        };
        [
            (speed, self.speed),
            (index, self.recommended_index),
            (output, self.standard_output),
        ]
        .into_iter()
        .find_map(|(named, value)| if named { wide(value) } else { None })
        .or_else(|| usable(self.panasonic))
    }
}

/// A GPS IFD's fields, kept together: a position is read from one IFD only.
#[derive(Default)]
struct Gps {
    latitude_ref: Option<u8>,
    latitude: Option<[Rational; 3]>,
    longitude_ref: Option<u8>,
    longitude: Option<[Rational; 3]>,
    altitude_ref: Option<u32>,
    altitude: Option<Rational>,
    time: Option<[Rational; 3]>,
    date: Option<[u8; 10]>,
    status: Option<u8>,
}

impl Gps {
    fn is_empty(&self) -> bool {
        self.latitude.is_none() && self.longitude.is_none() && self.time.is_none()
    }

    /// Degrees from the rationals and their references; none when a reference is missing, the
    /// status is void, the position is outside the globe, or every rational is zero, as cameras
    /// write it without a fix.
    fn position(&self) -> Option<GpsPosition> {
        if self.status == Some(b'V') {
            return None;
        }
        let latitude = self.latitude?;
        let longitude = self.longitude?;
        if latitude.iter().chain(&longitude).all(|part| part.num == 0) {
            return None;
        }
        let degrees = |parts: [Rational; 3]| {
            parts[0].value() + parts[1].value() / 60.0 + parts[2].value() / 3600.0
        };
        let latitude = match self.latitude_ref? {
            b'N' => degrees(latitude),
            b'S' => -degrees(latitude),
            _ => return None,
        };
        let longitude = match self.longitude_ref? {
            b'E' => degrees(longitude),
            b'W' => -degrees(longitude),
            _ => return None,
        };
        if latitude.abs() > 90.0 || longitude.abs() > 180.0 {
            return None;
        }
        let altitude = self.altitude.and_then(|altitude| match self.altitude_ref {
            None | Some(0) => Some(altitude.value()),
            Some(1) => Some(-altitude.value()),
            Some(_) => None,
        });
        Some(GpsPosition {
            latitude,
            longitude,
            altitude,
        })
    }

    /// The UTC date and time from GPSDateStamp and GPSTimeStamp, whole hours and minutes and a
    /// second below 60.
    fn time(&self) -> Option<GpsTime> {
        let date = self.date?;
        let (year, month, day) = date_parts(&date)?;
        let [hour, minute, second] = self.time?;
        let whole = |part: Rational| {
            part.num
                .is_multiple_of(part.den)
                .then_some(part.num / part.den)
        };
        let (hour, minute) = (whole(hour)?, whole(minute)?);
        let seconds = second.num / second.den;
        let nanos = u64::from(second.num % second.den) * 1_000_000_000 / u64::from(second.den);
        let utc = DateTime::new(
            year,
            month,
            day,
            u8::try_from(hour).ok()?,
            u8::try_from(minute).ok()?,
            u8::try_from(seconds).ok()?,
        )?;
        Some(GpsTime {
            utc,
            nanos: u32::try_from(nanos).ok()?,
        })
    }
}

/// The candidate thumbnails: the smallest JPEG and the smallest uncompressed RGB one.
#[derive(Default)]
struct Thumbnails {
    jpeg: Option<Thumbnail>,
    rgb: Option<Thumbnail>,
}

/// What a header read has found so far, first found wins.
#[derive(Default)]
pub(super) struct Found {
    pub(super) make: Option<String>,
    model: Option<String>,
    orientation: Option<u8>,
    /// By [`TimeSource`]: the time, its subsecond and its offset.
    times: [Option<DateTime>; 3],
    subsecs: [Option<u32>; 3],
    offsets: [Option<i16>; 3],
    exposure_time: Option<Rational>,
    f_number: Option<Rational>,
    pub(super) iso: Iso,
    bias: Option<SignedRational>,
    focal_length: Option<Rational>,
    focal_length_35mm: Option<u32>,
    /// The Exif IFD's image size, a fallback for containers whose raw image declares none.
    pixel_width: Option<u32>,
    pixel_height: Option<u32>,
    lens_make: Option<String>,
    lens_model: Option<String>,
    serials: [Option<String>; 3],
    gps: Option<Gps>,
    pub(super) dimensions: Option<Dimensions>,
    thumbnails: Thumbnails,
}

/// Fill `slot` from `read` when it is still empty; `read` is not called otherwise, so a filled
/// slot costs no read.
fn fill<T>(slot: &mut Option<T>, read: impl FnOnce() -> Option<T>) {
    if slot.is_none() {
        *slot = read();
    }
}

fn nonzero(value: Option<Rational>) -> Option<Rational> {
    value.filter(|value| value.num > 0)
}

impl Found {
    /// Keep `ifd`'s fields as a `kind` table.
    pub(super) fn collect(
        &mut self,
        source: &mut impl Source,
        tiff: &Tiff,
        ifd: &Ifd,
        kind: IfdKind,
    ) {
        match kind {
            IfdKind::Primary => {
                for entry in ifd.entries() {
                    self.primary(source, tiff, &entry);
                }
            }
            IfdKind::Exif => {
                for entry in ifd.entries() {
                    self.exif(source, tiff, &entry);
                }
            }
            IfdKind::Gps => {
                if self.gps.is_none() {
                    let gps = read_gps(source, tiff, ifd);
                    if !gps.is_empty() {
                        self.gps = Some(gps);
                    }
                }
            }
        }
    }

    fn primary(&mut self, source: &mut impl Source, tiff: &Tiff, entry: &Entry) {
        match entry.tag {
            MAKE => fill(&mut self.make, || tiff.text(source, entry)),
            MODEL => fill(&mut self.model, || tiff.text(source, entry)),
            ORIENTATION => fill(&mut self.orientation, || {
                tiff.uint(source, entry)
                    .and_then(|value| u8::try_from(value).ok())
                    .filter(|value| (1..=8).contains(value))
            }),
            DATE_TIME => self.time(source, tiff, entry, TimeSource::Modified),
            CAMERA_SERIAL => self.serial(SerialSource::Dng, || tiff.text(source, entry)),
            _ => {}
        }
    }

    fn exif(&mut self, source: &mut impl Source, tiff: &Tiff, entry: &Entry) {
        let uint = |source: &mut _| tiff.uint(source, entry);
        match entry.tag {
            EXPOSURE_TIME => fill(&mut self.exposure_time, || {
                nonzero(tiff.rational(source, entry))
            }),
            F_NUMBER => fill(&mut self.f_number, || nonzero(tiff.rational(source, entry))),
            PHOTOGRAPHIC_SENSITIVITY => fill(&mut self.iso.photographic, || uint(source)),
            SENSITIVITY_TYPE => fill(&mut self.iso.sensitivity_type, || uint(source)),
            STANDARD_OUTPUT_SENSITIVITY => fill(&mut self.iso.standard_output, || uint(source)),
            RECOMMENDED_EXPOSURE_INDEX => fill(&mut self.iso.recommended_index, || uint(source)),
            ISO_SPEED => fill(&mut self.iso.speed, || uint(source)),
            DATE_TIME_ORIGINAL => self.time(source, tiff, entry, TimeSource::Original),
            DATE_TIME_DIGITIZED => self.time(source, tiff, entry, TimeSource::Digitized),
            OFFSET_TIME => self.offset(source, tiff, entry, TimeSource::Modified),
            OFFSET_TIME_ORIGINAL => self.offset(source, tiff, entry, TimeSource::Original),
            OFFSET_TIME_DIGITIZED => self.offset(source, tiff, entry, TimeSource::Digitized),
            SUB_SEC_TIME => self.subsec(source, tiff, entry, TimeSource::Modified),
            SUB_SEC_TIME_ORIGINAL => self.subsec(source, tiff, entry, TimeSource::Original),
            SUB_SEC_TIME_DIGITIZED => self.subsec(source, tiff, entry, TimeSource::Digitized),
            EXPOSURE_BIAS => fill(&mut self.bias, || tiff.signed_rational(source, entry)),
            FOCAL_LENGTH => fill(&mut self.focal_length, || {
                nonzero(tiff.rational(source, entry))
            }),
            FOCAL_LENGTH_35MM => fill(&mut self.focal_length_35mm, || {
                uint(source).filter(|value| *value > 0)
            }),
            PIXEL_X_DIMENSION => fill(&mut self.pixel_width, || uint(source)),
            PIXEL_Y_DIMENSION => fill(&mut self.pixel_height, || uint(source)),
            BODY_SERIAL => self.serial(SerialSource::Exif, || tiff.text(source, entry)),
            LENS_MAKE => fill(&mut self.lens_make, || tiff.text(source, entry)),
            LENS_MODEL => fill(&mut self.lens_model, || tiff.text(source, entry)),
            _ => {}
        }
    }

    /// The Exif IFD's PixelXDimension and PixelYDimension: the image size a container states when
    /// its raw image declares none.
    pub(super) fn exif_size(&self) -> Option<Dimensions> {
        let (width, height) = (self.pixel_width?, self.pixel_height?);
        (width > 0 && height > 0).then_some(Dimensions { width, height })
    }

    fn time(&mut self, source: &mut impl Source, tiff: &Tiff, entry: &Entry, time: TimeSource) {
        fill(&mut self.times[time as usize], || {
            let mut buffer = [0; MAX_TEXT_BYTES];
            DateTime::parse(tiff.raw_text(source, entry, &mut buffer)?)
        });
    }

    fn offset(&mut self, source: &mut impl Source, tiff: &Tiff, entry: &Entry, time: TimeSource) {
        fill(&mut self.offsets[time as usize], || {
            let mut buffer = [0; MAX_TEXT_BYTES];
            parse_offset(tiff.raw_text(source, entry, &mut buffer)?)
        });
    }

    fn subsec(&mut self, source: &mut impl Source, tiff: &Tiff, entry: &Entry, time: TimeSource) {
        fill(&mut self.subsecs[time as usize], || {
            let mut buffer = [0; MAX_TEXT_BYTES];
            parse_subsec(tiff.raw_text(source, entry, &mut buffer)?)
        });
    }

    /// Keep a body serial from `from` when none from there or a preferred source is kept yet;
    /// otherwise `read` is not called, so a serial that would be discarded costs nothing.
    pub(super) fn serial(&mut self, from: SerialSource, read: impl FnOnce() -> Option<String>) {
        let from = from as usize;
        if self.serials[..from].iter().all(Option::is_none) {
            fill(&mut self.serials[from], read);
        }
    }

    /// Offer a JPEG thumbnail at `offset`, `length` bytes long: kept when it lies inside the file,
    /// is at most [`MAX_THUMBNAIL_JPEG_BYTES`], starts with SOI when its start is inside the head
    /// (already read, so checking it costs nothing and decides the same in both readers), and is
    /// the smallest so far.
    pub(super) fn offer_jpeg(
        &mut self,
        source: &mut impl Source,
        offset: u64,
        length: u64,
        size: Option<Dimensions>,
    ) {
        if length == 0 || length > MAX_THUMBNAIL_JPEG_BYTES || !inside(source, offset, length) {
            return;
        }
        if offset + 2 <= HEADER_HEAD_BYTES as u64 && source.array(offset) != Some([0xff, 0xd8]) {
            return;
        }
        let candidate = Thumbnail {
            offset,
            length,
            format: ThumbnailFormat::Jpeg { size },
        };
        if self
            .thumbnails
            .jpeg
            .is_none_or(|kept| candidate.length < kept.length)
        {
            self.thumbnails.jpeg = Some(candidate);
        }
    }

    /// Offer the JPEG `ifd` names by JPEGInterchangeFormat (0x0201) and its length (0x0202),
    /// when it lies inside the structure that names it: a JPEG's EXIF segment, or the file.
    pub(super) fn offer_jpeg_pointer(&mut self, source: &mut impl Source, tiff: &Tiff, ifd: &Ifd) {
        let (Some(start), Some(length)) = (ifd.find(0x0201), ifd.find(0x0202)) else {
            return;
        };
        let (Some(start), Some(length)) = (tiff.uint(source, &start), tiff.uint(source, &length))
        else {
            return;
        };
        let offset = tiff.absolute(start);
        if offset + u64::from(length) <= tiff.end() {
            self.offer_jpeg(source, offset, u64::from(length), None);
        }
    }

    /// Offer an uncompressed 8-bit RGB thumbnail: kept when its strip is exactly its pixels,
    /// lies inside the file, is at most [`MAX_THUMBNAIL_SIDE`] on each side, and is the smallest
    /// so far.
    pub(super) fn offer_rgb(
        &mut self,
        source: &mut impl Source,
        offset: u64,
        length: u64,
        size: Dimensions,
    ) {
        let pixels = u64::from(size.width) * u64::from(size.height);
        if pixels == 0
            || size.width.max(size.height) > MAX_THUMBNAIL_SIDE
            || length != pixels * 3
            || !inside(source, offset, length)
        {
            return;
        }
        let candidate = Thumbnail {
            offset,
            length,
            format: ThumbnailFormat::Rgb8 { size },
        };
        if self
            .thumbnails
            .rgb
            .is_none_or(|kept| candidate.length < kept.length)
        {
            self.thumbnails.rgb = Some(candidate);
        }
    }

    pub(super) fn finish(self, container: Container) -> FileHeader {
        let capture_time = [
            TimeSource::Original,
            TimeSource::Digitized,
            TimeSource::Modified,
        ]
        .into_iter()
        .find_map(|source| {
            let index = source as usize;
            Some(CaptureTime {
                local: self.times[index]?,
                subsec_nanos: self.subsecs[index],
                offset_minutes: self.offsets[index],
                source,
            })
        });
        let [exif, dng, maker] = self.serials;
        FileHeader {
            container,
            capture_time,
            gps_time: self.gps.as_ref().and_then(Gps::time),
            make: self.make,
            model: self.model,
            body_serial: exif.or(dng).or(maker),
            lens_make: self.lens_make,
            lens_model: self.lens_model,
            exposure_time: self.exposure_time,
            f_number: self.f_number,
            iso: self.iso.value(),
            exposure_bias: self.bias,
            focal_length: self.focal_length,
            focal_length_35mm: self.focal_length_35mm,
            gps: self.gps.as_ref().and_then(Gps::position),
            dimensions: self.dimensions,
            orientation: self.orientation,
            thumbnail: self.thumbnails.jpeg.or(self.thumbnails.rgb),
        }
    }
}

fn inside(source: &impl Source, offset: u64, length: u64) -> bool {
    offset
        .checked_add(length)
        .is_some_and(|end| end <= source.len())
}

fn read_gps(source: &mut impl Source, tiff: &Tiff, ifd: &Ifd) -> Gps {
    let mut gps = Gps::default();
    let letter = |source: &mut _, entry: &Entry| {
        let mut buffer = [0; MAX_TEXT_BYTES];
        match tiff.raw_text(source, entry, &mut buffer)? {
            [letter] => Some(letter.to_ascii_uppercase()),
            _ => None,
        }
    };
    // One to three rationals, the missing ones zero: some receivers write decimal degrees alone.
    let triple = |source: &mut _, entry: &Entry| {
        if !(1..=3).contains(&entry.count) {
            return None;
        }
        let mut parts = [Rational { num: 0, den: 1 }; 3];
        for (index, part) in (0..entry.count).zip(&mut parts) {
            *part = tiff.rational_at(source, entry, index)?;
        }
        Some(parts)
    };
    for entry in ifd.entries() {
        match entry.tag {
            GPS_LATITUDE_REF => fill(&mut gps.latitude_ref, || letter(source, &entry)),
            GPS_LATITUDE => fill(&mut gps.latitude, || triple(source, &entry)),
            GPS_LONGITUDE_REF => fill(&mut gps.longitude_ref, || letter(source, &entry)),
            GPS_LONGITUDE => fill(&mut gps.longitude, || triple(source, &entry)),
            GPS_ALTITUDE_REF => fill(&mut gps.altitude_ref, || tiff.uint(source, &entry)),
            GPS_ALTITUDE => fill(&mut gps.altitude, || tiff.rational(source, &entry)),
            GPS_TIME_STAMP => fill(&mut gps.time, || triple(source, &entry)),
            GPS_STATUS => fill(&mut gps.status, || letter(source, &entry)),
            GPS_DATE_STAMP => fill(&mut gps.date, || {
                let mut buffer = [0; MAX_TEXT_BYTES];
                tiff.raw_text(source, &entry, &mut buffer)?.try_into().ok()
            }),
            _ => {}
        }
    }
    gps
}

impl DateTime {
    /// A validated date and time, or none.
    pub fn new(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> Option<Self> {
        let leap =
            year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
        let days = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if leap => 29,
            2 => 28,
            _ => return None,
        };
        (year > 0 && (1..=days).contains(&day) && hour < 24 && minute < 60 && second < 60)
            .then_some(Self {
                year,
                month,
                day,
                hour,
                minute,
                second,
            })
    }

    /// `YYYY:MM:DD HH:MM:SS` as EXIF writes it (a `-` between the date's parts and a `T` before
    /// the time are accepted too); none when blank, zero or not a real date and time.
    pub(super) fn parse(text: &[u8]) -> Option<Self> {
        let [date @ .., b' ' | b'T', h0, h1, b':', m0, m1, b':', s0, s1] = text else {
            return None;
        };
        let (year, month, day) = date_parts(date)?;
        Self::new(
            year,
            month,
            day,
            two_digits(*h0, *h1)?,
            two_digits(*m0, *m1)?,
            two_digits(*s0, *s1)?,
        )
    }
}

fn digit(byte: u8) -> Option<u8> {
    byte.is_ascii_digit().then(|| byte - b'0')
}

fn two_digits(tens: u8, units: u8) -> Option<u8> {
    Some(digit(tens)? * 10 + digit(units)?)
}

/// `YYYY:MM:DD` (or with `-`) as year, month and day, unvalidated.
fn date_parts(text: &[u8]) -> Option<(u16, u8, u8)> {
    let [y0, y1, y2, y3, separator, m0, m1, again, d0, d1] = *text else {
        return None;
    };
    if !matches!(separator, b':' | b'-') || again != separator {
        return None;
    }
    let year = [y0, y1, y2, y3].into_iter().try_fold(0_u16, |year, byte| {
        Some(year * 10 + u16::from(digit(byte)?))
    })?;
    Some((year, two_digits(m0, m1)?, two_digits(d0, d1)?))
}

/// A SubSecTime's digits as the decimal fraction they are: "42" is 420,000,000 ns. Digits past
/// the ninth are ignored; anything but digits is invalid.
pub(super) fn parse_subsec(text: &[u8]) -> Option<u32> {
    if text.is_empty() || !text.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let digits = &text[..text.len().min(9)];
    let value = digits
        .iter()
        .fold(0_u32, |value, byte| value * 10 + u32::from(byte - b'0'));
    Some(value * 10_u32.pow(9 - digits.len() as u32))
}

/// An OffsetTime `+HH:MM` or `-HH:MM` in minutes east of UTC, within ±14:00.
pub(super) fn parse_offset(text: &[u8]) -> Option<i16> {
    let [sign, h0, h1, b':', m0, m1] = *text else {
        return None;
    };
    let (hours, minutes) = (two_digits(h0, h1)?, two_digits(m0, m1)?);
    if minutes >= 60 || i16::from(hours) * 60 + i16::from(minutes) > 14 * 60 {
        return None;
    }
    let magnitude = i16::from(hours) * 60 + i16::from(minutes);
    match sign {
        b'+' => Some(magnitude),
        b'-' => Some(-magnitude),
        _ => None,
    }
}
