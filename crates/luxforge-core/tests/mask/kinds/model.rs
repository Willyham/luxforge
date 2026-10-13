//! The model selection's side of the checklist. Its payload references a stored alpha raster, so
//! every payload it draws holds its raster in the [`Store`] the suite compiles and renders with,
//! exactly as a recipe's bound artifact table holds it.

use super::*;
use luxforge_core::mask::ALPHA_KIND;
use luxforge_reference::mask::{Model as RefModel, model_coverage};

pub(super) struct Model;

/// An alpha raster as a prepared artifact of the selection's kind.
fn raster(width: u32, height: u32, codes: Vec<u8>) -> PreparedArtifact {
    PreparedArtifact::of_bytes(ALPHA_KIND, Some(width), Some(height), codes.into())
}

/// A payload over `artifact` with recorded provenance and the two shaping numbers.
fn payload(artifact: &ArtifactId, threshold: f64, feather: f64) -> Value {
    json!({
        "artifact": artifact.as_str(),
        "class": "sky",
        "model": "test-model",
        "model_version": "1",
        "input_fingerprint": "a".repeat(64),
        "prefix_hash": "b".repeat(64),
        "threshold": threshold,
        "feather": feather,
    })
}

/// `raster` held in `store`, its payload and its reference falloff.
fn held(
    store: &mut Store,
    width: u32,
    height: u32,
    codes: Vec<u8>,
    threshold: f64,
    feather: f64,
) -> (Value, Falloff) {
    let reference = RefModel {
        width,
        height,
        alpha: codes.clone().into(),
        threshold,
        feather,
    };
    let id = store.artifact(raster(width, height, codes));
    (
        payload(&id, threshold, feather),
        Box::new(move |stage, u, v, _| model_coverage(&reference, stage, u, v)),
    )
}

/// A raster whose codes rise along x from 0 to 255 and repeat down y.
fn ramp(width: u32, height: u32) -> Vec<u8> {
    (0..height)
        .flat_map(|_| (0..width).map(move |x| (x * 255 / (width - 1)) as u8))
        .collect()
}

/// The fixed rasters [`Model::refusals`] and [`Model::legal`] compile against: a 4 × 3 ramp, a
/// tint that is not an alpha raster, and a raster declaring more texels than it holds.
fn fixed() -> [PreparedArtifact; 3] {
    [
        raster(4, 3, ramp(4, 3)),
        PreparedArtifact::of_bytes("tint", None, None, vec![1u8; 12].into()),
        raster(4, 4, vec![9u8; 12]),
    ]
}

