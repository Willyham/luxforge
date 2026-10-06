//! The reference renderer as the fallback of a desktop whose GPU stage cannot draw
//! (`docs/design/gpu-first.md`, "Headless and portability"), end to end against a real owner and
//! photograph: a launch that refused the stage (`--no-gpu-render`) and a surface whose stage cannot
//! run or whose device is lost hand the surface no plan, draw every tick from the CPU path naming
//! the stage's reason, and report the reference renderer with that reason in every client's
//! session; the status bar says so beside its render slot.
//!
//! No unit test has a pipeline to check a device, so a test stands in for the stage's answer as
//! the surface publishes it (`renderer.stage`); the photo surface's own tests prove that a refused
//! pipeline answers `no-adapter` and draws the CPU frame on a real device.
use super::{
    gpu_preview::SurfaceReport,
    gpu_preview_tests::{catalog, deliver_until, surface_ready},
    message::{draft::DraftMessage, preview::PreviewMessage, renderer::RendererMessage},
    tasks::call,
    testing::{attach_log, events, finish, let_go, logged, real_photo_launched, run_commit, slide},
    *,
};
use crate::state::status::renderer_notice;
use luxforge_core::Renderer;
use luxforge_ui::photo_surface::GpuStageState;

const ACTION: &str = "set-basic";
const FIELD: &str = "exposure";

fn fixture() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg")
}

/// The renderer `client`'s `session.state` reports, as an independent JSON client reads it.
fn session_renderer(editor: &Editor, client: ClientId) -> Value {
    call(&editor.owner, client, "session.state", json!({}))
        .expect("session.state answers")
        .0["renderer"]
        .clone()
}

/// The next update finds the surface's answer changed and starts a report: run it through the
/// owner as its task does and hand the answer back as the runtime does.
pub(super) fn report(editor: &mut Editor) {
    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    assert!(editor.renderer.in_flight(), "a report on its way");
    let renderer = editor.renderer.reported();
    assert_eq!(renderer, editor.renderer_now());
    let answer = editor
        .owner
        .report_renderer(editor.client, renderer)
        .map(Box::new)
        .map_err(|error| error.to_string());
    let _ = editor.update(Message::Renderer(RendererMessage::Reported(answer)));
    assert!(!editor.renderer.in_flight());
}

