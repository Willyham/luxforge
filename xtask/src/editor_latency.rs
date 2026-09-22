//! Desktop control-to-uploaded-frame measurement: the real editor, gesture and GPU upload.
//!
//! [`editor_performance`](crate::editor_performance) measures `render` on the catalog owner's own
//! thread. Nothing there schedules, uploads or presents, so it cannot answer the responsiveness
//! question the [Basic design][design] asks: how long after a slider input the frame carrying that
//! input is on screen. This module answers it by driving the shipped binary in a background
//! evidence launch, with one `slider` script step per input, and reading the timestamps out of the
//! run's own `events.jsonl`. The default measures Basic's exposure slider; `--control curve`
//! measures the developer proof curve while its canvas is visible in the tools panel.
//!
//! What "presented" means here: the desktop's `Uploaded` message, recorded as `preview_displayed`.
//! That is the moment the rendered pixels have been handed to the renderer as a texture and the
//! canvas draws them from the next frame on. It is **not** display scanout, which this harness
//! cannot observe; every figure is therefore an upper bound on the editor's own work and a lower
//! bound on what an eye sees.
//!
//! `--action <id> --parameter <name>` drives any other field-patch slider through the same
//! draft.begin/set/commit path, in place of the default Basic exposure: [`FieldTarget::lookup`]
//! reads the parameter's declared range and step from the module registry (what `module.list`
//! answers), so every generated gesture value the harness sends is one that action would actually
//! accept.
//!
//! [design]: ../../../docs/design/basic-and-histogram.md
use crate::*;
use lightwell_core::{ModuleRegistry, ParameterKind};
use std::time::{Duration, Instant};

/// The Basic module's patch action and the field the gesture drags, named as the module declares
/// them.
const SET_BASIC: &str = "set-basic";
const EXPOSURE: &str = "exposure";
const SET_CONTROLS: &str = "set-controls";
const MASTER: &str = "master";
const CONTROLS_MODULE: &str = "lightwell.controls";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Slider,
    Curve,
}

impl Control {
    fn name(self) -> &'static str {
        match self {
            Self::Slider => "slider",
            Self::Curve => "curve",
        }
    }
}

/// The field-patch action and parameter a slider gesture measures, with the range and step
/// [`FieldTarget::lookup`] reads from the module registry so every generated gesture value is one
/// the action would actually accept. `--control curve` never uses this: the proof curve is its own
/// fraction-based gesture, unrelated to any field's declared range.
struct FieldTarget {
    action: String,
    parameter: String,
    min: f64,
    max: f64,
    step: f64,
}

impl FieldTarget {
    /// The default this harness has always measured, `--action`/`--parameter` absent: Basic's
    /// exposure slider, -5..5 EV in steps of 0.01, mirroring
    /// `lightwell_core::modules::basic::EXPOSURE_MIN/MAX/STEP`, which are not exported.
    fn basic_exposure() -> Self {
        Self {
            action: SET_BASIC.into(),
            parameter: EXPOSURE.into(),
            min: -5.0,
            max: 5.0,
            step: 0.01,
        }
    }

    /// Resolve `--action <id> --parameter <name>` against the built-in module registry: the
    /// action must be a field-patch action, which drafts through draft.begin/set/commit exactly as
    /// set-basic does, and the parameter must be integer or number, so it declares a range and an
    /// optional step (defaulting to 1, as an undeclared step means for every other client) this
    /// harness can generate valid gesture values from.
    fn lookup(action: &str, parameter: &str) -> Result<Self> {
        let registry = ModuleRegistry::builtin();
        let (_, declared) = registry
            .action(action)
            .ok_or_else(|| format!("No module declares the action {action}"))?;
        ensure(
            declared.patch,
            format!(
                "Action {action} is not a field-patch action; editor-latency drives a field-patch slider exactly as it drives set-basic"
            ),
        )?;
        let parameter_descriptor = declared
            .parameter(parameter)
            .ok_or_else(|| format!("Action {action} declares no parameter {parameter}"))?;
        let (min, max) = match &parameter_descriptor.kind {
            ParameterKind::Integer { min, max } => (*min as f64, *max as f64),
            ParameterKind::Number { min, max } => (*min, *max),
            other => {
                return Err(format!(
                    "Parameter {parameter} of {action} is {other:?}, not an integer or a number"
                )
                .into());
            }
        };
        let step = parameter_descriptor.step.unwrap_or(1.0);
        ensure(
            step.is_finite() && step > 0.0 && min < max,
            format!("Parameter {parameter} of {action} declares no usable range or step"),
        )?;
        Ok(Self {
            action: action.to_owned(),
            parameter: parameter.to_owned(),
            min,
            max,
            step,
        })
    }

