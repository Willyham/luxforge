//! Desktop slider-to-presented-frame measurement: the real editor, the real gesture, the real GPU
//! upload.
//!
//! [`editor_performance`](crate::editor_performance) measures `render` on the catalog owner's own
//! thread. Nothing there schedules, uploads or presents, so it cannot answer the responsiveness
//! question the [Basic design][design] asks: how long after a slider input the frame carrying that
//! input is on screen. This module answers it by driving the shipped binary in a background
//! evidence launch, with one `slider` script step per input, and reading the timestamps out of the
//! run's own `events.jsonl`.
//!
//! What "presented" means here: the update in which the rendered raster became the photo surface's
//! source, recorded as `preview_displayed`. The photograph is drawn by a primitive that owns its
//! texture, so there is no upload message to wait for and no separate upload figure to report.
//! That is the moment the rendered pixels have been handed to the renderer as a texture and the
//! canvas draws them from the next frame on. It is **not** display scanout, which this harness
//! cannot observe; every figure is therefore an upper bound on the editor's own work and a lower
//! bound on what an eye sees.
//!
//! [design]: ../../../docs/design/basic-and-histogram.md
use crate::*;
use std::time::{Duration, Instant};

/// The Basic module's patch action and the field the gesture drags, named as the module declares
/// them.
const SET_BASIC: &str = "set-basic";
const EXPOSURE: &str = "exposure";

/// Every Basic field non-neutral, for the "holds a full Basic layer" resource workload. Each value
/// is inside the module's declared range and none of them is the neutral default, so the compiled
/// stack runs every one of the module's colour units.
fn full_basic() -> Value {
    json!({
        "exposure": 0.5,
        "contrast": 25.0,
        "highlights": -30.0,
        "shadows": 30.0,
        "whites": -15.0,
        "blacks": 15.0,
        "temperature": 20.0,
        "tint": -10.0,
        "vibrance": 30.0,
        "saturation": 15.0,
    })
}

/// The exposure values one gesture visits: `samples` distinct steps, none of them zero, all inside
/// the declared -5..5 EV range. Distinctness matters because a repeated value is not an input at
/// all: `draft.set` is only sent for a value that differs from the one already accepted.
fn gesture_values(samples: usize) -> Vec<f64> {
    (0..samples)
        .map(|index| ((index + 1) as f64 * 0.03 * 100.0).round() / 100.0)
        .collect()
}

fn percentile(sorted: &[f64], percent: usize) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (percent * sorted.len()).div_ceil(100).max(1);
    sorted.get(rank - 1).copied()
}

/// Nearest-rank p50/p95 with every sample retained, so a tail can never be dropped silently.
fn distribution(mut samples: Vec<f64>) -> Value {
    samples.sort_by(f64::total_cmp);
    json!({
        "count": samples.len(),
        "p50_ms": percentile(&samples, 50),
        "p95_ms": percentile(&samples, 95),
        "min_ms": samples.first(),
        "max_ms": samples.last(),
        "samples_ms": samples,
    })
}

fn elapsed(event: &Value) -> Result<f64> {
    event["elapsed_ms"]
        .as_f64()
        .ok_or_else(|| "An event carries no elapsed_ms".into())
}

/// `ps` CPU seconds and resident size, exactly as [`crate::diagnostics`] reads them.
fn usage(root: &Path, pid: u32) -> Result<(f64, f64)> {
    let raw = output(root, "ps", &["-o", "time=,rss=", "-p", &pid.to_string()])?;
    let fields: Vec<_> = raw.split_whitespace().collect();
    ensure(fields.len() == 2, "Missing ps measurements")?;
    let (min, sec) = fields[0].split_once(':').ok_or("Unexpected ps CPU time")?;
    Ok((
        min.parse::<f64>()? * 60.0 + sec.parse::<f64>()?,
        fields[1].parse::<f64>()? / 1024.0,
    ))
}

