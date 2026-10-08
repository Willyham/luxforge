use super::*;
use crate::{
    BASIC_EFFECT, CompileStage, EFFECT_FORMAT, Processing, Stage, ToolModule, modules::BasicModule,
};

fn compile(values: AutoToneValues) -> Result<Vec<ColorOperation>, Error> {
    let unit = BasicModule::new().compile(
        BASIC_EFFECT,
        EFFECT_FORMAT,
        &Value::Object(values.fields()),
        CompileStage::exact(Stage {
            width: 32,
            height: 32,
        }),
    )?;
    let Processing::Color(operation) = unit else {
        panic!("Basic is pointwise colour")
    };
    Ok(vec![operation])
}

fn sample(rgb: impl Iterator<Item = [f32; 3]>) -> SampleGrid {
    SampleGrid {
        grid: [32, 32],
        rgb: rgb.take(1024).collect(),
        source_clipped: vec![false; 1024],
    }
}

fn ramp() -> SampleGrid {
    sample((0..1024).map(|i| [0.01 + i as f32 * 0.09 / 1023.; 3]))
}

#[test]
fn auto_tone_targets_are_the_versioned_design_set() {
    assert_eq!(
        serde_json::to_value(AutoToneTargets::default()).unwrap(),
        json!({"median":0.46,"bright_guard":0.02,"highlights_scale":250.,"shadows_scale":200.,"spread":0.6,"clipping":0.0005,"vibrance_chroma":0.07,"saturation_chroma":0.14})
    );
}

#[test]
fn auto_tone_statistics_match_an_independent_rank_and_exclusion_oracle() {
    let mut input = sample((0..1024).map(|i| [i as f32 / 1023.; 3]));
    input.source_clipped[1023] = true;
    let stats = picture_statistics(&input, &Cancel::never()).unwrap();
    for (actual, index) in [
        (stats.p01, 10),
        (stats.p10, 102),
        (stats.p50, 511),
        (stats.p90, 921),
        (stats.p99, 1013),
    ] {
        assert!((actual - encode(f64::from(index as f32 / 1023.))).abs() < 1e-12);
    }
    assert_eq!(
        stats.highlight_clip, 0.,
        "source white remains in percentiles but not clipping"
    );
    assert_eq!(stats.shadow_clip, 1. / 1023.);
    assert!(stats.mean_chroma < 1e-6);
    assert_eq!(
        stats.bright,
        (0..1024)
            .filter(|&i| encode(f64::from(i as f32 / 1023.)) > 0.8)
            .count() as f64
            / 1024.
    );
}

#[test]
fn auto_tone_refusals_are_structured_and_cancel_is_not_a_refusal() {
    for (input, reason) in [
        (sample(std::iter::repeat_n([0.; 3], 1024)), "near-black"),
        (
            sample(std::iter::repeat_n([0.2; 3], 1024)),
            "no-tonal-range",
        ),
        (
            sample(std::iter::repeat_n([f32::NAN; 3], 1024)),
            "too-few-samples",
        ),
    ] {
        let error = solve(
            &input,
            AutoToneTargets::default(),
            compile,
            &Cancel::never(),
        )
        .unwrap_err();
        let refusal = AnalysisRefusal::of(&error).unwrap();
        assert_eq!(refusal.reason, reason);
        assert_eq!(
            error.data.unwrap().as_ref(),
            &json!({"analysis_refusal": reason})
        );
    }
    let cancelled = Cancel::new();
    cancelled.cancel();
    assert_eq!(
        solve(&ramp(), AutoToneTargets::default(), compile, &cancelled)
            .unwrap_err()
            .kind,
        crate::ErrorKind::Cancelled
    );
}

