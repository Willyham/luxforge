//! The `gpu-preview` smoke scenario: gestures at Fit drawn on the GPU over a held boundary, with
//! no preview job per tick, on the real editor (`docs/design/gpu-preview.md`, "A tick").
//!
//! One launch over the quadrant fixture. The photograph's own job hands the photo surface its
//! source and derives from it on the GPU the resident boundary, a cut of the source at Fit's exact
//! stage. A Basic exposure drag over the bare photograph starts from that boundary, deriving none:
//! its first tick is drawn on the CPU only until the surface has evaluated its plan and compiled
//! its program sequence, and its later ticks are drawn on the GPU, each frame tagged with its
//! tick's draft revision; its release commits, and once the committed frame has settled the
//! boundary stays on the GPU as the resident one, which a later gesture over the photograph's own
//! input draws from at its first tick. A Detail Amount drag draws from it, the Detail layer's input being the
//! photograph's, its ticks Detail's spatial step; the photograph fits the window at its own size,
//! so its Fit frame is the exact render and its release settles to exact pixels. Then, in Mask
//! mode, a linear gradient bound to a masked exposure is moved by its middle handle in two drags,
//! drawn on the GPU with the coverage overlay following it, and a brush stroke is painted through
//! the same mask, its positions drawn on the GPU. Last, back in the pointer mode, Presence is
//! committed with Dehaze and Clarity, and a Texture drag, a Clarity drag and a Basic drag under
//! Presence are each drawn on the GPU, every one reading Dehaze's light from its light link over
//! the source: the two Presence drags the light at rest, running at most five of Presence's compute
//! passes a tick (the spatial passes the tick's words change), and the Basic drag, which changes
//! the light's input, a light computed every tick, running them all.
//!
//! **Correlated readbacks.** Every frame drawn on the GPU is checked against the state the editor
//! recorded with it — the drawing path, the boundary version and the draft revision the surface
//! drew — and its pixels against the CPU's frame of the same settings: each drag's last value
//! against the frame its release commits, and the moved gradient, under the coverage tint, against
//! the frame its Apply commits, patch by patch.//!
//! **The settle hand-off.** Each commit that replaces a GPU frame dissolves to it from that
//! frame's own draft revision and boundary, as the editor records; the drag's GPU frame against
//! the settled frame that replaced it is held to the pointwise limits over the whole photograph,
//! and an `idle` step after the release's and the stroke's dissolves checks that nothing draws or
//! updates once they have ended. With the GPU preview turned off from the palette a drag takes the
//! CPU path, naming the preference, and its release starts no dissolve. With both clipping
//! overlays shown a drag is still drawn on the GPU, marking its own clipped pixels. Turning them
//! off, and the Detail drag after a third drag's release, each cancel a release's dissolve that
//! still runs when they take effect; one that had already ended, as a release capture longer than
//! 150 ms on a loaded host leaves it, is recorded with both times and passes.
use crate::{
    scenario::{Checked, Checks, Frame, Plan, Run, Step, plan::only},
    *,
};
use luxforge_core::{BASIC_EFFECT, DETAIL_EFFECT};
use luxforge_evidence::{
    self as script, DragHandle, MaskStep, PaintStep, PaletteStep, Reference, SliderStep,
    WorkspaceStep,
};

pub const SCENARIO: &str = "gpu-preview";
/// Four flat quadrants: every patch below reads one colour, which a drag moves by a known amount.
pub const FIXTURE: &str = "fixtures/s0/orientation-1.jpg";

pub(crate) const BASIC: &str = "set-basic";
pub(crate) const EXPOSURE: &str = "exposure";
pub(crate) const PRESENCE: &str = "set-presence";
/// The committed Presence the drags start from, then each Presence drag's first tick and its GPU
/// ticks, one a step, so each GPU step's frame counts one tick's compute passes.
pub(crate) const DEHAZE: f64 = 40.0;
pub(crate) const CLARITY: f64 = 30.0;
pub(crate) const TEXTURE_DRAG: [f64; 3] = [20.0, 35.0, 50.0];
pub(crate) const CLARITY_DRAG: [f64; 3] = [40.0, 50.0, 60.0];
/// The Basic drag under Presence, from the committed exposure.
pub(crate) const UNDER_DRAG: [f64; 3] = [0.5, 0.4, 0.3];
/// The most compute passes a tick that changes only Texture's or Clarity's gain may run: Clarity's
/// five, which read Texture's output.
const GAIN_PASSES: u64 = 5;
/// The drag's values: its first tick, before any boundary is held, then its GPU ticks.
const FIRST: f64 = 0.25;
const DRAGGED: [f64; 2] = [0.5, 0.75];
const MASKED: f64 = 1.0;
const DETAIL: &str = "set-detail";
const AMOUNT: &str = "sharpening";
/// The Detail drag's values: its first tick, then its GPU ticks.
const DETAIL_FIRST: f64 = 40.0;
const DETAIL_DRAGGED: [f64; 2] = [70.0, 100.0];
/// Patches across the white centre cross, where sharpening acts, beside the flat quadrants'.
const EDGES: [[f64; 2]; 2] = [[0.5, 0.25], [0.25, 0.5]];
/// How long the scenario leaves the editor alone for the surface to evaluate a plan and its
/// sequence to compile, and for a committed frame to settle. Generous: a 320 × 480 boundary is
/// derived in one pass and a sequence compiles in tens of milliseconds.
const QUIET_MS: u64 = 1500;
/// How long the 100% scenario leaves the editor alone once Presence is committed, and while a
/// drag's region plan is evaluated and its sequence compiles: a percentage view's plans are not
/// warmed.
pub(crate) const PRESENCE_QUIET_MS: u64 = 4000;
/// The most the scenario waits, after a commit's quiet, for the committed stack's warm list to
/// compile before the next Presence drag begins: each drag of a Presence layer or of a layer under
/// it compiles its own Presence passes, about a second each on a cold shader cache and several on
/// a loaded host, and a commit warms half a dozen. It ends as soon as nothing is left to compile.
const WARM_MS: u64 = 60_000;

/// The gradient as the sweep draws it, its middle at the centre, then where each drag leaves it.
const SWEEP: ([f64; 2], [f64; 2]) = ([0.5, 0.3], [0.5, 0.7]);
const FIRST_MOVE: [[f64; 2]; 3] = [[0.5, 0.5], [0.5, 0.47], [0.5, 0.44]];
const SECOND_MOVE: [[f64; 2]; 3] = [[0.5, 0.44], [0.5, 0.41], [0.5, 0.38]];

/// Patches inside the four quadrants, clear of the white centre cross and the black dash band, and
/// one in each half the gradient divides.
const PATCHES: [[f64; 2]; 4] = [[0.25, 0.2], [0.75, 0.2], [0.25, 0.8], [0.75, 0.8]];
/// Half the side of a measured patch, in capture pixels.
const PATCH_HALF: i64 = 6;
/// How far a GPU frame's patch mean may stand from the CPU frame of the same settings, per
/// channel, in codes: the pointwise limits allow a fraction of a code on average, and the window
/// capture's own scaling blends a few more.
const SAME_CODES: f64 = 2.0;
/// How far the exposure drag must lift a patch above the photograph as opened.
const LIFTED: f64 = 10.0;

/// The stroke's path, across the top quadrants where the gradient covers nothing, one position
/// per tick at [`STROKE_INTERVAL_MS`].
fn stroke_path() -> Vec<[f64; 2]> {
    (0..=24)
        .map(|index| [0.15 + 0.7 * f64::from(index) / 24.0, 0.15])
        .collect()
}
const STROKE_INTERVAL_MS: u64 = 60;

