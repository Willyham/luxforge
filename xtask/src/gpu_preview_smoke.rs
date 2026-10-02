//! The `gpu-preview` smoke scenario: gestures at Fit drawn on the GPU over a held boundary, with
//! no preview job per tick, on the real editor (`docs/design/gpu-preview.md`, "A tick").
//!
//! One launch over the quadrant fixture. A Basic exposure drag opens with a CPU tick whose job
//! carries the one boundary request; once the boundary is held and the drag's program sequence
//! compiled, its ticks are drawn on the GPU, each frame tagged with its tick's draft revision; its
//! release commits, and the boundary and the slot are let go once the committed frame has settled.
//! Then, in Mask mode, a linear gradient bound to a masked exposure is moved by its middle handle
//! in two drags, the second drawn on the GPU with the coverage overlay following it, and a brush
//! stroke is painted through the same mask, its later positions drawn on the GPU.
//!
//! **Correlated readbacks.** Every frame drawn on the GPU is checked against the state the editor
//! recorded with it — the drawing path, the boundary version and the draft revision the surface
//! drew — and its pixels against the CPU's frame of the same settings: the drag's last value
//! against the frame its release commits, and the moved gradient, under the coverage tint, against
//! the frame its Apply commits, patch by patch.
//!
//! **At 100% and above.** Back in Pointer mode, the same drag at 100% and at 200% is drawn over the
//! visible region at full scale: its first tick's region job carries the one boundary request,
//! for the region the view shows, and its later ticks are drawn on the GPU with no preview job —
//! no tick's and no region job for the view — the plan's region holding the view, their pixels
//! against the CPU frame the release commits. At 800%, where the view shows a corner of the
//! photograph, the drag is panned across it while it ticks: the pan past the held region lets
//! that boundary go and the next tick asks for the new region's, and every frame drawn on the GPU
//! draws a region that holds the view it was captured with.
use crate::{
    scenario::{Checked, Checks, Frame, Plan, Run, Step, plan::only},
    *,
};
use luxforge_core::BASIC_EFFECT;
use luxforge_evidence::{
    self as script, DragHandle, MaskStep, PaintStep, Reference, SliderStep, ViewStep, WorkspaceStep,
};

pub const SCENARIO: &str = "gpu-preview";
/// Four flat quadrants: every patch below reads one colour, which a drag moves by a known amount.
pub const FIXTURE: &str = "fixtures/s0/orientation-1.jpg";

const BASIC: &str = "set-basic";
const EXPOSURE: &str = "exposure";
/// The drag's values: its first tick, before any boundary is held, then its GPU ticks.
const FIRST: f64 = 0.25;
const DRAGGED: [f64; 2] = [0.5, 0.75];
const MASKED: f64 = 1.0;
/// How long the scenario leaves the editor alone for the boundary to arrive and the sequence to
/// compile, and for a committed frame to settle. Generous: a 320 × 480 boundary renders in a few
/// milliseconds and a sequence compiles in tens.
const QUIET_MS: u64 = 1500;

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

/// The percentage zooms a drag is drawn at over the visible region, each with its first tick's
/// value, its GPU ticks' values and its steps' names.
const ZOOMED: [(f32, f64, [f64; 2], [&str; 6]); 2] = [
    (
        100.0,
        0.5,
        [0.25, 0.1],
        [
            "zoom-100",
            "drag-100-first",
            "boundary-100",
            "drag-100-gpu",
            "release-100",
            "settled-100",
        ],
    ),
    (
        200.0,
        0.3,
        [0.45, 0.6],
        [
            "zoom-200",
            "drag-200-first",
            "boundary-200",
            "drag-200-gpu",
            "release-200",
            "settled-200",
        ],
    ),
];
/// The zoom the pan is drawn at, where the view shows a corner of the photograph.
const PANNED_ZOOM: f32 = 800.0;
/// The panned drag's values, a tick each at [`PAN_INTERVAL_MS`], and the scroll offset each is
/// sent at: three over the centre, then three past the region the first ones held.
const PAN_VALUES: [f64; 6] = [0.7, 0.8, 0.9, 1.0, 1.1, 1.2];
const PAN_PATH: [[f32; 2]; 6] = [
    [0.5, 0.5],
    [0.5, 0.5],
    [0.5, 0.5],
    [0.95, 0.95],
    [0.95, 0.95],
    [0.95, 0.95],
];
const PAN_INTERVAL_MS: u64 = 150;
/// The panned drag's later ticks, over the new region.
const PANNED: [f64; 2] = [1.3, 1.4];

