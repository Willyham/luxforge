//! The open photograph's residency against a real owner: Select's grace period letting go of the
//! desktop's GPU state and the owner's prepared source, and Develop preparing it again through the
//! ordinary preview path with the recipe and history untouched; a frame of an earlier stack never
//! standing in on the way back; memory pressure keeping what Develop draws and letting nothing go
//! during a draft; and the allocator relieved once Develop is idle after heavy work. Each owner
//! call runs as its task would, and its answer is handed back as the task's message.
use super::{
    message::{preview::PreviewMessage, residency::ResidencyMessage, select::SelectMessage},
    residency::{Why, subscription},
    tasks::{call, prepared_preview},
    testing::{
        attach_log, events, finish, hold_slider, idle_workers, logged, real_photo, real_photo_at,
    },
    *,
};
use crate::state::select::Shown;
use luxforge_core::AssetId;
use std::sync::atomic::Ordering;

fn catalog(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "luxforge-residency-{name}-{}.sqlite",
        std::process::id()
    ))
}

/// What `source.inspect` says of the photograph's prepared source.
fn readiness(editor: &Editor, asset: &AssetId) -> Value {
    call(
        &editor.owner,
        editor.client,
        "source.inspect",
        json!({"asset_id": asset}),
    )
    .unwrap()
    .0["readiness"]
        .clone()
}

/// The owner's answer to the release the desktop asked for, handed back as its task's message.
fn answer_release(editor: &mut Editor, asset: &AssetId, why: Why) -> luxforge_core::SourceRelease {
    let release = editor.owner.release_source(asset);
    let generation = editor.open_generation.load(Ordering::Acquire);
    let _ = editor.update(Message::Residency(ResidencyMessage::SourceReleased {
        asset: asset.clone(),
        why,
        generation,
        release,
    }));
    release
}

/// Show Select, and let its grace period pass.
fn select_idle(editor: &mut Editor) {
    let _ = editor.update(Message::Select(SelectMessage::Switch(Shown::Select)));
    assert!(editor.select_shown(), "{}", editor.status.text);
    let showing = editor.residency.showing;
    let _ = editor.update(Message::Residency(ResidencyMessage::SelectIdle(showing)));
}

/// Show Develop again on the open photograph, as the Develop tab does with no folder to read.
fn develop_again(editor: &mut Editor) {
    let _ = editor.show_develop_workspace();
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    assert!(!editor.select_shown());
}

