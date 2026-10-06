//! Rendered previews: keys and stale rows, each tier against the reference's exact frame
//! area-averaged to the same bounds, both tiers from one preparation, a spatial and a pixel-stage
//! stack, an original that is gone or changed, a missing provider and a missing artifact named by their
//! edits, cancellation, a backlog that never delays an open Develop preview and, with the supplied
//! RAW files, both tiers of an edited Nikon Z 6 photograph.
use super::*;
use crate::{
    BASIC_EFFECT, IndexDb, PreviewSource, RegistryOptions, SourceImage,
    artifacts::{
        object_path,
        testing::{registry as proof_registry, tint_bytes, tint_meta},
    },
    editor::{default_artifact_root, mutation},
    modules::{APPLY_PROOF_TINT, PROOF_MODULE},
    previews::rendered::band_tiles::{BandTiles, Draw},
    tiles::{ReferenceTiles, TileFallback, TileUnavailable},
};
use luxforge_testbase::{Gate, paths, wait_for, wait_until};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

const BOTH: [PreviewTier; 2] = [PreviewTier::Grid, PreviewTier::Large];

/// A `width` × `height` JPEG of gradients and fine texture, in a scratch directory of its own, so
/// its downscale averages real detail.
fn generated_jpeg(label: &str, width: u32, height: u32) -> PathBuf {
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            rgba.extend_from_slice(&[
                (x * 255 / width) as u8,
                (y * 255 / height) as u8,
                ((x * 7 + y * 13) % 256) as u8,
                255,
            ]);
        }
    }
    let mut jpeg = Vec::new();
    let settings = Settings {
        quality: 92,
        chroma: (1, 1),
        segments: &[],
        icc: None,
        pixels_per_inch: None,
    };
    luxforge_jpeg::encode(&mut jpeg, width, height, &rgba, &settings, &mut |_| {
        Ok::<(), Error>(())
    })
    .unwrap();
    let path = paths::temp_dir(label).join(format!("{label}.jpg"));
    fs::write(&path, jpeg).unwrap();
    path
}

/// A copy of the S0 fixture `name` in a scratch directory of its own, for a test that changes it.
fn copied_fixture(label: &str, name: &str) -> PathBuf {
    let path = paths::temp_dir(label).join(name);
    fs::copy(paths::fixture(&format!("s0/{name}")), &path).unwrap();
    path
}

/// A new catalog holding `path` with a Basic edit committed: a photograph open in Develop, its
/// source in the editor's cache.
fn edited(label: &str, path: &Path) -> (EditorService, AssetId) {
    let mut service = EditorService::open(&paths::temp_catalog(label)).unwrap();
    let asset = service.import(path).unwrap().asset.id;
    service
        .apply_action(
            &asset,
            mutation(0, "basic"),
            "set-basic",
            json!({"exposure": 0.4, "contrast": 25}),
        )
        .unwrap();
    (service, asset)
}

/// Every tier `request` renders, as its raster, from one production preparation.
fn rasters(request: &RenderRequest) -> Vec<(RenderedKey, Raster)> {
    render_with(
        request,
        &ReferenceTiles::new(),
        &Cancel::new(),
        prepare,
        |key, raster, _| Ok((key, raster)),
    )
    .unwrap()
}

/// The editor's own exact render of the asset's current entry, area-averaged to `side` × `side`
/// bounds by the byte downscale a source of display bytes takes, or the frame itself when it
/// already fits: what a session without a GPU draws at Fit at those bounds.
fn fit_reference(service: &EditorService, asset: &AssetId, side: u32) -> Raster {
    let exact = service.render_current(asset).unwrap();
    let bounds = ProxyBounds {
        width: side,
        height: side,
    };
    let size = (exact.width, exact.height);
    let Some(plan) = ProxyPlan::fit(size, size, bounds) else {
        return exact;
    };
    let PreviewSource::Jpeg(scaled) = PreviewSource::Jpeg(SourceImage {
        width: exact.width,
        height: exact.height,
        rgba: Arc::clone(&exact.rgba),
        fingerprint: String::new(),
        orientation: 1,
        capture: Arc::default(),
    })
    .proxy(plan)
    .unwrap() else {
        panic!("bytes");
    };
    Raster {
        width: scaled.width,
        height: scaled.height,
        rgba: scaled.rgba,
        ..exact
    }
}

fn sha256(path: &Path) -> String {
    format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
}

fn send_and_share<T: Send + Sync>() {}