/// The design's steps 1 to 8 ([auto-tone.md](../../../../../../docs/design/auto-tone.md#the-solve-auto-tone1)),
/// computed independently of the solver's searches: every search tries every value on its field's
/// grid, and every formula is written out from the design.
fn by_the_design(input: &SampleGrid) -> AutoToneValues {
    let targets = AutoToneTargets::default();
    let stats = |values: AutoToneValues, chroma: bool| {
        statistics(
            input,
            &compile(values).unwrap(),
            false,
            chroma,
            &Cancel::never(),
        )
        .unwrap()
    };
    // Step 1: the median nearest the target on the 0.01 EV grid (the lower on a tie), then the
    // highest Exposure at or below it whose near-white fraction is within the guard.
    let exposure = |values: AutoToneValues| {
        let all: Vec<Statistics> = (-400..=400)
            .map(|e| {
                stats(
                    AutoToneValues {
                        exposure: f64::from(e) / 100.,
                        ..values
                    },
                    false,
                )
            })
            .collect();
        let error = |i: usize| (all[i].p50 - targets.median).abs();
        let best = (0..all.len())
            .reduce(|best, i| if error(i) < error(best) { i } else { best })
            .unwrap();
        let guarded = (0..=best)
            .rev()
            .find(|&i| all[i].near_white <= targets.bright_guard)
            .unwrap_or(0);
        (guarded as f64 - 400.) / 100.
    };
    // Steps 5 and 6: the largest Whites and the smallest Blacks meeting the clipping fraction;
    // an unreachable constraint stops at the bound that clips least.
    let endpoints = |mut values: AutoToneValues| {
        values.whites = (-60..=60)
            .rev()
            .find(|&n| {
                stats(
                    AutoToneValues {
                        whites: f64::from(n),
                        ..values
                    },
                    false,
                )
                .highlight_clip
                    <= targets.clipping
            })
            .map_or(-60., f64::from);
        values.blacks = (-60..=60)
            .find(|&n| {
                stats(
                    AutoToneValues {
                        blacks: f64::from(n),
                        ..values
                    },
                    false,
                )
                .shadow_clip
                    <= targets.clipping
            })
            .map_or(60., f64::from);
        values
    };
    let mut values = AutoToneValues::default();
    values.exposure = exposure(values);
    // Steps 2 and 3.
    let bands = stats(values, false);
    values.highlights = (-250. * bands.bright).clamp(-100., 0.).round();
    values.shadows = (200. * bands.dark).clamp(0., 60.).round();
    // Step 4.
    let spread = stats(values, false);
    values.contrast = (100. * (0.60 - (spread.p90 - spread.p10)))
        .clamp(-50., 50.)
        .round();
    values = endpoints(values);
    // Step 7: exactly one more sweep of Exposure, Whites and Blacks.
    values.exposure = exposure(values);
    values = endpoints(values);
    // Step 8.
    let chroma = stats(values, true).mean_chroma;
    values.vibrance = (60. * (1. - chroma / 0.07)).clamp(0., 25.).round();
    values.saturation = if chroma > 0.14 {
        (-60. * (chroma / 0.14 - 1.)).clamp(-15., 0.).round()
    } else {
        0.
    };
    values
}

/// A dark neutral ramp: its first Exposure is the analytical solution `log2(t / m)` for the target
/// median's linear value `t` and the input median `m`, rounded to the 0.01 EV grid, because Basic's
/// Exposure scales linear light and a neutral ramp's median is its middle sample; it has no chroma,
/// so Vibrance is its +25 bound and Saturation 0; and every value is the design's.
#[test]
fn auto_tone_dark_neutral_ramp_equals_the_designs_steps_and_its_analytical_exposure() {
    let input = ramp();
    let report = solve(
        &input,
        AutoToneTargets::default(),
        compile,
        &Cancel::never(),
    )
    .unwrap();
    let median = f64::from(input.rgb[511][0]);
    let linear_target = ((0.46_f64 + 0.055) / 1.055).powf(2.4);
    let first = ((linear_target / median).log2() * 100.).round() / 100.;
    assert_eq!(
        report.bands,
        statistics(
            &input,
            &compile(AutoToneValues {
                exposure: first,
                ..Default::default()
            })
            .unwrap(),
            false,
            false,
            &Cancel::never()
        )
        .unwrap(),
        "the first sweep's Exposure is {first} EV"
    );
    assert_eq!(report.values, by_the_design(&input));
    assert_eq!(report.values.highlights, 0.);
    assert_eq!(report.values.vibrance, 25.);
    assert_eq!(report.values.saturation, 0.);
    assert!(
        serde_json::to_vec(&report).unwrap().len() < 3500,
        "leave room for layer provenance in the 4 KiB query"
    );
}