/// Once Select has shown for its grace period, the desktop lets go of the photograph's source,
/// boundary and picture at rest and asks the owner to let go of its prepared source, each logged
/// with why; a timer of an earlier showing changes nothing, and with nothing left to let go there
/// is no timer. Back in Develop the displayed entry's preview prepares the original again through
/// the ordinary preview path and hands the surface its source, with the recipe and history as they
/// were.
#[test]
fn select_idle_lets_the_pixels_go_and_develop_prepares_them_again() {
    let catalog = catalog("select-idle");
    let (mut editor, asset, _) = real_photo(&catalog);
    let before = editor.document.state.clone().expect("a photograph");
    assert!(editor.gpu.source().is_some());
    assert!(editor.gpu.summary()["resident"]["version"].is_u64());
    // On macOS the memory-pressure signal is always subscribed; the grace timer comes on top.
    let pressure = usize::from(cfg!(target_os = "macos"));
    assert_eq!(
        subscription(&editor).units(),
        pressure,
        "no timer in Develop"
    );
    // The open's own heavy work has had its relief.
    idle_workers(&mut editor);
    editor.residency.relieving = false;
    let reliefs = editor.residency.reliefs;
    let log = attach_log(&mut editor);

    let _ = editor.update(Message::Select(SelectMessage::Switch(Shown::Select)));
    assert_eq!(
        subscription(&editor).units(),
        pressure + 1,
        "one grace timer"
    );
    let showing = editor.residency.showing;
    let _ = editor.update(Message::Residency(ResidencyMessage::SelectIdle(
        showing - 1,
    )));
    assert!(editor.gpu.source().is_some(), "an earlier showing's timer");
    let _ = editor.update(Message::Residency(ResidencyMessage::SelectIdle(showing)));
    assert!(editor.gpu.source().is_none());
    assert!(editor.gpu.summary()["resident"].is_null());
    assert_eq!(editor.residency.released.as_ref(), Some(&asset));
    let release = answer_release(&mut editor, &asset, Why::SelectIdle);
    assert!(release.released && release.source_bytes > 0, "{release:?}");
    assert_eq!(readiness(&editor, &asset), "preparation-required");
    assert_eq!(
        subscription(&editor).units(),
        pressure,
        "nothing left to let go"
    );
    assert_eq!(
        editor.residency.reliefs,
        reliefs + 1,
        "the allocator is relieved"
    );
    let records = logged(&mut editor, &log);
    for event in ["gpu_source_released", "gpu_boundary_released"] {
        let released = events(&records, event);
        assert_eq!(released.len(), 1, "{event}: {records:?}");
        assert_eq!(released[0]["why"], "select-idle");
    }
    let cache = events(&records, "source_cache_released");
    assert_eq!(cache.len(), 1);
    assert_eq!(cache[0]["why"], "select-idle");
    assert_eq!(cache[0]["bytes"], json!(release.source_bytes));

    let log = attach_log(&mut editor);
    develop_again(&mut editor);
    assert!(editor.residency.released.is_none());
    let records = logged(&mut editor, &log);
    let restored = events(&records, "residency_restored");
    assert_eq!(restored.len(), 1, "{records:?}");
    assert_eq!(restored[0]["earlier_frame"], false);
    assert!(editor.presentation.has_picture(), "its own frame stays");
    // The preview re-entry asks for, as its task runs it: the original is prepared again.
    let loaded = prepared_preview(
        &editor.owner,
        editor.client,
        asset.clone(),
        editor.displayed_entry(),
        editor.drawn(),
    );
    let log = attach_log(&mut editor);
    assert!(loaded.is_ok(), "{loaded:?}");
    let _ = editor.update(Message::Residency(ResidencyMessage::Prepared(
        loaded.map(Box::new),
    )));
    let records = logged(&mut editor, &log);
    assert!(
        editor.gpu.source().is_some(),
        "the job hands the source again: {records:?}"
    );
    assert_eq!(readiness(&editor, &asset), "ready");
    let after = editor.document.state.as_ref().expect("still open");
    assert_eq!(
        (after.revision, &after.current_entry.id),
        (before.revision, &before.current_entry.id),
        "the recipe and history are untouched"
    );
    finish(editor, catalog);
}

/// Select draws no photo surface, so the pipeline lets go of a source the surface held whole and
/// whose pixels the desktop had let go. Back in Develop within the grace period, the displayed
/// entry's preview is asked for, and its job hands the surface the pixels again under the same
/// version: otherwise nothing would, and the picture at rest would stop with `source-missing`.
#[test]
fn develop_again_hands_back_a_source_the_pipeline_let_go_in_select() {
    let catalog = catalog("retired");
    let (mut editor, asset, _) = real_photo(&catalog);
    idle_workers(&mut editor);
    let version = editor.gpu.source().expect("the source").version();
    // The surface holds every row: the desktop lets the pixels go.
    let held = editor
        .gpu
        .source()
        .map(|source| luxforge_gpu::SourceFigures {
            version,
            bytes: source.bytes(),
            uploaded: source.bytes(),
            ready: true,
        });
    editor.gpu.source_figures = held;
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    assert!(
        editor
            .gpu
            .source()
            .is_some_and(|source| !source.holds_pixels())
    );
    let _ = editor.update(Message::Select(SelectMessage::Switch(Shown::Select)));
    assert!(editor.select_shown(), "{}", editor.status.text);
    // Select's first frame hands the pipeline no source: it lets it go.
    editor.gpu.source_figures = None;
    let log = attach_log(&mut editor);
    develop_again(&mut editor);
    let records = logged(&mut editor, &log);
    let restored = events(&records, "residency_restored");
    assert_eq!(restored.len(), 1, "{records:?}");
    assert_eq!(restored[0]["why"], "source-retired");
    assert_eq!(restored[0]["earlier_frame"], false);
    assert!(editor.presentation.has_picture(), "its own frame stays");
    // The preview the way back asks for, as its task runs it.
    let loaded = prepared_preview(
        &editor.owner,
        editor.client,
        asset,
        editor.displayed_entry(),
        editor.drawn(),
    );
    assert!(loaded.is_ok(), "{loaded:?}");
    let log = attach_log(&mut editor);
    let _ = editor.update(Message::Residency(ResidencyMessage::Prepared(
        loaded.map(Box::new),
    )));
    let records = logged(&mut editor, &log);
    let handed = events(&records, "gpu_source");
    assert_eq!(handed.len(), 1, "{records:?}");
    assert_eq!(handed[0]["why"], "surface-let-go");
    assert_eq!(handed[0]["version"], json!(version));
    assert!(
        editor
            .gpu
            .source()
            .is_some_and(|source| source.holds_pixels())
    );
    // Shown again with the pixels in hand, Develop asks for nothing more.
    let _ = editor.update(Message::Select(SelectMessage::Switch(Shown::Select)));
    let log = attach_log(&mut editor);
    develop_again(&mut editor);
    let records = logged(&mut editor, &log);
    assert!(
        events(&records, "residency_restored").is_empty(),
        "{records:?}"
    );
    finish(editor, catalog);
}