/// Run one evidence script to completion, sampling RSS about every 50 ms while it runs.
fn evidence_run(
    root: &Path,
    bin: &Path,
    out: &Path,
    name: &str,
    args: &[OsString],
    deadline: Duration,
) -> Result<(Vec<Value>, f64)> {
    let mut child = smoke::spawn_editor(root, bin, args, &out.join(format!("{name}.log")))?;
    let start = Instant::now();
    let mut rss = Vec::new();
    let status = loop {
        if let Some(status) = child.child.try_wait()? {
            break status;
        }
        ensure(
            start.elapsed() < deadline,
            format!("The {name} run exceeded its deadline"),
        )?;
        if let Ok((_, resident)) = usage(root, child.child.id()) {
            rss.push(json!([start.elapsed().as_secs_f64(), resident]));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    ensure(
        status.success(),
        format!(
            "The {name} run failed: {}",
            fs::read_to_string(out.join(format!("{name}.log"))).unwrap_or_default()
        ),
    )?;
    let peak = rss
        .iter()
        .filter_map(|row| row[1].as_f64())
        .fold(0.0, f64::max);
    Ok((rss, peak))
}

/// One input's journey, from the `draft.set` that carried it to the frame that showed it.
struct Input {
    value: f64,
    /// `slider_draft_set`: the desktop handed this value to the owner. This is the input's time.
    sent_ms: f64,
    /// `slider_draft_preview`: `draft.set` answered and the preview job for it was queued.
    queued_ms: f64,
    /// `preview_displayed` for that job's generation: its raster became the surface's source and
    /// the redraw that draws it was requested.
    displayed_ms: f64,
    generation: u64,
    draft_revision: u64,
    /// The desktop's own measurement of the GPU upload inside the interval above, when the binary
    /// reports one. The photo surface writes its texture during the frame that draws it, so the
    /// current binary reports none and this stays `NaN`.
    upload_ms: f64,
}

/// Pair every `draft.set` with the preview job it produced and the frame that job was displayed as.
///
/// The pairing is not a guess: a gesture holds one round trip at a time, so the
/// `slider_draft_preview` that follows a `slider_draft_set` is that set's own answer, and it
/// carries the preview generation, which `preview_displayed` repeats. The value is checked on both
/// ends, so a mispairing fails the run instead of producing a number.
fn inputs(events: &[Value]) -> Result<Vec<Input>> {
    let mut inputs = Vec::new();
    let mut pending: Option<(f64, f64)> = None;
    for event in events {
        match event["event"].as_str() {
            Some("slider_draft_set") => {
                let value = event["detail"]["fields"][EXPOSURE]
                    .as_f64()
                    .ok_or("A slider_draft_set carried no exposure field")?;
                ensure(
                    pending.is_none(),
                    "Two slider_draft_set events without an answer between them",
                )?;
                pending = Some((value, elapsed(event)?));
            }
            Some("slider_draft_preview") => {
                let (value, sent_ms) = pending
                    .take()
                    .ok_or("A slider_draft_preview answered no slider_draft_set")?;
                let detail = &event["detail"];
                ensure(
                    detail["value"].as_f64() == Some(value),
                    "A slider_draft_preview reports a value its slider_draft_set did not send",
                )?;
                inputs.push(Input {
                    value,
                    sent_ms,
                    queued_ms: elapsed(event)?,
                    displayed_ms: f64::NAN,
                    generation: detail["generation"].as_u64().ok_or("No generation")?,
                    draft_revision: detail["draft_revision"].as_u64().ok_or("No revision")?,
                    upload_ms: f64::NAN,
                });
            }
            _ => {}
        }
    }
    for event in events.iter().filter(|e| e["event"] == "preview_displayed") {
        let generation = event["detail"]["generation"].as_u64();
        if let Some(input) = inputs
            .iter_mut()
            .find(|input| Some(input.generation) == generation)
        {
            ensure(
                event["detail"]["draft_revision"].as_u64() == Some(input.draft_revision),
                "A displayed frame names another draft revision than the job it answers",
            )?;
            input.displayed_ms = elapsed(event)?;
            input.upload_ms = event["detail"]["upload_ms"].as_f64().unwrap_or(f64::NAN);
        }
    }
    Ok(inputs)
}

/// The gesture script: one slider step per value, each left open so the step settles on the frame
/// rendered from that value, then a last value that also releases, which is how a drag ends.
///
/// One value per step is deliberate. An open step settles only when the gesture has drained, so
/// every measured interval is exactly one input, one `draft.set`, one preview job and one frame,
/// with nothing from the previous input still in flight. A multi-value step measures the driver's
/// coalescing instead, which [`burst_step`] does separately.
///
/// The final step is the release. Its value is a real input like the others, and the commit that
/// follows it immediately supersedes its drafted preview — that job is requested and never
/// displayed, which is the queue cancellation this gesture actually performs. Its latency is
/// therefore excluded from the per-input distribution and measured through to the settled exact
/// histogram instead.
fn gesture_steps(values: &[f64]) -> Vec<Value> {
    let mut steps: Vec<Value> = values
        .iter()
        .map(|value| json!({"slider":{"action":SET_BASIC,"parameter":EXPOSURE,"values":[value]}}))
        .collect();
    if let Some(last) = steps.last_mut() {
        last["slider"]["release"] = json!(true);
    }
    steps
}

/// One gesture that sends every value at once, as a fast drag does between two ticks. The driver
/// keeps at most one round trip in flight and only the newest value waiting, so this step's
/// `draft.set` count against its value count is the coalescing the design specifies. The values are
/// negated so the gesture commits a real change rather than the value already current.
fn burst_step(values: &[f64]) -> Value {
    let negated: Vec<f64> = values.iter().map(|value| -value).collect();
    json!({"slider":{"action":SET_BASIC,"parameter":EXPOSURE,"values":negated,"release":true}})
}

/// One step per commit: each value is its own complete gesture, moved and released at once, so the
/// run produces one settled exact histogram per sample instead of one per script.
///
/// This is how the settled-histogram distribution is gathered. Each step opens a draft, sends the
/// one value, commits it, and the commit's own refresh renders and reduces the committed frame; the
/// drafted preview requested in between is superseded before it can be displayed, so this mode also
/// counts one cancelled preview job per commit.
fn commit_steps(values: &[f64]) -> Vec<Value> {
    values
        .iter()
        .map(
            |value| json!({"slider":{"action":SET_BASIC,"parameter":EXPOSURE,"values":[value],"release":true}}),
        )
        .collect()
}

/// Which distribution a run gathers. Drag and commit drive the same messages and differ in where
/// the gesture ends; burst drives a wild, undrained drag through the paced slider step instead.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// An open drag: every input is drained and displayed, so input-to-presented-frame is sampled
    /// once per input and the settled histogram once, at the single release that ends it.
    Drag,
    /// One complete gesture per input: the settled exact histogram is sampled once per input, and
    /// no drafted preview survives its own commit, so there is no input-to-presented distribution.
    Commit,
    /// A wild drag, undrained: [`BURST_SECONDS`] of exposure values at [`BURST_RATE_PER_SEC`] each,
    /// paced by the desktop's own timer rather than sent all at once, so the driver's real
    /// coalescing runs on them instead of the harness deciding what reaches the owner. `--samples`
    /// is ignored; the run is always the same fixed number of values.
    Burst,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::Drag => "drag",
            Self::Commit => "commit",
            Self::Burst => "burst",
        }
    }
}

/// How long the burst gesture lasts and how many exposure values it sends per second. Both are
/// named constants because the report and its target both quote them.
const BURST_SECONDS: f64 = 3.0;
const BURST_RATE_PER_SEC: f64 = 120.0;
/// The triangle wave's peak, in EV, on either side of zero.
const BURST_PEAK_EV: f64 = 2.0;

/// One value per tick, in milliseconds, at [`BURST_RATE_PER_SEC`].
fn burst_interval_ms() -> u64 {
    (1000.0 / BURST_RATE_PER_SEC).round() as u64
}

