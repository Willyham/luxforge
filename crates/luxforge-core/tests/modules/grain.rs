//! The `luxforge.grain` module end to end: real layers through the real host pipeline
//! (`ModuleRegistry`, `render`, `EditorService`).
//!
//! The descriptor, the Amount-0 neutrality rule, history label words, the compile's footprint and
//! the unit against every frozen fixture and the reduced-stage statistics are exercised in-crate,
//! next to the module, in `crates/luxforge-core/src/modules/grain/`. What grain shares with every
//! field-patch module — the field-patch rules, neutral payloads sharing the source, one layer per
//! target, an undeclared format refused, sample equal to render on both paths, drafts, history, an
//! unavailable provider and reopen — is proved once by the conformance suite (`field_patch`), and
//! its placement through the JSON method table by the Presence, mixer, vignette and grain chapter of
//! `editor-acceptance`. This module covers the rest of what is grain's own: placement after the
//! vignette whichever is committed first and through a crop, production against the reference
//! through `render`, a crop that re-lays the frame without changing the grain, and the refusals of
//! a stack grain cannot be part of.

use luxforge_core::{
    AssetId, CropPayload, EditorService, ErrorKind, GRAIN_EFFECT, Layer, ModuleRegistry, Mutation,
    SnapshotId, VIGNETTE_EFFECT,
};
use luxforge_reference::{grain as reference, srgb};
use luxforge_testbase::paths;
use luxforge_testkit::fixtures::{self, recipe, render, sample, source_of};
use serde_json::{Value, json};

/// A global grain layer holding `payload`.
fn layer(payload: Value) -> Layer {
    fixtures::layer(GRAIN_EFFECT, payload)
}

fn mutation(revision: u64, request: &str) -> Mutation {
    fixtures::mutation(revision, request, "grain-test")
}

fn effects_of(service: &EditorService, asset: &AssetId) -> Vec<String> {
    service
        .state(asset)
        .expect("state")
        .current_entry
        .snapshot
        .recipe
        .layers
        .into_iter()
        .map(|layer| layer.effect_id)
        .collect()
}

/// A layer that moves every field describes them in the declared order: amount, size, roughness.
#[test]
fn a_layer_describes_its_fields_in_the_declared_order() {
    let report = ModuleRegistry::builtin()
        .layer_report(&layer(
            json!({"roughness": 70.0, "size": 30.0, "amount": 40.0}),
        ))
        .expect("the layer is described");
    assert_eq!(
        report.summary,
        "Grain amount 40, Grain size 30, Grain roughness 70"
    );
    assert!(!report.neutral);
}

/// Grain follows the vignette whichever is committed first, and a crop committed afterwards joins
/// the geometry tail before both, through the editor service; the original's bytes are unchanged.
#[test]
fn grain_follows_the_vignette_whichever_is_committed_first() {
    let original = std::fs::read(paths::jpeg()).expect("the fixture");
    for (case, first, second) in [
        (
            "grain first",
            ("set-grain", json!({"amount": 40.0})),
            ("set-vignette", json!({"amount": -30.0})),
        ),
        (
            "vignette first",
            ("set-vignette", json!({"amount": -30.0})),
            ("set-grain", json!({"amount": 40.0})),
        ),
    ] {
        let path = paths::temp_catalog("grain-placement");
        let mut service = EditorService::open(&path).expect("a catalog");
        let asset = service.import(&paths::jpeg()).expect("an import").asset.id;
        for (revision, (action, parameters)) in [first, second].into_iter().enumerate() {
            service
                .apply_action(
                    &asset,
                    mutation(revision as u64, action),
                    action,
                    parameters,
                )
                .unwrap_or_else(|error| panic!("{case}: {action}: {error:?}"));
        }
        assert_eq!(
            effects_of(&service, &asset),
            [VIGNETTE_EFFECT, GRAIN_EFFECT],
            "{case}"
        );
        service
            .apply_action(
                &asset,
                mutation(2, "crop"),
                "crop",
                json!({"x": 0.1, "y": 0.1, "width": 0.5, "height": 0.5}),
            )
            .unwrap_or_else(|error| panic!("{case}: crop: {error:?}"));
        let effects = effects_of(&service, &asset);
        assert_eq!(
            effects[effects.len() - 2..],
            [VIGNETTE_EFFECT, GRAIN_EFFECT],
            "{case}: after the crop {effects:?}"
        );
        assert_eq!(effects[0], luxforge_core::CROP_EFFECT, "{case}");
        drop(service);
        let _ = std::fs::remove_file(&path);
    }
    assert_eq!(
        std::fs::read(paths::jpeg()).expect("the fixture"),
        original,
        "the original's bytes"
    );
}

