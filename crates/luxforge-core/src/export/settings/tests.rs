//! The settings' JSON shape and validation by field, the output-size arithmetic against an
//! independent floating-point calculation, the held-memory estimate and the plan's notices.
use super::*;
use crate::ErrorKind;

fn jpeg() -> Value {
    json!({
        "format": "jpeg", "size": {"fit": "full"}, "quality": 90, "optimise": false,
        "chroma": "444", "limit_bytes": null, "sharpen": null, "metadata": "all",
        "pixels_per_inch": null, "folder": "original", "suffix": "-edited",
    })
}

fn webp() -> Value {
    json!({
        "format": "webp", "size": {"fit": "long_edge", "pixels": 2048}, "quality": 80,
        "optimise": true, "limit_bytes": 512000, "sharpen": null, "metadata": "copyright",
        "pixels_per_inch": 72, "folder": {"path": "/Users/someone/Exports"}, "suffix": "-web",
    })
}

fn tiff() -> Value {
    json!({
        "format": "tiff", "size": {"fit": "within", "width": 3000, "height": 2000},
        "optimise": true, "bit_depth": 16, "sharpen": null, "metadata": "no_location",
        "pixels_per_inch": 300, "folder": "original", "suffix": "",
    })
}

/// `base` with `field` set to `value`, or removed for `None`.
fn with(base: Value, field: &str, value: Option<Value>) -> Value {
    let mut settings = base;
    let object = settings.as_object_mut().unwrap();
    match value {
        Some(value) => {
            object.insert(field.into(), value);
        }
        None => {
            object.remove(field);
        }
    }
    settings
}

fn refusal(settings: &Value) -> Error {
    ExportSettings::from_json(settings).expect_err(&format!("refused: {settings}"))
}

/// A refusal names its field in the message and in `data.field`.
fn assert_names(settings: Value, field: &str) {
    let error = refusal(&settings);
    assert_eq!(error.kind, ErrorKind::Validation, "{settings}");
    let named = format!("settings.{field}");
    assert!(
        error.detail.starts_with(&named),
        "{settings}: {} does not name {named}",
        error.detail
    );
    assert_eq!(error.data.as_deref().unwrap()["field"], json!(named));
}

#[test]
fn every_format_round_trips_its_exact_shape() {
    for value in [jpeg(), webp(), tiff()] {
        let settings = ExportSettings::from_json(&value).unwrap();
        assert_eq!(settings.to_json(), value);
        let through_serde: ExportSettings = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(through_serde, settings);
        assert_eq!(serde_json::to_value(&settings).unwrap(), value);
    }
    let webp = ExportSettings::from_json(&webp()).unwrap();
    assert_eq!(webp.format(), ExportFormat::Webp);
    assert_eq!(
        (webp.quality(), webp.limit_bytes(), webp.chroma()),
        (Some(80), Some(512_000), None)
    );
    let tiff = ExportSettings::from_json(&tiff()).unwrap();
    assert_eq!(tiff.depth(), Depth::Sixteen);
    assert_eq!((tiff.quality(), tiff.limit_bytes()), (None, None));
}

#[test]
fn the_baseline_is_todays_export_and_its_json_is_valid() {
    let baseline = ExportSettings::baseline(MetadataLevel::None);
    baseline.check().unwrap();
    assert_eq!(
        ExportSettings::from_json(&baseline.to_json()).unwrap(),
        baseline
    );
    assert_eq!(baseline.quality(), Some(90));
    assert_eq!(baseline.chroma(), Some(Chroma::Full));
    assert!(!baseline.optimise);
}

