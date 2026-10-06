use super::*;
use luxforge_core::{
    DETAIL_EFFECT, LinearSettings, MAX_MASKED_SPATIAL_LAYERS, ModuleRegistry, PRESENCE_EFFECT,
    SnapshotId,
    mask::{CompiledMask, Stroke},
    path::StrokeTable,
};
use luxforge_reference::{
    detail::{self, Image, Params},
    srgb,
};
use luxforge_testkit::{
    client::{self, Owner},
    fixtures,
};

fn detail_layer() -> Layer {
    fixtures::layer(
        DETAIL_EFFECT,
        json!({"luminance":40,"colour":40,"sharpening":50}),
    )
}

#[test]
fn detail_masks_blend_with_the_host_rule() {
    let registry = ModuleRegistry::builtin();
    let source = byte_source();
    let linear = decoded(&source);
    let input = source
        .rgba
        .chunks_exact(4)
        .map(|p| std::array::from_fn(|c| srgb::decode(p[c])))
        .collect::<Vec<_>>();
    let effect = detail::apply(
        &Image::new(WIDTH as usize, HEIGHT as usize, input.clone()),
        Params {
            luminance: 40.0,
            colour: 40.0,
            sharpening: 50.0,
            ..Params::default()
        },
        [1.0; 2],
    );
    let mut table = StrokeTable::new("Detail masks");
    let stroke = Stroke::capture(&[[0.2, 0.2], [0.8, 0.7]], 0.2, 50.0, 80.0, false).unwrap();
    let address = table.insert(stroke).to_string();
    for (kind, payload) in [
        ("linear", json!({"x0":0.2,"y0":0.3,"x1":0.7,"y1":0.8})),
        (
            "radial",
            json!({"x":0.5,"y":0.5,"radius_x":0.4,"radius_y":0.3,"angle":20,"feather":50}),
        ),
        ("brush", json!({"strokes":[address]})),
        (
            "luminance-range",
            json!({"low":20,"high":70,"low_feather":20,"high_feather":20}),
        ),
    ] {
        for amount in [0.0, 50.0, 100.0] {
            for invert in [false, true] {
                let mut mask = one_component(kind, payload.clone());
                mask.amount = amount;
                mask.invert = invert;
                let compiled = CompiledMask::new(&mask, stage(WIDTH, HEIGHT), &table).unwrap();
                let mut stack = recipe(vec![masked(detail_layer(), &mask)], vec![mask]);
                stack.strokes = table.clone();
                let frame =
                    fixtures::render(&registry, &source, SnapshotId::new(), &stack).unwrap();
                let raw = fixtures::render_linear(
                    &registry,
                    &linear,
                    SnapshotId::new(),
                    &stack,
                    LinearSettings::default(),
                )
                .unwrap();
                for y in 0..HEIGHT {
                    for x in 0..WIDTH {
                        let i = (y * WIDTH + x) as usize;
                        let m = compiled.coverage(x, y, input[i]);
                        let expected = std::array::from_fn::<_, 3, _>(|c| {
                            if m == 0.0 {
                                input[i][c]
                            } else {
                                (1.0 - m) * input[i][c] + m * effect.pixels[i][c]
                            }
                        });
                        for (c, value) in expected.iter().copied().enumerate() {
                            let label =
                                format!("Detail {kind} amount{amount} inverted{invert} ({x},{y})");
                            fixtures::assert_code_near_threshold(
                                frame.rgba[4 * i + c],
                                srgb::code(value),
                                value,
                                1e-5,
                                &label,
                            );
                            fixtures::assert_code_near_threshold(
                                raw.rgba[4 * i + c],
                                srgb::code(value),
                                value,
                                1e-5,
                                &label,
                            );
                        }
                        if m == 0.0 {
                            assert_eq!(
                                &frame.rgba[4 * i..4 * i + 4],
                                &source.rgba[4 * i..4 * i + 4]
                            );
                        }
                    }
                }
            }
        }
    }
    for (mask, expected) in [
        (
            gradient_mask(0.0, 1.5, 0.0, 2.0, 100.0).0,
            fixtures::recipe(vec![]),
        ),
        (
            gradient_mask(0.0, -1.0, 0.0, -0.5, 100.0).0,
            fixtures::recipe(vec![detail_layer()]),
        ),
    ] {
        let frame = fixtures::render(
            &registry,
            &source,
            SnapshotId::new(),
            &recipe(vec![masked(detail_layer(), &mask)], vec![mask]),
        )
        .unwrap();
        let unmasked = fixtures::render(&registry, &source, SnapshotId::new(), &expected).unwrap();
        assert_eq!(frame.rgba, unmasked.rgba);
    }
}