/// A key names its asset, entry, tier and generation, and each changes its preview key and its
/// file; a commit makes new keys while the old entry still plans its own; the loupe tier and
/// repeated or missing tiers are refused; the current test compares keys.
#[test]
fn keys_name_asset_entry_tier_and_generation_and_a_commit_makes_new_ones() {
    send_and_share::<RenderRequest>();
    let (mut service, asset) = edited("rendered-keys", &paths::fixture("s0/orientation-1.jpg"));
    let before = plan_render(&service, &asset, None, &BOTH).unwrap();
    let old = before.keys();
    assert_eq!(
        old.iter().map(|key| key.tier).collect::<Vec<_>>(),
        BOTH,
        "in the order asked"
    );
    for key in &old {
        assert_eq!(
            (&key.asset_id, &key.entry_id, key.generation),
            (
                &asset,
                &service.current_entry_id(&asset).unwrap(),
                RENDERER_GENERATION
            )
        );
    }

    service
        .apply_action(
            &asset,
            mutation(1, "contrast"),
            "set-basic",
            json!({"contrast": -20}),
        )
        .unwrap();
    let current = service.current_entry_id(&asset).unwrap();
    let new = plan_render(&service, &asset, None, &BOTH).unwrap().keys();
    for (old, new) in old.iter().zip(&new) {
        assert_eq!(new.entry_id, current);
        assert_ne!(
            old.preview_key(),
            new.preview_key(),
            "a commit is a new key"
        );
        assert_ne!(old.file_name(), new.file_name(), "and a new file");
    }
    let named = plan_render(&service, &asset, Some(before.entry_id()), &BOTH).unwrap();
    assert_eq!(named.keys(), old, "a named entry keeps its own keys");

    let grid = &new[0];
    let variants = [
        grid.clone(),
        RenderedKey {
            asset_id: AssetId::new(),
            ..grid.clone()
        },
        RenderedKey {
            entry_id: EntryId::new(),
            ..grid.clone()
        },
        RenderedKey {
            tier: PreviewTier::Large,
            ..grid.clone()
        },
        RenderedKey {
            generation: RENDERER_GENERATION + 1,
            ..grid.clone()
        },
    ];
    for (index, one) in variants.iter().enumerate() {
        for other in &variants[index + 1..] {
            assert_ne!(one.preview_key(), other.preview_key());
            assert_ne!(one.file_name(), other.file_name());
        }
        let file = one.file_name();
        assert!(file.is_relative() && file.starts_with("photos"), "{file:?}");
        assert_eq!(
            file.components().count(),
            3,
            "photos/<shard>/<name>: {file:?}"
        );
    }
    assert_eq!(
        grid.preview_key(),
        format!("photo:{asset}:{current}:grid:r{RENDERER_GENERATION}")
    );

    assert!(is_current(
        &grid.preview_key(),
        &asset,
        &current,
        PreviewTier::Grid
    ));
    for stale in [&old[0], &variants[4]] {
        assert!(
            !is_current(&stale.preview_key(), &asset, &current, PreviewTier::Grid),
            "{stale:?}"
        );
    }
    assert!(!is_current(
        &grid.preview_key(),
        &asset,
        &current,
        PreviewTier::Large
    ));

    for tiers in [
        &[][..],
        &[PreviewTier::Loupe][..],
        &[PreviewTier::Grid, PreviewTier::Grid][..],
    ] {
        let refused = plan_render(&service, &asset, None, tiers).err().unwrap();
        assert_eq!(refused.kind, ErrorKind::Validation, "{tiers:?}");
    }
    assert_eq!(
        RenderedKey::new(&asset, &current, PreviewTier::Loupe)
            .unwrap_err()
            .kind,
        ErrorKind::Validation
    );
}

