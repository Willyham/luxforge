//! The Basic sliders' responses (`docs/design/lightroom-alignment.md`, "Changing a control"): for
//! each realigned field, the strength its unit applies at each slider value, so that a value does
//! about what the same value does in Lightroom. The units' algorithms are unchanged and receive the
//! strength; the slider's value is what the payload stores, the history names and every API reads.
//!
//! A response is a frozen table of knots `[value, strength]` read by linear interpolation: both
//! strictly increasing, spanning the field's declared range and passing through `[0, 0]`, so the
//! neutral value is exactly neutral and every slider step moves the picture. A strength beyond the
//! unit's former ±100 (or ±5 EV) is a range extension, kept where the unit stays stable. The table
//! is `response.json`, written by `cargo xtask lr-align fold` from the measured rounds; a refit is a
//! reviewed change of that file and of Basic's format marker.
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::LazyLock;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Table {
    format: String,
    version: u64,
    #[allow(dead_code)]
    basis: String,
    responses: BTreeMap<String, Response>,
}

/// One field's response.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Response {
    /// `[value, strength]`, both strictly increasing, through `[0, 0]`.
    pub knots: Vec<[f64; 2]>,
}

static TABLE: LazyLock<Table> = LazyLock::new(|| {
    let table: Table = serde_json::from_str(include_str!("response.json"))
        .expect("the frozen Basic response table parses");
    assert_eq!(table.format, "luxforge.basic-response");
    assert_eq!(table.version, 1);
    for (field, response) in &table.responses {
        assert!(
            response
                .knots
                .windows(2)
                .all(|pair| pair[0][0] < pair[1][0] && pair[0][1] < pair[1][1]),
            "{field}'s response is not strictly increasing"
        );
        assert!(
            response.knots.contains(&[0.0, 0.0]),
            "{field}'s response does not keep 0 neutral"
        );
    }
    table
});

/// The response of Basic's `field`, if it is realigned; an unlisted field applies its value as its
/// strength.
pub fn response(field: &str) -> Option<&'static Response> {
    TABLE.responses.get(field)
}

/// The strength Basic's `field` applies at slider `value`.
pub(super) fn strength(field: &str, value: f64) -> f64 {
    response(field).map_or(value, |response| response.strength(value))
}

/// The slider value of Basic's `field` that applies `strength`, `None` beyond its response.
pub(super) fn value(field: &str, strength: f64) -> Option<f64> {
    response(field).map_or(Some(strength), |response| response.value(strength))
}

/// `y` at `x` by linear interpolation over `points`, strictly increasing in `x`, exact at a point
/// and `None` outside their span.
fn interpolate(points: impl Iterator<Item = (f64, f64)> + Clone, x: f64) -> Option<f64> {
    let mut previous: Option<(f64, f64)> = None;
    for (px, py) in points {
        if x == px {
            return Some(py);
        }
        if let Some((qx, qy)) = previous
            && x > qx
            && x < px
        {
            return Some(qy + (x - qx) / (px - qx) * (py - qy));
        }
        previous = Some((px, py));
    }
    None
}

impl Response {
    /// The strength at `value`. The field's range check runs first, so a value outside the
    /// knots' span is not reached; it would continue the end segment.
    pub fn strength(&self, value: f64) -> f64 {
        interpolate(self.knots.iter().map(|k| (k[0], k[1])), value).unwrap_or_else(|| {
            let (a, b) = if value < self.knots[0][0] {
                (self.knots[0], self.knots[1])
            } else {
                let n = self.knots.len();
                (self.knots[n - 2], self.knots[n - 1])
            };
            a[1] + (value - a[0]) / (b[0] - a[0]) * (b[1] - a[1])
        })
    }

