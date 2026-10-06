//! A GPU preview at a percentage zoom below 100% (`docs/design/gpu-preview.md`, "Below 100%"): the
//! desktop asks for it as it asks at Fit, at the job's display bounds, which below 100% are the
//! displayed size of the whole stage. So the draft's plan, its boundary, the committed stack's
//! resident plan and its warm list all address the proxy the CPU path draws at that zoom: Fit's
//! plan, at another size.
use super::{GpuAnswer, GpuPlan, GpuPreview, GpuView, plan_preview, plan_rest, plan_warm};
use crate::{
    AssetId, BASIC_EFFECT, Draft, DraftStamp, EntryId, Evaluation, HistoryEntry, Layer,
    ModuleRegistry, PreviewSource, ProxyBounds, ProxyPlan, RECIPE_FORMAT, Recipe, RenderContext,
    Snapshot, SnapshotId, SourceImage, modules::Stage, render::tests::fitted_crop,
};
use serde_json::{Value, json};
use std::sync::Arc;

const WIDTH: u32 = 600;
const HEIGHT: u32 = 400;

/// The percentages the tests draw at: the scenario's 50% and 33%, and a quarter.
const BELOW: [f64; 3] = [50.0, 33.0, 25.0];

/// A gradient JPEG source.
fn source() -> PreviewSource {
    let mut rgba = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            rgba.extend([
                (x * 251 / WIDTH) as u8,
                (y * 241 / HEIGHT) as u8,
                ((x * 7 + y * 3) % 256) as u8,
                255,
            ]);
        }
    }
    PreviewSource::Jpeg(SourceImage {
        width: WIDTH,
        height: HEIGHT,
        rgba: rgba.into(),
        fingerprint: "sha256:gpu-preview-below-100".into(),
        orientation: 1,
        capture: Default::default(),
    })
}

/// The bounds the desktop's job carries at `percent` of an output stage of `width` × `height`:
/// its displayed size in physical pixels, rounded and clamped as every job's bounds are.
fn displayed((width, height): (u32, u32), percent: f64) -> ProxyBounds {
    ProxyBounds {
        width: (f64::from(width) * percent / 100.0).round() as u32,
        height: (f64::from(height) * percent / 100.0).round() as u32,
    }
    .clamped()
}

fn recipe(layers: Vec<Layer>) -> Recipe {
    Recipe {
        format: RECIPE_FORMAT,
        layers,
        ..Recipe::default()
    }
}

fn basic(payload: Value) -> Layer {
    Layer::new(BASIC_EFFECT, payload)
}

/// The evaluation of `drafted` over an entry holding `entry`, as a job of `draft` sees it, or of
/// the committed stack itself without one.
fn evaluation(entry: Vec<Layer>, drafted: Vec<Layer>, draft: Option<&Draft>) -> Evaluation {
    let asset = AssetId::new();
    let entry = HistoryEntry {
        id: EntryId::new(),
        asset_id: asset.clone(),
        sequence: 1,
        action_id: "set-basic".into(),
        label: "Basic".into(),
        parameters: json!({}),
        actor: "test".into(),
        timestamp_ms: 0,
        request_id: None,
        base_revision: 0,
        result_revision: 1,
        snapshot: Snapshot {
            id: SnapshotId::new(),
            asset_id: asset,
            recipe: recipe(entry),
        },
        undo_parent: None,
        restore_target: None,
    };
    Evaluation::new(
        Arc::new(ModuleRegistry::builtin()),
        RenderContext::new(),
        source(),
        entry,
        recipe(drafted),
        draft.map(|draft| DraftStamp {
            draft_id: draft.draft_id.clone(),
            draft_revision: draft.draft_revision,
        }),
    )
}

/// A Basic drag's draft over `entry`, its stack `drafted`.
fn drag(entry: Vec<Layer>, drafted: Vec<Layer>) -> (Evaluation, Draft) {
    let mut draft = Draft::new("set-basic", AssetId::new(), 1);
    draft.draft_revision = 3;
    (evaluation(entry, drafted, Some(&draft)), draft)
}

