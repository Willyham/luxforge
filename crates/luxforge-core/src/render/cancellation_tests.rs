//! Cooperative cancellation of a render.

use super::testing::{frame_in, linear};
use super::tests::*;
use crate::{
    Cancel, EFFECT_FORMAT, ErrorKind, Layer, LayerId, LinearSettings, Recipe, RenderContext,
    RenderOptions, RenderSource, SnapshotId, SourceImage, Transform,
    modules::{HELD_EFFECT, HeldModule, ModuleRegistry},
    render::{frame_writes, parallel},
};
use luxforge_testbase::Gate;
use serde_json::json;
use std::sync::Arc;

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

/// On both pixel domains, a token cancelled before the render refuses it before any pass is
/// handed a frame.
#[test]
fn a_pre_cancelled_token_stops_a_render_before_it_allocates_a_frame() {
    let registry = ModuleRegistry::builtin();
    let (width, height) = (64, 48);
    let source = cancellation_source(width, height);
    let planes = varied(width, height);
    let recipe = cancellation_stack(width, height);
    let cancel = Cancel::new();
    cancel.cancel();
    assert!(cancel.is_cancelled());
    for (domain, input) in [
        ("byte", RenderSource::Byte(&source)),
        ("linear", linear(&planes, LinearSettings::default())),
    ] {
        let (outcome, written) = frame_writes::record(|| {
            frame_in(
                &RenderContext::new(),
                &registry,
                input,
                SnapshotId::new(),
                &recipe,
                RenderOptions::exact(&cancel),
            )
        });
        let error = outcome.expect_err("a cancelled token refuses the render");
        assert_eq!(error.kind, ErrorKind::Cancelled, "{domain}");
        assert_eq!(error.kind.code(), "cancelled", "{domain}");
        assert!(written.is_empty(), "{domain}: no pass was handed a frame");
    }
}

/// A colour pass cancelled while a chunk holds its scratch yields the cancelled kind, no frame,
/// and no reservation left behind, on both pixel domains, serially and on the pool. The held
/// layer stops the render in
/// its colour pass, inside a chunk that has reserved its scratch, so where the cancel lands is
/// decided by the gate and not by how fast this host renders. The turned stage is 150 colour
/// chunks tall and the cropped output over a hundred (the linear domain reads the colour through
/// the resample's rows), far more than the pool runs at once, so a chunk not yet started sees the
/// cancel on both paths. The photo-sized stop latency is the release timing in
/// `tests/cancellation.rs`.
#[test]
fn a_colour_pass_cancelled_mid_chunk_yields_no_frame_and_releases_every_reservation() {
    let (width, height) = (2400, 64);
    let source = cancellation_source(width, height);
    let planes = varied(width, height);
    let mut recipe = cancellation_stack(width, height);
    recipe.layers.insert(
        2,
        Layer {
            id: LayerId::new(),
            effect_id: HELD_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({}),
            mask: None,
            artifacts: Vec::new(),
        },
    );
    for (domain, input, pooled) in [
        RenderSource::Byte(&source),
        linear(&planes, LinearSettings::default()),
    ]
    .into_iter()
    .zip(["byte", "linear"])
    .flat_map(|(input, domain)| [false, true].map(|pooled| (domain, input, pooled)))
    {
        let gate = Arc::new(Gate::new());
        let mut registry = ModuleRegistry::builtin();
        registry
            .register(HeldModule::shared(gate.clone()))
            .expect("a valid holding module");
        let context = RenderContext::new();
        let budget = context.scratch();
        let cancel = Cancel::new();
        gate.shut();
        let outcome = std::thread::scope(|scope| {
            let render = scope.spawn(|| {
                parallel::force(Some(pooled));
                frame_in(
                    &context,
                    &registry,
                    input,
                    SnapshotId::new(),
                    &recipe,
                    RenderOptions::exact(&cancel),
                )
            });
            gate.wait_reached(1, "the colour pass to reach the held layer");
            assert_ne!(
                budget.in_use(),
                0,
                "{domain}, pooled {pooled}: the held chunk holds its scratch"
            );
            cancel.cancel();
            gate.open();
            render.join().expect("the render thread does not panic")
        });
        let error = outcome.expect_err("a cancelled render yields no frame, partial or otherwise");
        assert_eq!(
            error.kind,
            ErrorKind::Cancelled,
            "{domain}, pooled {pooled}"
        );
        assert_eq!(
            budget.in_use(),
            0,
            "{domain}, pooled {pooled}: a cancelled colour pass releases every reservation"
        );
    }
}
