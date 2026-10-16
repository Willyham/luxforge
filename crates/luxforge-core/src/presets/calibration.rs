//! The Lightroom alignment's calibrated conversions (`docs/design/lightroom-alignment.md`,
//! "Fitting the translation"; `docs/design/presets.md`, "Mapping"): for a setting the importer
//! transfers by value, the Luxforge value whose measured response is closest to Lightroom's,
//! from the frozen knots `cargo xtask lr-align measure` fitted, read by linear interpolation.
//!
//! A value at or past a range shortfall, where the closest Luxforge value would lie beyond its
//! range, is refused rather than clamped, as every out-of-range value is. The table is the round's
//! `maps.json`, frozen here; a new round that changes a map is a reviewed change of this file.
use serde::Deserialize;
use std::sync::LazyLock;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Table {
    format: String,
    version: u64,
    round: u32,
    #[allow(dead_code)]
    basis: String,
    maps: Vec<Calibration>,
}

/// One setting's calibrated conversion.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Calibration {
    pub setting: String,
    pub action: String,
    pub field: String,
    /// `[Lightroom, Luxforge]`, Lightroom strictly increasing, Luxforge never decreasing.
    pub knots: Vec<[f64; 2]>,
    /// Lightroom values at or past which the conversion is a range shortfall.
    pub refused_at_or_below: Option<f64>,
    pub refused_at_or_above: Option<f64>,
    #[allow(dead_code)]
    family: String,
}

static TABLE: LazyLock<Table> = LazyLock::new(|| {
    let table: Table = serde_json::from_str(include_str!("alignment-round-1.json"))
        .expect("the frozen calibration table parses");
    assert_eq!(table.format, "luxforge.lightroom-calibration");
    assert_eq!(table.version, 1);
    table
});

/// The calibrated conversion of Lightroom's `setting` onto `action.field`, if it has one: a map
/// whose target is not the row's own is never applied.
pub(super) fn calibrated(setting: &str, action: &str, field: &str) -> Option<&'static Calibration> {
    TABLE
        .maps
        .iter()
        .find(|map| map.setting == setting && map.action == action && map.field == field)
}

/// The calibrated conversion of Lightroom's `setting`, if it has one.
pub(super) fn calibration(setting: &str) -> Option<&'static Calibration> {
    TABLE.maps.iter().find(|map| map.setting == setting)
}

/// What the report names a calibrated conversion's rule.
pub(super) fn rule() -> String {
    format!(
        "calibrated by the Lightroom alignment, round {}",
        TABLE.round
    )
}

impl Calibration {
    /// The Luxforge value for Lightroom's `value`, or why there is none: a range shortfall, or a
    /// value outside the calibrated span.
    pub fn convert(&self, value: f64) -> Result<f64, String> {
        let shortfall = self.refused_at_or_below.is_some_and(|low| value <= low)
            || self.refused_at_or_above.is_some_and(|high| value >= high);
        if shortfall {
            return Err(format!(
                "beyond Luxforge's range through the Lightroom alignment calibration (round {})",
                TABLE.round
            ));
        }
        let span = |end: usize| self.knots[end][0];
        if !(value >= span(0) && value <= span(self.knots.len() - 1)) {
            return Err(format!(
                "outside the calibrated range {}..{}",
                span(0),
                span(self.knots.len() - 1)
            ));
        }
        let upper = self
            .knots
            .iter()
            .position(|[lightroom, _]| *lightroom >= value)
            .expect("inside the span");
        let [x1, y1] = self.knots[upper];
        if upper == 0 || x1 == value {
            return Ok(y1);
        }
        let [x0, y0] = self.knots[upper - 1];
        Ok(y0 + (value - x0) / (x1 - x0) * (y1 - y0))
    }

    #[cfg(test)]
    pub fn all() -> &'static [Calibration] {
        &TABLE.maps
    }
}

/// The calibrated conversion of Lightroom's `setting` at `value`: `None` when the setting has no
/// calibration, else the Luxforge value or why the value is refused. The alignment rig reads it
/// back through this to check a round's variants.
pub fn lightroom_calibration(setting: &str, value: f64) -> Option<Result<f64, String>> {
    calibration(setting).map(|map| map.convert(value))
}

#[cfg(test)]
mod tests {
    use super::super::mapping::{Rule, lookup};
    use super::*;
    use crate::{ModuleRegistry, inspect_preset};

    /// A modern XMP preset holding `setting` at `value`.
    fn preset(setting: &str, value: f64) -> String {
        format!(
            "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF \
             xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description \
             xmlns:crs=\"http://ns.adobe.com/camera-raw-settings/1.0/\" crs:ProcessVersion=\"11.0\" \
             crs:{setting}=\"{value}\"/></rdf:RDF></x:xmpmeta>"
        )
    }