fn planned(preview: &GpuPreview) -> &GpuPlan {
    match &preview.answer {
        GpuAnswer::Plan(plan) => plan,
        GpuAnswer::Fallback(reason) => panic!("expected a plan, got {reason}"),
    }
}

/// The reduced stage the GPU plans `evaluation`'s stack at, at `bounds` ([`crate::gpu_fit_plan`]):
/// the frame the view draws at that zoom.
fn reduced_plan(evaluation: &Evaluation, bounds: ProxyBounds) -> ProxyPlan {
    crate::gpu_fit_plan(
        evaluation.registry(),
        evaluation.recipe(),
        evaluation.source().dimensions(),
        bounds,
    )
    .expect("the stack compiles")
    .expect("a view below 100% has a reduced stage")
}

/// Below 100% a drag is planned exactly as at Fit, at the displayed size of the whole stage: the
/// plan addresses the reduced stage the GPU plans at those bounds, its output is the frame the view
/// draws, and its boundary is keyed by that proxy, holding no window of its
/// own and no region, at a magnification of one. Each percentage's key is its own, and the same
/// percentage gives the same key whatever else the view does.
#[test]
fn a_view_below_100_percent_is_planned_at_its_displayed_size_proxy() {
    let (evaluation, draft) = drag(Vec::new(), vec![basic(json!({"exposure": 0.3}))]);
    let mut keys = Vec::new();
    for percent in BELOW {
        let bounds = displayed((WIDTH, HEIGHT), percent);
        let preview = plan_preview(&evaluation, &draft, GpuView::Fit(bounds)).unwrap();
        let plan = planned(&preview);
        let request = preview.boundary.clone().expect("a boundary");
        let proxy = reduced_plan(&evaluation, bounds);
        assert_eq!(
            (proxy.width, proxy.height),
            (bounds.width, bounds.height),
            "{percent}%: the whole stage's displayed size"
        );
        assert_eq!(
            request.key.plan(),
            Some(proxy),
            "{percent}%: the reduced stage"
        );
        let stage = Stage {
            width: proxy.width,
            height: proxy.height,
        };
        assert_eq!(plan.boundary.stage, stage, "{percent}%: the proxy stage");
        let output = plan.geometry.output();
        assert_eq!(
            (output.width, output.height),
            (proxy.width, proxy.height),
            "{percent}%: the frame the view draws"
        );
        assert_eq!(request.key.region(), None, "{percent}%: a whole frame");
        assert_eq!(
            request.window, None,
            "{percent}%: the proxy names its window"
        );
        assert_eq!(request.magnification, 1.0, "{percent}%");
        let again = plan_preview(&evaluation, &draft, GpuView::Fit(bounds)).unwrap();
        assert_eq!(again.boundary, preview.boundary, "{percent}%: one key");
        keys.push(request.key);
    }
    assert!(
        keys[0] != keys[1] && keys[1] != keys[2] && keys[0] != keys[2],
        "every percentage holds its own proxy"
    );
}

/// Below 100% a cropped stack's proxy holds only the window the crop reads, as at Fit: the bounds
/// are the crop's output at that percentage, the plan addresses the whole proxy stage, and the
/// boundary's key names the window its texels hold.
#[test]
fn below_100_percent_a_crop_holds_the_window_its_proxy_reads() {
    let crop = Layer::crop(fitted_crop(WIDTH, HEIGHT, 4.0, [0.1, 0.12, 0.6, 0.55]));
    let (evaluation, draft) = drag(
        vec![crop.clone()],
        vec![basic(json!({"exposure": 0.3})), crop],
    );
    let output = {
        let stage = evaluation.compiled().expect("the stack compiles").stage();
        (stage.width, stage.height)
    };
    for percent in BELOW {
        let bounds = displayed(output, percent);
        let preview = plan_preview(&evaluation, &draft, GpuView::Fit(bounds)).unwrap();
        let plan = planned(&preview);
        let proxy = reduced_plan(&evaluation, bounds);
        let window = proxy.window.expect("the crop's window");
        assert!(
            window.width < proxy.width && window.height < proxy.height,
            "{percent}%: a window of the proxy stage"
        );
        assert_eq!(
            preview
                .boundary
                .as_ref()
                .and_then(|request| request.key.plan()),
            Some(proxy),
            "{percent}%"
        );
        assert_eq!(
            plan.boundary.stage,
            Stage {
                width: proxy.width,
                height: proxy.height
            },
            "{percent}%: the whole proxy stage is addressed"
        );
        assert!(
            plan.geometry.affine().is_some(),
            "{percent}%: the straightened crop is the tail"
        );
    }
}

