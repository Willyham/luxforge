//! The reference tile service: its reads against the reference renderer's own frames and samples,
//! and its worker's queue.

use super::{
    ReadAnswer, ReadStage, ReadValues, ReferenceReads, ReferenceTiles, TileCall, TileReads,
    TileService,
};
use crate::{
    AssetId, BASIC_EFFECT, Cancel, ClientId, Component, ComponentMode, CropPayload, DETAIL_EFFECT,
    EntryId, Error, Evaluation, HistoryEntry, Layer, LinearSettings, MIXER_EFFECT, Mask,
    ModuleRegistry, PRESENCE_EFFECT, PreviewSource, Recipe, Region, RenderContext, RenderOptions,
    RendererRecord, Snapshot, SnapshotId, VIGNETTE_EFFECT, WhiteBalanceApproximation,
    colour::srgb::quantize_pixel,
    render::tests::{assert_code_within_tolerance, gradient, varied},
};
use luxforge_testbase::{Gate, HANG};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{Receiver, channel, sync_channel},
};

/// Every pixel of a stage: a read of it answers the whole stage, clipped.
const EVERYTHING: Region = Region {
    x0: 0,
    y0: 0,
    width: u32::MAX,
    height: u32::MAX,
};

/// `recipe` over `source`, bound for evaluation as the catalog owner binds a saved entry's stack.
fn evaluation(
    registry: &Arc<ModuleRegistry>,
    context: &RenderContext,
    source: &PreviewSource,
    recipe: &Recipe,
) -> Evaluation {
    let asset_id = AssetId::new();
    let entry = HistoryEntry {
        id: EntryId::new(),
        asset_id: asset_id.clone(),
        sequence: 1,
        action_id: "test".into(),
        label: "Test".into(),
        parameters: json!({}),
        actor: "test".into(),
        timestamp_ms: 0,
        request_id: None,
        base_revision: 0,
        result_revision: 1,
        snapshot: Snapshot::original(asset_id),
        undo_parent: None,
        restore_target: None,
    };
    Evaluation::new(
        registry.clone(),
        context.clone(),
        source.clone(),
        entry,
        recipe.clone(),
        None,
    )
}

/// A JPEG and a developed RAW, exact and approximately white-balanced, `width` × `height`.
fn sources(width: u32, height: u32) -> Vec<(&'static str, PreviewSource)> {
    let balance = WhiteBalanceApproximation::from_matrix([
        [1.21, -0.11, -0.02],
        [-0.06, 1.08, -0.02],
        [0.01, -0.13, 1.12],
    ])
    .unwrap();
    vec![
        ("JPEG", PreviewSource::Jpeg(gradient(width, height))),
        (
            "RAW",
            PreviewSource::Raw {
                image: varied(width, height),
                settings: LinearSettings::default(),
            },
        ),
        (
            "RAW, white-balanced",
            PreviewSource::Raw {
                image: varied(width, height),
                settings: LinearSettings {
                    white_balance: Some(balance),
                },
            },
        ),
    ]
}

fn basic() -> Layer {
    Layer::new(
        BASIC_EFFECT,
        json!({"exposure": 0.4, "contrast": 20.0, "vibrance": 15.0}),
    )
}

fn presence() -> Layer {
    // Dehaze reads a global estimate of its whole input stage.
    Layer::new(
        PRESENCE_EFFECT,
        json!({"clarity": 40.0, "texture": 25.0, "dehaze": 20.0}),
    )
}

fn recipe_of(layers: Vec<Layer>, masks: Vec<Mask>) -> Recipe {
    Recipe {
        format: crate::RECIPE_FORMAT,
        layers,
        masks,
        ..Recipe::default()
    }
}

