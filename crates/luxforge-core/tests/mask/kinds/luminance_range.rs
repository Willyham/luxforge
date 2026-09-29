//! The luminance range's side of the checklist. `mask/range.rs` keeps what is a range selection's
//! alone: reading the masked operation's input on both paths, its composition with drawn kinds, and
//! its generated methods.

use super::*;
use luxforge_reference::range::{
    LuminanceRange as RefLuminanceRange, compile_luminance_range, luminance_coverage,
};

pub(in crate::kinds) struct LuminanceRange;

pub(crate) fn luminance_payload(band: &RefLuminanceRange) -> Value {
    json!({
        "low": band.low,
        "low_feather": band.low_feather,
        "high": band.high,
        "high_feather": band.high_feather,
    })
}

/// A randomized band inside every legality rule the study states: ordered edges, and a shoulder that
/// is either exactly zero or at least the floor.
pub(crate) fn sample_band(rng: &mut SplitMix64) -> RefLuminanceRange {
    let a = rng.next_range(0.0, 100.0);
    let b = rng.next_range(0.0, 100.0);
    let shoulder = |rng: &mut SplitMix64| {
        if rng.next_usize(5) == 0 {
            0.0
        } else {
            rng.next_range(1.0, 100.0)
        }
    };
    RefLuminanceRange {
        low: a.min(b),
        low_feather: shoulder(rng),
        high: a.max(b),
        high_feather: shoulder(rng),
    }
}

impl Kind for LuminanceRange {
    const KIND: &'static str = "luminance-range";
    const TITLE: &'static str = "Luminance range";
    const ICON: &'static str = "luminance";
    const MENU_TITLE: &'static str = "Luminance range";
    const VALUE_BASED: bool = true;
    const BAND: Option<&'static str> = Some("Range");
    const SEEDS: Seeds = Seeds {
        sparse: 0x0023_5A5E,
        dense: 0x0023_DE5E,
        alone: 0x0023_1A7E,
        evaluate: 0x0023_E7A1,
        bounds: 0x0023_B0D5,
    };
    const NARROWER: usize = 0;
    const ALONE_ROUNDS: usize = 600;
    // 9044 at the seed.
    const ALONE_PARTIAL: usize = 2000;

    fn sample(rng: &mut SplitMix64, _: &[(u32, u32)], _: &mut StrokeTable) -> (Value, Falloff) {
        let band = sample_band(rng);
        let oracle = compile_luminance_range(&band);
        (
            luminance_payload(&band),
            Box::new(move |_, _, _, rgb| luminance_coverage(&oracle, rgb)),
        )
    }

    fn nothing(_: &mut StrokeTable) -> Vec<Value> {
        Vec::new()
    }

    fn inverted_whole(_: &mut StrokeTable) -> Option<(Value, (u32, u32))> {
        None
    }

    /// A narrow band with wide shoulders, so nearly every pixel of the synthetic stage sits on a
    /// shoulder rather than at an endpoint.
    fn feathered(_: &mut StrokeTable) -> Value {
        luminance_payload(&RefLuminanceRange {
            low: 50.0,
            low_feather: 50.0,
            high: 53.0,
            high_feather: 50.0,
        })
    }

    /// The shoulder floor's gap is refused rather than rounded into the hard branch.
    fn refusals() -> Vec<(Value, ErrorKind, &'static str)> {
        vec![
            (
                json!({"low": 70.0, "low_feather": 0.0, "high": 30.0, "high_feather": 0.0}),
                ErrorKind::Validation,
                "validation: component Luminance range 1 luminance-range low must not be above \
                 high, and 30 is",
            ),
            (
                json!({"low": 10.0, "low_feather": 0.5, "high": 30.0, "high_feather": 0.0}),
                ErrorKind::Validation,
                "validation: component Luminance range 1 luminance-range low_feather must be \
                 exactly 0 for a hard edge or a number within 1..=100",
            ),
            (
                json!({"low": -1.0, "low_feather": 0.0, "high": 30.0, "high_feather": 0.0}),
                ErrorKind::Validation,
                "validation: component Luminance range 1 luminance-range parameter low must be a \
                 number within 0..=100",
            ),
            (
                json!({"low": 10.0, "high": 30.0, "high_feather": 0.0}),
                ErrorKind::Validation,
                "validation: component Luminance range 1 has an invalid luminance-range payload: \
                 missing field `low_feather`",
            ),
        ]
    }

    fn legal() -> Vec<Value> {
        vec![
            json!({"low": 0.0, "low_feather": 0.0, "high": 100.0, "high_feather": 100.0}),
            json!({"low": 40.0, "low_feather": 1.0, "high": 40.0, "high_feather": 1.0}),
        ]
    }

    /// A value test draws nothing a pixel grid can miss, so the thin-feature rule never fires for
    /// one — at any stage, and at the proxy size it is asked about.
    fn ramps(strokes: &mut StrokeTable) -> Vec<Ramp> {
        value_based_ramps(Self::feathered(strokes))
    }
}

/// The ramps of a value-based kind: an infinite smallest feature on a small, a middling and a
/// photo-sized stage, and at a quarter of each.
pub(in crate::kinds) fn value_based_ramps(payload: Value) -> Vec<Ramp> {
    [(37u32, 29u32), (640, 480), (6000, 4000)]
        .into_iter()
        .flat_map(|(width, height)| {
            let proxy = (width.div_ceil(4).max(1), height.div_ceil(4).max(1));
            [(width, height), proxy].map(|asked| Ramp {
                payload: payload.clone(),
                compiled: (width, height),
                asked,
                expect: Expect::Infinite,
                counted: None,
            })
        })
        .collect()
}

checklist!(LuminanceRange);