/// Where the frame on the presenter is an earlier stack's — the GPU's picture having been drawn
/// over it — it is not drawn on the way back: with no cached large preview of the entry held, the
/// canvas is empty until the job's picture arrives.
#[test]
fn an_earlier_frame_is_never_drawn_on_the_way_back() {
    let catalog = catalog("earlier");
    let (mut editor, asset, _) = real_photo(&catalog);
    idle_workers(&mut editor);
    editor.presentation.gpu_presented = Some(editor.presentation.presented_content + 1);
    select_idle(&mut editor);
    answer_release(&mut editor, &asset, Why::SelectIdle);
    let log = attach_log(&mut editor);
    develop_again(&mut editor);
    let records = logged(&mut editor, &log);
    let restored = events(&records, "residency_restored");
    assert_eq!(restored[0]["earlier_frame"], true);
    assert_eq!(restored[0]["cached_preview"], false);
    assert!(
        !editor.presentation.has_picture(),
        "nothing rather than an earlier stack"
    );
    finish(editor, catalog);
}

/// Memory pressure in Develop keeps what the surface draws — the source and the picture at rest —
/// and lets go of the boundary kept between drafts and the owner's prepared source; during a draft
/// it lets nothing go.
#[test]
fn memory_pressure_keeps_what_develop_draws_and_nothing_goes_during_a_draft() {
    let catalog = catalog("pressure");
    let (mut editor, asset, _) = real_photo(&catalog);
    hold_slider(&mut editor, "set-basic", "exposure");
    let log = attach_log(&mut editor);
    let _ = editor.update(Message::Residency(ResidencyMessage::Pressure));
    assert!(editor.gpu.summary()["resident"]["version"].is_u64());
    let records = logged(&mut editor, &log);
    assert_eq!(events(&records, "residency_kept").len(), 1, "{records:?}");
    editor.gesture = None;

    let log = attach_log(&mut editor);
    let _ = editor.update(Message::Residency(ResidencyMessage::Pressure));
    assert!(editor.gpu.source().is_some(), "Develop draws it");
    assert!(editor.gpu.summary()["resident"].is_null());
    assert!(editor.residency.released.is_none(), "nothing to take back");
    let release = answer_release(&mut editor, &asset, Why::MemoryPressure);
    assert!(release.released, "{release:?}");
    assert_eq!(readiness(&editor, &asset), "preparation-required");
    let records = logged(&mut editor, &log);
    assert_eq!(events(&records, "memory_pressure").len(), 1);
    let boundary = events(&records, "gpu_boundary_released");
    assert_eq!(boundary[0]["why"], "memory-pressure");
    assert!(events(&records, "gpu_source_released").is_empty());
    finish(editor, catalog);
}

