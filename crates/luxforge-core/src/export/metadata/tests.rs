//! The reader and writer against an independent EXIF implementation (`kamadak-exif`): it writes
//! every input, and reads back every payload the export writes.

use super::{CaptureMetadata, FIELDS, Ifd, MAX_PAYLOAD, Value};
use exif::experimental::Writer;
use exif::{Field, In, Rational, Reader, SRational, Tag, Value as ExifValue};
use std::io::Cursor;

fn ascii(text: &str) -> ExifValue {
    ExifValue::Ascii(vec![text.as_bytes().to_vec()])
}

fn rationals(pairs: &[(u32, u32)]) -> ExifValue {
    ExifValue::Rational(
        pairs
            .iter()
            .map(|&(num, denom)| Rational { num, denom })
            .collect(),
    )
}

/// Every field of the design's table with a representative value, in table order.
fn kept() -> Vec<(Tag, ExifValue)> {
    vec![
        (Tag::ImageDescription, ascii("Harbour at dusk")),
        (Tag::Make, ascii("NIKON CORPORATION")),
        (Tag::Model, ascii("NIKON Z 6")),
        (Tag::Artist, ascii("Will")),
        (
            Tag::Copyright,
            ExifValue::Ascii(vec![b"Photographer".to_vec(), b"Editor".to_vec()]),
        ),
        (Tag::ExposureTime, rationals(&[(1, 250)])),
        (Tag::FNumber, rationals(&[(28, 10)])),
        (Tag::ExposureProgram, ExifValue::Short(vec![3])),
        (Tag::PhotographicSensitivity, ExifValue::Short(vec![400])),
        (Tag::DateTimeOriginal, ascii("2026:09:27 18:04:05")),
        (Tag::DateTimeDigitized, ascii("2026:09:27 18:04:06")),
        (Tag::OffsetTimeOriginal, ascii("+01:00")),
        (Tag::OffsetTimeDigitized, ascii("-02:30")),
        (
            Tag::ExposureBiasValue,
            ExifValue::SRational(vec![SRational { num: -2, denom: 3 }]),
        ),
        (Tag::MeteringMode, ExifValue::Short(vec![5])),
        (Tag::Flash, ExifValue::Short(vec![16])),
        (Tag::FocalLength, rationals(&[(350, 10)])),
        (Tag::SubSecTimeOriginal, ascii("42")),
        (Tag::FocalLengthIn35mmFilm, ExifValue::Short(vec![35])),
        (
            Tag::LensSpecification,
            rationals(&[(24, 1), (70, 1), (4, 1), (0, 0)]),
        ),
        (Tag::LensMake, ascii("Nikon")),
        (Tag::LensModel, ascii("NIKKOR Z 24-70mm f/4 S")),
        (Tag::GPSVersionID, ExifValue::Byte(vec![2, 3, 0, 0])),
        (Tag::GPSLatitudeRef, ascii("N")),
        (
            Tag::GPSLatitude,
            rationals(&[(51, 1), (30, 1), (1234, 100)]),
        ),
        (Tag::GPSLongitudeRef, ascii("W")),
        (Tag::GPSLongitude, rationals(&[(0, 1), (7, 1), (3900, 100)])),
        (Tag::GPSAltitudeRef, ExifValue::Byte(vec![1])),
        (Tag::GPSAltitude, rationals(&[(355, 10)])),
        (Tag::GPSTimeStamp, rationals(&[(17, 1), (4, 1), (5, 1)])),
        (Tag::GPSImgDirectionRef, ascii("T")),
        (Tag::GPSImgDirection, rationals(&[(27150, 100)])),
        (Tag::GPSDateStamp, ascii("2026:09:27")),
    ]
}