/// Stacks through two spatial segments with geometry and colour around them, through a colour run
/// right after a spatial layer and a masked colour layer after that, and through no spatial
/// segment at all.
fn stacks() -> Vec<(&'static str, Recipe)> {
    let mut mask = Mask::new("Mask 1");
    mask.components.push(Component::new(
        "Linear 1",
        ComponentMode::Add,
        "linear",
        json!({"x0": 0.2, "y0": 0.1, "x1": 0.8, "y1": 0.9}),
    ));
    let masked = Layer {
        mask: Some(mask.id.clone()),
        ..Layer::new(BASIC_EFFECT, json!({"exposure": -0.6, "saturation": 30.0}))
    };
    let crop = Layer::crop(CropPayload {
        angle: 4.0,
        x: 0.15,
        y: 0.1,
        width: 0.7,
        height: 0.75,
    });
    vec![
        (
            "Detail, Basic, Presence, a straightened crop and a vignette",
            recipe_of(
                vec![
                    Layer::new(
                        DETAIL_EFFECT,
                        json!({"sharpening": 40.0, "luminance": 25.0}),
                    ),
                    basic(),
                    presence(),
                    crop.clone(),
                    Layer::new(VIGNETTE_EFFECT, json!({"amount": -40.0, "midpoint": 30.0})),
                ],
                Vec::new(),
            ),
        ),
        (
            "Presence, Basic and a masked Basic",
            recipe_of(vec![presence(), basic(), masked], vec![mask]),
        ),
        (
            "Basic, the colour mixer and a straightened crop",
            recipe_of(
                vec![
                    basic(),
                    Layer::new(
                        MIXER_EFFECT,
                        json!({"red-hue": 20.0, "aqua-saturation": -35.0, "blue-luminance": 15.0}),
                    ),
                    crop,
                ],
                Vec::new(),
            ),
        ),
    ]
}

/// The stage before `layer` as the call's owner would ask for it: read as that layer receives it.
fn before(registry: &ModuleRegistry, recipe: &Recipe, layer: usize) -> ReadStage {
    ReadStage::Before {
        layer,
        mode: crate::render::MaskInputMode::for_layer(registry, recipe, layer).into(),
    }
}

fn codes(answer: &ReadAnswer) -> Vec<[u8; 4]> {
    let rect = answer.rect;
    (rect.y0..rect.y1())
        .flat_map(|y| (rect.x0..rect.x1()).map(move |x| (x, y)))
        .map(|(x, y)| answer.code(x, y).expect("a code inside the rectangle"))
        .collect()
}

/// Every pixel of every prefix of every stack, on a JPEG and on RAW planes, read through the
/// reference service as codes and as linear values: the codes are the frame the reference renders
/// of that prefix, byte for byte, and the very values the point path reads there today (codes, and
/// linear values rounded to `f32`); the codes are the quantizer's codes of the linear values, a
/// code apart only at a code's threshold; and the output stage's codes are the stack's own frame.
/// The stacks hold a Presence and a Detail layer, so spatial segments are materialized as frames.
#[test]
fn reference_reads_of_every_prefix_equal_the_prefix_frame() {
    let registry = Arc::new(ModuleRegistry::developer());
    for (domain, source) in sources(157, 101) {
        for (stack, recipe) in stacks() {
            let context = RenderContext::new();
            let evaluation = evaluation(&registry, &context, &source, &recipe);
            let cancel = Cancel::never();
            let mut session = ReferenceReads.session(&evaluation, &cancel);
            let (width, height) = source.dimensions();
            let full = registry
                .compile_layers(
                    width,
                    height,
                    &recipe.layers,
                    &recipe.masks,
                    &recipe.strokes,
                    &recipe.artifacts,
                )
                .unwrap();
            for layer in 0..=recipe.layers.len() {
                let what = format!("{domain}, {stack}, before layer {layer}");
                let stage = before(&registry, &recipe, layer);
                let read = session.read(stage, EVERYTHING, ReadValues::Codes).unwrap();
                let linear = session.read(stage, EVERYTHING, ReadValues::Linear).unwrap();
                assert_eq!(read.answered.record, RendererRecord::Reference, "{what}");
                assert_eq!(read.answered.reason, None, "{what}");
                assert_eq!(
                    read.rect,
                    Region::whole(read.stage),
                    "{what}: the whole stage"
                );
                assert_eq!(
                    (linear.stage, linear.rect),
                    (read.stage, read.rect),
                    "{what}"
                );

                // The frame the reference renders of the prefix, on a context of its own.
                let prefix = Recipe {
                    layers: recipe.layers[..layer].to_vec(),
                    ..recipe.clone()
                };
                let frame = crate::render(
                    &registry,
                    source.input(),
                    &prefix,
                    RenderOptions::default(),
                    &RenderContext::new(),
                )
                .unwrap()
                .frame(SnapshotId::new())
                .unwrap();
                assert_eq!(
                    (read.stage.width, read.stage.height),
                    (frame.width, frame.height),
                    "{what}: the prefix's stage"
                );

                // The host's own read of the prefix, as a plan reads it off the owner.
                let compiled = registry
                    .compile_layers(
                        width,
                        height,
                        &recipe.layers[..layer],
                        &recipe.masks,
                        &recipe.strokes,
                        &recipe.artifacts,
                    )
                    .unwrap();
                let wide = full.prefix_spatial_input_wide(&compiled);
                let point_context = RenderContext::new();
                let point = crate::render::prefix_pixels(
                    source.input(),
                    compiled,
                    &point_context,
                    &Cancel::never(),
                    wide,
                    crate::render::MaskInputMode::for_layer(&registry, &recipe, layer),
                )
                .unwrap();

                for y in 0..read.stage.height {
                    for x in 0..read.stage.width {
                        let at = format!("{what}, ({x}, {y})");
                        let code = read.code(x, y).unwrap();
                        let value = linear.linear(x, y).unwrap();
                        assert_eq!(Some(code), frame.pixel(x, y), "{at}: the prefix frame");
                        assert_eq!(
                            Some(code),
                            point.rgba(x, y).unwrap(),
                            "{at}: the point path"
                        );
                        assert_eq!(
                            Some(value),
                            point
                                .linear(x, y)
                                .unwrap()
                                .map(|value| value.map(|channel| channel as f32)),
                            "{at}: the point path's linear value"
                        );
                        let quantized = quantize_pixel(value);
                        for channel in 0..3 {
                            assert_code_within_tolerance(
                                code[channel],
                                quantized[channel],
                                f64::from(value[channel]),
                                &at,
                            );
                        }
                    }
                }
            }

            // The output stage is the stack's own frame, and its linear values quantize to it.
            let what = format!("{domain}, {stack}, the output");
            let read = session
                .read(ReadStage::Output, EVERYTHING, ReadValues::Codes)
                .unwrap();
            let linear = session
                .read(ReadStage::Output, EVERYTHING, ReadValues::Linear)
                .unwrap();
            let frame = crate::render(
                &registry,
                source.input(),
                &recipe,
                RenderOptions::default(),
                &RenderContext::new(),
            )
            .unwrap()
            .frame(SnapshotId::new())
            .unwrap();
            assert_eq!(
                (read.stage.width, read.stage.height),
                (frame.width, frame.height),
                "{what}"
            );
            assert!(
                codes(&read).concat() == frame.rgba.as_slice(),
                "{what}: the stack's frame"
            );
            for y in 0..read.stage.height {
                for x in 0..read.stage.width {
                    let (code, value) = (read.code(x, y).unwrap(), linear.linear(x, y).unwrap());
                    let quantized = quantize_pixel(value);
                    for channel in 0..3 {
                        assert_code_within_tolerance(
                            code[channel],
                            quantized[channel],
                            f64::from(value[channel]),
                            &format!("{what}, ({x}, {y})"),
                        );
                    }
                }
            }
        }
    }
}