/// Heavy work marks the allocator for relief, which runs once Develop is idle and not in Select.
#[test]
fn the_allocator_is_relieved_once_develop_is_idle_after_heavy_work() {
    let catalog = catalog("relief");
    let (mut editor, _, _) = real_photo(&catalog);
    idle_workers(&mut editor);
    editor.residency.relieving = false;
    let reliefs = editor.residency.reliefs;
    let _ = editor.update(Message::Select(SelectMessage::Switch(Shown::Select)));
    editor.residency_heavy();
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    assert_eq!(editor.residency.reliefs, reliefs, "not while Select shows");
    develop_again(&mut editor);
    assert_eq!(editor.residency.reliefs, reliefs + 1);
    assert!(!editor.residency.heavy);
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    assert_eq!(editor.residency.reliefs, reliefs + 1, "once per heavy work");
    finish(editor, catalog);
}

/// A diagnostic, not a check: a RAW opened and edited with a masked Presence and two automatic
/// Corrections, each committed stack rendered whole by the reference, then Select's release and
/// the allocator's relief, and Develop's return, with the process's resident memory and its peak
/// at each step and the time back to a presented frame with and without the release. Run with
/// `LUXFORGE_RESIDENCY_RAW=FILE … -- --ignored --nocapture`.
#[test]
#[ignore = "memory diagnostic over a supplied RAW"]
fn residency_memory_diagnostic() {
    use std::time::Instant;
    let Some(raw) = std::env::var_os("LUXFORGE_RESIDENCY_RAW") else {
        eprintln!("LUXFORGE_RESIDENCY_RAW is not set");
        return;
    };
    let memory = |editor: &Editor| {
        let (read, _) = call(&editor.owner, editor.client, "resources.read", json!({})).unwrap();
        (
            read["memory"]["bytes"].as_u64().unwrap_or(0) >> 20,
            read["memory"]["peak_bytes"].as_u64().unwrap_or(0) >> 20,
        )
    };
    let report = |step: &str, editor: &Editor| {
        let (now, peak) = memory(editor);
        println!("residency_memory {step}: resident {now} MiB, peak {peak} MiB");
        // Under a tracer that writes its live set on a signal, one is asked for at each step.
        if std::env::var_os("BIGALLOC_OUT").is_some() {
            let _ = std::process::Command::new("kill")
                .args(["-USR2", &std::process::id().to_string()])
                .status();
        }
    };
    let catalog = catalog("diagnostic");
    let (mut editor, asset, agent) = real_photo_at(&catalog, std::path::Path::new(&raw));
    idle_workers(&mut editor);
    report("opened", &editor);
    let edit = |editor: &mut Editor, method: &str, mut params: Value| {
        let revision = editor.document.state.as_ref().unwrap().revision;
        params["asset_id"] = json!(asset);
        params["mutation"] = json!({"expected_revision": revision,
            "request_id": format!("diagnostic-{revision}"), "actor": "agent"});
        let (answer, _) = call(&editor.owner, agent, method, params).expect("accepted");
        let refreshed = super::tasks::refresh(
            &editor.owner,
            editor.client,
            asset.clone(),
            super::tasks::Scope::Elsewhere,
            editor.drawn(),
        )
        .unwrap();
        let _ = editor.update(Message::Sync(super::message::sync::SyncMessage::Refreshed(
            Ok(Box::new(refreshed)),
        )));
        idle_workers(editor);
        answer
    };
    edit(&mut editor, "edit.set-basic", json!({"exposure": 0.4}));
    let mask = edit(
        &mut editor,
        "mask.create-radial",
        json!({"x": 0.5, "y": 0.5, "radius_x": 0.2, "radius_y": 0.3, "angle": 0.0,
            "feather": 40.0}),
    )["mask"]
        .clone();
    edit(
        &mut editor,
        "edit.set-presence",
        json!({"mask": mask, "texture": 40.0, "clarity": 30.0}),
    );
    report("masked", &editor);
    for (x, y) in [(0.3, 0.4), (0.7, 0.6)] {
        edit(
            &mut editor,
            "edit.add-correction",
            json!({"points": [[x, y], [x + 0.02, y + 0.01]], "radius": 0.02}),
        );
    }
    report("corrected", &editor);

    // Develop again with nothing let go: the time to a presented frame of the displayed entry.
    let back = |editor: &mut Editor| {
        let started = Instant::now();
        let loaded = prepared_preview(
            &editor.owner,
            editor.client,
            asset.clone(),
            editor.displayed_entry(),
            editor.drawn(),
        );
        let _ = editor.update(Message::Residency(ResidencyMessage::Prepared(
            loaded.map(Box::new),
        )));
        idle_workers(editor);
        started.elapsed().as_secs_f64() * 1000.0
    };
    let kept = back(&mut editor);
    report("rendered again, kept", &editor);

    select_idle(&mut editor);
    let release = answer_release(&mut editor, &asset, Why::SelectIdle);
    println!("residency_memory release: {release:?}");
    report("released", &editor);
    let relieved = Instant::now();
    let relief = luxforge_process::memory::relieve_allocator();
    println!(
        "residency_memory relief: {relief:?} in {:.1} ms",
        relieved.elapsed().as_secs_f64() * 1000.0
    );
    report("relieved", &editor);
    let _ = editor.show_develop_workspace();
    let returned = back(&mut editor);
    report("back in Develop", &editor);
    println!(
        "residency_memory back to a presented frame: {kept:.0} ms kept, {returned:.0} ms released"
    );
    finish(editor, catalog);
}

