//! Suitability, exposure resolution, sensor clipping evidence and fusion against the independent
//! f64 reference (`luxforge_reference::hdr`), whose camera records a known scene at chosen
//! exposures with sensor white, the Bayer demosaic's input ceiling and optional noise.
//!
//! Declared tolerances: production fusion against the reference fusion of the same float32
//! samples, `1e-6 · max|reference| + 1e-9` per channel (f64 arithmetic in either order, one f32
//! rounding); noise-free recovered radiance against the scene, `1e-5 · max|scene| + 1e-9`;
//! measured offsets against the recorded ones, 0.02 EV.

use super::*;
use crate::{Error, ErrorKind};
use luxforge_reference::hdr::{self as reference, Camera, Exposure, Noise};

const WIDTH: usize = 96;
const HEIGHT: usize = 36;

fn raw(identity: &str) -> FrameMetadata {
    FrameMetadata {
        identity: identity.into(),
        make: Some("Nikon".into()),
        model: Some("Z 6".into()),
        recording: Recording::Raw {
            mode: "nikon-z6-14-lossless".into(),
            layout: SensorLayout::Bayer,
        },
        width: 6048,
        height: 4024,
        orientation: 1,
        exposure: ExposureSettings {
            shutter_s: None,
            f_number: Some(8.0),
            iso: Some(100),
            bias_ev: None,
            focal_mm: Some(24.0),
        },
        captured_at_ms: None,
    }
}

/// A RAW frame with `shutter` seconds, `bias` EV and a capture instant.
fn shot(identity: &str, shutter: Option<f64>, bias: Option<f64>, at: i64) -> FrameMetadata {
    let mut frame = raw(identity);
    frame.exposure.shutter_s = shutter;
    frame.exposure.bias_ev = bias;
    frame.captured_at_ms = Some(at);
    frame
}

fn bracket(shutters: &[f64], biases: &[f64]) -> Vec<FrameMetadata> {
    shutters
        .iter()
        .zip(biases)
        .enumerate()
        .map(|(i, (s, b))| shot(&format!("f{i}"), Some(*s), Some(*b), 1000 + i as i64))
        .collect()
}

fn resolved(frames: &[FrameMetadata], options: &ExposureOptions) -> ResolvedExposures {
    match resolve_exposures(frames, options).expect("resolves") {
        ExposureResolution::Resolved(resolved) => resolved,
        ExposureResolution::NeedsFit(plan) => panic!("unexpected fit plan {plan:?}"),
    }
}

fn offsets_ev(resolved: &ResolvedExposures) -> Vec<f64> {
    resolved
        .inputs
        .iter()
        .map(|input| input.offset_ev)
        .collect()
}

fn close(a: &[f64], b: &[f64], tolerance: f64) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| (a - b).abs() <= tolerance)
}

#[test]
fn refuses_every_unsuitable_request() {
    let good = || bracket(&[1.0 / 250.0, 1.0 / 60.0, 1.0 / 15.0], &[-2.0, 0.0, 2.0]);
    assert_eq!(validate_bracket(&good()), Ok(()));
    type Change = fn(&mut Vec<FrameMetadata>);
    let cases: Vec<(&str, Change, Refusal)> = vec![
        (
            "one input",
            |f| f.truncate(1),
            Refusal::InputCount { count: 1 },
        ),
        (
            "ten inputs",
            |f| {
                let more: Vec<_> = (3..10)
                    .map(|i| shot(&format!("f{i}"), Some(i as f64), Some(0.0), 2000 + i))
                    .collect();
                f.extend(more);
            },
            Refusal::InputCount { count: 10 },
        ),
        (
            "repeated file",
            |f| f[2].identity = "f0".into(),
            Refusal::RepeatedInput {
                first: 0,
                second: 2,
            },
        ),
        (
            "RAW and JPEG of one exposure",
            |f| {
                let mut jpeg = f[1].clone();
                jpeg.identity = "f1.jpg".into();
                jpeg.recording = Recording::Jpeg;
                f.push(jpeg);
            },
            Refusal::SameExposure {
                first: 1,
                second: 3,
            },
        ),
        (
            "a JPEG exposure",
            |f| {
                f[2].recording = Recording::Jpeg;
            },
            Refusal::RawOnly { input: 2 },
        ),
        (
            "only JPEGs",
            |f| {
                for frame in f.iter_mut() {
                    frame.recording = Recording::Jpeg;
                }
            },
            Refusal::RawOnly { input: 0 },
        ),
        (
            "another body",
            |f| f[1].model = Some("Z 7".into()),
            Refusal::MixedCameras { input: 1 },
        ),
        (
            "an unknown body beside a known one",
            |f| f[2].make = None,
            Refusal::MixedCameras { input: 2 },
        ),
        (
            "another recording mode",
            |f| {
                f[1].recording = Recording::Raw {
                    mode: "nikon-z6-12-lossless".into(),
                    layout: SensorLayout::Bayer,
                }
            },
            Refusal::MixedRecordingModes { input: 1 },
        ),
        (
            "monochrome and colour",
            |f| {
                for frame in f.iter_mut() {
                    frame.recording = Recording::Raw {
                        mode: "mono".into(),
                        layout: SensorLayout::Monochrome,
                    };
                }
                f[2].recording = Recording::Raw {
                    mode: "mono".into(),
                    layout: SensorLayout::Bayer,
                };
            },
            Refusal::MixedColour { input: 2 },
        ),
        (
            "other dimensions",
            |f| f[1].width = 4024,
            Refusal::MixedDimensions { input: 1 },
        ),
        (
            "another aperture",
            |f| f[2].exposure.f_number = Some(5.6),
            Refusal::MixedAperture { input: 2 },
        ),
        (
            "an unrecorded aperture beside recorded ones",
            |f| f[1].exposure.f_number = None,
            Refusal::MixedAperture { input: 1 },
        ),
        (
            "another focal length",
            |f| f[2].exposure.focal_mm = Some(35.0),
            Refusal::MixedFocalLength { input: 2 },
        ),
        (
            // 2 mm from 24 mm is beyond both 1 mm and 2% (0.48 mm).
            "a focal length 2 mm away",
            |f| f[1].exposure.focal_mm = Some(26.0),
            Refusal::MixedFocalLength { input: 1 },
        ),
        (
            "an unrecorded focal length beside recorded ones",
            |f| f[1].exposure.focal_mm = None,
            Refusal::MixedFocalLength { input: 1 },
        ),
    ];
    for (name, change, expected) in cases {
        let mut frames = good();
        change(&mut frames);
        assert_eq!(validate_bracket(&frames), Err(expected.clone()), "{name}");
        assert_eq!(
            resolve_exposures(&frames, &ExposureOptions::default()),
            Err(expected),
            "{name}: resolution validates first"
        );
    }
    // Consistent unknown bodies and manual lenses are not mixtures.
    let mut unknown = good();
    for frame in &mut unknown {
        (frame.make, frame.model, frame.exposure.f_number) = (None, None, None);
    }
    assert_eq!(validate_bracket(&unknown), Ok(()));
    // A RAW and a JPEG of different exposures are refused for the JPEG, not as a pair.
    let mut mixed = good();
    mixed[2].recording = Recording::Jpeg;
    mixed[2].captured_at_ms = mixed[1].captured_at_ms;
    assert_eq!(validate_bracket(&mixed), Err(Refusal::RawOnly { input: 2 }));
    // The error a client sees names the RAW-only scope and carries the typed reason.
    let error = Error::from(Refusal::RawOnly { input: 2 });
    assert_eq!(error.kind, ErrorKind::UnsupportedInput);
    assert!(
        error.detail.contains("RAW exposures only"),
        "{}",
        error.detail
    );
    assert_eq!(
        error.data.as_deref(),
        Some(&serde_json::json!({"reason": "raw-only", "input": 2}))
    );
    assert_eq!(
        Error::from(Refusal::MixedAperture { input: 1 }).kind,
        ErrorKind::Validation
    );
}

