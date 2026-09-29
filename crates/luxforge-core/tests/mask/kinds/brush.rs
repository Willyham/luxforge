//! The brush's side of the checklist. `mask/brush.rs` keeps what is the brush's alone: the shapes
//! the design names by hand, the accumulation properties, the grid index, the occupancy cap and the
//! stroke limits.

use super::*;
use luxforge_core::path::Stroke;
use luxforge_reference::mask::{Brush as RefBrush, brush_coverage};

pub(super) struct Brush;

/// A random stroke posted the way a client posts one, through the host's own capture, so every
/// sweep runs on strokes a gesture could actually have produced.
fn random_stroke(rng: &mut SplitMix64, erase: bool) -> Stroke {
    let count = 1 + rng.next_usize(8);
    let mut points = Vec::with_capacity(count);
    let mut x = rng.next_range(0.05, 0.95);
    let mut y = rng.next_range(0.05, 0.95);
    for _ in 0..count {
        points.push([x, y]);
        x = (x + rng.next_range(-0.25, 0.25)).clamp(-1.0, 2.0);
        y = (y + rng.next_range(-0.25, 0.25)).clamp(-1.0, 2.0);
    }
    Stroke::capture(
        &points,
        rng.next_range(0.01, 0.2),
        rng.next_range(0.0, 100.0),
        rng.next_range(1.0, 100.0),
        erase,
    )
    .expect("a legal stroke")
}

/// A brush payload over `strokes`, which go into the table the component resolves them through.
fn brushed(table: &mut StrokeTable, strokes: &[Stroke]) -> Value {
    let addresses: Vec<String> = strokes
        .iter()
        .map(|stroke| table.insert(stroke.clone()).to_string())
        .collect();
    json!({ "strokes": addresses })
}

fn line(size: f64, feather: f64, flow: f64, erase: bool) -> Stroke {
    Stroke::capture(&[[0.3, 0.5], [0.7, 0.5]], size, feather, flow, erase).expect("a legal stroke")
}

impl Kind for Brush {
    const KIND: &'static str = "brush";
    const TITLE: &'static str = "Brush";
    const ICON: &'static str = "brush";
    const MENU_TITLE: &'static str = "Brush";
    const VALUE_BASED: bool = false;
    const SEEDS: Seeds = Seeds {
        sparse: 0x018B_5A5E,
        dense: 0x018B_171D,
        alone: 0x018B_A10E,
        evaluate: 0x018B_E7A1,
        bounds: 0x018B_011D,
    };
    // 263 of 1200 at the seed: an inverted component or mask, or an erase-only component under an
    // inversion, legitimately bounds to the whole stage.
    const NARROWER: usize = 220;
    const ALONE_ROUNDS: usize = 200;
    // 3892 at the seed.
    const ALONE_PARTIAL: usize = 3500;

    /// One to four random strokes, each adding or erasing, the way the painted component holds them.
    fn sample(
        rng: &mut SplitMix64,
        _: &[(u32, u32)],
        strokes: &mut StrokeTable,
    ) -> (Value, Falloff) {
        let count = 1 + rng.next_usize(4);
        let held: Vec<Stroke> = (0..count)
            .map(|_| {
                let erase = rng.next_bool();
                random_stroke(rng, erase)
            })
            .collect();
        let reference = RefBrush {
            strokes: held.iter().map(reference_stroke).collect(),
        };
        (
            brushed(strokes, &held),
            Box::new(move |stage, u, v, rgb| brush_coverage(&reference, stage, u, v, rgb)),
        )
    }

    /// A component whose only stroke erases, one whose stroke has no flow, and one with no strokes
    /// at all cannot add coverage anywhere.
    fn nothing(strokes: &mut StrokeTable) -> Vec<Value> {
        vec![
            brushed(strokes, &[line(0.1, 50.0, 100.0, true)]),
            brushed(strokes, &[line(0.1, 50.0, 0.0, false)]),
            brushed(strokes, &[]),
        ]
    }

    /// A dab in the middle of the frame: its inversion is exactly one outside the capsule and
    /// exactly zero at its full-flow core.
    fn inverted_whole(strokes: &mut StrokeTable) -> Option<(Value, (u32, u32))> {
        let dab = Stroke::capture(&[[0.5, 0.5]], 0.1, 50.0, 100.0, false).expect("a legal stroke");
        Some((brushed(strokes, &[dab]), (20, 15)))
    }

    fn feathered(strokes: &mut StrokeTable) -> Value {
        let wide = Stroke::capture(&[[0.2, 0.3], [0.8, 0.7]], 0.3, 100.0, 100.0, false)
            .expect("a legal stroke");
        brushed(strokes, &[wide])
    }

    /// A payload that is not the reserved stroke list is refused by name, and a reference the store
    /// cannot answer is the store's own refusal — never an empty stroke.
    fn refusals() -> Vec<(Value, ErrorKind, &'static str)> {
        vec![
            (
                json!({"strokes": [], "size": 0.1}),
                ErrorKind::Validation,
                "validation: component Brush 1 has an invalid brush payload: unknown field `size`, \
                 expected `strokes`",
            ),
            (
                // A well-formed address no table holds.
                json!({"strokes": ["00000000000000000000000000000000"]}),
                ErrorKind::Incompatible,
                "incompatible: stroke 00000000000000000000000000000000 of this recipe is not in the \
                 stroke store referenced by component Brush 1 of mask Mask 1",
            ),
        ]
    }

    fn legal() -> Vec<Value> {
        vec![json!({"strokes": []})]
    }

    /// The smallest feature a brush draws is its narrowest ramp, in the stage's pixels: a radius of
    /// 0.1 at feather 50 has a ramp of half the radius, 20 px on a 400 px stage, up to the radius's
    /// own quantization onto the stored path grid, which is what `size()` reports. A hard-edged
    /// stroke has no ramp at all and says so, and a component with no strokes draws no feature.
    fn ramps(strokes: &mut StrokeTable) -> Vec<Ramp> {
        let soft = line(0.1, 50.0, 100.0, false);
        let expected = soft.size() * 0.5 * 400.0;
        assert!((expected - 20.0).abs() < 0.01, "{expected}");
        let at = |payload, compiled, expect| Ramp {
            payload,
            compiled,
            asked: compiled,
            expect,
            counted: None,
        };
        vec![
            at(
                brushed(strokes, &[soft]),
                (600, 400),
                Expect::Near(expected, 1e-3),
            ),
            at(
                brushed(strokes, &[line(0.1, 0.0, 100.0, false)]),
                (600, 400),
                Expect::Exact(0.0),
            ),
            at(brushed(strokes, &[]), (40, 30), Expect::Infinite),
        ]
    }
}

checklist!(Brush);
