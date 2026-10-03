//! The `gpu-preview-zoom` smoke scenario: drags at 100% and above drawn on the GPU over the visible
//! region at full scale, with no preview job per tick, on the real editor
//! (`docs/design/gpu-preview.md`, "At 100% and above").
//!
//! One launch over the quadrant fixture the `gpu-preview` scenario uses. A Basic exposure drag at
//! 100% and at 200% opens with a CPU tick whose region job carries the one boundary request, for
//! the region the view shows; once the boundary is held and the sequence compiled, its ticks are
//! drawn on the GPU with no preview job — no tick's, and no region job for the view — the plan's
//! region holding the view, their pixels against the CPU frame the release commits. At 800%, where
//! the view shows a corner of the photograph, the drag is panned across it while it ticks: the pan
//! past the held region lets that boundary go and a later tick asks for the new region's, and every
//! frame drawn on the GPU draws a region that holds the view it was captured with. Before that, back
//! at 100%, Presence is committed with Dehaze and Clarity: a Texture and a Clarity drag read Dehaze's
//! light from the store the exact frames filled and run at most five compute passes a tick; a Basic
//! drag under it, whose light the region alone cannot give, keeps the CPU path and names
//! `region-estimate`; and with Dehaze back at neutral a Basic drag under Presence is drawn on the GPU,
//! running every pass a tick. Each Basic release's committed frame dissolves in from the drag's last
//! GPU frame: the dissolve's start and its identities are checked, and the release's capture either
//! shows it running or follows its end, which a capture after 150 ms allows.
use crate::{
    gpu_preview_smoke::{
        BASIC, CLARITY, CLARITY_DRAG, DEHAZE, EXPOSURE, PRESENCE, PRESENCE_QUIET_MS, Settled,
        TEXTURE_DRAG, UNDER_DRAG, dissolve_from, drag_steps, gpu_drawn, named,
        presence_drag_checks, quiet, quiet_for, same_pixels, step_events, ticks,
    },
    scenario::{Checked, Checks, Frame, Plan, Run, Step, plan::only},
    *,
};
use luxforge_evidence::{SliderStep, ViewStep};

pub const SCENARIO: &str = "gpu-preview-zoom";
pub use crate::gpu_preview_smoke::FIXTURE;

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
/// The drag back at 100% after the pan: its first tick, then its GPU ticks.
const BACK: [f64; 3] = [1.0, 0.8, 0.6];