#[test]
fn iso_may_vary_and_focal_lengths_round() {
    let good = || bracket(&[1.0 / 250.0, 1.0 / 60.0, 1.0 / 15.0], &[-2.0, 0.0, 2.0]);
    // Auto ISO within a camera's bracket, and an ISO recorded on some inputs only: neither is a
    // suitability question.
    let mut frames = good();
    (frames[0].exposure.iso, frames[2].exposure.iso) = (Some(220), Some(130));
    assert_eq!(validate_bracket(&frames), Ok(()));
    frames[1].exposure.iso = None;
    assert_eq!(validate_bracket(&frames), Ok(()));
    // A zoom's whole-millimetre record flipping at a boundary (a Z 24-200 bracket at 60, 60 and
    // 59 mm), within 1 mm at short focal lengths and 2% at long ones.
    let focal = |values: [f64; 3]| {
        let mut frames = good();
        for (frame, value) in frames.iter_mut().zip(values) {
            frame.exposure.focal_mm = Some(value);
        }
        validate_bracket(&frames)
    };
    assert_eq!(focal([60.0, 60.0, 59.0]), Ok(()));
    assert_eq!(focal([24.0, 25.0, 23.0]), Ok(()));
    assert_eq!(focal([200.0, 196.0, 204.0]), Ok(()));
    assert_eq!(
        focal([200.0, 195.0, 200.0]),
        Err(Refusal::MixedFocalLength { input: 1 })
    );
    assert_eq!(
        focal([24.0, 24.0, 26.0]),
        Err(Refusal::MixedFocalLength { input: 2 })
    );
    // Absent on every input (a manual lens) agrees.
    let mut manual = good();
    for frame in &mut manual {
        frame.exposure.focal_mm = None;
    }
    assert_eq!(validate_bracket(&manual), Ok(()));
}

/// A bracket of `(shutter, ISO, bias)` frames in capture order.
fn iso_bracket(frames: &[(f64, u32, f64)]) -> Vec<FrameMetadata> {
    frames
        .iter()
        .enumerate()
        .map(|(i, &(shutter, iso, bias))| {
            let mut frame = shot(&format!("f{i}"), Some(shutter), Some(bias), 1000 + i as i64);
            frame.exposure.iso = Some(iso);
            frame
        })
        .collect()
}

#[test]
fn settings_count_iso_as_gain() {
    let settings = ExposureOrigin::Metadata {
        source: MetadataSource::Settings,
    };
    // An Air 2S AEB3 bracket (DJI_0606–0608): one shutter, auto ISO 100, 220 and 130 for −0.7,
    // +0.7 and 0 EV. The settings vary through the ISO alone and agree with the bias within half
    // a stop, so they decide: offsets `log2(ISO / 130)` from the zero-bias frame.
    let frames = iso_bracket(&[
        (1.0 / 15.0, 100, -0.7),
        (1.0 / 15.0, 220, 0.7),
        (1.0 / 15.0, 130, 0.0),
    ]);
    let result = resolved(&frames, &ExposureOptions::default());
    assert_eq!(
        (result.reference, result.reference_origin),
        (2, ReferenceOrigin::ZeroBias)
    );
    let want = [(100.0_f64 / 130.0).log2(), (220.0_f64 / 130.0).log2(), 0.0];
    assert!(close(&offsets_ev(&result), &want, 1e-12), "{result:?}");
    assert!(close(&want, &[-0.379, 0.759, 0.0], 1e-3));
    assert_eq!(result.inputs[0].origin, settings);
    // Time and ISO both varying (DJI_0589–0591 style): their stops add.
    let frames = iso_bracket(&[
        (1.0 / 100.0, 110, 0.0),
        (1.0 / 200.0, 110, -0.7),
        (1.0 / 60.0, 100, 0.7),
    ]);
    let result = resolved(&frames, &ExposureOptions::default());
    assert_eq!(result.reference, 0);
    let want = [
        0.0,
        -1.0,
        (100.0_f64 / 60.0).log2() + (100.0_f64 / 110.0).log2(),
    ];
    assert!(close(&offsets_ev(&result), &want, 1e-12), "{result:?}");
    assert_eq!(result.inputs[2].origin, settings);
    // A constant shutter with no ISO recorded on any input is still `log2(t)`, and a manual
    // bracket at varying ISO resolves without a bias.
    let mut frames = bracket(&[1.0 / 250.0, 1.0 / 60.0, 1.0 / 15.0], &[0.0; 3]);
    for frame in &mut frames {
        frame.exposure.iso = None;
    }
    let result = resolved(&frames, &ExposureOptions::default());
    assert!(close(
        &offsets_ev(&result),
        &[(60.0_f64 / 250.0).log2(), 0.0, (60.0_f64 / 15.0).log2()],
        1e-12
    ));
    let frames = iso_bracket(&[(1.0 / 60.0, 100, 0.0), (1.0 / 60.0, 400, 0.0)]);
    let result = resolved(&frames, &ExposureOptions::default());
    assert_eq!(result.reference, 0);
    assert!(close(&offsets_ev(&result), &[0.0, 2.0], 1e-12));
    // An ISO recorded on some inputs and not others leaves the settings unusable: the bias
    // decides when every input records one, else the bracket is measured.
    let mut frames = iso_bracket(&[
        (1.0 / 15.0, 100, -0.7),
        (1.0 / 15.0, 220, 0.7),
        (1.0 / 15.0, 130, 0.0),
    ]);
    frames[1].exposure.iso = None;
    assert!(matches!(
        MetadataExposure::of(&frames),
        MetadataExposure::Usable {
            source: MetadataSource::Bias,
            ..
        }
    ));
    let result = resolved(&frames, &ExposureOptions::default());
    assert!(close(&offsets_ev(&result), &[-0.7, 0.7, 0.0], 1e-12));
    for frame in &mut frames {
        frame.exposure.bias_ev = None;
    }
    assert_eq!(MetadataExposure::of(&frames), MetadataExposure::Absent);
    assert!(matches!(
        resolve_exposures(&frames, &ExposureOptions::default()),
        Ok(ExposureResolution::NeedsFit(_))
    ));
    // Equal ISO at one shutter with a varying bias: the settings do not vary, a contradiction.
    let frames = iso_bracket(&[(1.0 / 15.0, 130, -0.7), (1.0 / 15.0, 130, 0.7)]);
    assert_eq!(
        MetadataExposure::of(&frames),
        MetadataExposure::Contradictory
    );
    // The bias is checked against time and ISO together: a stop of ISO the other way.
    let frames = iso_bracket(&[
        (1.0 / 15.0, 260, -0.7),
        (1.0 / 15.0, 220, 0.7),
        (1.0 / 15.0, 130, 0.0),
    ]);
    assert_eq!(
        MetadataExposure::of(&frames),
        MetadataExposure::Contradictory
    );
    // The shutter alone agrees with that bias, but the ISO undoes it: a contradiction.
    let frames = iso_bracket(&[
        (1.0 / 30.0, 200, -1.0),
        (1.0 / 15.0, 100, 0.0),
        (1.0 / 8.0, 50, 1.0),
    ]);
    assert_eq!(
        MetadataExposure::of(&frames),
        MetadataExposure::Contradictory
    );
}