/// The exposure the preference-off drag moves through, the second GPU drag's and the third's. Each
/// release differs from the exposure before it, the third's from the Presence section's Basic
/// drag's too, so that each commits.
const OFF: [f64; 2] = [0.6, 0.65];
const AGAIN: [f64; 2] = [0.4, 0.45];
const THIRD: [f64; 2] = [0.2, 0.25];
/// An idle check: long enough a settle for a 150 ms dissolve to end and the slot to retire, then a
/// second over which nothing may draw.
const IDLE: script::IdleStep = script::IdleStep {
    settle_ms: 600,
    ms: 1000,
};

pub(crate) fn quiet(name: &str) -> Step {
    quiet_for(name, QUIET_MS)
}

pub(crate) fn quiet_for(name: &str, ms: u64) -> Step {
    Step::new(name, script::Step::Wait { ms }).commits(0)
}

/// [`quiet`], then until the GPU preview has compiled everything handed to it, the committed
/// stack's warm list among it, at most [`WARM_MS`] in all.
fn warmed(name: &str) -> Step {
    Step::new(
        name,
        script::Step::GpuWarmed {
            quiet_ms: QUIET_MS,
            ms: WARM_MS,
        },
    )
    .commits(0)
}

/// Every frame, in order: the open, then one per step.
pub fn plan(_: &[PathBuf]) -> Plan {
    let mask = |name: &str, step: MaskStep| Step::new(name, script::Step::Mask(step));
    Plan::new(
        vec![
            Step::opened("opened").no_draft().masks(0),
            // The Performance section's sampler wakes the editor every second while it is open; the
            // idle checks below are of the GPU preview alone.
            Step::new(
                "performance-closed",
                script::Step::Performance { expanded: false },
            )
            .commits(0),
            // 1: the drag's first tick, from the resident boundary the photograph's job derived.
            Step::new("drag-first", SliderStep::new(BASIC, EXPOSURE, [FIRST]))
                .commits(0)
                .draft(BASIC, json!({ EXPOSURE: FIRST })),
            // 2: nothing asked; the surface evaluates the plan and the sequence compiles meanwhile.
            quiet("boundary-held").draft(BASIC, json!({ EXPOSURE: FIRST })),
            // 3: the same gesture's later ticks, drawn on the GPU with no preview job.
            Step::new("drag-gpu", SliderStep::new(BASIC, EXPOSURE, DRAGGED))
                .commits(0)
                .draft(BASIC, json!({ EXPOSURE: DRAGGED[1] })),
            // 4: released at the last value: one entry, the CPU's frame of the same settings.
            Step::new(
                "release",
                SliderStep::new(BASIC, EXPOSURE, [DRAGGED[1]]).release(),
            )
            .commits(1)
            .no_draft()
            .payload(BASIC_EFFECT, json!({ EXPOSURE: DRAGGED[1] })),
            // 5: settled: the dissolve the release started has ended, nothing draws once it has,
            // and the boundary stays on the GPU as the resident one.
            Step::new("settled", script::Step::Idle(IDLE))
                .commits(0)
                .no_draft(),
            // The preference off from the palette: a drag takes the CPU path, naming it, and asks for
            // no boundary; its release starts no dissolve. Then on again.
            Step::new("preference-off", PaletteStep::Run("gpu preview".into()))
                .commits(0)
                .workspace("gpu_preview", json!(false)),
            Step::new("drag-off", SliderStep::new(BASIC, EXPOSURE, OFF))
                .commits(0)
                .draft(BASIC, json!({ EXPOSURE: OFF[1] })),
            Step::new(
                "drag-off-release",
                SliderStep::new(BASIC, EXPOSURE, [OFF[1]]).release(),
            )
            .commits(1)
            .no_draft(),
            Step::new("preference-on", PaletteStep::Run("gpu preview".into()))
                .commits(0)
                .workspace("gpu_preview", json!(true)),
            // Both clipping overlays shown (J): a drag is still drawn on the GPU, marking its own
            // clipped pixels.
            Step::new("clipping-on", script::Step::Key { key: "j".into() })
                .commits(0)
                .workspace("clip_shadows", json!(true))
                .workspace("clip_highlights", json!(true)),
            Step::new("again-first", SliderStep::new(BASIC, EXPOSURE, [AGAIN[0]])).commits(0),
            quiet("again-held"),
            Step::new("again-gpu", SliderStep::new(BASIC, EXPOSURE, AGAIN))
                .commits(0)
                .draft(BASIC, json!({ EXPOSURE: AGAIN[1] })),
            Step::new(
                "again-release",
                SliderStep::new(BASIC, EXPOSURE, [AGAIN[1]]).release(),
            )
            .commits(1)
            .no_draft(),
            Step::new("clipping-off", script::Step::Key { key: "j".into() })
                .commits(0)
                .workspace("clip_shadows", json!(false))
                .workspace("clip_highlights", json!(false)),
            // A third drag on the GPU, whose release's dissolve the next gesture's first tick, the
            // Detail drag's, cancels: the release's capture, with no overlay to derive, ends well
            // inside it.
            Step::new("third-first", SliderStep::new(BASIC, EXPOSURE, [THIRD[0]])).commits(0),
            quiet("third-held"),
            Step::new("third-gpu", SliderStep::new(BASIC, EXPOSURE, THIRD))
                .commits(0)
                .draft(BASIC, json!({ EXPOSURE: THIRD[1] })),
            Step::new(
                "third-release",
                SliderStep::new(BASIC, EXPOSURE, [THIRD[1]]).release(),
            )
            .commits(1)
            .no_draft(),
            // 6-10: a Detail Amount drag. Its ticks are Detail's spatial step drawn on the GPU
            // from the first, over the resident boundary, the restoration layer's input; its
            // release commits; and once settled the boundary is still the resident one.
            Step::new(
                "detail-first",
                SliderStep::new(DETAIL, AMOUNT, [DETAIL_FIRST]),
            )
            .commits(0)
            .draft(DETAIL, json!({ AMOUNT: DETAIL_FIRST })),
            quiet("detail-held").draft(DETAIL, json!({ AMOUNT: DETAIL_FIRST })),
            Step::new(
                "detail-gpu",
                SliderStep::new(DETAIL, AMOUNT, DETAIL_DRAGGED),
            )
            .commits(0)
            .draft(DETAIL, json!({ AMOUNT: DETAIL_DRAGGED[1] })),
            Step::new(
                "detail-release",
                SliderStep::new(DETAIL, AMOUNT, [DETAIL_DRAGGED[1]]).release(),
            )
            .commits(1)
            .no_draft()
            .payload(DETAIL_EFFECT, json!({ AMOUNT: DETAIL_DRAGGED[1] })),
            quiet("detail-settled").no_draft(),
            // Mask mode, a linear gradient swept and applied.
            Step::new(
                "mask-mode",
                script::Step::Workspace(WorkspaceStep::default().mode("mask")),
            )
            .commits(0)
            .mode("mask"),
            mask("new-linear", MaskStep::New("linear".into())).commits(0),
            mask(
                "sweep",
                MaskStep::Sweep {
                    from: SWEEP.0,
                    to: SWEEP.1,
                },
            )
            .commits(0),
            mask("apply-linear", MaskStep::Release).commits(1).masks(1),
            // 11: a masked exposure through it, as the panel's own drag.
            Step::new(
                "masked",
                SliderStep::new(BASIC, EXPOSURE, [MASKED]).release(),
            )
            .commits(1)
            .no_draft(),
            // 12: the coverage tint chosen, so the moved gradient's frames and the frame its Apply
            // commits draw the same overlay, and their pixels compare.
            Step::new(
                "overlay-tint",
                script::Step::Workspace(WorkspaceStep::default().mask_overlay("tint")),
            )
            .commits(0)
            .workspace("mask_overlay", json!("tint")),
            // 13-17: the gradient selected, its handles resting, and its middle handle pressed and
            // moved twice without letting go, the press's tick and the moves' drawn on the GPU over
            // the resident boundary; then Apply commits the held drag.
            mask(
                "select-linear",
                MaskStep::SelectComponent(Some(Reference::Index(0))),
            )
            .commits(0),
            mask(
                "move-first",
                MaskStep::Drag {
                    handle: DragHandle::Middle,
                    points: FIRST_MOVE.to_vec(),
                    release: false,
                },
            )
            .commits(0),
            quiet("move-held"),
            mask(
                "move-gpu",
                MaskStep::Drag {
                    handle: DragHandle::Middle,
                    points: SECOND_MOVE.to_vec(),
                    release: false,
                },
            )
            .commits(0),
            mask("move-apply", MaskStep::Release).commits(1),
            // The applied gradient once its dissolve has ended: the frame that replaced the GPU one.
            quiet("apply-settled"),
            // 17-18: a brush on the same mask, and one stroke painted a position per tick, each
            // drawn on the GPU, its release committing it as one entry.
            mask("new-brush", MaskStep::Paint(PaintStep::NewBrush)).commits(0),
            mask(
                "stroke",
                MaskStep::Stroke {
                    points: stroke_path(),
                    release: true,
                    interval_ms: Some(STROKE_INTERVAL_MS),
                    settle_between: false,
                },
            )
            .commits(1),
            // The stroke's dissolve ends, and nothing draws after it.
            Step::new("stroke-idle", script::Step::Idle(IDLE)).commits(0),
            // 19-: back in the pointer mode, Presence committed, then three drags on the GPU.
            Step::new(
                "pointer-mode",
                script::Step::Workspace(WorkspaceStep::default().mode("pointer")),
            )
            .commits(0)
            .mode("pointer"),
            Step::new(
                "dehaze",
                SliderStep::new(PRESENCE, "dehaze", [DEHAZE]).release(),
            )
            .commits(1)
            .no_draft(),
            Step::new(
                "clarity",
                SliderStep::new(PRESENCE, "clarity", [CLARITY]).release(),
            )
            .commits(1)
            .no_draft(),
            // The committed stack's warm list compiles before the first Presence drag, and each
            // drag's release warms the next stack before the next drag begins.
            warmed("presence-settled").no_draft(),
        ]
        .into_iter()
        .chain(drag_steps(
            "texture",
            PRESENCE,
            "texture",
            TEXTURE_DRAG,
            Held::Quiet(QUIET_MS),
            Settled::Warmed,
        ))
        .chain(drag_steps(
            "clarity",
            PRESENCE,
            "clarity",
            CLARITY_DRAG,
            Held::Quiet(QUIET_MS),
            Settled::Warmed,
        ))
        .chain(drag_steps(
            "under",
            BASIC,
            EXPOSURE,
            UNDER_DRAG,
            Held::Quiet(QUIET_MS),
            Settled::Quiet,
        ))
        .collect(),
    )
}

