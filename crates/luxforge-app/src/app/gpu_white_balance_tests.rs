//! A RAW white-balance drag drawn on the GPU, held to its release on the RAW corpus
//! (`docs/design/gpu-preview.md`, "RAW white balance"): a measurement of the owner's `raw-panel`
//! gates on every RAW the private manifest names, the surface on this host's adapter drawing what
//! the editor draws.
//!
//! For each RAW, opened at As shot, and each temperature: the draft's plan, its change a leading
//! step over the planes developed As shot, drawn over its boundary derived from that source — the
//! moving frame — at Fit and over the largest view's region at 100%; then the same temperature
//! committed, which redevelops the mosaic, and the committed stack's picture at rest at the same
//! view drawn from the redeveloped source, and its view plan, the frame a drag over the redeveloped
//! source draws. The moving frame is held to the picture at rest within the pointwise limits, as
//! the gate holds it; beside it are the moving frame against the redeveloped view plan, the change
//! approximated alone, and the view plan against the picture at rest, what a drag's frame at Fit
//! is apart from the picture at rest it settles to whatever it changes.
use super::{
    gpu_plan::surface_plan_over,
    gpu_preview::{derived_now, gpu_source_of, rest_now},
    gpu_qualification::{Opened, corpus_sources, fit_bounds, headless, largest_view},
    tasks::ready_preview_job,
};
use luxforge_core::{Evaluation, GpuAnswer, GpuPreview, GpuView, PreviewRequest};
use luxforge_reference::preview_error::{self, Class, Rgb8, Statistics};
use luxforge_testkit::client::{call, mutation, request_id, revision};
use luxforge_ui::photo_surface::{GpuSource, gpu_preview::headless::HeadlessSurface};
use serde_json::{Value, json};

/// The temperatures the `raw-panel` scenario drags to, far from any camera's as-shot white balance.
const KELVINS: [f64; 2] = [3500.0, 2500.0];

fn rgb(codes: &[[u8; 4]]) -> Vec<u8> {
    codes
        .iter()
        .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
        .collect()
}

fn compare(candidate: &[u8], reference: &[u8], (width, height): (u32, u32)) -> Statistics {
    preview_error::compare(
        Rgb8::new(width, height, candidate).expect("a frame"),
        Rgb8::new(width, height, reference).expect("a frame"),
        [0, 0, width, height],
    )
    .expect("two frames of one size")
}

fn figures(statistics: &Statistics) -> Value {
    json!({
        "mean": statistics.mean, "worst_block": statistics.worst_block, "p99": statistics.p99,
        "mean_delta_l": statistics.mean_delta_l,
        "pointwise": preview_error::verdict(statistics, Class::Pointwise).passed(),
    })
}

/// `preview`'s plan drawn by `surface` over its boundary derived from `gpu`, and its size.
fn drawn(
    surface: &mut HeadlessSurface,
    gpu: &GpuSource,
    preview: &GpuPreview,
    version: u64,
) -> (Vec<u8>, (u32, u32)) {
    let (GpuAnswer::Plan(plan), Some(request)) = (&preview.answer, &preview.boundary) else {
        panic!("no plan: {:?}", preview.answer.fallback());
    };
    let (boundary, origin, grid) =
        derived_now(gpu, plan, request, version).expect("a boundary derived from the source");
    let converted = surface_plan_over(plan, boundary, origin, grid.as_ref(), request.key.region())
        .expect("a plan the surface runs");
    let frame = surface.draw(gpu, &converted).expect("the plan drawn");
    (rgb(&frame.codes), frame.size)
}

/// The committed stack of `evaluation` at `view`, drawn from `gpu`: its picture at rest — its tiles
/// reduced to the view at Fit, its view plan over the region at 100% — and its view plan.
fn at_rest(
    surface: &mut HeadlessSurface,
    gpu: &GpuSource,
    evaluation: &Evaluation,
    view: GpuView,
    version: u64,
) -> (Vec<u8>, Vec<u8>) {
    let rest = luxforge_core::qualification::rest_plan(evaluation, view).expect("a rest plan");
    let (planned, _) = drawn(surface, gpu, &rest.view, version);
    let picture = match (&rest.tiles, view) {
        (Some(Ok(tiles)), GpuView::Fit(_)) if tiles.reduction.is_some() => {
            let handed = rest_now(gpu, tiles, version).expect("the tiles");
            rgb(&surface.rest(gpu, &handed).expect("the tiles drawn").codes)
        }
        _ => planned.clone(),
    };
    (picture, planned)
}