    /// `count` distinct, non-zero, ascending multiples of a spacing derived from the parameter's
    /// own declared step: the largest multiple of the step at or under `3 * step`, unless that
    /// would carry the last of `count` values (a drag's trailing release included) past the
    /// parameter's own bound, in which case the largest multiple of the step that keeps it inside.
    /// For the default Basic exposure target (-5..5, step 0.01) this reproduces the fixed 0.03 EV
    /// spacing this harness has always used.
    fn gesture_values(&self, count: usize) -> Vec<f64> {
        let preferred = 3.0 * self.step;
        let headroom = self.max / (count as f64 + 1.0);
        let spacing_steps = if preferred <= headroom {
            (preferred / self.step).round().max(1.0)
        } else {
            (headroom / self.step).floor().max(1.0)
        };
        let spacing = spacing_steps * self.step;
        (0..count)
            .map(|index| {
                let raw = (index + 1) as f64 * spacing;
                ((raw / self.step).round() * self.step).clamp(self.min, self.max)
            })
            .collect()
    }
}

/// What `--action`/`--parameter` resolve to: absent, the default Basic exposure slider, unchanged
/// from before this option existed; present, both are required together and name a field-patch
/// slider, which only the (default) slider control measures.
fn resolve_field(
    control: Control,
    action: Option<&str>,
    parameter: Option<&str>,
) -> Result<FieldTarget> {
    match (action, parameter) {
        (None, None) => Ok(FieldTarget::basic_exposure()),
        (Some(action), Some(parameter)) => {
            ensure(
                control == Control::Slider,
                "--action/--parameter measure a field-patch slider; pass no --control or --control slider",
            )?;
            FieldTarget::lookup(action, parameter)
        }
        _ => Err("--action and --parameter must be given together".into()),
    }
}

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

/// The values one gesture visits: `samples` distinct steps, none of them zero. A slider's values
/// are all inside the measured field's declared range and step ([`FieldTarget::gesture_values`]);
/// distinctness matters because a repeated value is not an input at all: `draft.set` is only sent
/// for a value that differs from the one already accepted.
fn gesture_values(samples: usize, control: Control, field: &FieldTarget) -> Vec<f64> {
    if control == Control::Curve {
        let curve_denominator = samples.next_power_of_two() as f64;
        (0..samples)
            .map(|index| {
                // The widget publishes f32 fractions, and the host sends those fractions back
                // through JSON. Binary-exact steps survive both conversions, allowing strict
                // equality against the draft.set payload without a tolerance that could mask a
                // different point or an out-of-order input.
                (index + 1) as f64 / curve_denominator
            })
            .collect()
    } else {
        field.gesture_values(samples)
    }
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
    /// `preview_displayed` for that job's generation: its texture is allocated and on screen.
    displayed_ms: f64,
    generation: u64,
    draft_revision: u64,
    /// The desktop's own measurement of the GPU upload inside the interval above.
    upload_ms: f64,
}

/// Pair every `draft.set` with the preview job it produced and the frame that job was displayed as.
///
/// The pairing is not a guess: a gesture holds one round trip at a time, so the
/// `slider_draft_preview` that follows a `slider_draft_set` is that set's own answer, and it
/// carries the preview generation, which `preview_displayed` repeats. The value is checked on both
/// ends, so a mispairing fails the run instead of producing a number.
fn event_value(value: &Value, control: Control) -> Option<f64> {
    match control {
        Control::Slider => value.as_f64(),
        Control::Curve => value.get(1)?.get(1)?.as_f64(),
    }
}