#[test]
fn settings_resolve_without_adding_the_bias() {
    // An auto-exposure bracket records both encodings of one step; settings win, never the sum.
    let frames = bracket(&[1.0 / 250.0, 1.0 / 60.0, 1.0 / 15.0], &[-2.0, 0.0, 2.0]);
    let result = resolved(&frames, &ExposureOptions::default());
    assert_eq!(result.reference, 1);
    assert_eq!(result.reference_origin, ReferenceOrigin::ZeroBias);
    let want = [(60.0_f64 / 250.0).log2(), 0.0, (60.0_f64 / 15.0).log2()];
    assert!(close(&offsets_ev(&result), &want, 1e-12), "{result:?}");
    assert!(
        (result.inputs[0].scale - 250.0 / 60.0).abs() < 1e-12,
        "scale brings the dark frame up to the reference"
    );
    assert_eq!(result.inputs[1].origin, ExposureOrigin::Reference);
    for input in [0, 2] {
        assert_eq!(
            result.inputs[input].origin,
            ExposureOrigin::Metadata {
                source: MetadataSource::Settings
            }
        );
    }
    // The bias alone, when no time is recorded.
    let mut frames = frames;
    for frame in &mut frames {
        frame.exposure.shutter_s = None;
    }
    frames[0].exposure.bias_ev = Some(-1.7);
    let result = resolved(&frames, &ExposureOptions::default());
    assert!(close(&offsets_ev(&result), &[-1.7, 0.0, 2.0], 1e-12));
    assert_eq!(
        result.inputs[2].origin,
        ExposureOrigin::Metadata {
            source: MetadataSource::Bias
        }
    );
}

#[test]
fn the_reference_is_the_zero_bias_frame_else_the_median_in_capture_order() {
    // A manual bracket records no varying bias: the median exposure is the reference.
    let frames = bracket(&[1.0 / 32.0, 1.0 / 512.0, 1.0 / 128.0], &[0.0, 0.0, 0.0]);
    let result = resolved(&frames, &ExposureOptions::default());
    assert_eq!(
        (result.reference, result.reference_origin),
        (2, ReferenceOrigin::MedianExposure)
    );
    assert!(close(&offsets_ev(&result), &[2.0, -2.0, 0.0], 1e-12));
    // An even count takes the lower middle.
    let frames = bracket(
        &[1.0 / 32.0, 1.0 / 512.0, 1.0 / 128.0, 1.0 / 8.0],
        &[0.0; 4],
    );
    assert_eq!(resolved(&frames, &ExposureOptions::default()).reference, 2);
    // A requested reference wins.
    let options = ExposureOptions {
        reference: Some(0),
        overrides: Vec::new(),
    };
    let result = resolved(&frames, &options);
    assert_eq!(
        (result.reference, result.reference_origin),
        (0, ReferenceOrigin::Requested)
    );
    assert!(close(&offsets_ev(&result), &[0.0, -4.0, -2.0, 2.0], 1e-12));
    assert_eq!(
        resolve_exposures(
            &frames,
            &ExposureOptions {
                reference: Some(4),
                overrides: Vec::new()
            }
        ),
        Err(Refusal::InvalidReference { reference: 4 })
    );
    // Without exposure metadata the provisional reference is the capture-order middle, not the
    // request-order middle; an unrecorded instant sorts last.
    let mut frames: Vec<_> = (0..4)
        .map(|i| shot(&format!("f{i}"), None, None, 0))
        .collect();
    frames[0].captured_at_ms = Some(30);
    frames[1].captured_at_ms = None;
    frames[2].captured_at_ms = Some(10);
    frames[3].captured_at_ms = Some(20);
    let ExposureResolution::NeedsFit(plan) =
        resolve_exposures(&frames, &ExposureOptions::default()).unwrap()
    else {
        panic!("absent metadata needs a fit");
    };
    // Capture order 2, 3, 0, 1: the lower middle is input 3.
    assert_eq!(plan.reference(), 3);
    assert!(plan.is_provisional());
    assert_eq!(plan.measured().collect::<Vec<_>>(), [0, 1, 2]);
}

#[test]
fn contradictory_or_absent_metadata_needs_a_fit() {
    let needs_fit = |frames: &[FrameMetadata]| {
        matches!(
            resolve_exposures(frames, &ExposureOptions::default()),
            Ok(ExposureResolution::NeedsFit(_))
        )
    };
    // The bias disagrees with the settings by a stop for one frame.
    let frames = bracket(&[1.0 / 250.0, 1.0 / 60.0, 1.0 / 15.0], &[-1.0, 0.0, 2.0]);
    assert_eq!(
        MetadataExposure::of(&frames),
        MetadataExposure::Contradictory
    );
    assert!(needs_fit(&frames));
    // Identical settings but a varying bias.
    let frames = bracket(&[1.0 / 60.0; 3], &[-2.0, 0.0, 2.0]);
    assert_eq!(
        MetadataExposure::of(&frames),
        MetadataExposure::Contradictory
    );
    let ExposureResolution::NeedsFit(plan) =
        resolve_exposures(&frames, &ExposureOptions::default()).unwrap()
    else {
        panic!("contradictory metadata needs a fit");
    };
    assert_eq!(
        (plan.reference(), plan.is_provisional()),
        (1, false),
        "the zero-bias frame still names the reference"
    );
    // Nominal rounding within half a stop is not a contradiction.
    let frames = bracket(&[1.0 / 200.0, 1.0 / 50.0], &[-2.0, 0.0]);
    assert!(matches!(
        MetadataExposure::of(&frames),
        MetadataExposure::Usable {
            source: MetadataSource::Settings,
            ..
        }
    ));
    // A frame without a time or a bias leaves the bracket without metadata.
    let mut frames = bracket(&[1.0 / 250.0, 1.0 / 60.0], &[-2.0, 0.0]);
    frames[0].exposure = ExposureSettings {
        shutter_s: None,
        bias_ev: None,
        ..frames[0].exposure.clone()
    };
    assert_eq!(MetadataExposure::of(&frames), MetadataExposure::Absent);
    assert!(needs_fit(&frames));
}