#[test]
fn every_field_is_validated_by_name_within_its_range() {
    let cases: Vec<(Value, &str)> = vec![
        (json!("jpeg"), "settings"),
        (with(jpeg(), "format", Some(json!("png"))), "format"),
        (with(jpeg(), "format", None), "format"),
        (with(jpeg(), "colour", Some(json!("srgb"))), "colour"),
        (with(jpeg(), "size", Some(json!({"fit": "huge"}))), "size"),
        (
            with(
                jpeg(),
                "size",
                Some(json!({"fit": "long_edge", "pixels": 15})),
            ),
            "size",
        ),
        (
            with(
                jpeg(),
                "size",
                Some(json!({"fit": "long_edge", "pixels": 65_536})),
            ),
            "size",
        ),
        (
            with(
                jpeg(),
                "size",
                Some(json!({"fit": "within", "width": 2000})),
            ),
            "size",
        ),
        (
            with(jpeg(), "size", Some(json!({"fit": "full", "pixels": 100}))),
            "size",
        ),
        (with(jpeg(), "quality", Some(json!(0))), "quality"),
        (with(jpeg(), "quality", Some(json!(101))), "quality"),
        (with(jpeg(), "quality", Some(json!(90.5))), "quality"),
        (with(jpeg(), "optimise", Some(json!("yes"))), "optimise"),
        (with(jpeg(), "chroma", Some(json!(444))), "chroma"),
        (with(jpeg(), "chroma", Some(json!("422"))), "chroma"),
        (
            with(jpeg(), "limit_bytes", Some(json!(10_239))),
            "limit_bytes",
        ),
        (
            with(jpeg(), "limit_bytes", Some(json!(1_073_741_825_u64))),
            "limit_bytes",
        ),
        (with(tiff(), "bit_depth", Some(json!(12))), "bit_depth"),
        (with(jpeg(), "metadata", Some(json!("some"))), "metadata"),
        (
            with(jpeg(), "pixels_per_inch", Some(json!(0))),
            "pixels_per_inch",
        ),
        (
            with(jpeg(), "pixels_per_inch", Some(json!(65_536))),
            "pixels_per_inch",
        ),
        (with(jpeg(), "folder", Some(json!("desktop"))), "folder"),
        (
            with(jpeg(), "folder", Some(json!({"path": "relative/exports"}))),
            "folder",
        ),
        (
            with(jpeg(), "folder", Some(json!({"path": "/a", "other": 1}))),
            "folder",
        ),
        (
            with(jpeg(), "suffix", Some(json!("x".repeat(65)))),
            "suffix",
        ),
        (with(jpeg(), "suffix", Some(json!("-a/b"))), "suffix"),
        (with(jpeg(), "suffix", Some(json!("-a\\b"))), "suffix"),
        (with(jpeg(), "suffix", Some(json!("-a\u{7}"))), "suffix"),
        (with(jpeg(), "suffix", Some(json!(".hidden"))), "suffix"),
        (with(jpeg(), "suffix", Some(json!(7))), "suffix"),
    ];
    for (settings, field) in cases {
        if field == "settings" {
            assert_eq!(refusal(&settings).kind, ErrorKind::Validation);
            continue;
        }
        assert_names(settings, field);
    }
    // Each bound and limit is inclusive.
    for (field, value) in [
        ("quality", json!(1)),
        ("quality", json!(100)),
        ("limit_bytes", json!(10_240)),
        ("limit_bytes", json!(1_073_741_824_u64)),
        ("pixels_per_inch", json!(65_535)),
        ("suffix", json!("é".repeat(64))),
        (
            "size",
            json!({"fit": "within", "width": 16, "height": 65_535}),
        ),
    ] {
        ExportSettings::from_json(&with(jpeg(), field, Some(value.clone())))
            .unwrap_or_else(|error| panic!("{field} {value}: {}", error.detail));
    }
}

