//! The grading alignment slice's measures and fits (`crates/luxforge-reference/src/grade_response.rs`)
//! on cases with known answers: level A fits that must recover a known scale, report a range
//! shortfall, stay monotone through a noisy sample and recover a known hue rotation. Their
//! CIEDE2000 is the preview-error measure's, checked against Sharma, Wu and Dalal's published pairs
//! there.

use luxforge_reference::colour::{self, Oklab};
use luxforge_reference::grade::{self, GradeParams, Wheel};
use luxforge_reference::grade_response::{
    self, Response, Sampled, circular_difference, fit_hue, fit_monotone, response,
};

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

/// An editor twice as strong runs out of Luxforge's range above 50: its 25 and 50 are matched at
/// Luxforge's 50 and 100, and its 75 and 100 sit at the end of the range, are flagged as a
/// shortfall and keep a real residual, never clamped silently.
#[test]
fn a_range_shortfall_is_reported() {
    let luxforge = sampled(&grid(5.0), 200.0, |v| v);
    // The reference's tint chroma is linear in saturation and unbounded above 100, so the stronger
    // editor's response at each value is the reference's at twice it.
    let stronger = sampled(&[25.0, 50.0, 75.0, 100.0], 200.0, |v| 2.0 * v);
    let fit = fit_monotone(&luxforge, &stronger);
    assert_eq!(fit.len(), 4);
    for (point, expected) in fit[..2].iter().zip([50.0, 100.0]) {
        assert!(
            (point.luxforge - expected).abs() <= 1.0 && point.residual < 0.05 && !point.shortfall,
            "{point:?}"
        );
    }
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
