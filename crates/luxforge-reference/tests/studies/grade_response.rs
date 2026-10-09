//! The grading alignment slice's measures and fits (`crates/luxforge-reference/src/grade_response.rs`)
//! on cases with known answers: CIEDE2000 against Sharma, Wu and Dalal's published pairs, and
//! level A fits that must recover a known scale, report a range shortfall, stay monotone through a
//! noisy sample and recover a known hue rotation.

use luxforge_reference::colour::{self, Oklab};
use luxforge_reference::grade::{self, GradeParams, Wheel};
use luxforge_reference::grade_response::{
    self, Response, Sampled, circular_difference, delta_e_2000, fit_hue, fit_monotone, response,
};

/// Sharma, Wu and Dalal (2005), Table 1: pairs 1, 7, 13, 17, 19, 25 and 34 and their published
/// differences, to four decimals.
#[test]
fn ciede2000_matches_the_published_pairs() {
    for (first, second, expected) in [
        ([50.0, 2.6772, -79.7751], [50.0, 0.0, -82.7485], 2.0425),
        ([50.0, 0.0, 0.0], [50.0, -1.0, 2.0], 2.3669),
        ([50.0, 2.49, -0.001], [50.0, -2.49, 0.0011], 7.2195),
        ([50.0, 2.5, 0.0], [73.0, 25.0, -18.0], 27.1492),
        ([50.0, 2.5, 0.0], [50.0, 3.1736, 0.5854], 1.0),
        (
            [60.2574, -34.0099, 36.2677],
            [60.4626, -34.1751, 39.4387],
            1.2644,
        ),
        (
            [2.0776, 0.0795, -1.1350],
            [0.9033, -0.0636, -0.5514],
            0.9082,
        ),
    ] {
        let measured = delta_e_2000(first, second);
        assert!(
            (measured - expected).abs() < 5e-5,
            "{first:?} against {second:?}: {measured}, published {expected}"
        );
    }
}

/// The patches a synthetic round measures: a grey wedge and a hue ring.
fn patches() -> Vec<[f64; 3]> {
    let mut patches: Vec<[f64; 3]> = (1..10)
        .map(|step| {
            colour::from_oklab(Oklab {
                l: f64::from(step) / 10.0,
                a: 0.0,
                b: 0.0,
            })
        })
        .collect();
    for step in 0..8 {
        let angle = f64::from(step) * 45f64.to_radians();
        patches.push(colour::from_oklab(Oklab {
            l: 0.6,
            a: 0.08 * angle.cos(),
            b: 0.08 * angle.sin(),
        }));
    }
    patches
}

/// The reference's responses to Midtones saturation over `values`, with Midtones hue `hue`, each
/// value first passed through `map`: a synthetic "other editor" whose saturation scale differs by
/// a known function.
fn sampled(values: &[f64], hue: f64, map: impl Fn(f64) -> f64) -> Sampled {
    let neutrals = patches();
    Sampled {
        samples: values
            .iter()
            .map(|value| {
                let mut params = GradeParams::default();
                params.wheels[1] = Wheel {
                    hue,
                    saturation: map(*value),
                    luminance: 0.0,
                };
                let responses = neutrals
                    .iter()
                    .map(|patch| response(*patch, grade::apply(*patch, &params)))
                    .collect();
                (*value, responses)
            })
            .collect(),
        neutrals,
    }
}

fn grid(step: f64) -> Vec<f64> {
    (0..=(100.0 / step) as usize)
        .map(|index| index as f64 * step)
        .collect()
}

/// An editor whose saturation does half of Luxforge's at the same value fits as `v / 2`, with
/// essentially no residual and no shortfall.
#[test]
fn a_known_scale_is_recovered() {
    let luxforge = sampled(&grid(5.0), 30.0, |v| v);
    let other = sampled(&[0.0, 20.0, 40.0, 60.0, 80.0, 100.0], 30.0, |v| v / 2.0);
    for point in fit_monotone(&luxforge, &other) {
        assert!(
            (point.luxforge - point.lightroom / 2.0).abs() <= 0.25,
            "{point:?}"
        );
        assert!(point.residual < 0.05, "{point:?}");
        assert!(!point.shortfall, "{point:?}");
    }
}