/// The burst gesture's own values: a triangle wave from 0 to +[`BURST_PEAK_EV`], down to
/// -[`BURST_PEAK_EV`] and back to 0, over [`BURST_SECONDS`] at [`BURST_RATE_PER_SEC`] values a
/// second, each rounded to two decimals. Consecutive values may repeat once rounded; the paced
/// driver sends every one of them regardless, and the core's own gesture round trip is what
/// coalesces a run the driver could not keep up with.
fn burst_values() -> Vec<f64> {
    let count = (BURST_SECONDS * BURST_RATE_PER_SEC).round() as usize;
    (0..count.max(2))
        .map(|index| {
            // Four quarters of one triangle period: 0..1 rises to the peak, 1..3 falls through
            // zero to the trough, 3..4 rises back to zero.
            let phase = index as f64 / (count.max(2) - 1) as f64 * 4.0;
            let unit = if phase <= 1.0 {
                phase
            } else if phase <= 3.0 {
                2.0 - phase
            } else {
                phase - 4.0
            };
            ((unit * BURST_PEAK_EV) * 100.0).round() / 100.0
        })
        .collect()
}

/// The one scripted step a burst run sends: every value paced by its own timer, released at the
/// end exactly as a real drag's release ends it.
fn burst_gesture_step(values: &[f64], interval_ms: u64) -> Value {
    json!({"slider":{
        "action":SET_BASIC,
        "parameter":EXPOSURE,
        "values":values,
        "interval_ms":interval_ms,
        "release":true,
    }})
}

pub struct Options<'a> {
    pub source: &'a Path,
    pub samples: usize,
    pub mode: Mode,
    /// Commit a straightening crop before the gesture, so the measured stack carries the crop
    /// resample as well as the colour pass.
    pub crop: Option<f64>,
    /// Also hold a full Basic layer and measure idle CPU for 30 seconds after it settles.
    pub idle: bool,
}

