//! The `no-gpu-render` smoke scenario: the editor launched with `--no-gpu-render`, which refuses the
//! photo surface's GPU stage as a machine whose adapter cannot run it does, so the reference
//! renderer draws everything (`docs/design/gpu-first.md`, "Headless and portability").
//!
//! One launch over the quadrant fixture: the photograph opened, a Basic exposure drag and its
//! release, and the settled frame after it. Every frame records the session's renderer as the
//! reference for `no-adapter`, the surface's stage as refused by the launch, the photograph drawn
//! on the CPU path with no plan handed to the surface and nothing charged to the GPU-preview
//! budget, and the desktop's reason for handing no plan. The status bar says the reference
//! renderer's notice beside its render slot in every frame, the opened and settled ones at rest
//! among them. Every tick of the drag takes the CPU path naming `no-adapter` and asks for no
//! boundary; no dissolve starts, and the desktop reports no other renderer. The adapter that drew
//! the window is identified in every frame: the window is still composited by it, and only the
//! photograph's pixels are the reference renderer's.
use crate::{
    gpu_preview_smoke::{named, step_events},
    scenario::{Checked, Checks, Plan, Run, Step, plan::only},
    *,
};
use luxforge_core::BASIC_EFFECT;
use luxforge_evidence::{self as script, SliderStep};

pub const SCENARIO: &str = "no-gpu-render";
/// Four flat quadrants, as the GPU preview scenario opens.
pub const FIXTURE: &str = "fixtures/s0/orientation-1.jpg";

/// The reference renderer's notice, as the status bar's table words it.
const REFERENCE_PHRASE: &str = "Reference renderer";
const REFERENCE_TOOLTIP: &str = "This graphics device cannot run the GPU renderer, or this launch \
                                 turned it off, so every frame is drawn by the reference renderer \
                                 on the CPU, which is slower.";

const BASIC: &str = "set-basic";
const EXPOSURE: &str = "exposure";
const DRAGGED: [f64; 3] = [0.25, 0.5, 0.75];

/// Every frame, in order: the open, the drag, its release and the frame settled after it.
pub fn plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("opened").no_draft(),
        Step::new("drag", SliderStep::new(BASIC, EXPOSURE, DRAGGED))
            .commits(0)
            .draft(BASIC, json!({ EXPOSURE: DRAGGED[2] })),
        Step::new(
            "release",
            SliderStep::new(BASIC, EXPOSURE, [DRAGGED[2]]).release(),
        )
        .commits(1)
        .no_draft()
        .payload(BASIC_EFFECT, json!({ EXPOSURE: DRAGGED[2] })),
        Step::new("settled", script::Step::Wait { ms: 500 })
            .commits(0)
            .no_draft(),
    ])
}

/// The reference renderer in every frame, the drag on the CPU path tick by tick, and the adapter
/// that drew the window identified.
pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let mut checks = Checks::new();
    let reference = json!({"record": "reference", "reason": "no-adapter"});
    for frame in &launch.frames {
        let state = frame.state();
        let gpu = &state["surface"]["gpu"];
        let bar = &state["status_bar"];
        let backend = &state["backend"];
        checks.note(
            frame,
            "the reference renderer, the GPU stage refused by the launch",
            json!({
                "renderer": state["renderer"],
                "stage": gpu["stage"],
                "drawing_path": gpu["drawing_path"],
                "plan_fallback": gpu["plan_fallback"],
                "gpu_fallback": gpu["gpu_fallback"],
                "drawn_gpu_boundary": gpu["drawn_gpu_boundary"],
                "gpu_preview_passes": gpu["gpu_preview_passes"],
                "gpu_preview_in_use_bytes": gpu["gpu_preview_in_use_bytes"],
                "fallback_notice": gpu["fallback_notice"],
                "render": bar["render"],
                "status_fallback": bar["fallback"],
                "backend": backend,
            }),
        );
        ensure(
            state["renderer"] == reference,
            format!(
                "{}: the session names {} as the renderer, not the reference for no-adapter",
                frame["file"], state["renderer"]
            ),
        )?;
        ensure(
            gpu["stage"] == json!({"state": "no-adapter", "refused": true}),
            format!(
                "{}: the surface's stage is {}, not refused by the launch",
                frame["file"], gpu["stage"]
            ),
        )?;
        ensure(
            gpu["drawing_path"] == json!("cpu")
                && gpu["plan_fallback"] == json!({"reason": "no-adapter"})
                && gpu["gpu_fallback"].is_null()
                && gpu["drawn_gpu_boundary"].is_null()
                && gpu["gpu_preview_passes"] == json!(0)
                && gpu["gpu_preview_in_use_bytes"] == json!(0),
            format!(
                "{}: not the CPU path with no plan handed: path {}, plan fallback {}, surface \
                 fallback {}, GPU boundary {}, {} passes, {} bytes in use",
                frame["file"],
                gpu["drawing_path"],
                gpu["plan_fallback"],
                gpu["gpu_fallback"],
                gpu["drawn_gpu_boundary"],
                gpu["gpu_preview_passes"],
                gpu["gpu_preview_in_use_bytes"]
            ),
        )?;
        ensure(
            bar["fallback"] == json!({"phrase": REFERENCE_PHRASE, "tooltip": REFERENCE_TOOLTIP})
                && bar["fallback"] == gpu["fallback_notice"]
                && bar["gpu_ms"].is_null(),
            format!(
                "{}: the status bar does not say the reference renderer draws: notice {}, \
                 evidence {}, GPU figure {}",
                frame["file"], bar["fallback"], gpu["fallback_notice"], bar["gpu_ms"]
            ),
        )?;
        ensure(
            backend["device_type"]
                .as_str()
                .is_some_and(|kind| !kind.is_empty()),
            format!(
                "{}: the adapter that drew the window is not identified: {backend}",
                frame["file"]
            ),
        )?;
    }

    // The drag: every tick on the CPU naming the stage's reason, no source held and no boundary
    // derived, no dissolve at its release, and no renderer reported other than the launch's.
    let events = step_events(launch, "drag")?;
    let ticks = named(events, "gpu_preview_tick");
    ensure(
        !ticks.is_empty()
            && ticks.iter().all(|tick| {
                tick["detail"]["path"] == json!("cpu")
                    && tick["detail"]["reason"] == json!("no-adapter")
            }),
        format!(
            "The drag's ticks were not the CPU's naming no-adapter: {:?}",
            ticks.iter().map(|tick| &tick["detail"]).collect::<Vec<_>>()
        ),
    )?;
    for name in [
        "gpu_source",
        "gpu_boundary",
        "gpu_boundary_resident",
        "gpu_dissolve_started",
        "renderer_reported",
    ] {
        let found = named(&launch.events, name);
        ensure(
            found.is_empty(),
            format!("A launch with no GPU stage logged {name}: {found:?}"),
        )?;
    }
    let drag = launch.at("drag")?;
    checks.note(
        drag,
        "every tick of the drag on the CPU path, naming no-adapter",
        json!({"ticks": ticks.len(), "drag": drag.state()["surface"]["gpu"]["gpu_preview"]["drag"]}),
    );
    checks.write(
        &launch.evidence,
        run.scenario(),
        json!({"renderer": reference}),
    )
}