/// Fields an original carries that the export must never copy, including the structural ones it
/// writes itself with its own values.
fn excluded() -> Vec<(Tag, ExifValue)> {
    vec![
        (Tag::Orientation, ExifValue::Short(vec![6])),
        (Tag::Software, ascii("Ver.3.01")),
        (Tag::DateTime, ascii("2026:09:27 18:04:05")),
        (Tag::XResolution, rationals(&[(300, 1)])),
        (Tag::ExifVersion, ExifValue::Undefined(b"0231".to_vec(), 0)),
        (Tag::ColorSpace, ExifValue::Short(vec![0xffff])),
        (Tag::PixelXDimension, ExifValue::Long(vec![6048])),
        (Tag::PixelYDimension, ExifValue::Long(vec![4024])),
        (
            Tag::MakerNote,
            ExifValue::Undefined(b"Nikon\0private".to_vec(), 0),
        ),
        (
            Tag::UserComment,
            ExifValue::Undefined(b"ASCII\0\0\0secret".to_vec(), 0),
        ),
        (Tag::BodySerialNumber, ascii("6012345")),
        (Tag::LensSerialNumber, ascii("20098765")),
        (Tag::GPSSatellites, ascii("7")),
    ]
}

/// A TIFF written by the independent writer, with a thumbnail in IFD1 when asked.
fn tiff(fields: &[(Tag, ExifValue)], little_endian: bool, thumbnail: bool) -> Vec<u8> {
    let fields: Vec<Field> = fields
        .iter()
        .map(|(tag, value)| Field {
            tag: *tag,
            ifd_num: In::PRIMARY,
            value: value.clone(),
        })
        .collect();
    let thumbnail_fields = [Field {
        tag: Tag::Orientation,
        ifd_num: In::THUMBNAIL,
        value: ExifValue::Short(vec![1]),
    }];
    let jpeg = [0xff, 0xd8, 0xff, 0xd9];
    let mut writer = Writer::new();
    for field in &fields {
        writer.push_field(field);
    }
    if thumbnail {
        writer.push_field(&thumbnail_fields[0]);
        writer.set_jpeg(&jpeg, In::THUMBNAIL);
    }
    let mut out = Cursor::new(Vec::new());
    writer.write(&mut out, little_endian).unwrap();
    out.into_inner()
}

fn segment(out: &mut Vec<u8>, marker: u8, payload: &[u8]) {
    out.extend_from_slice(&[0xff, marker]);
    out.extend_from_slice(&u16::try_from(payload.len() + 2).unwrap().to_be_bytes());
    out.extend_from_slice(payload);
}

/// A minimal JPEG whose EXIF APP1 follows a JFIF APP0 and an XMP APP1, as a camera's may.
fn jpeg(tiff: &[u8]) -> Vec<u8> {
    let mut out = vec![0xff, 0xd8];
    segment(&mut out, 0xe0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
    segment(
        &mut out,
        0xe1,
        b"http://ns.adobe.com/xap/1.0/\0<x:xmpmeta/>",
    );
    segment(&mut out, 0xe1, &[b"Exif\0\0".as_slice(), tiff].concat());
    segment(&mut out, 0xda, &[0; 10]);
    out.extend_from_slice(&[0x12, 0x34, 0xff, 0xd9]);
    out
}

/// A RAF header naming `jpeg` by its big-endian offset and length, then some sensor bytes.
fn raf(jpeg: &[u8]) -> Vec<u8> {
    let mut out = b"FUJIFILMCCD-RAW 0201FF383501".to_vec();
    out.resize(148, 0);
    let offset = u32::try_from(out.len()).unwrap();
    out[84..88].copy_from_slice(&offset.to_be_bytes());
    out[88..92].copy_from_slice(&u32::try_from(jpeg.len()).unwrap().to_be_bytes());
    out.extend_from_slice(jpeg);
    out.extend_from_slice(&[0xab; 64]);
    out
}

type ReadBack = Vec<(In, Tag, String)>;

/// A value's comparable form: an UNDEFINED value without the offset the reader records.
fn key(value: &ExifValue) -> String {
    match value {
        ExifValue::Undefined(bytes, _) => format!("Undefined({bytes:?})"),
        other => format!("{other:?}"),
    }
}

/// The payload as the independent reader sees it, without the IFD pointers.
fn read_back(payload: &[u8]) -> ReadBack {
    assert!(payload.len() <= MAX_PAYLOAD);
    let exif = Reader::new()
        .read_raw(payload.to_vec())
        .expect("the independent reader accepts the export's payload");
    assert!(exif.little_endian());
    let mut fields: ReadBack = exif
        .fields()
        .filter(|field| ![Tag::ExifIFDPointer, Tag::GPSInfoIFDPointer].contains(&field.tag))
        .map(|field| (field.ifd_num, field.tag, key(&field.value)))
        .collect();
    fields.sort_by_key(|(ifd, tag, _)| (*ifd, *tag));
    fields
}

/// What the export should write for `kept` at `width` × `height`.
fn expected(kept: &[(Tag, ExifValue)], width: u32, height: u32) -> ReadBack {
    let structural = [
        (Tag::Orientation, ExifValue::Short(vec![1])),
        (Tag::Software, ascii("Luxforge")),
        (Tag::ExifVersion, ExifValue::Undefined(b"0232".to_vec(), 0)),
        (Tag::ColorSpace, ExifValue::Short(vec![1])),
        (Tag::PixelXDimension, ExifValue::Long(vec![width])),
        (Tag::PixelYDimension, ExifValue::Long(vec![height])),
    ];
    let mut fields: ReadBack = kept
        .iter()
        .chain(&structural)
        .map(|(tag, value)| (In::PRIMARY, *tag, key(value)))
        .collect();
    fields.sort_by_key(|(ifd, tag, _)| (*ifd, *tag));
    fields
}

fn all_names() -> Vec<&'static str> {
    FIELDS.iter().map(|field| field.name).collect()
}