pub fn run(root: &Path, out: &Path, bin: &Path, options: Options) -> Result {
    ensure(
        cfg!(target_os = "macos"),
        "Editor latency measurement currently reads native macOS ps only",
    )?;
    if options.mode == Mode::Burst {
        return run_burst(root, out, bin, &options);
    }
    ensure(!out.exists(), "Editor latency output must be new")?;
    ensure(
        (1..=60).contains(&options.samples),
        "Samples must be 1..60; the evidence script accepts at most 64 steps",
    )?;
    fs::create_dir_all(out)?;
    let source = options.source.canonicalize()?;
    let source_hash = hash(&source)?;
    // A drag needs one more value than the measured sample count: the extra one is the release,
    // whose own drafted preview the commit supersedes, so it is measured to the settled histogram
    // instead. A commit run measures every value it sends.
    let drag = options.mode == Mode::Drag;
    let values = gesture_values(options.samples + usize::from(drag));

    let mut script = Vec::new();
    if let Some(angle) = options.crop {
        script.push(
            json!({"api":{"method":"edit.crop-fit","params":{"aspect":"16:9","angle":angle}}}),
        );
    }
    if drag {
        script.extend(gesture_steps(&values));
        script.push(burst_step(&values));
    } else {
        script.extend(commit_steps(&values));
    }
    let script_file = out.join("gesture-script.json");
    write_json(&script_file, &json!(script))?;

    let evidence = out.join("app");
    let args: Vec<OsString> = vec![
        "--evidence-dir".into(),
        evidence.clone().into_os_string(),
        "--evidence-script".into(),
        script_file.clone().into_os_string(),
        "--open".into(),
        source.clone().into_os_string(),
    ];
    let (rss, peak_rss) = evidence_run(
        root,
        bin,
        out,
        "gesture",
        &args,
        // The editor's own evidence deadline is 25 s; allow for the launch wrapper around it.
        Duration::from_secs(60),
    )?;

    let app = read_json(&evidence.join("result.json"))?;
    ensure(
        app["status"] == "captured",
        "The gesture run captured nothing",
    )?;
    let events = smoke::events(&evidence.join("events.jsonl"))?;
    ensure(
        app["had_input_errors"] == json!(false)
            && app["script"]
                .as_array()
                .is_some_and(|steps| steps.len() == script.len()),
        format!("A gesture step failed or never ran: {}", app["script"]),
    )?;
    let frames = app["frames"].as_array().ok_or("Missing frames")?;
    let last = frames.last().ok_or("No frame was captured")?;

    let measured = inputs(&events)?;
    ensure(
        measured.len() >= values.len(),
        format!(
            "Only {} inputs reached the owner; {} were scripted",
            measured.len(),
            values.len()
        ),
    )?;
    ensure(
        measured
            .iter()
            .zip(&values)
            .all(|(input, value)| input.value == *value),
        "The measured inputs are not the scripted values in order",
    )?;
    // In a drag, the drained per-input gesture is every open step, each of which settled on its own
    // frame; the release input follows it, and the burst step's coalesced `draft.set` follows that.
    // In a commit run no drafted preview survives its own commit, so there is nothing to drain.
    let drained = if drag {
        &measured[..options.samples]
    } else {
        &[][..]
    };
    ensure(
        drained.iter().all(|input| input.displayed_ms.is_finite()),
        "An input's preview job was never displayed, so the gesture was not drained per step",
    )?;

    let input_to_frame: Vec<f64> = drained
        .iter()
        .map(|input| input.displayed_ms - input.sent_ms)
        .collect();
    let set_round_trip: Vec<f64> = drained
        .iter()
        .map(|input| input.queued_ms - input.sent_ms)
        .collect();
    let render_and_upload: Vec<f64> = drained
        .iter()
        .map(|input| input.displayed_ms - input.queued_ms)
        .collect();
    let upload: Vec<f64> = drained
        .iter()
        .map(|input| input.upload_ms)
        .filter(|value| value.is_finite())
        .collect();

    // The settled exact histogram. A drafted preview is never analysed — the design keeps the plot
    // labelled stale during a gesture — so the exact report is reduced from the frame the commit's
    // own refresh renders, and `analysis_adopted` is the moment it is on screen with its pixels.
    // Each commit is paired with the last input before it and the first report adopted after it.
    let commits: Vec<f64> = events
        .iter()
        .filter(|event| event["event"] == "slider_draft_commit")
        .map(elapsed)
        .collect::<Result<Vec<_>>>()?;
    let adopted: Vec<f64> = events
        .iter()
        .filter(|event| event["event"] == "analysis_adopted")
        .map(elapsed)
        .collect::<Result<Vec<_>>>()?;
    let mut settled_from_input = Vec::new();
    let mut settled_from_commit = Vec::new();
    for commit in &commits {
        let Some(report) = adopted.iter().find(|time| *time >= commit).copied() else {
            continue;
        };
        let Some(input) = measured.iter().rfind(|input| input.sent_ms <= *commit) else {
            continue;
        };
        settled_from_input.push(report - input.sent_ms);
        settled_from_commit.push(report - commit);
    }
    ensure(
        !settled_from_input.is_empty(),
        "No exact histogram was adopted after a gesture committed",
    )?;
    if !drag {
        ensure(
            settled_from_input.len() >= options.samples,
            format!(
                "Only {} of {} commits settled into an exact histogram",
                settled_from_input.len(),
                options.samples
            ),
        )?;
    }

    // Queue behaviour over the whole run: what was asked for against what reached the screen. A
    // requested preview job whose generation never appears in a `preview_displayed` was superseded
    // before its pixels could be shown.
    let counted = |name: &str| events.iter().filter(|e| e["event"] == name).count();
    let burst_draft_sets = if drag {
        json!(measured.len() - options.samples - 1)
    } else {
        Value::Null
    };
    let superseded: Vec<u64> = measured
        .iter()
        .filter(|input| !input.displayed_ms.is_finite())
        .map(|input| input.generation)
        .collect();

    let result = json!({
        "status":"passed",
        "launch_mode":launch::MODE,
        "platform":host(root)?,
        "profile":"release",
        "binary_sha256":hash(bin)?,
        "lockfile_sha256":hash(&root.join("Cargo.lock"))?,
        "source":source,
        "source_sha256":source_hash,
        "source_dimensions":last["state"]["source_dimensions"],
        "preview_dimensions":last["state"]["preview_dimensions"],
        "backend":last["state"]["backend"],
        "physical_size":last["physical_size"],
        "scale":last["scale"],
        "crop_angle_deg":options.crop,
        "mode":options.mode.name(),
        "samples":options.samples,
        "gesture_values":values,
        "method":"Background evidence launch of the release binary, warm filesystem cache. In drag mode one scripted slider step per input is left open, so the step settles only when the gesture has drained: every interval is one input, one draft.set, one preview job and one frame. In commit mode each step is a whole gesture, moved and released at once, so each sample is one committed frame and its exact histogram. Presented means preview_displayed: the update in which the rendered raster became the photo surface's source, drawn by the redraw that update requests; it is not display scanout.",
        "timings_ms":{
            "input_to_presented_frame":distribution(input_to_frame),
            "draft_set_round_trip":distribution(set_round_trip),
            "render_and_upload":distribution(render_and_upload),
            "gpu_upload":if upload.is_empty() { Value::Null } else { distribution(upload) },
            "gpu_upload_note":"null when the binary's preview_displayed carries no upload_ms, which is true of the photo surface: the raster is written into the surface's own texture during the frame that draws it, so there is no upload step to time. render_and_upload then covers the render and the hand-over together.",
            "final_input_to_settled_histogram":distribution(settled_from_input),
            "commit_to_settled_histogram":distribution(settled_from_commit),
        },
        "queue":{
            "scripted_slider_values":if drag { values.len() + options.samples + 1 } else { values.len() },
            "draft_set_requests":counted("slider_draft_set"),
            "preview_jobs_requested":counted("slider_draft_preview"),
            "preview_jobs_superseded":superseded.len(),
            "superseded_generations":superseded,
            "commits":commits.len(),
            "analysis_reports_adopted":adopted.len(),
            "analysis_jobs_superseded":0,
            "burst_step_values":if drag { json!(options.samples + 1) } else { Value::Null },
            "burst_step_draft_sets":burst_draft_sets,
            "note":"Two bounds show here. Within one gesture the driver keeps at most one draft round trip in flight and only the newest value waiting, so a burst of moves between two ticks is coalesced: burst_step_values against burst_step_draft_sets is that reduction. At the queue, a requested preview job whose generation never reaches a preview_displayed was superseded; every commit supersedes the drafted preview of the value it commits, and a drag's open steps drain one at a time so none of theirs is. No analysis job is superseded because a drafted preview is never analysed: the exact report is reduced only from the committed frame.",
        },
        "resources":{
            "sampled_peak_rss_mib":peak_rss,
            "scratch":last["state"]["scratch"],
            "rss_samples":rss,
            "note":"RSS is sampled about every 50 ms by ps and includes captures, GPU resources and allocator retention; it is not a CPU-heap figure. The scratch object is the process-wide colour budget at the last captured frame, with peak_bytes its high-water mark over the whole run.",
        },
        "workspace":last["state"]["workspace"],
        "histogram":last["state"]["histogram"],
        "checks":[
            "Every scripted step reached a captured frame",
            "Every measured input is the scripted value, in order, with its own draft.set, preview job and displayed frame",
            "Each displayed frame names the draft revision of the job it answers",
            "The exact histogram measured is the first one adopted after the gesture committed",
            "Source SHA-256 is unchanged"
        ],
    });
    write_json(&out.join("latency.json"), &result)?;
    ensure(hash(&source)? == source_hash, "The source changed")?;

    if options.idle {
        hold_and_idle(root, out, bin, &source)?;
    }
    println!("PASS editor latency: {}", out.display());
    Ok(())
}

