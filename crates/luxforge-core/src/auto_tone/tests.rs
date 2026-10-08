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

fn sample(rgb: impl Iterator<Item = [f32; 3]>) -> AnalysisSample {
    AnalysisSample {
        grid: [32, 32],
        rgb: rgb.take(1024).collect(),
        source_white: vec![false; 1024],
    }
}

fn ramp() -> AnalysisSample {
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
    input.source_white[1023] = true;
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
        assert_eq!(error.data.unwrap()["auto_tone"]["reason"], reason);
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

#[test]
fn auto_tone_dark_neutral_ramp_has_hand_computed_first_exposure_and_colour() {
    let report = solve(
        &ramp(),
        AutoToneTargets::default(),
        compile,
        &Cancel::never(),
    )
    .unwrap();
    let input_median = f64::from(ramp().rgb[511][0]);
    let target_linear = ((0.46_f64 + 0.055) / 1.055).powf(2.4);
    let ideal = (target_linear / input_median).log2();
    // First Exposure is within half a field step of the analytical neutral-ramp solution.
    let measured = ((report.bands.p50 + 0.055) / 1.055).powf(2.4);
    assert!(((measured / input_median).log2() - ideal).abs() <= 0.0051);
    assert_eq!(report.values.highlights, 0.);
    assert_eq!(report.values.vibrance, 25.);
    assert_eq!(report.values.saturation, 0.);
    for (value, (min, max)) in report.values.array().into_iter().zip(BOUNDS) {
        assert!((min..=max).contains(&value));
    }
    assert_eq!(
        report.values.exposure * 100.,
        (report.values.exposure * 100.).round()
    );
    assert!(
        serde_json::to_vec(&report).unwrap().len() < 3500,
        "leave room for layer provenance in the 4 KiB query"
    );
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

#[test]
fn auto_tone_is_deterministic_across_pool_sizes_and_reports_unreachable_clipping() {
    let mut input = ramp();
    input.rgb[0] = [0.; 3];
    input.rgb[1] = [f32::NAN; 3];
    // Add one finite sample so the grid still has the minimum usable count.
    input.grid = [33, 32];
    input.rgb.push([0.04; 3]);
    input.source_white.push(false);
    let runs: Vec<_> = [1, 2, 4]
        .map(|threads| {
            rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap()
                .install(|| {
                    solve(
                        &input,
                        AutoToneTargets::default(),
                        compile,
                        &Cancel::never(),
                    )
                    .unwrap()
                })
        })
        .into();
    assert_eq!(runs[0], runs[1]);
    assert_eq!(runs[1], runs[2]);
    assert_eq!(runs[0].sample.non_finite, 1);
    assert_eq!(runs[0].sample.usable, 1024);
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
        let with_look = |values| {
            let mut operations = compile(values)?;
            operations.push(unit.clone());
            Ok(operations)
        };
        let report = solve_with_exposure_search(
            &input,
            AutoToneTargets::default(),
            with_look,
            ExposureSearch::Exhaustive,
            &Cancel::never(),
        )
        .unwrap();
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
