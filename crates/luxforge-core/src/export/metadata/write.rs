//! The export's EXIF writer: a little-endian TIFF of IFD0, the Exif IFD and an optional GPS IFD,
//! entries sorted by tag, values longer than four bytes in each IFD's data area on word
//! boundaries, and no IFD1.

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

pub(super) fn payload(fields: &[(usize, Value)], width: u32, height: u32) -> Vec<u8> {
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
    write_ifd(&mut out, primary);
    write_ifd(&mut out, exif);
    if !gps.is_empty() {
        write_ifd(&mut out, gps);
    }
    out
}

/// Append one IFD at `out`'s end, which is on a word boundary: its entries sorted by tag, a zero
/// next-IFD link, then the values longer than four bytes, each padded to an even length.
fn write_ifd(out: &mut Vec<u8>, mut entries: Vec<Entry>) {
    entries.sort_by_key(|entry| entry.tag);
    let data_at = out.len() + 2 + entries.len() * 12 + 4;
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
