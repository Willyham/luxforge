//! The minimap overview's lifecycle, without a device: what it makes, adopts, reuses and drops,
//! and what the minimap shows, from the facts the desktop hands it.
use super::*;
use luxforge_ui::{OverviewFigures, OverviewShown};

fn pixels(key: u64) -> OverviewContent {
    OverviewContent::Pixels(
        key,
        ProxyIdentity::Jpeg {
            fingerprint: "sha256:overview".into(),
            width: 6000,
            height: 4000,
            orientation: 1,
        },
    )
}

fn stage(width: u32, height: u32) -> OverviewStage {
    OverviewStage {
        kind: StageKind::Output,
        width,
        height,
    }
}

fn desired(asset: &luxforge_core::AssetId, content: u64, stage: OverviewStage) -> Desired {
    Desired {
        asset: asset.clone(),
        stage,
        key: key_of(asset, pixels(content), stage),
        lent: None,
    }
}

fn lent(width: u32, height: u32) -> Lent {
    Lent::Raster(Arc::new(luxforge_core::Raster {
        width,
        height,
        rgba: Arc::new(vec![128; (width * height * 4) as usize]),
        source_fingerprint: "sha256:overview".into(),
        snapshot_id: luxforge_core::SnapshotId::new(),
    }))
}

/// Facts at rest with the GPU allowed and a shown window, the surface holding `held`.
fn facts(held: &[(u64, (u32, u32))]) -> Facts {
    let mut figures = OverviewFigures::default();
    for (slot, (version, size)) in figures.held.iter_mut().zip(held) {
        *slot = Some(OverviewShown {
            version: *version,
            width: size.0,
            height: size.1,
        });
    }
    Facts {
        moving: false,
        gpu_allowed: true,
        work_allowed: true,
        surface: Some(figures),
        sweepable: None,
        sweep_failed: false,
    }
}

fn sweep_of(work: Work) -> u64 {
    match work {
        Work::Sweep { version } => version,
        _ => panic!("a sweep was expected"),
    }
}

/// Settle `content`'s overview through the GPU's sweep: asked for, made on the surface, adopted.
fn settle_gpu(
    overviews: &mut Overviews,
    asset: &luxforge_core::AssetId,
    content: u64,
) -> (Desired, u64) {
    let wanted = desired(asset, content, stage(6000, 4000));
    let mut at_rest = facts(&[]);
    at_rest.sweepable = wanted.key.clone();
    let version = sweep_of(overviews.reconcile(Some(&wanted), &at_rest));
    assert_eq!(overviews.sweep_version(), Some(version));
    assert_eq!(overviews.handed().awaited, Some(version));
    let mut made = facts(&[(version, (256, 171))]);
    made.sweepable = wanted.key.clone();
    assert!(matches!(
        overviews.reconcile(Some(&wanted), &made),
        Work::None
    ));
    (wanted, version)
}

#[test]
fn a_swept_overview_is_adopted_ready_and_reused_with_no_work() {
    let asset = luxforge_core::AssetId::new();
    let mut overviews = Overviews::default();
    let (wanted, version) = settle_gpu(&mut overviews, &asset, 7);
    let MinimapOverview::Ready(shown) = overviews.minimap(Some(&wanted)) else {
        panic!("ready: {:?}", overviews.minimap(Some(&wanted)));
    };
    assert_eq!(shown.image.version, version);
    assert_eq!(shown.image.size, (256, 171));
    assert_eq!(shown.stage, stage(6000, 4000));
    assert_eq!(overviews.handed().keep, Some(version));
    assert_eq!(overviews.handed().awaited, None);
    // Pan, resize and zoom above the threshold change nothing it is keyed by: no work, however
    // often reconciled, and the same image.
    let held = facts(&[(version, (256, 171))]);
    for _ in 0..5 {
        assert!(matches!(overviews.reconcile(Some(&wanted), &held), Work::None));
    }
    assert_eq!(overviews.counters().gpu_builds, 1);
    assert_eq!(overviews.counters().cpu_builds, 0);
    assert_eq!(overviews.counters().uploads, 0);
    // A content admitted again with the same pixels — a cancelled draft — keys the same overview.
    overviews.admitted(9, Some(pixels(7)));
    assert_eq!(overviews.content(9), Some(&pixels(7)));
    assert!(matches!(
        overviews.minimap(Some(&wanted)),
        MinimapOverview::Ready(_)
    ));
}