/// How a drag's `<name>-held` step waits after its first tick.
#[derive(Clone, Copy)]
pub(crate) enum Held {
    /// This many milliseconds of quiet while the surface evaluates the plan, the drag's sequence
    /// warmed.
    Quiet(u64),
    /// This many milliseconds of quiet while the surface evaluates the plan, then until the
    /// sequence the held boundary's first plan asked for has compiled, at most [`WARM_MS`] in all:
    /// a percentage view's plans are not warmed, so its drag compiles its own.
    Compiled(u64),
}

/// How a drag's last step waits after its release.
#[derive(Clone, Copy)]
pub(crate) enum Settled {
    /// [`QUIET_MS`] of quiet.
    Quiet,
    /// The quiet, then until the committed stack's warm list has compiled ([`warmed`]).
    Warmed,
}

/// One drag over a held boundary, a step a tick: `<name>-first`, drawn from the resident boundary
/// or asking for one; `<name>-held`, as `held` says, while a boundary asked for arrives, the surface
/// first runs every pass and the pause settles the tick; `<name>-gpu-1` and `<name>-gpu-2`, a GPU
/// tick each; `<name>-release`, which commits the last value; and `<name>-settled`, as `settled`
/// says.
pub(crate) fn drag_steps(
    name: &str,
    action: &str,
    field: &str,
    values: [f64; 3],
    held: Held,
    settled: Settled,
) -> Vec<Step> {
    let slider = |value: f64| SliderStep::new(action, field, [value]);
    vec![
        Step::new(format!("{name}-first"), slider(values[0])).commits(0),
        match held {
            Held::Quiet(ms) => quiet_for(&format!("{name}-held"), ms),
            Held::Compiled(quiet_ms) => Step::new(
                format!("{name}-held"),
                script::Step::GpuWarmed {
                    quiet_ms,
                    ms: WARM_MS,
                },
            )
            .commits(0),
        },
        Step::new(format!("{name}-gpu-1"), slider(values[1])).commits(0),
        Step::new(format!("{name}-gpu-2"), slider(values[2])).commits(0),
        Step::new(format!("{name}-release"), slider(values[2]).release())
            .commits(1)
            .no_draft(),
        match settled {
            Settled::Quiet => quiet(&format!("{name}-settled")),
            Settled::Warmed => warmed(&format!("{name}-settled")),
        }
        .no_draft(),
    ]
}

/// The events of the step whose frame is `frame`: from its `script_step` record to the next.
pub(crate) fn step_events<'a>(launch: &'a Checked, frame: &str) -> Result<&'a [Value]> {
    let step = launch.index(frame)? as u64;
    let events = &launch.events;
    let start = events
        .iter()
        .position(|event| event["event"] == "script_step" && event["detail"]["step"] == step)
        .ok_or_else(|| format!("no script_step {step} for {frame}"))?;
    let end = events[start + 1..]
        .iter()
        .position(|event| event["event"] == "script_step")
        .map_or(events.len(), |offset| start + 1 + offset);
    Ok(&events[start..end])
}

pub(crate) fn named<'a>(events: &'a [Value], name: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|event| event["event"] == name)
        .collect()
}

/// The GPU ticks and CPU ticks among `events`, and how many preview jobs went to the worker.
pub(crate) fn ticks(events: &[Value]) -> (usize, usize, usize) {
    let ticks = named(events, "gpu_preview_tick");
    let gpu = ticks
        .iter()
        .filter(|tick| tick["detail"]["path"] == "gpu")
        .count();
    (
        gpu,
        ticks.len() - gpu,
        named(events, "preview_job_requested").len(),
    )
}