/// A diagnostic, not a check: Select's lanes over a supplied folder — its first look read by the
/// index lane, then every file's grid preview and every RAW's loupe preview read by the preview
/// lane, as browsing it does — with the process's resident memory and its peak after each. Run
/// with `LUXFORGE_RESIDENCY_FOLDER=DIR … -- --ignored --nocapture`.
#[test]
#[ignore = "memory diagnostic over a supplied folder"]
fn select_lanes_memory_diagnostic() {
    let Some(folder) = std::env::var_os("LUXFORGE_RESIDENCY_FOLDER") else {
        eprintln!("LUXFORGE_RESIDENCY_FOLDER is not set");
        return;
    };
    let folder = std::path::PathBuf::from(folder);
    let catalog = catalog("lanes");
    let (owner, join) = luxforge_core::OwnerHandle::start(&catalog).unwrap();
    let client = owner.register();
    let ask = |method: &str, params: Value| call(&owner, client, method, params).unwrap().0;
    let report = |step: &str| {
        let read = ask("resources.read", json!({}));
        println!(
            "select_lanes {step}: resident {} MiB, peak {} MiB",
            read["memory"]["bytes"].as_u64().unwrap_or(0) >> 20,
            read["memory"]["peak_bytes"].as_u64().unwrap_or(0) >> 20
        );
        if std::env::var_os("BIGALLOC_OUT").is_some() {
            let _ = std::process::Command::new("kill")
                .args(["-USR2", &std::process::id().to_string()])
                .status();
        }
    };
    let settled = |job: &Value| {
        luxforge_testbase::wait_for("the job to end", || {
            let record = ask("job.read", json!({"job_id": job}));
            (!matches!(record["status"].as_str(), Some("queued" | "running"))).then_some(record)
        })
    };
    report("started");
    let started = ask(
        "index.refresh",
        json!({"source": {"kind": "folder", "path": folder}}),
    );
    let record = settled(&started["job_id"]);
    println!("select_lanes index: {}", record["status"]);
    report("indexed");
    let summary = ask(
        "browse.view",
        json!({"source": {"kind": "folder", "path": folder, "subfolders": true}}),
    );
    let count = summary["count"].as_u64().unwrap_or(0);
    let rows: Vec<Value> = (0..count.div_ceil(1000))
        .flat_map(|page| {
            ask(
                "browse.rows",
                json!({"from": page * 1000, "count": 1000, "revision": summary["revision"]}),
            )["rows"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .collect();
    println!("select_lanes rows: {}", rows.len());
    let read_tier = |row: &Value, tier: &str| {
        let answer = ask(
            "preview.read",
            json!({"item": {"kind": row["item"], "file_id": row["file_id"]}, "tier": tier,
                "priority": "visible"}),
        );
        if let Some(job) = answer.get("job_id").filter(|job| !job.is_null()) {
            settled(job);
        }
    };
    for row in &rows {
        read_tier(row, "grid");
    }
    report("grid previews");
    for row in rows.iter().filter(|row| row["kind"] == "raw") {
        read_tier(row, "loupe");
    }
    report("loupe previews");
    owner.stop();
    join.join().unwrap();
    let _ = std::fs::remove_file(&catalog);
}
