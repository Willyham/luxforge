//! `editor-latency --mode corrections`: the Corrections workspace's gestures, measured end to end
//! as the paint mode measures a mask stroke ([design], "Acceptance"; [module], "Measurement").
//!
//! One run enters the mode, sets the New correction source with an `⌥`-click, then paints one
//! paced brush stroke with that set source, so every position drafts a real repair rather than an
//! automatic-source add that drafts nothing until its release. The stroke's release commits it and
//! selects it, and one paced drag of its source handle follows. Three figures come out of it, each
//! from events the editor already logs, paired the way the paint mode pairs a stroke's:
//!
//! - **brush**: each `correction_draft_set` to the frame that presented it, through the generation
//!   its `correction_draft_preview` names or, on the GPU, the draw of its tick's plan, and each
//!   position (`correction_position`) to the first presented frame that carried it;
//! - **commit**: the release, the `correction_position` that lets go, to the first undrafted
//!   `preview_displayed` after the `correction_draft_commit` it raised, which is the repaired frame;
//! - **source drag**: the drag's drafted frames and its own release to its repaired frame.
//!
//! - **overlay**: each `correction_position` to the first `correction_overlay_drawn` at or after it.
//!   The desktop logs that event, at the draw's own instant, for the first draw of the overlay (pins,
//!   outlines and the source link) after each message while a run logs; a run whose log holds none
//!   reports the figure as unmeasured rather than standing a drafted frame in for it.
//!
//! A refused render is a failed measurement, never a pass: every `preview_failed`, refused commit or
//! refused press in a measured window is listed, the report is written with `"status": "failed"`
//! and the run fails.
//!
//! [design]: ../../../docs/design/corrections-workspace.md
//! [module]: ../../../docs/design/corrections.md
use super::*;
use luxforge_evidence::{
    CorrectionDrag, CorrectionHandle, CorrectionStep, PressModifier, PressStep,
};

/// A paced correction gesture's events.
const CORRECTION: GestureEvents = GestureEvents {
    position: "correction_position",
    set: "correction_draft_set",
    preview: "correction_draft_preview",
};

/// The pacing: the paint mode's own, so a correction position and a mask position are each one
/// input at the same rate and their figures read side by side.
const INTERVAL_MS: u64 = PAINT_INTERVAL_MS;
/// How many positions the measured stroke and the source drag each take when `--samples` does not
/// say: a stroke of about three seconds under a hand.
pub const CORRECTION_POSITIONS: usize = 120;
/// The most positions one gesture takes. Both gestures' real time, `2 × 400 × 24 ms` (19.2 s),
/// stays well inside the editor's scripted-evidence deadline beside the setup ([`deadline`]).
const CORRECTION_POSITIONS_MAX: usize = 400;
/// Where the `⌥`-click sets the source: the stroke's first position plus the offset it measures
/// is where the stroke samples from, above it and clear of its path.
const SOURCE_AT: [f64; 2] = [0.45, 0.3];

/// The measured stroke: a gentle wave across the middle of the frame, below [`SOURCE_AT`], so the
/// path and the region it samples stay inside the photograph and apart.
fn stroke_path(positions: usize) -> Vec<[f64; 2]> {
    (0..positions)
        .map(|index| {
            let t = index as f64 / (positions.max(2) - 1) as f64;
            [
                0.35 + 0.25 * t,
                0.6 + 0.04 * (std::f64::consts::TAU * 2.0 * t).sin(),
            ]
        })
        .collect()
}

/// The source handle's path: from near where the set source put it, up and to the right. The drag
/// presses the handle where the selected correction holds it, so the path's start is a move.
fn source_path(positions: usize) -> Vec<[f64; 2]> {
    (0..positions)
        .map(|index| {
            let t = index as f64 / (positions.max(2) - 1) as f64;
            [SOURCE_AT[0] + 0.15 * t, SOURCE_AT[1] - 0.1 * t]
        })
        .collect()
}

