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
    by_the_design_through(input, compile)
}

/// [`by_the_design`] through the model `compile` builds.
fn by_the_design_through(
    input: &SampleGrid,
    compile: impl Fn(AutoToneValues) -> Result<Vec<ColorOperation>, Error>,
) -> AutoToneValues {
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
    let middle = f64::from(input.rgb[511][0]);
    let linear_target = ((0.46_f64 + 0.055) / 1.055).powf(2.4);
    let first = ((linear_target / middle).log2() * 100.).round() / 100.;
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
        assert_eq!(report.exposure_search, ExposureSearch::CoarseToFine);
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

/// The Look's original payload at `amount`.
fn look_payload(registry: &crate::ModuleRegistry, amount: f64) -> Value {
    let mut payload = registry
        .module("luxforge.look")
        .unwrap()
        .original(&crate::OriginalContext {
            source: crate::SourceTag::Raw,
            raw: None,
            header: &crate::catalog_types::HeaderMetadata::default(),
            preferences: crate::OriginalPreferences::default(),
        })
        .unwrap()
        .unwrap()
        .payload;
    payload["amount"] = json!(amount);
    payload
}

/// The nested selection finds, at every rank, exactly the value a sort of the whole slice holds
/// there, with ties, repeated ranks and every length from one.
#[test]
fn nested_percentiles_equal_the_sorted_slices_nearest_ranks() {
    let mut state = 0x9e37_79b9_u32;
    for length in 1..200 {
        let values: Vec<f64> = (0..length)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                f64::from(state >> 27) / 4.
            })
            .collect();
        let mut sorted = values.clone();
        sorted.sort_by(f64::total_cmp);
        let fractions = [0.01, 0.10, 0.10, 0.50, 0.90, 0.99, 1.];
        let found = percentiles(&mut values.clone(), fractions);
        for (fraction, found) in fractions.into_iter().zip(found) {
            let rank = ((length as f64 * fraction).ceil() as usize).saturating_sub(1);
            assert_eq!(
                found,
                sorted[rank.min(length - 1)],
                "{length} at {fraction}"
            );
        }
    }
}

/// The guided search answers exactly what the binary search does for every monotone predicate,
/// from every guess; for any predicate its answer meets the predicate, or is the low end, and the
/// next step fails it, or it is the high end.
#[test]
fn guided_search_answers_the_binary_search_for_every_monotone_predicate_and_guess() {
    let (low, high) = (-6, 6);
    for last in low - 1..=high {
        for guess in low - 2..=high + 2 {
            let monotone = |step: i32| Ok(step <= last);
            assert_eq!(
                guided_last_true(low, high, guess, monotone).unwrap(),
                last_true(low, high, monotone).unwrap(),
                "last {last}, guess {guess}"
            );
        }
    }
    for pattern in 0..1u32 << 7 {
        let test = |step: i32| Ok(pattern >> (step + 3) & 1 == 1);
        for guess in -3..=3 {
            let found = guided_last_true(-3, 3, guess, test).unwrap();
            assert!(found == -3 || test(found).unwrap(), "{pattern:b} {guess}");
            assert!(
                found == 3 || !test(found + 1).unwrap(),
                "{pattern:b} {guess}"
            );
        }
    }
}

/// A deterministic pseudo-random scene of `count` points: log-uniform luminance over seven stops
/// with mild colour, seeded so every run reads the same sample.
fn scene(count: usize, seed: u64) -> SampleGrid {
    let mut state = seed;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f64 / (1u64 << 53) as f64
    };
    let rgb: Vec<[f32; 3]> = (0..count)
        .map(|_| {
            let base = (next() * 7. - 7.5).exp2();
            [
                (base * (0.7 + next() * 0.6)) as f32,
                base as f32,
                (base * (0.6 + next() * 0.8)) as f32,
            ]
        })
        .collect();
    SampleGrid {
        grid: [128, count.div_ceil(128) as u32],
        source_clipped: vec![false; rgb.len()],
        rgb,
    }
}

/// Larger synthetic samples whose reductions are big enough to locate on, with every chunk and the
/// last partial one: a dark ramp, a bright ramp, a saturated ramp, a source-clipped sky, a scene,
/// a scene with non-finite points, and stripes — dark and bright points alternating in scan order,
/// on which a plain stride of an even number of points would read only the dark ones.
fn reduced_cases() -> Vec<(&'static str, SampleGrid)> {
    let count = 3 * CHUNK + 1000;
    let ramp = |from: f32, span: f32| SampleGrid {
        grid: [128, count.div_ceil(128) as u32],
        rgb: (0..count)
            .map(|i| [from + i as f32 * span / (count - 1) as f32; 3])
            .collect(),
        source_clipped: vec![false; count],
    };
    let mut sky = ramp(0.02, 0.4);
    for (rgb, flag) in sky
        .rgb
        .iter_mut()
        .zip(&mut sky.source_clipped)
        .skip(count - 900)
    {
        *rgb = [1.; 3];
        *flag = true;
    }
    let mut saturated = ramp(0.02, 0.3);
    for rgb in &mut saturated.rgb {
        *rgb = [rgb[0] * 3., rgb[1] * 0.6, rgb[2] * 0.2];
    }
    let mut holes = scene(count, 0x9e37_79b9_7f4a_7c15);
    for index in (0..count).step_by(97) {
        holes.rgb[index] = [f32::NAN; 3];
    }
    let mut stripes = ramp(0.02, 0.6);
    for rgb in stripes.rgb.iter_mut().step_by(2) {
        *rgb = rgb.map(|c| c * 0.05);
    }
    vec![
        ("stripes", stripes),
        ("dark ramp", ramp(0.01, 0.09)),
        ("bright ramp", ramp(0.3, 1.2)),
        ("saturated", saturated),
        ("clipped sky", sky),
        ("scene", scene(count, 0x2545_f491_4f6c_dd1d)),
        ("scene with holes", holes),
    ]
}

