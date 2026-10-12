//! A derived photograph (an HDR merge's result, `docs/design/hdr-merge.md`) in the desktop: opened
//! in Develop as any photograph of the catalog is, its frame drawn from its stored source with no
//! original file, offered what a JPEG is and nothing of a RAW's; and its catalog tiers drawn by the
//! GPU tile worker on this host's adapter within the display limit of the reference's tiers.
//!
//! A GPU test with no adapter prints that it was skipped and asserts nothing: it is not GPU
//! evidence.
use super::{
    Boot, Editor,
    gpu_tiles::GpuTiles,
    gpu_tiles_tests::host_adapter,
    gpu_tiles_tier_tests::against,
    message::{Message, preview::PreviewMessage, sync::SyncMessage},
    tasks,
    testing::descriptors,
};
use crate::Config;
use luxforge_core::{
    AssetId, EditorService, Mutation, RendererRecord, SourceTag, catalog_types::RenderedBy,
    qualification, tiles::ReferenceTiles,
};
use luxforge_reference::preview_error::{Class, verdict};
use luxforge_testbase::{paths, wait_until};
use serde_json::json;
use std::{path::PathBuf, sync::atomic::Ordering};

/// A catalog in a directory of its own holding a JPEG and a derived photograph of the corpus's
/// synthetic planes at `width` × `height`, published as a merge publishes its result.
fn published(name: &str, width: u32, height: u32) -> (PathBuf, PathBuf, AssetId) {
    let dir = paths::temp_dir(&format!("derived-{name}"));
    let anchor = dir.join("anchor.jpg");
    std::fs::copy(paths::jpeg(), &anchor).unwrap();
    let catalog = dir.join("catalog.sqlite");
    let asset = qualification::publish_derived(
        &catalog,
        &anchor,
        width,
        height,
        &luxforge_testkit::derived::planes(width, height),
    )
    .unwrap();
    (dir, catalog, asset)
}

/// A derived photograph opens in Develop through the open every photograph of the catalog takes
/// (`source.prepare`, `job.adopt` and the reads an open makes), with no original file: its frame
/// is drawn and presented, nothing is refused, and its sections are a JPEG's, with no RAW
/// development or look.
#[test]
fn slow_a_derived_source_photograph_opens_in_develop() {
    let (dir, catalog, asset) = published("develop", 1600, 1000);
    let (owner, join) = luxforge_core::OwnerHandle::start(&catalog).unwrap();
    let (mut editor, _) = Editor::new(Boot {
        owner: owner.clone(),
        join,
        live_server: None,
        config: Config::default(),
        client: None,
        initial_import: None,
        window: (1440.0, 900.0),
    });
    let _ = editor.update(Message::Sync(SyncMessage::ModulesLoaded(Ok(descriptors()))));
    let generation = editor.open_generation.load(Ordering::Acquire) + 1;
    editor.open_generation.store(generation, Ordering::Release);
    let opened = tasks::open_photograph(
        &owner,
        editor.client,
        &asset,
        generation,
        &editor.open_generation,
        editor.drawn(),
    );
    assert!(opened.is_ok(), "{:?}", opened.as_ref().err());
    let _ = editor.update(Message::Sync(SyncMessage::ImportRefreshed(
        generation,
        opened.map(Box::new),
    )));
    wait_until("the derived photograph's frame on screen", || {
        let _ = editor.update(Message::Preview(PreviewMessage::Poll));
        editor.presentation.presented_entry.is_some() && !editor.presentation.queue.is_busy()
    });
    let state = editor.document.state.as_ref().expect("an open photograph");
    assert_eq!(state.asset.id, asset);
    assert_eq!(state.asset.source.tag(), SourceTag::Derived);
    assert_eq!((state.asset.width, state.asset.height), (1600, 1000));
    let snapshot = editor.snapshot();
    assert_eq!(
        snapshot["render_error"],
        serde_json::Value::Null,
        "{snapshot}"
    );
    let sections: Vec<String> = editor
        .workspace
        .tools
        .all()
        .map(|section| section.module_id.clone())
        .collect();
    assert!(
        sections.iter().any(|id| id == "luxforge.basic"),
        "{sections:?}"
    );
    for raw_only in ["luxforge.raw", "luxforge.look"] {
        assert!(
            !sections.iter().any(|id| id == raw_only),
            "{raw_only} on a derived photograph: {sections:?}"
        );
    }
    owner.stop();
    editor.owner_join.take().unwrap().join().unwrap();
    drop(editor);
    let _ = std::fs::remove_dir_all(dir);
}

/// A derived photograph's grid and large tiers, drawn by the GPU tile worker from its stored
/// source, are each within its class's display limit of the reference's tiers of the same entry,
/// before encoding, over every pixel: after Basic with a relative white balance and its headroom
/// brought down (pointwise), and after Presence and Detail (spatial). The planes hold values below
/// zero and up to four times diffuse white.
#[test]
fn slow_a_derived_source_gpu_tier_is_within_the_display_limit_of_the_reference_tier() {
    let test = "slow_a_derived_source_gpu_tier_is_within_the_display_limit_of_the_reference_tier";
    let Some(adapter) = host_adapter(test) else {
        return;
    };
    let (dir, catalog, asset) = published("tiers", 2600, 1700);
    let mut service = EditorService::open(&catalog).unwrap();
    let worker = GpuTiles::new(Some(adapter), false);
    let mut revision = service.state(&asset).unwrap().revision;
    for (action, params, class) in [
        (
            "set-basic",
            json!({"exposure": -1.2, "contrast": 20, "highlights": -30, "temperature": 20,
                   "tint": -8, "saturation": 10}),
            Class::Pointwise,
        ),
        (
            "set-presence",
            json!({"texture": 30, "clarity": 25, "dehaze": 15}),
            Class::Spatial,
        ),
        (
            "set-detail",
            json!({"sharpening": 40, "luminance": 25}),
            Class::Spatial,
        ),
    ] {
        revision = service
            .apply_action(
                &asset,
                Mutation {
                    expected_revision: revision,
                    request_id: format!("{action}-{revision}"),
                    actor: "test".into(),
                },
                action,
                params,
            )
            .unwrap()
            .revision;
        let (gpu, drawn) = qualification::photo_tiers(&service, &asset, &worker).unwrap();
        let (reference, by) =
            qualification::photo_tiers(&service, &asset, &ReferenceTiles::new()).unwrap();
        assert_eq!(drawn, RenderedBy::gpu(), "{action}");
        assert_eq!(by.record, RendererRecord::Reference);
        for ((tier, candidate), expected) in ["grid", "large"].iter().zip(&gpu).zip(&reference) {
            let statistics = against(candidate, expected);
            eprintln!(
                "{test}: {action}, the {tier} tier ({}x{}) against the reference's: \
                 {statistics:?}",
                candidate.width, candidate.height
            );
            assert!(
                verdict(&statistics, class).passed(),
                "{action}, {tier}: {statistics:?}"
            );
        }
    }
    let figures = worker.figures();
    assert_eq!(figures.references, 0, "{figures:?}");
    drop(service);
    let _ = std::fs::remove_dir_all(dir);
}