#[test]
fn a_field_the_format_does_not_use_is_refused_even_as_null_and_one_it_uses_is_required() {
    for (base, field) in [
        (tiff(), "quality"),
        (tiff(), "chroma"),
        (tiff(), "limit_bytes"),
        (jpeg(), "bit_depth"),
        (webp(), "chroma"),
        (webp(), "bit_depth"),
    ] {
        for value in [json!(null), json!(8)] {
            let settings = with(base.clone(), field, Some(value));
            assert_names(settings.clone(), field);
            assert!(refusal(&settings).detail.contains("does not apply"));
        }
    }
    for (base, field) in [
        (jpeg(), "quality"),
        (jpeg(), "chroma"),
        (jpeg(), "limit_bytes"),
        (webp(), "limit_bytes"),
        (tiff(), "bit_depth"),
        (tiff(), "sharpen"),
        (jpeg(), "suffix"),
        (jpeg(), "pixels_per_inch"),
    ] {
        let settings = with(base, field, None);
        assert_names(settings.clone(), field);
        assert!(refusal(&settings).detail.contains("is required"));
    }
}

#[test]
fn sharpening_takes_every_medium_and_amount_in_every_format_and_refuses_anything_else_by_name() {
    use crate::tiles::sharpen::{Sharpen, SharpenAmount, SharpenMedium};
    for base in [jpeg(), webp(), tiff()] {
        for medium in SharpenMedium::ALL {
            for amount in SharpenAmount::ALL {
                let value = json!({"medium": medium.as_str(), "amount": amount.as_str()});
                let settings = with(base.clone(), "sharpen", Some(value.clone()));
                let read = ExportSettings::from_json(&settings).unwrap();
                assert_eq!(read.sharpen, Some(Sharpen { medium, amount }));
                assert_eq!(read.to_json(), settings, "it writes back what it read");
            }
        }
    }
    for value in [
        json!("screen"),
        json!({"medium": "canvas", "amount": "standard"}),
        json!({"medium": "matte", "amount": "extreme"}),
        json!({"medium": "matte"}),
        json!({"medium": "matte", "amount": "low", "radius": 2}),
    ] {
        let settings = with(jpeg(), "sharpen", Some(value));
        assert_names(settings.clone(), "sharpen");
    }
}

#[test]
fn the_output_spec_sharpens_for_the_settings_density_or_300_ppi() {
    use crate::tiles::sharpen::{OutputSharpen, Sharpen, SharpenAmount, SharpenMedium};
    let sharpen = Sharpen {
        medium: SharpenMedium::Glossy,
        amount: SharpenAmount::Standard,
    };
    let mut settings = ExportSettings {
        sharpen: Some(sharpen),
        ..ExportSettings::baseline(MetadataLevel::All)
    };
    assert_eq!(
        settings.output_spec((600, 400)).sharpen,
        Some(OutputSharpen {
            sharpen,
            pixels_per_inch: 300
        })
    );
    settings.pixels_per_inch = Some(240);
    assert_eq!(
        settings
            .output_spec((600, 400))
            .sharpen
            .map(|s| s.pixels_per_inch),
        Some(240)
    );
    settings.sharpen = None;
    assert_eq!(settings.output_spec((600, 400)).sharpen, None);
}

#[test]
fn settings_built_in_code_are_checked_by_the_same_rules() {
    let mut settings = ExportSettings::baseline(MetadataLevel::All);
    settings.size = OutputSize::LongEdge { pixels: 8 };
    assert!(
        settings
            .check()
            .unwrap_err()
            .detail
            .starts_with("settings.size")
    );
    let mut settings = ExportSettings::baseline(MetadataLevel::All);
    settings.folder = ExportFolder::Path(PathBuf::from("exports"));
    assert!(
        settings
            .check()
            .unwrap_err()
            .detail
            .starts_with("settings.folder")
    );
    let mut settings = ExportSettings::baseline(MetadataLevel::All);
    settings.format = FormatSettings::Webp {
        quality: 0,
        limit_bytes: None,
    };
    assert!(
        settings
            .check()
            .unwrap_err()
            .detail
            .starts_with("settings.quality")
    );
}