/// A frame the surface drew on the GPU from the plan of the gesture's newest tick: the GPU path
/// with no fallback, the held boundary's version and the draft revision of the frame's own draft,
/// the slot's bytes within the budget, and the status bar's GPU label of the frame's own figure.
pub(crate) fn gpu_drawn(frame: &Frame) -> Result<Value> {
    let state = frame.state();
    let gpu = &state["surface"]["gpu"];
    let summary = &gpu["gpu_preview"]["drag"];
    let revision = &state["draft"]["draft_revision"];
    let bar = &state["status_bar"];
    ensure(
        gpu["drawing_path"] == json!("gpu")
            && gpu["gpu_fallback"].is_null()
            && gpu["plan_fallback"].is_null(),
        format!(
            "{} was not drawn on the GPU: path {}, fallback {}, plan fallback {}",
            frame["file"], gpu["drawing_path"], gpu["gpu_fallback"], gpu["plan_fallback"]
        ),
    )?;
    ensure(
        revision.as_u64().is_some()
            && &gpu["drawn_gpu_revision"] == revision
            && gpu["drawn_gpu_boundary"] == summary["boundary"]["version"]
            && !gpu["drawn_gpu_boundary"].is_null(),
        format!(
            "{} drew revision {} over boundary {}, while the draft is at {} and holds boundary {}",
            frame["file"],
            gpu["drawn_gpu_revision"],
            gpu["drawn_gpu_boundary"],
            revision,
            summary["boundary"]
        ),
    )?;
    let figure = |name: &str| gpu[name].as_u64().unwrap_or(0);
    ensure(
        figure("gpu_preview_in_use_bytes") > 0
            && figure("gpu_preview_in_use_bytes") <= figure("gpu_preview_budget_bytes")
            && gpu["gpu_preview_scratch_bytes"].is_u64()
            && figure("gpu_preview_scratch_bytes") <= figure("gpu_preview_in_use_bytes"),
        format!(
            "{}: {} GPU-preview bytes in use of {}, {} of them scratch",
            frame["file"],
            gpu["gpu_preview_in_use_bytes"],
            gpu["gpu_preview_budget_bytes"],
            gpu["gpu_preview_scratch_bytes"]
        ),
    )?;
    let ms = bar["gpu_ms"].as_f64().unwrap_or(f64::NAN);
    ensure(
        bar["render"]
            == json!(crate::smoke::gpu_text(
                ms,
                crate::smoke::gpu_at_rest(frame.state())
            )),
        format!(
            "{}: the status bar says {} for a GPU frame",
            frame["file"], bar["render"]
        ),
    )?;
    Ok(json!({
        "drawing_path": gpu["drawing_path"],
        "drawn_gpu_boundary": gpu["drawn_gpu_boundary"],
        "drawn_gpu_revision": gpu["drawn_gpu_revision"],
        "draft_revision": revision,
        "boundary": summary["boundary"],
        "boundaries_derived": summary["boundaries_derived"],
        "gpu_ticks": summary["gpu_ticks"],
        "cpu_ticks": summary["cpu_ticks"],
        "in_use_bytes": gpu["gpu_preview_in_use_bytes"],
        "scratch_bytes": gpu["gpu_preview_scratch_bytes"],
        "frame_us": gpu["gpu_preview_frame_us"],
        "render": bar["render"],
    }))
}

/// Each patch's mean colour in `gpu` against the same patch of `cpu`, the CPU's frame of the same
/// settings: within [`SAME_CODES`] on every channel.
pub(crate) fn same_pixels(gpu: &Frame, cpu: &Frame) -> Result<Value> {
    same_pixels_at(gpu, cpu, &PATCHES)
}

/// [`same_pixels`] at `patches`.
fn same_pixels_at(gpu: &Frame, cpu: &Frame, patches: &[[f64; 2]]) -> Result<Value> {
    let mut readings = Vec::new();
    for &at in patches {
        let (drawn, reference) = (gpu.rgb_at(at, PATCH_HALF)?, cpu.rgb_at(at, PATCH_HALF)?);
        let worst = (0..3)
            .map(|channel| (drawn[channel] - reference[channel]).abs())
            .fold(0.0, f64::max);
        ensure(
            worst <= SAME_CODES,
            format!(
                "{} at {at:?} is {drawn:?}, {worst:.2} codes from {}'s {reference:?}",
                gpu["file"], cpu["file"]
            ),
        )?;
        readings.push(json!({"at": at, "gpu": drawn, "cpu": reference, "worst": worst}));
    }
    Ok(json!(readings))
}

/// The frame `name`, once the drag `release` ended has settled: the photograph the GPU's picture
/// of the committed stack at rest — its view plan, or its picture at rest in tiles — no drag open,
/// and the boundary `version` the drag drew from kept on the GPU as the resident one, its slot
/// within the budget, handed over when the drag ended.
pub(crate) fn resident_kept(
    launch: &Checked,
    release: &str,
    name: &str,
    version: &Value,
) -> Result<Value> {
    let gpu = &launch.at(name)?.state()["surface"]["gpu"];
    let figure = |key: &str| gpu[key].as_u64().unwrap_or(0);
    let handed = named(span_events(launch, release, name)?, "gpu_boundary_resident")
        .iter()
        .filter(|event| {
            event["detail"]["why"] == "draft-ended" && &event["detail"]["version"] == version
        })
        .count();
    ensure(
        !version.is_null()
            && gpu["drawing_path"] == json!("gpu")
            && (gpu["picture"] == json!("view") || gpu["picture"] == json!("rest"))
            && gpu["gpu_preview"]["drag"].is_null()
            && &gpu["gpu_preview"]["resident"]["version"] == version
            && figure("gpu_preview_in_use_bytes") > 0
            && figure("gpu_preview_in_use_bytes") <= figure("gpu_preview_budget_bytes")
            && handed >= 1,
        format!(
            "After {release} settled the GPU preview does not draw the stack at rest over boundary \
             {version} held as the resident one: path {}, picture {}, drag {}, resident {}, {} \
             bytes in use of {}, handed over {handed} times",
            gpu["drawing_path"],
            gpu["picture"],
            gpu["gpu_preview"]["drag"],
            gpu["gpu_preview"]["resident"],
            gpu["gpu_preview_in_use_bytes"],
            gpu["gpu_preview_budget_bytes"]
        ),
    )?;
    Ok(
        json!({"drawing_path": gpu["drawing_path"], "picture": gpu["picture"],
            "resident": gpu["gpu_preview"]["resident"],
            "in_use": gpu["gpu_preview_in_use_bytes"], "scratch": gpu["gpu_preview_scratch_bytes"],
            "handed_over": handed}),
    )
}

pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let mut checks = Checks::new();

    // Opened: the photograph's own job held its source on the GPU and derived from it the boundary
    // every gesture over this source and view starts from, a cut of the source at Fit's exact
    // stage.
    let opened = launch.at("opened")?;
    let at_open = &opened.state()["surface"]["gpu"]["gpu_preview"];
    // The open's events: every one before the script's first step.
    let first_step = launch
        .events
        .iter()
        .position(|event| event["event"] == "script_step")
        .unwrap_or(launch.events.len());
    let derived_at_open = named(&launch.events[..first_step], "gpu_boundary");
    checks.note(
        opened,
        "the source held and the resident boundary derived from it as the photograph opened",
        json!({"source": at_open["source"], "resident": at_open["resident"],
            "gpu_boundary": derived_at_open}),
    );
    let resident = &at_open["resident"]["version"];
    ensure(
        at_open["source"]["version"].is_u64() && resident.is_u64(),
        format!("The photograph opened with no source or resident boundary on the GPU: {at_open}"),
    )?;

    // The drag's first tick, from the resident boundary: the drag derives none. It is drawn on the
    // GPU once the surface has evaluated its plan, and until then on the CPU, naming a reason that
    // passes.
    let first = launch.at("drag-first")?;
    let gpu = &first.state()["surface"]["gpu"];
    let summary = &gpu["gpu_preview"]["drag"];
    let events = step_events(launch, "drag-first")?;
    let (gpu_ticks, cpu_ticks, jobs) = ticks(events);
    let reasons: Vec<&Value> = named(events, "gpu_preview_tick")
        .iter()
        .map(|tick| &tick["detail"]["reason"])
        .collect();
    checks.note(
        first,
        "the drag's first tick, from the resident boundary",
        json!({"drawing_path": gpu["drawing_path"], "plan_fallback": gpu["plan_fallback"],
            "gpu_ticks": gpu_ticks, "cpu_ticks": cpu_ticks, "jobs": jobs, "reasons": reasons,
            "boundary": summary["boundary"]}),
    );
    ensure(
        gpu_ticks + cpu_ticks >= 1
            && summary["boundaries_derived"] == json!(0)
            && &summary["boundary"]["version"] == resident
            && reasons.iter().all(|reason| {
                reason.is_null()
                    || ["surface-pending", "compiling", "source-uploading"]
                        .iter()
                        .any(|passing| *reason == passing)
            }),
        format!(
            "The first tick was not drawn from the resident boundary {resident}: {gpu_ticks} \
             GPU and {cpu_ticks} CPU ticks, reasons {reasons:?}, drag {summary}"
        ),
    )?;

    // Held: the resident boundary, a cut of the source, and still none derived by the drag.
    let held = launch.at("boundary-held")?;
    let summary = &held.state()["surface"]["gpu"]["gpu_preview"]["drag"];
    checks.note(held, "the boundary held", json!({"drag": summary}));
    ensure(
        &summary["boundary"]["version"] == resident
            && summary["boundary"]["derived"] == json!("cut")
            && summary["boundaries_derived"] == json!(0),
        format!("The drag does not hold the resident boundary {resident}: {summary}"),
    )?;

    // The later ticks: drawn on the GPU, with no preview job, and the pixels the CPU commits.
    let dragged = launch.at("drag-gpu")?;
    let drawn = gpu_drawn(dragged)?;
    let (gpu_ticks, cpu_ticks, jobs) = ticks(step_events(launch, "drag-gpu")?);
    ensure(
        gpu_ticks >= 1 && cpu_ticks == 0 && jobs == 0,
        format!(
            "The drag's later ticks were {gpu_ticks} on the GPU and {cpu_ticks} on the CPU, with \
             {jobs} preview jobs"
        ),
    )?;
    ensure(
        dragged.state()["surface"]["gpu"]["gpu_preview"]["drag"]["boundaries_derived"] == json!(0),
        "The drag derived a boundary of its own",
    )?;
    let lifted = PATCHES
        .iter()
        .map(
            |at| Ok(dragged.luminance_at(*at, PATCH_HALF)? - opened.luminance_at(*at, PATCH_HALF)?),
        )
        .collect::<Result<Vec<f64>>>()?;
    ensure(
        lifted.iter().all(|lift| *lift >= LIFTED),
        format!("The GPU frame did not lift every quadrant by {LIFTED}: {lifted:?}"),
    )?;
    let released = launch.at("release")?;
    let compared = same_pixels(dragged, released)?;
    checks.note(
        dragged,
        "the drag drawn on the GPU, against the CPU frame its release commits",
        json!({"drawn": drawn, "gpu_ticks": gpu_ticks, "jobs": jobs, "lifted": lifted,
            "against_release": compared}),
    );

    // Settled: the photograph the CPU's, and the drag's boundary kept on the GPU as the resident
    // one.
    let settled = launch.at("settled")?;
    let version = &dragged.state()["surface"]["gpu"]["gpu_preview"]["drag"]["boundary"]["version"];
    let kept = resident_kept(launch, "release", "settled", version)?;
    checks.note(
        settled,
        "settled after the release: the boundary kept as the resident one",
        kept,
    );

    // The Detail drag: drawn on the GPU from its first tick over the resident boundary, which the
    // third drag drew from and is the Detail layer's own input, deriving none; its later ticks
    // with no preview job, the pixels its release commits; and the boundary still the resident
    // one once settled.
    let resident = &launch.at("third-gpu")?.state()["surface"]["gpu"]["gpu_preview"]["drag"]["boundary"]
        ["version"];
    let events = step_events(launch, "detail-first")?;
    let (gpu_ticks, cpu_ticks, _) = ticks(events);
    let first_ticks = named(events, "gpu_preview_tick");
    let derived = &launch.at("detail-first")?.state()["surface"]["gpu"]["gpu_preview"]["drag"]["boundaries_derived"];
    ensure(
        gpu_ticks >= 1
            && cpu_ticks == 0
            && derived == &json!(0)
            && first_ticks
                .iter()
                .all(|tick| &tick["detail"]["boundary"] == resident),
        format!(
            "The Detail drag's first tick was {gpu_ticks} GPU and {cpu_ticks} CPU ticks, {derived} \
             boundaries derived, not drawn from the resident boundary {resident}: {:?}",
            first_ticks
                .iter()
                .map(|tick| &tick["detail"])
                .collect::<Vec<_>>()
        ),
    )?;
    let held = launch.at("detail-held")?;
    let summary = &held.state()["surface"]["gpu"]["gpu_preview"]["drag"];
    ensure(
        summary["boundary"]["layer"] == json!(0)
            && &summary["boundary"]["version"] == resident
            && summary["boundaries_derived"] == json!(0),
        format!(
            "The Detail drag does not hold the resident boundary {resident} at the Detail \
             layer's input: {summary}"
        ),
    )?;
    let detailed = launch.at("detail-gpu")?;
    let drawn = gpu_drawn(detailed)?;
    let (gpu_ticks, cpu_ticks, jobs) = ticks(step_events(launch, "detail-gpu")?);
    ensure(
        gpu_ticks >= 1 && cpu_ticks == 0 && jobs == 0,
        format!(
            "The Detail drag's later ticks were {gpu_ticks} on the GPU and {cpu_ticks} on the \
             CPU, with {jobs} preview jobs"
        ),
    )?;
    ensure(
        detailed.state()["surface"]["gpu"]["gpu_preview"]["drag"]["boundary"]["layer"] == json!(0),
        "The Detail drag's GPU frame was not drawn from the Detail layer's input",
    )?;
    let committed = launch.at("detail-release")?;
    let patches: Vec<[f64; 2]> = PATCHES.iter().chain(&EDGES).copied().collect();
    let compared = same_pixels_at(detailed, committed, &patches)?;
    let kept = resident_kept(launch, "detail-release", "detail-settled", resident)?;
    checks.note(
        detailed,
        "a Detail drag drawn on the GPU from the resident boundary, the Detail layer's input, \
         against the CPU frame its release commits",
        json!({"drawn": drawn, "gpu_ticks": gpu_ticks, "jobs": jobs,
            "against_release": compared, "settled": kept}),
    );

    // The gradient's second drag: drawn on the GPU, the overlay's coverage following it.
    let moved = launch.at("move-gpu")?;
    let drawn = gpu_drawn(moved)?;
    let events = step_events(launch, "move-gpu")?;
    let (gpu_ticks, cpu_ticks, jobs) = ticks(events);
    let coverage = named(events, "mask_coverage_ready").len();
    ensure(
        gpu_ticks >= 1 && jobs == 0 && coverage >= 1,
        format!(
            "The gradient's second drag was {gpu_ticks} GPU and {cpu_ticks} CPU ticks with \
             {jobs} preview jobs and {coverage} coverage overlays"
        ),
    )?;
    // The tint over both: the coverage worker's, of the same geometry.
    let applied = launch.at("move-apply")?;
    let overlay = &moved.state()["mask_overlay"];
    ensure(
        overlay["effective"] == json!("tint")
            && applied.state()["mask_overlay"]["effective"] == json!("tint"),
        format!(
            "The moved and applied frames do not both draw the tint: {} and {}",
            overlay["effective"],
            applied.state()["mask_overlay"]["effective"]
        ),
    )?;
    let compared = same_pixels(moved, applied)?;
    checks.note(
        moved,
        "the gradient moved on the GPU, the overlay following it",
        json!({"drawn": drawn, "gpu_ticks": gpu_ticks, "cpu_ticks": cpu_ticks, "jobs": jobs,
            "coverage_overlays": coverage, "overlay": overlay, "against_apply": compared}),
    );

    // The stroke: its positions drawn on the GPU, each with no preview job of its own.
    let stroked = launch.at("stroke")?;
    let events = step_events(launch, "stroke")?;
    let (gpu_ticks, cpu_ticks, jobs) = ticks(events);
    checks.note(
        stroked,
        "the stroke painted, its positions drawn on the GPU",
        json!({"gpu_ticks": gpu_ticks, "cpu_ticks": cpu_ticks, "jobs": jobs,
            "positions": stroke_path().len()}),
    );
    // Every CPU tick has its job, and the release's commit one more frame; a GPU tick has none.
    ensure(
        gpu_ticks >= 1 && jobs <= cpu_ticks + 2,
        format!(
            "The stroke was {gpu_ticks} GPU and {cpu_ticks} CPU ticks with {jobs} preview jobs"
        ),
    )?;

    // The Presence drags and the Basic drag under Presence: each GPU tick drawn with no preview
    // job, from a plan reading Dehaze's light from its light link over the source, running the
    // compute passes its words change; its pixels the CPU's frame of the same settings.
    for (name, gain_only) in [("texture", true), ("clarity", true), ("under", false)] {
        presence_drag_checks(launch, &mut checks, name, &["source"], gain_only, true)?;
    }

    settle_checks(launch, &mut checks)?;
    checks.write(&launch.evidence, run.scenario(), json!({}))
}