/// A neutral pick's 25 points, read one at a time before Basic through one call on the reference
/// service, evaluate the stack before Basic once: its Presence frame is materialized once for the
/// whole call, on the service's own thread, never the caller's. Through a stack with no spatial
/// layer the points materialize nothing. The points are the patch one rectangle read answers.
#[test]
fn a_call_evaluates_one_prefix_for_its_25_points() {
    let registry = Arc::new(ModuleRegistry::developer());
    let source = PreviewSource::Jpeg(gradient(61, 43));
    let pointwise = recipe_of(
        vec![basic(), Layer::new(MIXER_EFFECT, json!({"red-hue": 20.0}))],
        Vec::new(),
    );
    for (recipe, frames) in [
        (recipe_of(vec![presence(), basic()], Vec::new()), 1),
        (pointwise, 0),
    ] {
        let context = RenderContext::new();
        let evaluation = evaluation(&registry, &context, &source, &recipe);
        let stage = before(&registry, &recipe, 1);
        let (cx, cy) = (30, 20);
        let service = ReferenceTiles::new();
        let (sender, answer) = sync_channel(1);
        let held = evaluation.clone();
        service.submit(TileCall::caller(
            ClientId::testing(1),
            Cancel::new(),
            move |reads, cancel| {
                let mut session = reads.session(&held, cancel);
                let mut points = Vec::new();
                for y in cy - 2..=cy + 2 {
                    for x in cx - 2..=cx + 2 {
                        let read = session.read(
                            stage,
                            Region {
                                x0: x,
                                y0: y,
                                width: 1,
                                height: 1,
                            },
                            ReadValues::Codes,
                        )?;
                        points.push(read.code(x, y).expect("the point read"));
                    }
                }
                Ok(json!({
                    "points": points,
                    "thread": std::thread::current().name(),
                }))
            },
            move |result| {
                let _ = sender.send(result);
            },
        ));
        let answered = answer.recv_timeout(HANG).unwrap().unwrap();
        assert_eq!(
            answered["thread"], "luxforge-tiles",
            "the service's thread reads the pixels"
        );
        assert_eq!(
            context.spatial_frames(),
            frames,
            "one evaluation of the prefix for all 25 points"
        );
        let patch = ReferenceReads
            .session(&evaluation, &Cancel::never())
            .read(
                stage,
                Region {
                    x0: cx - 2,
                    y0: cy - 2,
                    width: 5,
                    height: 5,
                },
                ReadValues::Codes,
            )
            .unwrap();
        let points: Vec<[u8; 4]> = serde_json::from_value(answered["points"].clone()).unwrap();
        assert_eq!(points, codes(&patch));
        service.stop();
    }
}