    /// The slider value that applies `strength`, `None` beyond the strengths the slider reaches.
    pub fn value(&self, strength: f64) -> Option<f64> {
        interpolate(self.knots.iter().map(|k| (k[1], k[0])), strength)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every realigned field's response spans its declared range, keeps 0 exactly neutral and
    /// inverts to the value it came from.
    #[test]
    fn each_response_spans_its_field_keeps_neutral_and_inverts() {
        let ranges = BTreeMap::from([
            ("exposure", (-5.0, 5.0)),
            ("contrast", (-100.0, 100.0)),
            ("highlights", (-100.0, 100.0)),
            ("shadows", (-100.0, 100.0)),
            ("whites", (-100.0, 100.0)),
            ("blacks", (-100.0, 100.0)),
            ("vibrance", (-100.0, 100.0)),
            ("saturation", (-100.0, 100.0)),
            ("temperature", (-100.0, 100.0)),
            ("tint", (-100.0, 100.0)),
        ]);
        assert!(!TABLE.responses.is_empty());
        for (field, response) in &TABLE.responses {
            let (low, high) = ranges[field.as_str()];
            assert_eq!(response.knots.first().unwrap()[0], low, "{field}");
            assert_eq!(response.knots.last().unwrap()[0], high, "{field}");
            assert_eq!(
                response.strength(0.0).to_bits(),
                0.0f64.to_bits(),
                "{field}"
            );
            assert_eq!(response.value(0.0), Some(0.0), "{field}");
            for step in 0..=200 {
                let v = low + (high - low) * f64::from(step) / 200.0;
                let back = response.value(response.strength(v)).unwrap();
                assert!((back - v).abs() < 1e-9, "{field} {v} -> {back}");
            }
            let beyond = response.knots.last().unwrap()[1] + 1.0;
            assert_eq!(response.value(beyond), None, "{field}");
        }
    }

    /// The strengths at each end of a field's response.
    fn ends(field: &str) -> [f64; 2] {
        let knots = &response(field).unwrap().knots;
        [knots[0][1], knots[knots.len() - 1][1]]
    }

    /// Where a response extends a strength past its former ±100, the unit stays stable (A10): the
    /// Tone unit's curve stays nondecreasing on a dense grey ramp at every corner of the five
    /// fields' end strengths, its Highlights and Shadows stage alone keeps a slope of at least 0.1,
    /// and white balance stays finite at every corner of Temperature and Tint.
    #[test]
    fn every_extended_strength_keeps_its_unit_stable() {
        use super::super::{tone::Tone, white_balance::WhiteBalance};
        use crate::modules::PointwiseColor;
        let luminance = |unit: &dyn PointwiseColor, grey: f32| {
            let mut row = [[grey; 3]];
            unit.apply_row(0, 0, &mut row);
            0.2126 * row[0][0] + 0.7152 * row[0][1] + 0.0722 * row[0][2]
        };
        let ramp: Vec<f32> = (0..=4000).map(|i| i as f32 / 4000.0).collect();
        let fields = ["contrast", "highlights", "shadows", "whites", "blacks"];
        for corner in 0..32 {
            let s: Vec<f64> = fields
                .iter()
                .enumerate()
                .map(|(i, field)| ends(field)[(corner >> i) & 1])
                .collect();
            let unit = Tone::new(s[0], s[1], s[2], s[3], s[4]);
            let out: Vec<f32> = ramp.iter().map(|g| luminance(&unit, *g)).collect();
            assert!(
                out.windows(2).all(|p| p[1] >= p[0] - 1e-6),
                "the tone curve at {s:?} decreases"
            );
        }
        for highlights in ends("highlights") {
            for shadows in ends("shadows") {
                let unit = Tone::new(0.0, highlights, shadows, 0.0, 0.0);
                let encoded = |g: f32| crate::colour::srgb::encode_f32(luminance(&unit, g));
                let slopes = ramp.windows(2).map(|p| {
                    let (a, b) = (
                        crate::colour::srgb::decode_f32(p[0]),
                        crate::colour::srgb::decode_f32(p[1]),
                    );
                    (encoded(b) - encoded(a)) / (p[1] - p[0])
                });
                let least = slopes.fold(f32::INFINITY, f32::min);
                assert!(
                    least >= 0.1,
                    "Highlights {highlights}, Shadows {shadows}: slope {least}"
                );
            }
        }
        // White balance at every corner of the two end strengths and at each axis' ends alone:
        // finite, with a positive luminance. A grey may leave the gamut there, as the pointwise
        // contract allows; the host clamps once at the end of the run.
        let temperatures = ends("temperature");
        let tints = ends("tint");
        let pairs = temperatures
            .iter()
            .flat_map(|t| tints.iter().map(move |n| (*t, *n)))
            .chain(temperatures.iter().map(|t| (*t, 0.0)))
            .chain(tints.iter().map(|n| (0.0, *n)));
        for (temperature, tint) in pairs {
            let unit = WhiteBalance::new(temperature, tint);
            assert!(unit.is_finite(), "{temperature}, {tint}");
            let mut row = [[0.5f32; 3]];
            unit.apply_row(0, 0, &mut row);
            let rgb = row[0];
            let y = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
            assert!(
                rgb.iter().all(|c| c.is_finite()) && y > 0.0,
                "{temperature}, {tint}: {rgb:?}"
            );
        }
    }

    #[test]
    fn a_response_interpolates_between_its_knots() {
        let response = Response {
            knots: vec![[-100.0, -50.0], [0.0, 0.0], [50.0, 100.0], [100.0, 120.0]],
        };
        assert_eq!(response.strength(-50.0), -25.0);
        assert_eq!(response.strength(25.0), 50.0);
        assert_eq!(response.strength(75.0), 110.0);
        assert_eq!(response.value(110.0), Some(75.0));
        assert_eq!(response.value(-60.0), None);
    }
}
