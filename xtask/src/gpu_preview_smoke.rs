//! The `gpu-preview` smoke scenario: gestures at Fit drawn on the GPU over a held boundary, with
//! no preview job per tick, on the real editor (`docs/design/gpu-preview.md`, "A tick").
//!
//! One launch over the quadrant fixture. A Basic exposure drag opens with a CPU tick whose job
//! carries the one boundary request; once the boundary is held and the drag's program sequence
//! compiled, its ticks are drawn on the GPU, each frame tagged with its tick's draft revision; its
//! release commits, and the boundary and the slot are let go once the committed frame has settled.
//! Then, in Mask mode, a linear gradient bound to a masked exposure is moved by its middle handle
//! in two drags, the second drawn on the GPU with the coverage overlay following it, and a brush
//! stroke is painted through the same mask, its later positions drawn on the GPU. Last, back in
//! the pointer mode, Presence is committed with Dehaze and Clarity, and a Texture drag, a Clarity
//! drag and a Basic drag under Presence are each drawn on the GPU after their first tick: the two
//! Presence drags read Dehaze's light from the store and run at most five of Presence's compute
//! passes a tick (the spatial passes the tick's words change), and the Basic drag, which changes
//! the light's input, takes the light on the GPU and runs them all.
//!
//! **Correlated readbacks.** Every frame drawn on the GPU is checked against the state the editor
//! recorded with it — the drawing path, the boundary version and the draft revision the surface
//! drew — and its pixels against the CPU's frame of the same settings: the drag's last value
//! against the frame its release commits, and the moved gradient, under the coverage tint, against
//! the frame its Apply commits, patch by patch.//!
//! **The settle hand-off.** Each commit that replaces a GPU frame dissolves to it from that
//! frame's own draft revision and boundary, as the editor records; the drag's GPU frame against
//! the settled frame that replaced it is held to the pointwise limits over the whole photograph,
//! and an `idle` step after the release's and the stroke's dissolves checks that nothing draws or
//! updates once they have ended. With the GPU preview turned off from the palette a drag takes the
//! CPU path, naming the preference, and its release starts no dissolve. With both clipping
//! overlays shown a drag is still drawn on the GPU, marking its own clipped pixels. A third drag's
//! release dissolves, and the next gesture's first tick cancels it.
use crate::{
    scenario::{Checked, Checks, Frame, Plan, Run, Step, plan::only},
    *,
};
use luxforge_core::BASIC_EFFECT;
use luxforge_evidence::{
    self as script, DragHandle, MaskStep, PaintStep, PaletteStep, Reference, SliderStep,
    WorkspaceStep,
};

pub const SCENARIO: &str = "gpu-preview";
/// Four flat quadrants: every patch below reads one colour, which a drag moves by a known amount.
pub const FIXTURE: &str = "fixtures/s0/orientation-1.jpg";

const BASIC: &str = "set-basic";
const EXPOSURE: &str = "exposure";
const PRESENCE: &str = "set-presence";
/// The committed Presence the drags start from, then each Presence drag's first tick and its GPU
/// ticks, one a step, so each GPU step's frame counts one tick's compute passes.
const DEHAZE: f64 = 40.0;
const CLARITY: f64 = 30.0;
const TEXTURE_DRAG: [f64; 3] = [20.0, 35.0, 50.0];
const CLARITY_DRAG: [f64; 3] = [40.0, 50.0, 60.0];
/// The Basic drag under Presence, from the committed exposure.
const UNDER_DRAG: [f64; 3] = [0.5, 0.4, 0.3];
/// The most compute passes a tick that changes only Texture's or Clarity's gain may run: Clarity's
/// five, which read Texture's output.
const GAIN_PASSES: u64 = 5;
/// The drag's values: its first tick, before any boundary is held, then its GPU ticks.
const FIRST: f64 = 0.25;
const DRAGGED: [f64; 2] = [0.5, 0.75];
const MASKED: f64 = 1.0;
/// How long the scenario leaves the editor alone for the boundary to arrive and the sequence to
/// compile, and for a committed frame to settle. Generous: a 320 × 480 boundary renders in a few
/// milliseconds and a sequence compiles in tens.
const QUIET_MS: u64 = 1500;
/// The same for a stack holding Presence, whose sequences compile in a few hundred milliseconds
/// each on one compile thread: a committed stack's warm list is compiled first, then the drag's own
/// sequence when its layer was not warmed.
const PRESENCE_QUIET_MS: u64 = 4000;

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

/// The exposure the preference-off drag moves through, the second GPU drag's, and the next
/// gesture's that cancels its dissolve.
const OFF: [f64; 2] = [0.6, 0.65];
const AGAIN: [f64; 2] = [0.4, 0.45];
const THIRD: [f64; 2] = [0.2, 0.25];
const CANCEL: f64 = 0.3;
/// An idle check: long enough a settle for a 150 ms dissolve to end and the slot to retire, then a
/// second over which nothing may draw.
const IDLE: script::IdleStep = script::IdleStep {
    settle_ms: 600,
    ms: 1000,
};

fn quiet(name: &str) -> Step {
    quiet_for(name, QUIET_MS)
}