/// The output by an independent route: the factor as a float, never above 1, each side rounded
/// half up and at least 1.
fn expected((width, height): (u32, u32), size: OutputSize) -> (u32, u32) {
    let (w, h) = (f64::from(width), f64::from(height));
    let factor = match size {
        OutputSize::Full => 1.0,
        OutputSize::LongEdge { pixels } => f64::from(pixels) / w.max(h),
        OutputSize::Within {
            width: bound_width,
            height: bound_height,
        } => (f64::from(bound_width) / w).min(f64::from(bound_height) / h),
    }
    .min(1.0);
    let side = |side: f64| ((side * factor + 0.5).floor() as u32).max(1);
    (side(w), side(h))
}

#[test]
fn output_sizes_match_an_independent_calculation_and_never_enlarge() {
    let stages = [
        ("landscape", (6000, 4000)),
        ("portrait", (4000, 6000)),
        ("cropped", (3217, 2011)),
        ("cropped and rotated", (2011, 3217)),
        ("square", (2500, 2500)),
        ("sliver", (9000, 40)),
    ];
    for (name, (width, height)) in stages {
        let long = width.max(height);
        let sizes = [
            OutputSize::Full,
            // Above, at and below the stage's long edge.
            OutputSize::LongEdge { pixels: long + 500 },
            OutputSize::LongEdge { pixels: long },
            OutputSize::LongEdge { pixels: long / 3 },
            OutputSize::LongEdge { pixels: 16 },
            // Above, at and below the stage on both sides, and below on one only.
            OutputSize::Within {
                width: width + 100,
                height: height + 100,
            },
            OutputSize::Within { width, height },
            OutputSize::Within {
                width: (width / 2).max(16),
                height: (height / 3).max(16),
            },
            OutputSize::Within {
                width: width + 100,
                height: (height / 4).max(16),
            },
            OutputSize::Within {
                width: (width / 5).max(16),
                height: height + 100,
            },
        ];
        for size in sizes {
            let output = size.apply((width, height));
            assert_eq!(
                output,
                expected((width, height), size),
                "{name} {width}x{height} {size:?}"
            );
            assert!(
                output.0 <= width && output.1 <= height,
                "{name} {size:?} enlarged to {output:?}"
            );
            match size {
                OutputSize::LongEdge { pixels } => {
                    assert_eq!(output.0.max(output.1), long.min(pixels), "{name} {size:?}")
                }
                OutputSize::Within {
                    width: bound_width,
                    height: bound_height,
                } => assert!(
                    output.0 <= bound_width && output.1 <= bound_height,
                    "{name} {size:?} {output:?}"
                ),
                OutputSize::Full => assert_eq!(output, (width, height)),
            }
        }
    }
}

