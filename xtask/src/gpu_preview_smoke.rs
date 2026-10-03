//! The `gpu-preview` smoke scenario: gestures at Fit drawn on the GPU over a held boundary, with
//! no preview job per tick, on the real editor (`docs/design/gpu-preview.md`, "A tick").
//!
//! One launch over the quadrant fixture. A Basic exposure drag opens with a CPU tick whose job
//! carries the one boundary request; once the boundary is held and the drag's program sequence
//! compiled, its ticks are drawn on the GPU, each frame tagged with its tick's draft revision; its
//! release commits, and the boundary and the slot are let go once the committed frame has settled.
//! A Detail Amount drag follows the same course from the Detail layer's own input, its ticks
//! Detail's spatial step; the photograph fits the window at its own size, so its Fit frame is the
//! exact render and its release settles to exact pixels. Then, in Mask mode, a linear gradient
//! bound to a masked exposure is moved by its middle handle in two drags, the second drawn on the
//! GPU with the coverage overlay following it, and a brush stroke is painted through the same
//! mask, its later positions drawn on the GPU. Last, back in
//! the pointer mode, Presence is committed with Dehaze and Clarity, and a Texture drag, a Clarity
//! drag and a Basic drag under Presence are each drawn on the GPU after their first tick: the two
//! Presence drags read Dehaze's light from the store and run at most five of Presence's compute
//! passes a tick (the spatial passes the tick's words change), and the Basic drag, which changes
//! the light's input, takes the light on the GPU and runs them all.
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
/// How long the scenario leaves the editor alone for the boundary to arrive and the sequence to
/// compile, and for a committed frame to settle. Generous: a 320 × 480 boundary renders in a few
/// milliseconds and a sequence compiles in tens.
const QUIET_MS: u64 = 1500;
/// How long the 100% scenario leaves the editor alone once Presence is committed, and while a
/// drag's region boundary arrives and its sequence compiles: a percentage view's plans are not
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
            // 1: the drag's first tick: the CPU frame, its job carrying the boundary request.
            Step::new("drag-first", SliderStep::new(BASIC, EXPOSURE, [FIRST]))
                .commits(0)
                .draft(BASIC, json!({ EXPOSURE: FIRST })),
            // 2: nothing asked; the boundary arrives and the sequence compiles meanwhile.
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
            // and the boundary and the slot are let go.
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
            // 6-10: a Detail Amount drag. Its first tick takes the CPU path and asks for the
            // boundary, the restoration layer's input; its later ticks are Detail's spatial step
            // drawn on the GPU; its release commits; and once settled nothing is held.
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
            mask("sweep-release", MaskStep::Release).commits(0),
            mask("apply-linear", MaskStep::Apply).commits(1).masks(1),
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
            // 13-17: the gradient reopened and moved by its middle handle twice: the reopening's tick
            // asks for the boundary, and the moves' ticks are drawn on the GPU once it is held; then
            // applied.
            mask("edit-shape", MaskStep::EditShape(Reference::Index(0))).commits(0),
            mask(
                "move-first",
                MaskStep::Drag {
                    handle: DragHandle::Middle,
                    points: FIRST_MOVE.to_vec(),
                },
            )
            .commits(0),
            quiet("move-held"),
            mask(
                "move-gpu",
                MaskStep::Drag {
                    handle: DragHandle::Middle,
                    points: SECOND_MOVE.to_vec(),
                },
            )
            .commits(0),
            mask("move-apply", MaskStep::Apply).commits(1),
            // The applied gradient once its dissolve has ended: the frame that replaced the GPU one.
            quiet("apply-settled"),
            // 17-18: a brush on the same mask, and one stroke painted a position per tick: its first
            // positions ask for the boundary, its later ones are drawn on the GPU, and its release
            // commits it as one entry.
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
            QUIET_MS,
            Settled::Warmed,
        ))
        .chain(drag_steps(
            "clarity",
            PRESENCE,
            "clarity",
            CLARITY_DRAG,
            QUIET_MS,
            Settled::Warmed,
        ))
        .chain(drag_steps(
            "under",
            BASIC,
            EXPOSURE,
            UNDER_DRAG,
            QUIET_MS,
            Settled::Quiet,
        ))
        .collect(),
    )
}

