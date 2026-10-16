//! The TIFF writer against independent readers: the `tiff` crate decodes the pixels and reads the
//! image's tags, the repository's own TIFF walkers (the export's EXIF reader and the index's header
//! reader) read the metadata, and `kamadak-exif` reads the structural fields.

use super::*;
use crate::{
    ErrorKind,
    export::{encode::srgb_profile, metadata::header::FileHeader},
    tiles::Pixels,
};
use exif::experimental::Writer;
use exif::{Field, In, Rational, Reader, Tag as ExifTag, Value as ExifValue};
use std::{cell::Cell, io, io::Cursor};
use tiff::{
    ColorType,
    decoder::{Decoder, DecodingResult},
    tags::Tag,
};

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

/// An original's metadata as the export reads it, with descriptive, capture and, when `gps`,
/// location fields: what the encoders of every format are handed for a level.
pub(crate) fn capture_metadata(gps: bool) -> CaptureMetadata {
    let mut fields = vec![
        (ExifTag::ImageDescription, ascii("Harbour at dusk")),
        (ExifTag::Make, ascii("NIKON CORPORATION")),
        (ExifTag::Model, ascii("NIKON Z 6")),
        (ExifTag::Artist, ascii("Will")),
        (ExifTag::Copyright, ascii("Will Demaine")),
        (ExifTag::ExposureTime, rationals(&[(1, 250)])),
        (ExifTag::FNumber, rationals(&[(28, 10)])),
        (
            ExifTag::PhotographicSensitivity,
            ExifValue::Short(vec![400]),
        ),
        (ExifTag::DateTimeOriginal, ascii("2026:09:27 18:04:05")),
        (ExifTag::LensModel, ascii("NIKKOR Z 24-70mm f/4 S")),
    ];
    if gps {
        fields.extend([
            (ExifTag::GPSLatitudeRef, ascii("N")),
            (
                ExifTag::GPSLatitude,
                rationals(&[(51, 1), (30, 1), (1234, 100)]),
            ),
            (ExifTag::GPSLongitudeRef, ascii("W")),
            (
                ExifTag::GPSLongitude,
                rationals(&[(0, 1), (7, 1), (3900, 100)]),
            ),
        ]);
    }
    let fields: Vec<Field> = fields
        .into_iter()
        .map(|(tag, value)| Field {
            tag,
            ifd_num: In::PRIMARY,
            value,
        })
        .collect();
    let mut writer = Writer::new();
    for field in &fields {
        writer.push_field(field);
    }
    let mut tiff = Cursor::new(Vec::new());
    writer.write(&mut tiff, true).unwrap();
    let metadata = CaptureMetadata::from_raw(&tiff.into_inner());
    assert_eq!(
        metadata.field_names().len(),
        if gps { 14 } else { 10 },
        "every field is kept"
    );
    metadata
}

/// Gradients, a hard edge and a fine texture in every sample's whole range, as `depth`.
fn frame(width: u32, height: u32, depth: Depth) -> Pixels {
    let mut samples = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let r = x * 65_535 / width;
            let g = y * 65_535 / height;
            let b = if x * 2 < width { 9_000 } else { 60_000 } + (x * 31 + y * 17) % 997;
            samples.extend([r, g, b, 65_535]);
        }
    }
    match depth {
        Depth::Eight => Pixels::Eight(samples.iter().map(|&s| (s >> 8) as u8).collect()),
        Depth::Sixteen => Pixels::Sixteen(samples.iter().map(|&s| s as u16).collect()),
    }
}

/// RGB samples.
#[derive(Debug, PartialEq)]
enum Rgb {
    Eight(Vec<u8>),
    Sixteen(Vec<u16>),
}

/// The RGB samples of `pixels`, alpha dropped, as the decoder returns them.
fn rgb(pixels: &Pixels) -> Rgb {
    match pixels {
        Pixels::Eight(rgba) => Rgb::Eight(
            rgba.chunks_exact(4)
                .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
                .collect(),
        ),
        Pixels::Sixteen(rgba) => Rgb::Sixteen(
            rgba.chunks_exact(4)
                .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
                .collect(),
        ),
    }
}

/// `pixels` in bands of `band` rows, the last cut short.
fn bands(pixels: &Pixels, width: u32, band: u32) -> Vec<(u32, PixelRows<'_>)> {
    let stride = width as usize * 4 * band as usize;
    let rows = |len: usize| (len / (width as usize * 4)) as u32;
    match pixels {
        Pixels::Eight(rgba) => rgba
            .chunks(stride)
            .map(|chunk| (rows(chunk.len()), PixelRows::Eight(chunk)))
            .collect(),
        Pixels::Sixteen(rgba) => rgba
            .chunks(stride)
            .map(|chunk| (rows(chunk.len()), PixelRows::Sixteen(chunk)))
            .collect(),
    }
}