#[test]
fn the_table_is_grouped_by_ifd_and_sorted_by_tag() {
    let rank = |ifd: Ifd| ifd as u8;
    for pair in FIELDS.windows(2) {
        assert!(
            (rank(pair[0].ifd), pair[0].tag) < (rank(pair[1].ifd), pair[1].tag),
            "{} before {}",
            pair[0].name,
            pair[1].name
        );
    }
    let names: Vec<String> = kept().iter().map(|(tag, _)| tag.to_string()).collect();
    assert_eq!(names, all_names(), "the table names the EXIF tags");
}

#[test]
fn every_kept_field_round_trips_from_each_container() {
    let mut input = kept();
    input.extend(excluded());
    for little_endian in [false, true] {
        let tiff = tiff(&input, little_endian, true);
        let from_jpeg = CaptureMetadata::from_jpeg(&jpeg(&tiff));
        let from_tiff = CaptureMetadata::from_raw(&tiff);
        let from_raf = CaptureMetadata::from_raw(&raf(&jpeg(&tiff)));
        assert_eq!(from_jpeg, from_tiff);
        assert_eq!(from_raf, from_tiff);
        for metadata in [from_jpeg, from_tiff, from_raf] {
            assert!(!metadata.is_empty());
            assert_eq!(metadata.field_names(), all_names());
            assert_eq!(
                read_back(&metadata.exif_payload(6000, 4000)),
                expected(&kept(), 6000, 4000),
                "little-endian input: {little_endian}"
            );
        }
    }
}

#[test]
fn excluded_fields_and_thumbnails_never_reach_the_export() {
    let mut input = kept();
    input.extend(excluded());
    let metadata = CaptureMetadata::from_raw(&tiff(&input, false, true));
    let payload = metadata.exif_payload(10, 20);
    let exif = Reader::new().read_raw(payload.clone()).unwrap();
    for tag in [
        Tag::MakerNote,
        Tag::UserComment,
        Tag::BodySerialNumber,
        Tag::LensSerialNumber,
        Tag::DateTime,
        Tag::XResolution,
        Tag::GPSSatellites,
    ] {
        assert!(exif.get_field(tag, In::PRIMARY).is_none(), "{tag}");
    }
    assert!(exif.fields().all(|field| field.ifd_num == In::PRIMARY));
    assert!(!payload.windows(7).any(|bytes| bytes == b"private"));
    assert!(!payload.windows(6).any(|bytes| bytes == b"secret"));
    let orientation = exif.get_field(Tag::Orientation, In::PRIMARY).unwrap();
    assert_eq!(orientation.value.get_uint(0), Some(1));
    let colour_space = exif.get_field(Tag::ColorSpace, In::PRIMARY).unwrap();
    assert_eq!(colour_space.value.get_uint(0), Some(1));
    let width = exif.get_field(Tag::PixelXDimension, In::PRIMARY).unwrap();
    assert!(matches!(width.value, ExifValue::Long(ref v) if v == &[10]));
    let height = exif.get_field(Tag::PixelYDimension, In::PRIMARY).unwrap();
    assert!(matches!(height.value, ExifValue::Long(ref v) if v == &[20]));
}