#[test]
fn a_retained_frame_is_reduced_on_a_worker_and_uploaded_once() {
    let asset = luxforge_core::AssetId::new();
    let mut overviews = Overviews::default();
    let mut wanted = desired(&asset, 3, stage(600, 400));
    wanted.lent = Some(lent(600, 400));
    let Work::Reduce {
        version,
        lent,
        stage: reduced_stage,
        size,
        cancel,
    } = overviews.reconcile(Some(&wanted), &facts(&[]))
    else {
        panic!("a reduction was expected");
    };
    assert_eq!((reduced_stage, size), ((600, 400), (256, 171)));
    // One at a time: reconciling again starts nothing.
    assert!(matches!(
        overviews.reconcile(Some(&wanted), &facts(&[])),
        Work::None
    ));
    let overview =
        luxforge_core::reduce_overview(lent.pixels(), reduced_stage, size, &cancel).unwrap();
    assert!(overviews.reduced(Reduced {
        version,
        result: Ok(overview),
    }));
    // Handed to the surface until it holds it, Pending until then.
    let upload = overviews.handed().upload.clone().expect("the codes");
    assert_eq!(
        (upload.version, upload.width, upload.height),
        (version, 256, 171)
    );
    assert!(matches!(
        overviews.minimap(Some(&wanted)),
        MinimapOverview::Pending { .. }
    ));
    let held = facts(&[(version, (256, 171))]);
    assert!(matches!(overviews.reconcile(Some(&wanted), &held), Work::None));
    assert!(overviews.handed().upload.is_none(), "its codes go once held");
    assert_eq!(overviews.handed().keep, Some(version));
    assert!(matches!(
        overviews.minimap(Some(&wanted)),
        MinimapOverview::Ready(_)
    ));
    let counters = overviews.counters();
    assert_eq!(
        (counters.cpu_builds, counters.uploads, counters.gpu_builds),
        (1, 1, 0)
    );
    // Its validity outlives the GPU stage: a reference overview needs none.
    let mut refused = held.clone();
    refused.gpu_allowed = false;
    assert!(matches!(
        overviews.reconcile(Some(&wanted), &refused),
        Work::None
    ));
    assert!(matches!(
        overviews.minimap(Some(&wanted)),
        MinimapOverview::Ready(_)
    ));
}

#[test]
fn a_late_or_switched_result_never_shows() {
    let asset = luxforge_core::AssetId::new();
    let mut overviews = Overviews::default();
    let mut first = desired(&asset, 1, stage(600, 400));
    first.lent = Some(lent(600, 400));
    let Work::Reduce {
        version: late,
        lent: lent_first,
        cancel,
        ..
    } = overviews.reconcile(Some(&first), &facts(&[]))
    else {
        panic!("a reduction was expected");
    };
    // Another content is shown before it answers: its work is stale and cancelled.
    let mut second = desired(&asset, 2, stage(600, 400));
    second.lent = Some(lent(600, 400));
    assert!(matches!(
        overviews.reconcile(Some(&second), &facts(&[])),
        Work::None
    ));
    assert!(cancel.check().is_err());
    let overview = luxforge_core::reduce_overview(
        lent_first.pixels(),
        (600, 400),
        (256, 171),
        &luxforge_core::Cancel::never(),
    )
    .unwrap();
    assert!(!overviews.reduced(Reduced {
        version: late,
        result: Ok(overview),
    }));
    assert!(overviews.handed().upload.is_none());
    assert!(overviews.counters().stale >= 1);
    // The in-flight answer let the next reduction start, for the content shown.
    let Work::Reduce { version, .. } = overviews.reconcile(Some(&second), &facts(&[])) else {
        panic!("the shown content's reduction");
    };
    assert_ne!(version, late);
    // A surface holding the late version adopts nothing for it.
    let held_late = facts(&[(late, (256, 171))]);
    let _ = overviews.reconcile(Some(&second), &held_late);
    assert!(matches!(
        overviews.minimap(Some(&second)),
        MinimapOverview::Pending { .. }
    ));
}