/// The grid tier of an edited 1200 × 800 JPEG is the exact render area-averaged to 512 px
/// bounds, byte for byte; its large tier, whose stage already fits 2048 px, is the exact render.
/// Both come from one preparation, encode to JPEGs of their sizes, are never labelled approximate,
/// are the same bytes every time whether or not the index directory exists, and follow a commit.
#[test]
fn a_grid_tier_is_the_exact_render_area_averaged_and_both_tiers_share_one_preparation() {
    let path = generated_jpeg("rendered-proxy", 1200, 800);
    let (mut service, asset) = edited("rendered-proxy", &path);
    let request = plan_render(&service, &asset, None, &BOTH).unwrap();
    let preparations = AtomicUsize::new(0);
    let tiers = render_with(
        &request,
        &ReferenceTiles::new(),
        &Cancel::new(),
        |request, cancel| {
            preparations.fetch_add(1, Ordering::SeqCst);
            prepare(request, cancel)
        },
        |key, raster, _| Ok((key, raster)),
    )
    .unwrap();
    assert_eq!(preparations.load(Ordering::SeqCst), 1, "one preparation");
    let [(grid_key, grid), (large_key, large)] = &tiers[..] else {
        panic!("two tiers");
    };
    assert_eq!(
        (grid_key.tier, large_key.tier),
        (PreviewTier::Grid, PreviewTier::Large)
    );

    let fit = fit_reference(&service, &asset, PHOTO_GRID_SIDE);
    assert_eq!((grid.width, grid.height), (512, 341));
    assert!(
        grid.rgba == fit.rgba,
        "the grid tier is the exact render area-averaged to the same bounds"
    );

    let exact = service.render_current(&asset).unwrap();
    assert_eq!((large.width, large.height), (1200, 800));
    assert!(
        large.rgba == exact.rgba,
        "the large tier is the exact render"
    );

    let encoded = render(&request, &ReferenceTiles::new(), &Cancel::new()).unwrap();
    for (tier, (key, raster)) in encoded.iter().zip(&tiers) {
        assert_eq!(&tier.key, key);
        let header = luxforge_jpeg::header(&tier.jpeg).unwrap();
        assert_eq!(
            (header.width, header.height, tier.width, tier.height),
            (raster.width, raster.height, raster.width, raster.height)
        );
        let info = tier.info(PathBuf::from("/c.index/previews").join(key.file_name()));
        assert_eq!(
            (info.origin, info.bytes, info.key.as_str(), info.tier),
            (
                PreviewOrigin::Rendered,
                tier.jpeg.len() as u64,
                key.preview_key().as_str(),
                key.tier
            )
        );
        assert!(!info.approximate, "the {} tier is exact", key.tier.as_str());
        println!(
            "{} tier {}×{}: {} bytes",
            key.tier.as_str(),
            tier.width,
            tier.height,
            tier.jpeg.len()
        );
    }
    // The cache is disposable: deleting the index directory loses nothing a render needs.
    drop(service.index().unwrap());
    fs::remove_dir_all(service.index_dir()).unwrap();
    let again = render(&request, &ReferenceTiles::new(), &Cancel::new()).unwrap();
    for (first, second) in encoded.iter().zip(&again) {
        assert!(
            first.jpeg == second.jpeg,
            "a tier is the same bytes every time"
        );
    }

    // A commit is a new entry, and its tiers show it.
    service
        .apply_action(
            &asset,
            mutation(1, "darker"),
            "set-basic",
            json!({"exposure": -1.0}),
        )
        .unwrap();
    let next = plan_render(&service, &asset, None, &[PreviewTier::Grid]).unwrap();
    let next = rasters(&next);
    let [(next_key, next_grid)] = &next[..] else {
        panic!("one tier");
    };
    assert_ne!(next_key, grid_key);
    assert!(next_grid.rgba != grid.rgba, "the new entry's pixels");
    assert!(
        next_grid.rgba == fit_reference(&service, &asset, PHOTO_GRID_SIDE).rgba,
        "the new entry's exact render area-averaged"
    );
}

/// A spatial stack (Clarity and Dehaze) is rendered exactly at its own stage and area-averaged to
/// each tier, so neither tier approximates its entry: the grid tier is the exact render reduced,
/// and neither says it is approximate.
#[test]
fn a_spatial_stack_is_rendered_exactly_and_never_approximated() {
    let path = generated_jpeg("rendered-spatial", 1200, 800);
    let (mut service, asset) = edited("rendered-spatial", &path);
    service
        .apply_action(
            &asset,
            mutation(1, "presence"),
            "set-presence",
            json!({"clarity": 40, "dehaze": 25}),
        )
        .unwrap();
    let request = plan_render(&service, &asset, None, &[PreviewTier::Grid]).unwrap();
    let tiers = rasters(&request);
    let [(_, grid)] = &tiers[..] else {
        panic!("one tier");
    };
    assert!(grid.rgba == fit_reference(&service, &asset, PHOTO_GRID_SIDE).rgba);

    let both = plan_render(&service, &asset, None, &BOTH).unwrap();
    for tier in render(&both, &ReferenceTiles::new(), &Cancel::new()).unwrap() {
        assert!(
            !tier
                .info(PathBuf::from("/c.index/previews/t.jpg"))
                .approximate,
            "{:?}",
            tier.key.tier
        );
    }
}

/// A tier is upright: a JPEG stored under EXIF orientation 6 renders as its upright 320 × 480
/// frame, the editor's own render of the entry.
#[test]
fn a_turned_original_renders_upright_tiers() {
    let (service, asset) = edited("rendered-upright", &paths::fixture("s0/orientation-6.jpg"));
    let exact = service.render_current(&asset).unwrap();
    assert_eq!((exact.width, exact.height), (320, 480));
    let request = plan_render(&service, &asset, None, &BOTH).unwrap();
    for (key, raster) in rasters(&request) {
        assert_eq!((raster.width, raster.height), (320, 480), "{:?}", key.tier);
        assert!(raster.rgba == exact.rgba, "{:?}", key.tier);
    }
}

