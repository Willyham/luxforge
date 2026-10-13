//! The minimap's overview on the desktop: what is held, reused, replaced and let go.
use super::*;
use luxforge_core::{AssetId, EntryId};

fn key(asset: &AssetId, entry: &EntryId, pixels: u64, geometry: u64) -> OverviewKey {
    OverviewKey {
        asset: asset.clone(),
        entry: entry.clone(),
        layers: None,
        pixels,
        geometry,
    }
}

fn overview(key: &OverviewKey, renderer: Renderer) -> Overview {
    Overview {
        key: key.clone(),
        renderer,
        width: 256,
        height: 171,
        rgba: Arc::from(vec![7u8; 256 * 171 * 4]),
    }
}

/// Asked for again — a pan's or a zoom's re-plan of the same content — the overview held is reused
/// with nothing made again; an edit that keeps the geometry shows it marked updating until its
/// own lands; a crop, which moves the geometry, lets it go and shows the placeholder, navigation
/// off; and an answer for content no longer wanted is dropped.
#[test]
fn an_overview_is_reused_kept_updating_and_let_go_by_its_key() {
    let (asset, entry) = (AssetId::new(), EntryId::new());
    let first = key(&asset, &entry, 1, 10);
    let mut overviews = Overviews::default();
    assert_eq!(overviews.view(false), OverviewView::None);
    overviews.want(Some(first.clone()), Some(1));
    assert_eq!(overviews.view(false), OverviewView::Placeholder);
    assert!(!overviews.view(false).navigable());
    assert!(overviews.adopt(overview(&first, Renderer::Gpu)));
    assert!(matches!(overviews.view(false), OverviewView::Ready(held) if held.key == first));
    for _ in 0..3 {
        overviews.want(Some(first.clone()), Some(1));
    }
    assert_eq!(overviews.figures.reused, 3);
    assert_eq!(overviews.figures.gpu_built, 1, "nothing made again");
    // In motion, and for an edit of the same geometry, the held one is shown updating.
    assert!(matches!(overviews.view(true), OverviewView::Updating(_)));
    let edited = key(&asset, &entry, 2, 10);
    overviews.want(Some(edited.clone()), Some(2));
    let view = overviews.view(false);
    assert!(matches!(view, OverviewView::Updating(held) if held.key == first));
    assert!(view.navigable(), "the stage is the same");
    // A late answer for the first content is not taken up.
    assert!(!overviews.adopt(overview(&first, Renderer::Gpu)));
    assert_eq!(overviews.figures.stale, 1);
    assert!(overviews.adopt(overview(&edited, Renderer::Reference)));
    assert!(matches!(overviews.view(false), OverviewView::Ready(held) if held.key == edited));
    // A crop moves the geometry: the thumbnail goes, navigation is off until its own lands.
    let cropped = key(&asset, &entry, 3, 11);
    overviews.want(Some(cropped.clone()), Some(3));
    assert_eq!(overviews.current(), None);
    assert_eq!(overviews.view(false), OverviewView::Placeholder);
    // Another photograph or the same pixels of another entry are other content.
    assert!(!key(&AssetId::new(), &entry, 3, 11).same_geometry(&cropped));
    assert!(key(&asset, &EntryId::new(), 3, 11).same_geometry(&cropped));
    assert_ne!(key(&asset, &EntryId::new(), 3, 11), cropped);
}

/// An overview that cannot be made says why, with navigation off; one of the same geometry held
/// stays shown, updating, rather than the photograph losing its minimap.
#[test]
fn a_failed_overview_is_unavailable_unless_one_of_its_geometry_is_held() {
    let (asset, entry) = (AssetId::new(), EntryId::new());
    let first = key(&asset, &entry, 1, 10);
    let mut overviews = Overviews::default();
    overviews.want(Some(first.clone()), Some(1));
    overviews.fail(first.clone(), "budget-exceeded");
    assert_eq!(
        overviews.view(false),
        OverviewView::Unavailable("budget-exceeded")
    );
    assert!(overviews.settled(&first), "nothing more is asked for it");
    assert!(!overviews.view(false).navigable());
    let later = key(&asset, &entry, 2, 10);
    overviews.want(Some(later.clone()), Some(2));
    assert!(overviews.adopt(overview(&later, Renderer::Gpu)));
    let edited = key(&asset, &entry, 3, 10);
    overviews.want(Some(edited.clone()), Some(3));
    overviews.fail(edited, "fallback");
    assert!(matches!(overviews.view(false), OverviewView::Updating(held) if held.key == later));
    assert_eq!(overviews.figures.failed, 2);
}