#[test]
fn motion_keeps_the_settled_overview_updating_and_a_geometry_change_withdraws_it() {
    let asset = luxforge_core::AssetId::new();
    let mut overviews = Overviews::default();
    let (_, version) = settle_gpu(&mut overviews, &asset, 7);
    let held = facts(&[(version, (256, 171))]);
    // A gesture's drafted content: the same stage, so the settled image is shown updating, and
    // nothing is made in motion.
    let drafted = desired(&asset, 8, stage(6000, 4000));
    let mut moving = held.clone();
    moving.moving = true;
    moving.sweepable = drafted.key.clone();
    assert!(matches!(
        overviews.reconcile(Some(&drafted), &moving),
        Work::None
    ));
    let MinimapOverview::Updating(shown) = overviews.minimap(Some(&drafted)) else {
        panic!("updating");
    };
    assert_eq!(shown.image.version, version);
    assert_eq!(
        MinimapOverview::Updating(shown).navigable(),
        Some(stage(6000, 4000))
    );
    // Another geometry: the image is withdrawn and navigation waits for the matching one.
    let cropped = desired(&asset, 9, stage(4000, 4000));
    assert!(matches!(
        overviews.reconcile(Some(&cropped), &moving),
        Work::None
    ));
    let minimap = overviews.minimap(Some(&cropped));
    assert_eq!(
        minimap,
        MinimapOverview::Pending {
            stage: stage(4000, 4000)
        }
    );
    assert_eq!(minimap.navigable(), None);
    // A crop draft's input stage is another stage kind, never the output's image.
    let crop = Desired {
        stage: OverviewStage {
            kind: StageKind::CropInput,
            width: 6000,
            height: 4000,
        },
        key: None,
        asset: asset.clone(),
        lent: None,
    };
    assert!(matches!(
        overviews.minimap(Some(&crop)),
        MinimapOverview::Pending { .. }
    ));
    // Another photograph's stage of the same size never shows this one's image.
    let other = luxforge_core::AssetId::new();
    assert!(matches!(
        overviews.minimap(Some(&desired(&other, 7, stage(6000, 4000)))),
        MinimapOverview::Pending { .. }
    ));
}

#[test]
fn a_refused_overview_keeps_a_correctly_keyed_one_or_says_unavailable() {
    let asset = luxforge_core::AssetId::new();
    let mut overviews = Overviews::default();
    let (ready, version) = settle_gpu(&mut overviews, &asset, 7);
    // The next content's sweep is refused its storage.
    let next = desired(&asset, 8, stage(6000, 4000));
    let mut at_rest = facts(&[(version, (256, 171))]);
    at_rest.sweepable = next.key.clone();
    let refused_version = sweep_of(overviews.reconcile(Some(&ready), &at_rest));
    let mut refusal = at_rest.clone();
    if let Some(figures) = refusal.surface.as_mut() {
        figures.refused = Some((
            refused_version,
            luxforge_gpu::GpuFallback::BudgetExceeded {
                requested: 1,
                in_use: 0,
                budget: 0,
            },
        ));
    }
    assert!(matches!(
        overviews.reconcile(Some(&ready), &refusal),
        Work::None
    ));
    // The ready overview of the content shown stays ready.
    assert!(matches!(
        overviews.minimap(Some(&ready)),
        MinimapOverview::Ready(_)
    ));
    assert_eq!(overviews.counters().refusals, 1);
    // Once the refused content is shown: unavailable, still navigable, and not asked again.
    assert!(matches!(
        overviews.reconcile(Some(&next), &refusal),
        Work::None
    ));
    let minimap = overviews.minimap(Some(&next));
    assert_eq!(
        minimap,
        MinimapOverview::Unavailable {
            stage: stage(6000, 4000),
            reason: Unavailable::Budget
        }
    );
    assert_eq!(minimap.navigable(), Some(stage(6000, 4000)));
}