/// Everything the burst report's own figures come from, computed purely from one run's
/// `events.jsonl`. Pulling this out of [`run_burst`] is what lets it be proven against a synthetic
/// event list rather than only against a real launch.
struct BurstAnalysis {
    /// `slider_step_value` events: every value the paced driver actually handed to the owner,
    /// independent of what the core's own gesture round trip did with it. A coalesced value the
    /// core never turned into a `draft.set` still counts here, as scripted.
    sent_values: usize,
    /// `preview_displayed` events from the first `slider_step_value` onward, drafted and committed
    /// alike: the count `presented_fps` divides by the same window's seconds. A frame presented
    /// before the gesture started (the initial open) is not one of these.
    presented_frames: usize,
    presented_fps: f64,
    /// One sample per drafted generation that reached the screen: its own `slider_draft_set` time
    /// to its `preview_displayed` time, paired by generation exactly as [`inputs`] pairs them for
    /// drag mode. The final value's drafted preview is superseded by the release's commit and so is
    /// never displayed, which is why this can be shorter than `sent_values`. It can be empty: a
    /// pipeline that cannot keep up with the input rate at all can drop every drafted frame and
    /// present only the frames either side of the gesture, which is a real measurement, not a
    /// broken run.
    staleness_ms: Vec<f64>,
    /// The intervals between consecutive presented **drafted** frames, in display order; excludes
    /// the final committed frame, which is not a drafted generation.
    frame_gap_ms: Vec<f64>,
    max_gap_ms: f64,
    draft_sets: usize,
    preview_jobs: usize,
    commits: usize,
    adopted: usize,
    /// `preview_exact_cancelled` events. The current binary emits none; a later phase's cancellable
    /// exact phase is what this will start counting.
    cancelled_exact: usize,
    /// Generations whose preview job never reached a `preview_displayed`.
    superseded: Vec<u64>,
    /// The last presented frame's own `proxy`/`proxy_dimensions`, or null with a note when the
    /// binary's `preview_displayed` carries neither, which is true of the current binary.
    proxy: Value,
}

/// Read [`BurstAnalysis`] out of one run's events, in the order described on the struct's fields.
fn analyze_burst(events: &[Value]) -> Result<BurstAnalysis> {
    let value_events: Vec<f64> = events
        .iter()
        .filter(|event| event["event"] == "slider_step_value")
        .map(elapsed)
        .collect::<Result<Vec<_>>>()?;
    ensure(
        !value_events.is_empty(),
        "no paced slider value reached the owner",
    )?;
    let displayed_events: Vec<f64> = events
        .iter()
        .filter(|event| event["event"] == "preview_displayed")
        .map(elapsed)
        .collect::<Result<Vec<_>>>()?;
    ensure(
        !displayed_events.is_empty(),
        "no frame was presented during the whole run",
    )?;
    // Scoped to the gesture's own window: a frame presented before the first input (the initial
    // open) is not part of what the gesture achieved, so it is excluded from both the count and the
    // window presented_fps divides by.
    let first_value_ms = value_events.first().copied().unwrap_or(0.0);
    let gesture_displayed: Vec<f64> = displayed_events
        .iter()
        .copied()
        .filter(|displayed_ms| *displayed_ms >= first_value_ms)
        .collect();
    let presented_frames = gesture_displayed.len();
    let presented_fps = match gesture_displayed.last() {
        Some(last) => {
            presented_frames as f64 / ((last - first_value_ms) / 1000.0).max(f64::EPSILON)
        }
        None => 0.0,
    };

    // Every drafted generation the gesture produced, paired with the frame that displayed it
    // exactly as drag mode pairs them; the final value's own drafted preview is superseded by the
    // release's commit, so it never appears here, which is the one cancellation this gesture always
    // produces. A pipeline overwhelmed by the input rate can drop every one of them, which is a real
    // measurement of that pipeline, not a broken run, so an empty list is not refused.
    let measured = inputs(events)?;
    let drafted: Vec<&Input> = measured
        .iter()
        .filter(|input| input.displayed_ms.is_finite())
        .collect();
    let staleness_ms: Vec<f64> = drafted
        .iter()
        .map(|input| input.displayed_ms - input.sent_ms)
        .collect();
    let mut drafted_displayed: Vec<f64> = drafted.iter().map(|input| input.displayed_ms).collect();
    drafted_displayed.sort_by(f64::total_cmp);
    let frame_gap_ms: Vec<f64> = drafted_displayed
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .collect();
    let max_gap_ms = frame_gap_ms.iter().copied().fold(0.0, f64::max);

    let counted = |name: &str| events.iter().filter(|e| e["event"] == name).count();
    let superseded: Vec<u64> = measured
        .iter()
        .filter(|input| !input.displayed_ms.is_finite())
        .map(|input| input.generation)
        .collect();

    // The proxy fields arrive with the desktop change this harness anticipates; against the current
    // binary, which carries neither, this reports them as null rather than failing the run.
    let proxy_detail = events
        .iter()
        .rev()
        .find(|event| event["event"] == "preview_displayed")
        .map(|event| &event["detail"]);
    let proxy = match proxy_detail {
        Some(detail) if !detail["proxy"].is_null() => json!({
            "proxy":detail["proxy"],
            "proxy_dimensions":detail["proxy_dimensions"],
        }),
        _ => json!({
            "proxy":Value::Null,
            "proxy_dimensions":Value::Null,
            "note":"the current binary's preview_displayed carries no proxy fields; a later phase adds them",
        }),
    };

    Ok(BurstAnalysis {
        sent_values: value_events.len(),
        presented_frames,
        presented_fps,
        staleness_ms,
        frame_gap_ms,
        max_gap_ms,
        draft_sets: counted("slider_draft_set"),
        preview_jobs: counted("slider_draft_preview"),
        commits: counted("slider_draft_commit"),
        adopted: counted("analysis_adopted"),
        cancelled_exact: counted("preview_exact_cancelled"),
        superseded,
        proxy,
    })
}