/// A rectangle read in frame mode is the samples it covers, byte for byte, through spatial
/// segments and a resample on both pixel domains: inside the stage, over the whole stage and at its
/// edge, both through the render's own read and through the reference service, which answers the
/// part of a rectangle the stage holds. A rectangle past the stage is not a render's to read.
#[test]
fn a_rect_read_equals_the_samples_it_covers() {
    let registry = Arc::new(ModuleRegistry::developer());
    for (domain, source) in sources(61, 43) {
        for (stack, recipe) in stacks() {
            let what = format!("{domain}, {stack}");
            let context = RenderContext::new();
            let render = crate::render(
                &registry,
                source.input(),
                &recipe,
                RenderOptions::default(),
                &context,
            )
            .unwrap();
            let sample = |x: u32, y: u32| render.sample(x, y).unwrap();
            let (width, height) = (sample(0, 0).width, sample(0, 0).height);
            let evaluation = evaluation(&registry, &context, &source, &recipe);
            let cancel = Cancel::never();
            let mut session = ReferenceReads.session(&evaluation, &cancel);
            for rect in [
                Region {
                    x0: 3,
                    y0: 5,
                    width: 17,
                    height: 13,
                },
                Region::whole(crate::Stage { width, height }),
                Region {
                    x0: width - 1,
                    y0: height - 1,
                    width: 1,
                    height: 1,
                },
            ] {
                let samples: Vec<[u8; 4]> = (rect.y0..rect.y1())
                    .flat_map(|y| (rect.x0..rect.x1()).map(move |x| (x, y)))
                    .map(|(x, y)| sample(x, y).rgba.unwrap())
                    .collect();
                assert_eq!(render.read_rect(rect).unwrap(), samples, "{what}, {rect:?}");
                let read = session
                    .read(ReadStage::Output, rect, ReadValues::Codes)
                    .unwrap();
                assert_eq!(read.rect, rect, "{what}, {rect:?}");
                assert_eq!(
                    codes(&read),
                    samples,
                    "{what}, {rect:?}: the service's read"
                );
            }
            // Past the stage's edge, the service answers the part the stage holds, and a render
            // refuses it.
            let edge = Region {
                x0: width - 2,
                y0: height - 3,
                width: 5,
                height: 5,
            };
            let read = session
                .read(ReadStage::Output, edge, ReadValues::Codes)
                .unwrap();
            assert_eq!(
                read.rect,
                Region {
                    x0: width - 2,
                    y0: height - 3,
                    width: 2,
                    height: 3,
                },
                "{what}"
            );
            for (x, y) in [(width - 2, height - 3), (width - 1, height - 1)] {
                assert_eq!(read.code(x, y), sample(x, y).rgba, "{what}, ({x}, {y})");
            }
            assert_eq!(
                read.code(width, height - 1),
                None,
                "{what}: outside the stage"
            );
            let outside = session
                .read(
                    ReadStage::Output,
                    Region {
                        x0: width + 3,
                        y0: 0,
                        width: 2,
                        height: 2,
                    },
                    ReadValues::Codes,
                )
                .unwrap();
            assert!(outside.rect.is_empty(), "{what}: nothing of the stage");
            assert_eq!(
                render.read_rect(edge).unwrap_err().kind.code(),
                "validation",
                "{what}"
            );
        }
    }
}

/// What a test call's caller receives: its request's identity, the sequence it was planned at and
/// its result.
type Answer = (String, u64, Result<Value, Error>);

/// A call from `client` answering `value`, whose caller receives `id`, a planned sequence of ten
/// times its client, and the result.
fn call(client: u64, id: &str, value: Value) -> (TileCall, Receiver<Answer>) {
    let (sender, answer) = sync_channel(1);
    let id = id.to_owned();
    (
        TileCall::caller(
            ClientId::testing(client),
            Cancel::new(),
            move |_, _| Ok(value),
            move |result| {
                let _ = sender.send((id, client * 10, result));
            },
        ),
        answer,
    )
}