#[test]
fn overrides_replace_metadata_and_record_their_origin() {
    let frames = bracket(&[1.0 / 250.0, 1.0 / 60.0, 1.0 / 15.0], &[-2.0, 0.0, 2.0]);
    let with = |reference: Option<usize>, overrides: Vec<Option<f64>>| ExposureOptions {
        reference,
        overrides,
    };
    let result = resolved(&frames, &with(None, vec![None, None, Some(3.0)]));
    assert_eq!(result.inputs[2].offset_ev, 3.0);
    assert_eq!(result.inputs[2].origin, ExposureOrigin::Override);
    assert!(matches!(
        result.inputs[0].origin,
        ExposureOrigin::Metadata { .. }
    ));
    assert_eq!(
        resolve_exposures(&frames, &with(None, vec![None, Some(0.5), None])),
        Err(Refusal::InvalidOverride { input: 1 }),
        "the reference's own override must be 0"
    );
    assert_eq!(
        resolve_exposures(&frames, &with(None, vec![None, None])),
        Err(Refusal::OverrideCount {
            count: 2,
            inputs: 3
        })
    );
    for bad in [f64::NAN, f64::INFINITY, 16.5] {
        assert_eq!(
            resolve_exposures(&frames, &with(None, vec![Some(bad), None, None])),
            Err(Refusal::InvalidOverride { input: 0 })
        );
    }
    // Without metadata, a complete set of overrides names its reference by its zero.
    let mut bare: Vec<_> = (0..3)
        .map(|i| shot(&format!("f{i}"), None, None, i))
        .collect();
    let result = resolved(&bare, &with(None, vec![Some(-1.5), Some(1.0), Some(0.0)]));
    assert_eq!(
        (result.reference, result.reference_origin),
        (2, ReferenceOrigin::ZeroOverride)
    );
    assert!(close(&offsets_ev(&result), &[-1.5, 1.0, 0.0], 0.0));
    assert_eq!(
        resolve_exposures(&bare, &with(None, vec![Some(-1.5), Some(1.0), Some(2.0)])),
        Err(Refusal::OverridesNeedReference)
    );
    assert_eq!(
        resolve_exposures(&bare, &with(None, vec![Some(-1.5), None, None])),
        Err(Refusal::OverridesNeedReference)
    );
    // A named reference lets partial overrides stand; only the rest are measured.
    let ExposureResolution::NeedsFit(plan) =
        resolve_exposures(&bare, &with(Some(1), vec![Some(-1.5), None, None])).unwrap()
    else {
        panic!("one input remains to measure");
    };
    assert_eq!(plan.measured().collect::<Vec<_>>(), [2]);
    // Overriding all but the named reference resolves with nothing to measure.
    bare[1].exposure.bias_ev = None;
    let result = resolved(&bare, &with(Some(1), vec![Some(-1.5), None, Some(1.25)]));
    assert!(close(&offsets_ev(&result), &[-1.5, 0.0, 1.25], 0.0));
}

#[test]
fn indistinct_and_out_of_range_exposures_refuse() {
    let frames = bracket(&[1.0 / 60.0, 1.0 / 250.0, 1.0 / 60.0], &[0.0, 0.0, 0.0]);
    assert_eq!(
        resolve_exposures(&frames, &ExposureOptions::default()),
        Err(Refusal::IndistinctExposures {
            first: 0,
            second: 2
        })
    );
    // A sixth of a stop is not a bracket step.
    let frames = bracket(&[1.0 / 60.0, 1.0 / 53.5], &[0.0, 0.0]);
    assert!(matches!(
        resolve_exposures(&frames, &ExposureOptions::default()),
        Err(Refusal::IndistinctExposures { .. })
    ));
    // Seventeen stops from a requested reference.
    let frames = bracket(&[1.0 / 8000.0, 1.0 / 60.0, 16.0], &[0.0, 0.0, 0.0]);
    assert!(matches!(
        resolve_exposures(&frames, &ExposureOptions::default()),
        Ok(ExposureResolution::Resolved(_))
    ));
    assert_eq!(
        resolve_exposures(
            &frames,
            &ExposureOptions {
                reference: Some(0),
                overrides: Vec::new()
            }
        ),
        Err(Refusal::ExposureOutOfRange { input: 2 })
    );
}

fn bayer() -> Camera {
    Camera {
        camera_to_srgb: reference::CAMERA_TO_SRGB,
        gains: [2.1, 1.0, 1.6],
        bayer: true,
    }
}

fn xtrans() -> Camera {
    Camera {
        bayer: false,
        ..bayer()
    }
}

/// Production's views of reference exposures, which own their levels.
struct Views {
    levels: Vec<Vec<SensorLevel>>,
}

impl Views {
    fn of(exposures: &[Exposure]) -> Self {
        Self {
            levels: exposures
                .iter()
                .map(|e| e.levels.iter().map(|&code| SensorLevel(code)).collect())
                .collect(),
        }
    }

    fn rects<'a>(&'a self, exposures: &'a [Exposure]) -> Vec<FrameRect<'a>> {
        exposures
            .iter()
            .zip(&self.levels)
            .map(|(e, levels)| FrameRect {
                width: e.width,
                height: e.height,
                stride: e.width,
                rgb: [&e.rgb[0], &e.rgb[1], &e.rgb[2]],
                levels,
            })
            .collect()
    }
}

/// Resolved exposures of `offsets` (EV from the scene's reference exposure) with `reference`.
fn exposures_at(offsets: &[f64], reference: usize) -> ResolvedExposures {
    let frames: Vec<_> = (0..offsets.len())
        .map(|i| shot(&format!("f{i}"), None, None, i as i64))
        .collect();
    let overrides = offsets
        .iter()
        .map(|o| Some(o - offsets[reference]))
        .collect();
    resolved(
        &frames,
        &ExposureOptions {
            reference: Some(reference),
            overrides,
        },
    )
}

struct Fusion {
    rgb: [Vec<f32>; 3],
    limits: Vec<PixelLimits>,
    diagnostics: FusionDiagnostics,
}

