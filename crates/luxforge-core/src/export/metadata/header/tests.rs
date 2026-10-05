//! The header reader over containers built here: one of each kind, structures past the head and
//! past the budget, the typed values' edge cases, and truncated and corrupted inputs. Every read
//! is made twice, bounded (through a cursor) and whole, and the two agree whenever the bounded read
//! was not capped.

use super::fields::{parse_offset, parse_subsec};
use super::{
    CHUNK_BYTES, CaptureTime, Container, DateTime, Dimensions, FileHeader, GpsPosition, GpsTime,
    HEADER_HEAD_BYTES, HeaderRead, MAX_HEADER_BYTES, MAX_HEADER_READS, MAX_THUMBNAIL_JPEG_BYTES,
    Rational, SignedRational, Thumbnail, ThumbnailFormat, TimeSource, read_header,
};
use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::path::Path;

// ---------------------------------------------------------------------------------------------
// Reading.

/// Read `bytes` bounded and whole; they agree unless the bounded read was capped.
fn read(bytes: &[u8]) -> HeaderRead {
    let bounded = read_header(&mut Cursor::new(bytes), bytes.len() as u64).expect("a cursor");
    assert!(bounded.bytes_read <= MAX_HEADER_BYTES as u64);
    assert!(bounded.reads as usize <= MAX_HEADER_READS);
    if !bounded.capped {
        assert_eq!(bounded.header, FileHeader::from_bytes(bytes));
    }
    bounded
}

/// The header of `bytes`, which the budget must not cap.
fn header(bytes: &[u8]) -> FileHeader {
    let read = read(bytes);
    assert!(!read.capped);
    read.header
}

// ---------------------------------------------------------------------------------------------
// Building containers.

/// An IFD entry's value.
#[derive(Clone, Debug)]
enum V {
    /// ASCII with its NUL.
    Ascii(String),
    /// Bytes of a one-byte type (BYTE, ASCII, UNDEFINED), as given.
    Raw(u16, Vec<u8>),
    Short(Vec<u16>),
    Long(Vec<u32>),
    /// One IFD offset, type 13.
    Ifd(u32),
    Rational(Vec<(u32, u32)>),
    SRational(Vec<(i32, i32)>),
    /// A type, count and offset written as given, with no data: a pointer at bytes placed
    /// elsewhere.
    At(u16, u32, u32),
}

fn ascii(text: &str) -> V {
    V::Ascii(text.to_owned())
}

fn short(value: u16) -> V {
    V::Short(vec![value])
}

fn long(value: u32) -> V {
    V::Long(vec![value])
}

fn rational(num: u32, den: u32) -> V {
    V::Rational(vec![(num, den)])
}

/// A TIFF structure written at chosen offsets, in one byte order.
struct Builder {
    bytes: Vec<u8>,
    big_endian: bool,
}

impl Builder {
    /// A file starting with `magic` and IFD0's offset.
    fn new(magic: &[u8; 4], ifd0: u32) -> Self {
        let mut builder = Self {
            bytes: Vec::new(),
            big_endian: magic[0] == b'M',
        };
        builder.put(0, magic);
        builder.put_u32(4, ifd0);
        builder
    }

    /// An empty buffer in a byte order, for a TIFF placed inside another container.
    fn raw(big_endian: bool) -> Self {
        Self {
            bytes: Vec::new(),
            big_endian,
        }
    }

    fn put(&mut self, at: u32, data: &[u8]) {
        let at = at as usize;
        if self.bytes.len() < at + data.len() {
            self.bytes.resize(at + data.len(), 0);
        }
        self.bytes[at..at + data.len()].copy_from_slice(data);
    }

    fn u16(&self, value: u16) -> [u8; 2] {
        if self.big_endian {
            value.to_be_bytes()
        } else {
            value.to_le_bytes()
        }
    }

    fn u32(&self, value: u32) -> [u8; 4] {
        if self.big_endian {
            value.to_be_bytes()
        } else {
            value.to_le_bytes()
        }
    }

    fn put_u16(&mut self, at: u32, value: u16) {
        let bytes = self.u16(value);
        self.put(at, &bytes);
    }

    fn put_u32(&mut self, at: u32, value: u32) {
        let bytes = self.u32(value);
        self.put(at, &bytes);
    }

    fn encode(&self, value: &V) -> (u16, u32, Vec<u8>) {
        match value {
            V::Ascii(text) => {
                let mut data = text.as_bytes().to_vec();
                data.push(0);
                (2, data.len() as u32, data)
            }
            V::Raw(kind, data) => (*kind, data.len() as u32, data.clone()),
            V::Short(values) => (
                3,
                values.len() as u32,
                values.iter().flat_map(|v| self.u16(*v)).collect(),
            ),
            V::Long(values) => (
                4,
                values.len() as u32,
                values.iter().flat_map(|v| self.u32(*v)).collect(),
            ),
            V::Ifd(offset) => (13, 1, self.u32(*offset).to_vec()),
            V::Rational(values) => (
                5,
                values.len() as u32,
                values
                    .iter()
                    .flat_map(|(num, den)| [self.u32(*num), self.u32(*den)])
                    .flatten()
                    .collect(),
            ),
            V::SRational(values) => (
                10,
                values.len() as u32,
                values
                    .iter()
                    .flat_map(|(num, den)| {
                        [self.u32(num.cast_unsigned()), self.u32(den.cast_unsigned())]
                    })
                    .flatten()
                    .collect(),
            ),
            V::At(kind, count, _) => (*kind, *count, Vec::new()),
        }
    }