/// `--no-gpu-render`: the owner reports the reference renderer for `no-adapter` from the start, to
/// every client, before the surface has checked its stage and after it answers as a refused stage
/// does, so the desktop never reports another. No plan reaches the surface and no boundary is asked
/// for: every tick of a drag takes the CPU path naming `no-adapter`, as the snapshot records, and
/// the release commits on the CPU. The status bar says so beside its render slot in the reference
/// renderer's words, read from the session, at rest as well as during the drag. No request lets an
/// agent claim another renderer.
#[test]
fn fallback_a_forced_no_gpu_launch_is_the_reference_in_the_session_and_every_tick_is_the_cpus() {
    let catalog = catalog("fallback-forced");
    let config = crate::Config {
        no_gpu_render: true,
        ..crate::Config::default()
    };
    let (mut editor, _, agent) = real_photo_launched(&catalog, &fixture(), config);
    let forced = json!({"record": "reference", "reason": "no-adapter"});
    assert_eq!(editor.gpu_stage(), GpuStageState::Unchecked);
    assert_eq!(editor.gpu_preview_allowed(), Err("no-adapter"));
    assert_eq!(session_renderer(&editor, agent), forced);
    assert_eq!(session_renderer(&editor, editor.client), forced);
    // The refused stage's own answer once a photograph is drawn is the same renderer.
    editor.renderer.stage = Some(GpuStageState::NoAdapter { refused: true });
    deliver_until(&mut editor, "the first frame", |editor| {
        editor.presentation.dimensions.is_some() && !editor.presentation.queue.is_busy()
    });
    assert!(!editor.renderer.in_flight(), "nothing new to report");
    assert_eq!(editor.renderer.reported(), editor.renderer_now());
    assert_eq!(
        serde_json::to_value(editor.session.renderer).unwrap(),
        forced
    );

    let notice = renderer_notice(Renderer::reference(
        luxforge_core::RendererReason::NoAdapter,
    ));
    assert!(notice.is_some(), "the reference renderer's class says it");
    assert_eq!(
        editor.workspace.status.fallback, notice,
        "at rest, before any gesture"
    );
    for value in [0.1, 0.2, 0.3] {
        let _ = slide(&mut editor, ACTION, FIELD, value);
        assert_eq!(editor.gpu.summary()["drag"]["reason"], json!("no-adapter"));
        assert!(
            editor
                .gpu_plan(editor.presentation.presenter.photo())
                .is_none(),
            "no plan reaches the surface"
        );
        assert_eq!(editor.workspace.status.fallback, notice);
    }
    assert_eq!(
        editor.gpu.ticks(),
        (0, 3, 0),
        "every tick on the CPU, no boundary asked for"
    );
    let snapshot = editor.snapshot();
    assert_eq!(snapshot["renderer"], forced);
    assert_eq!(
        snapshot["surface"]["gpu"]["plan_fallback"],
        json!({"reason": "no-adapter"})
    );
    assert_eq!(
        snapshot["surface"]["gpu"]["stage"],
        json!({"state": "no-adapter", "refused": true})
    );
    assert!(!snapshot["status_bar"]["fallback"].is_null());
    assert_eq!(
        snapshot["status_bar"]["fallback"],
        snapshot["surface"]["gpu"]["fallback_notice"]
    );

    // The release commits on the CPU, and the frame that replaces the drafted one is the CPU's.
    let _ = let_go(&mut editor, ACTION, FIELD);
    assert!(run_commit(&mut editor));
    deliver_until(&mut editor, "the committed frame", |editor| {
        !editor.gpu.has_drag()
    });
    let at_rest = editor.snapshot();
    assert_eq!(
        at_rest["stack"]["layers"][0]["effect"],
        json!(luxforge_core::BASIC_EFFECT),
        "the release committed"
    );
    assert_eq!(
        at_rest["surface"]["gpu"]["plan_fallback"],
        json!({"reason": "no-adapter"})
    );
    assert_eq!(editor.workspace.status.fallback, notice, "said at rest too");

    // An agent cannot claim another renderer.
    let claim = call(
        &editor.owner,
        agent,
        "workspace.set",
        json!({"renderer": {"record": "gpu", "reason": null}}),
    );
    assert!(claim.is_err(), "{claim:?}");
    assert_eq!(session_renderer(&editor, agent), forced);
    finish(editor, catalog);
}

/// An ordinary launch: the owner reports `surface-pending` until the surface has checked its stage,
/// which is no reason to refuse a plan; the surface's answer is reported to every client as it
/// comes — the GPU — and as it changes — the device lost, when the gate refuses with that reason and
/// evidence names it. A report goes only when the answer changes.
#[test]
fn fallback_the_surfaces_answer_reaches_every_session_as_it_comes_and_changes() {
    let catalog = catalog("fallback-reported");
    let (mut editor, _, agent) =
        real_photo_launched(&catalog, &fixture(), crate::Config::default());
    assert_eq!(
        session_renderer(&editor, agent),
        json!({"record": "reference", "reason": "surface-pending"})
    );
    assert_eq!(editor.gpu_preview_allowed(), Ok(()));

    editor.renderer.stage = Some(GpuStageState::Available);
    report(&mut editor);
    let gpu = json!({"record": "gpu", "reason": null});
    assert_eq!(session_renderer(&editor, agent), gpu);
    assert_eq!(editor.session.renderer, Renderer::gpu());
    assert_eq!(editor.snapshot()["renderer"], gpu);
    assert_eq!(editor.gpu_preview_allowed(), Ok(()));

    editor.renderer.stage = Some(GpuStageState::DeviceLost);
    report(&mut editor);
    let lost = json!({"record": "reference", "reason": "device-lost"});
    assert_eq!(session_renderer(&editor, agent), lost);
    assert_eq!(editor.snapshot()["renderer"], lost);
    assert_eq!(editor.gpu_preview_allowed(), Err("device-lost"));
    assert_eq!(
        editor.snapshot()["surface"]["gpu"]["plan_fallback"],
        json!({"reason": "device-lost"})
    );

    let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    assert!(!editor.renderer.in_flight(), "no change, no report");
    finish(editor, catalog);
}