/// The checks of one drag made by [`drag_steps`] named `name`: each GPU tick drawn with no preview
/// job, from a plan each of whose lights is computed as `lights` says, in order — from the source
/// through the prefix's colour, or by its stand-in with the spatial layers before it left out —
/// running at most
/// [`GAIN_PASSES`] compute passes a tick when the drag moves only a gain (`gain_only`), and the
/// last one's pixels the CPU frame its release commits. When its sequence was `warmed` and the warm
/// list had finished compiling as the drag began, no frame of the drag waits for it to compile;
/// when it had not, how many sequences were still compiling is recorded.
pub(crate) fn presence_drag_checks(
    launch: &Checked,
    checks: &mut Checks,
    name: &str,
    lights: &[&str],
    gain_only: bool,
    warmed: bool,
) -> Result {
    // A warmed drag begins once the step before it, a `gpu_warmed` wait, has seen the committed
    // stack's warm list compile: from then on no frame of the drag may wait for a compile, since
    // its sequence is among what was warmed. A wait that reached its deadline first fails the
    // drag, naming what was still compiling; a `compiling` frame is never taken for a GPU frame.
    // That warming covers a drag's shapes is proven in the core too
    // (`the_warmed_plans_hold_a_spatial_layers_drags`).
    let first = launch.index(&format!("{name}-first"))?;
    let before = &launch.frames[first.checked_sub(1).ok_or("a frame before the drag")?];
    let figures = &before.state()["surface"]["gpu"];
    let figure = |key: &str| figures[key].as_u64().unwrap_or(0);
    let pending = figure("gpu_preview_compile_pending");
    if warmed {
        let wait = &before["step"]["gpu_warmed"];
        ensure(
            pending == 0 && wait["finished"] == json!(true),
            format!(
                "the {name} drag began with {pending} sequences still compiling after {}: {wait}",
                launch.names()[first - 1]
            ),
        )?;
        checks.note(
            launch.at(&format!("{name}-first"))?,
            &format!("the {name} drag began once the warm list had compiled"),
            json!({"after": launch.names()[first - 1], "gpu_warmed": wait,
                "compiles": figure("gpu_preview_compiles"),
                "compiled": figure("gpu_preview_compiled"),
                "compile_max_us": figure("gpu_preview_compile_max_us")}),
        );
    }
    if warmed {
        for step in ["first", "held", "gpu-1", "gpu-2"].map(|step| format!("{name}-{step}")) {
            let gpu = &launch.at(&step)?.state()["surface"]["gpu"];
            let compiling = json!({"reason": "compiling"});
            let ticked = named(step_events(launch, &step)?, "gpu_preview_tick")
                .iter()
                .any(|tick| tick["detail"]["reason"] == json!("compiling"));
            ensure(
                gpu["gpu_fallback"] != compiling && gpu["plan_fallback"] != compiling && !ticked,
                format!(
                    "{step} waited for its sequence to compile: fallback {}, plan fallback {}",
                    gpu["gpu_fallback"], gpu["plan_fallback"]
                ),
            )?;
        }
    }
    let mut counted = Vec::new();
    let mut before = launch.at(&format!("{name}-held"))?;
    for tick in ["gpu-1", "gpu-2"] {
        let step = format!("{name}-{tick}");
        let frame = launch.at(&step)?;
        let drawn = gpu_drawn(frame)?;
        let (gpu_ticks, cpu_ticks, jobs) = ticks(step_events(launch, &step)?);
        ensure(
            gpu_ticks >= 1 && cpu_ticks == 0 && jobs == 0,
            format!(
                "{step} was {gpu_ticks} GPU and {cpu_ticks} CPU ticks with {jobs} preview jobs"
            ),
        )?;
        let summary = &frame.state()["surface"]["gpu"]["gpu_preview"]["drag"];
        let computed: Vec<Value> = summary["lights"]
            .as_array()
            .map(|read| read.iter().map(|light| light["computed"].clone()).collect())
            .unwrap_or_default();
        ensure(
            computed == lights.iter().map(|light| json!(light)).collect::<Vec<_>>(),
            format!(
                "{step}'s plan reads lights {}, not {lights:?}",
                summary["lights"]
            ),
        )?;
        let passes = |frame: &Frame| {
            frame.state()["surface"]["gpu"]["gpu_preview_spatial_passes"]
                .as_u64()
                .unwrap_or(0)
        };
        let ran = passes(frame).saturating_sub(passes(before));
        ensure(
            !gain_only || ran <= GAIN_PASSES * gpu_ticks as u64,
            format!("{step} ran {ran} compute passes in {gpu_ticks} gain-only ticks"),
        )?;
        counted.push(
            json!({"step": step, "gpu_ticks": gpu_ticks, "compute_passes": ran,
            "drawn": drawn}),
        );
        before = frame;
    }
    let compared = same_pixels(
        launch.at(&format!("{name}-gpu-2"))?,
        launch.at(&format!("{name}-release"))?,
    )?;
    checks.note(
        launch.at(&format!("{name}-gpu-2"))?,
        &format!("the {name} drag on the GPU, against the CPU frame its release commits"),
        json!({"lights": lights, "ticks": counted, "against_release": compared}),
    );
    Ok(())
}