/// Everything is let go with the photograph, a reduction on its way cancelled, and what is held
/// stays within the design's CPU bound: the overview and one replacement with its band.
#[test]
fn the_overviews_are_bounded_and_let_go_with_the_photograph() {
    let (asset, entry) = (AssetId::new(), EntryId::new());
    let first = key(&asset, &entry, 1, 10);
    let mut overviews = Overviews::default();
    overviews.want(Some(first.clone()), Some(1));
    assert!(overviews.adopt(overview(&first, Renderer::Gpu)));
    let cancel = Cancel::new();
    overviews.reducing = Some(Reducing {
        key: key(&asset, &entry, 2, 10),
        cancel: cancel.clone(),
    });
    assert!(overviews.bytes() <= luxforge_core::OVERVIEW_CPU_BYTES);
    assert!(overviews.bytes() >= 256 * 171 * 4);
    overviews.clear();
    assert!(cancel.check().is_err(), "the reduction is cancelled");
    assert_eq!(overviews.view(false), OverviewView::None);
    assert_eq!(overviews.bytes(), 0);
    assert_eq!(overviews.figures.reference_cancelled, 1);
}

/// The catalog an editor test opens its photograph in.
fn catalog(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "luxforge-overview-{name}-{}.sqlite",
        std::process::id()
    ))
}

/// Take up the preview worker's results until `done`.
fn deliver_until(editor: &mut Editor, what: &str, mut done: impl FnMut(&Editor) -> bool) {
    luxforge_testbase::wait_until(what, || {
        let _ = editor.update(Message::Preview(PreviewMessage::Poll));
        done(editor)
    });
}

use crate::app::message::{preview::PreviewMessage, view::ViewMessage};
use crate::app::testing::{finish, real_photo};
use luxforge_core::Zoom;

/// With no GPU the reference draws the photograph: at 200% its full-resolution frame, already
/// held, is reduced on a worker, exactly the core's reduction of it, and pans and zooms after it
/// reuse it with no reduction again. Fit and 199.9% start none.
#[test]
fn the_reference_overview_is_reduced_from_the_frame_held_and_reused() {
    let catalog = catalog("reference");
    let (mut editor, _, _) = real_photo(&catalog);
    deliver_until(&mut editor, "the open's reference frame", |editor| {
        editor.presentation.exact.is_some() && !editor.presentation.queue.is_busy()
    });
    let wanted = editor
        .overviews
        .wanted()
        .cloned()
        .expect("the open's content wanted");
    assert_eq!(wanted.layers, None);
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    assert!(editor.overviews.reducing.is_none(), "none at Fit");
    editor.session.preview.view.zoom = Zoom::Percent { value: 199.9 };
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    assert!(editor.overviews.reducing.is_none(), "none below 200%");
    editor.session.preview.view.zoom = Zoom::Percent { value: 200.0 };
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    let reducing = editor.overviews.reducing.as_ref().expect("a reduction");
    assert_eq!(reducing.key, wanted);
    assert_eq!(editor.overviews.figures.reference_started, 1);
    assert_eq!(editor.overview_view(), OverviewView::Placeholder);
    // The worker's answer, as the blocking pool computes it.
    let frame = editor.presentation.exact.as_ref().unwrap().raster.clone();
    let reduced = luxforge_core::reduce_overview(&frame, &Cancel::new()).unwrap();
    editor.reference_overview_reduced(ReferenceOverview {
        key: wanted.clone(),
        result: Ok(reduced.clone()),
    });
    let OverviewView::Ready(held) = editor.overview_view() else {
        panic!("the overview: {:?}", editor.overview_view());
    };
    assert_eq!(held.renderer, Renderer::Reference);
    assert_eq!((held.width, held.height), (reduced.width, reduced.height));
    assert_eq!(&*held.rgba, reduced.rgba.as_slice(), "exactly the core's");
    // Pans and zooms keep it: nothing is reduced again.
    for (x, y) in [(120.0, 40.0), (900.0, 600.0)] {
        let _ = editor.update(Message::View(ViewMessage::Panned(x, y)));
    }
    editor.session.preview.view.zoom = Zoom::Percent { value: 800.0 };
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    assert!(matches!(editor.overview_view(), OverviewView::Ready(_)));
    assert_eq!(editor.overviews.figures.reference_started, 1);
    assert_eq!(editor.overviews.figures.reference_built, 1);
    // A late answer for content no longer wanted is dropped, the one held kept.
    let other = OverviewKey {
        pixels: wanted.pixels ^ 1,
        ..wanted.clone()
    };
    editor.reference_overview_reduced(ReferenceOverview {
        key: other,
        result: Ok(reduced),
    });
    assert!(matches!(editor.overview_view(), OverviewView::Ready(held) if held.key == wanted));
    assert_eq!(editor.overviews.figures.stale, 1);
    finish(editor, catalog);
}