/// A wild, undrained drag: [`burst_values`] paced through the paced slider step at
/// [`burst_interval_ms`], released at the end. Unlike [`run`]'s drag mode, nothing here waits for a
/// value to be drained before the next one is sent; the desktop's own gesture round trip decides
/// what reaches the owner, exactly as a real fast drag would, and this reads that behaviour back out
/// of the run's own `events.jsonl`.
fn run_burst(root: &Path, out: &Path, bin: &Path, options: &Options) -> Result {
    ensure(!out.exists(), "Editor latency output must be new")?;
    fs::create_dir_all(out)?;
    let source = options.source.canonicalize()?;
    let source_hash = hash(&source)?;
    let values = burst_values();
    let interval_ms = burst_interval_ms();

    let mut script = Vec::new();
    if let Some(angle) = options.crop {
        script.push(
            json!({"api":{"method":"edit.crop-fit","params":{"aspect":"16:9","angle":angle}}}),
        );
    }
    script.push(burst_gesture_step(&values, interval_ms));
    let script_file = out.join("gesture-script.json");
    write_json(&script_file, &json!(script))?;

    let evidence = out.join("app");
    let args: Vec<OsString> = vec![
        "--evidence-dir".into(),
        evidence.clone().into_os_string(),
        "--evidence-script".into(),
        script_file.clone().into_os_string(),
        "--open".into(),
        source.clone().into_os_string(),
    ];
    let (rss, peak_rss) = evidence_run(
        root,
        bin,
        out,
        "gesture",
        &args,
        // The editor's own evidence deadline is 25 s; the burst itself paces BURST_SECONDS of
        // values through real round trips, so this allows generously for both plus the launch
        // wrapper around them.
        Duration::from_secs(60),
    )?;

    let app = read_json(&evidence.join("result.json"))?;
    ensure(
        app["status"] == "captured",
        "The burst run captured nothing",
    )?;
    let events = smoke::events(&evidence.join("events.jsonl"))?;
    ensure(
        app["had_input_errors"] == json!(false)
            && app["script"]
                .as_array()
                .is_some_and(|steps| steps.len() == script.len()),
        format!("The burst step failed or never ran: {}", app["script"]),
    )?;
    let frames = app["frames"].as_array().ok_or("Missing frames")?;
    let last = frames.last().ok_or("No frame was captured")?;

    let analysis = analyze_burst(&events)?;

    let result = json!({
        "status":"passed",
        "launch_mode":launch::MODE,
        "platform":host(root)?,
        "profile":"release",
        "binary_sha256":hash(bin)?,
        "lockfile_sha256":hash(&root.join("Cargo.lock"))?,
        "source":source,
        "source_sha256":source_hash,
        "source_dimensions":last["state"]["source_dimensions"],
        "preview_dimensions":last["state"]["preview_dimensions"],
        "backend":last["state"]["backend"],
        "physical_size":last["physical_size"],
        "scale":last["scale"],
        "crop_angle_deg":options.crop,
        "mode":"burst",
        "samples":Value::Null,
        "gesture_values":values,
        "method":format!("Background evidence launch of the release binary, warm filesystem cache. --samples is ignored: every burst run sends the same fixed {} values over {} s at {} values/s, paced one per tick of the desktop's own paced slider step rather than sent all at once, so the driver's real coalescing runs on them. Presented means preview_displayed: the update in which the rendered raster became the photo surface's source, drawn by the redraw that update requests; it is not display scanout.", values.len(), BURST_SECONDS, BURST_RATE_PER_SEC),
        "queue":{
            "scripted_slider_values":values.len(),
            "draft_set_requests":analysis.draft_sets,
            "preview_jobs_requested":analysis.preview_jobs,
            "preview_jobs_superseded":analysis.superseded.len(),
            "superseded_generations":analysis.superseded,
            "commits":analysis.commits,
            "analysis_reports_adopted":analysis.adopted,
            "note":"scripted_slider_values against draft_set_requests is the driver's own real-time coalescing of the paced values, exactly as a fast drag between two ticks coalesces. A requested preview job whose generation never reaches a preview_displayed was superseded; the final value's own drafted preview is superseded by the release's commit.",
        },
        "burst":{
            "seconds":BURST_SECONDS,
            "rate_per_second":BURST_RATE_PER_SEC,
            "interval_ms":interval_ms,
            "scripted_values":values.len(),
            "sent_values":analysis.sent_values,
            "draft_sets":analysis.draft_sets,
            "preview_jobs":analysis.preview_jobs,
            "presented_frames":analysis.presented_frames,
            "presented_fps":analysis.presented_fps,
            "staleness_ms":distribution(analysis.staleness_ms),
            "frame_gap_ms":distribution(analysis.frame_gap_ms),
            "max_gap_ms":analysis.max_gap_ms,
            "cancelled_exact":analysis.cancelled_exact,
            "cancelled_exact_note":(analysis.cancelled_exact == 0).then_some("the current binary emits no preview_exact_cancelled events; a later phase adds the cancellable exact render this would count"),
            "proxy":analysis.proxy,
        },
        "resources":{
            "sampled_peak_rss_mib":peak_rss,
            "scratch":last["state"]["scratch"],
            "rss_samples":rss,
            "note":"RSS is sampled about every 50 ms by ps and includes captures, GPU resources and allocator retention; it is not a CPU-heap figure. The scratch object is the process-wide colour budget at the last captured frame, with peak_bytes its high-water mark over the whole run.",
        },
        "workspace":last["state"]["workspace"],
        "histogram":last["state"]["histogram"],
        "checks":[
            "Every scripted value reached a captured tick and its own slider_step_value event",
            "Presented frames are counted from preview_displayed, and drafted staleness is paired with its own slider_draft_set by generation, exactly as drag mode pairs them",
            "Source SHA-256 is unchanged"
        ],
    });
    write_json(&out.join("latency.json"), &result)?;
    ensure(hash(&source)? == source_hash, "The source changed")?;

    println!("PASS editor latency (burst): {}", out.display());
    Ok(())
}