/// The events from the start of `first`'s step to the end of `last`'s.
pub(crate) fn span_events<'a>(launch: &'a Checked, first: &str, last: &str) -> Result<&'a [Value]> {
    let start = step_events(launch, first)?.as_ptr();
    let events = &launch.events;
    let from = events
        .iter()
        .position(|event| std::ptr::eq(event, start))
        .ok_or("the span's first step")?;
    let end = step_events(launch, last)?;
    let to = events
        .iter()
        .position(|event| std::ptr::eq(event, end.as_ptr()))
        .ok_or("the span's last step")?
        + end.len();
    Ok(&events[from..to])
}

/// The idle check the step `name` recorded, which must have passed.
fn idle_passed(launch: &Checked, name: &str) -> Result<Value> {
    let checks = named(step_events(launch, name)?, "idle_check");
    ensure(
        checks.len() == 1 && checks[0]["detail"]["passed"] == json!(true),
        format!(
            "{name}: the editor was not idle after the dissolve: {:?}",
            checks
                .iter()
                .map(|event| &event["detail"])
                .collect::<Vec<_>>()
        ),
    )?;
    let surface = &launch.at(name)?.state()["surface"]["gpu"];
    ensure(
        surface["dissolve"].is_null() && surface["settle"]["running"].is_null(),
        format!("{name}: a dissolve still runs: {}", surface["settle"]),
    )?;
    Ok(checks[0]["detail"].clone())
}

/// The settle a person sees from a gesture's frame `gpu` to the picture at rest `settled` that
/// replaced it, over the photograph, as the pointwise statistics and verdict report it: recorded,
/// not held to the limits, where the two are drawn differently by design — the drag's over the
/// source reduced first, the picture at rest process-first — which the gate measures on the corpus.
pub(crate) fn settle_report(gpu: &Frame, settled: &Frame) -> Result<Value> {
    let rect = gpu.visible_photo()?;
    ensure(
        settled.visible_photo()? == rect,
        format!(
            "{} and {} show different rectangles",
            gpu["file"], settled["file"]
        ),
    )?;
    let report = crate::preview_error::report(
        gpu.image()?,
        settled.image()?,
        rect,
        Some(luxforge_reference::preview_error::Class::Pointwise),
    )?;
    Ok(
        json!({"gpu": gpu["file"], "settled": settled["file"], "statistics": report["statistics"],
        "verdict": report["verdict"]}),
    )
}

/// The jump a person sees at settle: the GPU frame on screen against the CPU frame that replaced
/// it, over the photograph, held to the pointwise limits.
pub(crate) fn jump(gpu: &Frame, cpu: &Frame) -> Result<Value> {
    let rect = gpu.visible_photo()?;
    ensure(
        cpu.visible_photo()? == rect,
        format!(
            "{} and {} show different rectangles",
            gpu["file"], cpu["file"]
        ),
    )?;
    let report = crate::preview_error::report(
        gpu.image()?,
        cpu.image()?,
        rect,
        Some(luxforge_reference::preview_error::Class::Pointwise),
    )?;
    ensure(
        report["verdict"]["passed"] == json!(true),
        format!(
            "{} against {} misses the pointwise limits: {}",
            gpu["file"], cpu["file"], report["verdict"]
        ),
    )?;
    Ok(
        json!({"gpu": gpu["file"], "cpu": cpu["file"], "statistics": report["statistics"],
        "verdict": report["verdict"]}),
    )
}

/// The committed stack at rest after the gesture `what` whose `events` end at `frame`: drawn by
/// the GPU itself, its view plan or its picture at rest in tiles, with no dissolve into a CPU
/// frame started for it: the picture at rest is the GPU's (`docs/design/gpu-preview.md`, "The
/// picture at rest").
pub(crate) fn at_rest_after(events: &[Value], frame: &Frame, what: &str) -> Result<Value> {
    let committed: Vec<&Value> = named(events, "gpu_dissolve_started")
        .into_iter()
        .filter(|event| event["detail"]["case"] == json!("committed"))
        .collect();
    let gpu = &frame.state()["surface"]["gpu"];
    let render = &frame.state()["status_bar"]["render"];
    ensure(
        committed.is_empty()
            && gpu["drawing_path"] == json!("gpu")
            && (gpu["picture"] == json!("view") || gpu["picture"] == json!("rest"))
            && render
                .as_str()
                .is_some_and(|text| text.starts_with("GPU render")),
        format!(
            "{what}: the committed stack is not the GPU's at rest in {}: path {}, picture {}, \
             render {render}, dissolves into the committed frame {:?}",
            frame["file"],
            gpu["drawing_path"],
            gpu["picture"],
            committed
                .iter()
                .map(|event| &event["detail"])
                .collect::<Vec<_>>()
        ),
    )?;
    Ok(
        json!({"picture": gpu["picture"], "drawn_gpu_boundary": gpu["drawn_gpu_boundary"],
        "drawn_rest": gpu["drawn_rest"], "render": render}),
    )
}

