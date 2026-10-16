//! `editor-latency --mode minimap`: an actual minimap drag measured end to end, with the overview's
//! first latency, bytes and counters ([design], item 7 of "Implementation").
//!
//! One background launch opens the photograph at Fit with the Performance section open, crosses to
//! `--zoom` (200% by default; the card is shown from 200%) and waits for the card's ready overview,
//! then presses the viewport rectangle's centre on the card and drags it through `--samples` moves
//! around a loop of the stage, each a real routed minimap move. It releases, shows Select, where
//! the overview is let go, and Develop again, where it is made anew with the programs compiled.
//! Every figure comes from events the editor logs:
//!
//! - **pan to frame**: each move's `minimap_navigated` to the surface's first draw of the view it
//!   asked for — the draw of the view plan the GPU was handed for it (`gpu_at_rest`'s boundary), the
//!   CPU frame of the preview job it raised (`preview_displayed`'s generation), or, where neither
//!   was needed, the first draw after it. Neither is display scanout;
//! - **first overview**: the crossing's `script_step` to the first `overview_ready` (cold: the
//!   launch's first), and Develop's `script_step` to the first after it (warm);
//! - the overview's own retained GPU and CPU bytes and their peak over the captured frames, the
//!   process's memory and GPU allocation at Fit, with the overview at rest and at the end, and the
//!   overview lifecycle's counters: builds, sweeps, uploads and reuses over the drag and in all.
//!
//! The moves are paced by the evidence script, one per captured frame, not at a pointer's rate.
//!
//! [design]: ../../../docs/design/minimap.md
use super::*;
use luxforge_evidence::{MinimapPoint, MinimapStep, SelectStep, SelectWorkspace};

/// Moves a run may make: with the setup and the steps after the drag, the script stays inside
/// [`script::MAX_SCRIPT_STEPS`].
pub const MINIMAP_MOVES_MAX: usize = 48;
/// The moves a run makes when `--samples` is not given.
pub const MINIMAP_MOVES: usize = 40;
/// The zoom the card is first shown at, and the default measured view.
const MINIMAP_FROM: f32 = 200.0;
/// How long the editor is left alone where resources are read, in milliseconds: the Performance
/// section reads them at most once a second.
const HOLD_MS: u64 = 1200;
/// The loop the drag follows, as a radius about the stage's centre in its own fractions.
const LOOP_RADIUS: f64 = 0.35;

/// The overview lifecycle's counters a report names.
const COUNTERS: [&str; 7] = [
    "gpu_builds",
    "cpu_builds",
    "sweeps",
    "uploads",
    "reuses",
    "refusals",
    "stale",
];

/// The script and the step numbers it is measured by.
struct Script {
    steps: Vec<script::Step>,
    /// The step that crosses to the measured zoom.
    crossing: usize,
    /// The step after the overview's first ready, held for resources.
    ready_hold: usize,
    press: usize,
    moves: Vec<usize>,
    release: usize,
    develop: usize,
    /// The last step: held after the warm overview.
    end: usize,
}

/// The `index`th of `count` points on the drag's loop, in the card image's fractions.
fn loop_point(index: usize, count: usize) -> [f32; 2] {
    let turn = std::f64::consts::TAU * (index + 1) as f64 / count.max(1) as f64;
    [
        (0.5 + LOOP_RADIUS * turn.cos()) as f32,
        (0.5 + LOOP_RADIUS * turn.sin()) as f32,
    ]
}

