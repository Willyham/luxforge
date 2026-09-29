//! The radial gradient's side of the checklist. `mask/radial.rs` keeps what is the radial's alone:
//! the rotated ellipse's box at every angle, and a non-finite field's two doors.

use super::*;
use luxforge_reference::mask::{Radial as RefRadial, radial_coverage};

pub(super) struct Radial;

fn payload(radial: &RefRadial) -> Value {
    json!({
        "x": radial.x,
        "y": radial.y,
        "radius_x": radial.radius_x,
        "radius_y": radial.radius_y,
        "angle": radial.angle,
        "feather": radial.feather,
    })
}

fn radial(x: f64, y: f64, radius_x: f64, radius_y: f64, angle: f64, feather: f64) -> Value {
    json!({"x": x, "y": y, "radius_x": radius_x, "radius_y": radius_y, "angle": angle,
           "feather": feather})
}

fn legal_with(field: &str, value: Value) -> Value {
    let mut payload = radial(0.5, 0.5, 0.3, 0.2, 12.0, 50.0);
    payload[field] = value;
    payload
}

impl Kind for Radial {
    const KIND: &'static str = "radial";
    const TITLE: &'static str = "Radial";
    const ICON: &'static str = "radial";
    const MENU_TITLE: &'static str = "Radial gradient";
    const VALUE_BASED: bool = false;
    const SEEDS: Seeds = Seeds {
        sparse: 0x4A5C_0012,
        dense: 0x4A5C_0013,
        alone: 0x4A5C_0018,
        evaluate: 0x4A5C_0017,
        bounds: 0x4A5C_0015,
    };
    // 238 of 1200 at the seed. Most of the rest legitimately bound to the whole stage: half the
    // sampled masks are whole-mask inverted, half of every list's components are inverted, and one
    // sampled radius in eight is between 2 and 12 units, which covers any of these stages outright.
    const NARROWER: usize = 200;
    const ALONE_ROUNDS: usize = 200;
    // 1685 at the seed: most of a random radial's frame is outside it or inside its core.
    const ALONE_PARTIAL: usize = 1500;
    // The hard edge reached by rounding rather than by an equality against zero: 13 at the seed.
    const RARE: usize = 10;

    /// A centre anywhere in the widened `[-1, 2]` range, both radii inside the study's legal distance
    /// range, any angle, and a feather that lands on each of the three cases the study names — the
    /// explicit `0`, the `100` that starts the ramp at the centre, and a feather so small that
    /// `1 - feather / 100` rounds to exactly `1.0`, which is the hard edge reached by rounding.
    fn sample(rng: &mut SplitMix64, _: &[(u32, u32)], _: &mut StrokeTable) -> (Value, Falloff) {
        let radius = |rng: &mut SplitMix64| match rng.next_usize(8) {
            0 => rng.next_range(1e-4, 1e-2),
            1 => rng.next_range(2.0, 12.0),
            _ => rng.next_range(0.02, 0.9),
        };
        let reference = RefRadial {
            x: rng.next_range(-1.0, 2.0),
            y: rng.next_range(-1.0, 2.0),
            radius_x: radius(rng),
            radius_y: radius(rng),
            angle: rng.next_range(-180.0, 180.0),
            feather: match rng.next_usize(10) {
                0 => 0.0,
                1 => 100.0,
                2 => 1e-16,
                _ => rng.next_range(0.0, 100.0),
            },
        };
        (
            payload(&reference),
            Box::new(move |stage, u, v, _| radial_coverage(&reference, stage, u, v)),
        )
    }

    fn rare(payload: &Value) -> bool {
        payload["feather"] == json!(1e-16)
    }

    /// An ellipse dragged off the canvas selects nothing.
    fn nothing(_: &mut StrokeTable) -> Vec<Value> {
        vec![radial(-0.9, -0.9, 0.1, 0.1, 30.0, 40.0)]
    }

    /// The inner ellipse is where the inversion is exactly zero.
    fn inverted_whole(_: &mut StrokeTable) -> Option<(Value, (u32, u32))> {
        Some((radial(0.5, 0.5, 0.2, 0.2, 0.0, 50.0), (20, 15)))
    }

    fn feathered(_: &mut StrokeTable) -> Value {
        radial(0.5, 0.5, 0.4, 0.3, 20.0, 100.0)
    }