/// Every frame, in order: the open, then one per step.
pub fn plan(_: &[PathBuf]) -> Plan {
    let mut steps = vec![Step::opened("opened").no_draft().masks(0)];
    // 1-12: at 100% and at 200%, the drag's first tick asks for the region's boundary, its later
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
        steps.extend([
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
    // 13-38: back at 100%, Presence committed, its drags drawn on the GPU, a Basic drag under it
    // refused while Dehaze's light would come from the region alone, and drawn once Dehaze is
    // neutral. Each drag's first tick changes Presence's shape, which a percentage zoom does not
    // warm, so its boundary wait also covers a compile.
    let release = |name: &str, field: &str, value: f64| {
        Step::new(name, SliderStep::new(PRESENCE, field, [value]).release())
            .commits(1)
            .no_draft()
    };
    steps.extend([
        Step::new("presence-100", ViewStep::Percent(100.0))
            .commits(0)
            .no_draft(),
        release("dehaze-100", "dehaze", DEHAZE),
        release("clarity-commit-100", "clarity", CLARITY),
        quiet_for("presence-settled-100", PRESENCE_QUIET_MS).no_draft(),
    ]);
    steps.extend(drag_steps(
        "texture-100",
        PRESENCE,
        "texture",
        TEXTURE_DRAG,
        PRESENCE_QUIET_MS,
        Settled::Quiet,
    ));
    steps.extend(drag_steps(
        "clarity-100",
        PRESENCE,
        "clarity",
        CLARITY_DRAG,
        PRESENCE_QUIET_MS,
        Settled::Quiet,
    ));
    steps.extend([
        Step::new(
            "under-refused-100",
            SliderStep::new(BASIC, EXPOSURE, [UNDER_DRAG[0]]),
        )
        .commits(0),
        Step::new(
            "under-refused-release-100",
            SliderStep::new(BASIC, EXPOSURE, [UNDER_DRAG[0]]).release(),
        )
        .commits(1)
        .no_draft(),
        release("dehaze-off-100", "dehaze", 0.0),
    ]);
    steps.extend(drag_steps(
        "under-100",
        BASIC,
        EXPOSURE,
        UNDER_DRAG,
        PRESENCE_QUIET_MS,
        Settled::Quiet,
    ));
    // 39-43: at 800%, a drag panned past its region as it ticks, then ticked over the new region,
    // then released.
    steps.extend([
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
        // 44-48: back to 100%, where the whole photograph fits the surface and the scrollable
        // reports no offset: the view is the whole stage, not the corner the pan left, and a drag
        // there is drawn on the GPU over it.
        Step::new("back-100", ViewStep::Percent(100.0))
            .commits(0)
            .no_draft(),
        Step::new("back-first", SliderStep::new(BASIC, EXPOSURE, [BACK[0]]))
            .commits(0)
            .draft(BASIC, json!({ EXPOSURE: BACK[0] })),
        quiet("back-held").draft(BASIC, json!({ EXPOSURE: BACK[0] })),
        Step::new(
            "back-gpu",
            SliderStep::new(BASIC, EXPOSURE, [BACK[1], BACK[2]]),
        )
        .commits(0)
        .draft(BASIC, json!({ EXPOSURE: BACK[2] })),
        Step::new(
            "back-release",
            SliderStep::new(BASIC, EXPOSURE, [BACK[2]]).release(),
        )
        .commits(1)
        .no_draft(),
    ]);
    Plan::new(steps)
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

/// The release's dissolve, from the drag's GPU frame `gpu` into the committed frame of the view:
/// exactly one began, with the GPU frame's revision and boundary, and none was cancelled or cut
/// during the release; it may follow a newer frame of the same content. The release's capture shows it running at its own progress, or, when the
/// capture came after its 150 ms, its end is recorded: never a timing it must meet.
fn settled_through_a_dissolve(launch: &Checked, release: &str, gpu: &Frame) -> Result<Value> {
    let events = step_events(launch, release)?;
    let started = dissolve_from(events, gpu, release)?;
    let interrupted: Vec<&Value> = ["gpu_dissolve_cancelled", "gpu_dissolve_cut"]
        .iter()
        .flat_map(|name| named(events, name))
        .collect();
    ensure(
        interrupted.is_empty(),
        format!("{release}: its dissolve was interrupted: {interrupted:?}"),
    )?;
    // The dissolve follows a newer frame of the same content: the exact region, then the whole
    // exact frame of the committed entry.
    let targets: Vec<&Value> = std::iter::once(&started["to"])
        .chain(
            named(events, "gpu_dissolve_retargeted")
                .into_iter()
                .map(|event| &event["detail"]["to"]),
        )
        .collect();
    let captured = launch.at(release)?;
    let drawn = &captured.state()["surface"]["gpu"]["dissolve"];
    let outcome = if drawn.is_null() {
        json!({"captured": "after it ended",
            "ended": named(events, "gpu_dissolve_ended")
                .first()
                .map(|event| event["detail"].clone())})
    } else {
        ensure(
            drawn["from"] == started["from"] && targets.contains(&&drawn["to"]),
            format!(
                "{} drew the dissolve {drawn}, not the one that began, {started}, to {targets:?}",
                captured["file"]
            ),
        )?;
        json!({"captured": "while it ran", "drawn": drawn, "targets": targets})
    };
    Ok(json!({"started": started, "outcome": outcome}))
}

/// A frame at a percentage zoom drawn on the GPU drew a region holding the view it was captured
/// with; one that drew the CPU's frame was handed no plan, or one the view leaves. Whichever path
/// drew it, so the check holds whatever the capture's timing.
fn never_mixed(frame: &Frame) -> Result<Value> {
    let gpu = &frame.state()["surface"]["gpu"];
    let (visible, plan) = regions(frame);
    if gpu["drawing_path"] == json!("gpu") {
        ensure(
            visible
                .zip(plan)
                .is_some_and(|(visible, plan)| holds(plan, visible)),
            format!(
                "{} was drawn on the GPU over region {plan:?}, which does not hold the view \
                 {visible:?}",
                frame["file"]
            ),
        )?;
    }
    Ok(
        json!({"file": frame["file"], "drawing_path": gpu["drawing_path"],
        "visible_region": visible, "plan_region": plan}),
    )
}

pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let mut checks = Checks::new();

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
                "At {zoom}% the first tick was not a CPU frame asking for the boundary: \
                 {gpu_ticks} GPU and {cpu_ticks} CPU ticks, {asked} asking"
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
                "At {zoom}% the boundary held is not the visible region's from one request: \
                 {summary}, the view {visible:?}"
            ),
        )?;
        let dragged = launch.at(gpu_name)?;
        let drawn = gpu_drawn(dragged)?;
        let (gpu_ticks, cpu_ticks, jobs) = ticks(step_events(launch, gpu_name)?);
        ensure(
            gpu_ticks >= 1 && cpu_ticks == 0 && jobs == 0,
            format!(
                "At {zoom}% the later ticks were {gpu_ticks} on the GPU and {cpu_ticks} on the \
                 CPU, with {jobs} preview jobs"
            ),
        )?;
        let view = never_mixed(dragged)?;
        let settled = settled_through_a_dissolve(launch, release_name, dragged)?;
        let compared = same_pixels(dragged, launch.at(release_name)?)?;
        checks.note(
            dragged,
            &format!("the drag at {zoom}% drawn on the GPU over the visible region"),
            json!({"drawn": drawn, "gpu_ticks": gpu_ticks, "jobs": jobs, "view": view,
                "boundary": summary["boundary"], "against_release": compared,
                "settled": settled}),
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
            "After the pan the ticks were {gpu_ticks} on the GPU and {cpu_ticks} on the CPU, \
             with {jobs} preview jobs"
        ),
    )?;
    let views = [panned, held, moved]
        .into_iter()
        .map(never_mixed)
        .collect::<Result<Vec<Value>>>()?;
    let settled = settled_through_a_dissolve(launch, "pan-release", moved)?;
    // Back at 100%: the view is the whole stage, whatever offset the pan at 800% left, and the
    // drag there is drawn on the GPU over it.
    let back = launch.at("back-100")?;
    let (visible, _) = regions(back);
    ensure(
        visible == Some([0, 0, 480, 320]),
        format!("Back at 100% the view is {visible:?}, not the whole 480 × 320 stage"),
    )?;
    let again = launch.at("back-gpu")?;
    let drawn_back = gpu_drawn(again)?;
    let view_back = never_mixed(again)?;
    let (gpu_ticks_back, cpu_ticks_back, jobs_back) = ticks(step_events(launch, "back-gpu")?);
    ensure(
        gpu_ticks_back >= 1 && cpu_ticks_back == 0 && jobs_back == 0,
        format!(
            "Back at 100% the ticks were {gpu_ticks_back} on the GPU and {cpu_ticks_back} on the \
             CPU, with {jobs_back} preview jobs"
        ),
    )?;
    checks.note(
        again,
        "back at 100% after the pan: the whole stage in view, its drag drawn on the GPU",
        json!({"visible_region": visible, "drawn": drawn_back, "view": view_back}),
    );
    checks.note(
        moved,
        "the drag at 800% panned past its region, then drawn over the new one",
        json!({"drawn": drawn, "released_for_a_new_key": changed,
            "boundary_requests": summary["boundary_requests"], "views": views,
            "settled": settled}),
    );

    // At 100%: the Presence drags over the stored light, the Basic drag under Presence refused
    // while Dehaze's light would come from the region alone, then drawn once it is neutral.
    for (name, approximate, gain_only) in [
        ("texture-100", false, true),
        ("clarity-100", false, true),
        ("under-100", false, false),
    ] {
        presence_drag_checks(launch, &mut checks, name, approximate, gain_only, false)?;
    }
    let refused = launch.at("under-refused-100")?;
    let events = step_events(launch, "under-refused-100")?;
    let (gpu_ticks, cpu_ticks, jobs) = ticks(events);
    let reasons: Vec<&Value> = named(events, "gpu_preview_tick")
        .iter()
        .map(|tick| &tick["detail"]["reason"])
        .collect();
    let summary = &refused.state()["surface"]["gpu"]["gpu_preview"]["drag"];
    ensure(
        gpu_ticks == 0
            && cpu_ticks >= 1
            && jobs >= 1
            && reasons
                .iter()
                .all(|reason| **reason == json!("region-estimate"))
            && summary["boundary_requests"] == json!(0),
        format!(
            "The Basic drag under Presence with Dehaze at 100% was {gpu_ticks} GPU and \
             {cpu_ticks} CPU ticks for {reasons:?}, asking for {} boundaries",
            summary["boundary_requests"]
        ),
    )?;
    checks.note(
        refused,
        "a Basic drag under Presence with Dehaze at 100% keeps the CPU path: the region alone \
         cannot give the light",
        json!({"cpu_ticks": cpu_ticks, "jobs": jobs, "reasons": reasons,
            "plan_fallback": refused.state()["surface"]["gpu"]["plan_fallback"]}),
    );

    checks.write(&launch.evidence, run.scenario(), json!({}))
}