fn minimap_script(options: &Options) -> Script {
    // Script steps are numbered from one, in the order the editor runs them.
    let mut steps = vec![
        script::Step::performance(true),
        script::Step::wait(HOLD_MS),
        script::Step::View(ViewStep::Percent(options.zoom.unwrap_or(MINIMAP_FROM))),
    ];
    let crossing = steps.len();
    steps.push(script::Step::Minimap(MinimapStep::Ready));
    steps.push(script::Step::wait(HOLD_MS));
    let ready_hold = steps.len();
    steps.push(script::Step::Minimap(MinimapStep::Press(
        MinimapPoint::Rectangle([0.5, 0.5]),
    )));
    let press = steps.len();
    let mut moves = Vec::new();
    let mut last = [0.5, 0.5];
    for index in 0..options.samples {
        last = loop_point(index, options.samples);
        steps.push(script::Step::Minimap(MinimapStep::Move(
            MinimapPoint::Image(last),
        )));
        moves.push(steps.len());
    }
    steps.push(script::Step::Minimap(MinimapStep::Release(
        MinimapPoint::Image(last),
    )));
    let release = steps.len();
    steps.push(script::Step::wait(HOLD_MS));
    steps.push(script::Step::Select(SelectStep::Switch(
        SelectWorkspace::Select,
    )));
    steps.push(script::Step::Select(SelectStep::Switch(
        SelectWorkspace::Develop,
    )));
    let develop = steps.len();
    steps.push(script::Step::Minimap(MinimapStep::Ready));
    steps.push(script::Step::wait(HOLD_MS));
    let end = steps.len();
    Script {
        steps,
        crossing,
        ready_hold,
        press,
        moves,
        release,
        develop,
        end,
    }
}

pub(super) fn run(root: &Path, out: &Path, bin: &Path, options: &Options) -> Result {
    ensure(
        (1..=MINIMAP_MOVES_MAX).contains(&options.samples),
        format!("Minimap samples must be 1..{MINIMAP_MOVES_MAX} moves of one drag"),
    )?;
    ensure(
        options
            .zoom
            .is_none_or(|zoom| (MINIMAP_FROM..=1600.0).contains(&zoom)),
        "The minimap is shown from 200%: --zoom is 200 to 1600, or omitted for 200",
    )?;
    let run = Run::tool(root, out, TOOL, bin, Duration::from_secs(90))?;
    run.check(|run| measure(run, options))
}

/// Where `step`'s `script_step` lies in `events`.
fn sent(events: &[Value], step: usize) -> Result<usize> {
    events
        .iter()
        .position(|event| event["event"] == "script_step" && event["detail"]["step"] == json!(step))
        .ok_or_else(|| format!("Step {step} was never sent").into())
}

/// From `step`'s `script_step` to the first `name` after it, in milliseconds.
fn to_first(events: &[Value], step: usize, name: &str) -> Result<f64> {
    let at = sent(events, step)?;
    let found = events[at + 1..]
        .iter()
        .find(|event| event["event"] == name)
        .ok_or_else(|| format!("No {name} followed step {step}"))?;
    Ok(elapsed(found)? - elapsed(&events[at])?)
}

/// How a pan's frame was recognised.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Drawn {
    /// The surface's first draw of the view plan the GPU was handed for the pan.
    Plan,
    /// The surface's first draw of the CPU frame the pan's preview job rendered.
    Frame,
    /// Neither was needed: the first draw after the pan.
    Scroll,
}

impl Drawn {
    fn name(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Frame => "frame",
            Self::Scroll => "scroll",
        }
    }
}

/// A move's pan to the first draw of the view it asked for: `(navigated_ms, drawn_ms, how)`, or
/// `None` when the move panned nothing or a later pan replaced its view before it was drawn.
fn pan_frame(events: &[Value], step: usize) -> Result<Option<(f64, f64, Drawn)>> {
    let at = sent(events, step)?;
    let next = events[at + 1..]
        .iter()
        .position(|event| event["event"] == "script_step")
        .map_or(events.len(), |offset| at + 1 + offset);
    let Some(navigated) = events[at + 1..next]
        .iter()
        .position(|event| event["event"] == "minimap_navigated")
        .map(|offset| at + 1 + offset)
    else {
        return Ok(None);
    };
    let from = elapsed(&events[navigated])?;
    // What the pan raised before the next pan: a view plan handed, or a preview job's frame.
    let later = events[navigated + 1..]
        .iter()
        .position(|event| event["event"] == "minimap_navigated")
        .map_or(events.len(), |offset| navigated + 1 + offset);
    let raised = &events[navigated + 1..later];
    let plan = raised
        .iter()
        .find(|event| event["event"] == "gpu_at_rest" && event["detail"]["region"].is_array())
        .and_then(|event| event["detail"]["boundary"].as_u64());
    let frame = raised
        .iter()
        .find(|event| {
            event["event"] == "preview_displayed" && event["detail"]["path"] != json!("gpu")
        })
        .and_then(|event| event["detail"]["generation"].as_u64());
    let draws = drawn_frames(&events[navigated + 1..])?;
    let found = match (plan, frame) {
        (Some(boundary), _) => draws
            .iter()
            .find(|(_, detail)| detail["boundary"].as_u64() == Some(boundary))
            .map(|(ms, _)| (*ms, Drawn::Plan)),
        (None, Some(generation)) => draws
            .iter()
            .find(|(_, detail)| detail["generation"].as_u64() == Some(generation))
            .map(|(ms, _)| (*ms, Drawn::Frame)),
        (None, None) => draws.first().map(|(ms, _)| (*ms, Drawn::Scroll)),
    };
    Ok(found.map(|(drawn, how)| (from, drawn, how)))
}

