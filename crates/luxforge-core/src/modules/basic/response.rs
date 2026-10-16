//! The Basic sliders' responses ([`super::super::response`]): Contrast, Highlights, Shadows,
//! Whites, Blacks, Vibrance and Saturation. Exposure, Temperature and Tint act on the scene scale
//! (`scene.rs`) and have none.
pub use super::super::response::Response;
use super::super::response::Responses;
use std::sync::LazyLock;

static TABLE: LazyLock<Responses> = LazyLock::new(|| {
    let table = Responses::parse(include_str!("response.json"), "Basic");
    for (field, response) in table.iter() {
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
    TABLE.get(field)
}

/// The strength Basic's `field` applies at slider `value`.
pub(super) fn strength(field: &str, value: f64) -> f64 {
    TABLE.strength(field, value)
}

/// The slider value of Basic's `field` that applies `strength`, `None` beyond its response.
pub(super) fn value(field: &str, strength: f64) -> Option<f64> {
    response(field).map_or(Some(strength), |response| response.value(strength))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Every realigned field's response spans its declared range, keeps 0 exactly neutral and
    /// inverts to the value it came from.
    #[test]
    fn each_response_spans_its_field_keeps_neutral_and_inverts() {
        let ranges = BTreeMap::from([
            ("contrast", (-100.0, 100.0)),
            ("highlights", (-100.0, 100.0)),
            ("shadows", (-100.0, 100.0)),
            ("whites", (-100.0, 100.0)),
            ("blacks", (-100.0, 100.0)),
            ("vibrance", (-100.0, 100.0)),
            ("saturation", (-100.0, 100.0)),
        ]);
        assert!(TABLE.iter().next().is_some());
        for (field, response) in TABLE.iter() {
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
    /// fields' end strengths, and its Highlights and Shadows stage alone keeps a slope of at least
    /// 0.1.
    #[test]
    fn every_extended_strength_keeps_its_unit_stable() {
        use super::super::tone::Tone;
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
