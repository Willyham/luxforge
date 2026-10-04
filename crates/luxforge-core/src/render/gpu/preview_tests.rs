//! A GPU preview at a percentage zoom below 100% (`docs/design/gpu-preview.md`, "Below 100%"): the
//! desktop asks for it as it asks at Fit, at the job's display bounds, which below 100% are the
//! displayed size of the whole stage. So the draft's plan, its boundary, the committed stack's
//! resident plan and its warm list all address the proxy the CPU path draws at that zoom: Fit's
//! plan, at another size.
use super::{GpuAnswer, GpuPlan, GpuPreview, GpuView, plan_preview, plan_resident, plan_warm};
use crate::{
    AssetId, BASIC_EFFECT, Cancel, Draft, DraftStamp, EntryId, Evaluation, HistoryEntry, Layer,
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

/// The proxy the preview worker's proxy phase renders `evaluation`'s stack at, at `bounds`: what
/// the CPU path draws at that zoom.
fn worker_proxy(evaluation: &Evaluation, bounds: ProxyBounds) -> ProxyPlan {
    let exact = evaluation.exact(&Cancel::never()).expect("the exact stage");
    let plan = exact
        .proxy_plan(bounds)
        .expect("a view below 100% has a proxy");
    exact
        .proxy_window(evaluation.registry(), evaluation.recipe(), plan)
        .plan()
}

/// Below 100% a drag is planned exactly as at Fit, at the displayed size of the whole stage: the
/// plan addresses the proxy stage the CPU's proxy phase renders at those bounds, its output is the
/// proxy frame the view draws, and its boundary is keyed by that proxy, holding no window of its
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
        let proxy = worker_proxy(&evaluation, bounds);
        assert_eq!(
            (proxy.width, proxy.height),
            (bounds.width, bounds.height),
            "{percent}%: the whole stage's displayed size"
        );
        assert_eq!(
            request.key.plan(),
            Some(proxy),
            "{percent}%: the worker's proxy"
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
        let exact = evaluation.exact(&Cancel::never()).expect("the exact stage");
        exact.stage()
    };
    for percent in BELOW {
        let bounds = displayed(output, percent);
        let preview = plan_preview(&evaluation, &draft, GpuView::Fit(bounds)).unwrap();
        let plan = planned(&preview);
        let proxy = worker_proxy(&evaluation, bounds);
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
        let proxy = worker_proxy(&stack, bounds);
        let stage = Stage {
            width: proxy.width,
            height: proxy.height,
        };
        let resident = plan_resident(&stack, GpuView::Fit(bounds))
            .unwrap()
            .expect("a stack with a content layer");
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
