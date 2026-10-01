//! The original's capture metadata and the export's EXIF segment.
//!
//! Contract (`docs/design/export.md#keep-metadata`):
//! - `CaptureMetadata::from_jpeg(bytes)` reads the APP1 EXIF of a whole JPEG file;
//!   `CaptureMetadata::from_raw(bytes)` reads a TIFF-structured RAW (NEF, DNG) or the EXIF of the
//!   JPEG a RAF embeds. Neither ever fails: malformed or absent EXIF is empty metadata.
//! - `exif_payload(width, height)` is the TIFF payload of the export's APP1 segment, without the
//!   `Exif\0\0` header: the kept fields plus Orientation 1, ColorSpace sRGB, PixelXDimension,
//!   PixelYDimension, ExifVersion and Software.
//! - `field_names()` lists the kept fields by their EXIF names, in a stable order.
//! - `jpeg_orientation(bytes)` is a JPEG file's EXIF orientation, 1 when absent or invalid: what
//!   the import turns the decoded pixels upright by.
//!
//! Only the fields of [`FIELDS`] are ever kept, each with its own EXIF type and count; everything
//! else in the original (maker notes, thumbnails, serial numbers, user comments, orientation,
//! dimensions, colour space) is never kept, so it cannot reach an export. The orientation is read
//! only by `jpeg_orientation`, never into [`CaptureMetadata`].

// The index lane (`crate::index`) is the header reader's caller.
pub(crate) mod header;
mod read;
mod write;

#[cfg(test)]
mod tests;

/// The longest ASCII field kept, counted as stored with its terminating NUL; a longer one is
/// dropped rather than cut.
const MAX_ASCII: u32 = 1024;

/// The largest TIFF payload one APP1 segment carries: the segment's 16-bit length counts its own
/// two bytes, and the 6-byte `Exif\0\0` header precedes the payload.
pub(crate) const MAX_PAYLOAD: usize = 65_535 - 2 - 6;

// TIFF field types.
const BYTE: u16 = 1;
const ASCII: u16 = 2;
const SHORT: u16 = 3;
const LONG: u16 = 4;
const RATIONAL: u16 = 5;
const UNDEFINED: u16 = 7;
const SRATIONAL: u16 = 10;
const IFD: u16 = 13;

// The IFD0 pointers to the Exif and GPS IFDs.
const EXIF_POINTER: u16 = 0x8769;
const GPS_POINTER: u16 = 0x8825;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ifd {
    Primary,
    Exif,
    Gps,
}

/// A kept field's EXIF type and count. A value of any other type or count is dropped.
#[derive(Clone, Copy, Debug)]
enum Kind {
    /// ASCII of at most [`MAX_ASCII`] bytes; `Some(n)` also fixes the text at `n` characters.
    Ascii(Option<usize>),
    /// `n` BYTEs.
    Bytes(u32),
    /// One SHORT.
    Short,
    /// `n` RATIONALs.
    Rational(u32),
    /// One SRATIONAL.
    SRational,
}

struct Field {
    name: &'static str,
    ifd: Ifd,
    tag: u16,
    kind: Kind,
}

const fn field(name: &'static str, ifd: Ifd, tag: u16, kind: Kind) -> Field {
    Field {
        name,
        ifd,
        tag,
        kind,
    }
}