/// Bands of flat colour through a real grain layer: every rendered byte within one code of the
/// reference's `grain_pixel` at that output pixel, over the same 8-bit input decoded, and every
/// sample equal to its rendered byte.
#[test]
fn production_matches_the_reference_through_the_real_render_path() {
    let registry = ModuleRegistry::builtin();
    let (width, height) = (64u32, 48u32);
    let bands: [[u8; 3]; 4] = [
        [128, 128, 128],
        [200, 60, 30],
        [20, 24, 18],
        [240, 236, 250],
    ];
    let pixels: Vec<[u8; 3]> = (0..width * height)
        .map(|index| bands[((index % width) * 4 / width) as usize])
        .collect();
    let source = source_of(width, height, &pixels);
    for (amount, size, roughness) in [(100.0, 25.0, 50.0), (60.0, 0.0, 0.0), (100.0, 100.0, 100.0)]
    {
        let stack = recipe(vec![layer(
            json!({"amount": amount, "size": size, "roughness": roughness}),
        )]);
        let rendered = render(&registry, &source, SnapshotId::new(), &stack).expect("a render");
        let params = reference::GrainParams {
            amount,
            size,
            roughness,
        };
        let mut moved = 0;
        for y in 0..height {
            for x in 0..width {
                let byte = pixels[(y * width + x) as usize];
                let input = byte.map(srgb::decode);
                let expected =
                    reference::grain_pixel(x, y, input, &params, reference::Footprint::FULL)
                        .map(srgb::code);
                let actual = rendered.pixel(x, y).expect("an in-bounds pixel");
                for channel in 0..3 {
                    assert!(
                        actual[channel].abs_diff(expected[channel]) <= 1,
                        "{amount}/{size}/{roughness} ({x}, {y}) channel {channel}: {} against {}",
                        actual[channel],
                        expected[channel]
                    );
                }
                moved += usize::from(actual[..3] != byte[..]);
                if (x + y) % 7 == 0 {
                    let sampled = sample(&registry, &source, &stack, x, y)
                        .expect("a sample")
                        .rgba
                        .expect("inside the stage");
                    assert_eq!(sampled, actual, "({x}, {y}) sample equals the render");
                }
            }
        }
        assert!(
            moved * 2 > (width * height) as usize,
            "the grain is visible"
        );
    }
}

/// A crop moves the frame over the photograph, not the grain over the frame: on a flat grey, two
/// crops of different origins and sizes render the same byte at every output pixel they share, the
/// full-resolution field at that output pixel. The grain's size is that of full-resolution output
/// pixels, so a crop update leaves it unchanged.
#[test]
fn a_crop_update_leaves_the_grain_unchanged_at_every_output_pixel() {
    let registry = ModuleRegistry::builtin();
    let (width, height) = (96u32, 64u32);
    let pixels = vec![[150u8, 150, 150]; (width * height) as usize];
    let source = source_of(width, height, &pixels);
    let grain = layer(json!({"amount": 100.0, "size": 60.0, "roughness": 80.0}));
    let cropped = |x: f64, y: f64, w: f64, h: f64| {
        let crop = Layer::crop(CropPayload {
            angle: 0.0,
            x,
            y,
            width: w,
            height: h,
        });
        render(
            &registry,
            &source,
            SnapshotId::new(),
            &recipe(vec![crop, grain.clone()]),
        )
        .expect("a render")
    };
    let first = cropped(0.0, 0.0, 0.5, 0.5);
    let second = cropped(0.25, 0.375, 0.75, 0.625);
    assert_eq!((first.width, first.height), (48, 32));
    assert_eq!((second.width, second.height), (72, 40));
    let params = reference::GrainParams {
        amount: 100.0,
        size: 60.0,
        roughness: 80.0,
    };
    for y in 0..first.height {
        for x in 0..first.width {
            let a = first.pixel(x, y).unwrap();
            assert_eq!(a, second.pixel(x, y).unwrap(), "({x}, {y})");
            let expected = reference::grain_pixel(
                x,
                y,
                [srgb::decode(150); 3],
                &params,
                reference::Footprint::FULL,
            );
            assert!(a[0].abs_diff(srgb::code(expected[0])) <= 1, "({x}, {y})");
        }
    }
}

/// The stacks grain cannot be part of are refused by name, by the render and the sample alike: a
/// grain layer before a geometry layer, two grain layers and a grain layer of another payload
/// format. The recipe is read, never rewritten.
#[test]
fn a_finish_layer_before_geometry_two_layers_and_another_format_are_refused() {
    let registry = ModuleRegistry::builtin();
    let source = source_of(8, 8, &[[100, 110, 120]; 64]);
    let grain = layer(json!({"amount": 50.0}));
    let crop = Layer::crop(CropPayload {
        angle: 0.0,
        x: 0.0,
        y: 0.0,
        width: 0.5,
        height: 0.5,
    });
    let other_format = Layer {
        effect_format: 2,
        ..layer(json!({"amount": 50.0}))
    };
    for (case, stack, kind, detail) in [
        (
            "finish before geometry",
            recipe(vec![grain.clone(), crop]),
            ErrorKind::Validation,
            "finish layer precedes geometry",
        ),
        (
            "two grain layers",
            recipe(vec![grain.clone(), layer(json!({"amount": 20.0}))]),
            ErrorKind::Validation,
            "ambiguous Grain layers",
        ),
        (
            "another payload format",
            recipe(vec![other_format]),
            ErrorKind::Incompatible,
            "",
        ),
    ] {
        let before = stack.clone();
        let rendered = render(&registry, &source, SnapshotId::new(), &stack).expect_err(case);
        assert_eq!(rendered.kind, kind, "{case}: {rendered}");
        assert!(
            rendered.detail.contains(detail),
            "{case}: {}",
            rendered.detail
        );
        let sampled = sample(&registry, &source, &stack, 1, 1).expect_err(case);
        assert_eq!(sampled.kind, kind, "{case}: sample");
        assert_eq!(stack, before, "{case}: nothing rewritten");
    }
}
