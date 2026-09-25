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
//! What "presented" means here: the update in which the rendered raster became the photo surface's
//! source, recorded as `preview_displayed`. The photograph is drawn by a primitive that owns its
//! texture, so there is no upload message to wait for and no separate upload figure to report.
//! That is the moment the rendered pixels have been handed to the renderer as a texture and the
//! canvas draws them from the next frame on. It is **not** display scanout, which this harness
//! cannot observe; every figure is therefore an upper bound on the editor's own work and a lower
//! bound on what an eye sees.
//!
//! `--action <id> --parameter <name>` drives any other field-patch slider through the same
//! draft.begin/set/commit path, in place of the default Basic exposure: [`FieldTarget::lookup`]
//! reads the parameter's declared range and step from the module registry (what `module.list`
//! answers), so every generated gesture value the harness sends is one that action would actually
//! accept. That includes a RAW temperature or tint, whose drafted values the core previews
//! approximately on the developed planes; the frames say so (`approximate_white_balance`) and the
//! report counts them, and the release still redevelops the mosaic before the committed frame.
//!
//! [design]: ../../../docs/design/basic-and-histogram.md
use crate::*;
use lightwell_core::{ModuleRegistry, ParameterKind};
use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

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
    /// Where the gesture's values start from: zero when the range holds it, as every field-patch
    /// slider's does, otherwise the declared default (RAW's Custom temperature, 2000..12000 K).
    origin: f64,
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
            origin: 0.0,
        }
    }

    /// Resolve `--action <id> --parameter <name>` against the built-in module registry: the
    /// action must be one whose slider drafts through draft.begin/set/commit exactly as set-basic
    /// does — a field-patch action, or an action whose only parameter this is, as RAW's are — and
    /// the parameter must be integer or number, so it declares a range and an optional step
    /// (defaulting to 1, as an undeclared step means for every other client) this harness can
    /// generate valid gesture values from.
    fn lookup(action: &str, parameter: &str) -> Result<Self> {
        let registry = ModuleRegistry::builtin();
        let (_, declared) = registry
            .action(action)
            .ok_or_else(|| format!("No module declares the action {action}"))?;
        ensure(
            declared.patch
                || (declared.parameters.len() == 1 && declared.parameters[0].name == parameter),
            format!(
                "Action {action} is neither a field-patch action nor one whose only parameter is {parameter}; editor-latency drives a slider that drafts exactly as set-basic does"
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
        let origin = if (min..=max).contains(&0.0) {
            0.0
        } else {
            parameter_descriptor
                .default
                .as_ref()
                .and_then(Value::as_f64)
                .filter(|default| (min..=max).contains(default))
                .unwrap_or(min)
        };
        Ok(Self {
            action: action.to_owned(),
            parameter: parameter.to_owned(),
            min,
            max,
            step,
            origin,
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
        let headroom = (self.max - self.origin) / (count as f64 + 1.0);
        let spacing_steps = if preferred <= headroom {
            (preferred / self.step).round().max(1.0)
        } else {
            (headroom / self.step).floor().max(1.0)
        };
        let spacing = spacing_steps * self.step;
        (0..count)
            .map(|index| {
                let raw = self.origin + (index + 1) as f64 * spacing;
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

/// Nearest-rank p50/p95 with every sample retained, so a tail can never be dropped silently. The
/// one [`stats::Distribution`] shape every timing tool now writes.
fn distribution(samples: Vec<f64>) -> Value {
    stats::distribution_json(samples)
}

fn elapsed(event: &Value) -> Result<f64> {
    event["elapsed_ms"]
        .as_f64()
        .ok_or_else(|| "An event carries no elapsed_ms".into())
}

/// One process's sampled CPU and resident memory during an evidence launch.
struct ProcessObservation {
    rss_samples: Vec<Value>,
    peak_rss_mib: f64,
    process_cpu_seconds: Option<f64>,
}

/// Run one evidence script to completion, sampling CPU time and RSS about every 50 ms while it runs.
fn evidence_run(
    root: &Path,
    bin: &Path,
    out: &Path,
    name: &str,
    args: &[OsString],
    deadline: Duration,
) -> Result<ProcessObservation> {
    let mut child =
        scenario::launch::spawn_editor(root, bin, args, &out.join(format!("{name}.log")))?;
    let start = Instant::now();
    let watch = stats::Watch::new(root, child.child.id());
    let mut rss = Vec::new();
    let mut first_cpu_seconds = None;
    let mut last_cpu_seconds = None;
    let status = loop {
        if let Some(status) = child.child.try_wait()? {
            break status;
        }
        ensure(
            start.elapsed() < deadline,
            format!("The {name} run exceeded its deadline"),
        )?;
        if let Ok((cpu_seconds, resident)) = watch.usage()
            && resident > 0.0
        {
            // `ps` may return a zero RSS row in the race after the child exits but before
            // `try_wait` observes it. Do not let that reset the final CPU sample to zero.
            first_cpu_seconds.get_or_insert(cpu_seconds);
            last_cpu_seconds = Some(cpu_seconds);
            rss.push(json!([start.elapsed().as_secs_f64() * 1000.0, resident]));
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
    let process_cpu_seconds = first_cpu_seconds
        .zip(last_cpu_seconds)
        .map(|(first, last)| last - first)
        .filter(|delta| *delta >= 0.01);
    Ok(ProcessObservation {
        rss_samples: rss,
        peak_rss_mib: peak,
        process_cpu_seconds,
    })
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
    /// The preview job's generation and draft revision; `None` when the draft was accepted but its
    /// preview job was refused (`slider_draft_unpreviewed`) — a RAW draft whose development is not
    /// in memory, while a redevelopment is in flight — so the input has no frame of its own.
    generation: Option<u64>,
    draft_revision: Option<u64>,
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
                    generation: Some(detail["generation"].as_u64().ok_or("No generation")?),
                    draft_revision: Some(detail["draft_revision"].as_u64().ok_or("No revision")?),
                    upload_ms: f64::NAN,
                });
            }
            // The draft accepted the value but its preview job was refused: the input reached the
            // owner and no frame of its own follows it.
            Some("slider_draft_unpreviewed") => {
                let (value, sent_ms) = pending
                    .take()
                    .ok_or("A slider_draft_unpreviewed answered no slider_draft_set")?;
                ensure(
                    event_value(&event["detail"]["value"], control) == Some(value),
                    "An unpreviewed draft reports a value its draft.set did not send",
                )?;
                inputs.push(Input {
                    value,
                    sent_ms,
                    queued_ms: elapsed(event)?,
                    displayed_ms: f64::NAN,
                    generation: None,
                    draft_revision: None,
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
            .find(|input| input.generation.is_some() && input.generation == generation)
        {
            ensure(
                event["detail"]["draft_revision"].as_u64() == input.draft_revision,
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
    /// A **paint** gesture rather than a slider: one brush stroke whose positions are paced one per
    /// [`PAINT_INTERVAL_MS`] in real time, on a recipe holding one brush mask and one masked colour
    /// layer. `--samples` is the number of positions. This is the mode the [performance
    /// plan](../../../docs/specs/performance.md) named as the missing paint-gesture measurement: the
    /// `mask-range` scenario's figure is taken on four masked colour layers, three of whose masks
    /// bind the whole stage, and is therefore not a baseline for the gesture itself.
    Paint,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::Drag => "drag",
            Self::Commit => "commit",
            Self::Burst => "burst",
            Self::Paint => "paint",
        }
    }
}

/// How long the burst gesture lasts and how many exposure values it sends per second. Both are
/// named constants because the report and its target both quote them.
const BURST_SECONDS: f64 = 3.0;
const BURST_RATE_PER_SEC: f64 = 120.0;
/// The exposure burst's peak, in EV, on either side of zero: the historical fixed triangle, which
/// every exposure field's scaled burst reproduces value for value.
#[cfg(test)]
const BURST_PEAK_EV: f64 = 2.0;
/// The triangle's peak for any field, as a fraction of the smaller half of its declared range
/// around its origin: exactly [`BURST_PEAK_EV`] on an exposure's -5..5 EV, 40 on a -100..100 field
/// and 1802 K either side of Custom temperature's 6504 K.
const BURST_PEAK_FRACTION: f64 = 0.4;

/// One value per tick, in milliseconds, at [`BURST_RATE_PER_SEC`].
fn burst_interval_ms() -> u64 {
    (1000.0 / BURST_RATE_PER_SEC).round() as u64
}

/// The exposure burst's values: a triangle wave from 0 to +[`BURST_PEAK_EV`], down to
/// -[`BURST_PEAK_EV`] and back to 0, over [`BURST_SECONDS`] at [`BURST_RATE_PER_SEC`] values a
/// second, each rounded to two decimals. Consecutive values may repeat once rounded; the paced
/// driver sends every one of them regardless, and the core's own gesture round trip is what
/// coalesces a run the driver could not keep up with. A run sends
/// [`FieldTarget::burst_values`], which is this for an exposure field.
#[cfg(test)]
fn burst_values() -> Vec<f64> {
    burst_units()
        .into_iter()
        .map(|unit| ((unit * BURST_PEAK_EV) * 100.0).round() / 100.0)
        .collect()
}

/// The unrounded triangle every burst follows, from 0 to +1, down to -1 and back to 0.
fn burst_units() -> Vec<f64> {
    let count = (BURST_SECONDS * BURST_RATE_PER_SEC).round() as usize;
    (0..count.max(2))
        .map(|index| {
            // Four quarters of one triangle period: 0..1 rises to the peak, 1..3 falls through
            // zero to the trough, 3..4 rises back to zero.
            let phase = index as f64 / (count.max(2) - 1) as f64 * 4.0;
            if phase <= 1.0 {
                phase
            } else if phase <= 3.0 {
                2.0 - phase
            } else {
                phase - 4.0
            }
        })
        .collect()
}

impl FieldTarget {
    /// The burst's values for this field: the triangle of [`burst_units`] about the field's
    /// origin, peaking at [`BURST_PEAK_FRACTION`] of the smaller half of its declared range, each
    /// on the field's own step grid, as a slider on that step would produce. On an exposure field
    /// (origin 0, -5..5 EV, step 0.01) that is [`burst_values`] itself, value for value; on
    /// Custom temperature it swings from 6500 K to about 8300 K and 4710 K, and on a -100..100
    /// field ±40.
    fn burst_values(&self) -> Vec<f64> {
        let amplitude = BURST_PEAK_FRACTION * (self.max - self.origin).min(self.origin - self.min);
        // A whole step divides exactly; a fractional one multiplies by its inverse, which is how
        // the two-decimal exposure values have always been rounded.
        let snap = |value: f64| {
            if self.step >= 1.0 {
                (value / self.step).round() * self.step
            } else {
                let per_step = 1.0 / self.step;
                (value * per_step).round() / per_step
            }
        };
        burst_units()
            .into_iter()
            .map(|unit| snap(self.origin + unit * amplitude).clamp(self.min, self.max))
            .collect()
    }
}

/// The one scripted step a burst run sends: every value paced by its own timer, released at the
/// end exactly as a real drag's release ends it.
fn burst_gesture_step(field: &FieldTarget, values: &[f64], interval_ms: u64) -> Value {
    json!({"slider":{
        "action":field.action,
        "parameter":field.parameter,
        "values":values,
        "interval_ms":interval_ms,
        "release":true,
    }})
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
    /// Commit a Basic layer with every field non-neutral before the gesture, so the measured
    /// exposure drag runs every one of the module's colour units on each frame.
    pub basic: bool,
    /// Draw a linear gradient mask first and bind the panel's sections to it, so the measured
    /// gesture is a *masked* drag: the same slider, drafting and committing a layer the masked
    /// colour primitive evaluates per pixel. It is the end-to-end figure for what a mask costs a
    /// person's hand, with the unmasked run beside it as its baseline.
    pub mask: bool,
}

/// The steps that draw a mask and bind the generated sections to it before the gesture.
///
/// The mask is created through its own host command, then opened by the name the host gave it —
/// `mask.create-linear` assigns the identity, so a script has nothing else to name it by. From the
/// selection on, every generated slider gesture carries that mask, exactly as the panel's own drag
/// does.
fn mask_precondition() -> [Value; 3] {
    [
        json!({"api":{"method":"mask.create-linear",
            "params":{"x0":0.5,"y0":0.3,"x1":0.5,"y1":0.7}}}),
        json!({"workspace":{"mode":"mask"}}),
        json!({"mask":{"select":{"name":"Mask 1"}}}),
    ]
}

/// The step that commits the full Basic layer a `--basic` run drags over.
fn basic_precondition() -> Value {
    json!({"api":{"method":"edit.set-basic","params":full_basic()}})
}

/// The paint gesture's own pacing and brush, each a named constant because the report quotes it.
///
/// The interval is a little over the delivered masked-drag median of 16.8–17.9 ms, so every position
/// has a round trip of its own to finish rather than being coalesced into its neighbour — the same
/// choice, and the same figure, the `mask-range` scenario's paced stroke makes. Anything shorter
/// measures the desktop's coalescing instead of the gesture.
const PAINT_INTERVAL_MS: u64 = 24;
/// The brush, in mask-space units and `0..100`: a hard edge, so the profile costs one compare rather
/// than a `smooth`, and a size in the middle of the declared range.
const PAINT_SIZE: f64 = 0.06;
const PAINT_FEATHER: f64 = 0.0;
/// The exposure the one masked colour layer holds, in EV. Non-neutral, so the layer exists and its
/// unit runs on every frame the stroke draws.
const PAINT_EV: f64 = 0.6;

/// The measured stroke's path: a straight sweep across the middle of the frame, in normalized
/// content coordinates, one position per paced interval.
///
/// A straight path is deliberate. The measurement is the round trip from one position to the frame
/// carrying it, and a path that wanders changes the component's rectangle between positions, which
/// would put the growth of the bounds into a figure about latency.
fn paint_path(positions: usize) -> Vec<[f64; 2]> {
    (0..positions)
        .map(|index| {
            let t = index as f64 / (positions.max(2) - 1) as f64;
            [0.2 + 0.6 * t, 0.5]
        })
        .collect()
}

/// The steps that build the **bare** recipe the paint measurement wants: one brush mask, one masked
/// colour layer, and nothing else.
///
/// The seeding stroke is what creates the mask and its `Brush 1`, because a brush declares no
/// geometry and therefore has no `mask.create-brush` to call; the mask it makes is the one that
/// opens, so the Exposure slider under the component list binds to it with nothing to name it by.
fn paint_precondition() -> Vec<Value> {
    vec![
        json!({"workspace":{"mode":"mask"}}),
        json!({"mask":{"brush":{"size":PAINT_SIZE,"feather":PAINT_FEATHER,"flow":100.0,
            "erase":false,"limit_to_colour":false}}}),
        json!({"mask":{"paint":"new-mask"}}),
        json!({"mask":{"stroke":{"points":[[0.2,0.3],[0.8,0.3]],"release":true}}}),
        json!({"slider":{"action":SET_BASIC,"parameter":EXPOSURE,
            "values":[PAINT_EV],"release":true}}),
        json!({"mask":{"paint":{"component":0}}}),
    ]
}

#[derive(Clone, Debug)]
struct PaintPhaseSample {
    generation: u64,
    phase: &'static str,
    proxy: bool,
    input_to_presented_ms: f64,
    owner_round_trip_ms: f64,
    executor_wait_ms: f64,
    draft_set_ms: f64,
    preview_job_ms: f64,
    return_to_queue_ms: f64,
    queue_wait_ms: f64,
    worker_render_ms: f64,
    before_worker_result_ms: f64,
    result_to_surface_ms: f64,
}

/// Pair one measured paint input with its owner round-trip, worker result and presented frame.
fn paced_stroke_phase_samples(
    events: &[Value],
    positions: usize,
) -> Result<(usize, Vec<PaintPhaseSample>)> {
    let stroke_step = events
        .iter()
        .position(|event| {
            event["event"] == json!("script_step")
                && event["detail"]["request"]["mask"]["stroke"]["interval_ms"].as_u64()
                    == Some(PAINT_INTERVAL_MS)
                && event["detail"]["request"]["mask"]["stroke"]["points"]
                    .as_array()
                    .is_some_and(|points| points.len() == positions)
        })
        .ok_or("The event stream has no paced stroke step matching this run")?;
    let events = &events[stroke_step..];
    let mut pending: Option<f64> = None;
    let mut inputs: Vec<(f64, u64, [f64; 4])> = Vec::new();
    for event in events {
        match event["event"].as_str() {
            Some("mask_draft_set") => pending = Some(elapsed(event)?),
            Some("mask_draft_preview") => {
                let Some(sent) = pending.take() else {
                    return Err("A mask_draft_preview answered no mask_draft_set".into());
                };
                let generation = event["detail"]["generation"]
                    .as_u64()
                    .ok_or("A mask draft preview named no generation")?;
                let legs = &event["detail"]["round_trip_ms"];
                let legs = [
                    "executor_wait",
                    "draft_set",
                    "preview_job",
                    "return_to_queue",
                ]
                .map(|leg| {
                    legs[leg]
                        .as_f64()
                        .ok_or_else(|| format!("A mask draft preview has no {leg} timing"))
                })
                .into_iter()
                .collect::<std::result::Result<Vec<_>, _>>()?;
                let legs: [f64; 4] = legs
                    .try_into()
                    .map_err(|_| "A mask draft preview has an invalid owner timing count")?;
                inputs.push((sent, generation, legs));
            }
            _ => {}
        }
    }

    let mut samples = Vec::new();
    let mut seen = BTreeSet::new();
    for displayed in events
        .iter()
        .filter(|event| event["event"] == json!("preview_displayed"))
    {
        let Some(generation) = displayed["detail"]["generation"].as_u64() else {
            continue;
        };
        let Some((sent, _, owner_legs)) = inputs
            .iter()
            .find(|(_, held, _)| *held == generation)
            .copied()
        else {
            continue;
        };
        let owner_round_trip_ms = owner_legs.iter().sum::<f64>();
        if !seen.insert(generation) {
            continue;
        }
        let proxy = displayed["detail"]["proxy"].as_bool().unwrap_or(false);
        let phase = if proxy { "proxy" } else { "exact" };
        let received = events
            .iter()
            .find(|event| {
                event["event"] == json!("preview_result_received")
                    && event["detail"]["generation"].as_u64() == Some(generation)
                    && event["detail"]["phase"] == json!(phase)
            })
            .ok_or_else(|| format!("Generation {generation} has no received worker result"))?;
        let queue_wait_ms = received["detail"]["queue_wait_ms"]
            .as_f64()
            .ok_or("A measured worker result has no queue timing")?;
        let worker_render_ms = received["detail"]["render_ms"]
            .as_f64()
            .ok_or("A measured worker result has no render timing")?;
        let displayed_ms = elapsed(displayed)?;
        let received_ms = elapsed(received)?;
        ensure(
            received_ms <= displayed_ms,
            format!("Generation {generation} was displayed before its worker result was received"),
        )?;
        let input_to_presented_ms = displayed_ms - sent;
        let before_worker_result_ms =
            received_ms - sent - owner_round_trip_ms - queue_wait_ms - worker_render_ms;
        samples.push(PaintPhaseSample {
            generation,
            phase,
            proxy,
            input_to_presented_ms,
            owner_round_trip_ms,
            executor_wait_ms: owner_legs[0],
            draft_set_ms: owner_legs[1],
            preview_job_ms: owner_legs[2],
            return_to_queue_ms: owner_legs[3],
            queue_wait_ms,
            worker_render_ms,
            before_worker_result_ms,
            result_to_surface_ms: displayed_ms - received_ms,
        });
    }
    Ok((inputs.len(), samples))
}

/// One paced stroke's input-to-presented-frame samples, paired out of a run's own events.
///
/// The pairing is exact rather than by order: every `mask_draft_set` is answered by one
/// `mask_draft_preview` carrying the preview generation that set queued, and `preview_displayed`
/// repeats that generation. A gesture holds one round trip at a time, so a set with no answer before
/// the next one was refused rather than previewed, and a refusal is not a measurement.
///
/// Returns the number of inputs that queued a preview job and the latency of each one whose frame
/// reached the screen. The difference between the two is what a hand does not see: a position
/// superseded by the next one before its own pixels were drawn.
pub fn paced_stroke_latencies(events: &[Value]) -> Result<(usize, Vec<f64>)> {
    let mut pending: Option<f64> = None;
    let mut inputs: Vec<(f64, u64)> = Vec::new();
    for event in events {
        match event["event"].as_str() {
            Some("mask_draft_set") => pending = Some(elapsed(event)?),
            Some("mask_draft_preview") => {
                let Some(sent) = pending.take() else {
                    return Err("A mask_draft_preview answered no mask_draft_set".into());
                };
                let generation = event["detail"]["generation"]
                    .as_u64()
                    .ok_or("A mask draft preview named no generation")?;
                inputs.push((sent, generation));
            }
            _ => {}
        }
    }
    let mut latencies = Vec::new();
    for event in events
        .iter()
        .filter(|event| event["event"] == json!("preview_displayed"))
    {
        let generation = event["detail"]["generation"].as_u64();
        if let Some((sent, _)) = inputs
            .iter()
            .find(|(_, held)| Some(*held) == generation)
            .copied()
        {
            latencies.push(elapsed(event)? - sent);
        }
    }
    Ok((inputs.len(), latencies))
}

/// The paint mode: one paced brush stroke on a bare masked recipe, measured end to end.
///
/// This is the measurement the [performance plan](../../../docs/specs/performance.md) named as
/// untaken. It shares nothing with [`run`]'s slider path beyond the launch and the reporting,
/// because the two gestures are different: a stroke's positions are a path rather than a field's
/// values, its draft is the mask gesture's own, and its frames are paired through
/// `mask_draft_preview` rather than `slider_draft_preview`.
fn run_paint(root: &Path, out: &Path, bin: &Path, options: &Options) -> Result {
    ensure(!out.exists(), "Editor latency output must be new")?;
    ensure(
        (2..=48).contains(&options.samples),
        "Paint samples must be 2..48 positions; a stroke of one position has no path and the script takes at most 64 steps",
    )?;
    fs::create_dir_all(out)?;
    let source = options.source.canonicalize()?;
    let source_hash = hash(&source)?;
    let path = paint_path(options.samples);

    let mut script = Vec::new();
    if let Some(angle) = options.crop {
        script.push(
            json!({"api":{"method":"edit.crop-fit","params":{"aspect":"16:9","angle":angle}}}),
        );
    }
    if options.basic {
        script.push(basic_precondition());
    }
    script.extend(paint_precondition());
    script.push(json!({"mask":{"stroke":{"points":path,"release":true,
        "interval_ms":PAINT_INTERVAL_MS}}}));
    let script_file = out.join("gesture-script.json");
    write_json(&script_file, &json!(script))?;

    let load_start = crate::verify::load_average(root);
    let evidence = out.join("app");
    let args: Vec<OsString> = vec![
        "--evidence-dir".into(),
        evidence.clone().into_os_string(),
        "--evidence-script".into(),
        script_file.clone().into_os_string(),
        "--open".into(),
        source.clone().into_os_string(),
    ];
    let usage = evidence_run(
        root,
        bin,
        out,
        "gesture",
        &args,
        // The paced stroke itself takes samples × interval of real time on top of the editor's own
        // 25 s evidence deadline; allow generously for both plus the launch wrapper.
        Duration::from_secs(90),
    )?;

    let app = read_json(&evidence.join("result.json"))?;
    ensure(
        app["status"] == "captured",
        "The paint run captured nothing",
    )?;
    let events = scenario::events(&evidence.join("events.jsonl"))?;
    ensure(
        app["had_input_errors"] == json!(false)
            && app["script"]
                .as_array()
                .is_some_and(|steps| steps.len() == script.len()),
        format!("A paint step failed or never ran: {}", app["script"]),
    )?;
    let frames = app["frames"].as_array().ok_or("Missing frames")?;
    let last = frames.last().ok_or("No frame was captured")?;
    // The recipe the stroke was painted on is part of the measurement, not context: the figure this
    // mode exists to replace was taken on four masked layers, so this one states its own.
    let masks = last["state"]["masks"]["masks"]
        .as_array()
        .ok_or("The run captured no mask list")?;
    ensure(
        masks.len() == 1,
        format!(
            "A bare paint run must hold exactly one mask, not {}",
            masks.len()
        ),
    )?;
    // The open mask's own component list, which is where the panel state carries it.
    let components = last["state"]["masks"]["components"]
        .as_array()
        .ok_or("The open mask lists no components")?;
    ensure(
        components.len() == 1 && components[0]["kind"] == json!("brush"),
        format!(
            "A bare paint run must hold one brush component, not {}",
            last["state"]["masks"]["components"]
        ),
    )?;
    // The mask names the layers bound to it, which is the relation `mask.list` answers.
    let bound = masks[0]["layers"]
        .as_array()
        .ok_or("The mask names no bound layers")?;
    ensure(
        bound.len() == 1,
        format!(
            "A bare paint run must hold one masked layer, not {:?}",
            bound
        ),
    )?;
    let masked_layers = bound.len();
    let components = components.len();

    let (queued, phase_samples) = paced_stroke_phase_samples(&events, options.samples)?;
    let latencies: Vec<f64> = phase_samples
        .iter()
        .map(|sample| sample.input_to_presented_ms)
        .collect();
    ensure(
        !latencies.is_empty(),
        "The run painted no stroke whose drafted frame reached the screen",
    )?;
    let mut ranked = latencies.clone();
    ranked.sort_by(f64::total_cmp);
    let p95 = stats::Distribution::percentile(&ranked, 95);
    let load_end = crate::verify::load_average(root);

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
        "full_basic_layer":options.basic,
        "mode":"paint",
        "samples":options.samples,
        "method":format!("Background evidence launch of the release binary, warm filesystem cache. One brush stroke of {} positions is handed to the desktop one per {} ms in real time, so the first tick presses, each later one moves and the last releases: one paced step is still one stroke and one history entry. Each position is its own mask draft.set, preview job and displayed frame, paired by the generation mask_draft_preview carries. Presented means preview_displayed: the update in which the rendered raster became the photo surface's source; it is not display scanout.", options.samples, PAINT_INTERVAL_MS),
        "recipe":{
            "masks":masks.len(),
            "components":components,
            "masked_layers":masked_layers,
            "reads_pixels":false,
            "note":"One brush mask of one component and one masked Basic exposure layer: the bare recipe, stated because the figure this mode replaces was taken on four masked colour layers, three of whose masks hold a component that reads pixels and therefore bounds the whole stage.",
        },
        "brush":{"size":PAINT_SIZE,"feather":PAINT_FEATHER,"flow":100.0,"erase":false,
            "exposure_ev":PAINT_EV},
        "stroke":{
            "interval_ms":PAINT_INTERVAL_MS,
            "positions":options.samples,
            "path":path,
            "inputs_that_queued_a_preview":queued,
            "displayed":latencies.len(),
            "superseded":queued.saturating_sub(latencies.len()),
            "superseded_note":"A position whose own preview job was superseded by the next position before its pixels were drawn. It is what a hand does not see during a continuous stroke, and it is reported rather than averaged away.",
        },
        "provisional_input_to_frame_target":{"p95_below_ms":16.0,"acceptable_below_ms":32.0,
            "measured_p95_ms":p95,
            "met":p95.map(|ms| ms < 16.0),"acceptable":p95.map(|ms| ms < 32.0)},
        "timings_ms":{
            "input_to_presented_frame":distribution(latencies.clone()),
            "owner_round_trip_to_preview_queue":distribution(phase_samples.iter().map(|sample| sample.owner_round_trip_ms).collect()),
            "executor_wait":distribution(phase_samples.iter().map(|sample| sample.executor_wait_ms).collect()),
            "draft_set":distribution(phase_samples.iter().map(|sample| sample.draft_set_ms).collect()),
            "preview_job_planning":distribution(phase_samples.iter().map(|sample| sample.preview_job_ms).collect()),
            "owner_return_to_preview_queue":distribution(phase_samples.iter().map(|sample| sample.return_to_queue_ms).collect()),
            "preview_request_to_worker_start":distribution(phase_samples.iter().map(|sample| sample.queue_wait_ms).collect()),
            "preview_worker_render":distribution(phase_samples.iter().map(|sample| sample.worker_render_ms).collect()),
            "worker_result_to_surface_assignment":distribution(phase_samples.iter().map(|sample| sample.result_to_surface_ms).collect()),
            "pre_result_residual":distribution(phase_samples.iter().map(|sample| sample.before_worker_result_ms).collect()),
        },
        "phase_samples":phase_samples.iter().map(|sample| json!({
            "generation":sample.generation,
            "phase":sample.phase,
            "proxy":sample.proxy,
            "input_to_presented_frame_ms":sample.input_to_presented_ms,
            "owner_round_trip_to_preview_queue_ms":sample.owner_round_trip_ms,
            "executor_wait_ms":sample.executor_wait_ms,
            "draft_set_ms":sample.draft_set_ms,
            "preview_job_planning_ms":sample.preview_job_ms,
            "owner_return_to_preview_queue_ms":sample.return_to_queue_ms,
            "preview_request_to_worker_start_ms":sample.queue_wait_ms,
            "preview_worker_render_ms":sample.worker_render_ms,
            "pre_result_residual_ms":sample.before_worker_result_ms,
            "worker_result_to_surface_assignment_ms":sample.result_to_surface_ms,
        })).collect::<Vec<_>>(),
        "load_average_1m_start":load_start,
        "load_average_1m_end":load_end,
        "load_threshold":launch::LOAD_THRESHOLD,
        "provisional":load_start.or(load_end).is_none_or(|load| load > launch::LOAD_THRESHOLD),
        "resources":{
            "sampled_peak_rss_mib":usage.peak_rss_mib,
            "sampled_process_cpu_seconds":usage.process_cpu_seconds,
            "scratch":last["state"]["scratch"],
            "rss_samples":usage.rss_samples,
            "note":"RSS and process CPU time are sampled together by ps about every 50 ms. Peak RSS includes captures, GPU resources and allocator retention; CPU seconds are the first-to-last valid sampled process delta, may miss up to one polling interval at each edge, and are null when the delta is below ps's 0.01 s resolution.",
        },
        "workspace":last["state"]["workspace"],
        "scope":"mask_draft_set to the preview_displayed of the generation it queued, over one paced brush stroke on a bare masked recipe at this source's own size. It is the paint gesture's counterpart of drag mode's slider figure, and it is not comparable to the mask-range scenario's stroke, which is painted on four masked colour layers.",
        "checks":[
            "The recipe the stroke was painted on holds exactly one mask, one component and one masked layer",
            "Every measured interval pairs one mask_draft_set with the preview_displayed of the generation its own mask_draft_preview named",
            "Source SHA-256 is unchanged",
        ],
    });
    write_json(&out.join("latency.json"), &result)?;
    ensure(hash(&source)? == source_hash, "The source changed")?;

    println!("PASS editor latency (paint): {}", out.display());
    Ok(())
}

pub fn run(root: &Path, out: &Path, bin: &Path, options: Options) -> Result {
    ensure(
        cfg!(target_os = "macos"),
        "Editor latency measurement currently reads native macOS ps only",
    )?;
    if options.mode == Mode::Burst {
        return run_burst(root, out, bin, &options);
    }
    if options.mode == Mode::Paint {
        return run_paint(root, out, bin, &options);
    }
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
    if options.mask {
        script.extend(mask_precondition());
    }
    if options.basic {
        script.push(basic_precondition());
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
    let usage = evidence_run(
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
    let events = scenario::events(&evidence.join("events.jsonl"))?;
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
    let unpreviewed = measured
        .iter()
        .filter(|input| input.generation.is_none())
        .count();
    ensure(
        !drained.iter().any(|input| input.generation.is_none()),
        format!(
            "{unpreviewed} of {} draft.set answers of {} carried no preview job (slider_draft_unpreviewed): the core refused to preview a drafted value, so the drag has no frame per input to time",
            measured.len(),
            field.action
        ),
    )?;
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
    let input_p95 = stats::Distribution::percentile(&ranked_input_to_frame, 95);

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
    // The committed frame on screen: the first undrafted `preview_displayed` after each commit,
    // which is what a person sees on release, before its exact phase settles the histogram.
    let committed_displayed: Vec<f64> = events
        .iter()
        .filter(|event| {
            event["event"] == "preview_displayed" && event["detail"]["draft_revision"].is_null()
        })
        .map(elapsed)
        .collect::<Result<Vec<_>>>()?;
    let mut settled_from_input = Vec::new();
    let mut settled_from_commit = Vec::new();
    let mut presented_from_input = Vec::new();
    let mut presented_from_commit = Vec::new();
    for commit in &commits {
        let Some(input) = measured.iter().rfind(|input| input.sent_ms <= *commit) else {
            continue;
        };
        if let Some(shown) = committed_displayed.iter().find(|time| *time >= commit) {
            presented_from_input.push(shown - input.sent_ms);
            presented_from_commit.push(shown - commit);
        }
        let Some(report) = adopted.iter().find(|time| *time >= commit).copied() else {
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
        .filter_map(|input| input.generation)
        .collect();
    let approximate = approximate_frames(&events);

    let mut result = json!({
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
        "full_basic_layer":options.basic,
        "mode":options.mode.name(),
        "control":options.control.name(),
        "control_action":if options.control == Control::Curve { SET_CONTROLS } else { field.action.as_str() },
        "control_parameter":if options.control == Control::Curve { MASTER } else { field.parameter.as_str() },
        "field_range":if options.control == Control::Curve { Value::Null } else { json!({"min":field.min,"max":field.max,"step":field.step}) },
        "effect_scope":match (options.control, field.action.as_str(), field.parameter.as_str()) {
            (Control::Curve, ..) => "Developer proof curve: identity colour operation. Draft/preview scheduling and GPU upload are timed while the curve canvas is visible; the curve does not alter photo pixels.".to_owned(),
            (Control::Slider, SET_BASIC, EXPOSURE) => "Basic exposure: the photograph's colour pass is measured with the generated slider.".to_owned(),
            (Control::Slider, "set-raw-temperature" | "set-raw-tint", parameter) => format!("{} {parameter}: the slider is measured through draft.begin/set/commit exactly as Basic exposure is. Each drafted value is previewed approximately on the planes developed at the committed white balance (approximate_white_balance frames, never analysed); each release commits and redevelops the mosaic before its exact frame and histogram.", field.action),
            (Control::Slider, action, parameter) => format!("{action} {parameter}: the slider is measured through draft.begin/set/commit exactly as Basic exposure is."),
        },
        "view_setup":if options.control == Control::Curve {
            json!({"developer":true,"proof_section":CONTROLS_MODULE,
                "collapsed":["lightwell.basic","lightwell.pixel","lightwell.transform","lightwell.crop"],
                "raw_section":"absent for the JPEG latency source",
                "tools_scroll":1.0})
        } else { Value::Null },
        "samples":options.samples,
        "gesture_values":values,
        "method":"Background evidence launch of the release binary, warm filesystem cache. In drag mode one scripted control step per input is left open, so the step settles only when the gesture has drained: every interval is one input, one draft.set, one preview job and one frame. In commit mode each step is a whole gesture, moved and released at once, so each sample is one committed frame and its exact histogram. Presented means preview_displayed: the update in which the rendered raster became the photo surface's source, drawn by the redraw that update requests; it is not display scanout.",
        "provisional_input_to_frame_target":{"p95_below_ms":16.0,"acceptable_below_ms":32.0,"measured_p95_ms":input_p95,
            "met":input_p95.map(|ms| ms < 16.0),"acceptable":input_p95.map(|ms| ms < 32.0)},
        "timings_ms":{
            "input_to_presented_frame":distribution(input_to_frame),
            "draft_set_round_trip":distribution(set_round_trip),
            "render_and_upload":distribution(render_and_upload),
            "gpu_upload":if upload.is_empty() { Value::Null } else { distribution(upload) },
            "gpu_upload_note":"null when the binary's preview_displayed carries no upload_ms, which is true of the photo surface: the raster is written into the surface's own texture during the frame that draws it, so there is no upload step to time. render_and_upload then covers the render and the hand-over together.",
            "final_input_to_settled_histogram":distribution(settled_from_input),
            "commit_to_settled_histogram":distribution(settled_from_commit),
            "final_input_to_committed_frame":distribution(presented_from_input),
            "commit_to_committed_frame":distribution(presented_from_commit),
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
            "draft_sets_without_preview":counted("slider_draft_unpreviewed"),
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
            "sampled_peak_rss_mib":usage.peak_rss_mib,
            "sampled_process_cpu_seconds":usage.process_cpu_seconds,
            "scratch":last["state"]["scratch"],
            "rss_samples":usage.rss_samples,
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
    // Beside the rest rather than inside it: the report is already as deep as json! expands.
    result["approximate_white_balance_frames"] = approximate;
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

/// How many presented frames approximated a drafted RAW white balance, and at which phase: the
/// `preview_displayed` events whose `approximate_white_balance` is true, split by `proxy`.
fn approximate_frames(events: &[Value]) -> Value {
    let displayed = || {
        events.iter().filter(|event| {
            event["event"] == "preview_displayed"
                && event["detail"]["approximate_white_balance"] == json!(true)
        })
    };
    json!({
        "presented":displayed().count(),
        "proxy":displayed().filter(|event| event["detail"]["proxy"] == json!(true)).count(),
        "full_size":displayed().filter(|event| event["detail"]["proxy"] != json!(true)).count(),
    })
}

/// Read [`BurstAnalysis`] out of one run's events, in the order described on the struct's fields.
fn analyze_burst(events: &[Value], field: &FieldTarget) -> Result<BurstAnalysis> {
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
    // The inputs pair by the one field the burst drove.
    let measured = inputs(events, Control::Slider, field)?;
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
        .filter_map(|input| input.generation)
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
    let interval_ms = burst_interval_ms();
    // The burst's values are a triangle about the field's own origin, scaled to its declared
    // range: Basic's Exposure by default, any drafting slider with `--action`.
    let field = resolve_field(options.control, options.action, options.parameter)?;
    let values = field.burst_values();
    ensure(
        values
            .iter()
            .all(|value| (field.min..=field.max).contains(value)),
        format!(
            "The burst's values leave {}'s declared range {}..{}",
            field.parameter, field.min, field.max
        ),
    )?;

    let mut script = Vec::new();
    if let Some(angle) = options.crop {
        script.push(
            json!({"api":{"method":"edit.crop-fit","params":{"aspect":"16:9","angle":angle}}}),
        );
    }
    if options.mask {
        script.extend(mask_precondition());
    }
    if options.basic {
        script.push(basic_precondition());
    }
    script.push(burst_gesture_step(&field, &values, interval_ms));
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
    let usage = evidence_run(
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
    let events = scenario::events(&evidence.join("events.jsonl"))?;
    ensure(
        app["had_input_errors"] == json!(false)
            && app["script"]
                .as_array()
                .is_some_and(|steps| steps.len() == script.len()),
        format!("The burst step failed or never ran: {}", app["script"]),
    )?;
    let frames = app["frames"].as_array().ok_or("Missing frames")?;
    let last = frames.last().ok_or("No frame was captured")?;

    let analysis = analyze_burst(&events, &field)?;
    let approximate = approximate_frames(&events);

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
        "full_basic_layer":options.basic,
        "mode":"burst",
        "control_action":field.action,
        "control_parameter":field.parameter,
        "samples":Value::Null,
        "gesture_values":values,
        "approximate_white_balance_frames":approximate,
        "method":format!("Background evidence launch of the release binary, warm filesystem cache. --samples is ignored: every burst run of a field sends the same fixed {} values over {} s at {} values/s, a triangle about the field's origin peaking at {} of the smaller half of its declared range (±2 EV on exposure), paced one per tick of the desktop's own paced slider step rather than sent all at once, so the driver's real coalescing runs on them. Presented means preview_displayed: the update in which the rendered raster became the photo surface's source, drawn by the redraw that update requests; it is not display scanout.", values.len(), BURST_SECONDS, BURST_RATE_PER_SEC, BURST_PEAK_FRACTION),
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
            "sampled_peak_rss_mib":usage.peak_rss_mib,
            "sampled_process_cpu_seconds":usage.process_cpu_seconds,
            "scratch":last["state"]["scratch"],
            "rss_samples":usage.rss_samples,
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
    let hold_usage = evidence_run(
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
    let mut child = scenario::launch::spawn_editor(
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
    // One second of settling, then a 30 second window: the same idle window `measure` takes.
    let window = stats::idle_window(root, child.child.id())?;
    // The scratch budget travels in the state snapshot written beside a captured frame, and an
    // ordinary launch captures none, so the idle process cannot report it. The gesture process
    // above does, and it runs the same colour stack.
    let idle_events = scenario::events(&events)?;
    write_json(
        &out.join("resources.json"),
        &json!({
            "status":"passed",
            "workload":"One 24 MP image holding a Basic layer with all ten fields non-neutral, histogram on",
            "basic_payload":full_basic(),
            "gesture_process":{
                "sampled_peak_rss_mib":hold_usage.peak_rss_mib,
                "sampled_process_cpu_seconds":hold_usage.process_cpu_seconds,
                "scratch":frame["state"]["scratch"],
                "workspace":frame["state"]["workspace"],
                "histogram":frame["state"]["histogram"],
                "controls":frame["state"]["controls"],
            },
            "idle_process":{
                "duration_s":window.duration_s,
                "cpu_percent_one_core":window.cpu_percent_one_core,
                "rss_mib_start":window.rss_mib_start,
                "rss_mib_end":window.rss_mib_end,
                "rss_mib_peak":window.rss_mib_peak,
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

    /// The default Basic exposure target: the field the synthetic burst events carry, and a
    /// placeholder for the curve control, which ignores its `field` argument entirely.
    fn unused_field() -> FieldTarget {
        FieldTarget::basic_exposure()
    }

    #[test]
    fn paint_phase_samples_pair_worker_and_surface_by_generation() {
        let events = vec![
            json!({"event":"mask_draft_set","elapsed_ms":1.0}),
            json!({"event":"mask_draft_preview","elapsed_ms":2.0,"detail":{"generation":6}}),
            json!({"event":"script_step","elapsed_ms":9.0,"detail":{"request":{"mask":{"stroke":{
                "interval_ms":24,"points":[[0.2,0.5]]
            }}}}}),
            json!({"event":"mask_draft_set","elapsed_ms":10.0}),
            json!({"event":"mask_draft_preview","elapsed_ms":14.0,"detail":{
                "generation":7,
                "round_trip_ms":{"executor_wait":1.0,"draft_set":2.0,"preview_job":0.5,"return_to_queue":0.5}
            }}),
            json!({"event":"preview_result_received","elapsed_ms":23.0,"detail":{
                "generation":7,"phase":"proxy","queue_wait_ms":3.0,"render_ms":5.0
            }}),
            json!({"event":"preview_displayed","elapsed_ms":25.0,"detail":{
                "generation":7,"proxy":true
            }}),
        ];

        let (queued, samples) =
            paced_stroke_phase_samples(&events, 1).expect("paired phase sample");
        assert_eq!(queued, 1);
        assert_eq!(samples.len(), 1);
        let sample = &samples[0];
        assert_eq!(sample.generation, 7);
        assert_eq!(sample.phase, "proxy");
        assert!(sample.proxy);
        assert_eq!(sample.owner_round_trip_ms, 4.0);
        assert_eq!(sample.executor_wait_ms, 1.0);
        assert_eq!(sample.draft_set_ms, 2.0);
        assert_eq!(sample.preview_job_ms, 0.5);
        assert_eq!(sample.return_to_queue_ms, 0.5);
        assert_eq!(sample.queue_wait_ms, 3.0);
        assert_eq!(sample.worker_render_ms, 5.0);
        assert_eq!(sample.before_worker_result_ms, 1.0);
        assert_eq!(sample.result_to_surface_ms, 2.0);
        assert_eq!(sample.input_to_presented_ms, 15.0);
    }

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
        let analysis =
            analyze_burst(&synthetic_events(), &unused_field()).expect("a well-formed burst run");
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
        let analysis = analyze_burst(&events, &unused_field()).expect("a well-formed burst run");
        assert_eq!(analysis.proxy["proxy"], json!(true));
        assert_eq!(analysis.proxy["proxy_dimensions"], json!([960, 640]));
        assert!(analysis.proxy.get("note").is_none());
    }

    #[test]
    fn burst_analysis_refuses_a_run_with_no_presented_frame() {
        let events = vec![
            json!({"event":"slider_step_value","elapsed_ms":0.0,"detail":{"value":0.5,"index":0}}),
        ];
        assert!(analyze_burst(&events, &unused_field()).is_err());
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

        let analysis = analyze_burst(&events, &unused_field())
            .expect("an empty drafted set is still a valid run");
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

    /// Every field's burst is the same triangle, scaled about its own origin: exposure's is the
    /// historical ±2 EV one value for value, and Custom temperature's and tint's stay inside their
    /// declared ranges on their own steps, peaking at 40% of the smaller half of the range.
    #[test]
    fn a_fields_burst_is_the_triangle_scaled_to_its_own_range() {
        assert_eq!(FieldTarget::basic_exposure().burst_values(), burst_values());
        let raw_exposure =
            resolve_field(Control::Slider, Some("set-raw-exposure"), Some("ev")).unwrap();
        assert_eq!(raw_exposure.burst_values(), burst_values());
        for (action, parameter, peak) in [
            ("set-raw-temperature", "kelvin", 1800.0),
            ("set-raw-tint", "tint", 40.0),
        ] {
            let field = resolve_field(Control::Slider, Some(action), Some(parameter)).unwrap();
            let values = field.burst_values();
            assert_eq!(values.len(), burst_values().len());
            assert!((values[0] - field.origin).abs() <= field.step / 2.0);
            assert_eq!(*values.last().unwrap(), values[0]);
            for value in &values {
                assert!((field.min..=field.max).contains(value), "{value}");
                assert_eq!(
                    (value / field.step).round() * field.step,
                    *value,
                    "{value} is not on the {} step",
                    field.step
                );
            }
            let max = values.iter().copied().fold(f64::MIN, f64::max);
            let min = values.iter().copied().fold(f64::MAX, f64::min);
            assert!(
                (max - field.origin - peak).abs() <= 2.0 * field.step,
                "{max}"
            );
            assert!(
                (field.origin - min - peak).abs() <= 2.0 * field.step,
                "{min}"
            );
        }
    }

    /// The report counts the presented frames that approximated a drafted white balance, by phase.
    #[test]
    fn approximate_frames_are_counted_by_phase() {
        let events = vec![
            json!({"event":"preview_displayed","detail":{"proxy":true,"approximate_white_balance":true}}),
            json!({"event":"preview_displayed","detail":{"proxy":false,"approximate_white_balance":true}}),
            json!({"event":"preview_displayed","detail":{"proxy":true,"approximate_white_balance":false}}),
            json!({"event":"analysis_adopted","detail":{"approximate_white_balance":true}}),
        ];
        assert_eq!(
            approximate_frames(&events),
            json!({"presented":2,"proxy":1,"full_size":1})
        );
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

        // A RAW slider's action declares that one parameter alone, so its slider drafts too.
        let exposure =
            resolve_field(Control::Slider, Some("set-raw-exposure"), Some("ev")).unwrap();
        assert_eq!(
            (exposure.min, exposure.max, exposure.origin),
            (-5.0, 5.0, 0.0)
        );
        // Custom temperature's range holds no zero, so its gesture starts at the declared 6504 K
        // and every value stays inside 2000..12000 on its 10 K step.
        let kelvin =
            resolve_field(Control::Slider, Some("set-raw-temperature"), Some("kelvin")).unwrap();
        assert_eq!(
            (kelvin.min, kelvin.max, kelvin.step),
            (2000.0, 12000.0, 10.0)
        );
        assert_eq!(kelvin.origin, 6504.0);
        let values = kelvin.gesture_values(31);
        assert_eq!(values[0], 6530.0);
        assert!(values.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(
            values
                .iter()
                .all(|value| (2000.0..=12000.0).contains(value))
        );
        // The neutral pick declares two coordinates, so one of them is not a slider that drafts.
        assert!(resolve_field(Control::Slider, Some("pick-raw-neutral"), Some("x")).is_err());
    }

    /// A draft that accepted its value but whose preview job was refused — a RAW draft whose
    /// development is not in memory — is an input with no frame of its own: it pairs with its
    /// `draft.set`, keeps the next set's pairing intact, and a drag made of them is refused with
    /// the reason rather than timed.
    #[test]
    fn an_unpreviewed_draft_is_an_input_without_a_frame() {
        let field = resolve_field(Control::Slider, Some("set-raw-tint"), Some("tint")).unwrap();
        let events = vec![
            json!({"event":"slider_draft_set","elapsed_ms":1.0,"detail":{"fields":{"tint":3.0}}}),
            json!({"event":"slider_draft_unpreviewed","elapsed_ms":2.0,"detail":{"value":3.0,"draft_revision":1,"error":"preparation-required: source-job-1"}}),
            json!({"event":"slider_draft_set","elapsed_ms":3.0,"detail":{"fields":{"tint":6.0}}}),
            json!({"event":"slider_draft_unpreviewed","elapsed_ms":4.0,"detail":{"value":6.0,"draft_revision":2,"error":"preparation-required: source-job-2"}}),
        ];
        let paired = inputs(&events, Control::Slider, &field).unwrap();
        assert_eq!(paired.len(), 2);
        assert!(paired.iter().all(|input| input.generation.is_none()));
        assert_eq!(
            paired.iter().map(|input| input.value).collect::<Vec<_>>(),
            [3.0, 6.0]
        );
        let mut mismatched = events;
        mismatched[1]["detail"]["value"] = json!(4.0);
        assert!(inputs(&mismatched, Control::Slider, &field).is_err());
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
            origin: 0.0,
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
