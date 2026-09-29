//! The linear gradient's side of the checklist. Its own tests beside the kind table
//! (`luxforge-core`'s `mask::tests`) keep what is linear alone: the exact ends of its axis, the
//! half-plane rectangles and the axis length bounded against the stage.

use super::*;
use luxforge_reference::mask::{Linear as RefLinear, axis_is_legal, linear_coverage};

pub(super) struct Linear;

fn gradient(x0: f64, y0: f64, x1: f64, y1: f64) -> Value {
    json!({"x0": x0, "y0": y0, "x1": x1, "y1": y1})
}

impl Kind for Linear {
    const KIND: &'static str = "linear";
    const TITLE: &'static str = "Linear";
    const ICON: &'static str = "linear";
    const MENU_TITLE: &'static str = "Linear gradient";
    const VALUE_BASED: bool = false;
    const SEEDS: Seeds = Seeds {
        sparse: 0x4A5C_0004,
        dense: 0x4A5C_0005,
        alone: 0x4A5C_0008,
        evaluate: 0x4A5C_0006,
        bounds: 0x4A5C_0007,
    };
    // 147 of 1200 at the seed. Half the sampled masks are inverted, and a stored axis anywhere in
    // `[-1, 2]` often puts the whole frame in front of `p0`, so most cases legitimately bound to the
    // whole stage.
    const NARROWER: usize = 120;
    const ALONE_ROUNDS: usize = 200;
    // 20875 at the seed.
    const ALONE_PARTIAL: usize = 20_000;

    /// A stored position anywhere in the legal `[-1, 2]` range, so the widened range is swept rather
    /// than assumed, drawn again until its axis is legal on every stage it will be compiled on.
    fn sample(
        rng: &mut SplitMix64,
        stages: &[(u32, u32)],
        _: &mut StrokeTable,
    ) -> (Value, Falloff) {
        loop {
            let linear = RefLinear {
                x0: rng.next_range(-1.0, 2.0),
                y0: rng.next_range(-1.0, 2.0),
                x1: rng.next_range(-1.0, 2.0),
                y1: rng.next_range(-1.0, 2.0),
            };
            if stages
                .iter()
                .all(|&(width, height)| axis_is_legal(&linear, &ref_stage(width, height)))
            {
                return (
                    gradient(linear.x0, linear.y0, linear.x1, linear.y1),
                    Box::new(move |stage, u, v, _| linear_coverage(&linear, stage, u, v)),
                );
            }
        }
    }

    /// `p0` and `p1` both above the frame, pointing further up: every pixel is behind `p0`.
    fn nothing(_: &mut StrokeTable) -> Vec<Value> {
        vec![gradient(0.5, -0.4, 0.5, -0.9)]
    }

    /// An inverted gradient is the other half-plane, a rectangle of its own
    /// (`mask::tests::an_inverted_component_bounds_the_other_half_plane`).
    fn inverted_whole(_: &mut StrokeTable) -> Option<(Value, (u32, u32))> {
        None
    }

    fn feathered(_: &mut StrokeTable) -> Value {
        gradient(0.1, 0.1, 0.9, 0.9)
    }

    fn refusals() -> Vec<(Value, ErrorKind, &'static str)> {
        vec![
            (
                gradient(0.0, 0.0, 0.0, 0.0),
                ErrorKind::Validation,
                "validation: component Linear 1 linear axis length must be within 1e-4..=64 \
                 mask-space units on a 400x400 stage",
            ),
            (
                gradient(-1.5, 0.0, 0.5, 0.5),
                ErrorKind::Validation,
                "validation: component Linear 1 linear parameter x0 must be a number within -1..=2",
            ),
            (
                gradient(0.0, 2.5, 0.5, 0.5),
                ErrorKind::Validation,
                "validation: component Linear 1 linear parameter y0 must be a number within -1..=2",
            ),
            (
                json!({"x0": 0.0, "y0": 0.0, "x1": 1.0}),
                ErrorKind::Validation,
                "validation: component Linear 1 has an invalid linear payload: missing field `y1`",
            ),
            (
                json!({"x0": 0.0, "y0": 0.0, "x1": 1.0, "y1": 1.0, "angle": 4.0}),
                ErrorKind::Validation,
                "validation: component Linear 1 has an invalid linear payload: unknown field \
                 `angle`, expected one of `x0`, `y0`, `x1`, `y1`",
            ),
        ]
    }

    fn legal() -> Vec<Value> {
        vec![
            gradient(-1.0, 2.0, 2.0, -1.0),
            gradient(0.5, 0.5, 0.5, 0.501),
        ]
    }

    fn ramps(_: &mut StrokeTable) -> Vec<Ramp> {
        let short = gradient(0.5, 0.4, 0.5, 0.45);
        let at = |compiled, asked, payload: &Value, expect| Ramp {
            payload: payload.clone(),
            compiled,
            asked,
            expect,
            counted: None,
        };
        vec![
            // 0.05 mask-space units is 200 px at 4000 px of height, 12 px at 240, and below the two
            // pixels the proxy path treats as aliasing at 24.
            at((6000, 4000), (6000, 4000), &short, Expect::Exact(200.0)),
            at((6000, 4000), (360, 240), &short, Expect::Exact(12.0)),
            at((6000, 4000), (36, 24), &short, Expect::Below(2.0)),
            // Half the stage's height, counted down one column: the transition occupies the rows.
            Ramp {
                counted: Some((300, 199..=200)),
                ..at(
                    (600, 400),
                    (600, 400),
                    &gradient(0.5, 0.25, 0.5, 0.75),
                    Expect::Exact(200.0),
                )
            },
            // A diagonal axis measures its own length, not its projection, because one mask-space
            // unit is the stage's height on both axes: sqrt((0.5 · 1.5)² + 0.5²) · 400 px.
            at(
                (600, 400),
                (600, 400),
                &gradient(0.25, 0.25, 0.75, 0.75),
                Expect::Near(
                    ((0.5 * 1.5f64).powi(2) + 0.5f64.powi(2)).sqrt() * 400.0,
                    1e-3,
                ),
            ),
            at(
                (600, 400),
                (600, 400),
                &gradient(0.5, 0.5, 0.5, 0.55),
                Expect::Near(20.0, 1e-9),
            ),
        ]
    }
}

checklist!(Linear);