/// A pixel-stage stack — a pixel replacement — is rendered exactly once and area-averaged to each
/// tier like any other.
#[test]
fn a_pixel_stage_stack_is_rendered_exactly_and_area_averaged() {
    let path = generated_jpeg("rendered-ineligible", 1200, 800);
    let mut service = EditorService::open_with(
        &paths::temp_catalog("rendered-ineligible"),
        Arc::new(ModuleRegistry::developer()),
    )
    .unwrap();
    let asset = service.import(&path).unwrap().asset.id;
    service
        .apply_pixel(&asset, mutation(0, "pixel"), 600, 400, [255, 0, 255])
        .unwrap();
    let exact = service.render_current(&asset).unwrap();
    let request = plan_render(&service, &asset, None, &BOTH).unwrap();
    let tiers = rasters(&request);
    for (key, raster) in &tiers {
        let bounds = ProxyBounds {
            width: tier_side(key.tier).unwrap(),
            height: tier_side(key.tier).unwrap(),
        };
        let reference = match ProxyPlan::fit((1200, 800), (1200, 800), bounds) {
            None => exact.rgba.to_vec(),
            Some(plan) => {
                let PreviewSource::Jpeg(scaled) = PreviewSource::Jpeg(SourceImage {
                    width: exact.width,
                    height: exact.height,
                    rgba: Arc::clone(&exact.rgba),
                    fingerprint: String::new(),
                    orientation: 1,
                    capture: Arc::default(),
                })
                .proxy(plan)
                .unwrap() else {
                    panic!("bytes");
                };
                scaled.rgba.to_vec()
            }
        };
        assert!(*raster.rgba == reference, "{:?}", key.tier);
    }
    assert_eq!(
        (tiers[0].1.width, tiers[0].1.height, tiers[1].1.width),
        (512, 341, 1200)
    );
}

/// An original that changed in place, was replaced by another file, or is gone is
/// `source-unavailable` for a render planned before the change: nothing is rendered from other
/// bytes. Its own bytes again, in place, render again.
#[test]
fn a_missing_or_changed_original_is_source_unavailable() {
    let path = copied_fixture("rendered-unavailable", "orientation-1.jpg");
    let bytes = fs::read(&path).unwrap();
    let (service, asset) = edited("rendered-unavailable", &path);
    let request = plan_render(&service, &asset, None, &[PreviewTier::Grid]).unwrap();
    assert_eq!(rasters(&request).len(), 1);
    let unavailable = |what: &str| {
        let refused = render(&request, &ReferenceTiles::new(), &Cancel::new()).unwrap_err();
        assert_eq!(
            refused.kind,
            ErrorKind::SourceUnavailable,
            "{what}: {refused}"
        );
    };
    let rewrite = |contents: &[u8]| {
        use std::io::Write;
        let mut file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.write_all(contents).unwrap();
    };

    // The same file and length, other bytes that still decode: the JFIF header's horizontal
    // density.
    assert_eq!(&bytes[6..11], b"JFIF\0");
    let mut changed = bytes.clone();
    changed[15] ^= 0x02;
    rewrite(&changed);
    unavailable("changed in place");
    rewrite(&bytes);
    assert_eq!(rasters(&request).len(), 1, "its own bytes render again");

    fs::remove_file(&path).unwrap();
    fs::write(&path, [&bytes[..], b"\0"].concat()).unwrap();
    unavailable("replaced by another file");

    fs::remove_file(&path).unwrap();
    unavailable("gone");
}