/// The kept fields of the design's table, IFD0 then Exif then GPS, each group sorted by tag: the
/// order [`CaptureMetadata::field_names`] reports.
const FIELDS: [Field; 33] = {
    use Ifd::{Exif, Gps, Primary};
    use Kind::{Ascii, Bytes, Rational, SRational, Short};
    [
        field("ImageDescription", Primary, 0x010e, Ascii(None)),
        field("Make", Primary, 0x010f, Ascii(None)),
        field("Model", Primary, 0x0110, Ascii(None)),
        field("Artist", Primary, 0x013b, Ascii(None)),
        field("Copyright", Primary, COPYRIGHT, Ascii(None)),
        field("ExposureTime", Exif, 0x829a, Rational(1)),
        field("FNumber", Exif, 0x829d, Rational(1)),
        field("ExposureProgram", Exif, 0x8822, Short),
        field("PhotographicSensitivity", Exif, 0x8827, Short),
        field("DateTimeOriginal", Exif, 0x9003, Ascii(Some(19))),
        field("DateTimeDigitized", Exif, 0x9004, Ascii(Some(19))),
        field("OffsetTimeOriginal", Exif, 0x9011, Ascii(Some(6))),
        field("OffsetTimeDigitized", Exif, 0x9012, Ascii(Some(6))),
        field("ExposureBiasValue", Exif, 0x9204, SRational),
        field("MeteringMode", Exif, 0x9207, Short),
        field("Flash", Exif, 0x9209, Short),
        field("FocalLength", Exif, 0x920a, Rational(1)),
        field("SubSecTimeOriginal", Exif, 0x9291, Ascii(None)),
        field("FocalLengthIn35mmFilm", Exif, 0xa405, Short),
        field("LensSpecification", Exif, 0xa432, Rational(4)),
        field("LensMake", Exif, 0xa433, Ascii(None)),
        field("LensModel", Exif, 0xa434, Ascii(None)),
        field("GPSVersionID", Gps, 0x0000, Bytes(4)),
        field("GPSLatitudeRef", Gps, 0x0001, Ascii(Some(1))),
        field("GPSLatitude", Gps, 0x0002, Rational(3)),
        field("GPSLongitudeRef", Gps, 0x0003, Ascii(Some(1))),
        field("GPSLongitude", Gps, 0x0004, Rational(3)),
        field("GPSAltitudeRef", Gps, 0x0005, Bytes(1)),
        field("GPSAltitude", Gps, 0x0006, Rational(1)),
        field("GPSTimeStamp", Gps, 0x0007, Rational(3)),
        field("GPSImgDirectionRef", Gps, 0x0010, Ascii(Some(1))),
        field("GPSImgDirection", Gps, 0x0011, Rational(1)),
        field("GPSDateStamp", Gps, 0x001d, Ascii(Some(10))),
    ]
};

/// Copyright keeps its NUL-separated photographer and editor parts; every other ASCII field ends
/// at its first NUL.
const COPYRIGHT: u16 = 0x8298;

/// The descriptive fields, dropped first when the kept fields would not fit one APP1 segment.
const DESCRIPTIVE: [u16; 3] = [0x010e, 0x013b, COPYRIGHT];

/// One kept value, as stored: rationals stay numerator and denominator pairs, so nothing is
/// rounded and the metadata is `Eq`.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Value {
    /// The text without its terminating NUL or trailing spaces.
    Ascii(Box<[u8]>),
    Bytes(Box<[u8]>),
    Short(u16),
    Rational(Box<[[u32; 2]]>),
    SRational([i32; 2]),
}

/// The whitelisted, validated EXIF fields of one original. Holds no floats, so it is `Eq`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CaptureMetadata {
    /// `(index into FIELDS, value)` in table order, each field at most once. Always small enough
    /// for [`CaptureMetadata::exif_payload`] to fit one APP1 segment.
    fields: Vec<(usize, Value)>,
}

/// The EXIF orientation of a JPEG file's first `Exif` APP1 segment before its scan, read with the
/// same bounded reader as the kept fields: IFD0's first Orientation entry that is one SHORT from 1
/// to 8, else 1.
pub(crate) fn jpeg_orientation(bytes: &[u8]) -> u8 {
    read::jpeg_exif(bytes)
        .and_then(read::orientation)
        .unwrap_or(1)
}

impl CaptureMetadata {
    fn value(&self, ifd: Ifd, tag: u16) -> Option<&Value> {
        self.fields.iter().find_map(|(index, value)| {
            let field = &FIELDS[*index];
            (field.ifd == ifd && field.tag == tag).then_some(value)
        })
    }

    fn text(&self, ifd: Ifd, tag: u16) -> Option<&str> {
        match self.value(ifd, tag)? {
            Value::Ascii(bytes) => std::str::from_utf8(bytes).ok(),
            _ => None,
        }
    }