fn fuse_all(
    resolved: &ResolvedExposures,
    exposures: &[Exposure],
    motion: Option<&MotionSelection<'_>>,
) -> Result<Fusion, Error> {
    let views = Views::of(exposures);
    let rects = views.rects(exposures);
    let n = WIDTH * HEIGHT;
    let (mut r, mut g, mut b) = (vec![0.0; n], vec![0.0; n], vec![0.0; n]);
    let mut limits = vec![PixelLimits::default(); n];
    let diagnostics = fuse_rect(
        resolved,
        &FusionParameters::default(),
        &rects,
        motion,
        &mut FusedRect {
            width: WIDTH,
            height: HEIGHT,
            stride: WIDTH,
            rgb: [&mut r, &mut g, &mut b],
            limits: Some(&mut limits),
        },
    )?;
    Ok(Fusion {
        rgb: [r, g, b],
        limits,
        diagnostics,
    })
}

fn record(camera: &Camera, offsets: &[f64], noise: Option<u64>) -> Vec<Exposure> {
    let scene = reference::ramp_scene(WIDTH, HEIGHT);
    offsets
        .iter()
        .enumerate()
        .map(|(i, &offset)| {
            let noise = noise.map(|seed| Noise {
                seed: seed + i as u64,
                shot: 1e-4,
                read: 5e-4,
            });
            reference::expose(&scene, WIDTH, HEIGHT, camera, offset, noise)
        })
        .collect()
}

/// Every channel of every pixel within the declared tolerance of the reference fusion.
fn assert_matches_reference(fusion: &Fusion, want: &reference::Fused, tolerance: f64, what: &str) {
    for i in 0..WIDTH * HEIGHT {
        let magnitude = want.rgb[i].iter().fold(0.0_f64, |m, v| m.max(v.abs()));
        for c in 0..3 {
            let got = f64::from(fusion.rgb[c][i]);
            assert!(
                (got - want.rgb[i][c]).abs() <= tolerance * magnitude + 1e-9,
                "{what}: pixel {i} channel {c}: {got} against {}",
                want.rgb[i][c]
            );
        }
        assert_eq!(
            fusion.limits[i].contains(PixelLimits::ALL_INVALID),
            want.all_invalid[i],
            "{what}: pixel {i} fallback"
        );
    }
}

#[test]
fn fusion_matches_the_reference_over_complete_buffers() {
    type Case = (&'static str, Camera, Vec<f64>, usize, Option<u64>);
    let cases: [Case; 4] = [
        ("bayer ±2", bayer(), vec![-2.0, 0.0, 2.0], 1, None),
        ("bayer ±2 noisy", bayer(), vec![-2.0, 0.0, 2.0], 1, Some(7)),
        (
            "x-trans uneven",
            xtrans(),
            vec![1.3, -1.7, 0.0, -3.9, 2.6],
            2,
            Some(11),
        ),
        ("two frames", bayer(), vec![0.0, -3.0], 0, None),
    ];
    let params = FusionParameters::default();
    for (what, camera, offsets, reference_index, noise) in cases {
        let exposures = record(&camera, &offsets, noise);
        let resolved = exposures_at(&offsets, reference_index);
        // The reference sees exposures relative to its own reference frame.
        let mut relative = exposures.clone();
        for e in &mut relative {
            e.offset_ev -= offsets[reference_index];
        }
        let refs: Vec<&Exposure> = relative.iter().collect();
        let want = reference::fuse(&refs, params.rolloff_start, params.rolloff_end, None);
        let fusion = fuse_all(&resolved, &exposures, None).unwrap();
        assert_matches_reference(&fusion, &want, 1e-6, what);
        assert_eq!(
            fusion.diagnostics.all_invalid,
            want.all_invalid.iter().filter(|v| **v).count() as u64,
            "{what}"
        );
        assert_eq!(fusion.diagnostics.pixels, (WIDTH * HEIGHT) as u64);
    }
}

#[test]
fn an_auto_iso_bracket_merges_from_its_settings() {
    // The Air 2S bracket at one shutter and ISO 100, 220 and 130: the camera records each frame
    // at the exposure its time and gain give, and the merge resolved from that metadata matches
    // the reference fusion of those exposures.
    let frames = iso_bracket(&[
        (1.0 / 15.0, 100, -0.7),
        (1.0 / 15.0, 220, 0.7),
        (1.0 / 15.0, 130, 0.0),
    ]);
    let resolved = resolved(&frames, &ExposureOptions::default());
    let offsets = offsets_ev(&resolved);
    let exposures = record(&bayer(), &offsets, Some(5));
    let refs: Vec<&Exposure> = exposures.iter().collect();
    let params = FusionParameters::default();
    let want = reference::fuse(&refs, params.rolloff_start, params.rolloff_end, None);
    let fusion = fuse_all(&resolved, &exposures, None).unwrap();
    assert_matches_reference(&fusion, &want, 1e-6, "auto ISO bracket");
}

#[test]
fn noise_free_fusion_recovers_scene_radiance_with_signs_and_headroom() {
    let offsets = [-3.0, 0.0, 3.0];
    let exposures = record(&bayer(), &offsets, None);
    let fusion = fuse_all(&exposures_at(&offsets, 1), &exposures, None).unwrap();
    let scene = reference::ramp_scene(WIDTH, HEIGHT);
    let (mut negative, mut above_one, mut fallbacks) = (0, 0, 0);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let i = y * WIDTH + x;
            let truth = scene(x, y);
            let got = [0, 1, 2].map(|c| f64::from(fusion.rgb[c][i]));
            assert!(got.iter().all(|v| v.is_finite()));
            if fusion.limits[i].contains(PixelLimits::ALL_INVALID) {
                // Saturated in every exposure: the shortest exposure's lower bound, never black
                // and never above the scene.
                fallbacks += 1;
                let shortest = [0, 1, 2].map(|c| f64::from(exposures[0].rgb[c][i]) * 8.0);
                assert_eq!(got.map(|v| v as f32), shortest.map(|v| v as f32));
                assert!(got.iter().any(|v| *v > 1.0));
                continue;
            }
            let magnitude = truth.iter().fold(0.0_f64, |m, v| m.max(v.abs()));
            for c in 0..3 {
                assert!(
                    (got[c] - truth[c]).abs() <= 1e-5 * magnitude + 1e-9,
                    "({x}, {y}) channel {c}: {} against {}",
                    got[c],
                    truth[c]
                );
            }
            negative += usize::from(got.iter().any(|v| *v < 0.0));
            above_one += usize::from(got.iter().any(|v| *v > 1.0));
        }
    }
    assert!(negative > 0 && above_one > 0 && fallbacks > 0);
    assert_eq!(fusion.diagnostics.all_invalid, fallbacks);
    // The grey band keeps the reference exposure's brightness exactly.
    let grey = (HEIGHT - 1) * WIDTH + WIDTH / 2;
    for c in 0..3 {
        assert!(
            (f64::from(fusion.rgb[c][grey]) - f64::from(exposures[1].rgb[c][grey])).abs() < 1e-7
        );
    }
    let limitations = fusion.diagnostics.limitations();
    assert!(limitations.all_invalid && !limitations.motion && !limitations.no_additional_range);
    assert!(fusion.diagnostics.highlight_extended > 0 && fusion.diagnostics.shadow_extended > 0);
}