/// A committed stack's job at a view below 100% carries the plan of the stack itself and the warm
/// list at the view's bounds: the resident boundary is the proxy's, the key a drag at the same zoom
/// asks for, so that drag draws from it at its first tick; and every warmed plan addresses that
/// proxy stage.
#[test]
fn the_resident_plan_and_warm_list_follow_a_view_below_100_percent() {
    let committed = vec![basic(json!({"exposure": 0.3}))];
    let stack = evaluation(committed.clone(), committed.clone(), None);
    let (dragged, draft) = drag(committed, vec![basic(json!({"exposure": 0.6}))]);
    for percent in BELOW {
        let bounds = displayed((WIDTH, HEIGHT), percent);
        let proxy = reduced_plan(&stack, bounds);
        let stage = Stage {
            width: proxy.width,
            height: proxy.height,
        };
        let resident = plan_rest(&stack, GpuView::Fit(bounds)).unwrap().view;
        assert_eq!(planned(&resident).boundary.stage, stage, "{percent}%");
        let key = resident.boundary.expect("the resident boundary").key;
        assert_eq!(key.plan(), Some(proxy), "{percent}%");
        let drag = plan_preview(&dragged, &draft, GpuView::Fit(bounds)).unwrap();
        assert_eq!(
            drag.boundary.map(|request| request.key),
            Some(key),
            "{percent}%: the drag starts from the resident boundary"
        );
        let warm = plan_warm(&stack, GpuView::Fit(bounds)).unwrap();
        assert!(!warm.is_empty(), "{percent}%: drags to warm");
        assert!(
            warm.iter().all(|plan| plan.boundary.stage == stage),
            "{percent}%: every warmed plan addresses the proxy stage"
        );
    }
}

/// The identity of an affine tail: the output pixel is the boundary's own.
const IDENTITY: [f64; 6] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];

/// Every stack has a picture at rest on the GPU, planned from the source: the empty stack, whose
/// plan carries the source to the output untouched, and a stack of geometry alone, whose crop is
/// the plan's tail. The boundary is the first segment's input before its first operation, the
/// source itself, so Compare's Before and a photograph just opened draw on the GPU as any stack
/// does.
#[test]
fn an_empty_stack_is_planned_from_the_source() {
    let crop = Layer::crop(fitted_crop(WIDTH, HEIGHT, 4.0, [0.1, 0.12, 0.6, 0.55]));
    for (name, layers) in [("empty", Vec::new()), ("geometry alone", vec![crop])] {
        let stack = evaluation(layers.clone(), layers, None);
        for view in [
            GpuView::Fit(ProxyBounds {
                width: 160,
                height: 120,
            }),
            GpuView::Region {
                rect: crate::modules::Region {
                    x0: 20,
                    y0: 10,
                    width: 64,
                    height: 48,
                },
                magnification: 1.0,
            },
        ] {
            let rest = plan_rest(&stack, view).unwrap();
            let plan = planned(&rest.view);
            let request = rest.view.boundary.as_ref().expect("a boundary");
            assert_eq!(request.key.layer(), 0, "{name}, {view:?}: the source");
            assert_eq!(plan.boundary.layer, 0, "{name}, {view:?}");
            assert!(!plan.boundary.continues_run, "{name}, {view:?}");
            assert!(
                plan.content.is_empty() && plan.spatial.is_empty() && plan.output.is_empty(),
                "{name}, {view:?}: no operation over the source"
            );
            let tail = plan.geometry.affine();
            if name == "empty" {
                assert_eq!(
                    tail,
                    Some(IDENTITY),
                    "{name}, {view:?}: the source is the output"
                );
            } else {
                assert!(
                    tail.is_some_and(|map| map != IDENTITY),
                    "{name}, {view:?}: the crop is the tail"
                );
            }
        }
    }
}