/// The design's other synthetic cases: bright, low-contrast, high-key, a clipped source and a
/// saturated one each solve to exactly the values its steps give.
#[test]
fn auto_tone_synthetic_cases_equal_the_designs_steps() {
    let bright = sample((0..1024).map(|i| [0.3 + i as f32 * 1.2 / 1023.; 3]));
    let low_contrast = sample((0..1024).map(|i| [0.08 + i as f32 * 0.04 / 1023.; 3]));
    let high_key = sample((0..1024).map(|i| [0.5 + i as f32 * 0.5 / 1023.; 3]));
    // A quarter of the frame is sky already clipped in the file.
    let mut clipped = sample((0..1024).map(|i| {
        if i >= 768 {
            [1.; 3]
        } else {
            [0.02 + i as f32 * 0.3 / 767.; 3]
        }
    }));
    for flag in &mut clipped.source_clipped[768..] {
        *flag = true;
    }
    let saturated = sample((0..1024).map(|i| {
        let n = 0.02 + i as f32 * 0.3 / 1023.;
        [n * 3., n * 0.6, n * 0.2]
    }));
    for (name, input) in [
        ("bright", bright),
        ("low contrast", low_contrast),
        ("high key", high_key),
        ("clipped source", clipped),
        ("saturated", saturated),
    ] {
        let report = solve(
            &input,
            AutoToneTargets::default(),
            compile,
            &Cancel::never(),
        )
        .unwrap();
        assert_eq!(report.values, by_the_design(&input), "{name}");
    }
}

#[test]
fn auto_tone_endpoint_searches_match_exhaustive_constraints_on_neutral_samples() {
    let input = ramp();
    let targets = AutoToneTargets::default();
    let report = solve(&input, targets, compile, &Cancel::never()).unwrap();
    let mut values = report.values;
    values.vibrance = 0.;
    values.saturation = 0.;
    let acceptable_white: Vec<i32> = (-60..=60)
        .filter(|&n| {
            let mut candidate = values;
            candidate.whites = f64::from(n);
            statistics(
                &input,
                &compile(candidate).unwrap(),
                false,
                false,
                &Cancel::never(),
            )
            .unwrap()
            .highlight_clip
                <= targets.clipping
        })
        .collect();
    let acceptable_black: Vec<i32> = (-60..=60)
        .filter(|&n| {
            let mut candidate = values;
            candidate.blacks = f64::from(n);
            statistics(
                &input,
                &compile(candidate).unwrap(),
                false,
                false,
                &Cancel::never(),
            )
            .unwrap()
            .shadow_clip
                <= targets.clipping
        })
        .collect();
    assert_eq!(values.whites, f64::from(*acceptable_white.last().unwrap()));
    assert_eq!(values.blacks, f64::from(*acceptable_black.first().unwrap()));
}

/// Non-finite samples are counted and left out; a point already clipped in the source is left
/// out of the clipping fractions, so a clipped sky does not darken the photograph: the same frame
/// with its sky flagged takes Whites the unflagged frame cannot.
#[test]
fn auto_tone_counts_exclusions_and_never_darkens_a_sky_clipped_in_the_source() {
    let mut input = ramp();
    input.rgb[1] = [f32::NAN; 3];
    // Add one finite sample so the grid still has the minimum usable count.
    input.grid = [33, 32];
    input.rgb.push([0.04; 3]);
    input.source_clipped.push(false);
    let report = solve(
        &input,
        AutoToneTargets::default(),
        compile,
        &Cancel::never(),
    )
    .unwrap();
    assert_eq!(report.sample.non_finite, 1);
    assert_eq!(report.sample.usable, 1024);
    assert_eq!(report.sample.source_clipped, 0);

    let sky = |flagged: bool| {
        let mut input = sample((0..1024).map(|i| {
            if i >= 960 {
                [4.; 3]
            } else {
                [0.02 + i as f32 * 0.2 / 959.; 3]
            }
        }));
        input.source_clipped[960..].fill(flagged);
        solve(
            &input,
            AutoToneTargets::default(),
            compile,
            &Cancel::never(),
        )
        .unwrap()
    };
    let (flagged, unflagged) = (sky(true), sky(false));
    assert_eq!(flagged.sample.source_clipped, 64);
    assert_eq!(flagged.tone.highlight_clip, 0.);
    assert_eq!(flagged.values.whites, 60.);
    assert!(
        unflagged.values.whites < flagged.values.whites,
        "{unflagged:?}"
    );
}

#[test]
fn auto_tone_bounded_grid_refuses_malformed_input_before_compiling() {
    let mut input = ramp();
    input.grid = [2048, 2048];
    assert!(
        solve(
            &input,
            AutoToneTargets::default(),
            |_| panic!("invalid grid must not compile"),
            &Cancel::never()
        )
        .is_err()
    );
}