/// The plan's held-memory estimate is the encoders' own figures, at 24 MP and 60 MP outputs (a
/// Z6's and a 60 MP sensor's): an optimised JPEG's coefficient arrays as libjpeg pads them (the
/// figure `luxforge-jpeg` measures against libjpeg's own pools), 12 bytes a pixel at 4:4:4 and 6 at
/// 4:2:0 where nothing pads; a size limit's frame at 4 bytes a pixel beside one attempt's arrays and
/// two files at most the limit; a WebP's frame and libwebp's state (the figure `luxforge-webp`'s
/// footprint tests bound), with the best file so far under a limit; nothing for a streamed JPEG or a
/// TIFF.
#[test]
fn the_held_memory_estimate_is_the_encoders_own_at_24_and_60_megapixels() {
    use crate::export::{
        encode::{JpegOptions, encoder_bytes},
        webp::webp_held_bytes,
    };
    for (width, height) in [(6000, 4000), (9504, 6336)] {
        let output = (width, height);
        let pixels = u64::from(width) * u64::from(height);
        let frame = pixels * 4;
        let jpeg = |optimise, chroma, limit_bytes| ExportSettings {
            format: FormatSettings::Jpeg {
                quality: 80,
                chroma,
                limit_bytes,
            },
            optimise,
            ..ExportSettings::baseline(MetadataLevel::None)
        };
        let arrays = |chroma| {
            encoder_bytes(
                width,
                height,
                &JpegOptions {
                    quality: 80,
                    chroma,
                    optimise: true,
                },
            )
        };
        assert_eq!(
            arrays(Chroma::Full),
            pixels * 12,
            "{output:?}: no edge pads"
        );
        assert_eq!(arrays(Chroma::Half), pixels * 6, "{output:?}: no edge pads");
        assert_eq!(jpeg(false, Chroma::Full, None).held_bytes(output), 0);
        assert_eq!(
            jpeg(true, Chroma::Full, None).held_bytes(output),
            arrays(Chroma::Full)
        );
        assert_eq!(
            jpeg(true, Chroma::Half, None).held_bytes(output),
            arrays(Chroma::Half)
        );
        assert_eq!(
            jpeg(false, Chroma::Half, Some(500_000)).held_bytes(output),
            frame + 2 * 500_000
        );
        assert_eq!(
            jpeg(true, Chroma::Half, Some(500_000)).held_bytes(output),
            frame + arrays(Chroma::Half) + 2 * 500_000
        );
        let mut webp = ExportSettings::from_json(&webp()).unwrap();
        assert_eq!(
            webp.held_bytes(output),
            frame + webp_held_bytes(width, height, true) + 512_000
        );
        webp.format = FormatSettings::Webp {
            quality: 80,
            limit_bytes: None,
        };
        webp.optimise = false;
        assert_eq!(
            webp.held_bytes(output),
            frame + webp_held_bytes(width, height, false)
        );
        let tiff = ExportSettings::from_json(&tiff()).unwrap();
        assert_eq!(tiff.held_bytes(output), 0);
    }
    // The design's figure for an optimised 60 MP JPEG at 4:4:4: about 720 MB.
    let sixty = ExportSettings {
        optimise: true,
        ..ExportSettings::baseline(MetadataLevel::None)
    };
    assert_eq!(sixty.held_bytes((9504, 6336)).div_ceil(1_000_000), 723);
}

#[test]
fn the_large_image_notice_appears_exactly_above_24_megapixels_of_held_output() {
    let folder = luxforge_testbase::paths::temp_dir("export-settings-notices");
    let codes = |settings: &ExportSettings, output| -> Vec<&'static str> {
        settings
            .notices(output, Some(&folder))
            .into_iter()
            .map(|notice| notice.code)
            .collect()
    };
    let mut optimised = ExportSettings::baseline(MetadataLevel::None);
    optimised.optimise = true;
    // 24,000,000 pixels is at the threshold, 24,000,001 above it.
    let at = (6000, 4000);
    let above = (24_000_001, 1);
    assert!(codes(&optimised, at).is_empty());
    let notices = optimised.notices(above, Some(&folder));
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].code, LARGE_IMAGE);
    assert!(!notices[0].refuses);
    assert_eq!(notices[0].bytes, Some(optimised.held_bytes(above)));
    // A streamed JPEG holds nothing, whatever its size; a limit holds its frame.
    let streamed = ExportSettings::baseline(MetadataLevel::None);
    assert!(codes(&streamed, above).is_empty());
    let mut limited = streamed.clone();
    limited.format = FormatSettings::Jpeg {
        quality: 90,
        chroma: Chroma::Full,
        limit_bytes: Some(1_000_000),
    };
    assert_eq!(codes(&limited, above), [LARGE_IMAGE]);
    assert!(codes(&limited, at).is_empty());
    // A TIFF streams, optimised or not.
    let tiff = ExportSettings::from_json(&tiff()).unwrap();
    assert!(codes(&tiff, above).is_empty());
}