    /// Write an IFD at `base + offset` whose value offsets are relative to `base`: its entries in
    /// the order given, a link to `next`, and the values longer than four bytes right after the
    /// table. Returns where its data ends, relative to `base`.
    fn ifd(&mut self, base: u32, offset: u32, entries: &[(u16, V)], next: u32) -> u32 {
        let table = offset + 2;
        let mut data_at = table + entries.len() as u32 * 12 + 4;
        self.put_u16(base + offset, entries.len() as u16);
        for (index, (tag, value)) in entries.iter().enumerate() {
            let at = base + table + index as u32 * 12;
            let (kind, count, data) = self.encode(value);
            self.put_u16(at, *tag);
            self.put_u16(at + 2, kind);
            self.put_u32(at + 4, count);
            if let V::At(_, _, offset) = value {
                self.put_u32(at + 8, *offset);
            } else if data.len() <= 4 {
                let mut inline = [0; 4];
                inline[..data.len()].copy_from_slice(&data);
                self.put(at + 8, &inline);
            } else {
                self.put_u32(at + 8, data_at);
                self.put(base + data_at, &data);
                data_at += (data.len() as u32).next_multiple_of(2);
            }
        }
        self.put_u32(base + table + entries.len() as u32 * 12, next);
        data_at
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

/// A small JPEG stream, as thumbnails are.
const THUMB: [u8; 12] = [
    0xff, 0xd8, 0xff, 0xdb, 0x00, 0x04, 0x00, 0x00, 0xff, 0xd9, 0x00, 0x00,
];

fn segment(out: &mut Vec<u8>, marker: u8, payload: &[u8]) {
    out.extend_from_slice(&[0xff, marker]);
    out.extend_from_slice(&u16::try_from(payload.len() + 2).unwrap().to_be_bytes());
    out.extend_from_slice(payload);
}

/// Where a JPEG built by [`jpeg`] starts its EXIF TIFF: after SOI, a JFIF APP0 and an XMP APP1.
const JPEG_TIFF_AT: u64 = 2 + (4 + 14) + (4 + 41) + 10;

/// A JPEG whose `Exif` APP1 (after a JFIF APP0 and an XMP APP1) holds `tiff`, followed by an ICC
/// APP2, a quantization table, the frame header and a scan.
fn jpeg(tiff: &[u8], width: u16, height: u16) -> Vec<u8> {
    let mut out = vec![0xff, 0xd8];
    segment(&mut out, 0xe0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
    segment(
        &mut out,
        0xe1,
        b"http://ns.adobe.com/xap/1.0/\0<x:xmpmeta/>",
    );
    segment(&mut out, 0xe1, &[b"Exif\0\0".as_slice(), tiff].concat());
    segment(
        &mut out,
        0xe2,
        &[b"ICC_PROFILE\0\x01\x01".as_slice(), &[0; 64]].concat(),
    );
    segment(&mut out, 0xdb, &[0; 65]);
    let mut frame = vec![8];
    frame.extend_from_slice(&height.to_be_bytes());
    frame.extend_from_slice(&width.to_be_bytes());
    frame.extend_from_slice(&[3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
    segment(&mut out, 0xc0, &frame);
    segment(&mut out, 0xda, &[3, 1, 0, 2, 0x11, 3, 0x11, 0, 0x3f, 0]);
    out.extend_from_slice(&[0x12, 0x34, 0xff, 0x00, 0x56, 0xff, 0xd9]);
    out
}

// The tags the tests write.
const MAKE: u16 = 0x010f;
const MODEL: u16 = 0x0110;
const ORIENTATION: u16 = 0x0112;
const DATE_TIME: u16 = 0x0132;
const EXIF: u16 = 0x8769;
const GPS: u16 = 0x8825;
const SUB_IFDS: u16 = 0x014a;
const MAKER_NOTE: u16 = 0x927c;

/// A camera's EXIF TIFF: IFD0 at 8, the Exif IFD at 0x200, the GPS IFD at 0x500 and IFD1 at 0x700
/// naming [`THUMB`] at 0x780.
fn camera_exif(big_endian: bool) -> Vec<u8> {
    let mut tiff = Builder::new(if big_endian { b"MM\0*" } else { b"II*\0" }, 8);
    tiff.ifd(
        0,
        8,
        &[
            (MAKE, ascii("NIKON CORPORATION")),
            (MODEL, ascii("NIKON Z 6  ")),
            (ORIENTATION, short(6)),
            (DATE_TIME, ascii("2026:09:28 10:00:00")),
            (EXIF, long(0x200)),
            (GPS, long(0x500)),
        ],
        0x700,
    );
    tiff.ifd(
        0,
        0x200,
        &[
            (0x829a, rational(1, 250)),
            (0x829d, rational(28, 10)),
            (0x8827, short(400)),
            (0x9003, ascii("2026:09:27 18:04:05")),
            (0x9004, ascii("2026:09:27 18:04:06")),
            (0x9011, ascii("+01:00")),
            (0x9012, ascii("-02:30")),
            (0x9291, ascii("42")),
            (0x9204, V::SRational(vec![(-2, 3)])),
            (0x920a, rational(350, 10)),
            (0xa405, short(35)),
            (0xa431, ascii("6012345")),
            (0xa433, ascii("Nikon")),
            (0xa434, ascii("NIKKOR Z 24-70mm f/4 S")),
        ],
        0,
    );
    tiff.ifd(
        0,
        0x500,
        &[
            (0x0000, V::Raw(1, vec![2, 3, 0, 0])),
            (0x0001, ascii("N")),
            (0x0002, V::Rational(vec![(51, 1), (30, 1), (1234, 100)])),
            (0x0003, ascii("W")),
            (0x0004, V::Rational(vec![(0, 1), (7, 1), (3900, 100)])),
            (0x0005, V::Raw(1, vec![0])),
            (0x0006, rational(355, 10)),
            (0x0007, V::Rational(vec![(17, 1), (4, 1), (55, 10)])),
            (0x001d, ascii("2026:09:27")),
        ],
        0,
    );
    tiff.ifd(
        0,
        0x700,
        &[
            (0x0103, short(6)),
            (0x0201, long(0x780)),
            (0x0202, long(THUMB.len() as u32)),
        ],
        0,
    );
    tiff.put(0x780, &THUMB);
    tiff.finish()
}

/// A camera's JPEG with [`camera_exif`]'s header — a Nikon Z 6, a capture time with its subsecond
/// and offset, a GPS position and a thumbnail — for the index's tests.
pub(crate) fn camera_jpeg() -> Vec<u8> {
    jpeg(&camera_exif(false), 6048, 4024)
}

fn date_time(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> DateTime {
    DateTime::new(year, month, day, hour, minute, second).unwrap()
}

fn degrees(parts: [(u32, u32); 3]) -> f64 {
    parts
        .iter()
        .zip([1.0, 60.0, 3600.0])
        .map(|((num, den), scale)| {
            Rational {
                num: *num,
                den: *den,
            }
            .value()
                / scale
        })
        .sum()
}

/// What [`camera_exif`] holds, with its thumbnail at `thumbnail` and the given container, size.
fn camera_header(
    container: Container,
    thumbnail: u64,
    dimensions: Option<Dimensions>,
) -> FileHeader {
    FileHeader {
        container,
        capture_time: Some(CaptureTime {
            local: date_time(2026, 9, 27, 18, 4, 5),
            subsec_nanos: Some(420_000_000),
            subsec_digits: 2,
            offset_minutes: Some(60),
            source: TimeSource::Original,
        }),
        gps_time: Some(GpsTime {
            utc: date_time(2026, 9, 27, 17, 4, 5),
            nanos: 500_000_000,
        }),
        make: Some("NIKON CORPORATION".into()),
        model: Some("NIKON Z 6".into()),
        body_serial: Some("6012345".into()),
        lens_make: Some("Nikon".into()),
        lens_model: Some("NIKKOR Z 24-70mm f/4 S".into()),
        exposure_time: Some(Rational { num: 1, den: 250 }),
        f_number: Some(Rational { num: 28, den: 10 }),
        iso: Some(400),
        exposure_bias: Some(SignedRational { num: -2, den: 3 }),
        focal_length: Some(Rational { num: 350, den: 10 }),
        focal_length_35mm: Some(35),
        gps: Some(GpsPosition {
            latitude: degrees([(51, 1), (30, 1), (1234, 100)]),
            longitude: -degrees([(0, 1), (7, 1), (3900, 100)]),
            altitude: Some(35.5),
        }),
        dimensions,
        orientation: Some(6),
        thumbnail: Some(Thumbnail {
            offset: thumbnail,
            length: THUMB.len() as u64,
            format: ThumbnailFormat::Jpeg { size: None },
        }),
    }
}

// ---------------------------------------------------------------------------------------------
// One container of each kind.

#[test]
fn metadata_header_of_a_jpeg_reads_its_exif_frame_and_thumbnail() {
    for big_endian in [false, true] {
        let bytes = jpeg(&camera_exif(big_endian), 6048, 4024);
        let read = read(&bytes);
        assert_eq!(read.reads, 1);
        assert_eq!(read.bytes_read, bytes.len() as u64);
        let expected = camera_header(
            Container::Jpeg,
            JPEG_TIFF_AT + 0x780,
            Some(Dimensions {
                width: 6048,
                height: 4024,
            }),
        );
        assert_eq!(read.header, expected);
        assert_eq!(&bytes[JPEG_TIFF_AT as usize + 0x780..][..2], [0xff, 0xd8]);
        assert_eq!(read.header.exposure_seconds(), Some(0.004));
        assert_eq!(
            read.header.exposure_bias.map(SignedRational::value),
            Some(-2.0 / 3.0)
        );
    }
}

#[test]
fn metadata_header_matches_what_an_independent_writer_wrote() {
    use exif::experimental::Writer;
    use exif::{Field, In, Rational as ExifRational, SRational as ExifSRational, Tag, Value};
    let text = |text: &str| Value::Ascii(vec![text.as_bytes().to_vec()]);
    let fields = [
        (Tag::Make, text("FUJIFILM")),
        (Tag::Model, text("X100VI")),
        (Tag::Orientation, Value::Short(vec![8])),
        (Tag::DateTimeOriginal, text("2026:08:07 07:11:49")),
        (Tag::SubSecTimeOriginal, text("73")),
        (Tag::OffsetTimeOriginal, text("+01:00")),
        (
            Tag::ExposureTime,
            Value::Rational(vec![ExifRational {
                num: 10,
                denom: 520,
            }]),
        ),
        (
            Tag::FNumber,
            Value::Rational(vec![ExifRational {
                num: 200,
                denom: 100,
            }]),
        ),
        (Tag::PhotographicSensitivity, Value::Short(vec![125])),
        (
            Tag::ExposureBiasValue,
            Value::SRational(vec![ExifSRational { num: 0, denom: 100 }]),
        ),
        (
            Tag::FocalLength,
            Value::Rational(vec![ExifRational {
                num: 2300,
                denom: 100,
            }]),
        ),
        (Tag::BodySerialNumber, text("6B018117")),
        (Tag::GPSLatitudeRef, text("S")),
        (
            Tag::GPSLatitude,
            Value::Rational(vec![
                ExifRational { num: 33, denom: 1 },
                ExifRational { num: 52, denom: 1 },
                ExifRational { num: 0, denom: 1 },
            ]),
        ),
        (Tag::GPSLongitudeRef, text("E")),
        (
            Tag::GPSLongitude,
            Value::Rational(vec![
                ExifRational { num: 151, denom: 1 },
                ExifRational { num: 12, denom: 1 },
                ExifRational { num: 30, denom: 1 },
            ]),
        ),
    ];
    for little_endian in [true, false] {
        let fields: Vec<Field> = fields
            .iter()
            .map(|(tag, value)| Field {
                tag: *tag,
                ifd_num: In::PRIMARY,
                value: value.clone(),
            })
            .collect();
        let mut writer = Writer::new();
        for field in &fields {
            writer.push_field(field);
        }
        let orientation = Field {
            tag: Tag::Orientation,
            ifd_num: In::THUMBNAIL,
            value: Value::Short(vec![1]),
        };
        writer.push_field(&orientation);
        writer.set_jpeg(&THUMB, In::THUMBNAIL);
        let mut out = Cursor::new(Vec::new());
        writer.write(&mut out, little_endian).unwrap();
        let bytes = out.into_inner();
        let header = header(&bytes);
        let local = header.capture_time.unwrap();
        assert_eq!(local.local, date_time(2026, 8, 7, 7, 11, 49));
        assert_eq!(
            (local.subsec_nanos, local.offset_minutes),
            (Some(730_000_000), Some(60))
        );
        assert_eq!(header.make.as_deref(), Some("FUJIFILM"));
        assert_eq!(header.orientation, Some(8));
        assert_eq!(header.iso, Some(125));
        assert_eq!(header.exposure_time, Some(Rational { num: 10, den: 520 }));
        assert_eq!(
            header.exposure_bias,
            Some(SignedRational { num: 0, den: 100 })
        );
        assert_eq!(header.body_serial.as_deref(), Some("6B018117"));
        let gps = header.gps.unwrap();
        assert_eq!(gps.latitude, -degrees([(33, 1), (52, 1), (0, 1)]));
        assert_eq!(gps.longitude, degrees([(151, 1), (12, 1), (30, 1)]));
        let thumbnail = header.thumbnail.unwrap();
        let at = thumbnail.offset as usize;
        assert_eq!(&bytes[at..at + thumbnail.length as usize], THUMB);
    }
}

/// A DNG-like TIFF: IFD0 a 160×120 RGB thumbnail, SubIFDs for the raw image (with its default
/// crop) and, when `preview`, a 960×640 JPEG preview; the Exif IFD at `exif_at`.
fn dng(exif_at: u32, preview: bool) -> Vec<u8> {
    let mut tiff = Builder::new(b"II*\0", 8);
    let sub_ifds = if preview {
        vec![0x300, 0x400]
    } else {
        vec![0x300]
    };
    tiff.ifd(
        0,
        8,
        &[
            (0x00fe, long(1)),
            (0x0100, long(160)),
            (0x0101, long(120)),
            (0x0102, V::Short(vec![8, 8, 8])),
            (0x0103, short(1)),
            (0x0106, short(2)),
            (MAKE, ascii("DJI")),
            (MODEL, ascii("FC3411")),
            (0x0111, long(0x1000)),
            (ORIENTATION, short(1)),
            (0x0115, short(3)),
            (0x0117, long(160 * 120 * 3)),
            (SUB_IFDS, V::Long(sub_ifds)),
            (EXIF, long(exif_at)),
            (0xc62f, ascii("42UQJ1W13A02NY")),
        ],
        0,
    );
    tiff.ifd(
        0,
        0x300,
        &[
            (0x00fe, long(0)),
            (0x0100, long(5568)),
            (0x0101, long(3648)),
            (0x0102, short(16)),
            (0x0103, short(1)),
            (0x0106, short(32803)),
            (0x0111, long(0x2_0000)),
            (0x0115, short(1)),
            (0x0117, long(4096)),
            (0xc620, V::Rational(vec![(5464, 1), (3640, 1)])),
        ],
        0,
    );
    if preview {
        tiff.ifd(
            0,
            0x400,
            &[
                (0x00fe, long(1)),
                (0x0100, long(960)),
                (0x0101, long(640)),
                (0x0102, V::Short(vec![8, 8, 8])),
                (0x0103, short(7)),
                (0x0106, short(6)),
                (0x0111, long(0xf200)),
                (0x0115, short(3)),
                (0x0117, long(THUMB.len() as u32)),
            ],
            0,
        );
        tiff.put(0xf200, &THUMB);
    }
    tiff.put(0x1000, &[0x80; 160 * 120 * 3]);
    tiff.ifd(
        0,
        exif_at,
        &[
            (0x829a, rational(1, 2500)),
            (0x9003, ascii("2025:04:04 10:38:18")),
            (0xa431, ascii("42UQJ1W13A02NY")),
        ],
        0,
    );
    // The raw strip, and room for a whole chunk after a far Exif IFD.
    tiff.put(0x2_0000 + 4095, &[0]);
    if exif_at > 0x2_0000 {
        tiff.put(exif_at + 8191, &[0]);
    }
    tiff.finish()
}

#[test]
fn metadata_header_of_a_tiff_raw_reads_the_raw_size_its_crop_and_the_smallest_thumbnail() {
    let bytes = dng(0x800, true);
    let read = read(&bytes);
    let header = &read.header;
    assert_eq!(header.container, Container::Tiff);
    assert_eq!(
        header.dimensions,
        Some(Dimensions {
            width: 5464,
            height: 3640
        })
    );
    assert_eq!(header.body_serial.as_deref(), Some("42UQJ1W13A02NY"));
    assert_eq!(header.exposure_time, Some(Rational { num: 1, den: 2500 }));
    // The JPEG preview is preferred to the RGB thumbnail.
    assert_eq!(
        header.thumbnail,
        Some(Thumbnail {
            offset: 0xf200,
            length: THUMB.len() as u64,
            format: ThumbnailFormat::Jpeg {
                size: Some(Dimensions {
                    width: 960,
                    height: 640
                })
            },
        })
    );
    // Without it, the RGB strip.
    let rgb = header_of(&dng(0x800, false)).thumbnail;
    assert_eq!(
        rgb,
        Some(Thumbnail {
            offset: 0x1000,
            length: 160 * 120 * 3,
            format: ThumbnailFormat::Rgb8 {
                size: Dimensions {
                    width: 160,
                    height: 120
                }
            },
        })
    );
    // Everything but the raw strip lies inside the head: one read.
    assert_eq!(read.reads, 1);
    assert_eq!(read.bytes_read, HEADER_HEAD_BYTES as u64);
}

fn header_of(bytes: &[u8]) -> FileHeader {
    header(bytes)
}

/// A CR2-like TIFF: IFD0 an 8-bit JPEG preview with no NewSubfileType, IFD1 the thumbnail, IFD2 a
/// small 16-bit RGB image, IFD3 the raw data, declaring its size when `raw_size`; the Exif IFD
/// states the image size.
fn cr2(raw_size: bool) -> Vec<u8> {
    let mut tiff = Builder::new(b"II*\0", 16);
    tiff.put(8, b"CR\x02\0");
    tiff.ifd(
        0,
        16,
        &[
            (0x0100, short(5184)),
            (0x0101, short(3456)),
            (0x0102, V::Short(vec![8, 8, 8])),
            (0x0103, short(6)),
            (MAKE, ascii("Canon")),
            (0x0111, long(0x4000)),
            (0x0117, long(1_000_000)),
            (EXIF, long(0x200)),
        ],
        0x400,
    );
    tiff.ifd(0, 0x200, &[(0xa002, short(6000)), (0xa003, short(4000))], 0);
    tiff.ifd(
        0,
        0x400,
        &[(0x0201, long(0x3000)), (0x0202, long(THUMB.len() as u32))],
        0x500,
    );
    tiff.put(0x3000, &THUMB);
    tiff.ifd(
        0,
        0x500,
        &[
            (0x0100, short(600)),
            (0x0101, short(400)),
            (0x0102, V::Short(vec![16, 16, 16])),
            (0x0103, short(1)),
            (0x0106, short(2)),
            (0x0111, long(0x10_0000)),
            (0x0115, short(3)),
            (0x0117, long(1_440_000)),
        ],
        0x600,
    );
    let mut raw = vec![
        (0x0103, short(6)),
        (0x0111, long(0x20_0000)),
        (0x0117, long(1000)),
    ];
    if raw_size {
        raw.extend([(0x0100, short(6288)), (0x0101, short(4056))]);
    }
    tiff.ifd(0, 0x600, &raw, 0);
    tiff.finish()
}

#[test]
fn metadata_header_raw_size_is_never_a_previews() {
    let declared = header(&cr2(true));
    assert_eq!(
        declared.dimensions,
        Some(Dimensions {
            width: 6288,
            height: 4056
        })
    );
    // Without it, not the JPEG preview's nor the RGB image's, but the size the Exif IFD states.
    let stated = header(&cr2(false));
    assert_eq!(
        stated.dimensions,
        Some(Dimensions {
            width: 6000,
            height: 4000
        })
    );
    assert_eq!(
        stated.thumbnail.map(|thumbnail| thumbnail.offset),
        Some(0x3000)
    );
}

#[test]
fn metadata_header_reads_structures_the_container_names_past_the_head() {
    let exif_at = 200_000;
    let bytes = dng(exif_at, true);
    let read = read(&bytes);
    assert_eq!(
        read.header.exposure_time,
        Some(Rational { num: 1, den: 2500 })
    );
    assert_eq!(
        read.header.capture_time.map(|time| time.local),
        Some(date_time(2025, 4, 4, 10, 38, 18))
    );
    // The head, then one chunk around the Exif IFD and its values.
    assert_eq!(read.reads, 2);
    assert_eq!(read.bytes_read, (HEADER_HEAD_BYTES + CHUNK_BYTES) as u64);
    assert!(!read.capped);
    // A table across the head's end is read again whole, in the aligned chunks around it.
    let read = super::tests::read(&dng(HEADER_HEAD_BYTES as u32 - 6, true));
    assert_eq!(
        read.header.exposure_time,
        Some(Rational { num: 1, den: 2500 })
    );
    assert_eq!(
        (read.reads, read.bytes_read),
        (2, (HEADER_HEAD_BYTES + 2 * CHUNK_BYTES) as u64)
    );
}

#[test]
fn metadata_header_past_the_budget_is_partial_and_says_so() {
    // Every text value at its own far offset, each needing a read of its own: IFD0's four, then the
    // Exif IFD and its eleven, then the GPS IFD.
    let far = |index: u32| 100_000 + index * 8192;
    let text = |index: u32, count: u32| V::At(2, count, far(index));
    let mut tiff = Builder::new(b"II*\0", 8);
    tiff.ifd(
        0,
        8,
        &[
            (MAKE, text(0, 16)),
            (MODEL, text(1, 16)),
            (DATE_TIME, text(2, 20)),
            (0xc62f, text(3, 16)),
            (EXIF, long(far(4))),
            (GPS, long(far(5))),
        ],
        0,
    );
    let exif: Vec<(u16, V)> = [
        0x9003, 0x9004, 0x9010, 0x9011, 0x9012, 0x9290, 0x9291, 0x9292, 0xa431, 0xa433, 0xa434,
    ]
    .into_iter()
    .zip(6..)
    .map(|(tag, index)| (tag, text(index, 16)))
    .collect();
    tiff.ifd(0, far(4), &exif, 0);
    tiff.ifd(
        0,
        far(5),
        &[(0x0001, ascii("N")), (0x001d, text(17, 11))],
        0,
    );
    for index in (0..4).chain(6..18) {
        tiff.put(far(index), b"Camera 15 bytes\0");
    }
    let bytes = tiff.finish();
    let read = read(&bytes);
    let whole = FileHeader::from_bytes(&bytes);
    assert!(read.capped);
    assert_eq!(read.reads as usize, MAX_HEADER_READS);
    assert_eq!(
        read.bytes_read,
        (HEADER_HEAD_BYTES + 15 * CHUNK_BYTES) as u64
    );
    // What was read before the budget ran out is kept; the rest only the whole file has.
    assert_eq!(read.header.make.as_deref(), Some("Camera 15 bytes"));
    assert_eq!(read.header.make, whole.make);
    assert_eq!(read.header.lens_make, whole.lens_make);
    assert_eq!(read.header.lens_model, None);
    assert_eq!(whole.lens_model.as_deref(), Some("Camera 15 bytes"));
}

/// An ORF: IFD0 is the raw image, the Exif IFD holds an Olympus maker note with its Equipment
/// serial and thumbnail.
fn orf(magic: &[u8; 4]) -> Vec<u8> {
    let mut tiff = Builder::new(magic, 8);
    let note_at = 0x400;
    tiff.ifd(
        0,
        8,
        &[
            (0x0100, long(5240)),
            (0x0101, long(3912)),
            (MAKE, ascii("OLYMPUS CORPORATION    ")),
            (MODEL, ascii("E-M1X           ")),
            (ORIENTATION, short(1)),
            (EXIF, long(0x100)),
        ],
        0,
    );
    let mut note = Builder::raw(magic[0] == b'M');
    note.put(0, b"OLYMPUS\0");
    note.put(8, if magic[0] == b'M' { b"MM" } else { b"II" });
    note.put(10, &[3, 0]);
    let end = note.ifd(
        0,
        12,
        &[(0x0100, V::Raw(7, THUMB.to_vec())), (0x2010, V::Ifd(0x100))],
        0,
    );
    assert!(end <= 0x100);
    note.ifd(
        0,
        0x100,
        &[(0x0101, ascii("BJ4A05289                      "))],
        0,
    );
    let note = note.finish();
    tiff.put(note_at, &note);
    tiff.ifd(
        0,
        0x100,
        &[
            (0x9003, ascii("2019:03:24 18:32:12")),
            (0x9011, ascii("-05:00")),
            (MAKER_NOTE, V::At(7, note.len() as u32, note_at)),
        ],
        0,
    );
    tiff.finish()
}

#[test]
fn metadata_header_of_an_orf_reads_its_tiff_and_olympus_maker_note() {
    for magic in [b"IIRO", b"IIRS", b"MMOR"] {
        let header = header(&orf(magic));
        assert_eq!(header.container, Container::Orf);
        assert_eq!(header.make.as_deref(), Some("OLYMPUS CORPORATION"));
        assert_eq!(header.model.as_deref(), Some("E-M1X"));
        assert_eq!(header.body_serial.as_deref(), Some("BJ4A05289"));
        assert_eq!(
            header.dimensions,
            Some(Dimensions {
                width: 5240,
                height: 3912
            })
        );
        let time = header.capture_time.unwrap();
        assert_eq!((time.offset_minutes, time.subsec_nanos), (Some(-300), None));
        let thumbnail = header.thumbnail.unwrap();
        assert_eq!(thumbnail.length, THUMB.len() as u64);
        assert_eq!(thumbnail.format, ThumbnailFormat::Jpeg { size: None });
    }
    // The standard magic's walk does not take ORF's, nor the export's.
    let mut other = orf(b"IIRO");
    other[2..4].copy_from_slice(b"XX");
    assert_eq!(header(&other).container, Container::Unknown);
    assert!(
        super::super::CaptureMetadata::from_raw(&orf(b"IIRO"))
            .field_names()
            .is_empty()
    );
}

/// A RW2: IFD0 with Panasonic's sensor borders and ISO, an Exif IFD without ISO, and the
/// embedded JPEG whose EXIF has the rest and a thumbnail.
fn rw2(embedded_iso: Option<u16>) -> (Vec<u8>, u64) {
    let mut exif = Builder::new(b"II*\0", 8);
    let mut entries = vec![(0x9003, ascii("2017:06:13 16:06:33")), (0xa405, short(56))];
    if let Some(iso) = embedded_iso {
        entries.push((0x8827, short(iso)));
    }
    exif.ifd(
        0,
        8,
        &[(MAKE, ascii("Panasonic")), (EXIF, long(0x100))],
        0x300,
    );
    exif.ifd(0, 0x100, &entries, 0);
    exif.ifd(
        0,
        0x300,
        &[(0x0201, long(0x380)), (0x0202, long(THUMB.len() as u32))],
        0,
    );
    exif.put(0x380, &THUMB);
    let embedded = jpeg(&exif.finish(), 1920, 1440);
    let jpeg_at = 0x1000;
    let mut tiff = Builder::new(b"IIU\0", 0x18);
    tiff.ifd(
        0,
        0x18,
        &[
            (0x0002, short(5264)),
            (0x0003, short(3904)),
            (0x0004, short(8)),
            (0x0005, short(12)),
            (0x0006, short(3896)),
            (0x0007, short(5196)),
            (0x0017, short(200)),
            (0x002e, V::At(7, embedded.len() as u32, jpeg_at)),
            (MAKE, ascii("Panasonic")),
            (MODEL, ascii("DC-GH5")),
            (ORIENTATION, short(1)),
            (EXIF, long(0x400)),
        ],
        0,
    );
    tiff.ifd(
        0,
        0x400,
        &[
            (0x9003, ascii("2017:06:13 16:06:33")),
            (0x9291, ascii("877")),
            (0x9011, ascii("+02:00")),
        ],
        0,
    );
    tiff.put(jpeg_at, &embedded);
    (tiff.finish(), u64::from(jpeg_at) + JPEG_TIFF_AT + 0x380)
}

#[test]
fn metadata_header_of_a_rw2_reads_its_borders_iso_and_embedded_jpeg() {
    let (bytes, thumbnail) = rw2(Some(400));
    let header = header(&bytes);
    assert_eq!(header.container, Container::Rw2);
    assert_eq!(
        header.dimensions,
        Some(Dimensions {
            width: 5184,
            height: 3888
        })
    );
    // The embedded JPEG's PhotographicSensitivity comes before Panasonic's own ISO.
    assert_eq!(header.iso, Some(400));
    assert_eq!(header.focal_length_35mm, Some(56));
    let time = header.capture_time.unwrap();
    assert_eq!(
        (time.subsec_nanos, time.offset_minutes),
        (Some(877_000_000), Some(120))
    );
    assert_eq!(
        header.thumbnail.map(|thumbnail| thumbnail.offset),
        Some(thumbnail)
    );
    // Without it, Panasonic's.
    assert_eq!(header_of(&rw2(None).0).iso, Some(200));
}

/// A RAF: its header naming the embedded JPEG (EXIF with a Fujifilm maker note serial and a
/// thumbnail), sensor bytes, then its directory far past the head.
fn raf() -> (Vec<u8>, u64) {
    let mut exif = Builder::new(b"II*\0", 8);
    exif.ifd(
        0,
        8,
        &[
            (MAKE, ascii("FUJIFILM")),
            (MODEL, ascii("X100VI")),
            (ORIENTATION, short(8)),
            (EXIF, long(0x100)),
        ],
        0x400,
    );
    let mut note = Builder::raw(false);
    note.put(0, b"FUJIFILM");
    note.put_u32(8, 12);
    note.ifd(0, 12, &[(0x0010, ascii("FF02B7859149     5935373131"))], 0);
    let note = note.finish();
    exif.ifd(
        0,
        0x100,
        &[
            (0x9003, ascii("2026:08:07 07:11:49")),
            (MAKER_NOTE, V::At(7, note.len() as u32, 0x200)),
        ],
        0,
    );
    exif.put(0x200, &note);
    exif.ifd(
        0,
        0x400,
        &[(0x0201, long(0x480)), (0x0202, long(THUMB.len() as u32))],
        0,
    );
    exif.put(0x480, &THUMB);
    let embedded = jpeg(&exif.finish(), 1920, 1280);
    let mut out = b"FUJIFILMCCD-RAW 0201FF383501".to_vec();
    out.resize(148, 0);
    let jpeg_at = out.len() as u32;
    out.extend_from_slice(&embedded);
    out.resize(150_000, 0xab);
    let directory = out.len() as u32;
    let mut records = Vec::new();
    for (tag, data) in [
        (0x0100_u16, [0x14, 0x4c, 0x1e, 0xc0]),
        (0x0110, [0, 21, 0, 12]),
        (0x0111, [0x14, 0x20, 0x1e, 0x30]),
        (0x0130, [0x0c; 4]),
    ] {
        records.extend_from_slice(&tag.to_be_bytes());
        records.extend_from_slice(&4_u16.to_be_bytes());
        records.extend_from_slice(&data);
    }
    out.extend_from_slice(&4_u32.to_be_bytes());
    out.extend_from_slice(&records);
    let length = out.len() as u32 - directory;
    out.resize(out.len() + 64, 0xcd);
    for (at, word) in [
        (84, jpeg_at),
        (88, embedded.len() as u32),
        (92, directory),
        (96, length),
    ] {
        out[at..at + 4].copy_from_slice(&word.to_be_bytes());
    }
    (out, u64::from(jpeg_at) + JPEG_TIFF_AT + 0x480)
}

#[test]
fn metadata_header_of_a_raf_reads_the_embedded_exif_and_the_directory() {
    let (bytes, thumbnail) = raf();
    let read = read(&bytes);
    let header = &read.header;
    assert_eq!(header.container, Container::Raf);
    assert_eq!(header.make.as_deref(), Some("FUJIFILM"));
    assert_eq!(header.orientation, Some(8));
    assert_eq!(
        header.body_serial.as_deref(),
        Some("FF02B7859149     5935373131")
    );
    // The cropped size, stored height first.
    assert_eq!(
        header.dimensions,
        Some(Dimensions {
            width: 7728,
            height: 5152
        })
    );
    assert_eq!(
        header.thumbnail.map(|thumbnail| thumbnail.offset),
        Some(thumbnail)
    );
    assert_eq!(read.reads, 2, "the head, then the directory");
    // The export reads the same EXIF from the same file.
    let export = super::super::CaptureMetadata::from_raw(&bytes);
    assert_eq!(export.field_names(), ["Make", "Model", "DateTimeOriginal"]);
}

fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(body.len() + 8)
        .unwrap()
        .to_be_bytes()
        .to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    out
}

const CANON_UUID: [u8; 16] = [
    0x85, 0xc0, 0xb6, 0x87, 0x82, 0x0f, 0x11, 0xe0, 0x81, 0x11, 0xf4, 0xce, 0x46, 0x2b, 0x6a, 0x48,
];

/// A TIFF whose IFD0 holds `entries`.
fn single_ifd(entries: &[(u16, V)]) -> Vec<u8> {
    let mut tiff = Builder::new(b"II*\0", 8);
    tiff.ifd(0, 8, entries, 0);
    tiff.finish()
}

/// A CR3: `ftyp`, a free box with a 64-bit size, `moov` holding another `uuid` box then Canon's,
/// with `CMT1`–`CMT4` and a `THMB` of `thumbnail_version`, then `mdat` to the end.
fn cr3(thumbnail_version: u8) -> (Vec<u8>, u64) {
    let cmt1 = single_ifd(&[
        (0x0100, short(6720)),
        (0x0101, short(4480)),
        (MAKE, ascii("Canon")),
        (MODEL, ascii("Canon EOS R")),
        (ORIENTATION, short(1)),
        (DATE_TIME, ascii("2021:05:10 15:58:48")),
    ]);
    let cmt2 = single_ifd(&[
        (0x829a, rational(1, 100)),
        (0x8827, short(100)),
        (0x9003, ascii("2021:05:10 15:58:48")),
        (0x9011, ascii("+01:00")),
        (0x9291, ascii("05")),
        (0xa434, ascii("RF85mm F2 MACRO IS STM")),
    ]);
    let cmt3 = single_ifd(&[(0x0001, short(1)), (0x000c, long(12_345_678))]);
    let cmt4 = single_ifd(&[
        (0x0001, ascii("N")),
        (0x0002, V::Rational(vec![(48, 1), (51, 1), (0, 1)])),
        (0x0003, ascii("E")),
        (0x0004, V::Rational(vec![(2, 1), (21, 1), (0, 1)])),
    ]);
    let mut thumbnail = vec![thumbnail_version, 0, 0, 0, 0, 160, 0, 120];
    thumbnail.extend_from_slice(&(THUMB.len() as u32).to_be_bytes());
    thumbnail.extend_from_slice(&[0, 1, 0, 0]);
    thumbnail.extend_from_slice(&THUMB);
    let canon = [
        CANON_UUID.to_vec(),
        mp4_box(b"CNCV", b"CanonCR3_001/00.09.00/00.00.00"),
        mp4_box(b"CMT1", &cmt1),
        mp4_box(b"CMT2", &cmt2),
        mp4_box(b"CMT3", &cmt3),
        mp4_box(b"CMT4", &cmt4),
        mp4_box(b"THMB", &thumbnail),
    ]
    .concat();
    let other = [[0x11; 16].to_vec(), b"not Canon's".to_vec()].concat();
    let moov = [mp4_box(b"uuid", &other), mp4_box(b"uuid", &canon)].concat();
    let mut out = mp4_box(b"ftyp", b"crx \0\0\0\x01crx isom");
    // A box with a 64-bit size.
    out.extend_from_slice(&1_u32.to_be_bytes());
    out.extend_from_slice(b"free");
    out.extend_from_slice(&24_u64.to_be_bytes());
    out.extend_from_slice(&[0; 8]);
    let moov_at = out.len();
    out.extend_from_slice(&mp4_box(b"moov", &moov));
    // An `mdat` to the end of the file.
    out.extend_from_slice(&0_u32.to_be_bytes());
    out.extend_from_slice(b"mdat");
    out.extend_from_slice(&[0x5a; 1000]);
    let thumbnail_at = moov_at + 8 + (8 + other.len()) + 8 + canon.len() - THUMB.len();
    (out, thumbnail_at as u64)
}

#[test]
fn metadata_header_of_a_cr3_reads_its_boxes() {
    let (bytes, thumbnail) = cr3(0);
    let header = header(&bytes);
    assert_eq!(header.container, Container::Cr3);
    assert_eq!(header.model.as_deref(), Some("Canon EOS R"));
    assert_eq!(
        header.dimensions,
        Some(Dimensions {
            width: 6720,
            height: 4480
        })
    );
    let time = header.capture_time.unwrap();
    assert_eq!(time.source, TimeSource::Original);
    assert_eq!(
        (time.subsec_nanos, time.offset_minutes),
        (Some(50_000_000), Some(60))
    );
    assert_eq!(header.iso, Some(100));
    assert_eq!(header.lens_model.as_deref(), Some("RF85mm F2 MACRO IS STM"));
    assert_eq!(header.body_serial.as_deref(), Some("0012345678"));
    let gps = header.gps.unwrap();
    assert_eq!(gps.latitude, degrees([(48, 1), (51, 1), (0, 1)]));
    assert_eq!(gps.longitude, degrees([(2, 1), (21, 1), (0, 1)]));
    assert_eq!(
        header.thumbnail,
        Some(Thumbnail {
            offset: thumbnail,
            length: THUMB.len() as u64,
            format: ThumbnailFormat::Jpeg {
                size: Some(Dimensions {
                    width: 160,
                    height: 120
                })
            },
        })
    );
    assert_eq!(&bytes[thumbnail as usize..][..2], [0xff, 0xd8]);
    // A version 1 thumbnail (HEVC in the R8 and R5 Mark II samples) is not offered.
    assert_eq!(header_of(&cr3(1).0).thumbnail, None);
}

// ---------------------------------------------------------------------------------------------
// Times.

/// A TIFF with `ifd0` (after a Make) and, when not empty, an Exif IFD of `exif`.
fn tiff_with(ifd0: Vec<(u16, V)>, exif: Vec<(u16, V)>) -> Vec<u8> {
    let mut tiff = Builder::new(b"II*\0", 8);
    let mut entries = vec![(MAKE, ascii("Camera"))];
    entries.extend(ifd0);
    if !exif.is_empty() {
        entries.push((EXIF, long(0x800)));
        tiff.ifd(0, 0x800, &exif, 0);
    }
    tiff.ifd(0, 8, &entries, 0);
    tiff.finish()
}

fn capture(ifd0: Vec<(u16, V)>, exif: Vec<(u16, V)>) -> Option<CaptureTime> {
    header(&tiff_with(ifd0, exif)).capture_time
}

#[test]
fn metadata_header_times_take_their_source_in_order_with_its_own_paired_tags() {
    let original = || (0x9003, ascii("2026:09:27 18:04:05"));
    // Offset-less stays offset-less, never UTC.
    let time = capture(vec![], vec![original()]).unwrap();
    assert_eq!(
        (time.source, time.subsec_nanos, time.offset_minutes),
        (TimeSource::Original, None, None)
    );
    // Only the original's own subsecond and offset pair with it.
    let time = capture(
        vec![(DATE_TIME, ascii("2026:09:27 18:04:07"))],
        vec![
            original(),
            (0x9010, ascii("+05:00")),
            (0x9012, ascii("+06:00")),
            (0x9290, ascii("1")),
            (0x9292, ascii("2")),
        ],
    )
    .unwrap();
    assert_eq!(
        (time.source, time.subsec_nanos, time.offset_minutes),
        (TimeSource::Original, None, None)
    );
    // An invalid original falls back to the digitized time and its pair.
    for invalid in [
        "0000:00:00 00:00:00",
        "    :  :     :  :  ",
        "",
        "2026:02:29 10:00:00",
        "2026:09:27 24:00:00",
        "2026:09:27",
    ] {
        let time = capture(
            vec![],
            vec![
                (0x9003, ascii(invalid)),
                (0x9004, ascii("2026:09:27 18:04:06")),
                (0x9292, ascii("5")),
                (0x9012, ascii("-02:30")),
                (0x9291, ascii("9")),
            ],
        )
        .unwrap();
        assert_eq!(time.source, TimeSource::Digitized, "{invalid:?}");
        assert_eq!(time.local, date_time(2026, 9, 27, 18, 4, 6));
        assert_eq!(
            (time.subsec_nanos, time.offset_minutes),
            (Some(500_000_000), Some(-150))
        );
    }
    // Then IFD0's DateTime with SubSecTime and OffsetTime.
    let time = capture(
        vec![(DATE_TIME, ascii("2024:02:29 23:59:59"))],
        vec![(0x9290, ascii("123")), (0x9010, ascii("+00:00"))],
    )
    .unwrap();
    assert_eq!(time.source, TimeSource::Modified);
    assert_eq!(time.local, date_time(2024, 2, 29, 23, 59, 59));
    assert_eq!(
        (time.subsec_nanos, time.offset_minutes),
        (Some(123_000_000), Some(0))
    );
    // No time at all, and times of the wrong type.
    assert_eq!(capture(vec![], vec![(0x829a, rational(1, 60))]), None);
    assert_eq!(capture(vec![], vec![(0x9003, long(20_260_927))]), None);
    // Trailing spaces and NULs are trimmed; a time in UNDEFINED is still text.
    let time = capture(
        vec![],
        vec![
            (0x9003, V::Raw(7, b"2026:09:27 18:04:05\0\0".to_vec())),
            (0x9291, ascii("42  ")),
        ],
    )
    .unwrap();
    assert_eq!(time.subsec_nanos, Some(420_000_000));
}

#[test]
fn metadata_header_dates_times_subseconds_and_offsets_are_validated() {
    let parse = |text: &str| DateTime::parse(text.as_bytes());
    assert_eq!(
        parse("2026:09:27 18:04:05"),
        Some(date_time(2026, 9, 27, 18, 4, 5))
    );
    assert_eq!(
        parse("2026-09-27T18:04:05"),
        Some(date_time(2026, 9, 27, 18, 4, 5))
    );
    assert_eq!(
        parse("2000:02:29 00:00:00"),
        Some(date_time(2000, 2, 29, 0, 0, 0))
    );
    for invalid in [
        "0000:00:00 00:00:00",
        "    :  :     :  :  ",
        "1900:02:29 00:00:00",
        "2026:13:01 00:00:00",
        "2026:04:31 00:00:00",
        "2026:09:00 00:00:00",
        "2026:09:27 23:60:00",
        "2026:09:27 23:59:60",
        "2026:09:27-18:04:05",
        "2026:09-27 18:04:05",
        "2026:09:27 18:04:05 ",
        "2026:09:27 18:04",
        "2026:0a:27 18:04:05",
    ] {
        assert_eq!(parse(invalid), None, "{invalid:?}");
    }
    for (text, nanos) in [
        ("42", Some(420_000_000)),
        ("05", Some(50_000_000)),
        ("877", Some(877_000_000)),
        ("0", Some(0)),
        ("123456789012", Some(123_456_789)),
        ("", None),
        ("4 2", None),
        ("-1", None),
        ("ab", None),
    ] {
        assert_eq!(parse_subsec(text.as_bytes()), nanos, "{text:?}");
    }
    for (text, minutes) in [
        ("+01:00", Some(60)),
        ("-02:30", Some(-150)),
        ("+00:00", Some(0)),
        ("+14:00", Some(840)),
        ("-12:00", Some(-720)),
        ("+14:01", None),
        ("+01:60", None),
        ("   :  ", None),
        ("01:00", None),
        ("+1:00", None),
        ("+01:00:00", None),
    ] {
        assert_eq!(parse_offset(text.as_bytes()), minutes, "{text:?}");
    }
}

// ---------------------------------------------------------------------------------------------
// GPS.

fn gps_header(entries: Vec<(u16, V)>) -> FileHeader {
    let mut tiff = Builder::new(b"MM\0*", 8);
    tiff.ifd(0, 8, &[(MAKE, ascii("Camera")), (GPS, long(0x100))], 0);
    tiff.ifd(0, 0x100, &entries, 0);
    header(&tiff.finish())
}

fn position(entries: Vec<(u16, V)>) -> Option<GpsPosition> {
    gps_header(entries).gps
}

#[test]
fn metadata_header_gps_positions_need_refs_a_fix_and_the_globe() {
    let fix = |lat_ref: &str, lon_ref: &str| {
        vec![
            (0x0001, ascii(lat_ref)),
            (0x0002, V::Rational(vec![(33, 1), (52, 1), (3, 10)])),
            (0x0003, ascii(lon_ref)),
            (0x0004, V::Rational(vec![(151, 1), (12, 1), (30, 1)])),
        ]
    };
    let latitude = degrees([(33, 1), (52, 1), (3, 10)]);
    let longitude = degrees([(151, 1), (12, 1), (30, 1)]);
    let north_east = position(fix("N", "E")).unwrap();
    assert_eq!(
        (north_east.latitude, north_east.longitude),
        (latitude, longitude)
    );
    assert_eq!(north_east.altitude, None);
    let south_west = position(fix("S", "W")).unwrap();
    assert_eq!(
        (south_west.latitude, south_west.longitude),
        (-latitude, -longitude)
    );
    // Lower-case references are read too.
    assert!(position(fix("s", "w")).is_some());
    // Missing or unknown references.
    assert_eq!(position(fix("N", "E")[1..].to_vec()), None);
    assert_eq!(position(fix("X", "E")), None);
    assert_eq!(position(fix("North", "E")), None);
    // A void measurement.
    let mut void = fix("N", "E");
    void.push((0x0009, ascii("V")));
    assert_eq!(position(void), None);
    let mut active = fix("N", "E");
    active.push((0x0009, ascii("A")));
    assert!(position(active).is_some());
    // No fix: every rational zero.
    let zero = vec![
        (0x0001, ascii("N")),
        (0x0002, V::Rational(vec![(0, 1), (0, 1), (0, 10000)])),
        (0x0003, ascii("E")),
        (0x0004, V::Rational(vec![(0, 1), (0, 1), (0, 10000)])),
        (0x0005, V::Raw(1, vec![0])),
        (0x0006, rational(0, 1000)),
    ];
    assert_eq!(position(zero), None);
    // Off the globe, or a zero denominator.
    let mut far = fix("N", "E");
    far[1] = (0x0002, V::Rational(vec![(91, 1), (0, 1), (0, 1)]));
    assert_eq!(position(far), None);
    let mut undefined = fix("N", "E");
    undefined[3] = (0x0004, V::Rational(vec![(151, 0), (12, 1), (30, 1)]));
    assert_eq!(position(undefined), None);
    // Decimal degrees in one rational.
    let decimal = vec![
        (0x0001, ascii("N")),
        (0x0002, rational(515_034, 10_000)),
        (0x0003, ascii("W")),
        (0x0004, rational(1275, 10_000)),
    ];
    let decimal = position(decimal).unwrap();
    assert_eq!((decimal.latitude, decimal.longitude), (51.5034, -0.1275));
    // Altitude below sea level, and an unknown reference.
    let mut below = fix("N", "E");
    below.extend([(0x0005, V::Raw(1, vec![1])), (0x0006, rational(355, 10))]);
    assert_eq!(position(below).unwrap().altitude, Some(-35.5));
    let mut unknown = fix("N", "E");
    unknown.extend([(0x0005, V::Raw(1, vec![7])), (0x0006, rational(355, 10))]);
    assert_eq!(position(unknown).unwrap().altitude, None);
}

#[test]
fn metadata_header_gps_time_is_utc_date_and_time_as_written() {
    let at = |date: &str, parts: Vec<(u32, u32)>| {
        gps_header(vec![(0x0007, V::Rational(parts)), (0x001d, ascii(date))]).gps_time
    };
    assert_eq!(
        at("2026:09:27", vec![(17, 1), (4, 1), (3456, 100)]),
        Some(GpsTime {
            utc: date_time(2026, 9, 27, 17, 4, 34),
            nanos: 560_000_000
        })
    );
    assert_eq!(at("2026:09:27", vec![(24, 1), (0, 1), (0, 1)]), None);
    assert_eq!(at("2026:09:27", vec![(17, 2), (0, 1), (0, 1)]), None);
    assert_eq!(at("2026:09:27", vec![(17, 1), (4, 1), (60, 1)]), None);
    assert_eq!(at("2026:02:30", vec![(17, 1), (4, 1), (5, 1)]), None);
    assert_eq!(at("2026:09", vec![(17, 1), (4, 1), (5, 1)]), None);
    // A time without a date, and a date without a time.
    assert_eq!(
        gps_header(vec![(0x001d, ascii("2026:09:27"))]).gps_time,
        None
    );
    assert_eq!(
        gps_header(vec![(0x0007, V::Rational(vec![(1, 1), (2, 1), (3, 1)]))]).gps_time,
        None
    );
}

// ---------------------------------------------------------------------------------------------
// Values.

fn exif_header(exif: Vec<(u16, V)>) -> FileHeader {
    header(&tiff_with(vec![], exif))
}

#[test]
fn metadata_header_iso_follows_the_sensitivity_type() {
    let iso = |exif: Vec<(u16, V)>| exif_header(exif).iso;
    assert_eq!(iso(vec![(0x8827, short(400))]), Some(400));
    assert_eq!(iso(vec![(0x8827, long(800))]), Some(800));
    assert_eq!(iso(vec![(0x8827, V::Short(vec![200, 400]))]), Some(200));
    // 65535 means "at least": the tag the sensitivity type names.
    let high = |kind: u16| {
        iso(vec![
            (0x8827, short(65535)),
            (0x8830, short(kind)),
            (0x8831, long(51_200)),
            (0x8832, long(102_400)),
            (0x8833, long(204_800)),
        ])
    };
    assert_eq!(high(1), Some(51_200));
    assert_eq!(high(2), Some(102_400));
    assert_eq!(high(3), Some(204_800));
    assert_eq!(high(4), Some(102_400));
    assert_eq!(high(6), Some(204_800));
    assert_eq!(high(0), Some(204_800));
    assert_eq!(
        iso(vec![(0x8827, short(65535)), (0x8832, long(102_400))]),
        Some(102_400)
    );
    assert_eq!(
        iso(vec![(0x8830, short(2)), (0x8832, long(3200))]),
        Some(3200)
    );
    assert_eq!(iso(vec![(0x8827, short(65535))]), None);
    assert_eq!(iso(vec![(0x8827, short(0))]), None);
    assert_eq!(iso(vec![(0x8827, rational(400, 1))]), None);
}

#[test]
fn metadata_header_tolerates_the_type_variants_cameras_write() {
    let header = exif_header(vec![
        // SRATIONAL where RATIONAL is meant, and positive.
        (0x829a, V::SRational(vec![(1, 250)])),
        // Negative: not an f-number.
        (0x829d, V::SRational(vec![(-28, 10)])),
        // RATIONAL where SRATIONAL is meant, read as the same bits signed.
        (0x9204, V::Rational(vec![((-7_i32).cast_unsigned(), 10)])),
        (0x920a, rational(0, 10)),
        (0xa405, long(50)),
        (0xa431, V::Raw(7, b"12345\0\0\0".to_vec())),
        (0xa434, V::Raw(129, "Summilux 35 ƒ/1.4".as_bytes().to_vec())),
    ]);
    assert_eq!(header.exposure_time, Some(Rational { num: 1, den: 250 }));
    assert_eq!(header.f_number, None);
    assert_eq!(
        header.exposure_bias,
        Some(SignedRational { num: -7, den: 10 })
    );
    assert_eq!(header.focal_length, None, "a zero focal length is none");
    assert_eq!(header.focal_length_35mm, Some(50));
    assert_eq!(header.body_serial.as_deref(), Some("12345"));
    assert_eq!(header.lens_model.as_deref(), Some("Summilux 35 ƒ/1.4"));
    // Orientation as a LONG; out of range; a zero denominator.
    let orientation =
        |value: V| header_of(&tiff_with(vec![(ORIENTATION, value)], vec![])).orientation;
    assert_eq!(orientation(long(3)), Some(3));
    assert_eq!(orientation(short(0)), None);
    assert_eq!(orientation(short(9)), None);
    assert_eq!(
        exif_header(vec![(0x829a, rational(1, 0))]).exposure_time,
        None
    );
    // Text longer than the cap without a NUL is dropped; with one inside the cap it is cut there.
    let long_text = "x".repeat(300);
    let header = header_of(&tiff_with(
        vec![(MODEL, V::Raw(2, long_text.into_bytes()))],
        vec![(
            0xa434,
            V::Raw(2, [b"Lens\0".as_slice(), &[b'y'; 300]].concat()),
        )],
    ));
    assert_eq!(header.model, None);
    assert_eq!(header.lens_model.as_deref(), Some("Lens"));
}

// ---------------------------------------------------------------------------------------------
// Maker notes and serials.

/// A TIFF with Make `make` and an Exif IFD of `exif` and the maker note `note`, placed at 0x1000.
fn with_maker_note(make: &str, note: &[u8], exif: Vec<(u16, V)>) -> FileHeader {
    let mut tiff = Builder::new(b"II*\0", 8);
    tiff.ifd(0, 8, &[(MAKE, ascii(make)), (EXIF, long(0x800))], 0);
    let mut exif = exif;
    exif.push((MAKER_NOTE, V::At(7, note.len() as u32, 0x1000)));
    tiff.ifd(0, 0x800, &exif, 0);
    tiff.put(0x1000, note);
    header(&tiff.finish())
}

#[test]
fn metadata_header_reads_plain_maker_note_serials() {
    // Nikon: a TIFF 10 bytes in, its serial and its preview IFD's JPEG.
    let mut nikon = Builder::raw(false);
    nikon.put(0, b"Nikon\0\x02\x11\0\0II*\0");
    nikon.put_u32(14, 8);
    nikon.ifd(
        10,
        8,
        &[(0x001d, ascii("6055566")), (0x0011, long(0x80))],
        0,
    );
    nikon.ifd(
        10,
        0x80,
        &[(0x0201, long(0xc0)), (0x0202, long(THUMB.len() as u32))],
        0,
    );
    nikon.put(10 + 0xc0, &THUMB);
    let nikon = with_maker_note("NIKON CORPORATION", &nikon.finish(), vec![]);
    assert_eq!(nikon.body_serial.as_deref(), Some("6055566"));
    assert_eq!(
        nikon
            .thumbnail
            .map(|thumbnail| (thumbnail.offset, thumbnail.length)),
        Some((0x1000 + 10 + 0xc0, THUMB.len() as u64))
    );
    // Canon: a plain IFD, offsets from the TIFF; a number, or text.
    let canon = |value: V| {
        let mut tiff = Builder::new(b"II*\0", 8);
        tiff.ifd(0, 8, &[(MAKE, ascii("Canon")), (EXIF, long(0x800))], 0);
        tiff.ifd(0, 0x800, &[(MAKER_NOTE, V::At(7, 64, 0x1000))], 0);
        tiff.ifd(0, 0x1000, &[(0x0001, short(1)), (0x000c, value)], 0);
        tiff.put(0x1100, &[0]);
        header(&tiff.finish()).body_serial
    };
    assert_eq!(canon(long(930_405_291)).as_deref(), Some("0930405291"));
    assert_eq!(canon(ascii("UC0196516")).as_deref(), Some("UC0196516"));
    assert_eq!(canon(long(0)), None);
    // Fujifilm: little-endian, its IFD offset at 8, offsets from the note.
    let mut fuji = Builder::raw(false);
    fuji.put(0, b"FUJIFILM");
    fuji.put_u32(8, 12);
    fuji.ifd(0, 12, &[(0x0010, ascii("FF02B7859149"))], 0);
    assert_eq!(
        with_maker_note("FUJIFILM", &fuji.finish(), vec![])
            .body_serial
            .as_deref(),
        Some("FF02B7859149")
    );
    // OM System: 16 bytes of header, the Equipment IFD's serial.
    for big_endian in [false, true] {
        let mut om = Builder::raw(big_endian);
        om.put(0, b"OM SYSTEM\0\0\0");
        om.put(12, if big_endian { b"MM" } else { b"II" });
        om.ifd(0, 16, &[(0x2010, V::Ifd(0x80))], 0);
        om.ifd(0, 0x80, &[(0x0101, ascii("BJMA03284   "))], 0);
        assert_eq!(
            with_maker_note("OM Digital Solutions", &om.finish(), vec![])
                .body_serial
                .as_deref(),
            Some("BJMA03284")
        );
    }
    // Pentax: big-endian here, offsets from the note.
    let mut pentax = Builder::raw(true);
    pentax.put(0, b"PENTAX \0MM");
    pentax.ifd(0, 10, &[(0x0229, ascii("7273386"))], 0);
    assert_eq!(
        with_maker_note("RICOH IMAGING COMPANY, LTD.", &pentax.finish(), vec![])
            .body_serial
            .as_deref(),
        Some("7273386")
    );
    // Panasonic: offsets from the TIFF, the serial in UNDEFINED.
    let mut tiff = Builder::new(b"II*\0", 8);
    tiff.ifd(0, 8, &[(MAKE, ascii("Panasonic")), (EXIF, long(0x800))], 0);
    tiff.ifd(0, 0x800, &[(MAKER_NOTE, V::At(7, 64, 0x1000))], 0);
    tiff.put(0x1000, b"Panasonic\0\0\0");
    tiff.ifd(
        0,
        0x100c,
        &[(0x0025, V::Raw(7, b"XHL1704170065\0\0\0".to_vec()))],
        0,
    );
    tiff.put(0x1100, &[0]);
    assert_eq!(
        header(&tiff.finish()).body_serial.as_deref(),
        Some("XHL1704170065")
    );
    // EXIF's BodySerialNumber comes first.
    let header = with_maker_note(
        "NIKON CORPORATION",
        &nikon_serial("1111111"),
        vec![(0xa431, ascii("2222222"))],
    );
    assert_eq!(header.body_serial.as_deref(), Some("2222222"));
    // Sony's maker note is enciphered and not read; an unknown one is ignored.
    assert_eq!(
        with_maker_note("SONY", b"SONY DSC \0\0\0\x01\0\0\0", vec![]).body_serial,
        None
    );
}

fn nikon_serial(serial: &str) -> Vec<u8> {
    let mut nikon = Builder::raw(false);
    nikon.put(0, b"Nikon\0\x02\x11\0\0II*\0");
    nikon.put_u32(14, 8);
    nikon.ifd(10, 8, &[(0x001d, ascii(serial))], 0);
    nikon.finish()
}

#[test]
fn metadata_header_reads_the_serial_in_pentax_and_ricoh_dng_private_data() {
    for (prefix, ifd) in [
        (b"PENTAX \0II".as_slice(), 10),
        (b"RICOH\0II".as_slice(), 8),
    ] {
        let mut private = Builder::raw(false);
        private.put(0, prefix);
        private.ifd(0, ifd, &[(0x0229, ascii("0012439"))], 0);
        let private = private.finish();
        let mut tiff = Builder::new(b"II*\0", 8);
        tiff.ifd(
            0,
            8,
            &[
                (MAKE, ascii("RICOH IMAGING COMPANY, LTD.")),
                (0xc634, V::At(1, private.len() as u32, 0x800)),
            ],
            0,
        );
        tiff.put(0x800, &private);
        assert_eq!(
            header(&tiff.finish()).body_serial.as_deref(),
            Some("0012439")
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Thumbnails.

/// A TIFF whose IFD1 names a JPEG of `length` bytes at `offset`, with `bytes` written there.
fn thumbnail_at(offset: u32, length: u32, bytes: &[u8], file_length: usize) -> Option<Thumbnail> {
    let mut tiff = Builder::new(b"II*\0", 8);
    tiff.ifd(0, 8, &[(MAKE, ascii("Camera"))], 0x100);
    tiff.ifd(
        0,
        0x100,
        &[(0x0201, long(offset)), (0x0202, long(length))],
        0,
    );
    tiff.put(offset.min(0x10_0000), bytes);
    let mut bytes = tiff.finish();
    bytes.resize(bytes.len().max(file_length), 0);
    header(&bytes).thumbnail
}

#[test]
fn metadata_header_thumbnails_are_inside_the_file_small_and_jpeg() {
    assert!(thumbnail_at(0x200, THUMB.len() as u32, &THUMB, 0).is_some());
    // Empty, past the end, not a JPEG where that can be seen without reading, too large.
    assert_eq!(thumbnail_at(0x200, 0, &THUMB, 0), None);
    assert_eq!(thumbnail_at(0x200, 4096, &THUMB, 0), None);
    assert_eq!(thumbnail_at(0x200, THUMB.len() as u32, &[0; 12], 0), None);
    let large = MAX_THUMBNAIL_JPEG_BYTES as u32 + 1;
    assert_eq!(
        thumbnail_at(0x200, large, &THUMB, 0x200 + large as usize),
        None
    );
    // Past the head its first bytes are not read just to check them.
    let far = HEADER_HEAD_BYTES as u32 + 0x1000;
    assert!(thumbnail_at(far, 64, &[0; 64], 0).is_some());
    // RGB strips must be exactly their pixels and small.
    let rgb = |width: u32, height: u32, length: u32| {
        let mut tiff = Builder::new(b"II*\0", 8);
        tiff.ifd(
            0,
            8,
            &[
                (0x00fe, long(1)),
                (0x0100, long(width)),
                (0x0101, long(height)),
                (0x0102, V::Short(vec![8, 8, 8])),
                (0x0106, short(2)),
                (0x0111, long(0x1000)),
                (0x0115, short(3)),
                (0x0117, long(length)),
            ],
            0,
        );
        let mut bytes = tiff.finish();
        bytes.resize(0x1000 + length as usize, 0);
        header(&bytes).thumbnail
    };
    assert!(rgb(160, 120, 160 * 120 * 3).is_some());
    assert_eq!(rgb(160, 120, 160 * 120 * 3 - 1), None);
    assert_eq!(rgb(1025, 2, 1025 * 2 * 3), None);
}

// ---------------------------------------------------------------------------------------------
// Robustness.

#[test]
fn metadata_header_of_unknown_or_empty_files_is_empty() {
    let empty = FileHeader {
        container: Container::Unknown,
        capture_time: None,
        gps_time: None,
        make: None,
        model: None,
        body_serial: None,
        lens_make: None,
        lens_model: None,
        exposure_time: None,
        f_number: None,
        iso: None,
        exposure_bias: None,
        focal_length: None,
        focal_length_35mm: None,
        gps: None,
        dimensions: None,
        orientation: None,
        thumbnail: None,
    };
    let none = read(&[]);
    assert_eq!(
        (none.header.clone(), none.reads, none.bytes_read),
        (empty.clone(), 0, 0)
    );
    for bytes in [b"II".as_slice(), b"not a photo at all", &[0xff; 5000]] {
        assert_eq!(header(bytes), empty);
    }
    // A container with nothing inside it.
    for bytes in [
        b"II*\0".as_slice(),
        b"MM\0*\0\0\0\x08\0",
        b"\xff\xd8\xff",
        b"FUJIFILMCCD-RAW ",
    ] {
        let header = header(bytes);
        assert_ne!(header.container, Container::Unknown);
        assert_eq!(
            FileHeader {
                container: Container::Unknown,
                ..header
            },
            empty
        );
    }
}

/// A reader that fails reads at or past `fail_at`.
struct Failing {
    inner: Cursor<Vec<u8>>,
    fail_at: u64,
}

impl Read for Failing {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.inner.position() + out.len() as u64 > self.fail_at {
            return Err(io::Error::other("the card was removed"));
        }
        self.inner.read(out)
    }
}

impl Seek for Failing {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.inner.seek(to)
    }
}

#[test]
fn metadata_header_io_errors_are_errors() {
    let bytes = dng(200_000, true);
    let length = bytes.len() as u64;
    for fail_at in [0, 100, 150_000] {
        let mut file = Failing {
            inner: Cursor::new(bytes.clone()),
            fail_at,
        };
        let error = read_header(&mut file, length).unwrap_err();
        assert_eq!(error.to_string(), "the card was removed");
    }
    // A file shorter than its listed length.
    let error = read_header(&mut Cursor::new(&bytes[..1000]), length).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    // A read that never reaches the failure succeeds.
    let mut file = Failing {
        inner: Cursor::new(bytes.clone()),
        fail_at: length,
    };
    assert_eq!(
        read_header(&mut file, length).unwrap().header,
        header(&bytes)
    );
}

/// Every container built above, in both byte orders where they have one.
fn samples() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("jpeg", jpeg(&camera_exif(false), 640, 480)),
        ("jpeg big-endian", jpeg(&camera_exif(true), 640, 480)),
        ("tiff", dng(0x800, true)),
        ("tiff far", dng(70_000, true)),
        ("cr2", cr2(true)),
        ("orf", orf(b"IIRO")),
        ("orf big-endian", orf(b"MMOR")),
        ("rw2", rw2(Some(400)).0),
        ("raf", raf().0),
        ("cr3", cr3(0).0),
    ]
}

#[test]
fn metadata_header_of_truncated_inputs_is_bounded_and_equals_the_whole_read() {
    for (name, bytes) in samples() {
        let step = (bytes.len() / 700).max(1);
        for end in (0..bytes.len()).step_by(step).chain([bytes.len() - 1]) {
            let read = read(&bytes[..end]);
            assert!(!read.capped, "{name} cut at {end}");
        }
    }
}

#[test]
fn metadata_header_of_corrupted_inputs_never_panics_and_equals_the_whole_read() {
    let mut capped = 0;
    for (index, (name, bytes)) in samples().into_iter().enumerate() {
        // A fixed linear congruential sequence of one to four byte changes per trial, in the
        // structures (the first 4 KiB) or anywhere.
        let mut state = 0x2545_f491_4f6c_dd1d_u64 ^ index as u64;
        let mut next = || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as usize
        };
        for trial in 0..1500 {
            let mut bytes = bytes.clone();
            let span = if trial % 2 == 0 {
                bytes.len().min(4096)
            } else {
                bytes.len()
            };
            for _ in 0..=next() % 4 {
                let at = next() % span;
                bytes[at] = match next() % 4 {
                    0 => bytes[at] ^ (1 << (next() % 8)),
                    1 => 0xff,
                    2 => 0,
                    _ => next() as u8,
                };
            }
            let read = read(&bytes);
            capped += usize::from(read.capped);
            let _ = name;
        }
    }
    // A corrupted offset may send the walk far, but rarely past the whole budget.
    assert!(capped < 100, "{capped} capped reads");
}

// ---------------------------------------------------------------------------------------------
// The checked-in JPEG fixtures.

#[test]
fn metadata_header_of_the_fixture_jpegs_is_bounded_and_agrees_with_the_codec_and_export() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0");
    let mut count = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|ext| ext != "jpg") {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let mut file = std::fs::File::open(&path).unwrap();
        let read = read_header(&mut file, bytes.len() as u64).unwrap();
        assert!(!read.capped, "{name}");
        assert_eq!(read.header, FileHeader::from_bytes(&bytes), "{name}");
        // The export's own orientation reader, which defaults to 1.
        assert_eq!(
            read.header.orientation.unwrap_or(1),
            super::super::jpeg_orientation(&bytes),
            "{name}"
        );
        if let Some(n) = name
            .strip_prefix("orientation-")
            .and_then(|n| n.strip_suffix(".jpg"))
        {
            assert_eq!(read.header.orientation, n.parse().ok(), "{name}");
        }
        if let Ok(frame) = luxforge_jpeg::header(&bytes) {
            assert_eq!(
                read.header.dimensions,
                Some(Dimensions {
                    width: frame.width,
                    height: frame.height
                }),
                "{name}"
            );
        }
        if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            assert_eq!(read.header.container, Container::Jpeg, "{name}");
        }
        count += 1;
    }
    assert!(count >= 20, "{count} fixtures");
}