/// Measurement, not a gate: on every RAW the manifest names, the moving GPU white-balance frame
/// against the release at Fit and at 100%, printed and written to `LUXFORGE_WB_OUTPUT` when set.
/// `LUXFORGE_RAW_MANIFEST=~/projects/lightwell/private/raw-manifest.json cargo test --release -p
/// luxforge-app gpu_white_balance_on_the_raw_corpus -- --ignored --nocapture`.
#[test]
#[ignore = "the corpus RAWs: set LUXFORGE_RAW_MANIFEST to the private RAW manifest"]
fn gpu_white_balance_on_the_raw_corpus() {
    let test = "gpu_white_balance_on_the_raw_corpus";
    let Some(qualifier) = headless(test) else {
        return;
    };
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(std::env::var("LUXFORGE_RAW_MANIFEST").expect("a manifest"))
            .unwrap(),
    )
    .unwrap();
    let corpus: Value = serde_json::from_str(
        &std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/preview/corpus.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let sources: Vec<_> = corpus_sources(
        &corpus,
        std::path::Path::new("/nonexistent"),
        Some(&manifest),
    )
    .into_iter()
    .filter(|source| source.raw)
    .collect();
    assert!(!sources.is_empty(), "no RAW in the manifest");
    let dir = luxforge_testbase::paths::temp_dir("gpu-white-balance");
    let mut measured = Vec::new();
    for source in &sources {
        for kelvin in KELVINS {
            for at in ["fit", "100%"] {
                let catalog = dir.join(format!(
                    "{}-{kelvin}-{}.sqlite",
                    source.id,
                    at.trim_end_matches('%')
                ));
                let opened = Opened::new(source, &[], &catalog).expect("the RAW opened");
                let (owner, client, asset) = (&opened.owner, opened.client, &opened.asset);
                let id = json!(asset.as_str());
                let committed =
                    ready_preview_job(owner, PreviewRequest::new(client, asset.clone()))
                        .expect("the As shot frame");
                let stage = match luxforge_core::qualification::rest_plan(
                    &committed.evaluation,
                    GpuView::Fit(fit_bounds()),
                )
                .expect("a rest plan")
                .tiles
                {
                    Some(Ok(tiles)) => tiles.output,
                    _ => panic!("{}: no tiles", source.id),
                };
                let view = match at {
                    "fit" => GpuView::Fit(fit_bounds()),
                    _ => GpuView::Region {
                        rect: largest_view((stage.width, stage.height), 100.0)
                            .expect("a visible region"),
                        magnification: 1.0,
                    },
                };
                let request = |request: PreviewRequest| match view {
                    GpuView::Fit(bounds) => request.proxy(bounds).gpu(),
                    GpuView::Region {
                        rect,
                        magnification,
                    } => request.gpu_region(rect, magnification),
                };
                // The drag: a draft of the temperature over the As shot development.
                let draft = call(
                    owner,
                    client,
                    "draft.begin",
                    json!({"asset_id": id, "action": "set-raw"}),
                )
                .expect("a draft");
                let draft_id = draft["draft_id"].as_str().expect("a draft id").to_owned();
                call(
                    owner,
                    client,
                    "draft.set",
                    json!({"draft_id": draft_id, "fields": {"temperature": kelvin}}),
                )
                .expect("the draft set");
                let job = ready_preview_job(
                    owner,
                    request(
                        PreviewRequest::new(client, asset.clone())
                            .draft(luxforge_core::DraftId::parse(draft_id.clone()).unwrap()),
                    ),
                )
                .expect("the draft's job");
                let preview = job.gpu.as_deref().expect("a GPU preview");
                let source_gpu = gpu_source_of(1, job.evaluation.source()).expect("a source");
                let mut surface = qualifier.surface();
                let (moving, size) = drawn(&mut surface, &source_gpu, preview, 1);
                call(owner, client, "draft.cancel", json!({"draft_id": draft_id}))
                    .expect("the draft cancelled");
                // The release: the same temperature committed, the mosaic redeveloped.
                call(
                    owner,
                    client,
                    "edit.set-raw",
                    json!({"asset_id": id, "temperature": kelvin,
                        "mutation": mutation(revision(owner, client, &id).unwrap(),
                                             &request_id("white-balance"), "agent")}),
                )
                .expect("the temperature committed");
                let released =
                    ready_preview_job(owner, request(PreviewRequest::new(client, asset.clone())))
                        .expect("the released frame");
                let developed = gpu_source_of(2, released.evaluation.source()).expect("a source");
                let mut surface = qualifier.surface();
                let (rest, planned) =
                    at_rest(&mut surface, &developed, &released.evaluation, view, 2);
                assert_eq!(rest.len(), moving.len(), "{} at {at}: one size", source.id);
                let gate = compare(&moving, &rest, size);
                let record = json!({
                    "source": source.id, "kelvin": kelvin, "view": at, "size": [size.0, size.1],
                    "moving_against_rest": figures(&gate),
                    "moving_against_release_view_plan": figures(&compare(&moving, &planned, size)),
                    "release_view_plan_against_rest": figures(&compare(&planned, &rest, size)),
                });
                eprintln!("{test}: {record}");
                measured.push(record);
            }
        }
    }
    if let Ok(output) = std::env::var("LUXFORGE_WB_OUTPUT") {
        std::fs::write(
            output,
            serde_json::to_string_pretty(
                &json!({"adapter": qualifier.adapter(), "cells": measured}),
            )
            .unwrap(),
        )
        .unwrap();
    }
}