#[test]
fn empty_metadata_writes_only_the_structural_fields() {
    let empty = CaptureMetadata::default();
    assert!(empty.is_empty());
    assert!(empty.field_names().is_empty());
    assert_eq!(read_back(&empty.exif_payload(3, 2)), expected(&[], 3, 2));
    let exif = Reader::new().read_raw(empty.exif_payload(3, 2)).unwrap();
    assert!(
        exif.get_field(Tag::GPSInfoIFDPointer, In::PRIMARY)
            .is_none()
    );

    // No GPS field, no GPS IFD.
    let camera = CaptureMetadata::from_raw(&tiff(&[(Tag::Make, ascii("FUJIFILM"))], true, false));
    assert_eq!(camera.field_names(), ["Make"]);
    let exif = Reader::new().read_raw(camera.exif_payload(3, 2)).unwrap();
    assert!(
        exif.get_field(Tag::GPSInfoIFDPointer, In::PRIMARY)
            .is_none()
    );

    // Containers without EXIF.
    assert!(CaptureMetadata::from_jpeg(&jpeg(&[])).is_empty());
    let mut no_app1 = vec![0xff, 0xd8];
    segment(&mut no_app1, 0xe0, b"JFIF\0");
    no_app1.extend_from_slice(&[0xff, 0xd9]);
    assert!(CaptureMetadata::from_jpeg(&no_app1).is_empty());
    assert!(CaptureMetadata::from_raw(b"not a raw file at all").is_empty());
    assert!(CaptureMetadata::from_raw(&raf(&no_app1)).is_empty());
    assert!(CaptureMetadata::from_jpeg(&tiff(&kept(), true, false)).is_empty());
}

#[test]
fn fields_of_the_wrong_type_or_count_are_dropped() {
    let long_text = "x".repeat(1023);
    let input = vec![
        (Tag::ImageDescription, ascii("        ")),
        (Tag::Make, ExifValue::Undefined(b"NIKON".to_vec(), 0)),
        (Tag::Model, ascii(&"y".repeat(1024))),
        // 1023 characters and a NUL: the longest kept.
        (Tag::Artist, ascii(&long_text)),
        (
            Tag::ExposureTime,
            ExifValue::SRational(vec![SRational { num: 1, denom: 250 }]),
        ),
        (Tag::FNumber, rationals(&[(28, 10), (1, 1)])),
        (Tag::ExposureProgram, ExifValue::Long(vec![3])),
        (
            Tag::PhotographicSensitivity,
            ExifValue::Short(vec![400, 400]),
        ),
        (Tag::DateTimeOriginal, ascii("2026:09:27")),
        (Tag::OffsetTimeOriginal, ascii("+01:00:00")),
        (Tag::ExposureBiasValue, rationals(&[(1, 3)])),
        (Tag::Flash, ExifValue::Short(vec![16])),
        (
            Tag::LensSpecification,
            rationals(&[(24, 1), (70, 1), (4, 1)]),
        ),
        (Tag::GPSVersionID, ExifValue::Byte(vec![2, 3, 0])),
        (Tag::GPSLatitudeRef, ascii("North")),
        (Tag::GPSLatitude, rationals(&[(51, 1), (30, 1)])),
        (Tag::GPSAltitudeRef, ExifValue::Short(vec![0])),
    ];
    for little_endian in [false, true] {
        let metadata = CaptureMetadata::from_raw(&tiff(&input, little_endian, false));
        assert_eq!(metadata.field_names(), ["Artist", "Flash"]);
        assert_eq!(
            read_back(&metadata.exif_payload(1, 1)),
            expected(
                &[
                    (Tag::Artist, ascii(&long_text)),
                    (Tag::Flash, ExifValue::Short(vec![16]))
                ],
                1,
                1
            )
        );
    }
}

