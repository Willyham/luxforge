//! The colour range's side of the checklist. `mask/range.rs` keeps what is a range selection's
//! alone, the declared sample limit among it.

use super::luminance_range::value_based_ramps;
use super::*;
use luxforge_reference::range::{
    ColourRange as RefColourRange, colour_coverage, compile_colour_range,
};

pub(in crate::kinds) struct ColourRange;

pub(crate) fn colour_payload(range: &RefColourRange) -> Value {
    json!({ "samples": range.samples, "refine": range.refine })
}

/// Up to the declared five samples, drawn from a generous linear range so out-of-gamut and negative
/// components — which an earlier unit can legitimately produce — are part of the sweep rather than
/// excluded from it.
fn sample_colours(rng: &mut SplitMix64) -> RefColourRange {
    let count = rng.next_usize(6);
    RefColourRange {
        samples: (0..count)
            .map(|_| {
                [
                    rng.next_range(-0.2, 1.4),
                    rng.next_range(-0.2, 1.4),
                    rng.next_range(-0.2, 1.4),
                ]
            })
            .collect(),
        refine: rng.next_range(0.0, 100.0),
    }
}

impl Kind for ColourRange {
    const KIND: &'static str = "colour-range";
    const TITLE: &'static str = "Colour range";
    const ICON: &'static str = "colour";
    const MENU_TITLE: &'static str = "Colour range";
    const VALUE_BASED: bool = true;
    const SEEDS: Seeds = Seeds {
        sparse: 0x00C0_5A5E,
        dense: 0x00C0_DE5E,
        alone: 0x00C0_10E5,
        evaluate: 0x00C0_E7A1,
        bounds: 0x00C0_B0D5,
    };
    const NARROWER: usize = 0;
    const ALONE_ROUNDS: usize = 600;
    // 1707 at the seed.
    const ALONE_PARTIAL: usize = 200;
    // The unsampled component, which selects nothing until a colour is picked: 88 at the seed.
    const RARE: usize = 50;

    fn sample(rng: &mut SplitMix64, _: &[(u32, u32)], _: &mut StrokeTable) -> (Value, Falloff) {
        let range = sample_colours(rng);
        let oracle = compile_colour_range(&range);
        (
            colour_payload(&range),
            Box::new(move |_, _, _, rgb| colour_coverage(&oracle, rgb)),
        )
    }

    fn rare(payload: &Value) -> bool {
        payload["samples"] == json!([])
    }

    fn nothing(_: &mut StrokeTable) -> Vec<Value> {
        Vec::new()
    }

    fn inverted_whole(_: &mut StrokeTable) -> Option<(Value, (u32, u32))> {
        None
    }

    /// The synthetic stage's own middle pixel, with a refine low enough that a third of the stage
    /// sits on the falloff around it.
    fn feathered(_: &mut StrokeTable) -> Value {
        let source = byte_source();
        let offset = (((HEIGHT / 2) * WIDTH + WIDTH / 2) * 4) as usize;
        let picked: [f64; 3] = std::array::from_fn(|channel| {
            luxforge_reference::srgb::decode(source.rgba[offset + channel])
        });
        colour_payload(&RefColourRange {
            samples: vec![picked],
            refine: 10.0,
        })
    }

    fn refusals() -> Vec<(Value, ErrorKind, &'static str)> {
        vec![
            (
                json!({"samples": [], "refine": 120.0}),
                ErrorKind::Validation,
                "validation: component Colour range 1 colour-range parameter refine must be a \
                 number within 0..=100",
            ),
            (
                json!({"samples": [[0.0, 0.0, 99.0]], "refine": 50.0}),
                ErrorKind::Validation,
                "validation: component Colour range 1 colour-range sample parameter b must be a \
                 number within -16..=16",
            ),
        ]
    }

    fn legal() -> Vec<Value> {
        vec![
            json!({"samples": [], "refine": 0.0}),
            json!({"samples": vec![[-16.0, 16.0, 0.0]; 5], "refine": 100.0}),
        ]
    }

    fn ramps(strokes: &mut StrokeTable) -> Vec<Ramp> {
        value_based_ramps(Self::feathered(strokes))
    }
}

checklist!(ColourRange);