/// The reduction only decides where a search looks first: at strides that reduce the sample to
/// about a quarter, a tenth and the least it locates on, the whole report — every value and every
/// statistic — equals the solve without a reduction, whose values are the design's own steps tried
/// over every value of every field's grid ([`by_the_design`]). The design permits a reduction for
/// repeated evaluation only when it gives the whole sample's fractions and percentiles.
#[test]
fn the_reduction_locates_and_the_whole_sample_decides_every_value_and_statistic() {
    for (name, input) in reduced_cases() {
        let search = ExposureSearch::Monotone;
        let whole = |stride| {
            solve_reduced(
                &input,
                AutoToneTargets::default(),
                compile,
                search,
                stride,
                &Cancel::never(),
            )
            .unwrap()
        };
        let unreduced = whole(1);
        assert_eq!(unreduced.values, by_the_design(&input), "{name}");
        for stride in [4, 10, input.rgb.len() / 1024] {
            assert!(
                View::new(&input, stride).usable() >= 1024,
                "{name} locates at {stride}"
            );
            assert_eq!(whole(stride), unreduced, "{name} at stride {stride}");
        }
        assert_eq!(
            unreduced.output,
            statistics(
                &input,
                &compile(unreduced.values).unwrap(),
                false,
                true,
                &Cancel::never()
            )
            .unwrap(),
            "{name}: the reported statistics are the whole sample's"
        );
    }
}

/// The solve is the same on every size of the shared pool: each chunk of the sample is one task,
/// whose counts and chroma sum are added in chunk order, so neither the values nor any statistic
/// depends on how many threads evaluate them, through Basic alone and through a Look that needs
/// the coarse-to-fine Exposure search.
#[test]
fn the_solve_is_the_same_on_every_pool_size() {
    let input = scene(5 * CHUNK + 123, 0x0123_4567_89ab_cdef);
    let registry = crate::ModuleRegistry::builtin();
    let layers = [
        crate::Layer::new(BASIC_EFFECT, json!({})),
        crate::Layer::new(crate::LOOK_EFFECT, look_payload(&registry, 200.)),
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
    let solves = |threads: usize| {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        pool.install(|| {
            (
                solve(
                    &input,
                    AutoToneTargets::default(),
                    compile,
                    &Cancel::never(),
                )
                .unwrap(),
                model
                    .solve(&input, AutoToneTargets::default(), &Cancel::never())
                    .unwrap(),
            )
        })
    };
    let one = solves(1);
    assert_eq!(one.1.exposure_search, ExposureSearch::CoarseToFine);
    assert!(one.0.tone.mean_chroma > 0., "the chroma sum is exercised");
    for threads in [2, 3, 8] {
        assert_eq!(solves(threads), one, "{threads} threads");
    }
}

/// The global Look at an amount above 100, whose model is not monotonic in Exposure, through the
/// coarse-to-fine search, without a reduction and reduced to a quarter, a tenth and the least the
/// solve locates on (about 1,100 points, whose medians miss the whole sample's by up to a third
/// of a stop here): every value is the exhaustive design's ([`by_the_design_through`]), the
/// median nearest the target over all 801 steps of the whole sample, then the nearest lower step
/// within the bright guard. The coarse steps choose the basins; the whole sample finds its
/// crossing of the target, so the reduction's bias moves no value.
#[test]
fn the_coarse_to_fine_exposure_equals_the_exhaustive_design_through_a_look() {
    let registry = crate::ModuleRegistry::builtin();
    let look = look_payload(&registry, 200.);
    let Processing::Color(unit) = registry
        .module("luxforge.look")
        .unwrap()
        .compile(
            crate::LOOK_EFFECT,
            EFFECT_FORMAT,
            &look,
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
    for (name, input) in reduced_cases() {
        let exhaustive = by_the_design_through(&input, with_look);
        for stride in [1, 4, 10, input.rgb.len() / 1024] {
            let report = solve_reduced(
                &input,
                AutoToneTargets::default(),
                with_look,
                ExposureSearch::CoarseToFine,
                stride,
                &Cancel::never(),
            )
            .unwrap();
            assert_eq!(report.values, exhaustive, "{name} at stride {stride}");
        }
    }
}