fn quiet_for(name: &str, ms: u64) -> Step {
    Step::new(name, script::Step::Wait { ms }).commits(0)
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
            // A third drag on the GPU, whose release's dissolve the next gesture's first tick
            // cancels: the release's capture, with no overlay to derive, ends well inside it.
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
            Step::new("cancel-drag", SliderStep::new(BASIC, EXPOSURE, [CANCEL]))
                .commits(0)
                .draft(BASIC, json!({ EXPOSURE: CANCEL })),
            Step::new(
                "cancel-release",
                SliderStep::new(BASIC, EXPOSURE, [CANCEL]).release(),
            )
            .commits(1)
            .no_draft(),
            // 6-10: Mask mode, a linear gradient swept and applied.
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
            quiet_for("presence-settled", PRESENCE_QUIET_MS).no_draft(),
        ]
        .into_iter()
        .chain(drag_steps("texture", PRESENCE, "texture", TEXTURE_DRAG))
        .chain(drag_steps("clarity", PRESENCE, "clarity", CLARITY_DRAG))
        .chain(drag_steps("under", BASIC, EXPOSURE, UNDER_DRAG))
        .collect(),
    )
}

/// One drag over a held boundary, a step a tick: `<name>-first`, whose CPU tick asks for the
/// boundary; `<name>-held`, while it arrives and the surface first runs every pass; `<name>-gpu-1`
/// and `<name>-gpu-2`, a GPU tick each; `<name>-release`, which commits the last value; and
/// `<name>-settled`.
fn drag_steps(name: &str, action: &str, field: &str, values: [f64; 3]) -> Vec<Step> {
    let slider = |value: f64| SliderStep::new(action, field, [value]);
    vec![
        Step::new(format!("{name}-first"), slider(values[0])).commits(0),
        quiet_for(&format!("{name}-held"), PRESENCE_QUIET_MS),
        Step::new(format!("{name}-gpu-1"), slider(values[1])).commits(0),
        Step::new(format!("{name}-gpu-2"), slider(values[2])).commits(0),
        Step::new(format!("{name}-release"), slider(values[2]).release())
            .commits(1)
            .no_draft(),
        quiet(&format!("{name}-settled")).no_draft(),
    ]
}

/// The events of the step whose frame is `frame`: from its `script_step` record to the next.
fn step_events<'a>(launch: &'a Checked, frame: &str) -> Result<&'a [Value]> {
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

fn named<'a>(events: &'a [Value], name: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|event| event["event"] == name)
        .collect()
}

/// The GPU ticks and CPU ticks among `events`, and how many preview jobs went to the worker.
fn ticks(events: &[Value]) -> (usize, usize, usize) {
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
fn gpu_drawn(frame: &Frame) -> Result<Value> {
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
fn same_pixels(gpu: &Frame, cpu: &Frame) -> Result<Value> {
    let mut readings = Vec::new();
    for at in PATCHES {
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
    }

    settle_checks(launch, &mut checks)?;
    checks.write(&launch.evidence, run.scenario(), json!({}))
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
fn dissolve_from(events: &[Value], gpu: &Frame, what: &str) -> Result<Value> {
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
/// clipping marks of a GPU drag, and a dissolve cancelled by the next gesture.
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

    // A dissolve the next gesture cancels: its first tick is an input.
    let third = launch.at("third-gpu")?;
    gpu_drawn(third)?;
    let started = dissolve_from(
        step_events(launch, "third-release")?,
        third,
        "the third release",
    )?;
    let cancelled = named(
        span_events(launch, "third-release", "cancel-drag")?,
        "gpu_dissolve_cancelled",
    );
    let ended = named(
        span_events(launch, "third-release", "cancel-drag")?,
        "gpu_dissolve_ended",
    );
    ensure(
        cancelled.len() == 1
            && cancelled[0]["detail"]["why"] == json!("input")
            && cancelled[0]["detail"]["from"] == started["from"]
            && ended.is_empty(),
        format!(
            "The next gesture did not cancel the third release's dissolve: cancelled {:?}, \
             ended {:?} (a release capture longer than the 150 ms dissolve leaves nothing to \
             cancel)",
            cancelled
                .iter()
                .map(|event| &event["detail"])
                .collect::<Vec<_>>(),
            ended
                .iter()
                .map(|event| &event["detail"])
                .collect::<Vec<_>>()
        ),
    )?;
    checks.note(
        launch.at("cancel-drag")?,
        "the next gesture cancelled the release's dissolve",
        json!({"dissolve": started, "cancelled": cancelled[0]["detail"]}),
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
    let stroke = named(step_events(launch, "stroke")?, "gpu_dissolve_started");
    ensure(
        stroke.len() == 1 && stroke[0]["detail"]["case"] == json!("committed"),
        format!(
            "The stroke's commit did not dissolve from its GPU frame: {:?}",
            stroke
                .iter()
                .map(|event| &event["detail"])
                .collect::<Vec<_>>()
        ),
    )?;
    let idle = idle_passed(launch, "stroke-idle")?;
    checks.note(
        launch.at("stroke-idle")?,
        "the stroke dissolved, then idle",
        json!({"dissolve": stroke[0]["detail"], "idle": idle}),
    );
    Ok(())
}