    fn refusals() -> Vec<(Value, ErrorKind, &'static str)> {
        let range = |field: &str, value: f64, message| {
            (
                legal_with(field, json!(value)),
                ErrorKind::Validation,
                message,
            )
        };
        vec![
            range(
                "x",
                -1.5,
                "validation: component Radial 1 radial parameter x must be a number within -1..=2",
            ),
            range(
                "y",
                2.5,
                "validation: component Radial 1 radial parameter y must be a number within -1..=2",
            ),
            range(
                "radius_x",
                0.0,
                "validation: component Radial 1 radial parameter radius_x must be a number within \
                 0.0001..=64",
            ),
            range(
                "radius_y",
                5e-5,
                "validation: component Radial 1 radial parameter radius_y must be a number within \
                 0.0001..=64",
            ),
            range(
                "radius_x",
                64.5,
                "validation: component Radial 1 radial parameter radius_x must be a number within \
                 0.0001..=64",
            ),
            range(
                "radius_y",
                -0.3,
                "validation: component Radial 1 radial parameter radius_y must be a number within \
                 0.0001..=64",
            ),
            range(
                "angle",
                181.0,
                "validation: component Radial 1 radial parameter angle must be a number within \
                 -180..=180",
            ),
            range(
                "angle",
                -180.5,
                "validation: component Radial 1 radial parameter angle must be a number within \
                 -180..=180",
            ),
            range(
                "feather",
                100.5,
                "validation: component Radial 1 radial parameter feather must be a number within \
                 0..=100",
            ),
            range(
                "feather",
                -1.0,
                "validation: component Radial 1 radial parameter feather must be a number within \
                 0..=100",
            ),
            (
                json!({"x": 0.5, "y": 0.5, "radius_x": 0.3, "radius_y": 0.2, "angle": 12.0}),
                ErrorKind::Validation,
                "validation: component Radial 1 has an invalid radial payload: missing field \
                 `feather`",
            ),
            (
                json!({"x0": 0.0, "y0": 0.0, "x1": 1.0, "y1": 1.0}),
                ErrorKind::Validation,
                "validation: component Radial 1 has an invalid radial payload: unknown field `x0`, \
                 expected one of `x`, `y`, `radius_x`, `radius_y`, `angle`, `feather`",
            ),
        ]
    }

    /// Both ends of the distance range are legal, and so are both ends of the angle and the feather.
    fn legal() -> Vec<Value> {
        vec![
            legal_with("radius_x", json!(1e-4)),
            legal_with("radius_y", json!(64.0)),
            legal_with("angle", json!(-180.0)),
            legal_with("angle", json!(180.0)),
            legal_with("feather", json!(0.0)),
            legal_with("feather", json!(100.0)),
            legal_with("x", json!(-1.0)),
            legal_with("y", json!(2.0)),
        ]
    }

    fn ramps(_: &mut StrokeTable) -> Vec<Ramp> {
        // span = 0.5 and the narrower radius is 0.2, so the ramp is 0.1 units — 400 px at 4000 of
        // height, 24 at 240, and below two pixels at 16.
        let ellipse = radial(0.5, 0.5, 0.4, 0.2, 0.0, 50.0);
        let at = |compiled, asked, payload: Value, expect| Ramp {
            payload,
            compiled,
            asked,
            expect,
            counted: None,
        };
        let mut ramps = vec![
            at(
                (6000, 4000),
                (6000, 4000),
                ellipse.clone(),
                Expect::Exact(400.0),
            ),
            at(
                (6000, 4000),
                (360, 240),
                ellipse.clone(),
                Expect::Exact(24.0),
            ),
            at((6000, 4000), (24, 16), ellipse.clone(), Expect::Below(2.0)),
            // Counted down the narrow axis of a square stage: 0.1 units is 100 px, on each side of
            // the centre.
            Ramp {
                counted: Some((500, 198..=202)),
                ..at((1000, 1000), (1000, 1000), ellipse, Expect::Positive)
            },
            // A feather too narrow to sample is still the smooth branch, and reports the width it
            // has.
            at(
                (1200, 900),
                (1200, 900),
                radial(0.5, 0.5, 0.3, 0.2, 20.0, 0.01),
                Expect::Positive,
            ),
        ];
        // A hard edge has no ramp for a pixel grid to resolve, and says so, however it is reached.
        for feather in [0.0, 1e-16, 1e-18] {
            ramps.push(at(
                (120, 90),
                (120, 90),
                radial(0.5, 0.5, 0.3, 0.2, 20.0, feather),
                Expect::Exact(0.0),
            ));
        }
        ramps
    }
}

checklist!(Radial);