/// At 200% on the GPU the committed stack's view is planned with its tiles reduced to the
/// overview: the desktop holds them under the content's key, hands them to the surface, and takes
/// up the readback of their version alone — never another's. A pan's re-plan of the same content
/// keeps the tiles, with no tile drawn again, and reuses the overview.
#[test]
fn the_gpus_overview_is_taken_up_by_its_version_and_reused_across_pans() {
    let catalog = catalog("gpu");
    let (mut editor, asset, _) = real_photo(&catalog);
    deliver_until(&mut editor, "the open's reference frame", |editor| {
        editor.presentation.presenter.photo().is_some() && editor.presentation.analysis.is_some()
    });
    editor.renderer.stage = Some(luxforge_gpu::GpuStageState::Available);
    editor.session.preview.view.zoom = Zoom::Percent { value: 200.0 };
    let _ = editor.zoom_changed(&Zoom::Fit);
    let plan = |editor: &mut Editor| {
        let crate::app::gpu_preview::GpuAsk::Region(rect, magnification, _) = editor.gpu_ask()
        else {
            panic!("a region at 200%");
        };
        assert_eq!(magnification, 2.0);
        let job = crate::app::tasks::ready_preview_job(
            &editor.owner,
            luxforge_core::PreviewRequest::new(editor.client, asset.clone())
                .entry(editor.displayed_entry())
                .analyse()
                .gpu_region(rect, magnification),
        )
        .expect("the view's job");
        let key = crate::app::preview::overview_key(&job).expect("a committed stack's key");
        let source = job.evaluation.source().identity();
        let epoch = editor.view_plan.epoch;
        let _ = editor.update(Message::Preview(PreviewMessage::ViewLoaded {
            epoch,
            result: Ok(Box::new(job)),
        }));
        (key, source)
    };
    let (key, source) = plan(&mut editor);
    assert_eq!(editor.overviews.wanted(), Some(&key));
    assert!(editor.gpu.makes_overview(&key), "the GPU makes it");
    deliver_until(
        &mut editor,
        "tiles reduced to the overview handed",
        |editor| {
            editor
                .gpu_counts_handed()
                .or(editor.gpu_rest_handed())
                .is_some_and(|rest| rest.overview.is_some())
        },
    );
    let handed = editor
        .gpu_counts_handed()
        .or(editor.gpu_rest_handed())
        .unwrap();
    let version = handed.version;
    let size = handed.overview.as_ref().unwrap().view;
    assert!(size.0 <= 256 && size.1 <= 256);
    let pixels = luxforge_gpu::OverviewPixels {
        width: size.0,
        height: size.1,
        rgba: Arc::from(vec![90u8; size.0 as usize * size.1 as usize * 4]),
    };
    // A readback of another version is not this content's.
    editor.gpu.overview_read = Some((
        version + 1000,
        luxforge_gpu::OverviewOutcome::Ready(pixels.clone()),
    ));
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    assert_eq!(editor.overview_view(), OverviewView::Placeholder);
    editor.gpu.overview_read = Some((version, luxforge_gpu::OverviewOutcome::Ready(pixels)));
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    let OverviewView::Ready(held) = editor.overview_view() else {
        panic!("the overview: {:?}", editor.overview_view());
    };
    assert_eq!(held.renderer, Renderer::Gpu);
    assert_eq!(held.key, key);
    assert_eq!(editor.overviews.figures.reference_started, 0);
    // A pan's re-plan of the same content keeps the tiles and the overview.
    let versions = editor.gpu.rest_versions(&source).expect("the tiles held");
    assert!(versions.contains(&version));
    let _ = editor.update(Message::View(ViewMessage::Panned(400.0, 300.0)));
    let (again, _) = plan(&mut editor);
    assert_eq!(again, key);
    assert_eq!(
        editor.gpu.rest_versions(&source),
        Some(versions),
        "the same tiles held: none drawn again"
    );
    assert!(editor.overviews.figures.reused >= 1);
    assert!(matches!(editor.overview_view(), OverviewView::Ready(_)));
    assert_eq!(editor.overviews.figures.gpu_built, 1);
    finish(editor, catalog);
}