/// A layer whose provider is unavailable is refused when the render is planned, naming its layer;
/// an artifact the owner does not keep ready is read by the render itself and never handed back,
/// and one that is gone is refused naming the layer that references it, whether it went before the
/// plan or after it.
#[test]
fn a_missing_provider_or_artifact_is_refused_naming_its_edit() {
    let catalog = paths::temp_catalog("rendered-provider");
    let (asset, layer) = {
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service
            .import(&paths::fixture("s0/orientation-1.jpg"))
            .unwrap()
            .asset
            .id;
        service
            .apply_action(
                &asset,
                mutation(0, "basic"),
                "set-basic",
                json!({"contrast": 30}),
            )
            .unwrap();
        let layer = service
            .state(&asset)
            .unwrap()
            .current_entry
            .snapshot
            .recipe
            .layers[0]
            .id
            .clone();
        (asset, layer)
    };
    let disabled = ["luxforge.basic".to_owned()];
    let registry = ModuleRegistry::assemble(&RegistryOptions {
        disabled: &disabled,
        ..RegistryOptions::default()
    })
    .unwrap();
    let service = EditorService::open_with(&catalog, Arc::new(registry)).unwrap();
    let refused = plan_render(&service, &asset, None, &BOTH).err().unwrap();
    assert_eq!(refused.kind, ErrorKind::Incompatible, "{refused}");
    assert_eq!(refused.unavailable_effect_id(), Some(BASIC_EFFECT));
    assert!(refused.detail.contains(layer.as_str()), "{refused}");

    let directory = paths::temp_dir("rendered-artifact");
    let catalog = directory.join("catalog.sqlite");
    let (asset, artifact, layer, exact) = {
        let mut service = EditorService::open_with(&catalog, proof_registry()).unwrap();
        let asset = service
            .import(&paths::fixture("s0/orientation-1.jpg"))
            .unwrap()
            .asset
            .id;
        let writer = service.artifact_writer().unwrap();
        let (record, prepared) = writer
            .write(&tint_bytes([0.25, 1.0, 1.0]), tint_meta(), PROOF_MODULE)
            .unwrap();
        let artifact = record.id.clone();
        service.register_artifact(record, prepared, true).unwrap();
        service
            .apply_action(
                &asset,
                mutation(0, "tint"),
                APPLY_PROOF_TINT,
                json!({"artifact": artifact}),
            )
            .unwrap();
        let layer = service
            .state(&asset)
            .unwrap()
            .current_entry
            .snapshot
            .recipe
            .layers[0]
            .id
            .clone();
        let exact = service.render_current(&asset).unwrap();
        (asset, artifact, layer, exact)
    };
    // Reopened: the owner keeps no artifact ready and has prepared no original.
    let service = EditorService::open_with(&catalog, proof_registry()).unwrap();
    let request = plan_render(&service, &asset, None, &[PreviewTier::Grid]).unwrap();
    assert_eq!((request.ready.len(), request.reads.len()), (0, 1));
    let tiers = rasters(&request);
    let [(_, grid)] = &tiers[..] else {
        panic!("one tier");
    };
    assert!(grid.rgba == exact.rgba, "the tint's bytes were bound");
    assert_eq!(
        service
            .artifact_reads(std::slice::from_ref(&artifact))
            .unwrap()
            .len(),
        1,
        "the verified bytes stayed with the render"
    );
    assert!(
        service.cached_state(&asset).unwrap().is_none(),
        "the original stayed with the render"
    );

    fs::remove_file(object_path(&default_artifact_root(&catalog), &artifact)).unwrap();
    let after_plan = render(&request, &ReferenceTiles::new(), &Cancel::new()).unwrap_err();
    let before_plan = plan_render(&service, &asset, None, &[PreviewTier::Grid])
        .err()
        .unwrap();
    for refused in [after_plan, before_plan] {
        assert_eq!(refused.kind, ErrorKind::SourceUnavailable, "{refused}");
        assert!(
            refused.detail.contains(layer.as_str()) && refused.detail.contains(artifact.as_str()),
            "{refused}"
        );
        let data = refused.data.as_deref().unwrap();
        assert_eq!(data["layers"], json!([layer.as_str()]), "{refused}");
        assert_eq!(data["artifact_id"], json!(artifact.as_str()), "{refused}");
    }
}

/// A render cancelled before it starts prepares nothing; one cancelled while it prepares, after
/// it has prepared, or between its tiers returns `cancelled` and no further tier.
#[test]
fn a_cancelled_render_stops_and_answers_cancelled() {
    let path = generated_jpeg("rendered-cancel", 1200, 800);
    let (service, asset) = edited("rendered-cancel", &path);
    let request = plan_render(&service, &asset, None, &BOTH).unwrap();
    let preparations = AtomicUsize::new(0);
    let finished = AtomicUsize::new(0);
    let cancelled = |at: &str| -> Error {
        let cancel = Cancel::new();
        if at == "before" {
            cancel.cancel();
        }
        render_with(
            &request,
            &ReferenceTiles::new(),
            &cancel,
            |request, token| {
                preparations.fetch_add(1, Ordering::SeqCst);
                if at == "preparing" {
                    cancel.cancel();
                }
                let prepared = prepare(request, token);
                if at == "prepared" {
                    cancel.cancel();
                }
                prepared
            },
            |_, _, _| {
                finished.fetch_add(1, Ordering::SeqCst);
                if at == "between" {
                    cancel.cancel();
                }
                Ok(())
            },
        )
        .unwrap_err()
    };
    for (at, prepared, tiers) in [
        ("before", 0, 0),
        ("preparing", 1, 0),
        ("prepared", 1, 0),
        ("between", 1, 1),
    ] {
        preparations.store(0, Ordering::SeqCst);
        finished.store(0, Ordering::SeqCst);
        let error = cancelled(at);
        assert_eq!(error.kind, ErrorKind::Cancelled, "{at}: {error}");
        assert_eq!(
            (
                preparations.load(Ordering::SeqCst),
                finished.load(Ordering::SeqCst)
            ),
            (prepared, tiers),
            "{at}"
        );
    }
}