    /// The frozen table, empty while every measured slider follows Lightroom's response itself
    /// (A11), names only value-transfer rows, onto their own targets, across exactly
    /// the Lightroom range each row claims; its knots rise strictly in Lightroom, never fall in
    /// Luxforge and lie inside the target's range, with Lightroom's default on Luxforge's.
    #[test]
    fn every_calibration_is_a_monotone_map_of_a_value_transfer_row() {
        let registry = ModuleRegistry::builtin();
        for map in Calibration::all() {
            let row = lookup(&map.setting).expect("a recognised setting");
            let Rule::Transfer {
                action,
                field,
                lightroom,
            } = row.rule
            else {
                panic!("{} is not a value transfer", map.setting);
            };
            assert_eq!((action, field), (map.action.as_str(), map.field.as_str()));
            let first = map.knots.first().unwrap();
            let last = map.knots.last().unwrap();
            assert_eq!((first[0], last[0]), lightroom, "{}", map.setting);
            assert!(
                map.knots
                    .windows(2)
                    .all(|k| k[0][0] < k[1][0] && k[0][1] <= k[1][1])
            );
            let (_, descriptor) = registry.patch_action(action).unwrap();
            let parameter = descriptor.parameter(field).unwrap();
            for [_, luxforge] in &map.knots {
                crate::check_value(parameter, &serde_json::Value::from(*luxforge)).unwrap();
            }
            assert!(
                map.knots.iter().any(|[l, u]| *l == 0.0 && *u == 0.0) || action == "set-vignette"
            );
        }
    }

    /// Through the importer, each calibrated row converts exactly as its knots say, at every knot
    /// and halfway between two, and names its rule; a value at or past a shortfall is refused with
    /// its reason and reaches no settings.
    #[test]
    fn each_calibrated_row_converts_exactly_as_its_fixture_says() {
        let registry = ModuleRegistry::builtin();
        for map in Calibration::all() {
            let mut cases: Vec<(f64, f64)> = map.knots.iter().map(|[l, u]| (*l, *u)).collect();
            cases.extend(
                map.knots
                    .windows(2)
                    .map(|k| ((k[0][0] + k[1][0]) / 2.0, (k[0][1] + k[1][1]) / 2.0)),
            );
            for (lightroom, luxforge) in cases {
                let read =
                    inspect_preset(&preset(&map.setting, lightroom), None, &registry).unwrap();
                let shortfall = map.refused_at_or_below.is_some_and(|low| lightroom <= low)
                    || map
                        .refused_at_or_above
                        .is_some_and(|high| lightroom >= high);
                if shortfall {
                    let refused = &read.report.refused;
                    assert_eq!(refused.len(), 1, "{} {lightroom}", map.setting);
                    assert!(
                        refused[0]
                            .reason
                            .as_deref()
                            .unwrap()
                            .starts_with("beyond Luxforge's range")
                    );
                    assert!(
                        read.settings
                            .get(&map.action)
                            .and_then(|f| f.get(&map.field))
                            .is_none()
                    );
                    continue;
                }
                let applied = read.settings[&map.action][&map.field].as_f64().unwrap();
                assert!(
                    (applied - luxforge).abs() <= 1e-12 * luxforge.abs().max(1.0),
                    "{} {lightroom}: {applied} against {luxforge}",
                    map.setting
                );
                let entry = &read.report.mapped[0];
                assert_eq!(entry.rule.as_deref(), Some(rule().as_str()));
            }
        }
    }

    /// A row without a calibration still transfers its number unchanged, and its report entry
    /// names no rule, so a serialized report carries none.
    #[test]
    fn an_uncalibrated_row_transfers_its_number_and_names_no_rule() {
        let registry = ModuleRegistry::builtin();
        // Contrast's slider follows Lightroom's response, so its row is the identity and gone.
        for (setting, action, field) in [
            ("Texture", "set-presence", "texture"),
            ("PostCropVignetteMidpoint", "set-vignette", "midpoint"),
            ("Contrast2012", "set-basic", "contrast"),
        ] {
            assert!(calibration(setting).is_none());
            let read = inspect_preset(&preset(setting, 37.0), None, &registry).unwrap();
            assert_eq!(read.settings[action][field], serde_json::json!(37));
            assert_eq!(read.report.mapped[0].rule, None);
            let entry = serde_json::to_value(&read.report.mapped[0]).unwrap();
            assert!(entry.get("rule").is_none());
        }
    }
}