/// An editor twice as strong runs out of Luxforge's range above 50: those points sit at the end of
/// the range, are flagged as a shortfall and keep a real residual, never clamped silently.
#[test]
fn a_range_shortfall_is_reported() {
    let luxforge = sampled(&grid(5.0), 200.0, |v| v);
    let stronger = sampled(&[25.0, 50.0, 75.0, 100.0], 200.0, |v| {
        // The reference's saturation response at twice the value is its tint scaled by two, which
        // no Luxforge value reaches past 100.
        v
    });
    // Double the other editor's responses directly: a response twice Luxforge's at each value.
    let doubled = Sampled {
        neutrals: stronger.neutrals.clone(),
        samples: stronger
            .samples
            .iter()
            .map(|(value, responses)| {
                (
                    *value,
                    responses
                        .iter()
                        .map(|r| Response {
                            dl: 2.0 * r.dl,
                            da: 2.0 * r.da,
                            db: 2.0 * r.db,
                        })
                        .collect(),
                )
            })
            .collect(),
    };
    let fit = fit_monotone(&luxforge, &doubled);
    assert!(
        (fit[0].luxforge - 50.0).abs() <= 1.0 && !fit[0].shortfall,
        "{:?}",
        fit[0]
    );
    for point in &fit[2..] {
        assert_eq!(point.luxforge, 100.0, "{point:?}");
        assert!(point.shortfall && point.residual > 0.5, "{point:?}");
    }
}

/// A noisy measurement that would fold the map is pooled into a non-decreasing one.
#[test]
fn a_fitted_magnitude_map_is_monotone() {
    let luxforge = sampled(&grid(5.0), 120.0, |v| v);
    let values = [10.0, 20.0, 30.0, 40.0, 50.0];
    // A measured response at 30 that looks like 45 and one at 40 that looks like 25.
    let noisy = sampled(&values, 120.0, |v| match v as i32 {
        30 => 45.0,
        40 => 25.0,
        _ => v,
    });
    let fit = fit_monotone(&luxforge, &noisy);
    for pair in fit.windows(2) {
        assert!(pair[0].luxforge <= pair[1].luxforge, "{fit:?}");
    }
}

/// An editor whose wheel is rotated by 20 degrees fits each hue to the Luxforge hue 20 degrees on,
/// circularly across the seam.
#[test]
fn a_known_hue_rotation_is_recovered() {
    let neutrals = patches();
    let at_hue = |hue: f64| -> Vec<Response> {
        let mut params = GradeParams::default();
        params.wheels[3] = Wheel {
            hue,
            saturation: 50.0,
            luminance: 0.0,
        };
        neutrals
            .iter()
            .map(|patch| response(*patch, grade::apply(*patch, &params)))
            .collect()
    };
    let luxforge = Sampled {
        neutrals: neutrals.clone(),
        samples: (0..360)
            .map(|hue| (f64::from(hue), at_hue(f64::from(hue))))
            .collect(),
    };
    let rotated = Sampled {
        neutrals: neutrals.clone(),
        samples: [0.0, 90.0, 180.0, 350.0]
            .map(|hue| (hue, at_hue((hue + 20.0) % 360.0)))
            .to_vec(),
    };
    for point in fit_hue(&luxforge, &rotated) {
        let expected = (point.lightroom + 20.0) % 360.0;
        assert!(
            circular_difference(point.luxforge, expected).abs() <= 1.0,
            "{point:?}"
        );
        assert!(point.residual < 1.0, "{point:?}");
    }
    assert!(grade_response::tint_angle(&at_hue(0.0)).is_some());
    assert!(grade_response::tint_angle(&[Response::default(); 3]).is_none());
}