fn inputs(events: &[Value], control: Control, field: &FieldTarget) -> Result<Vec<Input>> {
    let key = if control == Control::Curve {
        MASTER
    } else {
        field.parameter.as_str()
    };
    let mut inputs = Vec::new();
    let mut pending: Option<(f64, f64)> = None;
    for event in events {
        match event["event"].as_str() {
            Some("slider_draft_set") => {
                let value = event_value(&event["detail"]["fields"][key], control)
                    .ok_or("A draft.set carried no measured control value")?;
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
                    event_value(&detail["value"], control) == Some(value),
                    "A draft preview reports a value its draft.set did not send",
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
/// every measured interval is exactly one input, one `draft.set`, one preview job and one upload,
/// with nothing from the previous input still in flight. A multi-value step measures the driver's
/// coalescing instead, which [`burst_step`] does separately.
///
/// The final step is the release. Its value is a real input like the others, and the commit that
/// follows it immediately supersedes its drafted preview — that job is requested and never
/// displayed, which is the queue cancellation this gesture actually performs. Its latency is
/// therefore excluded from the per-input distribution and measured through to the settled exact
/// histogram instead.
fn curve_step(points: Vec<f64>, finish: &str) -> Value {
    json!({"curve":{"action":SET_CONTROLS,"parameter":MASTER,"event":"move",
        "index":1,"points":points.into_iter().map(|y| [0.5,y]).collect::<Vec<_>>(),
        "finish":finish}})
}

fn gesture_steps(values: &[f64], control: Control, field: &FieldTarget) -> Vec<Value> {
    if control == Control::Curve {
        let mut steps: Vec<Value> = values
            .iter()
            .map(|value| curve_step(vec![*value], "open"))
            .collect();
        if let Some(last) = steps.last_mut() {
            last["curve"]["finish"] = json!("release");
        }
        return steps;
    }
    let mut steps: Vec<Value> = values
        .iter()
        .map(|value| json!({"slider":{"action":field.action,"parameter":field.parameter,"values":[value]}}))
        .collect();
    if let Some(last) = steps.last_mut() {
        last["slider"]["release"] = json!(true);
    }
    steps
}

/// One gesture that sends every value at once, as a fast drag does between two ticks. The driver
/// keeps at most one round trip in flight and only the newest value waiting, so this step's
/// `draft.set` count against its value count is the coalescing the design specifies. Each value is
/// reflected to the opposite end of the measured field's own declared range
/// (`min + max - value`), which is exactly negation on Basic exposure's symmetric -5..5 and stays
/// inside an asymmetric range like a mixer or vignette field's too, so the gesture commits a real
/// change rather than the value already current.
fn burst_step(values: &[f64], control: Control, field: &FieldTarget) -> Value {
    if control == Control::Curve {
        // Reverse the middle point's vertical journey while remaining in the declared [0,1]
        // range. The burst measures one replaceable pending draft value, not visible frames.
        return curve_step(
            values
                .iter()
                .map(|value| f64::from((1.0 - value) as f32))
                .collect(),
            "release",
        );
    }
    let reflected: Vec<f64> = values
        .iter()
        .map(|value| field.min + field.max - value)
        .collect();
    json!({"slider":{"action":field.action,"parameter":field.parameter,"values":reflected,"release":true}})
}

/// One step per commit: each value is its own complete gesture, moved and released at once, so the
/// run produces one settled exact histogram per sample instead of one per script.
///
/// This is how the settled-histogram distribution is gathered. Each step opens a draft, sends the
/// one value, commits it, and the commit's own refresh renders and reduces the committed frame; the
/// drafted preview requested in between is superseded before it can be displayed, so this mode also
/// counts one cancelled preview job per commit.
fn commit_steps(values: &[f64], control: Control, field: &FieldTarget) -> Vec<Value> {
    if control == Control::Curve {
        return values
            .iter()
            .map(|value| curve_step(vec![*value], "release"))
            .collect();
    }
    values
        .iter()
        .map(
            |value| json!({"slider":{"action":field.action,"parameter":field.parameter,"values":[value],"release":true}}),
        )
        .collect()
}

/// Keep the proof curve, including its canvas, in the real tools-panel viewport during the
/// measurement. A hidden curve would measure only controller/render work and miss tessellation.
fn curve_view_steps() -> Vec<Value> {
    // The latency source is JPEG; the RAW section is absent from its tools model entirely.
    let mut steps: Vec<Value> = [
        "lightwell.basic",
        "lightwell.pixel",
        "lightwell.transform",
        "lightwell.crop",
    ]
    .into_iter()
    .map(|module| json!({"section":{"module":module,"expanded":false}}))
    .collect();
    steps.push(json!({"section":{"module":CONTROLS_MODULE,"expanded":true}}));
    steps.push(json!({"tools_scroll":1.0}));
    steps
}

/// Which distribution a run gathers. Both drive the same messages; they differ in where the gesture
/// ends, and therefore in which interval the run can sample thirty times.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// An open drag: every input is drained and displayed, so input-to-presented-frame is sampled
    /// once per input and the settled histogram once, at the single release that ends it.
    Drag,
    /// One complete gesture per input: the settled exact histogram is sampled once per input, and
    /// no drafted preview survives its own commit, so there is no input-to-presented distribution.
    Commit,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::Drag => "drag",
            Self::Commit => "commit",
        }
    }
}

pub struct Options<'a> {
    pub source: &'a Path,
    pub samples: usize,
    pub mode: Mode,
    pub control: Control,
    /// `--action`/`--parameter`, resolved by [`resolve_field`]: the default Basic exposure slider
    /// when absent, or the named field-patch slider `--control slider` (the default) measures.
    /// Unused when `control` is `Curve`.
    pub action: Option<&'a str>,
    pub parameter: Option<&'a str>,
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
    ensure(!out.exists(), "Editor latency output must be new")?;
    ensure(
        (1..=60).contains(&options.samples),
        "Samples must be 1..60; the evidence script accepts at most 64 steps",
    )?;
    ensure(
        options.control != Control::Curve || options.samples <= 32,
        "Curve samples must be 1..32 so every middle-point fraction stays in range",
    )?;
    let field = resolve_field(options.control, options.action, options.parameter)?;
    fs::create_dir_all(out)?;
    let source = options.source.canonicalize()?;
    let source_hash = hash(&source)?;
    // A drag needs one more value than the measured sample count: the extra one is the release,
    // whose own drafted preview the commit supersedes, so it is measured to the settled histogram
    // instead. A commit run measures every value it sends.
    let drag = options.mode == Mode::Drag;
    let values = gesture_values(options.samples + usize::from(drag), options.control, &field);
    if options.control == Control::Slider {
        ensure(
            values
                .iter()
                .all(|value| (field.min..=field.max).contains(value) && *value != 0.0),
            format!(
                "A generated gesture value leaves {}'s declared range {}..{} or is zero",
                field.parameter, field.min, field.max
            ),
        )?;
        let mut distinct = values.clone();
        distinct.dedup_by(|a, b| a == b);
        ensure(
            distinct.len() == values.len(),
            "Generated gesture values are not distinct",
        )?;
    }

    let mut script = Vec::new();
    if let Some(angle) = options.crop {
        script.push(
            json!({"api":{"method":"edit.crop-fit","params":{"aspect":"16:9","angle":angle}}}),
        );
    }
    if options.control == Control::Curve {
        script.extend(curve_view_steps());
    }
    if drag {
        script.extend(gesture_steps(&values, options.control, &field));
        script.push(burst_step(&values, options.control, &field));
    } else {
        script.extend(commit_steps(&values, options.control, &field));
    }
    ensure(
        script.len() <= 64,
        "The latency script exceeds the 64-step evidence bound",
    )?;
    let script_file = out.join("gesture-script.json");
    write_json(&script_file, &json!(script))?;

    let evidence = out.join("app");
    let mut args: Vec<OsString> = vec![
        "--evidence-dir".into(),
        evidence.clone().into_os_string(),
        "--evidence-script".into(),
        script_file.clone().into_os_string(),
        "--open".into(),
        source.clone().into_os_string(),
    ];
    if options.control == Control::Curve {
        args.push("--developer".into());
    }
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
    if options.control == Control::Curve {
        let setup_index = usize::from(options.crop.is_some()) + curve_view_steps().len();
        let setup = frames
            .get(setup_index)
            .ok_or("No captured frame follows the curve viewport setup")?;
        let ready = setup["state"]["control_ui"]["curves"]
            .as_array()
            .and_then(|curves| {
                curves
                    .iter()
                    .find(|curve| curve["action"] == SET_CONTROLS && curve["parameter"] == MASTER)
            })
            .is_some_and(|curve| {
                curve["sample_count"] == 257
                    && curve["sample_source_entry"] == curve["display_entry"]
            });
        ensure(
            ready
                && setup["state"]["developer"] == true
                && setup["state"]["expanded"][CONTROLS_MODULE] == true
                && setup["state"]["tools_scroll"] == 1.0,
            "The proof curve was not expanded, scrolled into view and sampled to 257 points before timing",
        )?;
    }

    let measured = inputs(&events, options.control, &field)?;
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
    let mut ranked_input_to_frame = input_to_frame.clone();
    ranked_input_to_frame.sort_by(f64::total_cmp);
    let input_p95 = percentile(&ranked_input_to_frame, 95);

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
        "control":options.control.name(),
        "control_action":if options.control == Control::Curve { SET_CONTROLS } else { field.action.as_str() },
        "control_parameter":if options.control == Control::Curve { MASTER } else { field.parameter.as_str() },
        "field_range":if options.control == Control::Curve { Value::Null } else { json!({"min":field.min,"max":field.max,"step":field.step}) },
        "effect_scope":match (options.control, field.action.as_str(), field.parameter.as_str()) {
            (Control::Curve, ..) => "Developer proof curve: identity colour operation. Draft/preview scheduling and GPU upload are timed while the curve canvas is visible; the curve does not alter photo pixels.".to_owned(),
            (Control::Slider, SET_BASIC, EXPOSURE) => "Basic exposure: the photograph's colour pass is measured with the generated slider.".to_owned(),
            (Control::Slider, action, parameter) => format!("{action} {parameter}: the field-patch slider is measured through draft.begin/set/commit exactly as Basic exposure is."),
        },
        "view_setup":if options.control == Control::Curve {
            json!({"developer":true,"proof_section":CONTROLS_MODULE,
                "collapsed":["lightwell.basic","lightwell.pixel","lightwell.transform","lightwell.crop"],
                "raw_section":"absent for the JPEG latency source",
                "tools_scroll":1.0})
        } else { Value::Null },
        "samples":options.samples,
        "gesture_values":values,
        "method":"Background evidence launch of the release binary, warm filesystem cache. In drag mode one scripted control step per input is left open, so the step settles only when the gesture has drained: every interval is one input, one draft.set, one preview job and one upload. In commit mode each step is a whole gesture, moved and released at once, so each sample is one committed frame and its exact histogram. Presented means the desktop's Uploaded message (preview_displayed), when the rendered pixels have become a renderer texture; it is not display scanout.",
        "provisional_input_to_frame_target":{"p95_below_ms":100.0,"measured_p95_ms":input_p95,
            "met":input_p95.map(|ms| ms < 100.0)},
        "timings_ms":{
            "input_to_presented_frame":distribution(input_to_frame),
            "draft_set_round_trip":distribution(set_round_trip),
            "render_and_upload":distribution(render_and_upload),
            "gpu_upload":distribution(upload),
            "final_input_to_settled_histogram":distribution(settled_from_input),
            "commit_to_settled_histogram":distribution(settled_from_commit),
        },
        "queue":{
            "scripted_slider_values":if options.control == Control::Slider {
                json!(if drag { values.len() + options.samples + 1 } else { values.len() })
            } else { Value::Null },
            "scripted_curve_values":if options.control == Control::Curve {
                json!(if drag { values.len() + options.samples + 1 } else { values.len() })
            } else { Value::Null },
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

    /// The curve control ignores its `field` argument entirely, so every curve test passes the
    /// default Basic exposure target as an unused placeholder.
    fn unused_field() -> FieldTarget {
        FieldTarget::basic_exposure()
    }

    #[test]
    fn curve_script_keeps_every_point_in_range_and_below_the_evidence_bound() {
        let field = unused_field();
        let values = gesture_values(31, Control::Curve, &field);
        let steps = gesture_steps(&values, Control::Curve, &field);
        assert_eq!(steps.len(), 31);
        assert_eq!(steps[0]["curve"]["finish"], "open");
        assert_eq!(steps[30]["curve"]["finish"], "release");
        assert_eq!(steps[0]["curve"]["points"][0][0], 0.5);
        assert_eq!(values[0], 1.0 / 32.0);
        assert_eq!(values[30], 31.0 / 32.0);
        assert_eq!(gesture_values(33, Control::Curve, &field)[32], 33.0 / 64.0);
        let wire: Vec<Value> =
            serde_json::from_str(&serde_json::to_string(&steps).unwrap()).unwrap();
        for (step, expected) in wire.iter().zip(&values) {
            let fraction = step["curve"]["points"][0][1].as_f64().unwrap();
            assert_eq!(fraction, *expected);
            assert_eq!(f64::from(fraction as f32), *expected);
        }
        let setup = curve_view_steps();
        assert_eq!(setup.len(), 6);
        assert_eq!(setup.last().unwrap(), &json!({"tools_scroll":1.0}));
        let burst = burst_step(&values, Control::Curve, &field);
        assert_eq!(burst["curve"]["points"].as_array().unwrap().len(), 31);
        for point in burst["curve"]["points"].as_array().unwrap() {
            assert!((0.0..=1.0).contains(&point[1].as_f64().unwrap()));
        }
        assert!(setup.len() + steps.len() + 2 <= 64); // optional crop, then burst
    }

    #[test]
    fn curve_midpoint_pairs_one_draft_set_with_its_uploaded_generation() {
        let points = json!([[0.0, 0.0], [0.5, 0.375], [1.0, 1.0]]);
        let events = vec![
            json!({"event":"slider_draft_set","elapsed_ms":10.0,
                "detail":{"fields":{"master":points}}}),
            json!({"event":"slider_draft_preview","elapsed_ms":20.0,
                "detail":{"value":points,"generation":7,"draft_revision":2}}),
            json!({"event":"preview_displayed","elapsed_ms":35.0,
                "detail":{"generation":7,"draft_revision":2,"upload_ms":3.0}}),
        ];
        let paired = inputs(&events, Control::Curve, &unused_field()).unwrap();
        assert_eq!(paired.len(), 1);
        assert_eq!(paired[0].value, 0.375);
        assert_eq!(paired[0].displayed_ms - paired[0].sent_ms, 25.0);
        assert_eq!(paired[0].upload_ms, 3.0);
        let mut wrong = events;
        wrong[2]["detail"]["draft_revision"] = json!(3);
        assert!(inputs(&wrong, Control::Curve, &unused_field()).is_err());
    }

    #[test]
    fn resolve_field_accepts_the_default_and_a_declared_field_patch_slider_and_rejects_the_rest() {
        // Absent, the default Basic exposure target, unchanged from before the option existed.
        let default = resolve_field(Control::Slider, None, None).unwrap();
        assert_eq!(default.action, SET_BASIC);
        assert_eq!(default.parameter, EXPOSURE);
        assert_eq!((default.min, default.max, default.step), (-5.0, 5.0, 0.01));

        // A declared field-patch parameter of the mixer resolves to its own registry range/step.
        let mixer = resolve_field(Control::Slider, Some("set-mixer"), Some("red-hue")).unwrap();
        assert_eq!(mixer.action, "set-mixer");
        assert_eq!(mixer.parameter, "red-hue");
        assert_eq!((mixer.min, mixer.max, mixer.step), (-100.0, 100.0, 1.0));

        // One of the pair without the other is refused rather than silently defaulting.
        assert!(resolve_field(Control::Slider, Some("set-mixer"), None).is_err());
        assert!(resolve_field(Control::Slider, None, Some("red-hue")).is_err());
        // An override with the curve control is refused: the curve is its own fraction gesture.
        assert!(resolve_field(Control::Curve, Some("set-mixer"), Some("red-hue")).is_err());
        // An undeclared action, an undeclared parameter, and a non-patch action are each refused.
        assert!(resolve_field(Control::Slider, Some("edit.nothing"), Some("x")).is_err());
        assert!(resolve_field(Control::Slider, Some("set-mixer"), Some("hue")).is_err());
        assert!(resolve_field(Control::Slider, Some("reset-mixer"), Some("red-hue")).is_err());
    }

    #[test]
    fn gesture_values_for_an_integer_step_parameter_stay_distinct_nonzero_and_in_range() {
        // The mixer's own declared shape: -100..100 in steps of 1, exactly what `resolve_field`
        // reads off `set-mixer`'s `red-hue` parameter.
        let field = FieldTarget {
            action: "set-mixer".into(),
            parameter: "red-hue".into(),
            min: -100.0,
            max: 100.0,
            step: 1.0,
        };
        let values = field.gesture_values(31);
        assert_eq!(values.len(), 31);
        // Every value is a whole number (the declared step), ascending, distinct and nonzero.
        for value in &values {
            assert_eq!(value.fract(), 0.0, "{value} is not a whole step");
            assert!(
                (field.min..=field.max).contains(value),
                "{value} out of range"
            );
            assert_ne!(*value, 0.0);
        }
        assert!(values.windows(2).all(|pair| pair[0] < pair[1]));
        // 30 default samples plus the drag's trailing release value: 3, 6, .., 93, comfortably
        // inside -100..100 with headroom for the reflected burst step that follows.
        assert_eq!(values[0], 3.0);
        assert_eq!(values[30], 93.0);

        // A far larger sample count still keeps every value inside the declared range, never
        // silently overflowing it the way a fixed spacing would.
        let many = field.gesture_values(61);
        assert!(
            many.iter()
                .all(|value| (field.min..=field.max).contains(value))
        );
        let mut distinct = many.clone();
        distinct.dedup_by(|a, b| a == b);
        assert_eq!(
            distinct.len(),
            many.len(),
            "61 generated values are not all distinct"
        );
    }
}