/// A Temperature drag on a RAW is drawn on the GPU: at Fit and at 100% its every tick is planned
/// from the source the surface holds, the drafted change the plan's first step, over the boundary
/// the surface derives and draws on the GPU once it has evaluated it, never `boundary-stage`. The
/// surface is stood in for, as in every desktop test; the corpus test above draws the same plans
/// on the device.
#[test]
#[ignore = "the corpus RAWs: set LUXFORGE_RAW_MANIFEST to the private RAW manifest"]
fn a_raw_white_balance_drag_draws_on_the_gpu() {
    use super::{
        gpu_preview::SurfaceReport,
        gpu_preview_tests::{deliver_until, surface_ready},
        message::draft::DraftMessage,
        testing::{finish, slide},
    };
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(std::env::var("LUXFORGE_RAW_MANIFEST").expect("a manifest"))
            .unwrap(),
    )
    .unwrap();
    let path = manifest["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["id"] == "nikon-z6")
        .and_then(|source| source["path"].as_str())
        .map(std::path::PathBuf::from)
        .expect("the Z6 in the manifest");
    for at in ["fit", "100%"] {
        let catalog = luxforge_testbase::paths::temp_path(&format!(
            "raw-white-balance-{}.sqlite",
            at.trim_end_matches('%')
        ));
        let (mut editor, _, _) = crate::app::testing::real_photo_at(&catalog, &path);
        deliver_until(&mut editor, "the first frame", |editor| {
            editor.presentation.dimensions.is_some() && !editor.presentation.queue.is_busy()
        });
        if at == "100%" {
            editor.session.preview.view.zoom = luxforge_core::Zoom::Percent { value: 100.0 };
        }
        editor.gpu.surface = Some(SurfaceReport::default());
        for (tick, kelvin) in [3600.0, 3500.0].into_iter().enumerate() {
            let _ = slide(&mut editor, "set-raw", "temperature", kelvin);
            let (plan, request) = editor.gpu.planned().expect("a plan");
            assert_eq!(request.key.region().is_some(), at == "100%", "{at}");
            let first = &plan.content[0];
            assert_eq!(first.units.len(), 1, "{at}");
            assert_eq!(
                first.units[0].program.entry, "lf_basic_white_balance",
                "{at}: the change first"
            );
            if tick == 0 {
                // A lens warp's grid, computed off the interface thread as the blocking pool
                // answers it; the tick after it derives the boundary.
                let request = editor.gpu.planned().expect("a plan").1.clone();
                if let Some(grid) = request.grid() {
                    let _ = editor.update(crate::app::Message::Preview(
                        crate::app::message::preview::PreviewMessage::GridReady(Box::new(
                            super::gpu_preview::GridAnswer {
                                key: super::gpu_preview::GridKey::of(&request)
                                    .expect("a lens warp's key"),
                                grid: grid.map_err(|error| error.to_string()),
                            },
                        )),
                    ));
                    let _ = slide(&mut editor, "set-raw", "temperature", kelvin - 50.0);
                }
                assert!(
                    editor.gpu.holds_boundary(),
                    "{at}: the source's boundary: {}",
                    editor.gpu.summary()["drag"]
                );
                surface_ready(&mut editor);
            } else {
                assert_eq!(
                    editor.gpu.ticks().0,
                    1,
                    "{at}: a tick on the GPU: {}",
                    editor.gpu.summary()["drag"]
                );
                assert_eq!(editor.gpu_plan_fallback(), None, "{at}");
            }
        }
        let _ = editor.update(crate::app::Message::Draft(DraftMessage::Cancel));
        finish(editor, catalog);
    }
}
