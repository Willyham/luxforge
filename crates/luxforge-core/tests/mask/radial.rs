//! What is the radial gradient's alone, beside the checklist every kind passes (`mask::kinds`):
//! the conservative rectangle of a rotated ellipse at every angle of a full turn, and the two doors a
//! non-finite field meets.

use super::*;
use luxforge_core::{
    Component, ComponentMode, Mask,
    mask::{CompiledMask, RadialGradient},
};
use serde_json::json;

fn payload(radial: RadialGradient) -> serde_json::Value {
    json!({
        "x": radial.x,
        "y": radial.y,
        "radius_x": radial.radius_x,
        "radius_y": radial.radius_y,
        "angle": radial.angle,
        "feather": radial.feather,
    })
}

/// One stored mask holding a single radial component as drawn, at full amount.
fn single(radial: RadialGradient) -> Mask {
    let mut mask = Mask::new("Mask 1");
    let name = mask.next_component_name("radial");
    mask.components.push(Component::new(
        name,
        ComponentMode::Add,
        "radial",
        payload(radial),
    ));
    mask
}

fn compile(radial: RadialGradient, width: u32, height: u32) -> CompiledMask {
    CompiledMask::new(
        &single(radial),
        stage(width, height),
        &luxforge_core::path::StrokeTable::default(),
    )
    .expect("a legal radial compiles")
}

// ---------------------------------------------------------------------------
// The bounds rectangle of a rotated ellipse.
// ---------------------------------------------------------------------------

/// The conservative rectangle's property (`mask::kinds`, `bounds_never_exclude_a_non_zero_pixel`)
/// swept over the rotation alone, at every degree of a full turn, on a single
/// add component with no inversion — so the rectangle is the ellipse's own box and the angle is the
/// only thing varying. Every rectangle here is strictly smaller than its stage, which is the whole
/// point of the inside-selected default.
#[test]
fn the_rotated_ellipse_box_is_conservative_at_every_angle() {
    let stages = [(40u32, 30u32), (30, 40), (36, 36)];
    let mut narrower = 0usize;
    let mut cases = 0usize;
    for degrees in -180..=180 {
        for feather in [0.0, 1e-16, 12.5, 50.0, 100.0] {
            let radial = RadialGradient {
                x: 0.5,
                y: 0.5,
                radius_x: 0.34,
                radius_y: 0.13,
                angle: f64::from(degrees),
                feather,
            };
            for (width, height) in stages {
                let compiled = compile(radial, width, height);
                let bounds = compiled.bounds();
                cases += 1;
                if bounds.pixels() < u64::from(width) * u64::from(height) {
                    narrower += 1;
                }
                for y in 0..height {
                    for x in 0..width {
                        let coverage = compiled.coverage(x, y, ANY_PIXEL);
                        if coverage != 0.0 {
                            assert!(
                                bounds.contains(x, y),
                                "{degrees} deg, feather {feather}, {width}x{height}: coverage \
                                 {coverage} at ({x}, {y}) lies outside {bounds:?}"
                            );
                        }
                    }
                }
            }
        }
    }
    println!("{narrower} of {cases} rotated-ellipse rectangles were smaller than their stage");
    assert_eq!(
        narrower, cases,
        "an inside-selected radial that fits inside the frame must never bound to the whole stage"
    );
}

// ---------------------------------------------------------------------------
// Validation.
// ---------------------------------------------------------------------------

fn refusal(payload: serde_json::Value) -> (luxforge_core::ErrorKind, String) {
    let mut mask = Mask::new("Mask 1");
    mask.components.push(Component::new(
        "Radial 1",
        ComponentMode::Add,
        "radial",
        payload,
    ));
    let error = CompiledMask::new(
        &mask,
        stage(400, 400),
        &luxforge_core::path::StrokeTable::default(),
    )
    .unwrap_err();
    (error.kind, error.to_string())
}

fn legal() -> serde_json::Value {
    json!({"x": 0.5, "y": 0.5, "radius_x": 0.3, "radius_y": 0.2, "angle": 12.0, "feather": 50.0})
}

fn with(field: &str, value: serde_json::Value) -> serde_json::Value {
    let mut payload = legal();
    payload[field] = value;
    payload
}

/// A non-finite field cannot be written as JSON at all — `serde_json` stores an infinity or a NaN as
/// `null` — so it is refused by the payload parse naming the field, and the `is_finite` guard behind
/// that is the second door rather than the first.
#[test]
fn a_non_finite_field_is_refused_by_name() {
    for field in ["x", "radius_x", "angle", "feather"] {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let (kind, message) = refusal(with(field, json!(value)));
            assert_eq!(kind, luxforge_core::ErrorKind::Validation);
            assert_eq!(
                message,
                format!(
                    "validation: component Radial 1 has an invalid radial payload: invalid type: \
                     null, expected f64"
                ),
                "{field} = {value}"
            );
        }
    }
    // The other route a non-finite field could take is a JSON literal too large for an `f64`. That
    // spelling never reaches the payload at all: the document itself fails to parse, so a stored
    // component could not have carried it.
    let text = r#"{"x": 0.5, "y": 0.5, "radius_x": 0.3, "radius_y": 0.2, "angle": 12.0,
                   "feather": 1e400}"#;
    assert!(
        serde_json::from_str::<serde_json::Value>(text).is_err(),
        "a JSON number outside f64 was accepted, so the parse is not the first door after all"
    );
    // And the `is_finite` guard behind both doors is live rather than decorative: it is what refuses
    // a value the payload parse would have accepted, which is every finite number out of range.
    let (kind, message) = refusal(with("radius_x", json!(1e300)));
    assert_eq!(kind, luxforge_core::ErrorKind::Validation);
    assert!(
        message.contains("radius_x must be a number within"),
        "{message}"
    );
}
