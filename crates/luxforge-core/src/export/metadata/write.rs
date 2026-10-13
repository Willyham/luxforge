//! The export's EXIF writer: a little-endian TIFF of IFD0, the Exif IFD and an optional GPS IFD,
//! entries sorted by tag, values longer than four bytes in each IFD's data area on word
//! boundaries, and no IFD1. The same IFDs, with the image's own tags in IFD0, are a TIFF export's
//! directories ([`super::CaptureMetadata::tiff_directories`]).

use super::{
    ASCII, BYTE, EXIF_POINTER, FIELDS, GPS_POINTER, Ifd, LONG, RATIONAL, SHORT, SRATIONAL,
    UNDEFINED, Value,
};

// The structural fields the export defines itself.
const ORIENTATION: u16 = 0x0112;
const SOFTWARE: u16 = 0x0131;
const EXIF_VERSION: u16 = 0x9000;
const COLOR_SPACE: u16 = 0xa001;
const PIXEL_X_DIMENSION: u16 = 0xa002;
const PIXEL_Y_DIMENSION: u16 = 0xa003;

/// One IFD entry to write, its value already little-endian.
struct Entry {
    tag: u16,
    kind: u16,
    count: u32,
    data: Vec<u8>,
}

impl Entry {
    fn short(tag: u16, value: u16) -> Self {
        Self {
            tag,
            kind: SHORT,
            count: 1,
            data: value.to_le_bytes().to_vec(),
        }
    }

    fn long(tag: u16, value: u32) -> Self {
        Self {
            tag,
            kind: LONG,
            count: 1,
            data: value.to_le_bytes().to_vec(),
        }
    }

    fn ascii(tag: u16, text: &[u8]) -> Self {
        let mut data = Vec::with_capacity(text.len() + 1);
        data.extend_from_slice(text);
        data.push(0);
        Self {
            tag,
            kind: ASCII,
            // The reader caps kept text at `MAX_ASCII` bytes.
            count: data.len() as u32,
            data,
        }
    }

    fn value(tag: u16, value: &Value) -> Self {
        match value {
            Value::Ascii(text) => Self::ascii(tag, text),
            Value::Bytes(bytes) => Self {
                tag,
                kind: BYTE,
                count: bytes.len() as u32,
                data: bytes.to_vec(),
            },
            Value::Short(value) => Self::short(tag, *value),
            Value::Rational(pairs) => Self {
                tag,
                kind: RATIONAL,
                count: pairs.len() as u32,
                data: pairs
                    .iter()
                    .flatten()
                    .flat_map(|word| word.to_le_bytes())
                    .collect(),
            },
            Value::SRational(pair) => Self {
                tag,
                kind: SRATIONAL,
                count: 1,
                data: pair.iter().flat_map(|word| word.to_le_bytes()).collect(),
            },
        }
    }

    /// The bytes this entry adds to its IFD's data area: none when its value is inline.
    fn data_len(&self) -> usize {
        if self.data.len() > 4 {
            self.data.len().next_multiple_of(2)
        } else {
            0
        }
    }
}

// Until the export job writes TIFF, only the tests reach the TIFF directories.
#[cfg_attr(not(test), allow(dead_code))]
impl Entry {
    fn shorts(tag: u16, values: &[u16]) -> Self {
        Self {
            tag,
            kind: SHORT,
            count: values.len() as u32,
            data: values
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect(),
        }
    }

    /// `values` LONGs: a TIFF's strip table, at most one per row of at most 65,535.
    fn longs(tag: u16, values: &[u32]) -> Self {
        Self {
            tag,
            kind: LONG,
            count: values.len() as u32,
            data: values
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect(),
        }
    }

    fn rational(tag: u16, [numerator, denominator]: [u32; 2]) -> Self {
        Self::value(tag, &Value::Rational(Box::new([[numerator, denominator]])))
    }

    /// An ICC profile, which `crate::profile` bounds far below 4 GiB.
    fn undefined(tag: u16, bytes: &[u8]) -> Self {
        Self {
            tag,
            kind: UNDEFINED,
            count: bytes.len() as u32,
            data: bytes.to_vec(),
        }
    }
}

impl Value {
    /// The value's size as written, which [`super::CaptureMetadata::fit`] compares.
    pub(super) fn len(&self) -> usize {
        match self {
            Self::Ascii(text) => text.len() + 1,
            Self::Bytes(bytes) => bytes.len(),
            Self::Short(_) => 2,
            Self::Rational(pairs) => pairs.len() * 8,
            Self::SRational(_) => 8,
        }
    }
}

