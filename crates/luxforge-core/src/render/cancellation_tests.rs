//! Cooperative cancellation of a render.

use super::testing::render_cancellable;
use super::tests::*;
use crate::{
    Cancel, EFFECT_FORMAT, ErrorKind, Layer, LayerId, Recipe, SnapshotId, SourceImage, Transform,
    modules::ModuleRegistry,
};
use serde_json::json;

/// A programmatically filled source, so a photo-sized case costs an allocation and a fill and
/// reads no file. The pattern varies on both axes and in all three channels, so a wrong row,
/// a dropped channel or a short frame is visible in the byte comparison.
fn cancellation_source(width: u32, height: u32) -> SourceImage {
    let mut rgba = vec![0_u8; width as usize * height as usize * 4];
    for (index, pixel) in rgba.chunks_exact_mut(4).enumerate() {
        pixel.copy_from_slice(&[index as u8, (index >> 5) as u8, (index >> 11) as u8, 255]);
    }
    SourceImage {
        width,
        height,
        rgba: rgba.into(),
        fingerprint: "sha256:cancellation".into(),
        orientation: 1,
        capture: Default::default(),
    }
}

/// The stack every cancellation test renders: the one orientation layer, one colour-stage
/// Basic layer and a 7° straightening crop, so the exact transform pass, the streamed colour
/// pass and the resample all run over the frame. The quarter turn means the crop's input stage
/// is the turned one, which is the stage it is fitted onto.
fn cancellation_stack(width: u32, height: u32) -> Recipe {
    Recipe {
        format: crate::RECIPE_FORMAT,
        layers: vec![
            turn(Transform::RotateRight),
            Layer {
                id: LayerId::new(),
                effect_id: crate::BASIC_EFFECT.into(),
                effect_format: EFFECT_FORMAT,
                payload: json!({"exposure": 0.5, "contrast": 20.0, "vibrance": 30.0}),
                mask: None,
                artifacts: Vec::new(),
            },
            Layer::crop(fitted_crop(height, width, 7.0, [0.05, 0.05, 0.9, 0.9])),
        ],
        masks: Vec::new(),
        ..Recipe::default()
    }
}

#[test]
fn a_pre_cancelled_token_stops_a_render_before_it_allocates_a_frame() {
    let registry = ModuleRegistry::builtin();
    let source = cancellation_source(512, 384);
    let recipe = cancellation_stack(512, 384);
    let cancel = Cancel::new();
    cancel.cancel();
    assert!(cancel.is_cancelled());
    let error = render_cancellable(&registry, &source, SnapshotId::new(), &recipe, &cancel)
        .expect_err("a cancelled token refuses the render");
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(error.kind.code(), "cancelled");
}

// The photo-sized latency case and the scratch-budget observation of a cancelled colour pass
// live in `tests/cancellation.rs`, whose 24 MP renders and latency bound would otherwise run
// beside every other unit test.