/// A device that cannot run the stage on an ordinary launch: the reference for `no-adapter`, as
/// the forced launch is, but not refused by the launch.
#[test]
fn fallback_a_device_that_cannot_run_the_stage_is_the_reference_for_no_adapter() {
    let catalog = catalog("fallback-incapable");
    let (mut editor, _, agent) =
        real_photo_launched(&catalog, &fixture(), crate::Config::default());
    editor.renderer.stage = Some(GpuStageState::NoAdapter { refused: false });
    report(&mut editor);
    assert_eq!(
        session_renderer(&editor, agent),
        json!({"record": "reference", "reason": "no-adapter"})
    );
    assert_eq!(editor.gpu_preview_allowed(), Err("no-adapter"));
    assert_eq!(
        editor.snapshot()["surface"]["gpu"]["stage"],
        json!({"state": "no-adapter", "refused": false})
    );
    finish(editor, catalog);
}

/// A device lost in the middle of a gesture: the gate refuses the next tick with `device-lost`, and
/// the boundary the drag held for its GPU ticks is let go, because nothing will draw from it
/// again. The release names the gate's reason, and the
/// drag's later ticks ask for no boundary and hand the surface no plan.
#[test]
fn fallback_a_device_lost_mid_gesture_lets_the_held_boundary_go() {
    let catalog = catalog("fallback-lost-boundary");
    let (mut editor, _, _) = real_photo_launched(&catalog, &fixture(), crate::Config::default());
    editor.gpu.surface = Some(SurfaceReport::default());
    let _ = slide(&mut editor, ACTION, FIELD, 0.1);
    deliver_until(&mut editor, "the boundary", |editor| {
        editor.gpu.holds_boundary()
    });
    surface_ready(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.2);
    assert_eq!(
        editor.gpu.ticks().0,
        1,
        "a tick on the GPU over the held boundary"
    );
    let held = editor.gpu.held_version().expect("a held boundary");

    editor.renderer.stage = Some(GpuStageState::DeviceLost);
    let log = attach_log(&mut editor);
    let _ = slide(&mut editor, ACTION, FIELD, 0.3);
    let records = logged(&mut editor, &log);
    let released = events(&records, "gpu_boundary_released");
    assert_eq!(released.len(), 1, "{released:?}");
    assert_eq!(released[0]["why"], "device-lost");
    assert_eq!(released[0]["version"], json!(held));
    assert!(
        !editor.gpu.holds_boundary(),
        "nothing held for a stage that cannot draw"
    );
    assert_eq!(editor.gpu.summary()["drag"]["reason"], json!("device-lost"));
    let asked = editor.gpu.ticks().2;
    let _ = slide(&mut editor, ACTION, FIELD, 0.4);
    assert_eq!(
        editor.gpu.ticks().2,
        asked,
        "no boundary is asked for again"
    );
    assert!(
        editor
            .gpu_plan(editor.presentation.presenter.photo())
            .is_none()
    );
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    finish(editor, catalog);
}