/// The picture at rest and every drag over it are planned from the source, so they hold one
/// boundary key whatever the stack holds: the stack's own plan and a Basic drag over it start from
/// the same key at one view, and another stack over the same source at that view holds that key
/// too. Each plan holds every layer of its stack, its geometry in the tail.
#[test]
fn every_rest_plan_starts_at_the_source() {
    let view = GpuView::Fit(ProxyBounds {
        width: 160,
        height: 120,
    });
    let crop = Layer::crop(fitted_crop(WIDTH, HEIGHT, 4.0, [0.1, 0.12, 0.6, 0.55]));
    let committed = vec![basic(json!({"exposure": 0.3})), crop.clone()];
    let stack = evaluation(committed.clone(), committed.clone(), None);
    let rest = plan_rest(&stack, view).unwrap();
    let request = rest.view.boundary.as_ref().expect("a boundary");
    assert_eq!(
        request.key.layer(),
        0,
        "the source, reported under the first layer"
    );
    let plan = planned(&rest.view);
    assert_eq!(plan.content.len(), 1, "the Basic layer over the source");
    assert!(
        plan.geometry.affine().is_some_and(|map| map != IDENTITY),
        "the crop is the tail"
    );
    let (dragged, draft) = drag(
        committed,
        vec![basic(json!({"exposure": 0.8})), crop.clone()],
    );
    let ticked = plan_preview(&dragged, &draft, view).unwrap();
    let tick = ticked.boundary.as_ref().expect("a drag's boundary");
    assert_eq!(
        tick.key, request.key,
        "a drag starts from the rest boundary"
    );
    let other = evaluation(vec![crop.clone()], vec![crop], None);
    let other = plan_rest(&other, view).unwrap();
    assert_eq!(
        other.view.boundary.as_ref().map(|request| &request.key),
        Some(&request.key),
        "one key per source and view, whatever the stack holds"
    );
}

/// A picture at rest's tiles are planned within the rest's share, which a small photograph leaves
/// at its cap, so they take the longest side; tiles of a side a caller names have no share. Drawn
/// by slot shape: each shape's tiles together and row by row, the shapes in the order a row-by-row
/// walk meets them, every pixel of the output stage once.
#[test]
fn a_picture_at_rests_tiles_are_planned_within_its_share_by_shape() {
    let bounds = ProxyBounds {
        width: 160,
        height: 120,
    };
    let presence = Layer::new(
        crate::PRESENCE_EFFECT,
        json!({"texture": 30.0, "clarity": 25.0}),
    );
    let stack = evaluation(vec![presence.clone()], vec![presence], None);
    let rest = plan_rest(&stack, GpuView::Fit(bounds)).unwrap();
    let Some(Ok(tiles)) = rest.tiles else {
        panic!("tiles: {:?}", rest.tiles);
    };
    assert_eq!(tiles.share, Some(super::REST_SHARE_MAX));
    assert_eq!(tiles.tiles.len(), 1, "one tile of 2048 px");
    let tiles = super::plan_rest_tiles(&stack, bounds, super::RestSizing::Side(64))
        .unwrap()
        .expect("tiles at bounds smaller than the stage")
        .unwrap();
    assert_eq!(tiles.share, None);
    let shape = |tile: &super::RestTile| {
        (
            tile.window.width,
            tile.window.height,
            tile.rect.width,
            tile.rect.height,
        )
    };
    let mut rows = tiles.tiles.clone();
    rows.sort_by_key(|tile| (tile.rect.y0, tile.rect.x0));
    let mut met: Vec<_> = Vec::new();
    for tile in &rows {
        if !met.contains(&shape(tile)) {
            met.push(shape(tile));
        }
    }
    assert!(met.len() > 2, "edge tiles' windows are clamped: {met:?}");
    let expected: Vec<_> = met
        .iter()
        .flat_map(|held| rows.iter().filter(move |tile| shape(tile) == *held))
        .copied()
        .collect();
    assert_eq!(tiles.tiles, expected, "by shape, row by row within one");
    let area: u64 = tiles.tiles.iter().map(|tile| tile.rect.pixels()).sum();
    assert_eq!(area, u64::from(WIDTH * HEIGHT), "every pixel once");
}