#[test]
fn a_sweep_finished_before_its_content_is_shown_waits_as_the_replacement() {
    let asset = luxforge_core::AssetId::new();
    let mut overviews = Overviews::default();
    let (shown, old) = settle_gpu(&mut overviews, &asset, 7);
    // A commit hands the next content's tiles before the content is presented.
    let next = desired(&asset, 8, stage(6000, 4000));
    let mut at_rest = facts(&[(old, (256, 171))]);
    at_rest.sweepable = next.key.clone();
    let version = sweep_of(overviews.reconcile(Some(&shown), &at_rest));
    let mut made = facts(&[(old, (256, 171)), (version, (256, 171))]);
    made.sweepable = next.key.clone();
    assert!(matches!(
        overviews.reconcile(Some(&shown), &made),
        Work::None
    ));
    // Both are kept: the settled one shown, the made one awaiting its content.
    assert_eq!(overviews.handed().keep, Some(old));
    assert_eq!(overviews.handed().awaited, Some(version));
    assert!(matches!(
        overviews.minimap(Some(&shown)),
        MinimapOverview::Ready(ready) if ready.image.version == old
    ));
    // Presented: adopted with no more work, the old one let go.
    assert!(matches!(
        overviews.reconcile(Some(&next), &made),
        Work::None
    ));
    assert!(matches!(
        overviews.minimap(Some(&next)),
        MinimapOverview::Ready(ready) if ready.image.version == version
    ));
    assert_eq!(overviews.handed().keep, Some(version));
    assert_eq!(overviews.handed().awaited, None);
    assert_eq!(overviews.counters().gpu_builds, 2);
}

#[test]
fn a_surface_that_no_longer_holds_it_or_another_photograph_lets_it_go() {
    let asset = luxforge_core::AssetId::new();
    let mut overviews = Overviews::default();
    let (wanted, _) = settle_gpu(&mut overviews, &asset, 7);
    // A hidden window's surface was released: the image is withdrawn, and nothing starts while
    // the window is hidden.
    let mut hidden = facts(&[]);
    hidden.work_allowed = false;
    hidden.sweepable = wanted.key.clone();
    assert!(matches!(
        overviews.reconcile(Some(&wanted), &hidden),
        Work::None
    ));
    assert!(matches!(
        overviews.minimap(Some(&wanted)),
        MinimapOverview::Pending { .. }
    ));
    // Shown again: made again.
    let mut shown = facts(&[]);
    shown.sweepable = wanted.key.clone();
    sweep_of(overviews.reconcile(Some(&wanted), &shown));
    // Below the threshold no work is kept in progress; the minimap is gone.
    assert!(matches!(overviews.reconcile(None, &shown), Work::None));
    assert_eq!(overviews.handed().awaited, None);
    assert_eq!(overviews.minimap(None), MinimapOverview::None);
    // Another photograph lets everything go.
    let (_, version) = settle_gpu(&mut overviews, &asset, 7);
    let other = luxforge_core::AssetId::new();
    let elsewhere = desired(&other, 1, stage(6000, 4000));
    let _ = overviews.reconcile(Some(&elsewhere), &facts(&[(version, (256, 171))]));
    assert_eq!(overviews.handed().keep, None);
    assert!(overviews.content(7).is_none());
}

#[test]
fn a_failed_sweep_or_reduction_says_unavailable() {
    let asset = luxforge_core::AssetId::new();
    let mut overviews = Overviews::default();
    let wanted = desired(&asset, 1, stage(6000, 4000));
    let mut at_rest = facts(&[]);
    at_rest.sweepable = wanted.key.clone();
    sweep_of(overviews.reconcile(Some(&wanted), &at_rest));
    let mut failed = at_rest.clone();
    failed.sweep_failed = true;
    assert!(matches!(
        overviews.reconcile(Some(&wanted), &failed),
        Work::None
    ));
    assert!(matches!(
        overviews.minimap(Some(&wanted)),
        MinimapOverview::Unavailable {
            reason: Unavailable::Failed,
            ..
        }
    ));
    // A reduction past the reference's bound is a budget refusal.
    let mut narrow = desired(&asset, 2, stage(90_000, 400));
    narrow.lent = Some(lent(1, 1));
    let Work::Reduce { version, .. } = overviews.reconcile(Some(&narrow), &facts(&[])) else {
        panic!("a reduction was expected");
    };
    assert!(!overviews.reduced(Reduced {
        version,
        result: Err(ReduceError::Bound),
    }));
    assert!(matches!(
        overviews.minimap(Some(&narrow)),
        MinimapOverview::Unavailable {
            reason: Unavailable::Budget,
            ..
        }
    ));
}

#[test]
fn the_gpu_and_core_overview_bounds_agree() {
    assert_eq!(
        luxforge_core::OVERVIEW_GPU_BYTES,
        luxforge_ui::photo_surface::OVERVIEW_GPU_BUDGET
    );
    assert_eq!(luxforge_core::OVERVIEW_BOUNDS.width, 256);
    assert_eq!(MINIMAP_ZOOM, 200.0);
}