fn leak(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

impl Kind for Model {
    const KIND: &'static str = "model";
    const TITLE: &'static str = "Model";
    const ICON: &'static str = "model";
    const MENU_TITLE: &'static str = "Model selection";
    const VALUE_BASED: bool = false;
    const SEEDS: Seeds = Seeds {
        sparse: 0x30DE_1001,
        dense: 0x30DE_1002,
        alone: 0x30DE_1003,
        evaluate: 0x30DE_1004,
        bounds: 0x30DE_1005,
    };
    // 258 of 1200 at the seed: half the sampled masks are whole-mask inverted, half of every list's
    // components are inverted, and a raster whose non-zero codes reach its edges bounds to the stage.
    const NARROWER: usize = 220;
    const ALONE_ROUNDS: usize = 200;
    // 2933 at the seed.
    const ALONE_PARTIAL: usize = 2500;
    // The hard edge reached by a threshold of 100, whose band has no width at any feather: 13 at
    // the seed.
    const RARE: usize = 10;

    /// A raster of 1 to 24 texels a side whose non-zero codes fill a random rectangle of it — the
    /// whole raster one time in six — with saturated and zero codes among them, a threshold that
    /// lands on 1, 50 and 100 as well as between, and a feather that lands on 0 and 100.
    fn sample(rng: &mut SplitMix64, _: &[(u32, u32)], store: &mut Store) -> (Value, Falloff) {
        let width = 1 + rng.next_usize(24) as u32;
        let height = 1 + rng.next_usize(24) as u32;
        let whole = rng.next_usize(6) == 0;
        let span = |rng: &mut SplitMix64, side: u32| {
            if whole {
                (0, side - 1)
            } else {
                let a = rng.next_usize(side as usize) as u32;
                let b = rng.next_usize(side as usize) as u32;
                (a.min(b), a.max(b))
            }
        };
        let (x0, x1) = span(rng, width);
        let (y0, y1) = span(rng, height);
        let mut codes = vec![0u8; (width * height) as usize];
        for y in y0..=y1 {
            for x in x0..=x1 {
                codes[(y * width + x) as usize] = match rng.next_usize(8) {
                    0 | 1 => 255,
                    2 => 0,
                    _ => rng.next_usize(256) as u8,
                };
            }
        }
        let threshold = match rng.next_usize(10) {
            0 => 1.0,
            1 => 50.0,
            2 => 100.0,
            _ => rng.next_range(1.0, 100.0),
        };
        let feather = match rng.next_usize(8) {
            0 => 0.0,
            1 => 100.0,
            _ => rng.next_range(0.0, 100.0),
        };
        held(store, width, height, codes, threshold, feather)
    }

    fn rare(payload: &Value) -> bool {
        payload["threshold"] == json!(100.0) && payload["feather"] != json!(0.0)
    }

    /// A raster of zero codes selects nothing, at any threshold and feather.
    fn nothing(store: &mut Store) -> Vec<Value> {
        vec![
            held(store, 8, 6, vec![0; 48], 50.0, 100.0).0,
            held(store, 3, 5, vec![0; 15], 1.0, 0.0).0,
        ]
    }

    /// A block of full codes in the middle of an otherwise zero raster: its inversion is exactly
    /// one at the corners and exactly zero inside the block.
    fn inverted_whole(store: &mut Store) -> Option<(Value, (u32, u32))> {
        let mut codes = vec![0u8; 8 * 6];
        for y in 1..=4 {
            for x in 2..=5 {
                codes[y * 8 + x] = 255;
            }
        }
        Some((held(store, 8, 6, codes, 50.0, 100.0).0, (20, 15)))
    }

    /// A ramp across the frame at the defaults, whose coverage is the eased alpha: nearly every
    /// pixel is partially selected.
    fn feathered(store: &mut Store) -> Value {
        held(store, 6, 4, ramp(6, 4), 50.0, 100.0).0
    }

    fn bound() -> Store {
        let mut store = Store::bare();
        for artifact in fixed() {
            store.artifact(artifact);
        }
        store
    }

    /// Each declared number out of its range, a fingerprint that is not a digest, a missing field,
    /// an artifact the recipe was not bound with, one that is not an alpha raster and one shorter
    /// than its dimensions.
    fn refusals() -> Vec<(Value, ErrorKind, &'static str)> {
        let [ramp, tint, short] = fixed().map(|artifact| artifact.id);
        let with = |field: &str, value: Value| {
            let mut payload = payload(&ramp, 50.0, 100.0);
            payload[field] = value;
            payload
        };
        let unbound = ArtifactId::parse(format!("artifact-{}", "0".repeat(64))).unwrap();
        let mut missing = payload(&ramp, 50.0, 100.0);
        missing.as_object_mut().unwrap().remove("threshold");
        vec![
            (
                with("threshold", json!(0.5)),
                ErrorKind::Validation,
                "validation: component Model 1 model parameter threshold must be a number within \
                 1..=100",
            ),
            (
                with("feather", json!(100.5)),
                ErrorKind::Validation,
                "validation: component Model 1 model parameter feather must be a number within \
                 0..=100",
            ),
            (
                with("input_fingerprint", json!("abc")),
                ErrorKind::Validation,
                "validation: component Model 1 model parameter input_fingerprint must be a \
                 lowercase hex SHA-256",
            ),
            (
                missing,
                ErrorKind::Validation,
                "validation: component Model 1 has an invalid model payload: missing field \
                 `threshold`",
            ),
            (
                payload(&unbound, 50.0, 100.0),
                ErrorKind::SourceUnavailable,
                leak(format!(
                    "source-unavailable: artifact {unbound} of component Model 1 of mask Mask 1 \
                     is not bound to this recipe"
                )),
            ),
            (
                payload(&tint, 50.0, 100.0),
                ErrorKind::Validation,
                leak(format!(
                    "validation: artifact {tint} of component Model 1 is a tint artifact, not a \
                     mask-alpha raster"
                )),
            ),
            (
                payload(&short, 50.0, 100.0),
                ErrorKind::Validation,
                leak(format!(
                    "validation: artifact {short} of component Model 1 holds 12 bytes for a 4x4 \
                     raster"
                )),
            ),
        ]
    }

    fn legal() -> Vec<Value> {
        let [ramp, ..] = fixed().map(|artifact| artifact.id);
        vec![
            payload(&ramp, 1.0, 0.0),
            payload(&ramp, 1.0, 100.0),
            payload(&ramp, 100.0, 0.0),
            payload(&ramp, 100.0, 100.0),
        ]
    }

    /// The smallest feature is one texel crossed by the band: a 4 × 4 raster over a 400 px stage
    /// has 100 px texels, so the eased default ramps over 100 px and a band of 0.3 over 30 px; a
    /// hard edge has none. Rows 0 and 1 zero and rows 2 and 3 full ramp between the centres of
    /// texel rows 1 and 2, 100 rows down any column.
    fn ramps(store: &mut Store) -> Vec<Ramp> {
        let mut step = vec![0u8; 16];
        step[8..].fill(255);
        let soft = held(store, 4, 4, step.clone(), 50.0, 100.0).0;
        let narrow = held(store, 4, 4, step.clone(), 30.0, 50.0).0;
        let hard = held(store, 4, 4, step, 30.0, 0.0).0;
        vec![
            Ramp {
                payload: soft.clone(),
                compiled: (400, 400),
                asked: (400, 400),
                expect: Expect::Exact(100.0),
                counted: Some((200, 98..=100)),
            },
            Ramp {
                payload: soft,
                compiled: (400, 400),
                asked: (100, 100),
                expect: Expect::Exact(25.0),
                counted: None,
            },
            Ramp {
                payload: narrow,
                compiled: (400, 400),
                asked: (400, 400),
                expect: Expect::Near(30.0, 1e-4),
                counted: Some((200, 28..=32)),
            },
            Ramp {
                payload: hard,
                compiled: (400, 400),
                asked: (400, 400),
                expect: Expect::Exact(0.0),
                counted: Some((200, 0..=0)),
            },
        ]
    }
}

checklist!(Model);
