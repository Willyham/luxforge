//! The `gpu-preview-zoom` smoke scenario: drags at percentage zooms drawn on the GPU with no
//! preview job per tick, on the real editor: at 100% and above over the visible region at full
//! scale, and below 100% over the displayed-size proxy of the whole stage
//! (`docs/design/gpu-preview.md`, "At 100% and above" and "Below 100%").
//!
//! One launch over the quadrant fixture the `gpu-preview` scenario uses. A Basic exposure drag at
//! 100% draws from the region's boundary, a window of the source cut at full scale, which the
//! view's own job or the drag's first tick derives on the GPU; its first tick is on the CPU only
//! until the surface has evaluated the plan and compiled its sequence, and its later ticks are
//! drawn on the GPU with no preview job — no tick's, and no region job for the view — the plan's
//! region holding the view, their pixels against the CPU frame the release commits. The boundary
//! stays on the GPU as the resident one when the drag ends, and at 200%, where the view still
//! shows the whole photograph, the same region at full scale, a drag draws from it at its first
//! tick, deriving nothing. At 800%, where the view shows a corner of the photograph, the drag is
//! panned across it while it ticks: the pan past the held region lets that boundary go and a later
//! tick derives the new region's, and every frame drawn on the GPU draws a region that holds the
//! view it was captured with. Before that, back at 100%, Presence is committed with Dehaze and Clarity: a
//! Texture and a Clarity drag read Dehaze's light from the store the exact frames filled and run at
//! most five compute passes a tick; a Basic drag under it, whose light the region alone cannot
//! give, keeps the CPU path and names `region-estimate`; and with Dehaze back at neutral a Basic
//! drag under Presence is drawn on the GPU, running every pass a tick. Then Detail is committed
//! under Presence and Dehaze again, so Dehaze's light sits behind Detail, where the CPU cannot cut
//! the view's region and draws the whole exact frame: a Texture drag reads the light that frame
//! stored and a Detail drag the one its starting stack stored, held for the drag and named
//! approximate, both on the GPU; Dehaze then goes back to neutral, Detail staying under Presence
//! for the drags after. Each Basic release's committed frame dissolves in from the drag's last GPU
//! frame: the dissolve's start and its identities are checked, and the release's capture either
//! shows it running or follows its end, which a capture after 150 ms allows.
//!
//! A second, short launch drags Basic's exposure at 50% and then at 33%, where the view draws the
//! displayed-size proxy of the whole stage, as Fit draws the display-bounded one. Each drag draws
//! from the source reduced to that proxy on the GPU, derived by the view's own job or the drag's
//! first tick; once the surface has evaluated the plan its ticks are drawn on the GPU from a whole
//! frame's plan with no preview job,
//! each GPU frame's boundary, draft revision, budget figures and label checked against its state,
//! the same settings drawn twice to the same bytes, and the status bar saying nothing of the zoom.
//! The release's committed proxy frame dissolves in from the drag's last GPU frame, whose pixels
//! are the CPU frame's of the same settings, within the pointwise limits, and the boundary stays
//! resident. (The first launch's script holds the evidence's 64 steps.)
use crate::{
    gpu_preview_smoke::{
        BASIC, CLARITY, CLARITY_DRAG, DEHAZE, EXPOSURE, Held, PRESENCE, PRESENCE_QUIET_MS, Settled,
        TEXTURE_DRAG, UNDER_DRAG, at_rest_after, drag_steps, gpu_drawn, named,
        presence_drag_checks, quiet, quiet_for, resident_kept, same_pixels, settle_report,
        span_events, step_events, ticks,
    },
    scenario::{Checked, Checks, Frame, Plan, Run, Step},
    *,
};
use luxforge_evidence::{SliderStep, ViewStep};

pub const SCENARIO: &str = "gpu-preview-zoom";
pub use crate::gpu_preview_smoke::FIXTURE;