/// The acceptance's ordering check. A backlog of rendered previews of the photograph Develop has
/// open — each prepared, holding its decoded original, and held before it renders its tiers —
/// runs on lane threads while Develop asks for the photograph's preview through the existing
/// preview queue: its exact frame arrives while every render of the backlog is still held, so the preview never waited behind them, and the editor's cache still
/// holds the photograph. Released, the backlog completes.
#[test]
fn a_rendered_backlog_never_delays_an_open_develop_preview() {
    const BACKLOG: usize = 3;
    let path = generated_jpeg("rendered-backlog", 1200, 800);
    let (service, asset) = edited("rendered-backlog", &path);
    assert!(service.cached_state(&asset).unwrap().is_some());
    let request = Arc::new(plan_render(&service, &asset, None, &BOTH).unwrap());
    let hold = Arc::new(Gate::new());
    hold.shut();
    let finished = Arc::new(AtomicUsize::new(0));
    let backlog: Vec<_> = (0..BACKLOG)
        .map(|_| {
            let (request, hold, finished) = (
                Arc::clone(&request),
                Arc::clone(&hold),
                Arc::clone(&finished),
            );
            thread::spawn(move || {
                let tiers = render_with(
                    &request,
                    &ReferenceTiles::new(),
                    &Cancel::new(),
                    |request, cancel| {
                        let prepared = prepare(request, cancel);
                        hold.pass();
                        prepared
                    },
                    |key, raster, _| Ok((key, raster)),
                );
                finished.fetch_add(1, Ordering::SeqCst);
                tiers
            })
        })
        .collect();
    hold.wait_reached(BACKLOG as u64, "the backlog");
    wait_until("every render of the backlog is held", || {
        hold.waiting() == BACKLOG
    });

    let bounds = ProxyBounds {
        width: 1024,
        height: 1024,
    };
    let mut job = service
        .preview_job(&asset, None, None, None, Some(bounds))
        .unwrap();
    job.analyse = true;
    let mut queue = crate::PreviewQueue::default();
    queue.request(job);
    let exact = wait_for("Develop's exact frame", || queue.poll());
    let outcome = exact.exact().expect("the exact phase");
    assert!(outcome.result.is_ok() && outcome.report.is_some());
    assert_eq!(
        finished.load(Ordering::SeqCst),
        0,
        "no rendered preview finished before Develop's preview"
    );
    assert!(
        hold.holding() && hold.waiting() == BACKLOG,
        "the whole backlog was running throughout"
    );
    assert!(
        service.cached_state(&asset).unwrap().is_some(),
        "the editor's cache still holds the open photograph"
    );

    hold.open();
    for render in backlog {
        let tiers = render.join().unwrap().unwrap();
        assert_eq!(tiers.len(), 2);
    }
}

/// The stale rows of a photograph are its other entries' rows and its rendered rows of another
/// generation; a camera preview of its current entry stays until a render replaces it. The
/// generation listing pages through every photograph's rendered rows of another generation.
#[test]
fn stale_rows_are_other_entries_and_other_generations() {
    let (index, _) = IndexDb::open(
        &paths::temp_dir("rendered-stale").join("catalog.index"),
        "catalog-a",
    )
    .unwrap();
    // Identities in the order their names sort: the name, padded with zeros to a UUID's length.
    let id = |prefix: &str, name: &str| format!("{prefix}{name:0<32}");
    let (a, b, c) = (id("asset-", "a"), id("asset-", "b"), id("asset-", "c"));
    let (old, current, other, camera) = (
        id("entry-", "0"),
        id("entry-", "1"),
        id("entry-", "2"),
        id("entry-", "3"),
    );
    let other_generation = i64::from(RENDERER_GENERATION) + 1;
    let now = i64::from(RENDERER_GENERATION);
    let rows = [
        (&a, &old, "grid", now, "rendered"),
        (&a, &old, "large", 0, "embedded"),
        (&a, &current, "grid", now, "rendered"),
        (&a, &current, "large", other_generation, "rendered"),
        (&b, &other, "grid", other_generation, "rendered"),
        (&c, &camera, "grid", 0, "embedded"),
    ];
    for (n, (asset, entry, tier, renderer, origin)) in rows.iter().enumerate() {
        index
            .connection()
            .execute(
                "INSERT INTO photo_previews VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,
                         CASE ?9 WHEN 'rendered' THEN 'gpu' END, NULL)",
                params![
                    asset,
                    entry,
                    tier,
                    renderer,
                    format!("photos/{n}.jpg"),
                    512,
                    341,
                    1000 + n as i64,
                    origin,
                    0,
                    false
                ],
            )
            .unwrap();
    }
    let listed = |rows: Vec<PhotoPreviewRow>| -> Vec<(String, String, &'static str, i64)> {
        rows.into_iter()
            .map(|row| {
                (
                    row.asset_id.to_string(),
                    row.entry_id.to_string(),
                    row.tier.as_str(),
                    row.renderer,
                )
            })
            .collect()
    };
    let asset = |name: &String| AssetId::parse(name.clone()).unwrap();
    let entry = |name: &String| EntryId::parse(name.clone()).unwrap();
    assert_eq!(
        listed(stale_rows(index.connection(), &asset(&a), &entry(&current)).unwrap()),
        [
            (a.clone(), old.clone(), "grid", now),
            (a.clone(), old.clone(), "large", 0),
            (a.clone(), current.clone(), "large", other_generation),
        ]
    );
    let row = &stale_rows(index.connection(), &asset(&a), &entry(&current)).unwrap()[1];
    assert_eq!(
        (row.origin, row.path.as_path(), row.bytes),
        (PreviewOrigin::Embedded, Path::new("photos/1.jpg"), 1001)
    );
    assert!(
        stale_rows(index.connection(), &asset(&c), &entry(&camera))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        listed(other_generation_rows(index.connection(), 10).unwrap()),
        [
            (a.clone(), current.clone(), "large", other_generation),
            (b.clone(), other.clone(), "grid", other_generation),
        ]
    );
    assert_eq!(
        other_generation_rows(index.connection(), 1).unwrap().len(),
        1
    );
}