/// The run's script: the recipe's preconditions, the mode, the set source, then the paced stroke
/// and the paced source drag.
pub(super) fn script(options: &Options) -> Vec<script::Step> {
    let mut steps = geometry_preconditions(options);
    if options.curve_layer {
        steps.push(crate::scenario::recipe::moderate_curve());
    }
    if options.detail {
        steps.push(crate::scenario::recipe::moderate_detail());
    }
    if options.basic {
        steps.push(basic_precondition(options));
    }
    if options.presence {
        steps.push(moderate_presence());
    }
    steps.extend(zoom_step(options));
    steps.push(script::Step::key("q"));
    steps.push(script::Step::Correction(CorrectionStep::Press(PressStep {
        at: SOURCE_AT,
        to: Vec::new(),
        modifiers: vec![PressModifier::Alt],
        release: true,
        interval_ms: None,
    })));
    steps.extend(options.warm_ms.map(|ms| script::Step::Wait { ms }));
    let mut path = stroke_path(options.samples);
    let at = path.remove(0);
    steps.push(script::Step::Correction(CorrectionStep::Press(PressStep {
        at,
        to: path,
        modifiers: Vec::new(),
        release: true,
        interval_ms: Some(INTERVAL_MS),
    })));
    steps.push(script::Step::Correction(CorrectionStep::Drag(
        CorrectionDrag {
            handle: CorrectionHandle::Source,
            to: source_path(options.samples),
            interval_ms: Some(INTERVAL_MS),
        },
    )));
    steps
}

/// How long a run may take: 90 s for the launch, the setup and the wrapper, as the paint mode
/// allows, and both paced gestures' own real time, each a press plus `samples` positions.
pub(super) fn deadline(options: &Options) -> Duration {
    Duration::from_secs(90) + Duration::from_millis(2 * INTERVAL_MS * (options.samples as u64 + 1))
}

pub(super) fn run(root: &Path, out: &Path, bin: &Path, options: &Options) -> Result {
    ensure(
        (2..=CORRECTION_POSITIONS_MAX).contains(&options.samples),
        format!(
            "Corrections samples must be 2..{CORRECTION_POSITIONS_MAX} positions; a stroke of one \
             position has no path, and both gestures' real time must stay inside the editor's \
             scripted-evidence deadline"
        ),
    )?;
    let run = Run::tool(root, out, TOOL, bin, deadline(options))?;
    run.check(|run| measure(run, options))
}

/// The events of one paced correction step: from its `script_step`, the step whose request is a
/// `kind` (`press` or `drag`) paced at [`INTERVAL_MS`], to the next step's.
fn window<'a>(events: &'a [Value], kind: &str) -> Option<&'a [Value]> {
    let start = events.iter().position(|event| {
        event["event"] == "script_step"
            && event["detail"]["request"]["correction"][kind]["interval_ms"].as_u64()
                == Some(INTERVAL_MS)
    })?;
    let end = events[start + 1..]
        .iter()
        .position(|event| event["event"] == "script_step")
        .map_or(events.len(), |offset| start + 1 + offset);
    Some(&events[start..end])
}

/// One measured gesture's rows, what it counted and what failed in it.
struct Gesture {
    rows: Vec<Value>,
    summary: Value,
    failures: Vec<String>,
    /// Its input-to-presented-frame samples, both paths.
    latencies: Vec<f64>,
}

/// The release, the commit it raised and the repaired frame after it: from the position that let
/// go to the first undrafted `preview_displayed` after the `correction_draft_commit` at or after
/// it. `(release_to_frame, commit_to_frame)`, either `None` when it never happened, with why.
fn repaired(events: &[Value]) -> Result<(Option<f64>, Option<f64>, Option<&'static str>)> {
    let Some(release) = events
        .iter()
        .find(|event| event["event"] == CORRECTION.position && event["detail"]["release"] == true)
    else {
        return Ok((None, None, Some("never released")));
    };
    let release = elapsed(release)?;
    let mut commit = None;
    for event in events
        .iter()
        .filter(|event| event["event"] == "correction_draft_commit")
    {
        let at = elapsed(event)?;
        if at >= release {
            commit = Some(at);
            break;
        }
    }
    let Some(commit) = commit else {
        return Ok((None, None, Some("released without a draft.commit")));
    };
    for event in events.iter().filter(|event| {
        event["event"] == "preview_displayed" && event["detail"]["draft_revision"].is_null()
    }) {
        let at = elapsed(event)?;
        if at >= commit {
            return Ok((Some(at - release), Some(at - commit), None));
        }
    }
    Ok((
        None,
        None,
        Some("no repaired frame was presented after its commit"),
    ))
}

/// Each position to the first overlay draw at or after it: `correction_overlay_drawn`'s own
/// `drawn_ms` less the position's `elapsed_ms`. A position no draw follows in the window has none,
/// and the draws are counted beside the figure.
fn overlay_latencies(events: &[Value]) -> Result<(Vec<f64>, usize)> {
    let mut draws = Vec::new();
    for event in events
        .iter()
        .filter(|event| event["event"] == "correction_overlay_drawn")
    {
        draws.push(
            event["detail"]["drawn_ms"]
                .as_f64()
                .ok_or("A correction_overlay_drawn event carries no drawn_ms")?,
        );
    }
    draws.sort_by(f64::total_cmp);
    let mut latencies = Vec::new();
    for event in events
        .iter()
        .filter(|event| event["event"] == CORRECTION.position)
    {
        let at = elapsed(event)?;
        if let Some(drawn) = draws.iter().find(|drawn| **drawn >= at) {
            latencies.push(drawn - at);
        }
    }
    Ok((latencies, draws.len()))
}