fn quiet(name: &str) -> Step {
    Step::new(name, script::Step::Wait { ms: QUIET_MS }).commits(0)
}

/// Every frame, in order: the open, then one per step.
pub fn plan(_: &[PathBuf]) -> Plan {
    let mask = |name: &str, step: MaskStep| Step::new(name, script::Step::Mask(step));
    let mut zoomed = vec![
        // 20: Pointer mode, where the Basic panel edits the photograph's own layer.
        Step::new(
            "pointer-mode",
            script::Step::Workspace(WorkspaceStep::default().mode("pointer")),
        )
        .commits(0)
        .mode("pointer"),
    ];
    // 21-32: at 100% and at 200%, the drag's first tick asks for the region's boundary, its later
    // ticks are drawn on the GPU, and its release commits.
    for (zoom, first, dragged, names) in ZOOMED {
        let [
            zoom_name,
            first_name,
            held_name,
            gpu_name,
            release_name,
            settled_name,
        ] = names;
        zoomed.extend([
            Step::new(zoom_name, ViewStep::Percent(zoom))
                .commits(0)
                .no_draft(),
            Step::new(first_name, SliderStep::new(BASIC, EXPOSURE, [first]))
                .commits(0)
                .draft(BASIC, json!({ EXPOSURE: first })),
            quiet(held_name).draft(BASIC, json!({ EXPOSURE: first })),
            Step::new(gpu_name, SliderStep::new(BASIC, EXPOSURE, dragged))
                .commits(0)
                .draft(BASIC, json!({ EXPOSURE: dragged[1] })),
            Step::new(
                release_name,
                SliderStep::new(BASIC, EXPOSURE, [dragged[1]]).release(),
            )
            .commits(1)
            .no_draft(),
            quiet(settled_name).no_draft(),
        ]);
    }
    // 33-37: at 800%, a drag panned past its region as it ticks, then ticked over the new region,
    // then released.
    zoomed.extend([
        Step::new("zoom-800", ViewStep::Percent(PANNED_ZOOM))
            .commits(0)
            .no_draft(),
        Step::new(
            "pan-drag",
            SliderStep::new(BASIC, EXPOSURE, PAN_VALUES)
                .paced(PAN_INTERVAL_MS)
                .pan_path(PAN_PATH.to_vec()),
        )
        .commits(0)
        .draft(BASIC, json!({ EXPOSURE: PAN_VALUES[5] })),
        quiet("pan-held").draft(BASIC, json!({ EXPOSURE: PAN_VALUES[5] })),
        Step::new("pan-gpu", SliderStep::new(BASIC, EXPOSURE, PANNED))
            .commits(0)
            .draft(BASIC, json!({ EXPOSURE: PANNED[1] })),
        Step::new(
            "pan-release",
            SliderStep::new(BASIC, EXPOSURE, [PANNED[1]]).release(),
        )
        .commits(1)
        .no_draft(),
    ]);
    let mut steps = vec![
        Step::opened("opened").no_draft().masks(0),
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
        // 5: settled: the boundary and the slot let go.
        quiet("settled").no_draft(),
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
    ];
    steps.extend(zoomed);
    Plan::new(steps)
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

/// A captured frame's visible region and the region of the plan handed to the surface, each
/// `[x0, y0, x1, y1]` of the output stage.
fn regions(frame: &Frame) -> (Option<[u64; 4]>, Option<[u64; 4]>) {
    let gpu = &frame.state()["surface"]["gpu"];
    let rect = |value: &Value| serde_json::from_value::<[u64; 4]>(value.clone()).ok();
    (rect(&gpu["visible_region"]), rect(&gpu["plan_region"]))
}

fn holds(outer: [u64; 4], inner: [u64; 4]) -> bool {
    outer[0] <= inner[0] && outer[1] <= inner[1] && outer[2] >= inner[2] && outer[3] >= inner[3]
}

/// A frame at a percentage zoom drawn on the GPU drew a region holding the view it was captured
/// with; one that drew the CPU's frame was handed no plan, or one the view leaves.
fn never_mixed(frame: &Frame) -> Result<Value> {
    let gpu = &frame.state()["surface"]["gpu"];
    let (visible, plan) = regions(frame);
    if gpu["drawing_path"] == json!("gpu") {
        ensure(
            visible
                .zip(plan)
                .is_some_and(|(visible, plan)| holds(plan, visible)),
            format!(
                "{} was drawn on the GPU over region {plan:?}, which does not hold the view                  {visible:?}",
                frame["file"]
            ),
        )?;
    }
    Ok(
        json!({"file": frame["file"], "drawing_path": gpu["drawing_path"],
        "visible_region": visible, "plan_region": plan}),
    )
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

    // At 100% and 200%: the region's boundary from one request, its ticks drawn on the GPU with
    // no preview job, and their pixels the CPU's.
    for (zoom, _, _, names) in ZOOMED {
        let [_, first_name, held_name, gpu_name, release_name, _] = names;
        let first = launch.at(first_name)?;
        let events = step_events(launch, first_name)?;
        let (gpu_ticks, cpu_ticks, _) = ticks(events);
        let asked = named(events, "gpu_preview_tick")
            .iter()
            .filter(|tick| tick["detail"]["boundary_requested"] == json!(true))
            .count();
        ensure(
            first.state()["surface"]["gpu"]["drawing_path"] == json!("cpu")
                && gpu_ticks == 0
                && cpu_ticks >= 1
                && asked == 1,
            format!(
                "At {zoom}% the first tick was not a CPU frame asking for the boundary:                  {gpu_ticks} GPU and {cpu_ticks} CPU ticks, {asked} asking"
            ),
        )?;
        let held = launch.at(held_name)?;
        let summary = &held.state()["surface"]["gpu"]["gpu_preview"]["drag"];
        let (visible, _) = regions(held);
        let region = serde_json::from_value::<[u64; 4]>(summary["boundary"]["region"].clone())
            .ok()
            .map(|[x, y, width, height]| [x, y, x + width, y + height]);
        ensure(
            summary["boundary_requests"] == json!(1)
                && summary["zoom"] == json!(zoom)
                && region.is_some()
                && region == visible,
            format!(
                "At {zoom}% the boundary held is not the visible region's from one request:                  {summary}, the view {visible:?}"
            ),
        )?;
        let dragged = launch.at(gpu_name)?;
        let drawn = gpu_drawn(dragged)?;
        let (gpu_ticks, cpu_ticks, jobs) = ticks(step_events(launch, gpu_name)?);
        ensure(
            gpu_ticks >= 1 && cpu_ticks == 0 && jobs == 0,
            format!(
                "At {zoom}% the later ticks were {gpu_ticks} on the GPU and {cpu_ticks} on the                  CPU, with {jobs} preview jobs"
            ),
        )?;
        let view = never_mixed(dragged)?;
        let compared = same_pixels(dragged, launch.at(release_name)?)?;
        checks.note(
            dragged,
            &format!("the drag at {zoom}% drawn on the GPU over the visible region"),
            json!({"drawn": drawn, "gpu_ticks": gpu_ticks, "jobs": jobs, "view": view,
                "boundary": summary["boundary"], "against_release": compared}),
        );
    }

    // At 800%: the pan past the held region lets it go and asks for the new one; every frame
    // drawn on the GPU drew a region holding its view.
    let panned = launch.at("pan-drag")?;
    let events = step_events(launch, "pan-drag")?;
    let changed = named(events, "gpu_boundary_released")
        .iter()
        .filter(|event| event["detail"]["why"] == "key-changed")
        .count();
    let held = launch.at("pan-held")?;
    let summary = &held.state()["surface"]["gpu"]["gpu_preview"]["drag"];
    ensure(
        changed >= 1
            && summary["boundary_requests"]
                .as_u64()
                .is_some_and(|requests| requests >= 2),
        format!(
            "The pan let {changed} boundaries go for a new key, and the drag asked {} times",
            summary["boundary_requests"]
        ),
    )?;
    let moved = launch.at("pan-gpu")?;
    let drawn = gpu_drawn(moved)?;
    let (gpu_ticks, cpu_ticks, jobs) = ticks(step_events(launch, "pan-gpu")?);
    ensure(
        gpu_ticks >= 1 && cpu_ticks == 0 && jobs == 0,
        format!(
            "After the pan the ticks were {gpu_ticks} on the GPU and {cpu_ticks} on the CPU,              with {jobs} preview jobs"
        ),
    )?;
    let views = [panned, held, moved]
        .into_iter()
        .map(never_mixed)
        .collect::<Result<Vec<Value>>>()?;
    checks.note(
        moved,
        "the drag at 800% panned past its region, then drawn over the new one",
        json!({"drawn": drawn, "released_for_a_new_key": changed,
            "boundary_requests": summary["boundary_requests"], "views": views}),
    );

    checks.write(&launch.evidence, run.scenario(), json!({}))
}