/// An IFD's size: its entry count, entries, next-IFD link and data area.
fn ifd_len(entries: &[Entry]) -> usize {
    2 + entries.len() * 12 + 4 + entries.iter().map(Entry::data_len).sum::<usize>()
}

/// The kept fields and the structural ones the export defines, by the IFD each belongs in.
struct Ifds {
    primary: Vec<Entry>,
    exif: Vec<Entry>,
    gps: Vec<Entry>,
}

pub(super) fn payload(fields: &[(usize, Value)], width: u32, height: u32) -> Vec<u8> {
    let Ifds {
        mut primary,
        exif,
        gps,
    } = ifds(fields, width, height);
    // The pointers are inline LONGs, so IFD0's size does not depend on their values.
    let pointers = if gps.is_empty() { 1 } else { 2 };
    let exif_at = 8 + ifd_len(&primary) + pointers * 12;
    let gps_at = exif_at + ifd_len(&exif);
    // Every field is bounded by the reader, so each offset fits a LONG.
    primary.push(Entry::long(EXIF_POINTER, exif_at as u32));
    if !gps.is_empty() {
        primary.push(Entry::long(GPS_POINTER, gps_at as u32));
    }
    let mut out = Vec::with_capacity(gps_at + if gps.is_empty() { 0 } else { ifd_len(&gps) });
    out.extend_from_slice(b"II*\0");
    out.extend_from_slice(&8_u32.to_le_bytes());
    write_ifd(&mut out, 0, primary);
    write_ifd(&mut out, 0, exif);
    if !gps.is_empty() {
        write_ifd(&mut out, 0, gps);
    }
    out
}

fn ifds(fields: &[(usize, Value)], width: u32, height: u32) -> Ifds {
    let mut primary = vec![
        Entry::short(ORIENTATION, 1),
        Entry::ascii(SOFTWARE, b"Luxforge"),
    ];
    let mut exif = vec![
        Entry {
            tag: EXIF_VERSION,
            kind: UNDEFINED,
            count: 4,
            data: b"0232".to_vec(),
        },
        // sRGB.
        Entry::short(COLOR_SPACE, 1),
        Entry::long(PIXEL_X_DIMENSION, width),
        Entry::long(PIXEL_Y_DIMENSION, height),
    ];
    let mut gps = Vec::new();
    for (index, value) in fields {
        let field = &FIELDS[*index];
        let ifd = match field.ifd {
            Ifd::Primary => &mut primary,
            Ifd::Exif => &mut exif,
            Ifd::Gps => &mut gps,
        };
        ifd.push(Entry::value(field.tag, value));
    }
    Ifds { primary, exif, gps }
}