fn options(depth: Depth, compress: bool, pixels_per_inch: Option<u16>) -> TiffOptions {
    TiffOptions {
        depth,
        compress,
        pixels_per_inch,
    }
}

/// The whole file, written in bands of `band` rows.
fn write(
    (width, height): (u32, u32),
    pixels: &Pixels,
    options: &TiffOptions,
    metadata: Option<&CaptureMetadata>,
    band: u32,
) -> Vec<u8> {
    let mut writer = TiffWriter::start(
        Cursor::new(Vec::new()),
        width,
        height,
        options,
        srgb_profile(),
        metadata,
    )
    .unwrap();
    for (rows, band) in bands(pixels, width, band) {
        writer
            .write_rows(rows, band, &mut |_| {}, &|| Ok(()))
            .unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn decode(file: &[u8]) -> (u32, u32, ColorType, Rgb) {
    let mut decoder = Decoder::new(Cursor::new(file)).expect("a TIFF");
    let (width, height) = decoder.dimensions().unwrap();
    let colour = decoder.colortype().unwrap();
    let pixels = match decoder.read_image().expect("decodes") {
        DecodingResult::U8(samples) => Rgb::Eight(samples),
        DecodingResult::U16(samples) => Rgb::Sixteen(samples),
        other => panic!("not 8 or 16-bit samples: {other:?}"),
    };
    (width, height, colour, pixels)
}

/// At 8 and 16 bits, with and without compression and in bands of any size, the file decodes to
/// exactly the samples given, and the bands do not change its bytes.
#[test]
fn every_depth_and_compression_decodes_to_the_exact_samples() {
    let (width, height) = (1031, 300);
    for depth in [Depth::Eight, Depth::Sixteen] {
        let pixels = frame(width, height, depth);
        let expected = rgb(&pixels);
        let bits = (depth.bytes() * 8) as u8;
        for compress in [false, true] {
            let options = options(depth, compress, None);
            let whole = write((width, height), &pixels, &options, None, height);
            let (w, h, colour, decoded) = decode(&whole);
            assert_eq!((w, h), (width, height));
            assert_eq!(colour, ColorType::RGB(bits));
            assert!(decoded == expected, "{bits}-bit, compress {compress}");
            for band in [1, 7, 64] {
                let banded = write((width, height), &pixels, &options, None, band);
                assert!(
                    banded == whole,
                    "{bits}-bit, compress {compress}: bands of {band} write other bytes"
                );
            }
            let mut decoder = Decoder::new(Cursor::new(&whole)).unwrap();
            let compression = decoder.get_tag_u32(Tag::Compression).unwrap();
            assert_eq!(compression, if compress { 8 } else { 1 });
            let strips = decoder.get_tag_u32_vec(Tag::StripOffsets).unwrap().len();
            assert!(strips > 1, "{bits}-bit: more than one strip");
        }
        let plain = write(
            (width, height),
            &pixels,
            &options(depth, false, None),
            None,
            64,
        );
        let deflated = write(
            (width, height),
            &pixels,
            &options(depth, true, None),
            None,
            64,
        );
        assert!(
            deflated.len() < plain.len(),
            "{bits}-bit: Deflate shrinks it"
        );
    }
}

#[test]
fn the_profile_and_density_are_in_ifd0() {
    let pixels = frame(64, 40, Depth::Eight);
    for (ppi, unit, resolution) in [(Some(300), 2, (300, 1)), (None, 1, (1, 1))] {
        let file = write(
            (64, 40),
            &pixels,
            &options(Depth::Eight, true, ppi),
            None,
            40,
        );
        let mut decoder = Decoder::new(Cursor::new(&file)).unwrap();
        assert_eq!(
            decoder.get_tag_u8_vec(Tag::IccProfile).unwrap(),
            srgb_profile()
        );
        assert_eq!(decoder.get_tag_u32(Tag::ResolutionUnit).unwrap(), unit);
        for tag in [Tag::XResolution, Tag::YResolution] {
            let value = decoder.get_tag(tag).unwrap();
            assert_eq!(
                value,
                tiff::decoder::ifd::Value::Rational(resolution.0, resolution.1),
                "{tag:?} at {ppi:?}"
            );
        }
    }
}

/// Full and GPS-less metadata come back from the repository's own walkers exactly as given, in
/// IFD0, the Exif IFD and the GPS IFD, with the structural fields; without metadata the file holds
/// none of it.
#[test]
fn the_metadata_is_in_ifd0_and_its_exif_and_gps_ifds() {
    let (width, height) = (96, 70);
    let pixels = frame(width, height, Depth::Sixteen);
    let full = capture_metadata(true);
    let without_gps = capture_metadata(false);
    for (metadata, gps) in [(&full, true), (&without_gps, false)] {
        let file = write(
            (width, height),
            &pixels,
            &options(Depth::Sixteen, true, Some(300)),
            Some(metadata),
            16,
        );
        assert_eq!(&CaptureMetadata::from_raw(&file), metadata);
        let header = FileHeader::from_bytes(&file);
        assert_eq!(header.make.as_deref(), Some("NIKON CORPORATION"));
        assert_eq!(header.orientation, Some(1));
        assert_eq!(header.gps.is_some(), gps);
        assert!(header.capture_time.is_some());
        let exif = Reader::new().read_raw(file.clone()).unwrap();
        // Each field's value as `kamadak-exif` reads and shows it.
        let field = |tag| {
            exif.get_field(tag, In::PRIMARY)
                .map(|field| field.display_value().to_string())
        };
        let number = |tag| {
            exif.get_field(tag, In::PRIMARY)
                .and_then(|field| field.value.get_uint(0))
        };
        assert_eq!(field(ExifTag::Software).as_deref(), Some("\"Luxforge\""));
        assert_eq!(field(ExifTag::Artist).as_deref(), Some("\"Will\""));
        assert_eq!(number(ExifTag::Orientation), Some(1));
        assert_eq!(number(ExifTag::ColorSpace), Some(1));
        assert_eq!(number(ExifTag::PixelXDimension), Some(width));
        assert_eq!(number(ExifTag::PixelYDimension), Some(height));
        assert_eq!(
            field(ExifTag::GPSLatitudeRef).is_some(),
            gps,
            "the GPS IFD only with GPS fields"
        );
        // The pixels are unchanged by the directories around them.
        assert!(decode(&file).3 == rgb(&pixels));
    }

    let file = write(
        (width, height),
        &pixels,
        &options(Depth::Sixteen, true, Some(300)),
        None,
        16,
    );
    assert!(CaptureMetadata::from_raw(&file).is_empty());
    let header = FileHeader::from_bytes(&file);
    assert_eq!((header.make, header.orientation), (None, None));
    let exif = Reader::new().read_raw(file.clone()).unwrap();
    let names: Vec<String> = exif.fields().map(|field| field.tag.to_string()).collect();
    for absent in [
        "Software",
        "Orientation",
        "ExifIFDPointer",
        "GPSInfoIFDPointer",
    ] {
        assert!(
            !names.iter().any(|name| name == absent),
            "{absent}: {names:?}"
        );
    }
    // Empty metadata still writes the structural fields and the Exif IFD.
    let file = write(
        (width, height),
        &pixels,
        &options(Depth::Sixteen, false, None),
        Some(&CaptureMetadata::default()),
        16,
    );
    let exif = Reader::new().read_raw(file).unwrap();
    assert!(exif.get_field(ExifTag::Software, In::PRIMARY).is_some());
    assert!(exif.get_field(ExifTag::ColorSpace, In::PRIMARY).is_some());
    assert!(
        exif.get_field(ExifTag::GPSLatitudeRef, In::PRIMARY)
            .is_none()
    );
}

#[test]
fn a_file_too_large_for_32_bit_offsets_is_refused_before_anything_is_written() {
    // 16-bit samples: about 4.7 GB of samples.
    let mut out = Cursor::new(Vec::new());
    let options = options(Depth::Sixteen, true, None);
    let error = TiffWriter::start(&mut out, 28_000, 28_000, &options, &[], None)
        .err()
        .unwrap();
    assert_eq!(error.kind, ErrorKind::Validation);
    assert_eq!(error.data.as_deref().unwrap()["code"], TIFF_TOO_LARGE);
    assert!(out.get_ref().is_empty());
    // The largest 8-bit square that fits is far larger than any camera's frame.
    assert!(tiff_worst_case_bytes(36_000, 36_000, Depth::Eight, 1024) < MAX_FILE_BYTES);
    assert!(tiff_worst_case_bytes(26_000, 26_000, Depth::Sixteen, 1024) < MAX_FILE_BYTES);
}

#[test]
fn the_writer_writes_from_the_outputs_position() {
    let pixels = frame(40, 30, Depth::Eight);
    let mut out = Cursor::new(b"before".to_vec());
    out.set_position(6);
    let mut writer =
        TiffWriter::start(out, 40, 30, &options(Depth::Eight, true, None), &[], None).unwrap();
    for (rows, band) in bands(&pixels, 40, 9) {
        writer
            .write_rows(rows, band, &mut |_| {}, &|| Ok(()))
            .unwrap();
    }
    let out = writer.finish().unwrap();
    assert_eq!(
        out.position() as usize,
        out.get_ref().len(),
        "left at the end"
    );
    let file = out.into_inner();
    assert_eq!(&file[..6], b"before");
    assert!(decode(&file[6..]).3 == rgb(&pixels));
}

/// Progress rises to 1 at most about once per 1%; the cancel is checked before every strip, and
/// its error stops the writing with nothing more written.
#[test]
fn progress_rises_to_one_and_a_cancel_stops_between_strips() {
    let (width, height) = (1031, 300);
    let pixels = frame(width, height, Depth::Sixteen);
    let options = options(Depth::Sixteen, true, None);
    let strips = height.div_ceil(strips::rows_per_strip(width, Depth::Sixteen));
    let mut seen = Vec::new();
    let checks = Cell::new(0u32);
    let mut writer =
        TiffWriter::start(Cursor::new(Vec::new()), width, height, &options, &[], None).unwrap();
    for (rows, band) in bands(&pixels, width, 5) {
        writer
            .write_rows(rows, band, &mut |fraction| seen.push(fraction), &|| {
                checks.set(checks.get() + 1);
                Ok(())
            })
            .unwrap();
    }
    writer.finish().unwrap();
    assert_eq!(checks.get(), strips, "one check a strip");
    assert!(seen.windows(2).all(|pair| pair[0] < pair[1]), "{seen:?}");
    assert_eq!(seen.last(), Some(&1.0));

    let mut writer =
        TiffWriter::start(Cursor::new(Vec::new()), width, height, &options, &[], None).unwrap();
    checks.set(0);
    let cancel = || {
        checks.set(checks.get() + 1);
        if checks.get() == 3 {
            Err(Error::cancelled("stop"))
        } else {
            Ok(())
        }
    };
    let (_, band) = bands(&pixels, width, height)[0];
    let error = writer
        .write_rows(height, band, &mut |_| {}, &cancel)
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(writer.strips.rows(), 2 * writer.strips.rows_per_strip());
}

#[test]
fn bands_that_are_not_their_rows_and_an_early_finish_are_refused() {
    let pixels = frame(20, 10, Depth::Eight);
    let start = || {
        TiffWriter::start(
            Cursor::new(Vec::new()),
            20,
            10,
            &options(Depth::Eight, false, None),
            &[],
            None,
        )
        .unwrap()
    };
    let Pixels::Eight(rgba) = &pixels else {
        unreachable!()
    };
    let sixteen = vec![0u16; 20 * 4];
    for (rows, band, detail) in [
        (
            2,
            PixelRows::Eight(&rgba[..20 * 4]),
            "a band's pixels are not the rows it declares",
        ),
        (
            1,
            PixelRows::Sixteen(&sixteen),
            "a band's bit depth is not the TIFF export's",
        ),
    ] {
        let error = start()
            .write_rows(rows, band, &mut |_| {}, &|| Ok(()))
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Internal);
        assert!(error.detail.contains(detail), "{error:?}");
    }
    let mut writer = start();
    writer
        .write_rows(10, pixels.rows(), &mut |_| {}, &|| Ok(()))
        .unwrap();
    let error = writer
        .write_rows(
            1,
            PixelRows::Eight(&rgba[..20 * 4]),
            &mut |_| {},
            &|| Ok(()),
        )
        .unwrap_err();
    assert!(error.detail.contains("past the TIFF export's last row"));
    let mut writer = start();
    writer
        .write_rows(
            1,
            PixelRows::Eight(&rgba[..20 * 4]),
            &mut |_| {},
            &|| Ok(()),
        )
        .unwrap();
    let error = writer.finish().err().unwrap();
    assert_eq!(error.kind, ErrorKind::Internal);
    assert!(error.detail.contains("before the last row"));
}

#[test]
fn a_write_failure_is_a_file_access_error() {
    struct Full(Cursor<Vec<u8>>);
    impl Write for Full {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.0.position() + bytes.len() as u64 > 100 {
                return Err(io::Error::new(
                    io::ErrorKind::StorageFull,
                    "the disk is full",
                ));
            }
            self.0.write(bytes)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl Seek for Full {
        fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
            self.0.seek(to)
        }
    }
    let pixels = frame(64, 64, Depth::Eight);
    let mut writer = TiffWriter::start(
        Full(Cursor::new(Vec::new())),
        64,
        64,
        &options(Depth::Eight, false, None),
        &[],
        None,
    )
    .unwrap();
    let error = writer
        .write_rows(64, pixels.rows(), &mut |_| {}, &|| Ok(()))
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::FileAccess);
    assert!(error.detail.contains("the disk is full"), "{error:?}");
}