#[test]
fn noisy_fusion_lowers_noise_and_keeps_reference_brightness() {
    let offsets = [-2.0, 0.0, 2.0];
    let exposures = record(&bayer(), &offsets, Some(42));
    let fusion = fuse_all(&exposures_at(&offsets, 1), &exposures, None).unwrap();
    let scene = reference::ramp_scene(WIDTH, HEIGHT);
    // The neutral band where the reference itself is well inside its range.
    let (mut fused_error, mut reference_error, mut ratio, mut count) = (0.0, 0.0, 0.0, 0.0);
    for y in 0..HEIGHT / 6 {
        for x in 0..WIDTH {
            let truth = scene(x, y)[1];
            let i = y * WIDTH + x;
            if !(0.01..0.5).contains(&truth) {
                continue;
            }
            let fused = f64::from(fusion.rgb[1][i]);
            let single = f64::from(exposures[1].rgb[1][i]);
            fused_error += ((fused - truth) / truth).powi(2);
            reference_error += ((single - truth) / truth).powi(2);
            ratio += fused / truth;
            count += 1.0;
        }
    }
    assert!(count > 100.0);
    let (fused_rms, reference_rms) = (
        (fused_error / count).sqrt(),
        (reference_error / count).sqrt(),
    );
    assert!(
        fused_rms < 0.8 * reference_rms,
        "fused {fused_rms} against the reference alone {reference_rms}"
    );
    assert!(
        (ratio / count - 1.0).abs() < 0.01,
        "mean ratio {}",
        ratio / count
    );
}

#[test]
fn motion_selection_feathers_towards_the_selected_exposure() {
    let offsets = [-2.0, 0.0, 2.0];
    let exposures = record(&bayer(), &offsets, Some(3));
    let resolved = exposures_at(&offsets, 1);
    let n = WIDTH * HEIGHT;
    let selected: Vec<u8> = (0..n).map(|i| (i % 3) as u8).collect();
    let confidence: Vec<f32> = (0..n)
        .map(|i| match i % 5 {
            0 => 0.0,
            1 => 1.0,
            k => k as f32 / 5.0,
        })
        .collect();
    let motion = MotionSelection {
        width: WIDTH,
        height: HEIGHT,
        stride: WIDTH,
        selected: &selected,
        confidence: &confidence,
    };
    let fusion = fuse_all(&resolved, &exposures, Some(&motion)).unwrap();
    let refs: Vec<&Exposure> = exposures.iter().collect();
    let params = FusionParameters::default();
    let want = reference::fuse(
        &refs,
        params.rolloff_start,
        params.rolloff_end,
        Some((&selected, &confidence)),
    );
    assert_matches_reference(&fusion, &want, 1e-6, "motion");
    let moving = confidence.iter().filter(|c| **c > 0.0).count() as u64;
    assert_eq!(fusion.diagnostics.motion, moving);
    let invalid = (0..n)
        .filter(|&i| {
            confidence[i] > 0.0
                && params.highlight_weight(SensorLevel(exposures[selected[i] as usize].levels[i]))
                    == 0.0
        })
        .count() as u64;
    assert!(
        invalid > 0,
        "the brightest exposure is selected in its highlights"
    );
    assert_eq!(fusion.diagnostics.motion_selection_invalid, invalid);
    for (limits, c) in fusion.limits.iter().zip(&confidence) {
        assert_eq!(limits.contains(PixelLimits::MOTION), *c > 0.0);
    }
    let limitations = fusion.diagnostics.limitations();
    assert!(limitations.motion && limitations.motion_selection_invalid);
    // A selection naming a missing input, or a confidence outside [0, 1], is refused.
    for (index, value) in [(3_u8, 0.5_f32), (0, 1.5), (0, f32::NAN)] {
        let mut selected = selected.clone();
        let mut confidence = confidence.clone();
        selected[7] = index;
        confidence[7] = value;
        let motion = MotionSelection {
            selected: &selected,
            confidence: &confidence,
            ..motion
        };
        assert_eq!(
            fuse_all(&resolved, &exposures, Some(&motion))
                .err()
                .unwrap()
                .kind,
            ErrorKind::Validation
        );
    }
}

#[test]
fn fusion_is_deterministic_and_independent_of_rectangles() {
    let offsets = [-2.0, 0.0, 2.0, 4.0];
    let exposures = record(&xtrans(), &offsets, Some(5));
    let resolved = exposures_at(&offsets, 1);
    let whole = fuse_all(&resolved, &exposures, None).unwrap();
    let again = fuse_all(&resolved, &exposures, None).unwrap();
    let bits = |f: &Fusion| -> Vec<u32> { f.rgb.iter().flatten().map(|v| v.to_bits()).collect() };
    assert_eq!(bits(&whole), bits(&again));
    assert_eq!(whole.diagnostics, again.diagnostics);
    // Four uneven rectangles of strided views into the same buffers and output.
    let views = Views::of(&exposures);
    let n = WIDTH * HEIGHT;
    let mut out = [vec![0.0_f32; n], vec![0.0_f32; n], vec![0.0_f32; n]];
    let mut total = FusionDiagnostics::default();
    for (x0, y0, w, h) in [
        (0, 0, 37, 20),
        (37, 0, 59, 20),
        (0, 20, 50, 16),
        (50, 20, 46, 16),
    ] {
        let start = y0 * WIDTH + x0;
        let rects: Vec<FrameRect<'_>> = exposures
            .iter()
            .zip(&views.levels)
            .map(|(e, levels)| FrameRect {
                width: w,
                height: h,
                stride: WIDTH,
                rgb: [&e.rgb[0][start..], &e.rgb[1][start..], &e.rgb[2][start..]],
                levels: &levels[start..],
            })
            .collect();
        let [r, g, b] = &mut out;
        let diagnostics = fuse_rect(
            &resolved,
            &FusionParameters::default(),
            &rects,
            None,
            &mut FusedRect {
                width: w,
                height: h,
                stride: WIDTH,
                rgb: [&mut r[start..], &mut g[start..], &mut b[start..]],
                limits: None,
            },
        )
        .unwrap();
        total.add(&diagnostics);
    }
    let split: Vec<u32> = out.iter().flatten().map(|v| v.to_bits()).collect();
    assert_eq!(split, bits(&whole));
    assert_eq!(total, whole.diagnostics);
}