#[cfg_attr(not(test), allow(dead_code))]
impl super::CaptureMetadata {
    /// A TIFF export's directories, to be written at file offset `base` (even), which is where
    /// IFD0 starts: IFD0 with `image`'s own tags, then, when `metadata` is given, the Exif IFD and,
    /// when it keeps any GPS field, the GPS IFD, each followed by its values longer than four
    /// bytes. IFD0 holds the image's structure (RGB, `image.bits_per_sample` per sample,
    /// uncompressed or Deflate with the horizontal predictor, its strips), its density
    /// (XResolution and YResolution at `pixels_per_inch` with ResolutionUnit inch, or 1/1 with
    /// no unit) and its ICC profile when not empty. `metadata` adds IFD0's descriptive fields, the
    /// structural fields [`Self::exif_payload`] writes (Orientation 1, Software, ExifVersion,
    /// ColorSpace sRGB and the pixel dimensions) and the Exif and GPS IFDs with their pointers;
    /// without it IFD0 holds no EXIF field and there is no other IFD. The caller keeps the whole
    /// file within 4 GiB, so every offset fits a LONG.
    pub(crate) fn tiff_directories(
        metadata: Option<&Self>,
        image: &crate::export::tiff::TiffImage<'_>,
        base: u32,
    ) -> Vec<u8> {
        // The TIFF export's own baseline tags (TIFF 6.0) and its ICC profile's (34675).
        const IMAGE_WIDTH: u16 = 256;
        const IMAGE_LENGTH: u16 = 257;
        const BITS_PER_SAMPLE: u16 = 258;
        const COMPRESSION: u16 = 259;
        const PHOTOMETRIC_INTERPRETATION: u16 = 262;
        const STRIP_OFFSETS: u16 = 273;
        const SAMPLES_PER_PIXEL: u16 = 277;
        const ROWS_PER_STRIP: u16 = 278;
        const STRIP_BYTE_COUNTS: u16 = 279;
        const X_RESOLUTION: u16 = 282;
        const Y_RESOLUTION: u16 = 283;
        const PLANAR_CONFIGURATION: u16 = 284;
        const RESOLUTION_UNIT: u16 = 296;
        const PREDICTOR: u16 = 317;
        const ICC_PROFILE: u16 = 34675;

        let samples = [image.bits_per_sample; 3];
        let (resolution, unit) = match image.pixels_per_inch {
            Some(ppi) => ([u32::from(ppi), 1], 2),
            None => ([1, 1], 1),
        };
        let mut primary = vec![
            Entry::long(IMAGE_WIDTH, image.width),
            Entry::long(IMAGE_LENGTH, image.height),
            Entry::shorts(BITS_PER_SAMPLE, &samples),
            // 8 is Adobe's Deflate, the code libtiff and every current reader use for it.
            Entry::short(COMPRESSION, if image.compressed { 8 } else { 1 }),
            // RGB.
            Entry::short(PHOTOMETRIC_INTERPRETATION, 2),
            Entry::longs(STRIP_OFFSETS, image.strip_offsets),
            Entry::short(SAMPLES_PER_PIXEL, 3),
            Entry::long(ROWS_PER_STRIP, image.rows_per_strip),
            Entry::longs(STRIP_BYTE_COUNTS, image.strip_byte_counts),
            Entry::rational(X_RESOLUTION, resolution),
            Entry::rational(Y_RESOLUTION, resolution),
            // Chunky: each pixel's samples together.
            Entry::short(PLANAR_CONFIGURATION, 1),
            Entry::short(RESOLUTION_UNIT, unit),
        ];
        if image.compressed {
            // Horizontal differencing.
            primary.push(Entry::short(PREDICTOR, 2));
        }
        if !image.icc.is_empty() {
            primary.push(Entry::undefined(ICC_PROFILE, image.icc));
        }
        let base = base as usize;
        let Some(metadata) = metadata else {
            let mut out = Vec::with_capacity(ifd_len(&primary));
            write_ifd(&mut out, base, primary);
            return out;
        };
        let Ifds {
            primary: kept,
            exif,
            gps,
        } = ifds(&metadata.fields, image.width, image.height);
        primary.extend(kept);
        let pointers = if gps.is_empty() { 1 } else { 2 };
        let exif_at = base + ifd_len(&primary) + pointers * 12;
        let gps_at = exif_at + ifd_len(&exif);
        primary.push(Entry::long(EXIF_POINTER, exif_at as u32));
        if !gps.is_empty() {
            primary.push(Entry::long(GPS_POINTER, gps_at as u32));
        }
        let mut out =
            Vec::with_capacity(gps_at - base + if gps.is_empty() { 0 } else { ifd_len(&gps) });
        write_ifd(&mut out, base, primary);
        write_ifd(&mut out, base, exif);
        if !gps.is_empty() {
            write_ifd(&mut out, base, gps);
        }
        out
    }
}

/// Append one IFD at `out`'s end, which is file offset `base + out.len()`, on a word boundary:
/// its entries sorted by tag, a zero next-IFD link, then the values longer than four bytes, each
/// padded to an even length.
fn write_ifd(out: &mut Vec<u8>, base: usize, mut entries: Vec<Entry>) {
    entries.sort_by_key(|entry| entry.tag);
    let data_at = base + out.len() + 2 + entries.len() * 12 + 4;
    let mut data = Vec::with_capacity(entries.iter().map(Entry::data_len).sum());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for entry in &entries {
        out.extend_from_slice(&entry.tag.to_le_bytes());
        out.extend_from_slice(&entry.kind.to_le_bytes());
        out.extend_from_slice(&entry.count.to_le_bytes());
        if entry.data.len() <= 4 {
            let mut inline = [0; 4];
            inline[..entry.data.len()].copy_from_slice(&entry.data);
            out.extend_from_slice(&inline);
        } else {
            out.extend_from_slice(&((data_at + data.len()) as u32).to_le_bytes());
            data.extend_from_slice(&entry.data);
            if data.len() % 2 == 1 {
                data.push(0);
            }
        }
    }
    // No next IFD: the export carries no IFD1, so no thumbnail.
    out.extend_from_slice(&0_u32.to_le_bytes());
    out.extend_from_slice(&data);
}