#[test]
fn auto_tone_first_exposure_matches_exhaustive_picture_oracles_with_bright_and_coloured_inputs() {
    let targets = AutoToneTargets::default();
    for input in [
        sample((0..1024).map(|i| [0.1 + i as f32 * 1.9 / 1023.; 3])),
        sample((0..1024).map(|i| [0.5 + i as f32 * 0.5 / 1023.; 3])),
        sample((0..1024).map(|i| {
            let n = 0.005 + i as f32 * 0.2 / 1023.;
            [n * 4., n, n * 0.25]
        })),
    ] {
        let report = solve(&input, targets, compile, &Cancel::never()).unwrap();
        let exhaustive: Vec<_> = (-400..=400)
            .map(|e| {
                let values = AutoToneValues {
                    exposure: f64::from(e) / 100.,
                    ..AutoToneValues::default()
                };
                statistics(
                    &input,
                    &compile(values).unwrap(),
                    false,
                    false,
                    &Cancel::never(),
                )
                .unwrap()
            })
            .collect();
        let best = exhaustive
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                (a.p50 - targets.median)
                    .abs()
                    .total_cmp(&(b.p50 - targets.median).abs())
            })
            .unwrap()
            .0;
        let guarded = (0..=best)
            .rev()
            .find(|&i| exhaustive[i].near_white <= targets.bright_guard)
            .unwrap_or(0);
        assert_eq!(report.bands, exhaustive[guarded]);
        assert_eq!(
            report.values.highlights,
            (-250. * report.bands.bright).clamp(-100., 0.).round()
        );
        assert_eq!(
            report.values.shadows,
            (200. * report.bands.dark).clamp(0., 60.).round()
        );
    }
}

#[test]
fn auto_tone_look_uses_the_actual_compiled_forward_model() {
    let registry = crate::ModuleRegistry::builtin();
    let look = registry.module("luxforge.look").unwrap();
    let mut payload = look
        .original(&crate::OriginalContext {
            source: crate::SourceTag::Raw,
            raw: None,
            header: &crate::catalog_types::HeaderMetadata::default(),
            preferences: crate::OriginalPreferences::default(),
        })
        .unwrap()
        .unwrap()
        .payload;
    payload["amount"] = json!(200.);
    for input in [
        ramp(),
        sample((0..1024).map(|i| [0.2 + i as f32 * 1.8 / 1023.; 3])),
    ] {
        let Processing::Color(unit) = look
            .compile(
                crate::LOOK_EFFECT,
                EFFECT_FORMAT,
                &payload,
                CompileStage::exact(Stage {
                    width: 32,
                    height: 32,
                }),
            )
            .unwrap()
        else {
            panic!("Look is pointwise");
        };
        let with_look = |values| -> Result<Vec<ColorOperation>, Error> {
            let mut operations = compile(values)?;
            operations.push(unit.clone());
            Ok(operations)
        };
        // The production model over Basic and this Look, against the hand-built units above.
        let layers = [
            crate::Layer::new(BASIC_EFFECT, json!({})),
            crate::Layer::new(crate::LOOK_EFFECT, payload.clone()),
        ];
        let model = super::super::forward_model(
            &registry,
            &layers,
            0,
            CompileStage::exact(Stage {
                width: 32,
                height: 32,
            }),
        )
        .unwrap();
        let report = model
            .solve(&input, AutoToneTargets::default(), &Cancel::never())
            .unwrap();
        assert_eq!(report.exposure_search, ExposureSearch::Exhaustive);
        let plain = solve(
            &input,
            AutoToneTargets::default(),
            compile,
            &Cancel::never(),
        )
        .unwrap();
        assert_ne!(report.values, plain.values);
        let operations = with_look(report.values).unwrap();
        let mut rendered = input.clone();
        for operation in &operations {
            for unit in operation.units() {
                unit.apply_row(0, 0, &mut rendered.rgb);
            }
        }
        assert_eq!(
            report.output,
            picture_statistics(&rendered, &Cancel::never()).unwrap()
        );
        let exhaustive: Vec<_> = (-400..=400)
            .map(|e| {
                statistics(
                    &input,
                    &with_look(AutoToneValues {
                        exposure: f64::from(e) / 100.,
                        ..Default::default()
                    })
                    .unwrap(),
                    false,
                    false,
                    &Cancel::never(),
                )
                .unwrap()
            })
            .collect();
        let best = exhaustive
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| (a.p50 - 0.46).abs().total_cmp(&(b.p50 - 0.46).abs()))
            .unwrap()
            .0;
        let guarded = (0..=best)
            .rev()
            .find(|&i| exhaustive[i].near_white <= 0.02)
            .unwrap_or(0);
        assert_eq!(report.bands, exhaustive[guarded]);
    }
}