// ---------------------------------------------------------------------------------------------
// The catalog's shape.

/// Every field reaches the catalog's `HeaderMetadata` through its own type's rules: the capture
/// time through the catalog's EXIF parser with the subsecond digits as written, the position and
/// orientation through their validating constructors, the thumbnail through `EmbeddedImage::new`.
#[test]
fn metadata_maps_every_field_to_the_catalog_header() {
    use crate::catalog_types::{self, EmbeddedFormat, ExifOrientation};
    let header = camera_header(
        Container::Tiff,
        4096,
        Some(Dimensions {
            width: 6048,
            height: 4024,
        }),
    );
    let metadata = header.metadata();
    let capture = metadata.capture.as_ref().expect("a dated header");
    assert_eq!(capture.text, "2026-09-27T18:04:05.42+01:00");
    assert_eq!(capture.offset_minutes, Some(60));
    assert_eq!(capture.local_ms.rem_euclid(1000), 420);
    let expected =
        catalog_types::CaptureTime::from_exif("2026:09:27 18:04:05", Some("42"), Some("+01:00"));
    assert_eq!(metadata.capture, expected);
    let position = metadata.position.expect("a position");
    let gps = header.gps.unwrap();
    assert_eq!(
        (position.lat, position.lon, position.alt_m),
        (gps.latitude, gps.longitude, Some(35.5))
    );
    let camera = metadata.camera.as_ref().expect("a camera");
    assert_eq!(
        (
            camera.make.as_str(),
            camera.model.as_str(),
            camera.serial.as_deref()
        ),
        ("NIKON CORPORATION", "NIKON Z 6", Some("6012345"))
    );
    assert_eq!(metadata.lens.as_deref(), Some("NIKKOR Z 24-70mm f/4 S"));
    let exposure = metadata.exposure;
    assert_eq!(exposure.time_s, Some(1.0 / 250.0));
    assert_eq!(exposure.f_number, Some(2.8));
    assert_eq!(exposure.iso, Some(400));
    assert_eq!(exposure.bias_ev, Some(-2.0 / 3.0));
    assert_eq!(exposure.focal_mm, Some(35.0));
    assert_eq!(exposure.focal_35mm_mm, Some(35.0));
    assert_eq!(
        metadata.dimensions,
        Some(catalog_types::Dimensions {
            width: 6048,
            height: 4024
        })
    );
    assert_eq!(metadata.orientation, ExifOrientation::new(6));
    let thumbnail = metadata.thumbnail.expect("a thumbnail");
    assert_eq!(
        (
            thumbnail.offset(),
            thumbnail.len() as usize,
            thumbnail.format(),
            thumbnail.size()
        ),
        (4096, THUMB.len(), EmbeddedFormat::Jpeg, None)
    );

    // Subsecond digits as written, a western offset, and no subsecond at all.
    let mut header = camera_header(Container::Jpeg, 4096, None);
    let time = header.capture_time.as_mut().unwrap();
    (time.subsec_nanos, time.subsec_digits, time.offset_minutes) =
        (Some(300_000_000), 3, Some(-330));
    assert_eq!(
        header.metadata().capture.unwrap().text,
        "2026-09-27T18:04:05.300-05:30"
    );
    let time = header.capture_time.as_mut().unwrap();
    (time.subsec_nanos, time.subsec_digits, time.offset_minutes) = (None, 0, None);
    let capture = header.metadata().capture.unwrap();
    assert_eq!(
        (capture.text.as_str(), capture.offset_minutes),
        ("2026-09-27T18:04:05", None)
    );

    // A model alone is a camera; neither is none. An RGB strip carries its size; one whose length
    // is not three bytes a pixel is refused, as the catalog's type refuses it.
    header.make = None;
    assert_eq!(header.metadata().camera.unwrap().make, "");
    header.model = None;
    assert_eq!(header.metadata().camera, None);
    let rgb = |length| Thumbnail {
        offset: 512,
        length,
        format: ThumbnailFormat::Rgb8 {
            size: Dimensions {
                width: 160,
                height: 120,
            },
        },
    };
    header.thumbnail = Some(rgb(160 * 120 * 3));
    let strip = header.metadata().thumbnail.unwrap();
    assert_eq!(
        (
            strip.format(),
            strip.size().map(|size| (size.width, size.height))
        ),
        (EmbeddedFormat::Rgb8, Some((160, 120)))
    );
    header.thumbnail = Some(rgb(160 * 120 * 3 + 1));
    assert_eq!(header.metadata().thumbnail, None);
    header.thumbnail = Some(Thumbnail {
        offset: 0,
        length: u64::from(u32::MAX) + 1,
        format: ThumbnailFormat::Jpeg { size: None },
    });
    assert_eq!(header.metadata().thumbnail, None);

    // An empty header is empty metadata.
    assert_eq!(
        FileHeader::from_bytes(b"not a photo").metadata(),
        Default::default()
    );
}