/// A percentage zoom a drag is drawn at over the visible region.
struct Zoomed {
    zoom: f32,
    /// Whether the drag's first tick draws from the resident boundary an earlier drag left over the
    /// same region, rather than deriving one.
    resident: bool,
    /// The first tick's value, then the GPU ticks' values.
    first: f64,
    dragged: [f64; 2],
    /// The steps' names: the zoom, the first tick, the wait, the GPU ticks, the release and the
    /// wait after it.
    names: [&'static str; 6],
}

const ZOOMED: [Zoomed; 2] = [
    Zoomed {
        zoom: 100.0,
        resident: false,
        first: 0.5,
        dragged: [0.25, 0.1],
        names: [
            "zoom-100",
            "drag-100-first",
            "boundary-100",
            "drag-100-gpu",
            "release-100",
            "settled-100",
        ],
    },
    Zoomed {
        zoom: 200.0,
        resident: true,
        first: 0.3,
        dragged: [0.45, 0.6],
        names: [
            "zoom-200",
            "drag-200-first",
            "boundary-200",
            "drag-200-gpu",
            "release-200",
            "settled-200",
        ],
    },
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
/// Dehaze behind Detail at 100%: the Detail layer committed under Presence, a Texture drag over
/// it and a Detail drag from it.
const DETAIL: &str = "set-detail";
const SHARPENING: &str = "sharpening";
const BEHIND_DETAIL: f64 = 40.0;
const BEHIND_TEXTURE: [f64; 3] = [30.0, 40.0, 60.0];
const BEHIND_SHARPEN: [f64; 3] = [55.0, 70.0, 85.0];

/// A drag in the second launch, below 100%, over the displayed-size proxy of the whole stage.
struct Below {
    zoom: f32,
    /// The first tick's value, then the GPU ticks' values, which a second step draws again.
    first: f64,
    dragged: [f64; 2],
    /// The steps' names: the zoom, the first tick, the wait, the GPU ticks, the same ticks again,
    /// the release and the wait after it.
    names: [&'static str; 7],
}

/// At 50%, then at 33%, whose proxy is another and whose first tick lets the one at 50% go.
const BELOW: [Below; 2] = [
    Below {
        zoom: 50.0,
        first: 0.5,
        dragged: [0.25, 0.1],
        names: [
            "zoom-50",
            "drag-50-first",
            "boundary-50",
            "drag-50-gpu",
            "drag-50-again",
            "release-50",
            "settled-50",
        ],
    },
    Below {
        zoom: 33.0,
        first: 0.3,
        dragged: [0.45, 0.6],
        names: [
            "zoom-33",
            "drag-33-first",
            "boundary-33",
            "drag-33-gpu",
            "drag-33-again",
            "release-33",
            "settled-33",
        ],
    },
];

/// The second launch's frames, in order: the open, then for each zoom below 100% the zoom, the
/// drag's first tick, the wait while the surface evaluates its plan, its GPU ticks, the same ticks again, its
/// release and the settle after it.
pub fn below_plan(_: &[PathBuf]) -> Plan {
    let mut steps = vec![Step::opened("opened-below").no_draft().masks(0)];
    for Below {
        zoom,
        first,
        dragged,
        names,
    } in BELOW
    {
        let [
            zoom_name,
            first_name,
            held_name,
            gpu_name,
            again_name,
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
            Step::new(again_name, SliderStep::new(BASIC, EXPOSURE, dragged))
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
    Plan::new(steps)
}

/// Every frame of the first launch, in order: the open, then one per step.
pub fn plan(_: &[PathBuf]) -> Plan {
    let mut steps = vec![Step::opened("opened").no_draft().masks(0)];
    // 1-12: at 100% and at 200%, the drag's first tick asks for the region's boundary at 100% and
    // draws from the resident one at 200%, its later ticks are drawn on the GPU, and its release
    // commits.
    for Zoomed {
        zoom,
        first,
        dragged,
        names,
        ..
    } in ZOOMED
    {
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
    // warm, so the wait after it also covers a compile.
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
        Held::Compiled(PRESENCE_QUIET_MS),
        Settled::Quiet,
    ));
    steps.extend(drag_steps(
        "clarity-100",
        PRESENCE,
        "clarity",
        CLARITY_DRAG,
        Held::Compiled(PRESENCE_QUIET_MS),
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
        Held::Compiled(PRESENCE_QUIET_MS),
        Settled::Quiet,
    ));
    // Still at 100%, Detail committed under Presence and Dehaze committed again, so Dehaze's light
    // sits behind Detail and the CPU cannot cut the view's region: a Presence drag reads the light
    // the whole exact frame stored, and a Detail drag the one the stack it started from stored,
    // held for the drag. Dehaze then goes back to neutral for the drags after them, which a
    // Detail layer under Presence leaves on the GPU; the evidence script's 64 steps hold no more,
    // so a Basic drag between Detail and Presence, which keeps the CPU path as the one above does,
    // is the core's to prove (`behind_detail_a_region_plan_holds_dehazes_stored_light`).
    let detail = |name: &str, value: f64| {
        Step::new(name, SliderStep::new(DETAIL, SHARPENING, [value]).release())
            .commits(1)
            .no_draft()
    };
    steps.extend([
        detail("behind-detail-commit-100", BEHIND_DETAIL),
        release("behind-dehaze-100", "dehaze", DEHAZE),
        quiet_for("behind-settled-100", PRESENCE_QUIET_MS).no_draft(),
    ]);
    steps.extend(drag_steps(
        "behind-texture-100",
        PRESENCE,
        "texture",
        BEHIND_TEXTURE,
        Held::Compiled(PRESENCE_QUIET_MS),
        Settled::Quiet,
    ));
    steps.extend(drag_steps(
        "behind-sharpen-100",
        DETAIL,
        SHARPENING,
        BEHIND_SHARPEN,
        Held::Compiled(PRESENCE_QUIET_MS),
        Settled::Quiet,
    ));
    steps.push(release("behind-dehaze-off-100", "dehaze", 0.0));
    // Then at 800%, a drag panned past its region as it ticks, then ticked over the new region,
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
        // Then back to 100%, where the whole photograph fits the surface and the scrollable
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

/// The release `release` starts no dissolve into a CPU frame: the committed stack at rest is
/// drawn by the GPU, its view plan over the region the view shows, as the drag's frame was, and
/// the release's own frame, whichever path drew it, holds the view it was captured with.
fn released_with_no_dissolve(launch: &Checked, release: &str) -> Result<Value> {
    let started = named(step_events(launch, release)?, "gpu_dissolve_started");
    ensure(
        started.is_empty(),
        format!(
            "{release} dissolved into a CPU frame: {:?}",
            started
                .iter()
                .map(|event| &event["detail"])
                .collect::<Vec<_>>()
        ),
    )?;
    never_mixed(launch.at(release)?)
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

/// The photograph's pixels in two frames of the same settings, byte for byte: on one machine the
/// GPU draws the same stack to the same bytes frame to frame.
fn same_bytes(first: &Frame, second: &Frame) -> Result<Value> {
    let rect = first.visible_photo()?;
    ensure(
        second.visible_photo()? == rect,
        format!(
            "{} and {} show different rectangles",
            first["file"], second["file"]
        ),
    )?;
    let (one, other) = (first.image()?, second.image()?);
    let [left, top, right, bottom] = rect;
    let differing = (top..bottom)
        .flat_map(|y| (left..right).map(move |x| (x, y)))
        .filter(|&(x, y)| one.get_pixel(x, y) != other.get_pixel(x, y))
        .count();
    ensure(
        differing == 0,
        format!(
            "{} and {} draw the same settings {differing} pixels apart",
            first["file"], second["file"]
        ),
    )?;
    Ok(
        json!({"first": first["file"], "second": second["file"], "rect": rect,
        "pixels": (right - left) * (bottom - top), "differing": differing}),
    )
}

/// Whether a tick's reason for the CPU path is one that passes within a frame or two: the surface
/// has not evaluated the plan, its sequence still compiles, or the source still uploads. A tick on
/// the GPU names none.
fn passing(reason: &Value) -> bool {
    reason.is_null()
        || ["surface-pending", "compiling", "source-uploading"]
            .iter()
            .any(|passing| reason == passing)
}

/// The drags below 100%, each over the displayed-size proxy of the whole stage, planned as Fit's
/// is at the view's bounds: the first tick drawn from the boundary the view's own job derived from
/// the source, reduced to that proxy, or one the tick derives, on the CPU only for a reason that
/// passes, with nothing said of the zoom; the boundary held the whole proxy stage at the view's
/// bounds, the size of the CPU frame the view draws; the later ticks drawn on the GPU from a whole
/// frame's plan with no preview job and nothing derived again, each frame's boundary, revision,
/// budget figures and label its state's, the same settings drawn twice to the same bytes; the
/// release's committed proxy frame dissolving in from the last GPU frame, whose pixels are the CPU
/// frame's of the same settings within the pointwise limits; and the boundary kept as the
/// resident one, the view's.
fn drawn_on_the_gpu_below_100(launch: &Checked, checks: &mut Checks) -> Result {
    for Below { zoom, names, .. } in BELOW {
        let [
            _,
            first_name,
            held_name,
            gpu_name,
            again_name,
            release_name,
            settled_name,
        ] = names;
        let first = launch.at(first_name)?;
        let events = step_events(launch, first_name)?;
        let (gpu_ticks, cpu_ticks, _) = ticks(events);
        let first_ticks = named(events, "gpu_preview_tick");
        let reasons: Vec<&Value> = first_ticks
            .iter()
            .map(|tick| &tick["detail"]["reason"])
            .collect();
        let state = first.state();
        let derived = state["surface"]["gpu"]["gpu_preview"]["drag"]["boundaries_derived"].clone();
        ensure(
            gpu_ticks + cpu_ticks >= 1
                && derived.as_u64().is_some_and(|derived| derived <= 1)
                && reasons.iter().all(|reason| passing(reason))
                && state["status_bar"]["fallback"].is_null(),
            format!(
                "At {zoom}% the first tick did not draw from the proxy's boundary saying nothing: \
                 {gpu_ticks} GPU and {cpu_ticks} CPU ticks for {reasons:?}, {derived} derived, the \
                 status bar saying {}",
                state["status_bar"]["fallback"]
            ),
        )?;
        let held = launch.at(held_name)?;
        let state = held.state();
        let summary = &state["surface"]["gpu"]["gpu_preview"]["drag"];
        let boundary = &summary["boundary"];
        // The view's proxy at its bounds, the size of the frame the CPU's proxy phase draws there,
        // whichever frame the CPU last presented.
        let (bounds, raster) = (&state["proxy"]["bounds"], &state["surface"]["raster"]);
        ensure(
            summary["boundaries_derived"] == derived
                && summary["zoom"] == json!(zoom)
                && boundary["derived"] == json!("reduce")
                && boundary["region"].is_null()
                && boundary["proxy"]["bounds"] == json!([bounds["width"], bounds["height"]])
                && json!([boundary["width"], boundary["height"]])
                    == json!([boundary["proxy"]["width"], boundary["proxy"]["height"]])
                && boundary["width"]
                    .as_u64()
                    .zip(bounds["width"].as_u64())
                    .is_some_and(|(width, bound)| width <= bound)
                && boundary["height"]
                    .as_u64()
                    .zip(bounds["height"].as_u64())
                    .is_some_and(|(height, bound)| height <= bound),
            format!(
                "At {zoom}% the boundary held is not the source reduced to the view's proxy: \
                 {summary}, the view's bounds {bounds}, the CPU frame {raster}"
            ),
        )?;
        let (dragged, again) = (launch.at(gpu_name)?, launch.at(again_name)?);
        let mut drawn = Vec::new();
        for (name, frame) in [(gpu_name, dragged), (again_name, again)] {
            let figures = gpu_drawn(frame)?;
            let (gpu_ticks, cpu_ticks, jobs) = ticks(step_events(launch, name)?);
            let state = frame.state();
            let gpu = &state["surface"]["gpu"];
            ensure(
                gpu_ticks >= 1
                    && cpu_ticks == 0
                    && jobs == 0
                    && gpu["gpu_preview"]["drag"]["boundaries_derived"] == derived
                    && gpu["gpu_preview"]["drag"]["boundary"]["version"] == boundary["version"]
                    && gpu["plan_region"].is_null()
                    && gpu["visible_region"].is_null()
                    && state["status_bar"]["fallback"].is_null()
                    && gpu["fallback_notice"].is_null(),
                format!(
                    "At {zoom}% {} was {gpu_ticks} GPU and {cpu_ticks} CPU ticks with {jobs} \
                     preview jobs, drawn over {} by the plan of region {}, saying {}",
                    frame["file"],
                    gpu["gpu_preview"]["drag"]["boundary"],
                    gpu["plan_region"],
                    state["status_bar"]["fallback"]
                ),
            )?;
            drawn.push(json!({"drawn": figures, "gpu_ticks": gpu_ticks, "jobs": jobs}));
        }
        let repeated = same_bytes(dragged, again)?;
        let settled = launch.at(settled_name)?;
        let dissolved = at_rest_after(
            span_events(launch, release_name, settled_name)?,
            settled,
            release_name,
        )?;
        let compared = same_pixels(again, launch.at(release_name)?)?;
        // Below 100% the picture at rest is process-first, the stack at full resolution in tiles
        // reduced to the view, and the drag's frame processes the source reduced first: the
        // settle from one to the other is the drag's distance from the frame it settles to, which
        // the gate measures on the corpus; here the picture at rest dissolves in over the frame
        // before it, and the distance is recorded.
        let jumped = settle_report(again, settled)?;
        let rested = named(
            span_events(launch, release_name, settled_name)?,
            "gpu_rest_drawn",
        );
        ensure(
            rested
                .iter()
                .any(|event| !event["detail"]["dissolve"].is_null()),
            format!(
                "At {zoom}% the picture at rest did not dissolve in over the frame before it: \
                 {:?}",
                rested
                    .iter()
                    .map(|event| &event["detail"])
                    .collect::<Vec<_>>()
            ),
        )?;
        let version =
            &again.state()["surface"]["gpu"]["gpu_preview"]["drag"]["boundary"]["version"];
        let kept = resident_kept(launch, release_name, settled_name, version)?;
        let resident = &settled.state()["surface"]["gpu"]["gpu_preview"]["resident"];
        ensure(
            resident["proxy"] == boundary["proxy"],
            format!(
                "At {zoom}% the resident boundary {resident} is not the view's proxy {}",
                boundary["proxy"]
            ),
        )?;
        checks.note(
            again,
            &format!("the drag at {zoom}% drawn on the GPU from the displayed-size proxy"),
            json!({"first": {"derived": derived, "reasons": reasons}, "boundary": boundary,
                "view_bounds": bounds, "cpu_frame": raster, "drawn": drawn,
                "same_bytes": repeated, "settled": dissolved, "against_release": compared,
                "jump": jumped, "resident": kept}),
        );
    }
    Ok(())
}

pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let [launch, below] = launches else {
        return Err(format!("Expected two launches, found {}", launches.len()).into());
    };
    let mut checks = Checks::new();

    // At 100% the region's boundary, a window of the source cut at full scale, derived by the
    // view's own job or the drag's first tick, and at 200% the resident boundary the 100% drag
    // left over the same region, deriving none: the ticks drawn on the GPU with no preview job,
    // and their pixels the CPU's.
    let mut left: Option<Value> = None;
    for Zoomed {
        zoom,
        resident,
        names,
        ..
    } in ZOOMED
    {
        let [
            _,
            first_name,
            held_name,
            gpu_name,
            release_name,
            settled_name,
        ] = names;
        let first = launch.at(first_name)?;
        let events = step_events(launch, first_name)?;
        let (gpu_ticks, cpu_ticks, _) = ticks(events);
        let first_ticks = named(events, "gpu_preview_tick");
        let reasons: Vec<&Value> = first_ticks
            .iter()
            .map(|tick| &tick["detail"]["reason"])
            .collect();
        let derived =
            first.state()["surface"]["gpu"]["gpu_preview"]["drag"]["boundaries_derived"].clone();
        if resident {
            let left = left
                .as_ref()
                .ok_or("a boundary an earlier drag left resident")?;
            gpu_drawn(first)?;
            ensure(
                gpu_ticks >= 1
                    && cpu_ticks == 0
                    && derived == json!(0)
                    && first_ticks
                        .iter()
                        .all(|tick| &tick["detail"]["boundary"] == left),
                format!(
                    "At {zoom}% the first tick was not drawn from the resident boundary {left}: \
                     {gpu_ticks} GPU and {cpu_ticks} CPU ticks, {derived} derived"
                ),
            )?;
        } else {
            ensure(
                gpu_ticks + cpu_ticks >= 1
                    && derived.as_u64().is_some_and(|derived| derived <= 1)
                    && reasons.iter().all(|reason| passing(reason)),
                format!(
                    "At {zoom}% the first tick did not draw from the region's boundary: \
                     {gpu_ticks} GPU and {cpu_ticks} CPU ticks for {reasons:?}, {derived} derived"
                ),
            )?;
        }
        let held = launch.at(held_name)?;
        let summary = &held.state()["surface"]["gpu"]["gpu_preview"]["drag"];
        let (visible, _) = regions(held);
        let region = serde_json::from_value::<[u64; 4]>(summary["boundary"]["region"].clone())
            .ok()
            .map(|[x, y, width, height]| [x, y, x + width, y + height]);
        ensure(
            summary["boundaries_derived"] == derived
                && summary["boundary"]["derived"] == json!("cut")
                && summary["zoom"] == json!(zoom)
                && region.is_some()
                && region == visible,
            format!(
                "At {zoom}% the boundary held is not the visible region's, cut from the source: \
                 {summary}, the view {visible:?}"
            ),
        )?;
        left = Some(summary["boundary"]["version"].clone());
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
        let settled = at_rest_after(
            span_events(launch, release_name, settled_name)?,
            launch.at(settled_name)?,
            release_name,
        )?;
        let compared = same_pixels(dragged, launch.at(release_name)?)?;
        checks.note(
            dragged,
            &format!("the drag at {zoom}% drawn on the GPU over the visible region"),
            json!({"drawn": drawn, "gpu_ticks": gpu_ticks, "jobs": jobs, "view": view,
                "boundary": summary["boundary"], "against_release": compared,
                "settled": settled}),
        );
    }

    // At 800%: the pan past the held region lets it go and derives the new one; every frame
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
            && summary["boundaries_derived"]
                .as_u64()
                .is_some_and(|derived| derived >= 1),
        format!(
            "The pan let {changed} boundaries go for a new key, and the drag derived {}",
            summary["boundaries_derived"]
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
    let settled = released_with_no_dissolve(launch, "pan-release")?;
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
            "boundaries_derived": summary["boundaries_derived"], "views": views,
            "settled": settled}),
    );

    // At 100%: the Presence drags over the stored light, the Basic drag under Presence refused
    // while Dehaze's light would come from the region alone, then drawn once it is neutral.
    // Behind Detail: the Texture drag over the stored light and the Detail drag over the held
    // one, approximate, both drawn on the GPU over the whole stage the CPU cannot cut.
    for (name, approximate, gain_only) in [
        ("texture-100", false, true),
        ("clarity-100", false, true),
        ("under-100", false, false),
        ("behind-texture-100", false, true),
        ("behind-sharpen-100", true, false),
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
            && summary["boundaries_derived"] == json!(0),
        format!(
            "The Basic drag under Presence with Dehaze at 100% was {gpu_ticks} GPU and \
             {cpu_ticks} CPU ticks for {reasons:?}, deriving {} boundaries",
            summary["boundaries_derived"]
        ),
    )?;
    checks.note(
        refused,
        "a Basic drag under Presence with Dehaze at 100% keeps the CPU path: the region alone \
         cannot give the light",
        json!({"cpu_ticks": cpu_ticks, "jobs": jobs, "reasons": reasons,
            "plan_fallback": refused.state()["surface"]["gpu"]["plan_fallback"]}),
    );

    // Below 100%: drawn on the GPU from the displayed-size proxy, as at Fit.
    drawn_on_the_gpu_below_100(below, &mut checks)?;

    checks.write(&launch.evidence, run.scenario(), json!({}))
}