/// How a drag's last step waits after its release.
#[derive(Clone, Copy)]
pub(crate) enum Settled {
    /// [`QUIET_MS`] of quiet.
    Quiet,
    /// The quiet, then until the committed stack's warm list has compiled ([`warmed`]).
    Warmed,
}

/// One drag over a held boundary, a step a tick: `<name>-first`, whose CPU tick asks for the
/// boundary; `<name>-held`, `held_ms` while it arrives and the surface first runs every pass;
/// `<name>-gpu-1` and `<name>-gpu-2`, a GPU tick each; `<name>-release`, which commits the last
/// value; and `<name>-settled`, as `settled` says.
pub(crate) fn drag_steps(
    name: &str,
    action: &str,
    field: &str,
    values: [f64; 3],
    held_ms: u64,
    settled: Settled,
) -> Vec<Step> {
    let slider = |value: f64| SliderStep::new(action, field, [value]);
    vec![
        Step::new(format!("{name}-first"), slider(values[0])).commits(0),
        quiet_for(&format!("{name}-held"), held_ms),
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
            && figure("gpu_preview_in_use_bytes") <= figure("gpu_preview_budget_bytes"),
        format!(
            "{}: {} GPU-preview bytes in use of {}",
            frame["file"], gpu["gpu_preview_in_use_bytes"], gpu["gpu_preview_budget_bytes"]
        ),
    )?;
    let ms = bar["gpu_ms"].as_f64().unwrap_or(f64::NAN);
    ensure(
        bar["render"] == json!(crate::smoke::gpu_text(ms)),
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
        "boundary_requests": summary["boundary_requests"],
        "gpu_ticks": summary["gpu_ticks"],
        "cpu_ticks": summary["cpu_ticks"],
        "in_use_bytes": gpu["gpu_preview_in_use_bytes"],
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

pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let mut checks = Checks::new();

    // The drag's first tick: a CPU frame whose job asked for the one boundary.
    let first = launch.at("drag-first")?;
    let gpu = &first.state()["surface"]["gpu"];
    let events = step_events(launch, "drag-first")?;
    let (gpu_ticks, cpu_ticks, jobs) = ticks(events);
    let asked = named(events, "gpu_preview_tick")
        .iter()
        .filter(|tick| tick["detail"]["boundary_requested"] == json!(true))
        .count();
    checks.note(
        first,
        "the drag's first tick on the CPU, asking for the boundary",
        json!({"drawing_path": gpu["drawing_path"], "plan_fallback": gpu["plan_fallback"],
            "gpu_ticks": gpu_ticks, "cpu_ticks": cpu_ticks, "jobs": jobs, "asked": asked}),
    );
    ensure(
        gpu["drawing_path"] == json!("cpu") && gpu_ticks == 0 && cpu_ticks >= 1 && asked == 1,
        format!(
            "The first tick was not a CPU frame asking for the boundary: path {}, {gpu_ticks} \
             GPU and {cpu_ticks} CPU ticks, {asked} asking",
            gpu["drawing_path"]
        ),
    )?;

    // Held: the boundary arrived, from the one request.
    let held = launch.at("boundary-held")?;
    let summary = &held.state()["surface"]["gpu"]["gpu_preview"]["drag"];
    let boundaries = named(step_events(launch, "boundary-held")?, "gpu_boundary");
    checks.note(
        held,
        "the boundary held",
        json!({"drag": summary, "gpu_boundary": boundaries}),
    );
    ensure(
        !summary["boundary"].is_null() && summary["boundary_requests"] == json!(1),
        format!("No boundary held after the first tick's one request: {summary}"),
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
        dragged.state()["surface"]["gpu"]["gpu_preview"]["drag"]["boundary_requests"] == json!(1),
        "The drag asked for its boundary more than once",
    )?;
    let opened = launch.at("opened")?;
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

    // Settled: the boundary and the slot let go, the photograph the CPU's.
    let settled = launch.at("settled")?;
    let gpu = &settled.state()["surface"]["gpu"];
    let released_events: Vec<&Value> = launch
        .events
        .iter()
        .filter(|event| event["event"] == "gpu_boundary_released")
        .collect();
    checks.note(
        settled,
        "settled after the release: nothing held",
        json!({"drawing_path": gpu["drawing_path"], "in_use": gpu["gpu_preview_in_use_bytes"],
            "drag": gpu["gpu_preview"]["drag"], "released": released_events}),
    );
    ensure(
        gpu["drawing_path"] == json!("cpu")
            && gpu["gpu_preview_in_use_bytes"] == json!(0)
            && gpu["gpu_preview"]["drag"].is_null()
            && released_events
                .iter()
                .any(|event| event["detail"]["why"] == "draft-ended"),
        format!(
            "After settling the GPU preview still holds something: path {}, {} bytes in use, \
             drag {}",
            gpu["drawing_path"], gpu["gpu_preview_in_use_bytes"], gpu["gpu_preview"]["drag"]
        ),
    )?;

    // The Detail drag: its first tick a CPU frame asking for the boundary, the Detail layer's own
    // input; its later ticks drawn on the GPU with no preview job, the pixels its release
    // commits; and nothing held once settled.
    let events = step_events(launch, "detail-first")?;
    let (gpu_ticks, cpu_ticks, _) = ticks(events);
    let asked = named(events, "gpu_preview_tick")
        .iter()
        .filter(|tick| tick["detail"]["boundary_requested"] == json!(true))
        .count();
    ensure(
        gpu_ticks == 0 && cpu_ticks >= 1 && asked == 1,
        format!(
            "The Detail drag's first tick was {gpu_ticks} GPU and {cpu_ticks} CPU ticks, {asked} \
             asking for the boundary"
        ),
    )?;
    let held = launch.at("detail-held")?;
    let summary = &held.state()["surface"]["gpu"]["gpu_preview"]["drag"];
    ensure(
        summary["boundary"]["layer"] == json!(0) && summary["boundary_requests"] == json!(1),
        format!("The Detail drag holds no boundary at the Detail layer's input: {summary}"),
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
    let after = launch.at("detail-settled")?;
    let gpu = &after.state()["surface"]["gpu"];
    ensure(
        gpu["drawing_path"] == json!("cpu")
            && gpu["gpu_preview_in_use_bytes"] == json!(0)
            && gpu["gpu_preview"]["drag"].is_null(),
        format!(
            "After the Detail drag settled the GPU preview still holds something: path {}, {} \
             bytes in use, drag {}",
            gpu["drawing_path"], gpu["gpu_preview_in_use_bytes"], gpu["gpu_preview"]["drag"]
        ),
    )?;
    checks.note(
        detailed,
        "a Detail drag drawn on the GPU from the Detail layer's input, against the CPU frame its \
         release commits",
        json!({"drawn": drawn, "gpu_ticks": gpu_ticks, "jobs": jobs,
            "against_release": compared, "settled_in_use": gpu["gpu_preview_in_use_bytes"]}),
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

    // The stroke: its later positions drawn on the GPU, each with no preview job of its own.
    let stroked = launch.at("stroke")?;
    let events = step_events(launch, "stroke")?;
    let (gpu_ticks, cpu_ticks, jobs) = ticks(events);
    checks.note(
        stroked,
        "the stroke painted, its later positions drawn on the GPU",
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
    // job, from a plan whose light is stored or taken on the GPU, running the compute passes its
    // words change; its pixels the CPU's frame of the same settings.
    for (name, approximate, gain_only) in [
        ("texture", false, true),
        ("clarity", false, true),
        ("under", true, false),
    ] {
        presence_drag_checks(launch, &mut checks, name, approximate, gain_only, true)?;
    }

    settle_checks(launch, &mut checks)?;
    checks.write(&launch.evidence, run.scenario(), json!({}))
}

/// The checks of one drag made by [`drag_steps`] named `name`: each GPU tick drawn with no preview
/// job, from a plan whose light is stored or taken on the GPU as `approximate` says, running at most
/// [`GAIN_PASSES`] compute passes a tick when the drag moves only a gain (`gain_only`), and the
/// last one's pixels the CPU frame its release commits. When its sequence was `warmed` and the warm
/// list had finished compiling as the drag began, no frame of the drag waits for it to compile;
/// when it had not, how many sequences were still compiling is recorded.
pub(crate) fn presence_drag_checks(
    launch: &Checked,
    checks: &mut Checks,
    name: &str,
    approximate: bool,
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
        ensure(
            summary["approximate"] == json!(approximate),
            format!(
                "{step}'s plan is approximate {}, not {approximate}",
                summary["approximate"]
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
        json!({"approximate": approximate, "ticks": counted, "against_release": compared}),
    );
    Ok(())
}

/// The events from the start of `first`'s step to the end of `last`'s.
fn span_events<'a>(launch: &'a Checked, first: &str, last: &str) -> Result<&'a [Value]> {
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

/// The one `gpu_dissolve_started` among `events`, from the GPU frame `gpu` drew: its revision and
/// boundary.
pub(crate) fn dissolve_from(events: &[Value], gpu: &Frame, what: &str) -> Result<Value> {
    let started = named(events, "gpu_dissolve_started");
    let drawn = &gpu.state()["surface"]["gpu"];
    ensure(
        started.len() == 1
            && started[0]["detail"]["case"] == json!("committed")
            && started[0]["detail"]["from"] == drawn["drawn_gpu_revision"]
            && started[0]["detail"]["gpu_boundary"] == drawn["drawn_gpu_boundary"],
        format!(
            "{what}: expected one dissolve from {}'s revision {} over boundary {}, got {:?}",
            gpu["file"],
            drawn["drawn_gpu_revision"],
            drawn["drawn_gpu_boundary"],
            started
                .iter()
                .map(|event| &event["detail"])
                .collect::<Vec<_>>()
        ),
    )?;
    Ok(started[0]["detail"].clone())
}

/// What became of the dissolve `release`'s commit began, given the input the step `input` sent,
/// which takes effect at its first `effect` event and cancels a running dissolve (`why`). Still
/// running then, it must be cancelled by it; ended first, which a release capture that outlasts
/// the 150 ms dissolve on a loaded host allows, it is recorded with both times and passes. A
/// dissolve that ends, or is cut, after the input took effect fails.
fn cancelled_or_ended(
    launch: &Checked,
    release: &str,
    input: &str,
    effect: &str,
    why: &str,
) -> Result<Value> {
    let events = span_events(launch, release, input)?;
    let sent = events.len() - step_events(launch, input)?.len();
    let start = events
        .iter()
        .position(|event| event["event"] == "gpu_dissolve_started")
        .ok_or_else(|| format!("{release}: no dissolve started"))?;
    let ends = [
        "gpu_dissolve_cancelled",
        "gpu_dissolve_ended",
        "gpu_dissolve_cut",
    ];
    let end = events[start + 1..]
        .iter()
        .position(|event| ends.iter().any(|name| event["event"] == *name))
        .map(|offset| start + 1 + offset);
    let took = events[sent..]
        .iter()
        .position(|event| event["event"] == effect)
        .map(|offset| sent + offset)
        .ok_or_else(|| format!("{input}: its input never took effect ({effect})"))?;
    let at = |index: usize| {
        events[index]["elapsed_ms"].as_f64().unwrap_or(f64::NAN)
            - events[start]["elapsed_ms"].as_f64().unwrap_or(f64::NAN)
    };
    let Some(end) = end else {
        return Err(format!(
            "{release}'s dissolve neither ended nor was cancelled by {input}'s input"
        )
        .into());
    };
    let (name, detail) = (&events[end]["event"], &events[end]["detail"]);
    let mut outcome = json!({"input_sent_ms": at(sent), "input_ms": at(took)});
    if name == "gpu_dissolve_cancelled" && detail["why"] == json!(why) && end > took {
        outcome["outcome"] = json!("cancelled");
        outcome["cancelled_ms"] = detail["elapsed_ms"].clone();
    } else if name == "gpu_dissolve_ended" && end < took {
        outcome["outcome"] = json!("ended before the input");
        outcome["ended_ms"] = detail["elapsed_ms"].clone();
    } else {
        return Err(format!(
            "{release}'s dissolve, still running when {input}'s input took effect {:.1} ms in, \
             was not cancelled by it ({why}): {name} {detail}",
            at(took)
        )
        .into());
    }
    Ok(outcome)
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

/// The jump a person sees at settle: the GPU frame on screen against the CPU frame that replaced
/// it, over the photograph, held to the pointwise limits.
fn jump(gpu: &Frame, cpu: &Frame) -> Result<Value> {
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

/// The settle hand-off: each release's dissolve from the GPU frame on screen, the jump it hides
/// within the pointwise limits, idle once it has ended, the preference turned off and on, the
/// clipping marks of a GPU drag, and a dissolve cancelled by a clipping toggle and by the next
/// gesture, each where it still ran when the input took effect.
fn settle_checks(launch: &Checked, checks: &mut Checks) -> Result {
    // The drag's release: its committed frame dissolves in from the drag's last GPU frame, and
    // once it has ended nothing draws.
    let (dragged, settled) = (launch.at("drag-gpu")?, launch.at("settled")?);
    let started = dissolve_from(
        span_events(launch, "release", "settled")?,
        dragged,
        "the release",
    )?;
    ensure(
        !named(
            span_events(launch, "release", "settled")?,
            "gpu_dissolve_ended",
        )
        .is_empty(),
        "The release's dissolve never ended",
    )?;
    let idle = idle_passed(launch, "settled")?;
    let jumped = jump(dragged, settled)?;
    checks.note(
        settled,
        "the release dissolved from the GPU frame to the committed frame, then idle",
        json!({"dissolve": started, "release_frame_dissolve":
            launch.at("release")?.state()["surface"]["gpu"]["dissolve"], "idle": idle,
            "jump": jumped}),
    );

    // The preference off: every tick on the CPU, naming it, no boundary asked for, and no
    // dissolve at release; on again.
    let off = launch.at("drag-off")?;
    let events = step_events(launch, "drag-off")?;
    let ticks = named(events, "gpu_preview_tick");
    let gpu = &off.state()["surface"]["gpu"];
    ensure(
        !ticks.is_empty()
            && ticks.iter().all(|tick| {
                tick["detail"]["path"] == json!("cpu")
                    && tick["detail"]["reason"] == json!("preference-off")
                    && tick["detail"]["boundary_requested"] == json!(false)
            })
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

    // Clipping turned off while the second release's dissolve may run: a view input.
    let again = launch.at("again-gpu")?;
    let started = dissolve_from(
        step_events(launch, "again-release")?,
        again,
        "the second release",
    )?;
    let outcome = cancelled_or_ended(
        launch,
        "again-release",
        "clipping-off",
        "gpu_settle_view",
        "view",
    )?;
    checks.note(
        launch.at("clipping-off")?,
        "turning the clipping overlays off cancelled the release's dissolve, or it had ended first",
        json!({"dissolve": started, "outcome": outcome}),
    );

    // The next gesture, the Detail drag, while the third release's dissolve may run: its first
    // tick is an input.
    let third = launch.at("third-gpu")?;
    gpu_drawn(third)?;
    let started = dissolve_from(
        step_events(launch, "third-release")?,
        third,
        "the third release",
    )?;
    let outcome = cancelled_or_ended(
        launch,
        "third-release",
        "detail-first",
        "slider_draft_begin",
        "input",
    )?;
    checks.note(
        launch.at("detail-first")?,
        "the next gesture cancelled the release's dissolve, or it had ended first",
        json!({"dissolve": started, "outcome": outcome}),
    );

    // The moved gradient's Apply, and the stroke: each dissolves from its GPU frame, and the
    // editor is idle after the last. The moved gradient's frame carries its handles, which its
    // applied frame does not, so its pixels are held to the CPU's by the patches above rather than
    // over the whole photograph.
    let moved = launch.at("move-gpu")?;
    let applied = dissolve_from(
        span_events(launch, "move-apply", "apply-settled")?,
        moved,
        "the gradient's Apply",
    )?;
    checks.note(
        launch.at("apply-settled")?,
        "the gradient's Apply dissolved from its GPU frame",
        json!({"dissolve": applied}),
    );
    let stroke = stroke_dissolves(step_events(launch, "stroke")?)?;
    let idle = idle_passed(launch, "stroke-idle")?;
    checks.note(
        launch.at("stroke-idle")?,
        "the stroke dissolved from its GPU frame, then idle",
        json!({"dissolve": stroke["commit"], "held": stroke["held"], "idle": idle}),
    );
    Ok(())
}

/// The stroke's dissolves. Its release's commit dissolves once, from the GPU frame on screen when
/// it started, over that frame's boundary. Before it, the design's held case may dissolve too
/// (`docs/design/gpu-preview.md`, "Settle and the dissolve"): when the stroke's first job brings its
/// frame and boundary only after the next position has gone to the CPU with a job of its own — one
/// 60 ms interval, which a loaded host's first job outlasts — the surface draws that position's plan
/// over the new boundary, and its own CPU frame, of the same revision, then dissolves in over it,
/// until the next position's tick cancels the dissolve. Each such dissolve must start from a GPU
/// frame this stroke drew, over the same boundary, and must end or be cancelled before the commit's
/// begins; any other dissolve, or a commit from another frame, fails.
fn stroke_dissolves(events: &[Value]) -> Result<Value> {
    let started: Vec<(usize, &Value)> = events
        .iter()
        .enumerate()
        .filter(|(_, event)| event["event"] == "gpu_dissolve_started")
        .collect();
    let details: Vec<&Value> = started.iter().map(|(_, event)| &event["detail"]).collect();
    let committed: Vec<usize> = started
        .iter()
        .enumerate()
        .filter(|(_, (_, event))| event["detail"]["case"] == json!("committed"))
        .map(|(index, _)| index)
        .collect();
    ensure(
        committed.len() == 1 && committed[0] == started.len() - 1,
        format!("The stroke's commit did not dissolve once, last: {details:?}"),
    )?;
    let (at, commit) = started[committed[0]];
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
    let on_screen = drawn
        .iter()
        .rev()
        .find(|(_, _, index)| *index < at)
        .ok_or("The stroke drew no GPU frame before its commit")?;
    ensure(
        commit["detail"]["from"] == *on_screen.0
            && commit["detail"]["gpu_boundary"] == *on_screen.1,
        format!(
            "The stroke's commit did not dissolve from its GPU frame, revision {} over boundary {}: \
             {details:?}",
            on_screen.0, on_screen.1
        ),
    )?;
    let mut held = Vec::new();
    for (index, dissolve) in &started[..committed[0]] {
        let detail = &dissolve["detail"];
        let from_drawn = drawn.iter().any(|(revision, boundary, drawn_at)| {
            *drawn_at < *index
                && detail["from"] == **revision
                && detail["gpu_boundary"] == **boundary
        });
        let finished = events[index + 1..at].iter().any(|event| {
            (event["event"] == "gpu_dissolve_cancelled" || event["event"] == "gpu_dissolve_ended")
                && event["detail"]["from"] == detail["from"]
        });
        ensure(
            detail["case"] == json!("held") && from_drawn && finished,
            format!(
                "The stroke dissolved before its commit other than from a frame it drew, held \
                 and finished before the commit: {details:?}"
            ),
        )?;
        held.push(detail.clone());
    }
    Ok(json!({"commit": commit["detail"], "held": held}))
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

    /// The commit's dissolve alone, from the last GPU frame; and with the held dissolve a loaded
    /// host's late first job brings, from a frame the stroke drew and cancelled by the next tick.
    #[test]
    fn a_strokes_commit_dissolves_from_its_gpu_frame_after_any_held_one() {
        let plain = [drawn(2), drawn(3), drawn(25), started("committed", 25)];
        let checked = stroke_dissolves(&plain).expect("the commit's dissolve");
        assert_eq!(checked["held"], json!([]));
        let loaded = [
            drawn(2),
            started("held", 2),
            cancelled(2),
            drawn(3),
            drawn(25),
            started("committed", 25),
        ];
        let checked = stroke_dissolves(&loaded).expect("a held dissolve before the commit's");
        assert_eq!(checked["held"].as_array().map(Vec::len), Some(1));
    }

    /// What is not the design's: no commit dissolve, two, a commit from an older frame, a held
    /// dissolve still running at the commit or from a frame the stroke never drew, and any other
    /// case before the commit.
    #[test]
    fn a_stroke_fails_any_other_dissolve() {
        let cases: [(&str, Vec<Value>); 6] = [
            ("no commit dissolve", vec![drawn(25)]),
            (
                "two commit dissolves",
                vec![
                    drawn(25),
                    started("committed", 25),
                    started("committed", 25),
                ],
            ),
            (
                "a commit from an older frame",
                vec![drawn(24), drawn(25), started("committed", 24)],
            ),
            (
                "a held dissolve still running",
                vec![
                    drawn(2),
                    started("held", 2),
                    drawn(25),
                    started("committed", 25),
                ],
            ),
            (
                "a held dissolve from a frame never drawn",
                vec![
                    started("held", 2),
                    cancelled(2),
                    drawn(25),
                    started("committed", 25),
                ],
            ),
            (
                "a commit that is not the last dissolve",
                vec![
                    drawn(2),
                    started("committed", 2),
                    cancelled(2),
                    drawn(25),
                    started("held", 25),
                ],
            ),
        ];
        for (name, events) in cases {
            assert!(stroke_dissolves(&events).is_err(), "{name} passed");
        }
    }
}