#[test]
fn ascii_ends_at_its_nul_and_loses_trailing_spaces() {
    let input = [
        (Tag::Make, ascii("NIKON CORPORATION   ")),
        (
            Tag::Model,
            ExifValue::Ascii(vec![b"Z 6".to_vec(), b"junk".to_vec()]),
        ),
        (
            Tag::Copyright,
            ExifValue::Ascii(vec![b" ".to_vec(), b"Editor".to_vec()]),
        ),
    ];
    let metadata = CaptureMetadata::from_raw(&tiff(&input, true, false));
    let text = |index: usize| match &metadata.fields[index].1 {
        Value::Ascii(text) => text.to_vec(),
        other => panic!("{other:?}"),
    };
    assert_eq!(text(0), b"NIKON CORPORATION");
    assert_eq!(text(1), b"Z 6");
    assert_eq!(text(2), b" \0Editor");
}

/// A little-endian TIFF with one IFD at 8 of `(tag, type, count, value or offset)` entries, then
/// `tail`, and a zero link.
fn raw_tiff(entries: &[(u16, u16, u32, u32)], tail: &[u8]) -> Vec<u8> {
    let mut out = b"II*\0".to_vec();
    out.extend_from_slice(&8_u32.to_le_bytes());
    out.extend_from_slice(&u16::try_from(entries.len()).unwrap().to_le_bytes());
    for (tag, kind, count, value) in entries {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.extend_from_slice(&0_u32.to_le_bytes());
    out.extend_from_slice(tail);
    out
}

#[test]
fn malformed_structures_yield_empty_or_partial_metadata() {
    const ASCII: u16 = 2;
    const LONG: u16 = 4;
    const RATIONAL: u16 = 5;
    let make = u32::from_le_bytes(*b"Ab\0\0");
    let names = |bytes: &[u8]| CaptureMetadata::from_raw(bytes).field_names();

    // The Exif and GPS pointers loop back to IFD0: it is read once, as IFD0 only.
    let looping = raw_tiff(
        &[
            (0x010f, ASCII, 3, make),
            (0x8769, LONG, 1, 8),
            (0x8825, LONG, 1, 8),
        ],
        &[],
    );
    assert_eq!(names(&looping), ["Make"]);

    // The GPS pointer names the Exif IFD: read once, as the Exif IFD.
    let exif_at = 8 + 2 + 3 * 12 + 4;
    let shared = raw_tiff(
        &[
            (0x010f, ASCII, 3, make),
            (0x8769, LONG, 1, exif_at),
            (0x8825, LONG, 1, exif_at),
        ],
        &[
            &1_u16.to_le_bytes()[..],
            &[0x09, 0x92, 3, 0, 1, 0, 0, 0, 16, 0, 0, 0],
            &[0; 4],
        ]
        .concat(),
    );
    assert_eq!(names(&shared), ["Make", "Flash"]);

    // A huge count, a value past the end, a pointer past the end, and one good field.
    let partial = raw_tiff(
        &[
            (0x010f, ASCII, u32::MAX, 0),
            (0x0110, ASCII, 40, 4096),
            (0x013b, ASCII, 3, make),
            (0x8769, LONG, 1, u32::MAX),
            (0x8825, RATIONAL, 1, 8),
        ],
        &[],
    );
    assert_eq!(names(&partial), ["Artist"]);

    // A count past 256 entries is not read, even when the table fits.
    let mut crowded = raw_tiff(&[(0x010f, ASCII, 3, make); 257], &[]);
    assert!(CaptureMetadata::from_raw(&crowded).is_empty());
    crowded[8..10].copy_from_slice(&256_u16.to_le_bytes());
    assert_eq!(names(&crowded), ["Make"], "a duplicated tag is kept once");
    let mut declared = raw_tiff(&[(0x010f, ASCII, 3, make)], &[]);
    declared[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
    assert!(CaptureMetadata::from_raw(&declared).is_empty());

    // IFD0 past the end, and a header alone.
    let mut far = raw_tiff(&[(0x010f, ASCII, 3, make)], &[]);
    far[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(CaptureMetadata::from_raw(&far).is_empty());
    assert!(CaptureMetadata::from_raw(b"II*\0").is_empty());
    assert!(CaptureMetadata::from_raw(b"MM\0*\0\0\0\x08\0").is_empty());

    // A JPEG segment longer than the file, and one declaring less than its own length field.
    let tiff = tiff(&kept(), true, false);
    let mut long = jpeg(&tiff);
    long[4..6].copy_from_slice(&u16::MAX.to_be_bytes());
    assert!(CaptureMetadata::from_jpeg(&long).is_empty());
    let mut short = jpeg(&tiff);
    short[4..6].copy_from_slice(&0_u16.to_be_bytes());
    assert!(CaptureMetadata::from_jpeg(&short).is_empty());

    // RAF offsets outside the file.
    let good = raf(&jpeg(&tiff));
    assert_eq!(CaptureMetadata::from_raw(&good).field_names(), all_names());
    for (offset, length) in [
        (u32::MAX, 16),
        (u32::MAX, u32::MAX),
        (148, u32::MAX),
        (u32::try_from(good.len()).unwrap(), 1),
        (0, 0),
    ] {
        let mut bogus = good.clone();
        bogus[84..88].copy_from_slice(&offset.to_be_bytes());
        bogus[88..92].copy_from_slice(&length.to_be_bytes());
        assert!(
            CaptureMetadata::from_raw(&bogus).is_empty(),
            "{offset} {length}"
        );
    }
    assert!(CaptureMetadata::from_raw(&good[..90]).is_empty());
}

#[test]
fn truncated_and_corrupted_inputs_never_panic_and_always_write_a_readable_payload() {
    let mut input = kept();
    input.extend(excluded());
    let check = |metadata: CaptureMetadata| {
        let payload = metadata.exif_payload(7, 5);
        assert!(payload.len() <= MAX_PAYLOAD);
        let exif = Reader::new()
            .read_raw(payload)
            .expect("the payload stays readable");
        assert_eq!(
            exif.fields()
                .filter(|field| ![Tag::ExifIFDPointer, Tag::GPSInfoIFDPointer].contains(&field.tag))
                .count(),
            metadata.field_names().len() + 6
        );
    };
    for little_endian in [false, true] {
        let tiff = tiff(&input, little_endian, true);
        let wrapped = jpeg(&tiff);
        let raf = raf(&wrapped);
        for end in 0..wrapped.len() {
            check(CaptureMetadata::from_jpeg(&wrapped[..end]));
            check(CaptureMetadata::from_raw(&tiff[..end.min(tiff.len())]));
        }
        for end in (0..raf.len()).step_by(7) {
            check(CaptureMetadata::from_raw(&raf[..end]));
        }
        // A fixed linear congruential sequence of one to four byte changes per trial.
        let mut state = 0x2545_f491_4f6c_dd1d_u64 ^ u64::from(little_endian);
        let mut next = || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as usize
        };
        for _ in 0..3000 {
            let mut bytes = tiff.clone();
            for _ in 0..=next() % 4 {
                let at = next() % bytes.len();
                bytes[at] = match next() % 3 {
                    0 => bytes[at] ^ (1 << (next() % 8)),
                    1 => 0xff,
                    _ => 0,
                };
            }
            check(CaptureMetadata::from_raw(&bytes));
            check(CaptureMetadata::from_jpeg(&jpeg(&bytes)));
        }
    }
}

#[test]
fn oversized_fields_are_dropped_longest_descriptive_first() {
    let index = |name: &str| FIELDS.iter().position(|field| field.name == name).unwrap();
    let text = |length: usize| Value::Ascii(vec![b'a'; length].into());
    let mut metadata = CaptureMetadata {
        fields: vec![
            (index("ImageDescription"), text(30_000)),
            (index("Make"), text(35_000)),
            (index("Model"), text(10)),
            (index("Artist"), text(20_000)),
        ],
    };
    metadata.fit();
    assert_eq!(metadata.field_names(), ["Make", "Model", "Artist"]);

    let mut metadata = CaptureMetadata {
        fields: vec![
            (index("Make"), text(40_000)),
            (index("Model"), text(30_000)),
            (index("Artist"), text(10)),
        ],
    };
    // Artist goes first although it is the shortest; then the longest of the rest.
    metadata.fit();
    assert_eq!(metadata.field_names(), ["Model"]);
    let payload = metadata.exif_payload(1, 1);
    assert!(payload.len() <= MAX_PAYLOAD);
    Reader::new().read_raw(payload).unwrap();
}
