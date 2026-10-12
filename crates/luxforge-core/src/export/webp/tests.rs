//! The WebP encoder against independent readers: `image-webp` decodes the file and reads its
//! chunks, the RIFF chunks are walked here, and the EXIF chunk is read by the repository's own
//! reader and by `kamadak-exif`.

use super::*;
use crate::{
    ErrorKind,
    export::{encode::srgb_profile, metadata::CaptureMetadata, tiff::tests::capture_metadata},
};
use exif::{In, Reader, Tag};
use image_webp::WebPDecoder;
use std::{cell::Cell, io::Cursor};

fn frame(width: u32, height: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let r = (x * 255 / width) as u8;
            let g = (y * 255 / height) as u8;
            let b = ((x + y) % 64) as u8 + 100;
            rgba.extend_from_slice(&[r, g, b, 255]);
        }
    }
    rgba
}

fn encode(
    (width, height): (u32, u32),
    rgba: &[u8],
    options: &WebpOptions,
    exif: Option<&[u8]>,
) -> Vec<u8> {
    encode_webp(
        width,
        height,
        rgba,
        options,
        srgb_profile(),
        exif,
        &mut |_| {},
        &|| Ok(()),
    )
    .expect("encodes")
}

/// The chunks of a WebP file after its RIFF header, as `(fourcc, payload)`.
fn chunks(file: &[u8]) -> Vec<([u8; 4], &[u8])> {
    assert_eq!(
        (&file[0..4], &file[8..12]),
        (b"RIFF".as_slice(), b"WEBP".as_slice())
    );
    let mut found = Vec::new();
    let mut at = 12;
    while at < file.len() {
        let len = u32::from_le_bytes(file[at + 4..at + 8].try_into().unwrap()) as usize;
        found.push((
            file[at..at + 4].try_into().unwrap(),
            &file[at + 8..at + 8 + len],
        ));
        at += 8 + len + len % 2;
    }
    found
}

/// Each metadata level the encoders are handed (all, all but location, none) gives a file of the
/// output's size with the sRGB profile as its `ICCP` chunk and exactly that level's fields in its
/// `EXIF` chunk, at every quality and with Optimise on or off.
#[test]
fn files_decode_to_the_output_with_the_profile_and_the_levels_exif() {
    let (width, height) = (301, 157);
    let rgba = frame(width, height);
    let full = capture_metadata(true);
    let without_gps = capture_metadata(false);
    let levels = [Some(&full), Some(&without_gps), None];
    for (index, metadata) in levels.into_iter().enumerate() {
        let payload = metadata.map(|metadata| metadata.exif_payload(width, height));
        let options = WebpOptions {
            quality: [80, 1, 100][index],
            optimise: index != 1,
        };
        let file = encode((width, height), &rgba, &options, payload.as_deref());
        let mut decoder = WebPDecoder::new(Cursor::new(&file)).expect("a WebP file");
        assert_eq!(decoder.dimensions(), (width, height));
        assert!(decoder.is_lossy());
        let mut rgb = vec![0; decoder.output_buffer_size().unwrap()];
        decoder.read_image(&mut rgb).expect("decodes");
        assert_eq!(
            decoder.icc_profile().unwrap().as_deref(),
            Some(srgb_profile())
        );
        let found = chunks(&file);
        let iccp = found.iter().find(|(fourcc, _)| fourcc == b"ICCP").unwrap();
        crate::profile::check(iccp.1, 3).expect("the sRGB profile");
        let exif = found.iter().find(|(fourcc, _)| fourcc == b"EXIF");
        let Some(metadata) = metadata else {
            assert!(exif.is_none(), "no EXIF chunk without metadata");
            assert_eq!(decoder.exif_metadata().unwrap(), None);
            continue;
        };
        let exif = exif.expect("an EXIF chunk").1;
        assert_eq!(&CaptureMetadata::from_raw(exif), metadata);
        let fields = Reader::new().read_raw(exif.to_vec()).unwrap();
        let has = |tag| fields.get_field(tag, In::PRIMARY).is_some();
        assert!(has(Tag::Software) && has(Tag::Artist) && has(Tag::Copyright));
        assert_eq!(has(Tag::GPSLatitude), metadata == &full);
        let pixel_x = fields.get_field(Tag::PixelXDimension, In::PRIMARY).unwrap();
        assert_eq!(pixel_x.value.get_uint(0), Some(width));
    }
}

#[test]
fn an_output_over_16383_pixels_a_side_is_refused_before_encoding() {
    for (width, height) in [(16_384, 10), (10, 16_384)] {
        let calls = Cell::new(0);
        let error = encode_webp(
            width,
            height,
            &[],
            &WebpOptions {
                quality: 80,
                optimise: false,
            },
            srgb_profile(),
            None,
            &mut |_| calls.set(calls.get() + 1),
            &|| {
                calls.set(calls.get() + 1);
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Validation);
        let data = error.data.as_deref().unwrap();
        assert_eq!(data["code"], WEBP_TOO_LARGE);
        assert_eq!(data["limit"], 16_383);
        assert_eq!(calls.get(), 0, "nothing ran");
        assert!(!webp_fits(width, height));
    }
    assert!(webp_fits(16_383, 16_383));
    // The largest output's holdings beyond its frame: about 1.07 GB plain, 2.95 GB optimised.
    assert_eq!(webp_held_bytes(16_383, 16_383, false), 16_383 * 16_383 * 4);
    assert_eq!(webp_held_bytes(16_383, 16_383, true), 16_383 * 16_383 * 11);
}

#[test]
fn progress_rises_to_one_and_a_cancel_stops_the_encode_with_its_error() {
    let rgba = frame(512, 384);
    let options = WebpOptions {
        quality: 80,
        optimise: true,
    };
    let mut seen = Vec::new();
    encode_webp(
        512,
        384,
        &rgba,
        &options,
        srgb_profile(),
        None,
        &mut |fraction| seen.push(fraction),
        &|| Ok(()),
    )
    .unwrap();
    assert!(seen.windows(2).all(|pair| pair[0] < pair[1]), "{seen:?}");
    assert_eq!(seen.last(), Some(&1.0));

    for stop_at in [1, 4] {
        let checks = Cell::new(0);
        let error = encode_webp(
            512,
            384,
            &rgba,
            &options,
            srgb_profile(),
            None,
            &mut |_| {},
            &|| {
                checks.set(checks.get() + 1);
                if checks.get() == stop_at {
                    Err(Error::cancelled("stop"))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled);
        assert_eq!(error.detail, "stop");
        assert_eq!(checks.get(), stop_at, "no check after the one that stopped");
    }
}

#[test]
fn a_frame_that_is_not_its_size_is_internal() {
    let rgba = frame(16, 16);
    let error = encode_webp(
        16,
        17,
        &rgba,
        &WebpOptions {
            quality: 80,
            optimise: false,
        },
        &[],
        None,
        &mut |_| {},
        &|| Ok(()),
    )
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Internal);
}