/// Both tiers of an edited Nikon Z 6 photograph (Basic and Clarity) in a reopened catalog: the
/// original prepared by the render alone and adopted by nothing, each tier byte for byte the
/// editor's exact render area-averaged to its bounds once Develop prepares the photograph, and
/// the file unchanged. Set `LUXFORGE_RAW_OWNER_DIR` to the directory holding it and run in release:
///
/// ```text
/// LUXFORGE_RAW_OWNER_DIR=/path/to/raw cargo test --release -p luxforge-core --lib \
///   preview_rendered -- --ignored --nocapture
/// ```
#[test]
#[ignore = "requires the supplied RAW files; run in release"]
fn a_supplied_raw_renders_both_tiers_of_an_edited_photograph() {
    let owner = PathBuf::from(std::env::var("LUXFORGE_RAW_OWNER_DIR").expect("RAW directory"));
    let path = owner.join("nikon_z6.NEF");
    let before = sha256(&path);
    let catalog = paths::temp_catalog("rendered-raw");
    let asset = {
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&path).unwrap().asset.id;
        service
            .apply_action(
                &asset,
                mutation(0, "basic"),
                "set-basic",
                json!({"exposure": 0.3, "contrast": 20}),
            )
            .unwrap();
        service
            .apply_action(
                &asset,
                mutation(1, "presence"),
                "set-presence",
                json!({"clarity": 30}),
            )
            .unwrap();
        asset
    };
    let mut service = EditorService::open(&catalog).unwrap();
    assert!(service.cached_state(&asset).unwrap().is_none());
    let request = plan_render(&service, &asset, None, &BOTH).unwrap();
    let started = std::time::Instant::now();
    let tiers = rasters(&request);
    println!("nikon_z6.NEF: both tiers in {:?}", started.elapsed());
    assert!(
        service.cached_state(&asset).unwrap().is_none(),
        "the render adopted nothing into the editor's cache"
    );
    let needs = service.entry_needs(&asset, None).unwrap();
    service.prepare(&needs).unwrap();
    for (key, raster) in &tiers {
        let side = tier_side(key.tier).unwrap();
        assert_eq!(raster.width.max(raster.height), side, "{:?}", key.tier);
        assert!(
            raster.rgba == fit_reference(&service, &asset, side).rgba,
            "{:?}: the tier differs from the exact render area-averaged",
            key.tier
        );
    }
    for tier in render(&request, &ReferenceTiles::new(), &Cancel::new()).unwrap() {
        println!(
            "nikon_z6.NEF: {} tier {}×{}, {} bytes",
            tier.key.tier.as_str(),
            tier.width,
            tier.height,
            tier.jpeg.len()
        );
    }
    assert_eq!(sha256(&path), before, "nikon_z6.NEF changed");
}

/// Every tier `request` renders through `tiles`, as its raster, with the renderer that drew them.
fn drawn(
    request: &RenderRequest,
    tiles: &dyn TileService,
) -> (Vec<(RenderedKey, Raster)>, RenderedBy) {
    let mut by = None;
    let tiers = render_with(
        request,
        tiles,
        &Cancel::new(),
        prepare,
        |key, raster, renderer| {
            by = Some(renderer.clone());
            Ok((key, raster))
        },
    )
    .unwrap();
    (tiers, by.expect("a tier"))
}

/// The renderer `{record: reference, reason}`, as a tier records it.
fn reference_for(reason: Option<&str>) -> RenderedBy {
    RenderedBy {
        record: crate::RendererRecord::Reference,
        reason: reason.map(str::to_owned),
    }
}