/// The resource workload: a 24 MP image holding a full Basic layer with the histogram on, reopened
/// in a second process that is then left alone for 30 seconds.
///
/// Two processes are needed because an evidence run exits when its script ends, and the editor's
/// own evidence deadline is shorter than the idle window. The first run commits the layer into a
/// catalog that outlives it; the second opens the same file, which the catalog already holds, so it
/// renders and reduces the committed stack and then has nothing left to do.
fn hold_and_idle(root: &Path, out: &Path, bin: &Path, source: &Path) -> Result {
    let catalog = out.join("held-catalog.sqlite");
    let evidence = out.join("hold");
    let script = out.join("hold-script.json");
    write_json(
        &script,
        &json!([{"api":{"method":"edit.set-basic","params":full_basic()}}]),
    )?;
    let (_, hold_peak) = evidence_run(
        root,
        bin,
        out,
        "hold",
        &[
            "--evidence-dir".into(),
            evidence.clone().into_os_string(),
            "--catalog".into(),
            catalog.clone().into_os_string(),
            "--evidence-script".into(),
            script.into_os_string(),
            "--open".into(),
            source.to_path_buf().into_os_string(),
        ],
        Duration::from_secs(60),
    )?;
    let held = read_json(&evidence.join("result.json"))?;
    let frame = held["frames"]
        .as_array()
        .and_then(|frames| frames.last())
        .ok_or("The hold run captured no frame")?
        .clone();

    // The second process: the same catalog, no script, left idle after its first frame.
    let data = out.join("idle-data");
    let log = out.join("idle.log");
    let mut child = smoke::spawn_editor(
        root,
        bin,
        &[
            "--catalog".into(),
            catalog.into_os_string(),
            "--data-root".into(),
            data.clone().into_os_string(),
            "--open".into(),
            source.to_path_buf().into_os_string(),
        ],
        &log,
    )?;
    let events = data.join("logs/events.jsonl");
    let start = Instant::now();
    loop {
        if fs::read_to_string(&events)
            .unwrap_or_default()
            .contains("\"event\":\"analysis_adopted\"")
        {
            break;
        }
        ensure(
            child.child.try_wait()?.is_none(),
            format!(
                "The idle process exited before its histogram: {}",
                fs::read_to_string(&log).unwrap_or_default()
            ),
        )?;
        ensure(
            start.elapsed() < Duration::from_secs(30),
            "The idle process never adopted a histogram",
        )?;
        std::thread::sleep(Duration::from_millis(50));
    }
    // One second of settling, exactly as `measure` does, then a 30 second window.
    std::thread::sleep(Duration::from_secs(1));
    let before = usage(root, child.child.id())?;
    let window = Instant::now();
    let mut peak = before.1;
    while window.elapsed() < Duration::from_secs(30) {
        peak = peak.max(usage(root, child.child.id())?.1);
        std::thread::sleep(Duration::from_millis(500));
    }
    let after = usage(root, child.child.id())?;
    let seconds = window.elapsed().as_secs_f64();
    // The scratch budget travels in the state snapshot written beside a captured frame, and an
    // ordinary launch captures none, so the idle process cannot report it. The gesture process
    // above does, and it runs the same colour stack.
    let idle_events = smoke::events(&events)?;
    write_json(
        &out.join("resources.json"),
        &json!({
            "status":"passed",
            "workload":"One 24 MP image holding a Basic layer with all ten fields non-neutral, histogram on",
            "basic_payload":full_basic(),
            "gesture_process":{
                "sampled_peak_rss_mib":hold_peak,
                "scratch":frame["state"]["scratch"],
                "workspace":frame["state"]["workspace"],
                "histogram":frame["state"]["histogram"],
                "controls":frame["state"]["controls"],
            },
            "idle_process":{
                "duration_s":seconds,
                "cpu_percent_one_core":(after.0-before.0)/seconds*100.0,
                "rss_mib_start":before.1,
                "rss_mib_end":after.1,
                "rss_mib_peak":peak,
                "events":idle_events.len(),
                "scratch":"not observable: the budget travels in the state snapshot beside a captured frame, and an ordinary launch captures none",
            },
            "method":"The first process commits the layer into a catalog that outlives it and is sampled by ps about every 50 ms while it edits, with its frame captures included in that RSS. The second opens the same file from that catalog, renders and reduces the committed stack, then is left alone; CPU is the ps CPU-time delta over 30 seconds after one second of settling. The child is then killed, so this is not clean-close evidence. RSS includes GPU resources and allocator retention and is not separated.",
        }),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One `slider_draft_set`/`slider_draft_preview`/`preview_displayed` triple, the same shape
    /// `inputs` and [`analyze_burst`] read out of a real `events.jsonl`.
    fn drafted(
        set_ms: f64,
        preview_ms: f64,
        displayed_ms: Option<f64>,
        value: f64,
        generation: u64,
        revision: u64,
    ) -> Vec<Value> {
        let mut events = vec![
            json!({"event":"slider_draft_set","elapsed_ms":set_ms,"detail":{"draft_id":"d","fields":{EXPOSURE:value}}}),
            json!({"event":"slider_draft_preview","elapsed_ms":preview_ms,"detail":{"value":value,"generation":generation,"draft_revision":revision}}),
        ];
        if let Some(displayed_ms) = displayed_ms {
            events.push(
                json!({"event":"preview_displayed","elapsed_ms":displayed_ms,"detail":{"generation":generation,"draft_revision":revision}}),
            );
        }
        events
    }

    /// A four-value burst, paced 8 ms apart, that the core's own round trip coalesces into three
    /// `draft.set`s: the first two paced values land inside the first round trip, so only the
    /// newest of them (1.0) is ever sent. The third round trip carries the release value and is
    /// superseded by the commit that follows it, so it is never displayed. The commit's own frame
    /// is presented too, one generation the drafted pairing never matches.
    fn synthetic_events() -> Vec<Value> {
        let mut events = vec![
            json!({"event":"slider_step_value","elapsed_ms":0.0,"detail":{"value":0.5,"index":0}}),
            json!({"event":"slider_step_value","elapsed_ms":8.0,"detail":{"value":1.0,"index":1}}),
            json!({"event":"slider_step_value","elapsed_ms":16.0,"detail":{"value":1.5,"index":2}}),
            json!({"event":"slider_step_value","elapsed_ms":24.0,"detail":{"value":2.0,"index":3}}),
        ];
        events.extend(drafted(5.0, 10.0, Some(20.0), 1.0, 101, 1));
        events.extend(drafted(22.0, 28.0, Some(40.0), 1.5, 102, 2));
        events.extend(drafted(32.0, 38.0, None, 2.0, 103, 3));
        events.push(json!({"event":"slider_draft_commit","elapsed_ms":39.0,"detail":{}}));
        events.push(json!({"event":"analysis_adopted","elapsed_ms":45.0,"detail":{}}));
        // The committed frame's own presented generation, which pairs with none of the drafted
        // inputs above and so must be excluded from staleness and the frame gap, but still counts
        // toward presented_frames and is where presented_fps's own window ends.
        events.push(
            json!({"event":"preview_displayed","elapsed_ms":50.0,"detail":{"generation":104,"draft_revision":3}}),
        );
        events
    }

    #[test]
    fn burst_analysis_reads_fps_staleness_gaps_and_counts_from_synthetic_events() {
        let analysis = analyze_burst(&synthetic_events()).expect("a well-formed burst run");
        assert_eq!(analysis.sent_values, 4, "every slider_step_value counts");
        assert_eq!(
            analysis.presented_frames, 3,
            "two drafted frames plus the committed frame"
        );
        // 3 frames over the 50 ms from the first input to the last presented frame.
        assert!(
            (analysis.presented_fps - 60.0).abs() < 1e-9,
            "{}",
            analysis.presented_fps
        );
        assert_eq!(analysis.staleness_ms, vec![15.0, 18.0]);
        assert_eq!(
            analysis.frame_gap_ms,
            vec![20.0],
            "one gap between the two drafted frames' own displayed times, 20 and 40 ms"
        );
        assert_eq!(analysis.max_gap_ms, 20.0);
        assert_eq!(analysis.draft_sets, 3);
        assert_eq!(analysis.preview_jobs, 3);
        assert_eq!(analysis.commits, 1);
        assert_eq!(analysis.adopted, 1);
        assert_eq!(
            analysis.cancelled_exact, 0,
            "the current binary emits no preview_exact_cancelled events"
        );
        assert_eq!(
            analysis.superseded,
            vec![103],
            "the release value's own drafted preview, superseded by the commit"
        );
        // The synthetic events carry no proxy fields, as the current binary's own events do not;
        // the analysis reports that as null rather than failing.
        assert_eq!(analysis.proxy["proxy"], Value::Null);
        assert_eq!(analysis.proxy["proxy_dimensions"], Value::Null);
        assert!(analysis.proxy["note"].is_string());
    }

    #[test]
    fn burst_analysis_reads_the_proxy_fields_when_the_binary_carries_them() {
        let mut events = synthetic_events();
        let last = events
            .iter_mut()
            .rev()
            .find(|event| event["event"] == "preview_displayed")
            .expect("the committed frame's own preview_displayed");
        last["detail"]["proxy"] = json!(true);
        last["detail"]["proxy_dimensions"] = json!([960, 640]);
        let analysis = analyze_burst(&events).expect("a well-formed burst run");
        assert_eq!(analysis.proxy["proxy"], json!(true));
        assert_eq!(analysis.proxy["proxy_dimensions"], json!([960, 640]));
        assert!(analysis.proxy.get("note").is_none());
    }

    #[test]
    fn burst_analysis_refuses_a_run_with_no_presented_frame() {
        let events = vec![
            json!({"event":"slider_step_value","elapsed_ms":0.0,"detail":{"value":0.5,"index":0}}),
        ];
        assert!(analyze_burst(&events).is_err());
    }

    /// A pipeline overwhelmed by the input rate can drop every drafted frame: the render queue
    /// never keeps up, so nothing between the gesture's start and its commit is ever displayed.
    /// That is a real, if grim, measurement — the pre-instant-preview baseline this mode exists to
    /// show — and must not be refused. The frame the initial open presented, before the gesture's
    /// first input, is excluded from presented_frames and the fps window it divides.
    #[test]
    fn burst_analysis_reports_zero_drafted_frames_rather_than_failing() {
        let mut events = vec![
            // The initial open's own frame, well before the gesture starts.
            json!({"event":"preview_displayed","elapsed_ms":1.0,"detail":{"generation":2}}),
            json!({"event":"slider_step_value","elapsed_ms":10.0,"detail":{"value":0.5,"index":0}}),
            json!({"event":"slider_step_value","elapsed_ms":18.0,"detail":{"value":1.0,"index":1}}),
        ];
        // One drafted round trip that never reaches the screen: superseded before it renders.
        events.extend(drafted(12.0, 16.0, None, 1.0, 101, 1));
        events.push(json!({"event":"slider_draft_commit","elapsed_ms":20.0,"detail":{}}));
        events.push(json!({"event":"analysis_adopted","elapsed_ms":30.0,"detail":{}}));
        events.push(
            json!({"event":"preview_displayed","elapsed_ms":30.0,"detail":{"generation":102,"draft_revision":1}}),
        );

        let analysis = analyze_burst(&events).expect("an empty drafted set is still a valid run");
        assert_eq!(analysis.sent_values, 2);
        assert_eq!(
            analysis.presented_frames, 1,
            "the initial open's frame precedes the gesture and is excluded"
        );
        // One frame at 30 ms, 20 ms after the first input at 10 ms: 1 / 0.02 s.
        assert_eq!(analysis.presented_fps, 50.0);
        assert!(analysis.staleness_ms.is_empty());
        assert!(analysis.frame_gap_ms.is_empty());
        assert_eq!(analysis.max_gap_ms, 0.0);
        assert_eq!(analysis.superseded, vec![101]);
    }

    /// The triangle wave starts and ends at zero, reaches [`BURST_PEAK_EV`] and its negation, is
    /// exactly [`BURST_SECONDS`] times [`BURST_RATE_PER_SEC`] values long and every value is
    /// rounded to two decimals.
    #[test]
    fn the_burst_values_are_a_two_decimal_triangle_wave_of_the_declared_length() {
        let values = burst_values();
        assert_eq!(
            values.len(),
            (BURST_SECONDS * BURST_RATE_PER_SEC).round() as usize
        );
        assert_eq!(*values.first().unwrap(), 0.0);
        assert_eq!(*values.last().unwrap(), 0.0);
        let max = values.iter().copied().fold(f64::MIN, f64::max);
        let min = values.iter().copied().fold(f64::MAX, f64::min);
        // The discrete sample nearest each turning point need not land exactly on it; it must land
        // within one rounded step of it.
        assert!((max - BURST_PEAK_EV).abs() <= 0.02, "{max}");
        assert!((min + BURST_PEAK_EV).abs() <= 0.02, "{min}");
        for value in &values {
            assert_eq!(
                *value,
                (value * 100.0).round() / 100.0,
                "{value} has more than two decimals"
            );
        }
        assert_eq!(burst_interval_ms(), 8);
    }
}