    /// The camera maker as captured in EXIF, without decoder normalization.
    pub fn make(&self) -> Option<&str> {
        self.text(Ifd::Primary, 0x010f)
    }

    /// The camera model as captured in EXIF, without decoder normalization.
    pub fn model(&self) -> Option<&str> {
        self.text(Ifd::Primary, 0x0110)
    }

    /// The lens maker as captured in EXIF.
    pub fn lens_make(&self) -> Option<&str> {
        self.text(Ifd::Exif, 0xa433)
    }

    /// The lens model as captured in EXIF.
    pub fn lens_model(&self) -> Option<&str> {
        self.text(Ifd::Exif, 0xa434)
    }

    /// The physical focal length in millimetres. A zero or invalid rational is absent.
    pub fn focal_length_mm(&self) -> Option<f64> {
        match self.value(Ifd::Exif, 0x920a)? {
            Value::Rational(values) => {
                let [numerator, denominator] = *values.first()?;
                (numerator > 0 && denominator > 0)
                    .then(|| f64::from(numerator) / f64::from(denominator))
            }
            _ => None,
        }
    }

    /// The 35 mm equivalent focal length. EXIF zero means unknown.
    pub fn focal_length_35mm(&self) -> Option<u16> {
        match self.value(Ifd::Exif, 0xa405)? {
            Value::Short(value) if *value > 0 => Some(*value),
            _ => None,
        }
    }

    /// The EXIF of a JPEG file's first `Exif` APP1 segment before its scan.
    pub(crate) fn from_jpeg(bytes: &[u8]) -> Self {
        read::jpeg_exif(bytes)
            .map(Self::from_tiff)
            .unwrap_or_default()
    }

    /// The EXIF of a RAW original: the file's own TIFF structure (NEF, DNG), or the EXIF of the
    /// JPEG a RAF embeds. Any other container is empty metadata.
    pub(crate) fn from_raw(bytes: &[u8]) -> Self {
        if read::is_tiff(bytes) {
            Self::from_tiff(bytes)
        } else if let Some(jpeg) = read::raf_jpeg(bytes) {
            Self::from_jpeg(jpeg)
        } else {
            Self::default()
        }
    }

    fn from_tiff(tiff: &[u8]) -> Self {
        let mut fields = read::fields(tiff);
        // A stable sort keeps the first of a duplicated tag, which the dedup then keeps.
        fields.sort_by_key(|(index, _)| *index);
        fields.dedup_by_key(|(index, _)| *index);
        let mut metadata = Self { fields };
        metadata.fit();
        metadata
    }

    /// Drop fields until the payload fits one APP1 segment: the longest descriptive field first,
    /// then the longest of any. With every ASCII field capped at [`MAX_ASCII`] the table cannot
    /// reach the limit, so this only bounds the writer; it never runs on a real original.
    fn fit(&mut self) {
        while write::payload(&self.fields, 0, 0).len() > MAX_PAYLOAD {
            let Some(victim) = self
                .fields
                .iter()
                .enumerate()
                .max_by_key(|(_, (index, value))| {
                    (DESCRIPTIVE.contains(&FIELDS[*index].tag), value.len())
                })
                .map(|(position, _)| position)
            else {
                return;
            };
            self.fields.remove(victim);
        }
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// The kept fields' EXIF names: IFD0, then Exif, then GPS, each by tag.
    pub(crate) fn field_names(&self) -> Vec<&'static str> {
        self.fields
            .iter()
            .map(|(index, _)| FIELDS[*index].name)
            .collect()
    }

    /// The export's EXIF: a little-endian TIFF with IFD0, the Exif IFD and, when any GPS field is
    /// kept, the GPS IFD, and no IFD1, so no thumbnail. `width` and `height` are the exported
    /// frame's. At most [`MAX_PAYLOAD`] bytes.
    pub(crate) fn exif_payload(&self, width: u32, height: u32) -> Vec<u8> {
        write::payload(&self.fields, width, height)
    }
}