/// A tier the tile service draws is its bands area-averaged as they arrive: streamed in bands of
/// one row, of seven, of 64 and of the whole stage, the reference's own frame is the reference's
/// tiers byte for byte, at a grid and a large tier that are both reduced and at a large tier the
/// stage already fits; and the tiers name the GPU, as their preview does. A stream whose codes
/// differ from the reference's draws tiers that differ, so the tiers are the stream's pixels.
#[test]
fn a_tier_the_tile_service_draws_is_its_bands_reduced_and_names_the_gpu() {
    for (label, width, height) in [
        ("rendered-bands", 2600, 1700),
        ("rendered-bands-fit", 1200, 800),
    ] {
        let path = generated_jpeg(label, width, height);
        let (service, asset) = edited(label, &path);
        let request = plan_render(&service, &asset, None, &BOTH).unwrap();
        let (reference, by) = drawn(&request, &ReferenceTiles::new());
        assert_eq!(
            by,
            reference_for(None),
            "a host with no GPU provider names no reason"
        );
        for rows in [1, 7, 64, height] {
            let tiles = BandTiles::new(Draw::Bands(rows));
            let (tiers, by) = drawn(&request, tiles.as_ref());
            assert_eq!(by, RenderedBy::gpu());
            assert_eq!(tiles.streams(), 1, "one stream for both tiers");
            for ((key, tier), (_, expected)) in tiers.iter().zip(&reference) {
                assert_eq!((tier.width, tier.height), (expected.width, expected.height));
                assert!(
                    tier.rgba == expected.rgba,
                    "{label}: the {} tier in bands of {rows}",
                    key.tier.as_str()
                );
            }
        }
        let marked = BandTiles::new(Draw::Marked(64));
        let (tiers, _) = drawn(&request, marked.as_ref());
        for ((key, tier), (_, expected)) in tiers.iter().zip(&reference) {
            assert!(tier.rgba != expected.rgba, "{label}: {:?}", key.tier);
        }
        let streamed = BandTiles::new(Draw::Bands(64));
        for tier in render(&request, streamed.as_ref(), &Cancel::new()).unwrap() {
            assert_eq!(tier.renderer, RenderedBy::gpu());
            let info = tier.info(PathBuf::from("/c.index/previews/t.jpg"));
            assert_eq!(info.renderer, Some(RenderedBy::gpu()));
            assert!(!info.approximate);
            let json = serde_json::to_value(&info).unwrap();
            assert_eq!(json["renderer"], json!({"record": "gpu", "reason": null}));
        }
        let sides: Vec<_> = reference
            .iter()
            .map(|(_, raster)| (raster.width, raster.height))
            .collect();
        println!("{label}: tiers {sides:?}");
    }
}

/// A service that cannot draw the stack, at once or part-way, has the reference draw every tier
/// from nothing, naming why: refused, no adapter and a surface still pending before the stream,
/// the budget, and a device lost after three bands of a stream whose codes differ from the
/// reference's, of which nothing is kept. Every tier is then the reference's, byte for byte.
#[test]
fn a_service_that_cannot_draw_has_the_reference_draw_every_tier_naming_why() {
    let path = generated_jpeg("rendered-fallback", 2600, 1700);
    let (service, asset) = edited("rendered-fallback", &path);
    let request = plan_render(&service, &asset, None, &BOTH).unwrap();
    let (reference, _) = drawn(&request, &ReferenceTiles::new());
    let unavailable = |reason| Draw::Refuse(TileFallback::Unavailable(reason));
    for (draw, reason) in [
        (unavailable(TileUnavailable::Refused), "refused"),
        (unavailable(TileUnavailable::NoAdapter), "no-adapter"),
        (unavailable(TileUnavailable::Pending), "surface-pending"),
        (
            Draw::Refuse(TileFallback::Budget {
                requested: 2,
                budget: 1,
            }),
            "tiles-budget",
        ),
        (
            Draw::StopAfter {
                rows: 64,
                bands: 3,
                fallback: TileFallback::Unavailable(TileUnavailable::DeviceLost),
            },
            "device-lost",
        ),
    ] {
        let tiles = BandTiles::new(draw);
        let (tiers, by) = drawn(&request, tiles.as_ref());
        assert_eq!(by, reference_for(Some(reason)));
        for ((key, tier), (_, expected)) in tiers.iter().zip(&reference) {
            assert!(tier.rgba == expected.rgba, "{reason}: {:?}", key.tier);
        }
    }
}

/// A render cancelled while its stream is drawn stops and answers `cancelled`: no tier is
/// finished, and the stream ends between its bands.
#[test]
fn a_render_cancelled_inside_its_stream_answers_cancelled() {
    let path = generated_jpeg("rendered-stream-cancel", 1200, 800);
    let (service, asset) = edited("rendered-stream-cancel", &path);
    let request = Arc::new(plan_render(&service, &asset, None, &BOTH).unwrap());
    let tiles = BandTiles::new(Draw::Bands(8));
    tiles.gate.shut();
    let cancel = Cancel::new();
    let finished = Arc::new(AtomicUsize::new(0));
    let render = {
        let (request, tiles, cancel, finished) = (
            Arc::clone(&request),
            Arc::clone(&tiles),
            cancel.clone(),
            Arc::clone(&finished),
        );
        thread::spawn(move || {
            render_with(&request, tiles.as_ref(), &cancel, prepare, |_, _, _| {
                finished.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        })
    };
    tiles.gate.wait_reached(1, "the stream");
    cancel.cancel();
    tiles.gate.open();
    let error = render.join().unwrap().unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled, "{error}");
    assert_eq!(finished.load(Ordering::SeqCst), 0, "no tier finished");
    assert_eq!(tiles.sent(), 0, "the stream ended before its first band");
}