/// The overview summary of a captured frame.
fn overview(frame: &Value) -> &Value {
    &frame["state"]["surface"]["gpu"]["minimap_overview"]
}

/// The overview lifecycle's `name` counter in a captured frame.
fn counted(frame: &Value, name: &str) -> Result<u64> {
    overview(frame)["lifecycle"][name]
        .as_u64()
        .ok_or_else(|| format!("A captured frame has no overview lifecycle {name}").into())
}

/// The overview's own bytes in a captured frame: the pipeline's GPU bytes, and the reference
/// overview's codes held for upload.
fn overview_bytes(frame: &Value) -> (Option<f64>, Option<f64>) {
    let summary = overview(frame);
    (
        summary["gpu_bytes"].as_f64(),
        summary["lifecycle"]["cpu_bytes"].as_f64(),
    )
}

fn measure(run: &mut Run, options: &Options) -> Result {
    let (root, out) = (&run.root().to_path_buf(), &run.out().to_path_buf());
    let source = options.source.canonicalize()?;
    let source_hash = hash(&source)?;
    let script = minimap_script(options);
    ensure(
        script.steps.len() <= script::MAX_SCRIPT_STEPS,
        "The minimap drag exceeds the 64-step evidence bound",
    )?;
    let load_start = launch::load_average(root);
    let launch = rendered_by(
        gesture_launch(
            out,
            "minimap",
            "minimap-script.json",
            &script.steps,
            &source,
            false,
            options.window,
        ),
        options,
    )
    .watch(sampled(root, "minimap"));
    let Launched {
        dir: evidence,
        watched: usage,
    } = run.launch(launch)?;
    let app = read_json(&evidence.join("result.json"))?;
    ensure(
        app["status"] == "captured"
            && app["had_input_errors"] == json!(false)
            && app["script"]
                .as_array()
                .is_some_and(|recorded| recorded.len() == script.steps.len()),
        format!("A minimap step failed or never ran: {}", app["script"]),
    )?;
    let events = scenario::events(&evidence.join("events.jsonl"))?;
    let header = run.provenance(&events)?;
    let frames = app["frames"].as_array().ok_or("Missing frames")?;
    // The opened frame comes first, then one frame per step.
    let frame = |step: usize| frame_at(frames, step, "minimap");
    let pressed = events[sent(&events, script.press)?..]
        .iter()
        .find(|event| event["event"] == "minimap_pressed")
        .ok_or("The press reached no minimap")?;
    ensure(
        pressed["detail"]["grabbed"] == json!(true),
        format!(
            "The press did not grab the viewport rectangle: {}",
            pressed["detail"]
        ),
    )?;
    let mut pans = Vec::new();
    let mut per_move = Vec::new();
    let mut unpanned = 0;
    let mut replaced = 0;
    let mut how = BTreeMap::<&str, usize>::new();
    for &step in &script.moves {
        match pan_frame(&events, step)? {
            Some((navigated, drawn, by)) => {
                pans.push(drawn - navigated);
                *how.entry(by.name()).or_default() += 1;
                per_move.push(json!({"step":step,"pan_to_frame_ms":drawn - navigated,
                    "drawn":by.name()}));
            }
            None => {
                let navigated = events[sent(&events, step)?..]
                    .iter()
                    .take_while(|event| {
                        event["event"] != "script_step" || event["detail"]["step"] == json!(step)
                    })
                    .any(|event| event["event"] == "minimap_navigated");
                if navigated {
                    replaced += 1;
                } else {
                    unpanned += 1;
                }
                per_move.push(json!({"step":step,"pan_to_frame_ms":null,
                    "drawn":if navigated { "replaced" } else { "no pan" }}));
            }
        }
    }
    ensure(
        !pans.is_empty(),
        "No minimap move was drawn: the drag measured nothing",
    )?;
    let cold = to_first(&events, script.crossing, "overview_ready")?;
    let warm = to_first(&events, script.develop, "overview_ready")?;
    // Counters over the drag: from the frame before the press to the one after the release.
    let (before, after) = (frame(script.press - 1)?, frame(script.release)?);
    let last = frame(script.end)?;
    let mut drag_counters = serde_json::Map::new();
    let mut total_counters = serde_json::Map::new();
    let mut rows = vec![
        stats::row("minimap_pan_to_frame", "ms", pans.iter().copied()),
        stats::scalar("first_overview_cold", "ms", Some(cold)),
        stats::scalar("first_overview_warm", "ms", Some(warm)),
    ];
    for name in COUNTERS {
        let drag = counted(after, name)?.saturating_sub(counted(before, name)?);
        let total = counted(last, name)?;
        drag_counters.insert(name.into(), json!(drag));
        total_counters.insert(name.into(), json!(total));
        rows.push(counter(
            &format!("overview_{name}_drag"),
            "count",
            Some(drag),
        ));
        rows.push(counter(
            &format!("overview_{name}_total"),
            "count",
            Some(total),
        ));
    }
    let bytes: Vec<(Option<f64>, Option<f64>)> = frames.iter().map(overview_bytes).collect();
    let peak = |gpu: bool| {
        bytes
            .iter()
            .filter_map(|(on_gpu, on_cpu)| if gpu { *on_gpu } else { *on_cpu })
            .reduce(f64::max)
    };
    let (held_gpu, held_cpu) = overview_bytes(frame(script.ready_hold)?);
    rows.push(stats::scalar(
        "overview_gpu_bytes_retained",
        "bytes",
        held_gpu,
    ));
    rows.push(stats::scalar(
        "overview_cpu_bytes_retained",
        "bytes",
        held_cpu,
    ));
    rows.push(stats::scalar(
        "overview_gpu_bytes_peak",
        "bytes",
        peak(true),
    ));
    rows.push(stats::scalar(
        "overview_cpu_bytes_peak",
        "bytes",
        peak(false),
    ));
    let fit = frame(script.crossing - 1)?;
    let held = frame(script.ready_hold)?;
    for (label, shown) in [("fit", fit), ("overview_at_rest", held), ("end", last)] {
        rows.push(stats::scalar(
            &format!("memory_{label}"),
            "MiB",
            resource_mib(shown, &["memory", "bytes"]),
        ));
        rows.push(stats::scalar(
            &format!("gpu_allocated_{label}"),
            "MiB",
            resource_mib(shown, &["gpu", "allocated_bytes"]),
        ));
    }
    rows.extend(resource_rows(&usage, last));
    let load_end = launch::load_average(root);
    let mut result = json!({
        "status":"passed",
        "source":source,
        "source_sha256":source_hash,
        "source_dimensions":held["state"]["source_dimensions"],
        "preview_dimensions":held["state"]["preview_dimensions"],
        "backend":held["state"]["backend"],
        "physical_size":held["physical_size"],
        "scale":held["scale"],
        "mode":"minimap",
        "zoom":options.zoom.unwrap_or(MINIMAP_FROM),
        "reference_renderer":options.no_gpu_render,
        "samples":options.samples,
        "drawn":how,
        "unpanned_moves":unpanned,
        "replaced_moves":replaced,
        "drag_counters":drag_counters,
        "total_counters":total_counters,
        "overview_at_rest":overview(held),
        "method":format!("Background evidence launch of the release binary, warm filesystem cache, the Performance section open. The view crosses from Fit to the measured zoom and waits for the card's ready overview, then one drag on the card: a press on the viewport rectangle's centre, {} moves round a loop of radius {LOOP_RADIUS} of the stage about its centre, each a minimap move step routed as the card's own, and the release. Moves are paced one per captured frame, not at a pointer's rate. Pan to frame is each move's minimap_navigated to the surface's first draw of the view it asked for: the view plan the GPU was handed for it (gpu_at_rest's boundary), the CPU frame its preview job rendered (preview_displayed's generation), or where neither was needed the first draw after it; not display scanout. First overview is the crossing step's script_step to the first overview_ready (cold, the launch's first) and the Develop step's, after Select let the overview go, to the first after it (warm, its programs compiled). Overview bytes are the photograph surface's overview GPU bytes and the lifecycle's reference codes held for upload, retained after the first ready and peak over the captured frames; memory and GPU allocation are the Performance section's figures at Fit, with the overview at rest and at the end.", options.samples),
        "rows":rows,
        "moves":per_move,
        "load":launch::load(load_start),
        "load_average_1m_end":load_end,
        "resources":{"rss_samples":usage["rss_samples"]},
        "scope":"One minimap drag at the measured zoom on this source: pan to frame per move, the first overview cold and warm, the overview's bytes and the lifecycle's counters over the drag and in all.",
    });
    report_geometry(&mut result, options);
    stamp(&mut result, &header);
    write_json(&out.join("latency.json"), &result)?;
    ensure(hash(&source)? == source_hash, "The source changed")?;
    println!("PASS editor latency (minimap): {}", out.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(name: &str, ms: f64, detail: Value) -> Value {
        json!({"event": name, "elapsed_ms": ms, "detail": detail})
    }

    #[test]
    fn a_pan_is_timed_to_the_first_draw_of_the_plan_it_raised() {
        let events = vec![
            event("script_step", 0.0, json!({"step": 7})),
            event("minimap_navigated", 1.0, json!({"pan": [10.0, 0.0]})),
            event(
                "surface_frame_drawn",
                3.0,
                json!({"boundary": 4, "drawn_ms": 3.0}),
            ),
            event(
                "gpu_at_rest",
                9.0,
                json!({"boundary": 5, "region": [0, 0, 9, 9]}),
            ),
            event(
                "surface_frame_drawn",
                17.0,
                json!({"boundary": 5, "drawn_ms": 16.5}),
            ),
            event("script_step", 40.0, json!({"step": 8})),
        ];
        assert_eq!(
            pan_frame(&events, 7).unwrap(),
            Some((1.0, 16.5, Drawn::Plan))
        );
        // A move that panned nothing is no sample.
        let still = vec![
            event("script_step", 0.0, json!({"step": 7})),
            event("script_step", 40.0, json!({"step": 8})),
        ];
        assert_eq!(pan_frame(&still, 7).unwrap(), None);
    }

    #[test]
    fn the_drag_fits_the_script_bound_and_loops_round_the_stage() {
        let source = Path::new("fixtures/generated/24mp.jpg");
        let options = Options {
            source,
            samples: MINIMAP_MOVES_MAX,
            mode: Mode::Minimap,
            control: Control::Slider,
            action: None,
            parameter: None,
            crop: None,
            idle: false,
            basic: false,
            presence: false,
            curve_layer: false,
            detail: false,
            lens: false,
            perspective: false,
            mask: false,
            zoom: None,
            moving_pan: false,
            mask_overlay: false,
            contend: None,
            warm_ms: None,
            no_gpu_render: false,
            masks: 1,
            mask_presence: false,
            window: None,
            usage: None,
        };
        let script = minimap_script(&options);
        assert!(script.steps.len() <= script::MAX_SCRIPT_STEPS);
        assert_eq!(script.moves.len(), MINIMAP_MOVES_MAX);
        assert_eq!(script.end, script.steps.len());
        let [x, y] = loop_point(MINIMAP_MOVES_MAX - 1, MINIMAP_MOVES_MAX);
        assert!((x - 0.85).abs() < 1e-6 && (y - 0.5).abs() < 1e-6);
    }
}
