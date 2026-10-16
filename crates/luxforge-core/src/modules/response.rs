//! Slider responses (`docs/design/lightroom-alignment.md`, "Changing a control"): for each
//! realigned field of a module, the strength its unit applies at each slider value, so that a
//! value does about what the same value does in Lightroom Classic. The units' algorithms are
//! unchanged and receive the strength; the slider's value is what the payload stores, the history
//! names and every API reads.
//!
//! A response is a frozen table of knots `[value, strength]` read by linear interpolation: both
//! strictly increasing, spanning the field's declared range and passing through the field's
//! neutral `[n, n]`, so the neutral value is exactly neutral and every slider step moves the
//! picture. A strength beyond the unit's former range is a range extension, kept where the unit
//! stays stable. Each module's table is its `response.json`, written by `cargo xtask lr-align
//! fold` from the measured rounds; a refit is a reviewed change of that file and of the module's
//! format marker.
use serde::Deserialize;
use std::collections::BTreeMap;

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

/// One module's frozen responses, by field.
#[derive(Debug)]
pub(crate) struct Responses {
    responses: BTreeMap<String, Response>,
}

impl Responses {
    /// The table `json` holds, checked: each response strictly increasing in both coordinates.
    pub(crate) fn parse(json: &str, module: &str) -> Self {
        let table: Table = serde_json::from_str(json)
            .unwrap_or_else(|error| panic!("{module}'s frozen response table parses: {error}"));
        assert_eq!(table.format, "luxforge.slider-response", "{module}");
        assert_eq!(table.version, 1, "{module}");
        for (field, response) in &table.responses {
            assert!(
                response
                    .knots
                    .windows(2)
                    .all(|pair| pair[0][0] < pair[1][0] && pair[0][1] < pair[1][1]),
                "{module}'s {field} response is not strictly increasing"
            );
        }
        Self {
            responses: table.responses,
        }
    }

    /// The response of `field`, if it is realigned.
    pub(crate) fn get(&self, field: &str) -> Option<&Response> {
        self.responses.get(field)
    }

    /// Every realigned field and its response.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&String, &Response)> {
        self.responses.iter()
    }

    /// The strength `field` applies at slider `value`: its response's, or the value itself.
    pub(crate) fn strength(&self, field: &str, value: f64) -> f64 {
        self.get(field)
            .map_or(value, |response| response.strength(value))
    }
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