/// A service whose worker is held at a shut gate before it answers each call.
fn held(capacity: usize) -> (ReferenceTiles, Arc<Gate>) {
    let service = ReferenceTiles::with_capacity(capacity);
    let gate = Arc::new(Gate::new());
    gate.shut();
    let held = gate.clone();
    service.hold(Some(Arc::new(move || held.pass())));
    (service, gate)
}

#[test]
fn calls_are_answered_in_order_with_their_identity_and_planned_sequence() {
    let service = ReferenceTiles::new();
    assert!(!service.started(), "no thread before the first call");
    let (sender, answers) = channel();
    for client in 1..=3_u64 {
        let sender = sender.clone();
        let id = format!("s{client}");
        service.submit(TileCall::caller(
            ClientId::testing(client),
            Cancel::new(),
            move |_, _| Ok(json!(client)),
            move |result| {
                let _ = sender.send((id, client * 10, result));
            },
        ));
    }
    assert!(service.started());
    for client in 1..=3_u64 {
        let (id, sequence, result) = answers.recv_timeout(HANG).unwrap();
        assert_eq!(id, format!("s{client}"), "answered in the order queued");
        assert_eq!(sequence, client * 10);
        assert_eq!(result.unwrap(), json!(client));
    }
    service.stop();
}

#[test]
fn a_full_queue_refuses_with_resource_limit_and_a_disconnect_drops_only_that_clients_calls() {
    let (service, gate) = held(2);
    // The first call is taken and held; two more fill the queue behind it.
    let (first, first_answer) = call(1, "running", json!(1));
    service.submit(first);
    gate.wait_reached(1, "the worker, with the first call");
    let (second, second_answer) = call(2, "queued-2", json!(2));
    let (third, third_answer) = call(3, "queued-3", json!(3));
    service.submit(second);
    service.submit(third);
    assert_eq!(service.waiting(), 2);
    let (fourth, fourth_answer) = call(4, "refused", json!(4));
    service.submit(fourth);
    let (id, sequence, refused) = fourth_answer.recv_timeout(HANG).unwrap();
    assert_eq!(refused.unwrap_err().kind.code(), "resource-limit");
    assert_eq!(
        (id.as_str(), sequence),
        ("refused", 40),
        "a refusal answers the call's own reply"
    );

    // Client 2 goes; its waiting call closes unanswered and client 3's stays.
    service.disconnect(ClientId::testing(2));
    assert_eq!(service.waiting(), 1);
    assert!(
        second_answer.recv_timeout(HANG).is_err(),
        "a disconnected client's call is dropped, not answered"
    );
    gate.open();
    assert_eq!(
        first_answer.recv_timeout(HANG).unwrap().2.unwrap(),
        json!(1)
    );
    assert_eq!(
        third_answer.recv_timeout(HANG).unwrap().2.unwrap(),
        json!(3)
    );
    service.stop();
}

#[test]
fn a_panicking_call_answers_internal_and_the_worker_answers_the_next() {
    let service = ReferenceTiles::new();
    let (sender, panicked) = sync_channel(1);
    service.submit(TileCall::caller(
        ClientId::testing(1),
        Cancel::new(),
        |_, _| -> Result<Value, Error> { panic!("a call panicked") },
        move |result| {
            let _ = sender.send(result);
        },
    ));
    assert_eq!(
        panicked
            .recv_timeout(HANG)
            .unwrap()
            .unwrap_err()
            .kind
            .code(),
        "internal"
    );
    let (next, answer) = call(1, "next", json!("fine"));
    service.submit(next);
    assert_eq!(answer.recv_timeout(HANG).unwrap().2.unwrap(), json!("fine"));
    service.stop();
}

#[test]
fn disconnect_cancels_the_active_call_before_its_next_pixel_read() {
    let (service, gate) = held(2);
    let executed = Arc::new(AtomicBool::new(false));
    let ran = executed.clone();
    let (sender, answer) = sync_channel(1);
    service.submit(TileCall::caller(
        ClientId::testing(1),
        Cancel::new(),
        move |_, _| {
            ran.store(true, Ordering::Relaxed);
            Ok(json!(1))
        },
        move |result| {
            let _ = sender.send(result);
        },
    ));
    gate.wait_reached(1, "the worker, with the call");
    service.disconnect(ClientId::testing(1));
    gate.open();
    assert_eq!(
        answer.recv_timeout(HANG).unwrap().unwrap_err().kind.code(),
        "cancelled"
    );
    assert!(!executed.load(Ordering::Relaxed));
    service.stop();
}