/// Every refusal a measured window logged: a preview job's (`preview_failed`, with its code), a
/// commit's (`correction_draft_refused`) and a press's (`correction_press_refused`,
/// `correction_limit_refused`).
fn refusals(events: &[Value]) -> Vec<Value> {
    events
        .iter()
        .filter_map(|event| {
            let name = event["event"].as_str()?;
            let detail = &event["detail"];
            match name {
                "preview_failed" => Some(json!({
                    "event": name,
                    "generation": detail["generation"],
                    "draft_revision": detail["draft_revision"],
                    "error_code": detail["error_code"],
                })),
                "correction_draft_refused" => {
                    Some(json!({"event": name, "reason": detail["reason"]}))
                }
                "correction_press_refused" => {
                    Some(json!({"event": name, "reason": detail["reason"]}))
                }
                "correction_limit_refused" => Some(json!({"event": name, "text": detail["text"]})),
                _ => None,
            }
        })
        .collect()
}

/// Analyse one paced gesture: its drafted frames on both paths, its press, its positions when its
/// previews say how many each carried (a stroke's do, a handle drag's do not), its release to the
/// repaired frame, and every refusal in it. Each row is named `<prefix>_<metric>`.
fn gesture(prefix: &str, label: &str, events: &[Value], positions: bool) -> Result<Gesture> {
    let (queued, phase) = phase_samples(events, CORRECTION)?;
    let (gpu_ticks, gpu) = gpu_samples(events, CORRECTION)?;
    let presented = presented_paint_frames(&phase, &gpu);
    let cpu_latencies: Vec<f64> = phase.iter().map(|s| s.input_to_presented_ms).collect();
    let gpu_latencies: Vec<f64> = gpu.iter().map(|s| s.input_to_presented_ms).collect();
    let latencies: Vec<f64> = cpu_latencies
        .iter()
        .chain(&gpu_latencies)
        .copied()
        .collect();
    let name = |metric: &str| format!("{prefix}_{metric}");
    let mut rows = vec![
        stats::row(&name("input_to_presented_frame"), "ms", latencies.clone()),
        stats::row(&name("gpu_input_to_presented_frame"), "ms", gpu_latencies),
        stats::row(&name("cpu_input_to_presented_frame"), "ms", cpu_latencies),
        stats::row(
            &name("gpu_tick_to_drawn_frame"),
            "ms",
            gpu.iter().map(|s| s.tick_to_drawn_ms),
        ),
    ];
    type Phase = fn(&PaintPhaseSample) -> f64;
    let phases: [(&str, Phase); 6] = [
        ("owner_round_trip_to_preview_queue", |s| {
            s.owner_round_trip_ms
        }),
        ("draft_set", |s| s.draft_set_ms),
        ("preview_job_planning", |s| s.preview_job_ms),
        ("preview_request_to_worker_start", |s| s.queue_wait_ms),
        ("preview_worker_render", |s| s.worker_render_ms),
        ("worker_result_to_surface_assignment", |s| {
            s.result_to_surface_ms
        }),
    ];
    for (metric, sample) in phases {
        rows.push(stats::row(&name(metric), "ms", phase.iter().map(sample)));
    }
    let (press_to_set, press_to_frame) = press_timing(events, CORRECTION, &presented)?;
    rows.push(stats::row(
        &name("press_to_first_draft_set"),
        "ms",
        press_to_set,
    ));
    rows.push(stats::row(
        &name("press_to_first_presented_frame"),
        "ms",
        press_to_frame,
    ));
    let carried = if positions {
        let carried = carried_latencies(events, CORRECTION, &presented)?;
        rows.push(stats::row(
            &name("position_to_presented_frame"),
            "ms",
            carried.iter().map(|(_, ms)| *ms),
        ));
        Some(carried.len())
    } else {
        None
    };
    let (overlay, overlay_draws) = overlay_latencies(events)?;
    rows.push(stats::row(
        &name("position_to_overlay_drawn"),
        "ms",
        overlay,
    ));
    let (release_to_frame, commit_to_frame, unrepaired) = repaired(events)?;
    rows.push(stats::row(
        &name("release_to_repaired_frame"),
        "ms",
        release_to_frame,
    ));
    rows.push(stats::row(
        &name("commit_to_repaired_frame"),
        "ms",
        commit_to_frame,
    ));

    let mut cpu_reasons: BTreeMap<String, usize> = BTreeMap::new();
    let mut gpu_tick_count = 0usize;
    for event in events.iter().filter(|e| e["event"] == "gpu_preview_tick") {
        if event["detail"]["path"] == "cpu" {
            let reason = event["detail"]["reason"].as_str().unwrap_or("none");
            *cpu_reasons.entry(reason.to_owned()).or_default() += 1;
        } else {
            gpu_tick_count += 1;
        }
    }
    let refused = refusals(events);
    let sets = events
        .iter()
        .filter(|e| e["event"] == CORRECTION.set)
        .count();
    let mut failures = Vec::new();
    if latencies.is_empty() {
        failures.push(format!("{label}: no drafted frame reached the screen"));
    }
    if let Some(why) = unrepaired {
        failures.push(format!("{label}: {why}"));
    }
    if !refused.is_empty() {
        let codes: BTreeSet<String> = refused
            .iter()
            .map(|refusal| {
                refusal["error_code"]
                    .as_str()
                    .or(refusal["event"].as_str())
                    .unwrap_or("refused")
                    .to_owned()
            })
            .collect();
        failures.push(format!(
            "{label}: {} refusals ({})",
            refused.len(),
            codes.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    let summary = json!({
        "interval_ms": INTERVAL_MS,
        "positions": events.iter().filter(|e| e["event"] == CORRECTION.position).count(),
        "draft_sets": sets,
        "inputs_that_queued_a_preview": queued,
        "gpu_ticks": gpu_ticks,
        "gpu_frames_drawn": gpu.len(),
        "displayed": latencies.len(),
        "superseded": (queued + gpu_ticks).saturating_sub(latencies.len()),
        "unpreviewed_sets": sets.saturating_sub(queued + gpu_ticks),
        "positions_carried_to_the_screen": carried,
        "overlay_draws": overlay_draws,
        "tick_paths": {"gpu": gpu_tick_count, "cpu_reasons": cpu_reasons},
        "refusals": refused,
    });
    Ok(Gesture {
        rows,
        summary,
        failures,
        latencies,
    })
}

/// Why each `script_step_failed` in the run failed, in order.
fn step_failures(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .filter(|event| event["event"] == "script_step_failed")
        .map(|event| {
            event["detail"]["reason"]
                .as_str()
                .unwrap_or("unknown")
                .to_owned()
        })
        .collect()
}

/// The run's report from its events and its last captured frame. `Err` only when the run cannot
/// be read at all; a measurement that failed is a report with `"status": "failed"` and its
/// `failures`, which [`measure`] writes before failing the run.
fn report(
    events: &[Value],
    app: &Value,
    steps: usize,
    options: &Options,
) -> Result<(Value, Vec<String>)> {
    let mut failures = Vec::new();
    let ran = app["script"].as_array().map_or(0, Vec::len);
    if app["status"] != "captured" {
        failures.push(format!("the run did not capture: {}", app["status"]));
    }
    if ran != steps {
        failures.push(format!("only {ran} of {steps} script steps ran"));
    }
    if app["had_input_errors"] != json!(false) {
        failures.push(format!(
            "script steps failed: {}",
            step_failures(events).join("; ")
        ));
    }
    if !events
        .iter()
        .any(|event| event["event"] == "correction_source_set")
    {
        failures.push("the ⌥-click set no source, so the stroke drafted no repair".into());
    }
    let mut rows = Vec::new();
    let mut sections = serde_json::Map::new();
    let mut brush_latencies = Vec::new();
    for (prefix, label, kind, positions) in [
        ("brush", "brush stroke", "press", true),
        ("source_drag", "source-handle drag", "drag", false),
    ] {
        let Some(events) = window(events, kind) else {
            failures.push(format!("{label}: the step never ran"));
            sections.insert(prefix.into(), Value::Null);
            continue;
        };
        // An event stream the pairing cannot read is a failed measurement of this gesture, reported
        // beside the other's rather than leaving the run without a report.
        let measured = match gesture(prefix, label, events, positions) {
            Ok(measured) => measured,
            Err(error) => {
                failures.push(format!("{label}: {error}"));
                sections.insert(prefix.into(), Value::Null);
                continue;
            }
        };
        if prefix == "brush" {
            brush_latencies = measured.latencies.clone();
        }
        rows.extend(measured.rows);
        failures.extend(measured.failures);
        sections.insert(prefix.into(), measured.summary);
    }
    let frames = app["frames"].as_array().map_or(&[][..], Vec::as_slice);
    let last = frames.last().unwrap_or(&Value::Null);
    let overlay_draws = events
        .iter()
        .filter(|event| event["event"] == "correction_overlay_drawn")
        .count();
    let not_measured = if overlay_draws == 0 {
        json!([{
            "metric": "input_to_overlay",
            "reason": "The run's log holds no correction_overlay_drawn event, so no input-to-overlay figure is read; brush_input_to_presented_frame is the drafted repair's, not the overlay's.",
        }])
    } else {
        json!([])
    };
    let brush_p95 = stats::Distribution::of(brush_latencies).map(|d| d.p95);
    let result = json!({
        "status": if failures.is_empty() { "passed" } else { "failed" },
        "failures": failures.clone(),
        "mode": "corrections",
        "samples": options.samples,
        "source_dimensions": last["state"]["source_dimensions"],
        "preview_dimensions": last["state"]["preview_dimensions"],
        "backend": last["state"]["backend"],
        "physical_size": last["physical_size"],
        "scale": last["scale"],
        "crop_angle_deg": options.crop,
        "full_basic_layer": options.basic,
        "detail_layer": options.detail,
        "presence_layer": options.presence,
        "reference_renderer": options.no_gpu_render,
        "method": format!("Background evidence launch of the release binary, warm filesystem cache. The Corrections mode is entered with Q and the New correction source set with an ⌥-click at {SOURCE_AT:?}. One brush stroke of {} positions is then handed to the desktop one per {INTERVAL_MS} ms in real time with that set source, so each position drafts a repair: the first tick presses, each later one moves and the last releases, one stroke and one history entry. Its release commits and selects it, and its source handle is dragged through {} positions at the same pace. Each correction_draft_set is paired with its preview job and displayed frame by the generation correction_draft_preview names, or, drawn on the GPU, with the GPU tick of its own update and the surface's first draw of that tick's plan; each stroke position with the first presented frame whose draft.set carried it; each release with the first undrafted preview_displayed after the correction_draft_commit it raised. Each position is also paired with the first correction_overlay_drawn at or after it, the desktop's log of the overlay canvas's first draw after each message at the draw's own instant. Neither presented nor drawn is display scanout.", options.samples, options.samples),
        "stroke": {"path": "x = 0.35 + 0.25t, y = 0.6 + 0.04 sin(4πt), t in 0..1", "source_set_at": SOURCE_AT},
        "source_drag": {"handle": "source", "path": format!("from the selected correction's source handle through x = {} + 0.15t, y = {} - 0.1t", SOURCE_AT[0], SOURCE_AT[1])},
        "gestures": sections,
        "overlay_draws": overlay_draws,
        "not_measured": not_measured,
        "provisional_input_to_frame_target": {"p95_below_ms": 16.0, "acceptable_below_ms": 32.0,
            "measured_p95_ms": brush_p95,
            "met": brush_p95.map(|ms| ms < 16.0), "acceptable": brush_p95.map(|ms| ms < 32.0),
            "note": "The paint gesture's provisional target, quoted for comparison; nothing is claimed from one run."},
        "rows": rows,
        "workspace": last["state"]["workspace"],
        "corrections": last["state"]["corrections"],
        "layers": last["state"]["stack"]["layers"],
        "scope": "correction_draft_set to the frame that presented it, and a release to its repaired frame, over one paced set-source brush stroke and one paced source-handle drag at this source's own size. A refused render, commit or press fails the run; it is never averaged away.",
        "checks": [
            "The ⌥-click set a source before the measured stroke",
            "Every drafted interval pairs one correction_draft_set with the frame of the generation or GPU tick it raised",
            "Every release reaches an undrafted presented frame after its commit",
            "Every position pairs with the first overlay draw at or after it, when the run logged overlay draws",
            "No render, commit or press was refused in a measured gesture",
            "Source SHA-256 is unchanged",
        ],
    });
    Ok((result, failures))
}

fn measure(run: &mut Run, options: &Options) -> Result {
    let (root, out) = (&run.root().to_path_buf(), &run.out().to_path_buf());
    let source = options.source.canonicalize()?;
    let source_hash = hash(&source)?;
    let steps = script(options);
    ensure(
        steps.len() <= script::MAX_SCRIPT_STEPS,
        "The corrections script holds more steps than one run accepts",
    )?;
    let load_start = launch::load_average(root);
    let gesture = gesture_launch(
        out,
        "gesture",
        "gesture-script.json",
        &steps,
        &source,
        false,
        options.window,
    )
    .watch(sampled(root, "gesture"));
    let gesture = rendered_by(gesture, options);
    let Launched {
        dir: evidence,
        watched: usage,
    } = run.launch(gesture)?;
    let app = read_json(&evidence.join("result.json"))?;
    let events = scenario::events(&evidence.join("events.jsonl"))?;
    let header = run.provenance(&events)?;
    let (mut result, mut failures) = report(&events, &app, steps.len(), options)?;
    let last = app["frames"]
        .as_array()
        .and_then(|frames| frames.last())
        .cloned()
        .unwrap_or(Value::Null);
    if let Some(rows) = result["rows"].as_array_mut() {
        rows.extend(resource_rows(&usage, &last));
    }
    result["source"] = json!(source);
    result["source_sha256"] = json!(source_hash);
    result["load"] = launch::load(load_start);
    result["load_average_1m_end"] = json!(launch::load_average(root));
    result["resources"] = json!({
        "scratch": last["state"]["scratch"],
        "rss_samples": usage["rss_samples"],
        "note": "RSS and process CPU time are sampled together by ps about every 50 ms, as the paint mode samples them.",
    });
    if hash(&source)? != source_hash {
        failures.push("the source changed".into());
        result["status"] = json!("failed");
        result["failures"] = json!(failures);
    }
    report_geometry(&mut result, options);
    stamp(&mut result, &header);
    write_json(&out.join("latency.json"), &result)?;
    if !failures.is_empty() {
        return Err(format!(
            "Corrections latency failed ({}): {}",
            out.join("latency.json").display(),
            failures.join("; ")
        )
        .into());
    }
    println!("PASS editor latency (corrections): {}", out.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(source: &Path) -> Options<'_> {
        Options {
            source,
            samples: 4,
            mode: Mode::Corrections,
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
            warm_ms: Some(500),
            no_gpu_render: false,
            masks: 1,
            mask_presence: false,
            window: None,
        }
    }

    /// The script enters the mode, sets the source with an unpaced `⌥`-click, waits, then paints
    /// one paced stroke of `samples` positions and drags the source handle through `samples`
    /// positions at the same pace, and every step it writes parses back as the editor reads it.
    #[test]
    fn the_script_sets_a_source_then_paces_a_stroke_and_a_source_drag() {
        let source = Path::new("photo.jpg");
        let options = options(source);
        let steps = script(&options);
        let written = script::write(&steps);
        assert_eq!(script::parse(&written.to_string()).unwrap(), steps);
        let tail = &written.as_array().unwrap()[written.as_array().unwrap().len() - 5..];
        assert_eq!(steps[steps.len() - 5], script::Step::key("q"));
        assert_eq!(
            tail[1],
            json!({"correction": {"press": {"at": [0.45, 0.3], "modifiers": ["alt"]}}})
        );
        assert_eq!(tail[2], json!({"wait": {"ms": 500}}));
        let press = &tail[3]["correction"]["press"];
        assert_eq!(press["interval_ms"], 24);
        assert_eq!(press["to"].as_array().unwrap().len(), options.samples - 1);
        assert!(press.get("release").is_none(), "a measured stroke releases");
        assert!(press.get("modifiers").is_none());
        let drag = &tail[4]["correction"]["drag"];
        assert_eq!(drag["handle"], "source");
        assert_eq!(drag["interval_ms"], 24);
        assert_eq!(drag["to"].as_array().unwrap().len(), options.samples);
        // Both gestures' real time, presses included, is inside the deadline beside the setup.
        assert_eq!(
            deadline(&options),
            Duration::from_millis(90_000 + 2 * 24 * 5)
        );
        let max = Options {
            samples: CORRECTION_POSITIONS_MAX,
            ..options
        };
        assert!(deadline(&max) < Duration::from_secs(300));
    }

    /// One drafted CPU frame, one GPU frame and a release whose commit reaches its repaired frame.
    fn paced(kind: &str, start: f64) -> Vec<Value> {
        let request = if kind == "press" {
            json!({"correction": {"press": {"at": [0.35, 0.6], "to": [[0.4, 0.6]], "interval_ms": 24}}})
        } else {
            json!({"correction": {"drag": {"handle": "source", "to": [[0.5, 0.3]], "interval_ms": 24}}})
        };
        let at = |ms: f64| start + ms;
        vec![
            json!({"event": "script_step", "elapsed_ms": at(0.0), "detail": {"request": request}}),
            json!({"event": "correction_position", "elapsed_ms": at(1.0),
                "detail": {"index": 0, "release": false}}),
            json!({"event": "correction_draft_set", "elapsed_ms": at(2.0)}),
            json!({"event": "correction_draft_preview", "elapsed_ms": at(3.0), "detail": {
                "generation": 7, "positions": 1,
                "round_trip_ms": {"executor_wait": 0.2, "draft_set": 0.5, "preview_job": 0.2, "return_to_queue": 0.1}}}),
            json!({"event": "preview_result_received", "elapsed_ms": at(10.0),
                "detail": {"generation": 7, "queue_wait_ms": 1.0, "render_ms": 5.0}}),
            json!({"event": "preview_displayed", "elapsed_ms": at(12.0),
                "detail": {"generation": 7, "draft_revision": 1}}),
            json!({"event": "correction_position", "elapsed_ms": at(25.0),
                "detail": {"index": 1, "release": true}}),
            json!({"event": "correction_draft_set", "elapsed_ms": at(25.5)}),
            json!({"event": "gpu_preview_tick", "elapsed_ms": at(26.0),
                "detail": {"path": "gpu", "draft_id": "d", "draft_revision": 2}}),
            json!({"event": "surface_frame_drawn", "elapsed_ms": at(31.0),
                "detail": {"path": "gpu", "drawn_ms": at(30.0), "draft_id": "d", "draft_revision": 2}}),
            json!({"event": "correction_draft_commit", "elapsed_ms": at(27.0)}),
            json!({"event": "preview_displayed", "elapsed_ms": at(65.0),
                "detail": {"generation": 8, "draft_revision": null}}),
        ]
    }

    static NULL: Value = Value::Null;

    fn row<'a>(result: &'a Value, metric: &str) -> &'a Value {
        stats::distribution(result, metric).unwrap_or(&NULL)
    }

    fn app(steps: usize) -> Value {
        json!({"status": "captured", "had_input_errors": false,
            "script": vec![json!({}); steps], "frames": []})
    }

    /// Each gesture's drafted frames pair on both paths, the stroke's positions with the frames
    /// that carried them, and each release with the repaired frame after its commit.
    #[test]
    fn the_report_pairs_drafted_and_repaired_frames_for_the_stroke_and_the_drag() {
        let mut events = vec![json!({"event": "correction_source_set", "elapsed_ms": 0.5})];
        events.extend(paced("press", 100.0));
        events.extend(paced("drag", 1000.0));
        let source = Path::new("photo.jpg");
        let (result, failures) = report(&events, &app(3), 3, &options(source)).unwrap();
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(result["status"], "passed");
        for prefix in ["brush", "source_drag"] {
            let frames = row(&result, &format!("{prefix}_input_to_presented_frame"));
            assert_eq!(frames["count"], 2, "{prefix}: one CPU and one GPU frame");
            assert_eq!(
                row(&result, &format!("{prefix}_cpu_input_to_presented_frame"))["p50"],
                10.0
            );
            assert_eq!(
                row(&result, &format!("{prefix}_gpu_input_to_presented_frame"))["p50"],
                4.5
            );
            assert_eq!(
                row(&result, &format!("{prefix}_release_to_repaired_frame"))["p50"],
                40.0
            );
            assert_eq!(
                row(&result, &format!("{prefix}_commit_to_repaired_frame"))["p50"],
                38.0
            );
            assert_eq!(
                row(&result, &format!("{prefix}_press_to_first_draft_set"))["p50"],
                1.0
            );
            assert_eq!(result["gestures"][prefix]["displayed"], 2);
        }
        assert_eq!(
            row(&result, "brush_position_to_presented_frame")["count"],
            2
        );
        assert!(stats::distribution(&result, "source_drag_position_to_presented_frame").is_none());
        assert_eq!(result["not_measured"][0]["metric"], "input_to_overlay");
        assert_eq!(result["overlay_draws"], 0);
    }

    /// With the desktop's overlay draws in the log each position pairs with the first draw at or
    /// after it, and the figure is no longer listed as unmeasured.
    #[test]
    fn positions_pair_with_the_first_overlay_draw_at_or_after_them() {
        let mut events = vec![json!({"event": "correction_source_set", "elapsed_ms": 0.5})];
        events.extend(paced("press", 100.0));
        // Positions at 101 and 125; draws at 104 (for the first), 131 (for the second) and 150.
        for drawn in [104.0, 131.0, 150.0] {
            events.push(
                json!({"event": "correction_overlay_drawn", "elapsed_ms": drawn + 3.0,
                "detail": {"drawn_ms": drawn, "pins": 1, "live_points": 2}}),
            );
        }
        events.extend(paced("drag", 1000.0));
        let source = Path::new("photo.jpg");
        let (result, failures) = report(&events, &app(3), 3, &options(source)).unwrap();
        assert!(failures.is_empty(), "{failures:?}");
        let overlay = row(&result, "brush_position_to_overlay_drawn");
        assert_eq!(overlay["count"], 2);
        assert_eq!(overlay["min"], 3.0);
        assert_eq!(overlay["max"], 6.0);
        assert_eq!(result["gestures"]["brush"]["overlay_draws"], 3);
        assert_eq!(result["gestures"]["source_drag"]["overlay_draws"], 0);
        assert_eq!(result["overlay_draws"], 3);
        assert!(result["not_measured"].as_array().unwrap().is_empty());
    }

    /// A refused render is a failure the report lists, never a pass: here every drafted frame of
    /// the stroke and its repaired frame were refused, as a layer of corrections is refused until
    /// its renderer lands, and the drag never ran.
    #[test]
    fn a_refused_repair_fails_the_report() {
        let events = vec![
            json!({"event": "correction_source_set", "elapsed_ms": 0.5}),
            json!({"event": "script_step", "elapsed_ms": 1.0, "detail": {"request":
                {"correction": {"press": {"at": [0.35, 0.6], "to": [[0.4, 0.6]], "interval_ms": 24}}}}}),
            json!({"event": "correction_position", "elapsed_ms": 2.0, "detail": {"index": 0, "release": false}}),
            json!({"event": "correction_draft_set", "elapsed_ms": 3.0}),
            json!({"event": "correction_draft_preview", "elapsed_ms": 4.0, "detail": {"generation": 3,
                "round_trip_ms": {"executor_wait": 0.2, "draft_set": 0.5, "preview_job": 0.2, "return_to_queue": 0.1}}}),
            json!({"event": "preview_failed", "elapsed_ms": 9.0, "detail": {
                "generation": 3, "draft_revision": 1, "error_code": "unsupported"}}),
            json!({"event": "correction_position", "elapsed_ms": 26.0, "detail": {"index": 1, "release": true}}),
            json!({"event": "correction_draft_commit", "elapsed_ms": 27.0}),
            json!({"event": "preview_failed", "elapsed_ms": 40.0, "detail": {
                "generation": 4, "draft_revision": null, "error_code": "unsupported"}}),
            json!({"event": "script_step_failed", "elapsed_ms": 41.0, "detail": {"reason": "render refused"}}),
        ];
        let source = Path::new("photo.jpg");
        let mut app = app(2);
        app["had_input_errors"] = json!(true);
        let (result, failures) = report(&events, &app, 3, &options(source)).unwrap();
        assert_eq!(result["status"], "failed");
        let text = failures.join("\n");
        for expected in [
            "only 2 of 3 script steps ran",
            "script steps failed: render refused",
            "brush stroke: no drafted frame reached the screen",
            "brush stroke: no repaired frame was presented after its commit",
            "brush stroke: 2 refusals (unsupported)",
            "source-handle drag: the step never ran",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in {text}");
        }
        assert_eq!(
            result["gestures"]["brush"]["refusals"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(result["gestures"]["brush"]["superseded"], 1);
        assert!(stats::distribution(&result, "brush_input_to_presented_frame").is_none());
    }

    /// A gesture whose events the pairing cannot read fails in the report, which is still written.
    #[test]
    fn an_unreadable_gesture_is_a_failure_in_a_written_report() {
        let mut events = vec![json!({"event": "correction_source_set", "elapsed_ms": 0.5})];
        events.extend(paced("press", 100.0));
        events.extend(paced("drag", 1000.0));
        // The drag's preview answers no draft.set.
        let set = events
            .iter()
            .rposition(|event| {
                event["event"] == "correction_draft_set" && event["elapsed_ms"] == 1002.0
            })
            .unwrap();
        events.remove(set);
        let source = Path::new("photo.jpg");
        let (result, failures) = report(&events, &app(3), 3, &options(source)).unwrap();
        assert_eq!(result["status"], "failed");
        assert_eq!(
            failures,
            ["source-handle drag: A correction_draft_preview answered no correction_draft_set"]
        );
        assert!(result["gestures"]["source_drag"].is_null());
        assert!(stats::distribution(&result, "brush_input_to_presented_frame").is_some());
    }

    /// A stroke whose ⌥-click set no source drafts no repair, which the report says.
    #[test]
    fn a_stroke_without_a_set_source_fails_the_report() {
        let events = paced("press", 0.0);
        let source = Path::new("photo.jpg");
        let (_, failures) = report(&events, &app(3), 3, &options(source)).unwrap();
        assert!(
            failures
                .iter()
                .any(|failure| failure.contains("set no source")),
            "{failures:?}"
        );
    }
}