#[test]
fn a_webp_side_above_16383_and_a_missing_folder_refuse_the_export() {
    let folder = luxforge_testbase::paths::temp_dir("export-settings-webp");
    let mut webp = ExportSettings::from_json(&webp()).unwrap();
    webp.format = FormatSettings::Webp {
        quality: 80,
        limit_bytes: None,
    };
    assert!(webp.notices((16_383, 100), Some(&folder)).is_empty());
    let notices = webp.notices((16_384, 100), Some(&folder));
    assert_eq!(notices.len(), 1);
    assert_eq!(
        (notices[0].code, notices[0].refuses),
        (WEBP_TOO_LARGE, true)
    );
    let refusal = notices[0].refusal();
    assert_eq!(refusal.kind, ErrorKind::Validation);
    assert_eq!(refusal.data.as_deref().unwrap()["notice"], WEBP_TOO_LARGE);

    let baseline = ExportSettings::baseline(MetadataLevel::None);
    for folder in [Some(folder.join("gone").as_path()), None] {
        let notices = baseline.notices((100, 100), folder);
        assert_eq!(notices.len(), 1);
        assert_eq!(
            (notices[0].code, notices[0].refuses),
            (FOLDER_MISSING, true)
        );
    }
}

#[test]
fn a_folder_resolves_to_the_originals_or_the_chosen_one() {
    let original = Path::new("/Volumes/Card/DCIM/DSC_0042.NEF");
    assert_eq!(
        ExportFolder::Original.resolve(Some(original)),
        Some(PathBuf::from("/Volumes/Card/DCIM"))
    );
    assert_eq!(ExportFolder::Original.resolve(None), None);
    let chosen = ExportFolder::Path(PathBuf::from("/Users/someone/Exports"));
    assert_eq!(
        chosen.resolve(None),
        Some(PathBuf::from("/Users/someone/Exports"))
    );
}

/// A WebP of any size within the format's 16,383 px a side is written, with no memory cap: above
/// 24 MP it carries `large-image`, which warns and never refuses (owner, 2026-10-13); a TIFF that
/// could pass the format's 4 GiB is refused as `tiff-too-large`, by the writer's own bound.
#[test]
fn a_large_webp_is_warned_and_a_tiff_past_4_gib_refuses_the_export() {
    let folder = luxforge_testbase::paths::temp_dir("export-settings-too-large");
    let codes = |settings: &ExportSettings, output| -> Vec<(&'static str, bool)> {
        settings
            .notices(output, Some(&folder))
            .into_iter()
            .map(|notice| (notice.code, notice.refuses))
            .collect()
    };
    let mut webp = ExportSettings::from_json(&webp()).unwrap();
    webp.format = FormatSettings::Webp {
        quality: 80,
        limit_bytes: None,
    };
    // 48 MP and the largest WebP, 268 MP, past the 512 MiB frame limit plain or optimised: warned
    // with the held bytes, never refused; 24 MP is below the threshold.
    for optimise in [false, true] {
        webp.optimise = optimise;
        for output in [(8000, 6000), (WEBP_MAX_SIDE, WEBP_MAX_SIDE)] {
            assert_eq!(codes(&webp, output), [(LARGE_IMAGE, false)], "{output:?}");
            let notice = &webp.notices(output, Some(&folder))[0];
            assert_eq!(notice.bytes, Some(webp.held_bytes(output)));
        }
        assert!(codes(&webp, (6000, 4000)).is_empty());
    }
    // Past the format's side, refused.
    assert_eq!(
        codes(&webp, (WEBP_MAX_SIDE + 1, 100)),
        [(WEBP_TOO_LARGE, true)]
    );

    let mut tiff = ExportSettings::from_json(&tiff()).unwrap();
    // 750 MP: a 16-bit TIFF could pass 4 GiB, an 8-bit one cannot.
    let huge = (30_000, 25_000);
    assert_eq!(codes(&tiff, huge), [(TIFF_TOO_LARGE, true)]);
    assert!(!crate::export::tiff::tiff_fits(
        30_000,
        25_000,
        Depth::Sixteen
    ));
    tiff.format = FormatSettings::Tiff {
        bit_depth: Depth::Eight,
    };
    assert!(codes(&tiff, huge).is_empty());
    assert!(crate::export::tiff::tiff_fits(30_000, 25_000, Depth::Eight));
}