/// Masked Detail and masked Presence layers count together against the cap: as many masks as it
/// allows, each holding one or the other, render; one more masked Presence layer, on the first
/// mask beside its Detail, is refused by name with the stack kept.
#[test]
fn a_masked_detail_or_presence_layer_past_the_cap_is_refused() {
    let registry = ModuleRegistry::builtin();
    let source = byte_source();
    let masks = (0..MAX_MASKED_SPATIAL_LAYERS)
        .map(|_| gradient_mask(0.0, -1.0, 0.0, -0.5, 100.0).0)
        .collect::<Vec<_>>();
    let presence = || fixtures::layer(PRESENCE_EFFECT, json!({"texture":30}));
    let mut layers = masks
        .iter()
        .enumerate()
        .map(|(i, m)| {
            masked(
                if i % 2 == 0 {
                    detail_layer()
                } else {
                    presence()
                },
                m,
            )
        })
        .collect::<Vec<_>>();
    fixtures::render(
        &registry,
        &source,
        SnapshotId::new(),
        &recipe(layers.clone(), masks.clone()),
    )
    .unwrap();
    layers.push(masked(presence(), &masks[0]));
    let stack = recipe(layers, masks);
    let error = fixtures::render(&registry, &source, SnapshotId::new(), &stack).unwrap_err();
    assert_eq!(error.kind, luxforge_core::ErrorKind::ResourceLimit);
    assert!(
        error.detail.contains(&format!(
            "{} masked spatial layers",
            MAX_MASKED_SPATIAL_LAYERS + 1
        )) && error
            .detail
            .contains(&format!("{MAX_MASKED_SPATIAL_LAYERS} the host evaluates")),
        "{error}"
    );
    assert_eq!(stack.layers.len(), MAX_MASKED_SPATIAL_LAYERS + 1);
}

#[test]
fn detail_overlapping_masks_process_in_mask_list_order() {
    let directory = temp("detail-order");
    std::fs::create_dir_all(&directory).unwrap();
    let owner = Owner::start(
        &directory.join("catalog.sqlite"),
        ModuleRegistry::builtin(),
        "detail-mask",
    )
    .unwrap();
    let client = owner.client();
    let asset = owner.open(client, &paths::jpeg()).unwrap()["asset"]["id"].clone();
    let mutate = |method: &str, mut params: Value| {
        params["asset_id"] = asset.clone();
        params["mutation"] = client::mutation(
            owner.revision(client, &asset).unwrap(),
            &client::request_id(method),
            "detail-mask",
        );
        owner.call(client, method, params).unwrap()
    };
    mutate("edit.set-detail", json!({"luminance":20}));
    let first = mutate(
        "mask.create-radial",
        json!({"x":0.5,"y":0.5,"radius_x":0.4,"radius_y":0.4,"angle":0,"feather":30}),
    )["mask"]
        .clone();
    mutate(
        "edit.set-detail",
        json!({"mask":first,"colour":40,"luminance":40}),
    );
    let second = mutate(
        "mask.create-radial",
        json!({"x":0.55,"y":0.5,"radius_x":0.4,"radius_y":0.4,"angle":0,"feather":30}),
    )["mask"]
        .clone();
    mutate("edit.set-detail", json!({"mask":second,"sharpening":90}));
    let before = owner.recipe(client, &asset).unwrap();
    assert_eq!(
        before
            .layers
            .iter()
            .filter(|l| l.effect_id == DETAIL_EFFECT)
            .map(|l| l.mask.as_ref().map(|id| id.as_str().to_owned()))
            .collect::<Vec<_>>(),
        vec![
            None,
            Some(first.as_str().unwrap().to_owned()),
            Some(second.as_str().unwrap().to_owned())
        ]
    );
    mutate("mask.reorder", json!({"mask":second,"index":0}));
    let after = owner.recipe(client, &asset).unwrap();
    assert_eq!(
        after
            .layers
            .iter()
            .filter(|l| l.effect_id == DETAIL_EFFECT)
            .map(|l| l.mask.as_ref().map(|id| id.as_str().to_owned()))
            .collect::<Vec<_>>(),
        vec![
            None,
            Some(second.as_str().unwrap().to_owned()),
            Some(first.as_str().unwrap().to_owned())
        ]
    );
    owner.close().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}