#[test]
fn a_bracket_inside_the_reference_range_reports_no_additional_range() {
    // A low-contrast scene the reference records whole: brackets add only noise reduction.
    let scene = |_x: usize, y: usize| {
        let l = 0.05 + 0.3 * y as f64 / HEIGHT as f64;
        [l, 0.9 * l, 0.8 * l]
    };
    let offsets = [-1.0, 0.0, 1.0];
    let exposures: Vec<Exposure> = offsets
        .iter()
        .map(|&o| reference::expose(&scene, WIDTH, HEIGHT, &bayer(), o, None))
        .collect();
    let fusion = fuse_all(&exposures_at(&offsets, 1), &exposures, None).unwrap();
    assert_eq!(
        fusion.diagnostics.outcome(),
        RangeOutcome::NoAdditionalRange
    );
    assert!(fusion.diagnostics.limitations().no_additional_range);
    // The ramp extends both ends.
    let exposures = record(&bayer(), &offsets, None);
    let fusion = fuse_all(&exposures_at(&offsets, 1), &exposures, None).unwrap();
    assert_eq!(fusion.diagnostics.outcome(), RangeOutcome::Extended);
}

#[test]
fn fusion_refuses_malformed_views() {
    let offsets = [-2.0, 0.0];
    let exposures = record(&bayer(), &offsets, None);
    let resolved = exposures_at(&offsets, 1);
    let views = Views::of(&exposures);
    let rects = views.rects(&exposures);
    let n = WIDTH * HEIGHT;
    let mut out = [vec![0.0_f32; n], vec![0.0_f32; n], vec![0.0_f32; n]];
    let mut run = |rects: &[FrameRect<'_>], params: FusionParameters, width: usize| {
        let [r, g, b] = &mut out;
        fuse_rect(
            &resolved,
            &params,
            rects,
            None,
            &mut FusedRect {
                width,
                height: HEIGHT,
                stride: width,
                rgb: [r, g, b],
                limits: None,
            },
        )
        .map(|_| ())
        .map_err(|error| error.kind)
    };
    let fine = FusionParameters::default();
    assert_eq!(run(&rects, fine, WIDTH), Ok(()));
    assert_eq!(run(&rects[..1], fine, WIDTH), Err(ErrorKind::Validation));
    assert_eq!(run(&rects, fine, WIDTH - 1), Err(ErrorKind::Validation));
    let short = [
        FrameRect {
            levels: &views.levels[0][..n - 1],
            ..rects[0]
        },
        rects[1],
    ];
    assert_eq!(run(&short, fine, WIDTH), Err(ErrorKind::Validation));
    let mut poisoned = exposures[1].rgb[2].clone();
    poisoned[17] = f32::NAN;
    let nan = [
        rects[0],
        FrameRect {
            rgb: [&exposures[1].rgb[0], &exposures[1].rgb[1], &poisoned],
            ..rects[1]
        },
    ];
    assert_eq!(run(&nan, fine, WIDTH), Err(ErrorKind::Validation));
    let backwards = FusionParameters {
        rolloff_start: 0.95,
        rolloff_end: 0.8,
        ..fine
    };
    assert_eq!(run(&rects, backwards, WIDTH), Err(ErrorKind::Validation));
    // A rectangle above the bound is refused before any view is read.
    let huge = [FrameRect {
        width: 1025,
        height: 1025,
        stride: 1025,
        ..rects[0]
    }; 2];
    let mut tiny = [vec![0.0_f32; 1], vec![0.0_f32; 1], vec![0.0_f32; 1]];
    let [r, g, b] = &mut tiny;
    let error = fuse_rect(
        &resolved,
        &fine,
        &huge,
        None,
        &mut FusedRect {
            width: 1025,
            height: 1025,
            stride: 1025,
            rgb: [r, g, b],
            limits: None,
        },
    )
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::ResourceLimit);
}

#[test]
fn measured_fit_recovers_offsets_and_moves_a_provisional_reference() {
    // Recorded at −2.3, +0.7 and −0.6 EV with no metadata; captured in request order.
    let offsets = [-2.3, 0.7, -0.6];
    let exposures = record(&bayer(), &offsets, Some(9));
    let frames: Vec<_> = (0..3)
        .map(|i| shot(&format!("f{i}"), None, None, i as i64))
        .collect();
    let ExposureResolution::NeedsFit(plan) =
        resolve_exposures(&frames, &ExposureOptions::default()).unwrap()
    else {
        panic!("absent metadata needs a fit");
    };
    assert_eq!((plan.reference(), plan.is_provisional()), (1, true));
    let views = Views::of(&exposures);
    let mut samples = FitSamples::new(3);
    samples.gather(&views.rects(&exposures), 1).unwrap();
    assert_eq!(samples.len(), WIDTH * HEIGHT);
    let result = plan.complete(&samples).unwrap();
    // The median measured exposure, −0.6 EV, is the final reference.
    assert_eq!(
        (result.reference, result.reference_origin),
        (2, ReferenceOrigin::MedianFit)
    );
    assert!(
        close(&offsets_ev(&result), &[-1.7, 1.3, 0.0], 0.02),
        "{result:?}"
    );
    for input in [0, 1] {
        let ExposureOrigin::Fit {
            confidence,
            samples,
            ..
        } = result.inputs[input].origin
        else {
            panic!("input {input} is measured");
        };
        assert!(confidence > 0.5 && samples >= MIN_PAIR_SAMPLES_U32);
    }
    assert_eq!(result.inputs[2].origin, ExposureOrigin::Reference);
    // A fixed reference and an override: only the rest is measured.
    let ExposureResolution::NeedsFit(plan) = resolve_exposures(
        &frames,
        &ExposureOptions {
            reference: Some(2),
            overrides: vec![None, Some(1.25), None],
        },
    )
    .unwrap() else {
        panic!("one input remains to measure");
    };
    let result = plan.complete(&samples).unwrap();
    assert_eq!(result.reference_origin, ReferenceOrigin::Requested);
    assert!(close(&offsets_ev(&result), &[-1.7, 1.25, 0.0], 0.02));
    assert_eq!(result.inputs[1].origin, ExposureOrigin::Override);
}

const MIN_PAIR_SAMPLES_U32: u32 = exposure::MIN_PAIR_SAMPLES as u32;