/// The desktop names the adapter its window draws with to its GPU tile worker once the photo
/// surface has checked its GPU stage, and once: until then the worker answers the reference as
/// `surface-pending`. A launch that is not an evidence run asks Iced for the adapter's name only
/// then, and the worker takes Iced's answer; an evidence run's name, from the system information it
/// asked for at launch, is taken the same way once the stage is checked. A later name changes
/// nothing, and a launch that refused the stage names nothing.
#[test]
fn the_tile_worker_is_named_the_windows_adapter_once_the_stage_is_checked() {
    use super::{gpu_tiles::GpuTiles, renderer::WindowAdapter};
    use luxforge_core::tiles::{TileFallback, TileService, TileStatus, TileUnavailable};
    let pending = TileStatus::Reference(Some(TileFallback::Unavailable(TileUnavailable::Pending)));
    let poll = |editor: &mut Editor| {
        let _ = editor.update(Message::Preview(PreviewMessage::Poll));
    };
    let catalog = catalog("tile-adapter");
    let (mut editor, _, _) = real_photo_launched(&catalog, &fixture(), crate::Config::default());
    let worker = Arc::new(GpuTiles::pending(false));
    editor.renderer.tiles = Some(Arc::clone(&worker));
    editor.renderer.stage = Some(GpuStageState::Unchecked);
    poll(&mut editor);
    assert_eq!(
        editor.renderer.adapter(),
        &WindowAdapter::Unknown,
        "not yet"
    );
    assert_eq!(worker.status(), pending);
    editor.renderer.stage = Some(GpuStageState::Available);
    poll(&mut editor);
    assert_eq!(editor.renderer.adapter(), &WindowAdapter::Asked);
    poll(&mut editor);
    assert_eq!(
        editor.renderer.adapter(),
        &WindowAdapter::Asked,
        "asked once"
    );
    assert_eq!(worker.status(), pending, "until Iced answers");
    let named = |backend: &str, name: &str| {
        Message::Renderer(RendererMessage::Adapter {
            backend: backend.into(),
            name: name.into(),
        })
    };
    let _ = editor.update(named("Metal", "Apple M4 Pro"));
    assert_eq!(editor.renderer.adapter(), &WindowAdapter::Told);
    assert_eq!(worker.status(), TileStatus::Gpu);
    let _ = editor.update(named("Vulkan", "llvmpipe"));
    assert_eq!(
        editor.renderer.adapter(),
        &WindowAdapter::Told,
        "named once"
    );
    assert!(!worker.started(), "nothing opened until an export asks");
    finish(editor, catalog);

    // An evidence run's name, which comes with its launch, before the stage is checked.
    let catalog = super::gpu_preview_tests::catalog("tile-adapter-named");
    let (mut editor, _, _) = real_photo_launched(&catalog, &fixture(), crate::Config::default());
    let worker = Arc::new(GpuTiles::pending(false));
    editor.renderer.tiles = Some(Arc::clone(&worker));
    editor.renderer.stage = Some(GpuStageState::Unchecked);
    editor.window_adapter_named("Metal".into(), "Apple M4 Pro".into());
    poll(&mut editor);
    assert_eq!(worker.status(), pending, "not before the stage is checked");
    editor.renderer.stage = Some(GpuStageState::NoAdapter { refused: false });
    poll(&mut editor);
    assert_eq!(
        editor.renderer.adapter(),
        &WindowAdapter::Told,
        "asked nothing"
    );
    assert_eq!(worker.status(), TileStatus::Gpu, "its own device decides");
    finish(editor, catalog);

    // A launch that refused the stage names nothing, whatever it hears.
    let catalog = super::gpu_preview_tests::catalog("tile-adapter-refused");
    let config = crate::Config {
        no_gpu_render: true,
        ..crate::Config::default()
    };
    let (mut editor, _, _) = real_photo_launched(&catalog, &fixture(), config);
    let worker = Arc::new(GpuTiles::pending(true));
    editor.renderer.tiles = Some(Arc::clone(&worker));
    editor.renderer.stage = Some(GpuStageState::NoAdapter { refused: true });
    let _ = editor.update(named("Metal", "Apple M4 Pro"));
    poll(&mut editor);
    assert_ne!(editor.renderer.adapter(), &WindowAdapter::Told);
    assert_eq!(
        worker.status(),
        TileStatus::Reference(Some(TileFallback::Unavailable(TileUnavailable::Refused)))
    );
    finish(editor, catalog);
}
