//! The EXIF a generated file carries, written with the pinned `kamadak-exif` writer the core's own
//! tests use: IFD0's make, model and orientation; the Exif IFD's capture time to the millisecond
//! with its offset (for the bodies that write one), body serial, lens and exposure; a GPS IFD for
//! positioned frames; and IFD1's embedded JPEG thumbnail.
use super::plan::{Frame, Gps};
use crate::{Result, ensure};
use exif::experimental::Writer;
use exif::{Field, In, Rational, SRational, Tag, Value};

/// The largest TIFF payload that fits one APP1 segment with its `Exif\0\0` header and length.
const MOST_APP1_PAYLOAD: usize = 65_535 - 2 - 6;

fn ascii(text: &str) -> Value {
    Value::Ascii(vec![text.as_bytes().to_vec()])
}

fn rational(num: u32, den: u32) -> Value {
    Value::Rational(vec![reduced(num, den)])
}

fn reduced(num: u32, den: u32) -> Rational {
    let divisor = gcd(num, den).max(1);
    Rational {
        num: num / divisor,
        denom: den / divisor,
    }
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// Degrees, minutes and seconds of `micro` microdegrees, exactly: the seconds are a fraction over
/// a million, so `degrees + minutes / 60 + seconds / 3600` is `micro / 1e6`.
fn dms(micro: i32) -> Value {
    let micro = micro.unsigned_abs();
    let minutes_micro = micro % 1_000_000 * 60;
    Value::Rational(vec![
        Rational {
            num: micro / 1_000_000,
            denom: 1,
        },
        Rational {
            num: minutes_micro / 1_000_000,
            denom: 1,
        },
        Rational {
            num: minutes_micro % 1_000_000 * 60,
            denom: 1_000_000,
        },
    ])
}

/// The exposure bias in thirds of an EV, as cameras write it: whole EVs over 1, others over 3.
pub fn bias(thirds: i32) -> SRational {
    if thirds % 3 == 0 {
        SRational {
            num: thirds / 3,
            denom: 1,
        }
    } else {
        SRational {
            num: thirds,
            denom: 3,
        }
    }
}

fn gps_fields(gps: Gps) -> [(Tag, Value); 7] {
    [
        (Tag::GPSVersionID, Value::Byte(vec![2, 3, 0, 0])),
        (
            Tag::GPSLatitudeRef,
            ascii(if gps.latitude < 0 { "S" } else { "N" }),
        ),
        (Tag::GPSLatitude, dms(gps.latitude)),
        (
            Tag::GPSLongitudeRef,
            ascii(if gps.longitude < 0 { "W" } else { "E" }),
        ),
        (Tag::GPSLongitude, dms(gps.longitude)),
        (
            Tag::GPSAltitudeRef,
            Value::Byte(vec![u8::from(gps.altitude < 0)]),
        ),
        (Tag::GPSAltitude, rational(gps.altitude.unsigned_abs(), 10)),
    ]
}

/// The TIFF structure of `frame`'s EXIF (what follows `Exif\0\0` in APP1), for an image of
/// `width` × `height` whose thumbnail is the JPEG `thumbnail`.
pub fn tiff(frame: &Frame, width: u32, height: u32, thumbnail: &[u8]) -> Result<Vec<u8>> {
    let body = frame.body();
    let exposure = frame.exposure;
    let mut primary = vec![
        (Tag::Make, ascii(body.make)),
        (Tag::Model, ascii(body.model)),
        (Tag::Orientation, Value::Short(vec![1])),
        (Tag::XResolution, rational(300, 1)),
        (Tag::YResolution, rational(300, 1)),
        (Tag::ResolutionUnit, Value::Short(vec![2])),
        (Tag::ExifVersion, Value::Undefined(b"0232".to_vec(), 0)),
        (
            Tag::ExposureTime,
            rational(exposure.time.0, exposure.time.1),
        ),
        (Tag::FNumber, rational(exposure.f_number, 100)),
        (
            Tag::PhotographicSensitivity,
            Value::Short(vec![exposure.iso]),
        ),
        (
            Tag::ExposureBiasValue,
            Value::SRational(vec![bias(exposure.bias)]),
        ),
        (Tag::FocalLength, rational(exposure.focal, 1000)),
        (
            Tag::FocalLengthIn35mmFilm,
            Value::Short(vec![exposure.focal_35]),
        ),
        (Tag::ColorSpace, Value::Short(vec![1])),
        (Tag::PixelXDimension, Value::Long(vec![width])),
        (Tag::PixelYDimension, Value::Long(vec![height])),
        (Tag::LensModel, ascii(body.lens)),
    ];
    if let Some(serial) = body.serial {
        primary.push((Tag::BodySerialNumber, ascii(serial)));
    }
    if let Some(time) = frame.time {
        primary.push((Tag::DateTimeOriginal, ascii(&time.exif())));
        primary.push((Tag::SubSecTimeOriginal, ascii(&time.subsec())));
    }
    if let Some(offset) = frame.written_offset() {
        primary.push((
            Tag::OffsetTimeOriginal,
            ascii(&super::clock::offset_text(offset)),
        ));
    }
    if let Some(gps) = frame.gps {
        primary.extend(gps_fields(gps));
    }
    let fields: Vec<Field> = primary
        .into_iter()
        .map(|(tag, value)| (tag, In::PRIMARY, value))
        .chain([
            (Tag::Compression, In::THUMBNAIL, Value::Short(vec![6])),
            (Tag::XResolution, In::THUMBNAIL, rational(72, 1)),
            (Tag::YResolution, In::THUMBNAIL, rational(72, 1)),
            (Tag::ResolutionUnit, In::THUMBNAIL, Value::Short(vec![2])),
        ])
        .map(|(tag, ifd_num, value)| Field {
            tag,
            ifd_num,
            value,
        })
        .collect();
    let mut writer = Writer::new();
    for field in &fields {
        writer.push_field(field);
    }
    writer.set_jpeg(thumbnail, In::THUMBNAIL);
    let mut tiff = std::io::Cursor::new(Vec::new());
    writer.write(&mut tiff, body.little_endian)?;
    let tiff = tiff.into_inner();
    ensure(
        tiff.len() <= MOST_APP1_PAYLOAD,
        format!("EXIF of {} bytes does not fit one APP1 segment", tiff.len()),
    )?;
    Ok(tiff)
}