/// The settle hand-off at rest: each release's committed stack drawn by the GPU itself, over the
/// boundary the drag held, with no dissolve into a CPU frame and within the pointwise limits of
/// the drag's last frame; idle once settled; the preference turned off and on; the clipping marks
/// of a GPU drag, which the view plan at rest carries and lets go with the overlay; and the next
/// gesture taking the surface back from the picture at rest.
fn settle_checks(launch: &Checked, checks: &mut Checks) -> Result {
    // The drag's release: the committed stack's view plan takes the drag's frame's place, the same
    // programs over the same boundary, and nothing draws once it is on screen.
    let (dragged, settled) = (launch.at("drag-gpu")?, launch.at("settled")?);
    let rest = at_rest_after(
        span_events(launch, "release", "settled")?,
        settled,
        "the release",
    )?;
    let idle = idle_passed(launch, "settled")?;
    let jumped = jump(dragged, settled)?;
    checks.note(
        settled,
        "the release: the committed stack drawn at rest by the GPU with no dissolve, then idle",
        json!({"rest": rest, "idle": idle, "jump": jumped}),
    );

    // The preference off: every tick on the CPU, naming it, no boundary derived, and no dissolve
    // at release; on again.
    let off = launch.at("drag-off")?;
    let events = step_events(launch, "drag-off")?;
    let ticks = named(events, "gpu_preview_tick");
    let gpu = &off.state()["surface"]["gpu"];
    ensure(
        !ticks.is_empty()
            && ticks.iter().all(|tick| {
                tick["detail"]["path"] == json!("cpu")
                    && tick["detail"]["reason"] == json!("preference-off")
            })
            && named(events, "gpu_boundary").is_empty()
            && gpu["drawing_path"] == json!("cpu")
            && gpu["plan_fallback"] == json!({"reason": "preference-off"}),
        format!(
            "With the preference off the drag was not the CPU's: ticks {:?}, path {}, plan \
             fallback {}",
            ticks.iter().map(|tick| &tick["detail"]).collect::<Vec<_>>(),
            gpu["drawing_path"],
            gpu["plan_fallback"]
        ),
    )?;
    ensure(
        named(
            step_events(launch, "drag-off-release")?,
            "gpu_dissolve_started",
        )
        .is_empty(),
        "A release with no GPU frame on screen started a dissolve",
    )?;
    checks.note(
        off,
        "the preference off: the drag on the CPU",
        json!({"ticks": ticks.len(), "plan_fallback": gpu["plan_fallback"],
            "preference_on": launch.at("preference-on")?.state()["workspace"]["gpu_preview"]}),
    );

    // Clipping shown: the drag still drawn on the GPU, marking its own clipped pixels.
    let marked = launch.at("again-gpu")?;
    let drawn = gpu_drawn(marked)?;
    let marks = &marked.state()["surface"]["gpu"]["clipping_marks"];
    ensure(
        marks == &json!({"shadows": true, "highlights": true, "approximate": true}),
        format!("The GPU drag with clipping shown drew marks {marks}"),
    )?;
    checks.note(
        marked,
        "a drag with both clipping overlays shown, drawn on the GPU with its own marks",
        json!({"drawn": drawn, "clipping_marks": marks}),
    );

    // Clipping turned off at rest after the second release: the view plan drawn at rest lets its
    // marks go with the overlay.
    let cleared = launch.at("clipping-off")?;
    let rest = at_rest_after(
        span_events(launch, "again-release", "clipping-off")?,
        cleared,
        "the second release",
    )?;
    let marks = &cleared.state()["surface"]["gpu"]["clipping_marks"];
    ensure(
        marks.is_null(),
        format!("With clipping turned off at rest the picture still drew marks {marks}"),
    )?;
    checks.note(
        cleared,
        "the second release at rest on the GPU, its marks gone with the overlay",
        json!({"rest": rest}),
    );

    // The next gesture after the third release: the Detail drag takes the surface back from the
    // picture at rest with its first tick.
    let third = launch.at("third-gpu")?;
    gpu_drawn(third)?;
    let next = launch.at("detail-first")?;
    ensure(
        named(
            span_events(launch, "third-release", "detail-first")?,
            "gpu_dissolve_started",
        )
        .is_empty(),
        "The third release dissolved into a CPU frame",
    )?;
    checks.note(
        next,
        "the next gesture took the surface back from the picture at rest",
        json!({"picture": next.state()["surface"]["gpu"]["picture"]}),
    );

    // The moved gradient's Apply, and the stroke: each committed stack drawn at rest by the GPU,
    // and the editor idle after the last. The moved gradient's frame carries its handles, which
    // its applied frame does not, so its pixels are held to the CPU's by the patches above rather
    // than over the whole photograph.
    let applied = at_rest_after(
        span_events(launch, "move-apply", "apply-settled")?,
        launch.at("apply-settled")?,
        "the gradient's Apply",
    )?;
    checks.note(
        launch.at("apply-settled")?,
        "the gradient's Apply drawn at rest by the GPU",
        json!({"rest": applied}),
    );
    let held = stroke_settles(step_events(launch, "stroke")?)?;
    let rest = at_rest_after(
        span_events(launch, "stroke", "stroke-idle")?,
        launch.at("stroke-idle")?,
        "the stroke",
    )?;
    let idle = idle_passed(launch, "stroke-idle")?;
    checks.note(
        launch.at("stroke-idle")?,
        "the stroke drawn at rest by the GPU, then idle",
        json!({"rest": rest, "held": held, "idle": idle}),
    );
    Ok(())
}

/// The stroke's dissolves. Its commit starts none: the committed stack at rest is drawn by the
/// GPU. Before it, the design's held case may dissolve (`docs/design/gpu-preview.md`, "Settle and
/// the dissolve"): when the stroke's first job brings its frame and boundary only after the next
/// position has gone to the CPU with a job of its own — one 60 ms interval, which a loaded host's
/// first job outlasts — the surface draws that position's plan over the new boundary, and its own
/// CPU frame, of the same revision, then dissolves in over it, until the next position's tick
/// cancels the dissolve. Each such dissolve must start from a GPU frame this stroke drew, over the
/// same boundary, and must end or be cancelled within the stroke; any other dissolve fails.
fn stroke_settles(events: &[Value]) -> Result<Value> {
    let started: Vec<(usize, &Value)> = events
        .iter()
        .enumerate()
        .filter(|(_, event)| event["event"] == "gpu_dissolve_started")
        .collect();
    let details: Vec<&Value> = started.iter().map(|(_, event)| &event["detail"]).collect();
    // The GPU frames the stroke drew, each a draft revision over a boundary, in order.
    let drawn: Vec<(&Value, &Value, usize)> = events
        .iter()
        .enumerate()
        .filter(|(_, event)| {
            event["event"] == "surface_frame_drawn" && event["detail"]["path"] == json!("gpu")
        })
        .map(|(index, event)| {
            (
                &event["detail"]["draft_revision"],
                &event["detail"]["boundary"],
                index,
            )
        })
        .collect();
    let mut held = Vec::new();
    for (index, dissolve) in &started {
        let detail = &dissolve["detail"];
        let from_drawn = drawn.iter().any(|(revision, boundary, drawn_at)| {
            *drawn_at < *index
                && detail["from"] == **revision
                && detail["gpu_boundary"] == **boundary
        });
        let finished = events[index + 1..].iter().any(|event| {
            (event["event"] == "gpu_dissolve_cancelled" || event["event"] == "gpu_dissolve_ended")
                && event["detail"]["from"] == detail["from"]
        });
        ensure(
            detail["case"] == json!("held") && from_drawn && finished,
            format!(
                "The stroke dissolved other than from a frame it drew, held and finished within \
                 it: {details:?}"
            ),
        )?;
        held.push(detail.clone());
    }
    Ok(json!(held))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(name: &str, detail: Value) -> Value {
        json!({"event": name, "detail": detail})
    }

    fn drawn(revision: u64) -> Value {
        event(
            "surface_frame_drawn",
            json!({"path": "gpu", "draft_revision": revision, "boundary": 6}),
        )
    }

    fn started(case: &str, from: u64) -> Value {
        event(
            "gpu_dissolve_started",
            json!({"case": case, "from": from, "to": 19, "gpu_boundary": 6}),
        )
    }

    fn cancelled(from: u64) -> Value {
        event(
            "gpu_dissolve_cancelled",
            json!({"from": from, "to": 18, "why": "input"}),
        )
    }

    /// A stroke's commit starts no dissolve; a held dissolve a loaded host's late first job
    /// brings, from a frame the stroke drew and cancelled by the next tick, is the design's.
    #[test]
    fn a_strokes_commit_starts_no_dissolve_and_a_held_one_is_cancelled() {
        let plain = [drawn(2), drawn(3), drawn(25)];
        assert_eq!(stroke_settles(&plain).expect("no dissolve"), json!([]));
        let loaded = [
            drawn(2),
            started("held", 2),
            cancelled(2),
            drawn(3),
            drawn(25),
        ];
        let checked = stroke_settles(&loaded).expect("a held dissolve, cancelled");
        assert_eq!(checked.as_array().map(Vec::len), Some(1));
    }

    /// What is not the design's: a commit's dissolve into the CPU frame, a held dissolve still
    /// running at the end of the stroke or from a frame the stroke never drew.
    #[test]
    fn a_stroke_fails_any_other_dissolve() {
        let cases: [(&str, Vec<Value>); 3] = [
            (
                "a commit dissolve",
                vec![drawn(25), started("committed", 25)],
            ),
            (
                "a held dissolve still running",
                vec![drawn(2), started("held", 2), drawn(25)],
            ),
            (
                "a held dissolve from a frame never drawn",
                vec![started("held", 2), cancelled(2), drawn(25)],
            ),
        ];
        for (name, events) in cases {
            assert!(stroke_settles(&events).is_err(), "{name} passed");
        }
    }
}