#[test]
fn fit_refuses_insufficient_and_ambiguous_samples() {
    let frames: Vec<_> = (0..3)
        .map(|i| shot(&format!("f{i}"), None, None, i as i64))
        .collect();
    let ExposureResolution::NeedsFit(plan) =
        resolve_exposures(&frames, &ExposureOptions::default()).unwrap()
    else {
        panic!("absent metadata needs a fit");
    };
    // The brightest frame is saturated almost everywhere it overlaps the others.
    let offsets = [0.0, 9.0, 1.0];
    let exposures = record(&bayer(), &offsets, None);
    let views = Views::of(&exposures);
    let mut samples = FitSamples::new(3);
    samples.gather(&views.rects(&exposures), 1).unwrap();
    // Keep only the points the bright frame clips.
    let mut clipped = FitSamples::new(3);
    let rects = views.rects(&exposures);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let points: Vec<_> = rects.iter().map(|r| r.at(x, y)).collect();
            if points[1].1.is_clipped() {
                let rgb: Vec<_> = points.iter().map(|p| p.0).collect();
                let levels: Vec<_> = points.iter().map(|p| p.1).collect();
                clipped.push(&rgb, &levels).unwrap();
            }
        }
    }
    // Input 1 is the provisional reference: it, not the others, lacks the overlap.
    assert_eq!(
        plan.complete(&clipped),
        Err(Refusal::InsufficientOverlap { input: 1 })
    );
    assert_eq!(
        plan.complete(&FitSamples::new(3)),
        Err(Refusal::InsufficientOverlap { input: 1 })
    );
    // Unrelated pixels: every pair's ratios scatter.
    let mut rng = luxforge_reference::SplitMix64(17);
    let mut scattered = FitSamples::new(3);
    for _ in 0..4096 {
        let rgb: Vec<[f32; 3]> = (0..3)
            .map(|_| [rng.next_range(0.01, 0.7) as f32; 3])
            .collect();
        let levels: Vec<_> = rgb
            .iter()
            .map(|p| SensorLevel::from_fraction(f64::from(p[0])))
            .collect();
        scattered.push(&rgb, &levels).unwrap();
    }
    assert!(matches!(
        plan.complete(&scattered),
        Err(Refusal::AmbiguousExposure { .. })
    ));
    // The bound: a full set takes no more points.
    let mut full = FitSamples::new(2);
    let point = [[0.5_f32; 3]; 2];
    let levels = [SensorLevel(100); 2];
    while full.push(&point, &levels).unwrap() {}
    assert_eq!(full.len(), exposure::MAX_FIT_SAMPLES);
    assert!(full.push(&point[..1], &levels[..1]).is_err());
}

#[test]
fn sensor_levels_match_the_simulated_development() {
    let rgb_cam = reference::CAMERA_TO_SRGB.map(|row| row.map(|v| v as f32));
    for camera in [bayer(), xtrans()] {
        let layout = if camera.bayer {
            SensorLayout::Bayer
        } else {
            SensorLayout::XTrans
        };
        let gains = camera.gains.map(|g| g as f32);
        let model = SensorClipModel::new(rgb_cam, gains, layout).unwrap();
        let want_ceiling = camera.ceiling();
        for (got, want) in model.ceiling().iter().zip(want_ceiling) {
            assert!((got - want).abs() < 1e-7);
        }
        let exposures = record(&camera, &[-2.0, 0.0, 3.0], Some(23));
        for e in &exposures {
            let mut levels = vec![SensorLevel(0); WIDTH * HEIGHT];
            model
                .levels(
                    WIDTH,
                    HEIGHT,
                    WIDTH,
                    [&e.rgb[0], &e.rgb[1], &e.rgb[2]],
                    &mut levels,
                )
                .unwrap();
            let mut clipped = 0;
            for (i, &got) in levels.iter().enumerate() {
                if e.clipped[i] {
                    assert!(got.is_clipped(), "pixel {i} reached its ceiling");
                    clipped += 1;
                } else if e.fraction[i] < 0.998 {
                    assert!(
                        !got.is_clipped() && got.0.abs_diff(e.levels[i]) <= 2,
                        "pixel {i}: {} against {}",
                        got.0,
                        e.levels[i]
                    );
                }
            }
            assert!(clipped > 0 || e.offset_ev < 0.0);
        }
    }
    assert!(SensorClipModel::new(rgb_cam, [2.0, 1.0, 1.5], SensorLayout::Monochrome).is_err());
    assert!(SensorClipModel::new(rgb_cam, [0.0, 1.0, 1.5], SensorLayout::Bayer).is_err());
    let singular = [[1.0, 1.0, 1.0]; 3];
    assert_eq!(
        SensorClipModel::new(singular, [1.0; 3], SensorLayout::XTrans)
            .unwrap_err()
            .kind,
        ErrorKind::UnsupportedColor
    );
    assert_eq!(SensorLevel::from_fraction(-0.5), SensorLevel(0));
    assert_eq!(SensorLevel::from_fraction(0.5), SensorLevel(16384));
    assert_eq!(SensorLevel::from_fraction(0.9989), SensorLevel(32731));
    assert!(SensorLevel::from_fraction(0.999).is_clipped());
    assert!(SensorLevel::from_fraction(f64::NAN).is_clipped());
}

/// A real development's own saturated sites read as clipped: every default-crop pixel whose own
/// CFA site recorded sensor white, developed at the as-shot gains and calibrated by the shared
/// preparation, has a clipped [`SensorLevel`]. Set `LUXFORGE_HDR_RAW` to a Bayer or X-Trans RAW
/// without DNG corrections:
///
/// ```text
/// LUXFORGE_HDR_RAW=/path/to/z6.NEF cargo test -p luxforge-core --lib \
///   hdr_merge_fusion::supplied_raw -- --ignored --nocapture
/// ```
#[test]
#[ignore = "requires a supplied RAW file"]
fn supplied_raw_saturated_sites_read_as_clipped() {
    use std::sync::atomic::AtomicBool;
    let path = std::env::var("LUXFORGE_HDR_RAW").expect("RAW path");
    let bytes = std::fs::read(&path).expect("read RAW");
    let cancel = AtomicBool::new(false);
    let prepared =
        crate::source::RawPrepared::decode(bytes, "hdr".into(), None, &cancel).expect("develop");
    let metadata = prepared.sensor.metadata();
    let model = SensorClipModel::of_raw(metadata, prepared.gains).unwrap();
    let linear = prepared.linear.as_ref().unwrap();
    let (planes, (width, height), _, _) = linear.shared_planes();
    let (width, height) = (width as usize, height as usize);
    let n = width * height;
    let samples = prepared.sensor.source_samples();
    assert_eq!(samples.len(), n, "a mosaic, one sample per site");
    let crop = metadata.default_crop;
    let (mut saturated, mut flagged, mut all_clipped) = (0_u64, 0_u64, 0_u64);
    for y in crop.y as usize + 12..(crop.y + crop.height) as usize - 12 {
        for x in crop.x as usize + 12..(crop.x + crop.width) as usize - 12 {
            let i = y * width + x;
            let level = model.level([planes[i], planes[n + i], planes[2 * n + i]]);
            all_clipped += u64::from(level.is_clipped());
            if f32::from(samples[i]) >= metadata.sensor_white {
                saturated += 1;
                flagged += u64::from(level.is_clipped());
            }
        }
    }
    println!(
        "{}",
        serde_json::json!({
            "file": path,
            "model": metadata.model,
            "gains": prepared.gains,
            "ceiling": model.ceiling(),
            "saturated_sites": saturated,
            "flagged_of_saturated": flagged,
            "clipped_pixels": all_clipped,
        })
    );
    assert_eq!(flagged, saturated, "every saturated site reads as clipped");
}
