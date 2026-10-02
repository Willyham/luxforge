//! Desktop control-to-uploaded-frame measurement: the real editor, gesture and GPU upload.
//!
//! [`editor_performance`](crate::editor_performance) measures exact core rendering over a cached
//! source. Nothing there schedules, uploads or presents, so it cannot answer the responsiveness
//! question the [Basic design][design] asks: how long after a slider input the frame carrying that
//! input is on screen. This module answers it by driving the shipped binary in a background
//! evidence launch, with one `slider` script step per input, and reading the timestamps out of the
//! run's own `events.jsonl`. The default measures Basic's exposure slider; `--control curve`
//! measures the developer proof curve while its canvas is visible in the tools panel, and
//! `--control curve --action <id> --parameter <name>` a registered module's curve instead
//! ([`FieldTarget::lookup_curve`]): the Tone curve's `set-curve` `luminance`, seeded with a
//! mid-tone point ([`MID_TONE_SEED`]) that the gesture drags.
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
use crate::{
    scenario::{
        Launch, Launched, Run,
        launch::{Flag, Poll, Watched, Watcher, stamp, until_exit, watch},
    },
    *,
};
use luxforge_core::{CanvasInteraction, ModuleRegistry, ParameterKind, SourceTag};
use luxforge_evidence::{
    self as script, BrushStep, CurveStep, CurveStepEvent, DraftStep, MaskStep, PaintStep,
    PaletteStep, Reference, SliderEnd, SliderStep, ViewStep, WorkspaceStep,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

/// The Basic module's patch action and the field the gesture drags, named as the module declares
/// them.
const SET_BASIC: &str = "set-basic";
const EXPOSURE: &str = "exposure";
const SET_CONTROLS: &str = "set-controls";
const MASTER: &str = "master";
const CONTROLS_MODULE: &str = "luxforge.controls";

/// The curve a module-curve run commits through its action before the first sample. A fresh
/// curve is `[[0, 0], [1, 1]]`, whose point 1 is the white point; with this seed point 1 is a
/// mid-tone point, at the same `x` as the proof curve's middle point, so the gesture drags a
/// mid-tone and every drafted frame runs the curve's colour unit.
const MID_TONE_SEED: [[f64; 2]; 3] = [[0.0, 0.0], [0.5, 0.5], [1.0, 1.0]];

/// The point a curve gesture drags: point 1, at `x` 0.5, of the proof curve or the seeded curve.
const DRAGGED_POINT: usize = 1;
const DRAGGED_X: f32 = 0.5;

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

/// Which curve a `--control curve` gesture drags, and so how the run is launched and set up.
#[derive(Clone, Debug, PartialEq, Eq)]
enum CurveOwner {
    /// The developer controls proof's master curve: a developer launch, nothing seeded, the proof
    /// section expanded at the bottom of the tools panel.
    Proof,
    /// A registered, non-developer module's curve, named by the module's identity: an ordinary
    /// launch, [`MID_TONE_SEED`] committed through the action first, every section above the
    /// module's collapsed and the module's expanded.
    Module(String),
}

/// The action and parameter a gesture measures. A slider's carries the range and step
/// [`FieldTarget::lookup`] reads from the module registry so every generated gesture value is one
/// the action would actually accept. A curve's ([`FieldTarget::proof_curve`],
/// [`FieldTarget::lookup_curve`]) carries the curve's own `0..=1` coordinates and no step: its
/// gesture is the binary fractions [`gesture_values`] makes, unrelated to any field's step.
struct FieldTarget {
    action: String,
    parameter: String,
    min: f64,
    max: f64,
    step: f64,
    /// Where the gesture's values start from: zero when the range holds it, as every field-patch
    /// slider's does, otherwise the declared default (a RAW photo's Temperature, 2000..12000 K).
    origin: f64,
    /// `Some` for a curve target: whose curve it is.
    curve: Option<CurveOwner>,
}

impl FieldTarget {
    /// The default this harness has always measured, `--action`/`--parameter` absent: Basic's
    /// exposure slider, -5..5 EV in steps of 0.01, mirroring
    /// `luxforge_core::modules::basic::EXPOSURE_MIN/MAX/STEP`, which are not exported.
    fn basic_exposure() -> Self {
        Self {
            action: SET_BASIC.into(),
            parameter: EXPOSURE.into(),
            min: -5.0,
            max: 5.0,
            step: 0.01,
            origin: 0.0,
            curve: None,
        }
    }

    /// `--control curve` without `--action`: the developer proof's master curve, unchanged from
    /// before a module's curve could be named.
    fn proof_curve() -> Self {
        Self {
            action: SET_CONTROLS.into(),
            parameter: MASTER.into(),
            min: 0.0,
            max: 1.0,
            step: 0.0,
            origin: 0.0,
            curve: Some(CurveOwner::Proof),
        }
    }

    /// Resolve `--control curve --action <id> --parameter <name>` against the built-in module
    /// registry, which holds no developer module: the action must be a registered, non-developer
    /// field-patch action, and the parameter one of its curve parameters that can hold
    /// [`MID_TONE_SEED`]. A number parameter is refused by name: it is measured without
    /// `--control curve`.
    fn lookup_curve(action: &str, parameter: &str) -> Result<Self> {
        let registry = ModuleRegistry::builtin();
        let (_, declared) = registry.action(action).ok_or_else(|| {
            format!(
                "No product module declares the action {action}; --control curve --action drives a registered, non-developer module's curve"
            )
        })?;
        let module = registry
            .descriptors()
            .into_iter()
            .find(|module| {
                module
                    .actions
                    .iter()
                    .any(|candidate| candidate.id == action)
            })
            .ok_or_else(|| format!("No module descriptor lists the action {action}"))?;
        ensure(
            !module.developer,
            format!(
                "Action {action} belongs to the developer module {}; --control curve without --action measures the proof curve",
                module.id
            ),
        )?;
        ensure(
            declared.patch,
            format!(
                "Action {action} is not a field-patch action; --control curve drives a field-patch action's curve"
            ),
        )?;
        let parameter_descriptor = declared
            .parameter(parameter)
            .ok_or_else(|| format!("Action {action} declares no parameter {parameter}"))?;
        match &parameter_descriptor.kind {
            ParameterKind::Curve {
                points_min,
                points_max,
                fixed_x: None,
                ..
            } if (*points_min..=*points_max).contains(&MID_TONE_SEED.len()) => {}
            ParameterKind::Curve { .. } => {
                return Err(format!(
                    "Curve parameter {parameter} of {action} cannot hold the mid-tone seed {MID_TONE_SEED:?}"
                )
                .into());
            }
            other => {
                return Err(format!(
                    "Parameter {parameter} of {action} is a {} parameter, not a curve; measure it without --control curve",
                    other.name()
                )
                .into());
            }
        }
        Ok(Self {
            action: action.to_owned(),
            parameter: parameter.to_owned(),
            min: 0.0,
            max: 1.0,
            step: 0.0,
            origin: 0.0,
            curve: Some(CurveOwner::Module(module.id.clone())),
        })
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
            ParameterKind::Curve { .. } => {
                return Err(format!(
                    "Parameter {parameter} of {action} is a curve; measure it with --control curve"
                )
                .into());
            }
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
            curve: None,
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
        // A decimal step (0.01) is taken as a whole number of steps per unit and each value as that
        // many steps divided by it, the nearest f64 to the decimal the slider itself sends:
        // `57.0 * 0.01` is 0.5700000000000001, which no rail fraction sends, while `57.0 / 100.0`
        // is 0.57. A step of one or more is exact as a product.
        let per_unit = (1.0 / self.step).round();
        let decimal = self.step < 1.0 && (per_unit * self.step - 1.0).abs() < 1e-9;
        (0..count)
            .map(|index| {
                let raw = self.origin + (index + 1) as f64 * spacing;
                let steps = (raw / self.step).round();
                let value = if decimal {
                    steps / per_unit
                } else {
                    steps * self.step
                };
                value.clamp(self.min, self.max)
            })
            .collect()
    }
}

/// What `--control` and `--action`/`--parameter` resolve to. Absent, the default Basic exposure
/// slider, or with `--control curve` the developer proof curve, both unchanged from before these
/// options existed; present, both are required together and name a field-patch slider
/// ([`FieldTarget::lookup`]) or, with `--control curve`, a module's curve
/// ([`FieldTarget::lookup_curve`]).
fn resolve_field(
    control: Control,
    action: Option<&str>,
    parameter: Option<&str>,
) -> Result<FieldTarget> {
    match (control, action, parameter) {
        (Control::Slider, None, None) => Ok(FieldTarget::basic_exposure()),
        (Control::Curve, None, None) => Ok(FieldTarget::proof_curve()),
        (Control::Slider, Some(action), Some(parameter)) => FieldTarget::lookup(action, parameter),
        (Control::Curve, Some(action), Some(parameter)) => {
            FieldTarget::lookup_curve(action, parameter)
        }
        _ => Err("--action and --parameter must be given together".into()),
    }
}

/// The kind of photograph `source` is, read from its first bytes: a JPEG starts with its
/// start-of-image marker, and anything else the latency harness opens is a RAW file. The tools
/// panel lists a section only for a module that applies to the photo's kind ([`lists_section`]).
fn source_tag(source: &Path) -> Result<SourceTag> {
    use std::io::Read;
    let mut magic = [0u8; 2];
    fs::File::open(source)?.read_exact(&mut magic)?;
    Ok(if magic == [0xFF, 0xD8] {
        SourceTag::Jpeg
    } else {
        SourceTag::Raw
    })
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

/// A gesture launch's sampled resources as rows of the one shape: its peak RSS and process CPU
/// seconds (from [`sampled`]) and the owner render context's scratch high-water mark at the last
/// captured frame.
fn resource_rows(usage: &Value, last: &Value) -> Vec<Value> {
    let mut rows = vec![
        stats::scalar(
            "sampled_peak_rss_mib",
            "MiB",
            usage["peak_rss_mib"].as_f64(),
        ),
        stats::scalar(
            "sampled_process_cpu_seconds",
            "s",
            usage["process_cpu_seconds"].as_f64(),
        ),
        stats::scalar(
            "scratch_peak_bytes",
            "bytes",
            last["state"]["scratch"]["peak_bytes"].as_f64(),
        ),
        stats::scalar(
            "spatial_scratch_peak_bytes",
            "bytes",
            last["state"]["performance"]["resources"]["budgets"]["spatial"]["peak_bytes"].as_f64(),
        ),
        stats::scalar(
            "last_native_gpu_allocated",
            "MiB",
            resource_mib(last, &["gpu", "allocated_bytes"]),
        ),
    ];
    for field in [
        "full_resident_bytes",
        "region_resident_bytes",
        "retiring_bytes",
        "stage_resident_bytes",
    ] {
        rows.push(counter(
            &format!("last_surface_{field}"),
            "bytes",
            last["state"]["surface"]["gpu"][field].as_u64(),
        ));
    }
    // The process's memory footprint as the Performance section last read it: its lifetime peak,
    // which catches what falls between samples, and its level then. Null where it was not read.
    let memory = &last["state"]["performance"]["resources"]["memory"];
    rows.push(stats::scalar(
        "process_peak_footprint_mib",
        "MiB",
        memory["peak_bytes"]
            .as_f64()
            .map(|bytes| bytes / (1024.0 * 1024.0)),
    ));
    rows.push(stats::scalar(
        "last_footprint_mib",
        "MiB",
        memory["bytes"]
            .as_f64()
            .map(|bytes| bytes / (1024.0 * 1024.0)),
    ));
    // The GPU preview stage's own budget, charged outside the photo slots: the most it has held
    // over the run, what it holds at the last frame, and its budget.
    let preview = &last["state"]["surface"]["gpu"];
    rows.push(counter(
        "gpu_preview_peak_bytes",
        "bytes",
        preview["gpu_preview_peak_bytes"].as_u64(),
    ));
    rows.push(counter(
        "last_gpu_preview_in_use_bytes",
        "bytes",
        preview["gpu_preview_in_use_bytes"].as_u64(),
    ));
    rows.push(counter(
        "gpu_preview_budget_bytes",
        "bytes",
        preview["gpu_preview_budget_bytes"].as_u64(),
    ));
    rows
}

/// A GPU counter as a one-sample row.
fn counter(metric: &str, unit: &str, value: Option<u64>) -> Value {
    stats::scalar(metric, unit, value.map(|count| count as f64))
}

fn elapsed(event: &Value) -> Result<f64> {
    event["elapsed_ms"]
        .as_f64()
        .ok_or_else(|| "An event carries no elapsed_ms".into())
}

/// The one tool name every editor-latency run records itself under.
const TOOL: &str = "editor-latency";

/// Editor-latency's argument order: the evidence directory, the catalog and the data root first,
/// then the script, the photograph and the developer flag.
const ORDER: [Flag; 10] = [
    Flag::Evidence,
    Flag::Catalog,
    Flag::DataRoot,
    Flag::Script,
    Flag::Open,
    Flag::Developer,
    Flag::Disable,
    Flag::Endpoint,
    Flag::Window,
    Flag::GpuIdentity,
];

/// Sample the editor's CPU time and RSS about every 50 ms until it exits, within the launch's
/// deadline counted from the moment the watch starts, just after the spawn. What it returns is the
/// process's `rss_samples`, their peak as `peak_rss_mib`, and `process_cpu_seconds`.
fn sampled(root: &Path, name: &str) -> Watcher {
    let (root, late) = (
        root.to_path_buf(),
        format!("The {name} run exceeded its deadline"),
    );
    Box::new(move |child, _, deadline| {
        let start = Instant::now();
        let sampler = stats::Watch::new(&root, child.child.id());
        let mut rss = Vec::new();
        let mut first_cpu_seconds = None;
        let mut last_cpu_seconds = None;
        let poll = Poll {
            every: Duration::from_millis(50),
            from: start,
            deadline,
            late: &late,
        };
        let status = until_exit(child, poll, |_| {
            if let Ok((cpu_seconds, resident)) = sampler.usage()
                && resident > 0.0
            {
                // `ps` may return a zero RSS row in the race after the child exits but before
                // `try_wait` observes it. Do not let that reset the final CPU sample to zero.
                first_cpu_seconds.get_or_insert(cpu_seconds);
                last_cpu_seconds = Some(cpu_seconds);
                rss.push(json!([start.elapsed().as_secs_f64() * 1000.0, resident]));
            }
            Ok(())
        })?;
        let peak = rss
            .iter()
            .filter_map(|row| row[1].as_f64())
            .fold(0.0, f64::max);
        let process_cpu_seconds = first_cpu_seconds
            .zip(last_cpu_seconds)
            .map(|(first, last)| last - first)
            .filter(|delta| *delta >= 0.01);
        Ok((
            Some(status),
            json!({"rss_samples":rss,"peak_rss_mib":peak,"process_cpu_seconds":process_cpu_seconds}),
        ))
    })
}

/// A scripted gesture launch, the viewport journey's included: its evidence in `<out>/app`, its
/// console in `<name>.log` and its script kept as `file`. The proof curve's adds the developer
/// flag.
fn gesture_launch(
    out: &Path,
    name: &str,
    file: &str,
    steps: &[script::Step],
    source: &Path,
    developer: bool,
) -> Launch {
    let launch = Launch::named(name)
        .evidence_dir(&out.join("app"))
        .script(file, script::write(steps))
        .open_all(&[source.into()])
        .order(ORDER);
    if developer {
        launch.developer()
    } else {
        launch
    }
}

/// The idle launch: the held catalog, reopened by an ordinary launch.
fn idle_launch(catalog: &Path, data: &Path, source: &Path, developer: bool) -> Launch {
    let launch = Launch::ordinary("idle")
        .catalog(catalog)
        .data_root(data)
        .open_all(&[source.into()])
        .order(ORDER);
    if developer {
        launch.developer()
    } else {
        launch
    }
}

/// Which path drew an input's frame: the GPU stage, from the plan its tick handed the surface, or
/// the CPU frame its preview job rendered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FramePath {
    Gpu,
    Cpu,
}

impl FramePath {
    fn name(self) -> &'static str {
        match self {
            Self::Gpu => "gpu",
            Self::Cpu => "cpu",
        }
    }
}

/// One input's journey, from the `draft.set` that carried it to the frame that showed it.
struct Input {
    value: f64,
    /// `slider_draft_set`: the desktop handed this value to the owner. This is the input's time.
    sent_ms: f64,
    /// The tick's answer. On the CPU path `slider_draft_preview`: `draft.set` answered and the
    /// preview job for it queued. On the GPU path `gpu_preview_tick`: the plan of the draft's new
    /// revision handed to the surface in the same update, with no preview job.
    queued_ms: f64,
    /// When the frame carrying the input was presented. A CPU frame: `preview_displayed` for its
    /// job's generation, the update in which its raster became the surface's source and the redraw
    /// that draws it was requested. A GPU frame: the surface's first draw of the plan tagged with
    /// the input's draft revision (`surface_frame_drawn`), since its pixels exist only once the
    /// surface draws them.
    displayed_ms: f64,
    /// The surface's first draw of the frame carrying the input, on either path
    /// (`surface_frame_drawn`): the moment the draw was encoded, before the frame is submitted, so
    /// not display scanout. NaN when no draw of it was logged.
    drawn_ms: f64,
    /// The preview job's generation; `None` on the GPU path, which queues no job, and when the
    /// draft was accepted but its preview job was refused.
    generation: Option<u64>,
    draft_revision: Option<u64>,
    /// The draft a GPU tick drew for, which its draw names too.
    draft_id: Option<String>,
    path: FramePath,
    /// Why a tick of a gesture the GPU stage follows took the CPU path, as its `gpu_preview_tick`
    /// says; `None` on the GPU path and where no GPU tick was logged.
    reason: Option<String>,
    /// `slider_draft_unpreviewed`: the draft accepted the value but its preview job was refused —
    /// a RAW draft whose development is not in memory, while a redevelopment is in flight — so the
    /// input has no frame of its own.
    unpreviewed: bool,
}

impl Input {
    fn new(value: f64, sent_ms: f64, queued_ms: f64, path: FramePath) -> Self {
        Self {
            value,
            sent_ms,
            queued_ms,
            displayed_ms: f64::NAN,
            drawn_ms: f64::NAN,
            generation: None,
            draft_revision: None,
            draft_id: None,
            path,
            reason: None,
            unpreviewed: false,
        }
    }

    /// The input's row of a report.
    fn sample(&self) -> Value {
        let since = |at: f64| at.is_finite().then_some(at - self.sent_ms);
        json!({
            "value": self.value,
            "path": self.path.name(),
            "reason": self.reason,
            "draft_id": self.draft_id,
            "draft_revision": self.draft_revision,
            "generation": self.generation,
            "sent_ms": self.sent_ms,
            "input_to_presented_ms": since(self.displayed_ms),
            "input_to_drawn_ms": since(self.drawn_ms),
        })
    }
}

/// Pair every `draft.set` with the tick that answered it and the frame that showed it.
///
/// The pairing is not a guess: a gesture holds one round trip at a time, so the
/// `slider_draft_preview` or GPU `gpu_preview_tick` that follows a `slider_draft_set` is that
/// set's own answer. A CPU tick's answer carries the preview generation, which `preview_displayed`
/// repeats; a GPU tick's carries the draft and its revision, which the surface's draw of its plan
/// repeats (`surface_frame_drawn`). The value is checked on both ends of a CPU tick, so a
/// mispairing fails the run instead of producing a number.
fn event_value(value: &Value, control: Control) -> Option<f64> {
    match control {
        Control::Slider => value.as_f64(),
        Control::Curve => value.get(1)?.get(1)?.as_f64(),
    }
}

fn inputs(events: &[Value], control: Control, field: &FieldTarget) -> Result<Vec<Input>> {
    let key = field.parameter.as_str();
    let mut inputs = Vec::new();
    let mut pending: Option<(f64, f64)> = None;
    // The reason a CPU tick names, logged before its `slider_draft_preview`.
    let mut reason: Option<String> = None;
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
                reason = None;
            }
            Some("gpu_preview_tick") => {
                let detail = &event["detail"];
                if detail["path"] != "gpu" {
                    reason = detail["reason"].as_str().map(str::to_owned);
                    continue;
                }
                // The tick drawn on the GPU answers the draft.set of its own update; one with no
                // slider set before it is another gesture's.
                let Some((value, sent_ms)) = pending.take() else {
                    continue;
                };
                let mut input = Input::new(value, sent_ms, elapsed(event)?, FramePath::Gpu);
                input.draft_revision = Some(
                    detail["draft_revision"]
                        .as_u64()
                        .ok_or("A GPU tick names no draft revision")?,
                );
                input.draft_id = detail["draft_id"].as_str().map(str::to_owned);
                inputs.push(input);
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
                let mut input = Input::new(value, sent_ms, elapsed(event)?, FramePath::Cpu);
                input.generation = Some(detail["generation"].as_u64().ok_or("No generation")?);
                input.draft_revision =
                    Some(detail["draft_revision"].as_u64().ok_or("No revision")?);
                input.reason = reason.take();
                inputs.push(input);
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
                let mut input = Input::new(value, sent_ms, elapsed(event)?, FramePath::Cpu);
                input.reason = reason.take();
                input.unpreviewed = true;
                inputs.push(input);
            }
            _ => {}
        }
    }
    for event in events.iter().filter(|e| e["event"] == "preview_displayed") {
        let generation = event["detail"]["generation"].as_u64();
        let displayed_ms = elapsed(event)?;
        if let Some(input) = inputs.iter_mut().rev().find(|input| {
            input.generation.is_some()
                && input.generation == generation
                // An unchanged draft may reuse the already displayed generation. Its earlier
                // committed frame is not a response to this input.
                && displayed_ms >= input.queued_ms
        }) {
            ensure(
                event["detail"]["draft_revision"].as_u64() == input.draft_revision,
                "A displayed frame names another draft revision than the job it answers",
            )?;
            // One generation can now display an interactive region, exact refinement and a full
            // frame. Input-to-first-visible-response stops at its first adoption.
            if !input.displayed_ms.is_finite() {
                input.displayed_ms = displayed_ms;
            }
        }
    }
    for (drawn_ms, frame) in drawn_frames(events)? {
        let input = inputs.iter_mut().find(|input| {
            !input.drawn_ms.is_finite()
                && drawn_ms >= input.queued_ms
                && match input.path {
                    FramePath::Gpu => {
                        frame["path"] == "gpu"
                            && frame["draft_revision"].as_u64() == input.draft_revision
                            && (frame["draft_id"].is_null()
                                || frame["draft_id"].as_str() == input.draft_id.as_deref())
                    }
                    FramePath::Cpu => {
                        frame["path"] == "cpu"
                            && input.generation.is_some()
                            && frame["generation"].as_u64() == input.generation
                    }
                }
        });
        if let Some(input) = input {
            input.drawn_ms = drawn_ms;
            if input.path == FramePath::Gpu {
                input.displayed_ms = drawn_ms;
            }
        }
    }
    Ok(inputs)
}

/// Every `surface_frame_drawn` in the run, in log order: when the surface first drew the frame, on
/// the run's clock, and the event's detail naming it.
fn drawn_frames(events: &[Value]) -> Result<Vec<(f64, &Value)>> {
    events
        .iter()
        .filter(|event| event["event"] == "surface_frame_drawn")
        .map(|event| {
            let detail = &event["detail"];
            let drawn_ms = detail["drawn_ms"]
                .as_f64()
                .ok_or("A drawn frame carries no drawn_ms")?;
            Ok((drawn_ms, detail))
        })
        .collect()
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
/// Point 1 of the target's curve — the proof curve's middle point, or the seeded mid-tone point of
/// a module's curve — dragged through `points`, each a height the widget publishes in single
/// precision, through the target's own action and parameter.
fn curve_step(field: &FieldTarget, points: Vec<f64>, finish: SliderEnd) -> script::Step {
    script::Step::Curve(CurveStep {
        action: field.action.clone(),
        parameter: field.parameter.clone(),
        event: CurveStepEvent::Move {
            index: DRAGGED_POINT,
            points: points.into_iter().map(|y| [DRAGGED_X, y as f32]).collect(),
        },
        finish,
    })
}

fn gesture_steps(values: &[f64], control: Control, field: &FieldTarget) -> Vec<script::Step> {
    let last = values.len().saturating_sub(1);
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let release = index == last;
            if control == Control::Curve {
                return curve_step(
                    field,
                    vec![*value],
                    if release {
                        SliderEnd::Release
                    } else {
                        SliderEnd::Open
                    },
                );
            }
            let slider = SliderStep::new(&field.action, &field.parameter, [*value]);
            script::Step::Slider(if release { slider.release() } else { slider })
        })
        .collect()
}

/// One gesture that sends every value at once, as a fast drag does between two ticks. The driver
/// keeps at most one round trip in flight and only the newest value waiting, so this step's
/// `draft.set` count against its value count is the coalescing the design specifies. Each value is
/// reflected to the opposite end of the measured field's own declared range
/// (`min + max - value`), which is exactly negation on Basic exposure's symmetric -5..5 and stays
/// inside an asymmetric range like a mixer or vignette field's too, so the gesture commits a real
/// change rather than the value already current.
fn burst_step(values: &[f64], control: Control, field: &FieldTarget) -> script::Step {
    if control == Control::Curve {
        // Reverse the middle point's vertical journey while remaining in the declared [0,1]
        // range. The burst measures one replaceable pending draft value, not visible frames.
        return curve_step(
            field,
            values
                .iter()
                .map(|value| f64::from((1.0 - value) as f32))
                .collect(),
            SliderEnd::Release,
        );
    }
    let reflected: Vec<f64> = values
        .iter()
        .map(|value| field.min + field.max - value)
        .collect();
    script::Step::Slider(SliderStep::new(&field.action, &field.parameter, reflected).release())
}

/// One step per commit: each value is its own complete gesture, moved and released at once, so the
/// run produces one settled exact histogram per sample instead of one per script.
///
/// This is how the settled-histogram distribution is gathered. Each step opens a draft, sends the
/// one value, commits it, and the commit's own refresh renders and reduces the committed frame; the
/// drafted preview requested in between is superseded before it can be displayed, so this mode also
/// counts one cancelled preview job per commit.
fn commit_steps(values: &[f64], control: Control, field: &FieldTarget) -> Vec<script::Step> {
    values
        .iter()
        .map(|value| match control {
            Control::Curve => curve_step(field, vec![*value], SliderEnd::Release),
            _ => script::Step::Slider(
                SliderStep::new(&field.action, &field.parameter, [*value]).release(),
            ),
        })
        .collect()
}

/// Keep the measured curve, including its canvas, in the real tools-panel viewport during the
/// measurement. A hidden curve would measure only controller/render work and miss tessellation.
///
/// The proof curve's section is the developer section at the bottom of the panel: the sections
/// open by default above it are collapsed, it is expanded and the panel is scrolled to its end, as
/// before a module's curve could be named. A module's curve: every section the panel lists above
/// the module's is collapsed ([`lists_section`]), the module's own is expanded, and the panel is
/// scrolled to its top. A slider target has no view steps.
fn curve_view_steps(field: &FieldTarget, source: SourceTag, masking: bool) -> Vec<script::Step> {
    match &field.curve {
        None => Vec::new(),
        Some(CurveOwner::Proof) => {
            // The latency source is JPEG; the RAW section is absent from its tools model entirely.
            let mut steps: Vec<script::Step> = [
                "luxforge.basic",
                "luxforge.pixel",
                "luxforge.transform",
                "luxforge.crop",
            ]
            .into_iter()
            .map(|module| script::Step::section(module, false))
            .collect();
            steps.push(script::Step::section(CONTROLS_MODULE, true));
            steps.push(script::Step::tools_scroll(1.0));
            steps
        }
        Some(CurveOwner::Module(module)) => {
            let registry = ModuleRegistry::builtin();
            let mut steps: Vec<script::Step> = registry
                .descriptors()
                .into_iter()
                .take_while(|descriptor| descriptor.id != *module)
                .filter(|descriptor| lists_section(descriptor, source, masking))
                .map(|descriptor| script::Step::section(&descriptor.id, false))
                .collect();
            steps.push(script::Step::section(module, true));
            steps.push(script::Step::tools_scroll(0.0));
            steps
        }
    }
}

/// Whether an ordinary (non-developer) editor's tools panel lists a section for `module` over a
/// photograph of kind `source`, by the desktop's own rule (`state::tools::derive`): the module
/// applies to the photo, draws a section (it declares controls, a capability surface or the crop
/// frame) and, in the mask workspace, has a maskable effect. A section step naming a module the
/// panel does not list is refused, so the view steps name exactly these.
fn lists_section(
    module: &luxforge_core::ModuleDescriptor,
    source: SourceTag,
    masking: bool,
) -> bool {
    let draws = !module.controls.is_empty()
        || module.settings.is_some()
        || !module.resources.is_empty()
        || !module.tasks.is_empty()
        || matches!(module.canvas, Some(CanvasInteraction::CropFrame { .. }));
    !module.developer
        && module.applies_to(source)
        && draws
        && (!masking || module.effects.iter().any(|effect| effect.maskable))
}

/// The commit a module-curve run makes before its view steps: [`MID_TONE_SEED`] through the
/// target's own action, so the gesture's point 1 is a mid-tone point. A masked run seeds the
/// masked layer its drag drafts, naming the mask [`mask_precondition`] made.
fn seed_step(field: &FieldTarget, masked: bool) -> script::Step {
    let mut params = serde_json::Map::new();
    params.insert(field.parameter.clone(), json!(MID_TONE_SEED));
    if masked {
        params.insert("mask".into(), json!(Reference::name("Mask 1")));
    }
    script::Step::call(format!("edit.{}", field.action), Value::Object(params))
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
    /// plan](../../docs/specs/performance.md) named as the missing paint-gesture measurement: the
    /// `mask-range` scenario's figure is taken on four masked colour layers, three of whose masks
    /// bind the whole stage, and is therefore not a baseline for the gesture itself.
    Paint,
    /// Native brush hover after a committed masked adjustment, without pressing the pointer.
    /// Fractions are routed through the real window widgets and captured with cursor geometry.
    Hover,
    /// An open drafted adjustment, pans, quiet refinement and release at percentage zoom.
    Viewport,
    /// A crop draft's open: `--samples` Starts at Fit, each held open and then cancelled, over
    /// the recipe the flags commit. From events the editor already logs it reads the Start step to
    /// `crop_draft_started`, and to the frame captured once the crop layer's input stage is on
    /// screen, and from the Performance section the memory and GPU figures while each draft is
    /// open.
    CropStart,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::Drag => "drag",
            Self::Commit => "commit",
            Self::Burst => "burst",
            Self::Paint => "paint",
            Self::Hover => "hover",
            Self::Viewport => "viewport",
            Self::CropStart => "crop-start",
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
/// The triangle's peak, as a fraction of the smaller half of its declared range around an interior
/// origin, or the available range when its origin is a limit: exactly [`BURST_PEAK_EV`] on an
/// exposure's -5..5 EV, 40 on a -100..100 field and 1802 K either side of RAW Temperature's 6504 K.
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
    /// The burst's values for this field: an interior origin follows [`burst_units`], peaking at
    /// [`BURST_PEAK_FRACTION`] of the smaller half of its declared range. A limit origin instead
    /// makes one inward triangle to that fraction of the available range and back. Each value is
    /// on the field's own step grid, as a slider on that step would produce. On an exposure field
    /// (origin 0, -5..5 EV, step 0.01) that is [`burst_values`] itself, value for value; on
    /// RAW Temperature it swings from 6500 K to about 8300 K and 4710 K, and on a -100..100
    /// field ±40.
    fn burst_values(&self) -> Vec<f64> {
        let above = self.max - self.origin;
        let below = self.origin - self.min;
        let at_limit = above == 0.0 || below == 0.0;
        let amplitude = BURST_PEAK_FRACTION
            * if at_limit {
                above.max(below)
            } else {
                above.min(below)
            };
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
        let units = burst_units();
        let last = (units.len() - 1) as f64;
        units
            .into_iter()
            .enumerate()
            .map(|(index, unit)| {
                let unit = if at_limit {
                    let inward = 1.0 - (2.0 * index as f64 / last - 1.0).abs();
                    if above == 0.0 { -inward } else { inward }
                } else {
                    unit
                };
                snap(self.origin + unit * amplitude).clamp(self.min, self.max)
            })
            .collect()
    }
}

/// The one scripted step a burst run sends: every value paced by its own timer, released at the
/// end exactly as a real drag's release ends it.
fn burst_gesture_step(
    field: &FieldTarget,
    values: &[f64],
    interval_ms: u64,
    moving_pan: bool,
) -> script::Step {
    let slider = SliderStep::new(&field.action, &field.parameter, values)
        .release()
        .paced(interval_ms);
    // The same paced tick moves the actual scrollable alongside the slider. A separate Pan script
    // step would run only after the burst released, so it could not qualify moving viewport work.
    let slider = if moving_pan {
        let path = (0..values.len())
            .map(|index| {
                let phase = index as f32 / (values.len().saturating_sub(1).max(1)) as f32;
                let sweep = if phase <= 0.5 {
                    phase * 2.0
                } else {
                    (1.0 - phase) * 2.0
                };
                [0.1 + 0.8 * sweep, 0.9 - 0.8 * sweep]
            })
            .collect();
        slider.pan_path(path)
    } else {
        slider
    };
    script::Step::Slider(slider)
}

pub struct Options<'a> {
    pub source: &'a Path,
    pub samples: usize,
    pub mode: Mode,
    pub control: Control,
    /// `--action`/`--parameter`, resolved with `control` by [`resolve_field`]: when absent, the
    /// default Basic exposure slider or, with `--control curve`, the proof curve; when present, the
    /// named field-patch slider or, with `--control curve`, the named module curve.
    pub action: Option<&'a str>,
    pub parameter: Option<&'a str>,
    /// Commit a straightening crop before the gesture, so the measured stack carries the crop
    /// resample as well as the colour pass.
    pub crop: Option<f64>,
    /// Reopen the gesture's committed stack and measure idle CPU for 30 seconds after it settles.
    pub idle: bool,
    /// Commit a Basic layer with every field non-neutral before the gesture, so the measured
    /// exposure drag runs every one of the module's colour units on each frame.
    pub basic: bool,
    /// Commit a Presence layer with all three fields at full strength before a drag, commit or
    /// crop-start gesture, so the measured stack holds its neighbourhood operations.
    pub presence: bool,
    /// Seed a real moderate global Tone curve, independent of the curve control being measured.
    pub curve_layer: bool,
    /// Seed moderate global Detail before binding any masked gesture target.
    pub detail: bool,
    /// Resolve and select the first eligible offline profile before the measured gesture.
    pub lens: bool,
    /// Seed a nonneutral +20/-10 Perspective before the crop and measured gesture.
    pub perspective: bool,
    /// Draw a linear gradient mask first and bind the panel's sections to it, so the measured
    /// gesture is a *masked* drag: the same slider, drafting and committing a layer the masked
    /// colour primitive evaluates per pixel. It is the end-to-end figure for what a mask costs a
    /// person's hand, with the unmasked run beside it as its baseline.
    pub mask: bool,
    /// Percentage view chosen before the measured gesture; absent means Fit.
    pub zoom: Option<f32>,
    /// Move the scrollable on each paced burst tick; kept explicit so the fixed-view burst script
    /// can also run against the pre-viewport binary for a like-for-like baseline.
    pub moving_pan: bool,
    /// Paint mode only: show the selected mask's tint while the stroke is painted, so each drafted
    /// job fills the overlay's coverage grid beside its frame.
    pub mask_overlay: bool,
    /// Drag mode only: queue this many JPEG exports of the committed stack (`export.jpeg`, the
    /// export lane's one running and four waiting jobs at most) just before the drag, so an exact
    /// render holds the shared pool while it runs, and read the lane's windows back with
    /// `activity.list` after the release.
    pub contend: Option<usize>,
    /// Drag mode only: leave the editor alone this many milliseconds after the preconditions and
    /// before the gesture, so the GPU programs the committed stack's warm list names finish
    /// compiling off the interface thread, as they would before a person's next drag.
    pub warm_ms: Option<u64>,
    /// Drag and commit modes only: turn the GPU preview off from the palette before anything else,
    /// as a person does, so every tick takes the CPU path: the same build's baseline for a GPU run.
    pub gpu_preview_off: bool,
}

fn zoom_step(options: &Options) -> Option<script::Step> {
    options
        .zoom
        .map(|zoom| script::Step::View(ViewStep::Percent(zoom)))
}

/// The steps that draw a mask and bind the generated sections to it before the gesture.
///
/// The mask is created through its own host command, then opened by the name the host gave it —
/// `mask.create-linear` assigns the identity, so a script has nothing else to name it by. From the
/// selection on, every generated slider gesture carries that mask, exactly as the panel's own drag
/// does.
fn mask_precondition() -> [script::Step; 3] {
    [
        script::Step::call(
            "mask.create-linear",
            json!({"x0":0.5,"y0":0.3,"x1":0.5,"y1":0.7}),
        ),
        script::Step::Workspace(WorkspaceStep::default().mode("mask")),
        script::Step::Mask(MaskStep::Select(Reference::name("Mask 1"))),
    ]
}

/// The step that commits the full Basic layer a `--basic` run drags over.
///
/// On a RAW photo Temperature and Tint are the source development's (`set-raw`), and Basic refuses
/// them, so its full layer there is every other field.
fn basic_precondition(options: &Options) -> script::Step {
    let mut fields = full_basic();
    if source_tag(options.source).is_ok_and(|tag| tag == SourceTag::Raw)
        && let Some(fields) = fields.as_object_mut()
    {
        fields.remove("temperature");
        fields.remove("tint");
    }
    script::Step::call("edit.set-basic", fields)
}

/// The paint gesture's own pacing and brush, each a named constant because the report quotes it.
///
/// The interval is a little over the delivered masked-drag median of 16.8–17.9 ms, so every position
/// has a round trip of its own to finish rather than being coalesced into its neighbour — the same
/// choice, and the same figure, the `mask-range` scenario's paced stroke makes. Anything shorter
/// measures the desktop's coalescing instead of the gesture.
const PAINT_INTERVAL_MS: u64 = 24;
/// The brush, in mask-space units and `0..100`: a size in the middle of the declared range and the
/// panel's own default feather, the brush a person paints with. A feathered edge is the ramp the
/// proxy phase point samples; a hard edge (feather 0) is narrower than two proxy pixels, so it would
/// force the 2 × 2 supersample of the mask field on every frame and measure that instead.
const PAINT_SIZE: f64 = 0.06;
const PAINT_FEATHER: f64 = 50.0;
/// The exposure the one masked colour layer holds, in EV. Non-neutral, so the layer exists and its
/// unit runs on every frame the stroke draws.
const PAINT_EV: f64 = 0.6;
/// How many positions the measured stroke paints when `--samples` does not say: hundreds, so a cost
/// that grows with the length of the path already drawn shows between the stroke's early and late
/// positions.
pub const PAINT_POSITIONS: usize = 400;
/// The most positions one paint run takes: the stroke's real time, `PAINT_POSITIONS_MAX ×
/// PAINT_INTERVAL_MS`, stays well inside the editor's 60 s scripted-evidence deadline.
const PAINT_POSITIONS_MAX: usize = 1000;

/// The measured stroke's path: a sine across the frame, in normalized content coordinates, one
/// position per paced interval.
///
/// It is curved on purpose. The stroke's decimated path then keeps positions along its whole
/// length — a straight sweep decimates to its two ends — so the work that is proportional to the
/// path already drawn (its decimation, validation, capture, hashing and index) grows along the
/// stroke as it does under a hand. It advances monotonically in `x` exactly as the straight sweep
/// did, so the component's rectangle grows the same way along the stroke, and its amplitude keeps
/// it inside the frame.
fn paint_path(positions: usize) -> Vec<[f64; 2]> {
    (0..positions)
        .map(|index| {
            let t = index as f64 / (positions.max(2) - 1) as f64;
            [
                0.2 + 0.8 * t,
                0.5 + 0.2 * (std::f64::consts::TAU * 2.5 * t).sin(),
            ]
        })
        .collect()
}

/// One brush mask and one masked colour layer, plus the requested global Detail precondition.
///
/// The seeding stroke is what creates the mask and its `Brush 1`, because a brush declares no
/// geometry and therefore has no `mask.create-brush` to call; the mask it makes is the one that
/// opens, so the Exposure slider under the component list binds to it with nothing to name it by.
fn paint_precondition(options: &Options) -> Vec<script::Step> {
    // Commit global Detail before New Mask changes the action target to the brush mask.
    let mut steps = Vec::new();
    if options.curve_layer {
        steps.push(crate::scenario::recipe::moderate_curve());
    }
    if options.detail {
        steps.push(crate::scenario::recipe::moderate_detail());
    }
    steps.extend(brushed_mask(options));
    steps.push(script::Step::Slider(
        SliderStep::new(SET_BASIC, EXPOSURE, [PAINT_EV]).release(),
    ));
    steps.push(script::Step::Mask(MaskStep::Paint(PaintStep::Component(
        Reference::Index(0),
    ))));
    steps
}

/// Mask mode with the paint brush's settings and one committed brush mask. With
/// `--mask-overlay`, the selected mask's tint is on, so every accepted draft also asks for its
/// coverage grid.
fn brushed_mask(options: &Options) -> Vec<script::Step> {
    let workspace = WorkspaceStep::default().mode("mask");
    let workspace = if options.mask_overlay {
        workspace.mask_overlay("tint")
    } else {
        workspace
    };
    vec![
        script::Step::Workspace(workspace),
        script::Step::Mask(MaskStep::Brush(BrushStep {
            size: Some(PAINT_SIZE),
            feather: Some(PAINT_FEATHER),
            flow: Some(100.0),
            erase: Some(false),
            limit_to_colour: Some(false),
            ..BrushStep::default()
        })),
        script::Step::Mask(MaskStep::Paint(PaintStep::NewMask)),
        script::Step::Mask(MaskStep::Stroke {
            points: vec![[0.2, 0.3], [0.8, 0.3]],
            release: true,
            interval_ms: None,
            settle_between: false,
        }),
    ]
}

/// The optional straightening crop every mode may commit before its gesture.
fn crop_precondition(options: &Options) -> Option<script::Step> {
    options
        .crop
        .map(|angle| script::Step::call("edit.crop-fit", json!({"aspect":"16:9","angle":angle})))
}

/// Geometry is seeded through the same module actions in every measured mode. Lens remains an
/// independent option; Perspective is explicit so comparisons can keep that stack unchanged.
fn geometry_preconditions(options: &Options) -> Vec<script::Step> {
    let raw = source_tag(options.source).is_ok_and(|kind| kind == SourceTag::Raw);
    let mut steps = Vec::new();
    if options.lens {
        steps.extend(crate::scenario::recipe::lens_profile(raw));
    } else if raw {
        // A supported RAW is imported with its detected profile applied; a baseline without Lens
        // turns it off so the two runs differ only by the profile.
        steps.push(crate::scenario::recipe::lens_off());
    }
    if options.perspective {
        steps.push(script::Step::call(
            "edit.set-perspective",
            json!({"horizontal":20,"vertical":-10}),
        ));
    }
    steps.extend(crop_precondition(options));
    steps
}

fn report_geometry(result: &mut Value, options: &Options) {
    result["curve_layer"] = json!(options.curve_layer);
    result["curve_seed"] = if options.curve_layer {
        json!({"luminance":crate::scenario::recipe::MODERATE_CURVE})
    } else {
        Value::Null
    };
    result["lens_layer"] = json!(options.lens);
    result["perspective_layer"] = json!(options.perspective);
    result["perspective_seed"] = if options.perspective {
        json!({"horizontal":20,"vertical":-10})
    } else {
        Value::Null
    };
}

/// The paint run's script: its preconditions, then the one paced stroke along `path`.
fn paint_script(options: &Options, path: Vec<[f64; 2]>) -> Vec<script::Step> {
    let mut steps = geometry_preconditions(options);
    if options.basic {
        steps.push(basic_precondition(options));
    }
    steps.extend(paint_precondition(options));
    steps.extend(zoom_step(options));
    steps.push(script::Step::Mask(MaskStep::Stroke {
        points: path,
        release: true,
        interval_ms: Some(PAINT_INTERVAL_MS),
        // Deliberately left unset: this run is a controlled, dedicated measurement of
        // one host rather than a smoke scenario sharing it with whatever else is running, so
        // `PAINT_INTERVAL_MS` alone is kept as the measurement's own definition rather than folding
        // in the mask-range scenario's load-tolerant wait. See `mask_range_smoke::stroke_latency`.
        settle_between: false,
    }));
    steps
}

/// A drag or commit run's preconditions before any zoom: the crop, mask, Basic and Presence
/// layers asked for, then for a curve its seed (a module's curve only) and its view steps. The
/// frame captured after the last of them is the one the curve's readiness is checked on.
fn setup_steps(options: &Options, field: &FieldTarget, source: SourceTag) -> Vec<script::Step> {
    let mut steps: Vec<script::Step> = options
        .gpu_preview_off
        .then(|| script::Step::Palette(PaletteStep::Run("gpu preview".into())))
        .into_iter()
        .collect();
    steps.extend(geometry_preconditions(options));
    if options.curve_layer {
        steps.push(crate::scenario::recipe::moderate_curve());
    }
    if options.detail {
        steps.push(crate::scenario::recipe::moderate_detail());
    }
    if options.mask {
        steps.extend(mask_precondition());
    }
    if options.basic {
        steps.push(basic_precondition(options));
    }
    if options.presence {
        steps.push(crate::scenario::recipe::full_presence());
    }
    if matches!(field.curve, Some(CurveOwner::Module(_))) {
        steps.push(seed_step(field, options.mask));
    }
    steps.extend(curve_view_steps(field, source, options.mask));
    steps
}

/// The most exports a contended run queues: the export lane's one running job and four waiting.
const CONTEND_MAX: usize = 5;

/// The idle check a drag run with `--idle` makes after its release has dissolved from the drag's
/// last GPU frame: a settle long enough for the dissolve and the committed frame's exact phase and
/// histogram, then the longest window an evidence step takes.
const IDLE_AFTER_DISSOLVE: script::IdleStep = script::IdleStep {
    settle_ms: 4000,
    ms: script::MAX_WAIT_MS,
};

/// The exports a contended run queues before its drag, each to a new file in `dir`.
fn contention_steps(dir: &Path, count: usize) -> Vec<script::Step> {
    (0..count)
        .map(|index| {
            script::Step::call(
                "export.jpeg",
                json!({"destination": dir.join(format!("export-{index}.jpg"))}),
            )
        })
        .collect()
}

/// What a drag run adds after its release, before the burst step: the export lane's activity read
/// back for a contended run, and for `--idle` the Performance section closed — its one-second
/// sampler would wake the editor — and an idle check after the release's dissolve.
fn after_release_steps(options: &Options) -> Vec<script::Step> {
    let mut steps = Vec::new();
    if options.contend.is_some() {
        steps.push(script::Step::call("activity.list", json!({})));
    }
    if options.idle {
        steps.push(script::Step::Performance { expanded: false });
        steps.push(script::Step::Idle(IDLE_AFTER_DISSOLVE));
    }
    steps
}

/// A drag run's script with what `--contend` and `--idle` add around the gesture: the exports just
/// before its first input, and [`after_release_steps`] just after its release. Without either it
/// is [`gesture_script`]'s.
fn measured_script(
    options: &Options,
    field: &FieldTarget,
    source: SourceTag,
    values: &[f64],
    drag: bool,
    contention: &Path,
) -> Vec<script::Step> {
    let mut steps = gesture_script(options, field, source, values, drag);
    if !drag {
        return steps;
    }
    let first = setup_steps(options, field, source).len() + usize::from(options.zoom.is_some());
    let mut exports: Vec<script::Step> = options
        .warm_ms
        .map(|ms| script::Step::Wait { ms })
        .into_iter()
        .collect();
    exports.extend(contention_steps(contention, options.contend.unwrap_or(0)));
    let release = first + exports.len() + values.len();
    steps.splice(first..first, exports);
    steps.splice(release..release, after_release_steps(options));
    steps
}

/// A drag or commit run's script: its preconditions, then the gesture's steps.
fn gesture_script(
    options: &Options,
    field: &FieldTarget,
    source: SourceTag,
    values: &[f64],
    drag: bool,
) -> Vec<script::Step> {
    let mut steps = setup_steps(options, field, source);
    steps.extend(zoom_step(options));
    if drag {
        steps.extend(gesture_steps(values, options.control, field));
        steps.push(burst_step(values, options.control, field));
    } else {
        steps.extend(commit_steps(values, options.control, field));
    }
    steps
}

/// A burst run's script: its preconditions, then the one paced gesture.
fn burst_script(
    options: &Options,
    field: &FieldTarget,
    values: &[f64],
    interval_ms: u64,
) -> Vec<script::Step> {
    let mut steps = geometry_preconditions(options);
    if options.curve_layer {
        steps.push(crate::scenario::recipe::moderate_curve());
    }
    if options.detail {
        steps.push(crate::scenario::recipe::moderate_detail());
    }
    if options.mask {
        steps.extend(mask_precondition());
    }
    if options.basic {
        steps.push(basic_precondition(options));
    }
    steps.extend(zoom_step(options));
    steps.push(burst_gesture_step(
        field,
        values,
        interval_ms,
        options.moving_pan,
    ));
    steps
}

/// Verify the recorded numeric-workload precondition before the measured gesture changes anything.
/// Curve control gestures use their own independently declared target and mid-tone seed.
fn check_curve_layer_seed(frames: &[Value], steps: &[script::Step], expected: bool) -> Result {
    if !expected {
        return Ok(());
    }
    let seed = steps
        .iter()
        .position(|step| *step == crate::scenario::recipe::moderate_curve())
        .ok_or("The real Tone curve precondition step is missing")?;
    let frame = frames
        .get(seed + 1)
        .ok_or("No frame follows the Tone curve precondition")?;
    let layers = frame["state"]["stack"]["layers"]
        .as_array()
        .ok_or("No curve seed stack")?;
    let curve: Vec<_> = layers
        .iter()
        .filter(|layer| layer["effect"] == luxforge_core::CURVE_EFFECT)
        .collect();
    ensure(
        curve.len() == 1
            && curve[0]["mask"].is_null()
            && curve[0]["payload"] == json!({"luminance":crate::scenario::recipe::MODERATE_CURVE}),
        "The numeric workload did not seed its declared real global S-curve layer",
    )
}

/// Refuse a measurement whose captured recipe missed the requested global restoration layer.
fn check_detail_precondition(frame: &Value, expected: bool) -> Result {
    let layers = frame["state"]["stack"]["layers"]
        .as_array()
        .ok_or("The frame records no stack")?;
    let detail: Vec<_> = layers
        .iter()
        .filter(|l| l["effect"] == luxforge_core::DETAIL_EFFECT)
        .collect();
    ensure(
        detail.len() == usize::from(expected),
        "Captured Detail layer count disagrees with --detail",
    )?;
    if expected {
        ensure(
            detail[0]["mask"].is_null()
                && detail[0]["payload"]
                    == json!({"sharpening":60.0,"luminance":40.0,"colour":40.0}),
            "The workload requires the declared moderate global Detail precondition",
        )?;
        let detail_index = layers
            .iter()
            .position(|l| l["effect"] == luxforge_core::DETAIL_EFFECT)
            .unwrap();
        ensure(
            layers
                .iter()
                .enumerate()
                .filter(|(_, l)| l["effect"] == luxforge_core::BASIC_EFFECT)
                .all(|(index, _)| detail_index < index),
            "Detail must precede the measured Basic layers",
        )?;
    }
    Ok(())
}

/// One open draft crosses two pans and a quiet interval. A second value resumes motion before
/// release; after the exact report settles, a final pan tests the retained full texture slot.
fn viewport_script(options: &Options, field: &FieldTarget) -> (Vec<script::Step>, [usize; 7]) {
    let mut steps = geometry_preconditions(options);
    if options.curve_layer {
        steps.push(crate::scenario::recipe::moderate_curve());
    }
    if options.detail {
        steps.push(crate::scenario::recipe::moderate_detail());
    }
    if options.mask {
        steps.extend(mask_precondition());
    }
    if options.basic {
        steps.push(basic_precondition(options));
    }
    steps.extend(zoom_step(options));
    let values = field.gesture_values(2);
    let mut indices = [0; 7];
    steps.push(script::Step::Slider(SliderStep::new(
        &field.action,
        &field.parameter,
        [values[0]],
    )));
    indices[0] = steps.len();
    steps.push(script::Step::pan(0.15, 0.15));
    indices[1] = steps.len();
    steps.push(script::Step::wait(1500));
    indices[2] = steps.len();
    steps.push(script::Step::Slider(SliderStep::new(
        &field.action,
        &field.parameter,
        [values[1]],
    )));
    indices[3] = steps.len();
    steps.push(script::Step::pan(0.75, 0.75));
    steps.push(script::Step::wait(1500));
    indices[4] = steps.len();
    steps.push(script::Step::Slider(
        SliderStep::new(&field.action, &field.parameter, [values[1]]).release(),
    ));
    steps.push(script::Step::wait(2500));
    indices[5] = steps.len();
    steps.push(script::Step::pan(0.3, 0.3));
    steps.push(script::Step::wait(500));
    indices[6] = steps.len();
    (steps, indices)
}

fn frame_at<'a>(frames: &'a [Value], index: usize, label: &str) -> Result<&'a Value> {
    frames
        .get(index)
        .ok_or_else(|| format!("No {label} frame at index {index}").into())
}

fn gpu_count(frame: &Value, name: &str) -> Result<u64> {
    frame["state"]["surface"]["gpu"][name]
        .as_u64()
        .ok_or_else(|| format!("Captured frame has no surface.gpu.{name}").into())
}

/// Pair each pan with the interactive and quiet-settlement generations requested in its own
/// script window. A neighbouring input's frame can never stand in for the pan being measured.
fn pan_region_samples(events: &[Value], windows: &[(usize, usize)]) -> Result<Vec<Value>> {
    let step_index = |step| {
        events
            .iter()
            .position(|event| {
                event["event"] == "script_step" && event["detail"]["step"] == json!(step)
            })
            .ok_or_else(|| format!("Viewport step {step} was never sent"))
    };
    windows.iter().map(|&(pan, end)| {
        let start = step_index(pan)?;
        let stop = step_index(end)?;
        ensure(start < stop, "Viewport pan window is reversed")?;
        let window = &events[start..stop];
        let requested = |intent:&str| -> Result<&Value> {
            window.iter().find(|event|event["event"] == "preview_view_requested" && event["detail"]["intent"] == intent)
                .ok_or_else(|| format!("Pan step {pan} requested no {intent} region").into())
        };
        let interactive = requested("interactive")?;
        let refined = requested("settle")?;
        let shown = |request:&Value, quality:&str| -> Result<&Value> {
            let generation = request["detail"]["generation"].as_u64().ok_or("Viewport request has no generation")?;
            window.iter().find(|event|event["event"] == "preview_displayed"
                && event["detail"]["generation"] == generation && event["detail"]["path"] == "region"
                && event["detail"]["quality"] == quality)
                .ok_or_else(|| format!("Pan step {pan} has no {quality} adoption for generation {generation}").into())
        };
        let interactive_shown = shown(interactive,"interactive")?;
        let exact_shown = shown(refined,"exact")?;
        ensure(interactive_shown["detail"]["draft_revision"].is_u64()
            && interactive_shown["detail"]["draft_revision"] == exact_shown["detail"]["draft_revision"]
            && interactive_shown["detail"]["entry_id"] == exact_shown["detail"]["entry_id"]
            && interactive_shown["detail"]["source_fingerprint"] == exact_shown["detail"]["source_fingerprint"],
            "Pan refinement changed the draft, entry or source identity")?;
        let pan_ms = elapsed(&events[start])?;
        let interactive_ms = elapsed(interactive_shown)?;
        let exact_ms = elapsed(exact_shown)?;
        let refine_ms = elapsed(refined)?;
        ensure(pan_ms <= elapsed(interactive)? && interactive_ms <= refine_ms && refine_ms <= exact_ms,
            "Viewport pan/refinement timestamps are out of order")?;
        Ok(json!({"step":pan,"interactive_generation":interactive["detail"]["generation"],
            "exact_generation":refined["detail"]["generation"],"draft_revision":interactive_shown["detail"]["draft_revision"],
            "source_fingerprint":interactive_shown["detail"]["source_fingerprint"],
            "pan_to_interactive_ms":interactive_ms-pan_ms,"pan_to_exact_ms":exact_ms-pan_ms,
            "refinement_request_to_exact_ms":exact_ms-refine_ms}))
    }).collect()
}

/// Merge the actual observations, rather than averaging the journeys' percentiles.
fn combined_rows(reports: &[Value]) -> Vec<Value> {
    let mut observations: BTreeMap<(String, String), Vec<f64>> = BTreeMap::new();
    for report in reports {
        for row in stats::rows(report) {
            let metric = row["metric"].as_str().unwrap_or_default().to_owned();
            let unit = row["unit"].as_str().unwrap_or_default().to_owned();
            let values = observations.entry((metric, unit)).or_default();
            if let Some(samples) = row["distribution"]["samples"].as_array() {
                values.extend(samples.iter().filter_map(Value::as_f64));
            }
        }
    }
    observations
        .into_iter()
        .map(|((metric, unit), values)| stats::row(&metric, &unit, values))
        .collect()
}

/// A focused native viewport journey. Event timestamps measure desktop adoption; capture-side
/// `surface.gpu` counters report actual draw encoding and writes, never display scanout.
fn run_viewport(root: &Path, out: &Path, bin: &Path, options: &Options) -> Result {
    ensure(
        (1..=60).contains(&options.samples),
        "Viewport samples must be 1..60 sequential journeys",
    )?;
    ensure(
        matches!(options.zoom, Some(100.0 | 200.0)),
        "--mode viewport requires --zoom 100 or --zoom 200",
    )?;
    ensure(
        options.control == Control::Slider,
        "Viewport mode measures a slider",
    )?;
    ensure(!options.idle, "Viewport mode has its own held-draft pause")?;
    let field = resolve_field(options.control, options.action, options.parameter)?;
    let source = options.source.canonicalize()?;
    let source_hash = hash(&source)?;
    let (steps, positions) = viewport_script(options, &field);
    ensure(
        steps.len() <= script::MAX_SCRIPT_STEPS,
        "Viewport script exceeds the evidence step bound",
    )?;
    let run = Run::tool(root, out, TOOL, bin, Duration::from_secs(90))?;
    run.check(|run| {
        let mut reports = Vec::with_capacity(options.samples);
        for journey in 0..options.samples {
            let report = viewport(run,options,&field,&source,&source_hash,(&steps,positions),journey)?;
            ensure(report["status"] == "passed", format!("Viewport journey {journey} is unavailable"))?;
            reports.push(report);
        }
        let mut result = reports[0].clone();
        result.as_object_mut().expect("viewport report").remove("frames");
        result.as_object_mut().expect("viewport report").remove("regions");
        result["rows"] = json!(combined_rows(&reports));
        result["samples"] = json!(options.samples);
        result["pan_samples"] = json!(options.samples*2);
        result["journeys"] = json!((0..options.samples).map(|index|format!("viewport-{index:03}.json")).collect::<Vec<_>>());
        let maximum_load = reports.iter().flat_map(|report|[report["load"]["load_average_1m"].as_f64(),report["load_average_1m_end"].as_f64()]).flatten().reduce(f64::max);
        result["load_before"] = reports[0]["load"].clone();
        result["load"] = launch::load(maximum_load);
        result["load_average_1m_end"] = reports.last().expect("viewport report")["load_average_1m_end"].clone();
        result["load_scope"] = json!("Maximum observed load before/after all sequential journeys; each journey retains its own endpoints");
        result["method"] = json!("Each sample is one sequential background viewport journey with two pans over an open draft, quiet exact refinement, release and retained full-texture pan. The pan/refinement rows pool the actual generation-correlated observations from every journey; filesystem cache is warm, each journey imports/prepares its own source before the measured gesture. Readbacks and per-step settling occur between measured pans. No concurrent editor launches.");
        write_json(&out.join("latency.json"), &result)?;
        println!("PASS editor latency ({} viewport journeys): {}",options.samples,out.display());
        Ok(())
    })
}

/// The viewport journey's launch and its checks, in `run`.
fn viewport(
    run: &mut Run,
    options: &Options,
    field: &FieldTarget,
    source: &Path,
    source_hash: &str,
    sequence: (&[script::Step], [usize; 7]),
    journey: usize,
) -> Result<Value> {
    let (steps, positions) = sequence;
    let out = &run.out().to_path_buf();
    let name = format!("viewport-{journey:03}");
    let load_start = launch::load_average(run.root());
    let viewport = gesture_launch(out, &name, "viewport-script.json", steps, source, false)
        .evidence_dir(&out.join(&name))
        .watch(sampled(run.root(), "viewport"));
    let Launched {
        dir: evidence,
        watched: usage,
    } = run.launch(viewport)?;
    let app = read_json(&evidence.join("result.json"))?;
    ensure(
        app["status"] == "captured" && app["had_input_errors"] == false,
        format!(
            "Viewport evidence did not complete cleanly: {}",
            app["script"]
        ),
    )?;
    let frames = app["frames"]
        .as_array()
        .ok_or("Viewport evidence has no frames")?;
    check_curve_layer_seed(frames, steps, options.curve_layer)?;
    ensure(
        frames.len() == steps.len() + 1,
        "Viewport evidence missed a captured frame",
    )?;
    // Cumulative draw diagnostics expose blank frames between captures as well as at captures.
    // Missing fields mean an older binary cannot qualify the viewport journey.
    let mut stale_draws = 0;
    for (index, frame) in frames.iter().enumerate() {
        let blanks = gpu_count(frame, "blank_photo_draws")?;
        stale_draws = gpu_count(frame, "stale_photo_draws")?;
        ensure(
            blanks == 0,
            format!("Viewport frame {index} followed {blanks} blank photo draws"),
        )?;
    }
    let events = scenario::events(&evidence.join("events.jsonl"))?;
    let header = run.provenance(&events)?;
    let [
        draft,
        first_pan,
        first_pause,
        resumed,
        second_pause,
        settled,
        final_pan,
    ] = positions;
    let [
        draft,
        first_pan,
        first_pause,
        resumed,
        second_pause,
        settled,
        final_pan,
    ] = [
        frame_at(frames, draft, "draft")?,
        frame_at(frames, first_pan, "first pan")?,
        frame_at(frames, first_pause, "first pause")?,
        frame_at(frames, resumed, "resumed")?,
        frame_at(frames, second_pause, "second pause")?,
        frame_at(frames, settled, "settled")?,
        frame_at(frames, final_pan, "final pan")?,
    ];
    let regions: Vec<&Value> = events
        .iter()
        .filter(|event| {
            event["event"] == "preview_displayed" && event["detail"]["path"] == "region"
        })
        .collect();
    if regions.is_empty() {
        let mut result = json!({
            "status":"unavailable",
            "mode":"viewport",
            "reason":"The binary emitted no region preview_displayed events; viewport evidence is unsupported, not a pass",
            "source":source,
            "source_sha256":source_hash,
            "zoom_percent":options.zoom,
        });
        report_geometry(&mut result, options);
        stamp(&mut result, &header);
        write_json(&out.join(format!("{name}.json")), &result)?;
        run.record("latency", json!("unavailable"));
        ensure(hash(source)? == source_hash, "The source changed")?;
        println!("UNAVAILABLE editor latency (viewport): {}", out.display());
        return Ok(result);
    }
    let region_summary: Vec<Value> = regions
        .iter()
        .map(|event| {
            json!({
                "elapsed_ms":event["elapsed_ms"],
                "generation":event["detail"]["generation"],
                "draft_revision":event["detail"]["draft_revision"],
                "entry_id":event["detail"]["entry_id"],
                "snapshot_id":event["detail"]["snapshot_id"],
                "source_fingerprint":event["detail"]["source_fingerprint"],
                "region":event["detail"]["region"],
                "region_stage":event["detail"]["region_stage"],
                "quality":event["detail"]["quality"],
                "viewport_declined":event["detail"]["viewport_declined"],
            })
        })
        .collect();
    for event in &regions {
        let detail = &event["detail"];
        let rect: [u32; 4] = serde_json::from_value(detail["region"].clone())
            .map_err(|_| "A region event has no full-stage rectangle")?;
        let stage: [u32; 2] = serde_json::from_value(detail["dimensions"].clone())
            .map_err(|_| "A region event has no full-stage dimensions")?;
        ensure(
            rect[0] < rect[2]
                && rect[1] < rect[3]
                && rect[2] <= stage[0]
                && rect[3] <= stage[1]
                && detail["generation"].as_u64().is_some()
                && detail["entry_id"].as_str().is_some()
                && detail["source_fingerprint"].as_str().is_some(),
            format!("A region event lacks valid content, generation or stage geometry: {detail}"),
        )?;
    }
    let first_draft_revision = draft["state"]["displayed_draft_revision"]
        .as_u64()
        .ok_or("The first drafted viewport did not display its draft revision")?;
    let resumed_revision = resumed["state"]["displayed_draft_revision"]
        .as_u64()
        .ok_or("Resumed motion did not display its draft revision")?;
    ensure(
        resumed_revision > first_draft_revision,
        "Resumed motion did not advance the displayed draft revision",
    )?;
    ensure(
        regions
            .iter()
            .any(|event| event["detail"]["draft_revision"].as_u64() == Some(first_draft_revision)),
        "No region was displayed for the initial draft",
    )?;
    ensure(
        regions
            .iter()
            .any(|event| event["detail"]["draft_revision"].as_u64() == Some(resumed_revision)),
        "No region was displayed for resumed motion",
    )?;
    ensure(
        draft["state"]["histogram"]["stale"] == true,
        "The first viewport-only draft incorrectly made the full-image histogram current",
    )?;
    for (revision, label) in [
        (first_draft_revision, "first draft"),
        (resumed_revision, "resumed draft"),
    ] {
        ensure(
            regions.iter().any(|event| {
                event["detail"]["draft_revision"].as_u64() == Some(revision)
                    && event["detail"]["quality"] == "interactive"
            }),
            format!("{label} produced no interactive viewport frame"),
        )?;
        ensure(
            regions.iter().any(|event| {
                event["detail"]["draft_revision"].as_u64() == Some(revision)
                    && event["detail"]["quality"] == "exact"
            }),
            format!("{label} never refined to an exact viewport frame while held"),
        )?;
    }
    ensure(
        settled["state"]["histogram"]["stale"] == false
            && settled["state"]["histogram"]["identity"]["draft_revision"].is_null(),
        "Release did not settle an exact full-image histogram",
    )?;
    let histogram = &settled["state"]["histogram"];
    let dimensions = &settled["state"]["preview_dimensions"];
    ensure(
        histogram["identity"]["width"] == dimensions[0]
            && histogram["identity"]["height"] == dimensions[1],
        "The settled histogram does not describe the full rendered stage",
    )?;
    if !histogram["overlay"].is_null() {
        ensure(
            histogram["overlay"]["approximate"] == false,
            "The settled clipping overlay remained approximate",
        )?;
    }
    let before_pan_writes = gpu_count(settled, "photo_writes")?;
    let after_pan_writes = gpu_count(final_pan, "photo_writes")?;
    ensure(
        after_pan_writes == before_pan_writes,
        "A settled pan uploaded photograph pixels instead of reusing the full slot",
    )?;
    ensure(
        gpu_count(settled, "full_resident_bytes")? > 0,
        "The settled full texture is not resident",
    )?;
    ensure(
        final_pan["state"]["surface"]["gpu"]["drawn_full_version"]
            == settled["state"]["surface"]["gpu"]["drawn_full_version"],
        "The settled pan did not draw the same full texture version",
    )?;
    ensure(
        final_pan["state"]["surface"]["gpu"]["drawn_content"]
            == settled["state"]["surface"]["gpu"]["drawn_content"],
        "The settled pan drew a different content identity",
    )?;
    let first_input_ms = events
        .iter()
        .find(|e| e["event"] == "slider_draft_set")
        .map(elapsed)
        .transpose()?
        .ok_or("The viewport run sent no draft input")?;
    let first_region_ms = regions
        .iter()
        .find(|e| e["detail"]["draft_revision"].as_u64() == Some(first_draft_revision))
        .map(|e| elapsed(e))
        .transpose()?
        .ok_or("The first draft has no region event")?;
    // The one scalar and the GPU counters, each a one-sample row. The counters are the photo
    // surface's actual texture writes, counted during draw encoding.
    let pan_samples = pan_region_samples(
        &events,
        &[
            (positions[1], positions[3]),
            (positions[4] - 1, positions[5] - 1),
        ],
    )?;
    let mut rows = vec![
        stats::scalar(
            "input_to_first_region_adoption_ms",
            "ms",
            Some(first_region_ms - first_input_ms),
        ),
        counter(
            "blank_photo_draws",
            "count",
            Some(gpu_count(final_pan, "blank_photo_draws")?),
        ),
        counter("stale_photo_draws", "count", Some(stale_draws)),
        counter(
            "photo_writes_before_settled_pan",
            "count",
            Some(before_pan_writes),
        ),
        counter(
            "photo_writes_after_settled_pan",
            "count",
            Some(after_pan_writes),
        ),
        counter(
            "upload_bytes_before_settled_pan",
            "bytes",
            Some(gpu_count(settled, "upload_bytes")?),
        ),
        counter(
            "upload_bytes_after_settled_pan",
            "bytes",
            Some(gpu_count(final_pan, "upload_bytes")?),
        ),
    ];
    for (metric, field) in [
        (
            "pan_to_interactive_region_adoption",
            "pan_to_interactive_ms",
        ),
        ("pan_to_exact_region_adoption", "pan_to_exact_ms"),
        (
            "quiet_refinement_request_to_exact_adoption",
            "refinement_request_to_exact_ms",
        ),
    ] {
        rows.push(stats::row(
            metric,
            "ms",
            pan_samples
                .iter()
                .filter_map(|sample| sample[field].as_f64()),
        ));
    }
    rows.extend(resource_rows(&usage, final_pan));
    let mut result = json!({
        "status":"passed", "mode":"viewport", "zoom_percent":options.zoom,
        "source":source, "source_sha256":source_hash,
        "backend":settled["state"]["backend"],
        "control_action":field.action, "control_parameter":field.parameter,
        "crop_angle_deg":options.crop, "full_basic_layer":options.basic, "mask":options.mask,
        "rows":rows,
        "pan_samples":pan_samples,"load":launch::load(load_start),"load_average_1m_end":launch::load_average(run.root()),
        "regions":region_summary,
        "frames":{
            "draft":draft["state"], "first_pan":first_pan["state"],
            "first_pause":first_pause["state"], "resumed":resumed["state"],
            "second_pause":second_pause["state"], "settled":settled["state"],
            "settled_pan":final_pan["state"],
        },
        "gpu_note":"The photo_writes and upload_bytes rows are the photo surface's actual texture writes, counted during draw encoding. They do not measure display scanout or backend-owned staging.",
        "scope":"A held drafted slider at percentage zoom, two pans, quiet refinement, resumed motion, release, exact full-image report and a settled pan. preview_displayed is frame adoption, not confirmed GPU upload or display scanout. Captured surface.gpu counters describe actual draw encoding and texture writes.",
    });
    report_geometry(&mut result, options);
    stamp(&mut result, &header);
    write_json(&out.join(format!("{name}.json")), &result)?;
    ensure(hash(source)? == source_hash, "The source changed")?;
    Ok(result)
}

#[derive(Clone, Debug)]
struct PaintPhaseSample {
    generation: u64,
    /// How many positions the stroke had captured when the `draft.set` behind this frame went,
    /// which is how far along the stroke the frame is.
    positions: Option<usize>,
    phase: &'static str,
    proxy: bool,
    /// When the frame was presented, on the run's own clock.
    displayed_ms: f64,
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

/// The run's events from its paced stroke step on.
fn paced_stroke_events(events: &[Value], positions: usize) -> Result<&[Value]> {
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
    Ok(&events[stroke_step..])
}

/// Pair one measured paint input with its owner round-trip, worker result and presented frame.
fn paced_stroke_phase_samples(
    events: &[Value],
    positions: usize,
) -> Result<(usize, Vec<PaintPhaseSample>)> {
    let events = paced_stroke_events(events, positions)?;
    let mut pending: Option<f64> = None;
    let mut inputs: Vec<(f64, u64, [f64; 4], Option<usize>)> = Vec::new();
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
                let positions = event["detail"]["positions"]
                    .as_u64()
                    .and_then(|positions| usize::try_from(positions).ok());
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
                inputs.push((sent, generation, legs, positions));
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
        let Some((sent, _, owner_legs, positions)) = inputs
            .iter()
            .find(|(_, held, _, _)| *held == generation)
            .copied()
        else {
            continue;
        };
        let owner_round_trip_ms = owner_legs.iter().sum::<f64>();
        if !seen.insert(generation) {
            continue;
        }
        let proxy = displayed["detail"]["proxy"].as_bool().unwrap_or(false);
        let phase = if displayed["detail"]["path"] == json!("region") {
            "region"
        } else if proxy {
            "proxy"
        } else {
            "exact"
        };
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
            positions,
            phase,
            proxy,
            displayed_ms,
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

/// One paint input drawn on the GPU: its `mask_draft_set`, the GPU tick that handed the plan of its
/// draft revision to the surface in the same update, and the surface's first draw of that plan.
struct PaintGpuSample {
    draft_revision: u64,
    /// How many positions the stroke had captured when the `draft.set` behind this frame went.
    positions: Option<usize>,
    /// When the surface first drew it, on the run's own clock.
    displayed_ms: f64,
    input_to_presented_ms: f64,
    tick_to_drawn_ms: f64,
}

/// The paced stroke's GPU ticks, and those of them the surface drew: each `mask_draft_set` paired
/// with the GPU `gpu_preview_tick` of its own update, and that tick's draft and revision with the
/// first `surface_frame_drawn` naming them. Returns how many ticks were drawn on the GPU path and
/// the samples of those whose plan the surface drew before a newer one replaced it.
fn paced_stroke_gpu_samples(
    events: &[Value],
    positions: usize,
) -> Result<(usize, Vec<PaintGpuSample>)> {
    let events = paced_stroke_events(events, positions)?;
    let mut pending: Option<(f64, Option<usize>)> = None;
    let mut ticks = Vec::new();
    for event in events {
        match event["event"].as_str() {
            Some("mask_draft_set") => {
                let carried = event["detail"]["fields"]["points"].as_array().map(Vec::len);
                pending = Some((elapsed(event)?, carried));
            }
            Some("mask_draft_preview") => pending = None,
            Some("gpu_preview_tick") if event["detail"]["path"] == "gpu" => {
                if let Some((sent, carried)) = pending.take() {
                    let detail = &event["detail"];
                    let revision = detail["draft_revision"]
                        .as_u64()
                        .ok_or("A GPU tick names no draft revision")?;
                    ticks.push((
                        sent,
                        elapsed(event)?,
                        revision,
                        detail["draft_id"].as_str(),
                        carried,
                    ));
                }
            }
            _ => {}
        }
    }
    let drawn = drawn_frames(events)?;
    let samples = ticks
        .iter()
        .filter_map(|(sent, tick, revision, draft, carried)| {
            drawn
                .iter()
                .find(|(at, frame)| {
                    at >= tick
                        && frame["path"] == "gpu"
                        && frame["draft_revision"].as_u64() == Some(*revision)
                        && (frame["draft_id"].is_null() || frame["draft_id"].as_str() == *draft)
                })
                .map(|(at, _)| PaintGpuSample {
                    draft_revision: *revision,
                    positions: *carried,
                    displayed_ms: *at,
                    input_to_presented_ms: at - sent,
                    tick_to_drawn_ms: at - tick,
                })
        })
        .collect();
    Ok((ticks.len(), samples))
}

/// Every frame of the stroke that reached the screen, on either path: when it was presented and how
/// many positions its `draft.set` carried.
fn presented_paint_frames(
    cpu: &[PaintPhaseSample],
    gpu: &[PaintGpuSample],
) -> Vec<(f64, Option<usize>)> {
    cpu.iter()
        .map(|sample| (sample.displayed_ms, sample.positions))
        .chain(
            gpu.iter()
                .map(|sample| (sample.displayed_ms, sample.positions)),
        )
        .collect()
}

/// The paced stroke's press, as drag mode's press rows read a slider's: from the stroke's first
/// `mask_stroke_position`, logged in the update that hands the press to the desktop, to the first
/// `mask_draft_set` after it and to the first of the stroke's frames presented. Every later position
/// is timed from its own `draft.set`, so these are the only figures that include whatever the press
/// waits for before its first `draft.set` goes. One stroke is one press, so a run adds one sample to
/// each; a first frame is `None` when no frame of the stroke reached the screen.
fn stroke_press(
    events: &[Value],
    positions: usize,
    presented: &[(f64, Option<usize>)],
) -> Result<(f64, Option<f64>)> {
    let events = paced_stroke_events(events, positions)?;
    let press = events
        .iter()
        .find(|event| {
            event["event"] == json!("mask_stroke_position")
                && event["detail"]["index"].as_u64() == Some(0)
        })
        .ok_or("The paced stroke logged no press")?;
    let press = elapsed(press)?;
    let first_set = events
        .iter()
        .filter(|event| event["event"] == json!("mask_draft_set"))
        .map(elapsed)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .find(|at| *at >= press)
        .ok_or("The press sent no mask draft.set")?;
    let first_frame = presented
        .iter()
        .map(|(displayed_ms, _)| *displayed_ms)
        .filter(|at| *at >= press)
        .reduce(f64::min)
        .map(|at| at - press);
    Ok((first_set - press, first_frame))
}

/// Every paced position's own wait for the screen: from its `mask_stroke_position`, logged in the
/// update that hands it to the desktop, to the first of the stroke's frames presented after it whose
/// `draft.set` already carried it (the frame's `positions` reach past the position's index).
///
/// It is timed from the input rather than from a `draft.set`, so it counts a position however the
/// desktop gets it to the owner, and a position whose own frame was superseded is answered by the
/// newer frame that carries it, which is what a hand sees. Returns `(index, milliseconds)` for every
/// position some presented frame carried.
fn position_latencies(
    events: &[Value],
    positions: usize,
    frames: &[(f64, Option<usize>)],
) -> Result<Vec<(usize, f64)>> {
    let events = paced_stroke_events(events, positions)?;
    let mut presented: Vec<(f64, usize)> = frames
        .iter()
        .filter_map(|(displayed_ms, carried)| Some((*displayed_ms, (*carried)?)))
        .collect();
    presented.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut latencies = Vec::new();
    for event in events
        .iter()
        .filter(|event| event["event"] == json!("mask_stroke_position"))
    {
        let index = event["detail"]["index"]
            .as_u64()
            .and_then(|index| usize::try_from(index).ok())
            .ok_or("A stroke position carries no index")?;
        let at = elapsed(event)?;
        if let Some((shown, _)) = presented
            .iter()
            .find(|(shown, carried)| *shown >= at && *carried > index)
        {
            latencies.push((index, shown - at));
        }
    }
    Ok(latencies)
}

/// How long each of the stroke's presented frames waited for its mask overlay: from its
/// `preview_displayed` to the `mask_overlay` of the same generation, which is logged in the update
/// that hands the grid to the presenter. Empty when no overlay was asked for.
fn overlay_lags(events: &[Value], samples: &[PaintPhaseSample]) -> Result<Vec<f64>> {
    let mut lags = Vec::new();
    for sample in samples {
        let overlay = events.iter().find(|event| {
            event["event"] == json!("mask_overlay")
                && event["detail"]["generation"].as_u64() == Some(sample.generation)
        });
        if let Some(overlay) = overlay {
            lags.push(elapsed(overlay)? - sample.displayed_ms);
        }
    }
    Ok(lags)
}

/// Whether a frame belongs to the stroke's first or last quarter, by how many positions its
/// `draft.set` carried: the split that shows whether a position's cost grows with the path already
/// drawn.
fn stroke_quarter(positions: Option<usize>, total: usize) -> Option<&'static str> {
    let positions = positions?;
    if positions * 4 <= total {
        Some("early")
    } else if positions * 4 > total * 3 {
        Some("late")
    } else {
        None
    }
}

/// Accepted stroke inputs with separate photograph-adoption and authoritative-coverage samples.
/// An input that was superseded before either feedback arrived appears in the input count only.
#[derive(Debug, Default)]
pub struct StrokeFeedback {
    pub inputs: usize,
    pub photograph_ms: Vec<f64>,
    pub coverage_ms: Vec<f64>,
}

/// Keep photograph adoption and authoritative coverage feedback separate. Unbound mask changes
/// can reuse the photograph; their accepted draft identity/revision pairs only with coverage,
/// and never acquires an invented photograph rendering or presentation time.
pub fn paced_stroke_latencies(events: &[Value]) -> Result<StrokeFeedback> {
    let mut pending: Option<(f64, Option<String>)> = None;
    let mut inputs: Vec<(f64, u64, Option<String>, Option<u64>)> = Vec::new();
    for event in events {
        match event["event"].as_str() {
            Some("mask_draft_set") => {
                pending = Some((
                    elapsed(event)?,
                    event["detail"]["draft_id"].as_str().map(str::to_owned),
                ))
            }
            Some("mask_draft_preview") => {
                let Some((sent, draft_id)) = pending.take() else {
                    return Err("A mask_draft_preview answered no mask_draft_set".into());
                };
                let generation = event["detail"]["generation"]
                    .as_u64()
                    .ok_or("A mask draft preview named no generation")?;
                inputs.push((
                    sent,
                    generation,
                    draft_id,
                    event["detail"]["draft_revision"].as_u64(),
                ));
            }
            _ => {}
        }
    }
    let mut feedback = StrokeFeedback {
        inputs: inputs.len(),
        ..StrokeFeedback::default()
    };
    for (sent, generation, draft_id, revision) in &inputs {
        let identity_matches = |event: &Value| {
            let stamp = &event["detail"]["identity"]["draft"];
            draft_id
                .as_deref()
                .is_some_and(|id| stamp["draft_id"].as_str() == Some(id))
                && revision.is_some()
                && stamp["draft_revision"].as_u64() == *revision
        };
        let reused = events
            .iter()
            .any(|event| event["event"] == "preview_pixels_reused" && identity_matches(event));
        if let Some(covered) = events.iter().find(|event| {
            event["event"] == "mask_coverage_ready"
                && identity_matches(event)
                && event["elapsed_ms"].as_f64().is_some_and(|at| at >= *sent)
        }) {
            feedback.coverage_ms.push(elapsed(covered)? - *sent);
        }
        if !reused
            && let Some(displayed) = events.iter().find(|event| {
                event["event"] == "preview_displayed"
                    && event["detail"]["generation"].as_u64() == Some(*generation)
                    && revision.is_none_or(|revision| {
                        event["detail"]["draft_revision"].as_u64() == Some(revision)
                    })
                    && event["elapsed_ms"].as_f64().is_some_and(|at| at >= *sent)
            })
        {
            feedback.photograph_ms.push(elapsed(displayed)? - *sent);
        }
    }
    Ok(feedback)
}

/// The paint mode: one paced brush stroke on a bare masked recipe, measured end to end.
///
/// This is the measurement the [performance plan](../../docs/specs/performance.md) named as
/// untaken. It shares nothing with [`run`]'s slider path beyond the launch and the reporting,
/// because the two gestures are different: a stroke's positions are a path rather than a field's
/// values, its draft is the mask gesture's own, and its frames are paired through
/// `mask_draft_preview` rather than `slider_draft_preview`.
fn run_paint(root: &Path, out: &Path, bin: &Path, options: &Options) -> Result {
    ensure(
        (2..=PAINT_POSITIONS_MAX).contains(&options.samples),
        format!(
            "Paint samples must be 2..{PAINT_POSITIONS_MAX} positions; a stroke of one position has no path, and the stroke's real time must stay inside the editor's scripted-evidence deadline"
        ),
    )?;
    // The paced stroke itself takes samples × interval of real time on top of the launch and the
    // setup steps; allow generously for both plus the launch wrapper.
    let stroke = Duration::from_millis(PAINT_INTERVAL_MS * options.samples as u64);
    let run = Run::tool(root, out, TOOL, bin, Duration::from_secs(90) + stroke)?;
    run.check(|run| paint(run, options))
}

fn hover_script(options: &Options) -> Result<(Vec<script::Step>, Vec<usize>)> {
    let field = FieldTarget::lookup(
        options.action.unwrap_or("set-presence"),
        options.parameter.unwrap_or("clarity"),
    )?;
    let value = if field.parameter == EXPOSURE {
        0.5
    } else if field.parameter == "clarity" {
        50.0
    } else {
        field.gesture_values(1)[0]
    };
    let mut steps = geometry_preconditions(options);
    if options.curve_layer {
        steps.push(crate::scenario::recipe::moderate_curve());
    }
    if options.basic {
        steps.push(basic_precondition(options));
    }
    if options.detail {
        steps.push(crate::scenario::recipe::moderate_detail());
    }
    if options.presence {
        steps.push(crate::scenario::recipe::full_presence());
    }
    // Reproduce the reported route: one committed brush mask with an adjustment, then New Mask
    // and cursor motion with no press. The new tool's overlay is enabled by its own UI path.
    steps.extend(brushed_mask(options));
    steps.push(script::Step::Slider(
        SliderStep::new(&field.action, &field.parameter, [value]).release(),
    ));
    steps.extend(zoom_step(options));
    steps.push(script::Step::Mask(MaskStep::Paint(PaintStep::NewMask)));
    let points = (0..options.samples)
        .map(|index| {
            let t = index as f32 / options.samples.saturating_sub(1).max(1) as f32;
            [
                0.2 + 0.6 * t,
                0.5 + 0.15 * (t * std::f32::consts::TAU).sin(),
            ]
        })
        .collect();
    steps.push(script::Step::CanvasHoverSweep {
        points,
        interval_ms: 16,
    });
    let hovers = vec![steps.len()];
    ensure(
        steps.len() <= script::MAX_SCRIPT_STEPS,
        "Hover script exceeds its step bound",
    )?;
    Ok((steps, hovers))
}

fn run_hover(root: &Path, out: &Path, bin: &Path, options: &Options) -> Result {
    ensure(
        (1..=240).contains(&options.samples),
        "Hover samples must be 1..240",
    )?;
    ensure(
        options.control == Control::Slider,
        "Hover drives a masked adjustment and needs --control slider",
    )?;
    let run = Run::tool(root, out, TOOL, bin, Duration::from_secs(90))?;
    run.check(|run| {
        let source = options.source.canonicalize()?;
        let source_hash = hash(&source)?;
        let (steps, hovers) = hover_script(options)?;
        let load_start = launch::load_average(root);
        let launched = gesture_launch(out, "hover", "hover-script.json", &steps, &source, false)
            .watch(sampled(root, "hover"));
        let Launched { dir: evidence, watched: usage } = run.launch(launched)?;
        let app = read_json(&evidence.join("result.json"))?;
        ensure(app["status"] == "captured" && app["had_input_errors"] == json!(false)
            && app["script"].as_array().is_some_and(|recorded| recorded.len() == steps.len()),
            format!("A hover step failed or never ran: {}", app["script"]))?;
        let events = scenario::events(&evidence.join("events.jsonl"))?;
        let header = run.provenance(&events)?;
        let frames = app["frames"].as_array().ok_or("Missing frames")?;
    check_curve_layer_seed(frames, &steps, options.curve_layer)?;
        let records = app["script"].as_array().ok_or("Missing script records")?;
        let probe = &records[hovers[0]-1]["cursor_probe"];
        let traces: Vec<Value> = probe["samples"].as_array().cloned().unwrap_or_else(|| vec![probe.clone()]);
        let coalesced = probe["coalesced"].as_u64().ok_or("Probe records no coalescing count")?;
        ensure(traces.len() + coalesced as usize == options.samples, "Hover lost input without naming coalescing")?;
        for (index, trace) in traces.iter().enumerate() {
            ensure(trace["epoch"] == json!(index+1) && trace["readout_position"].is_array()
                && trace["input_to_cursor_geometry_ms"].as_f64().is_some()
                && trace["editor_update_ms"].as_f64().is_some(),
                format!("Hover {} has no real mask cursor and readout route: {trace}",index+1))?;
        }
        let shown = frame_at(frames, hovers[0], "hover")?;
        ensure(shown["state"]["masks"]["masks"].as_array().is_some_and(|masks| masks.len()==1),
            "Hover committed an extra mask without a press")?;
        let sent = events.iter().position(|event| event["event"] == "script_step"
            && event["detail"]["step"] == json!(hovers[0])).ok_or("No hover was sent")?;
        let hover_events = &events[sent..];
        let photo_jobs = hover_events.iter().filter(|event| event["event"] == "preview_job_requested").count();
        let draft_sets = hover_events.iter().filter(|event| event["event"] == "mask_draft_set").count();
        ensure(photo_jobs == 0 && draft_sets == 0, "Hover queued photograph work or altered a mask")?;
        let point_queries = hover_events.iter().filter(|event| event["event"] == "pointer_sample_requested").count();
        let retained_reads = hover_events.iter().filter(|event| event["event"] == "pointer_retained_readout").count();
        // At Fit over a settled frame every readout comes from the retained exact raster, so a
        // regression back to per-move point queries cannot pass.
        if options.zoom.is_none() {
            ensure(point_queries == 0 && retained_reads > 0,
                format!("Hover asked {point_queries} point queries and read {retained_reads} retained pixels at Fit"))?;
        }
        let mut rows = Vec::new();
        for (metric,field) in [("input_to_cursor_geometry","input_to_cursor_geometry_ms"),
            ("native_widget_update","widget_update_ms"),("cursor_geometry","cursor_geometry_ms"),
            ("native_dispatch_delay","dispatch_delay_ms"),("input_to_editor_pointer_update","input_to_pointer_update_ms"),
            ("editor_pointer_update","pointer_update_ms"),("editor_update","editor_update_ms"),
            ("editor_rederive","editor_rederive_ms")] {
            rows.push(stats::row(metric,"ms",traces.iter().filter_map(|trace|trace[field].as_f64())));
        }
        let baseline = frame_at(frames, hovers[0]-1, "hover baseline")?;
        let last = frames.last().ok_or("No captured frame")?;
        ensure(baseline["state"]["surface"]["version"] == last["state"]["surface"]["version"],
            "Hover replaced the photograph surface")?;
        rows.extend(resource_rows(&usage,last));
        let mut result = json!({
            "status":"passed","mode":"hover","source":source,"source_sha256":source_hash,
            "source_dimensions":baseline["state"]["source_dimensions"],
            "preview_dimensions":baseline["state"]["preview_dimensions"],
            "backend":baseline["state"]["backend"],"physical_size":baseline["physical_size"],"scale":baseline["scale"],
            "samples":options.samples,"action":options.action.unwrap_or("set-presence"),
            "parameter":options.parameter.unwrap_or("clarity"),"zoom_percent":options.zoom,
            "method":"Background native window, masked Clarity +50 by default (Exposure +0.5 EV with --action set-basic --parameter exposure), followed by New Mask and a continuous path scheduled every 16 ms without pressing. Each emitted move is dispatched through the real laid-out MaskCanvas and surrounding mouse_area, paired by epoch with MaskCanvas::draw geometry and its matching editor update/rederive; overdue positions are coalesced before dispatch and counted. One correlated native screenshot captures the final cursor after the sweep. CPU geometry may be built before the queued editor pointer update, so their timings are separate; neither measures GPU completion or scanout.",
            "scope":"input to CPU brush cursor geometry construction/submission; no GPU completion or display scanout claim",
            "rows":rows,"traces":traces,"load":launch::load(load_start),
            "load_average_1m_end":launch::load_average(root),
            "hover_work":{"point_queries":point_queries,"retained_exact_reads":retained_reads,"photograph_jobs":photo_jobs,"draft_sets":draft_sets,"emitted":traces.len(),"coalesced":coalesced,"interval_ms":16},
            "resources":{"rss_samples":usage["rss_samples"]},
            "checks":["Each move emitted its real surrounding pointer message and built brush cursor geometry",
                "No hover queued a photograph job, posted a mask draft or committed a second mask","Source SHA-256 is unchanged"]
        });
        report_geometry(&mut result, options);
        stamp(&mut result,&header);
        write_json(&out.join("latency.json"),&result)?;
        ensure(hash(&source)? == source_hash,"The source changed")?;
        println!("PASS editor latency (hover): {}",out.display());
        Ok(())
    })
}

/// The paint mode's launch and its report, in `run`.
fn paint(run: &mut Run, options: &Options) -> Result {
    let (root, out) = (&run.root().to_path_buf(), &run.out().to_path_buf());
    let source = options.source.canonicalize()?;
    let source_hash = hash(&source)?;
    let path = paint_path(options.samples);

    let steps = paint_script(options, path);
    let load_start = launch::load_average(root);
    let gesture = gesture_launch(
        out,
        "gesture",
        "gesture-script.json",
        &steps,
        &source,
        false,
    )
    .watch(sampled(root, "gesture"));
    let Launched {
        dir: evidence,
        watched: usage,
    } = run.launch(gesture)?;

    let app = read_json(&evidence.join("result.json"))?;
    ensure(
        app["status"] == "captured",
        "The paint run captured nothing",
    )?;
    let events = scenario::events(&evidence.join("events.jsonl"))?;
    let header = run.provenance(&events)?;
    ensure(
        app["had_input_errors"] == json!(false)
            && app["script"]
                .as_array()
                .is_some_and(|recorded| recorded.len() == steps.len()),
        format!("A paint step failed or never ran: {}", app["script"]),
    )?;
    let frames = app["frames"].as_array().ok_or("Missing frames")?;
    check_curve_layer_seed(frames, &steps, options.curve_layer)?;
    let last = frames.last().ok_or("No frame was captured")?;
    check_detail_precondition(last, options.detail)?;
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
    let (gpu_ticks, gpu_samples) = paced_stroke_gpu_samples(&events, options.samples)?;
    let presented = presented_paint_frames(&phase_samples, &gpu_samples);
    let feedback = paced_stroke_latencies(paced_stroke_events(&events, options.samples)?)?;
    let cpu_latencies: Vec<f64> = phase_samples
        .iter()
        .map(|sample| sample.input_to_presented_ms)
        .collect();
    let gpu_latencies: Vec<f64> = gpu_samples
        .iter()
        .map(|sample| sample.input_to_presented_ms)
        .collect();
    let latencies: Vec<f64> = cpu_latencies
        .iter()
        .chain(&gpu_latencies)
        .copied()
        .collect();
    ensure(
        !latencies.is_empty() || !feedback.coverage_ms.is_empty(),
        "The run painted no stroke with photograph or authoritative coverage feedback",
    )?;
    let input_p95 = stats::Distribution::of(latencies.clone()).map(|d| d.p95);
    let load_end = launch::load_average(root);
    let mut rows = vec![
        stats::row("input_to_presented_frame", "ms", latencies.clone()),
        stats::row("gpu_input_to_presented_frame", "ms", gpu_latencies),
        stats::row("cpu_input_to_presented_frame", "ms", cpu_latencies),
        stats::row(
            "gpu_tick_to_drawn_frame",
            "ms",
            gpu_samples.iter().map(|sample| sample.tick_to_drawn_ms),
        ),
    ];
    rows.push(stats::row(
        "input_to_authoritative_mask_coverage",
        "ms",
        feedback.coverage_ms.iter().copied(),
    ));
    type Phase = fn(&PaintPhaseSample) -> f64;
    let phases: [(&str, Phase); 9] = [
        ("owner_round_trip_to_preview_queue", |s| {
            s.owner_round_trip_ms
        }),
        ("executor_wait", |s| s.executor_wait_ms),
        ("draft_set", |s| s.draft_set_ms),
        ("preview_job_planning", |s| s.preview_job_ms),
        ("owner_return_to_preview_queue", |s| s.return_to_queue_ms),
        ("preview_request_to_worker_start", |s| s.queue_wait_ms),
        ("preview_worker_render", |s| s.worker_render_ms),
        ("worker_result_to_surface_assignment", |s| {
            s.result_to_surface_ms
        }),
        ("pre_result_residual", |s| s.before_worker_result_ms),
    ];
    for (metric, phase) in phases {
        rows.push(stats::row(metric, "ms", phase_samples.iter().map(phase)));
    }
    let (press_to_set, press_to_frame) = stroke_press(&events, options.samples, &presented)?;
    rows.push(stats::row("press_to_first_draft_set", "ms", [press_to_set]));
    rows.push(stats::row(
        "press_to_first_presented_frame",
        "ms",
        press_to_frame,
    ));
    let carried = position_latencies(&events, options.samples, &presented)?;
    rows.push(stats::row(
        "position_to_presented_frame",
        "ms",
        carried.iter().map(|(_, ms)| *ms),
    ));
    // The stroke's first and last quarters side by side: a cost proportional to the path already
    // drawn shows as a late row above its early one.
    for quarter in ["early", "late"] {
        rows.push(stats::row(
            &format!("{quarter}_position_to_presented_frame"),
            "ms",
            carried
                .iter()
                .filter(|(index, _)| {
                    stroke_quarter(Some(index + 1), options.samples) == Some(quarter)
                })
                .map(|(_, ms)| *ms),
        ));
        let in_quarter = || {
            phase_samples.iter().filter(move |sample| {
                stroke_quarter(sample.positions, options.samples) == Some(quarter)
            })
        };
        let quartered: [(&str, Phase); 4] = [
            ("input_to_presented_frame", |s| s.input_to_presented_ms),
            ("draft_set", |s| s.draft_set_ms),
            ("preview_job_planning", |s| s.preview_job_ms),
            ("preview_worker_render", |s| s.worker_render_ms),
        ];
        for (metric, phase) in quartered {
            rows.push(stats::row(
                &format!("{quarter}_{metric}"),
                "ms",
                in_quarter().map(phase),
            ));
        }
    }
    let overlay_lag = overlay_lags(&events, &phase_samples)?;
    if options.mask_overlay {
        ensure(
            !overlay_lag.is_empty() || !feedback.coverage_ms.is_empty(),
            "The run showed the mask overlay but no frame of the stroke drew its grid",
        )?;
    }
    rows.push(stats::row(
        "presented_frame_to_mask_overlay",
        "ms",
        overlay_lag.iter().copied(),
    ));
    rows.extend(resource_rows(&usage, last));

    let mut result = json!({
        "status":"passed",
        "source":source,
        "source_sha256":source_hash,
        "source_dimensions":last["state"]["source_dimensions"],
        "preview_dimensions":last["state"]["preview_dimensions"],
        "backend":last["state"]["backend"],
        "physical_size":last["physical_size"],
        "scale":last["scale"],
        "crop_angle_deg":options.crop,
        "full_basic_layer":options.basic,
        "detail_layer":options.detail,
        "mode":"paint",
        "samples":options.samples,
        "mask_overlay":if options.mask_overlay {"tint"} else {"off"},
        "method":format!("Background evidence launch of the release binary, warm filesystem cache. One curved brush stroke of {} positions is handed to the desktop one per {} ms in real time, so the first tick presses, each later one moves and the last releases: one paced step is still one stroke and one history entry. Each mask draft.set is paired with its preview job and displayed frame by the generation mask_draft_preview carries, or, drawn on the GPU, with the GPU tick of its own update and the surface's first draw of that tick's plan, and each position with the first presented frame whose draft.set carried it. The early_ and late_ rows are the stroke's first and last quarters by the positions a frame's draft.set carried; their phase rows are the CPU frames'. Presented means, for a CPU frame, preview_displayed: the update in which the rendered raster became the photo surface's source; for a GPU frame, the surface's first draw of the tick's plan (surface_frame_drawn). Neither is display scanout.", options.samples, PAINT_INTERVAL_MS),
        "recipe":{
            "masks":masks.len(),
            "components":components,
            "masked_layers":masked_layers,
            "global_detail_layers":usize::from(options.detail),
            "layers":last["state"]["stack"]["layers"],
            "reads_pixels":false,
            "note":"One unlimited brush mask of one component and one masked Basic exposure layer, with requested global Basic/Detail and crop preconditions recorded. The brush reads no input pixels, so this measures overlay coverage and restoration-prefix reuse rather than the value-mask input-grid cache.",
        },
        "brush":{"size":PAINT_SIZE,"feather":PAINT_FEATHER,"flow":100.0,"erase":false,
            "exposure_ev":PAINT_EV,
            "note":"Feathered, so the proxy phase point samples the mask field; a hard edge would force its 2 × 2 supersample."},
        "stroke":{
            "interval_ms":PAINT_INTERVAL_MS,
            "positions":options.samples,
            "path":"a sine across the frame: x = 0.2 + 0.8t, y = 0.5 + 0.2 sin(5πt), t in 0..1",
            "positions_carried_to_the_screen":carried.len(),
            "inputs_that_queued_a_preview":queued,
            "displayed":latencies.len(),
            "gpu_ticks":gpu_ticks,
            "gpu_frames_drawn":gpu_samples.len(),
            "authoritative_coverage_feedback":feedback.coverage_ms.len(),
            "superseded":(queued + gpu_ticks).saturating_sub(latencies.len()),
            "superseded_note":"A position whose own preview job was superseded by the next position before its pixels were drawn. It is what a hand does not see during a continuous stroke, and it is reported rather than averaged away.",
        },
        "provisional_input_to_frame_target":{"p95_below_ms":16.0,"acceptable_below_ms":32.0,
            "measured_p95_ms":input_p95,
            "met":input_p95.map(|ms| ms < 16.0),"acceptable":input_p95.map(|ms| ms < 32.0)},
        "rows":rows,
        "press_note":"press_to_first_draft_set and press_to_first_presented_frame start at the stroke's first mask_stroke_position, logged in the update that hands the press to the desktop, and end at its first mask draft.set and at the first preview_displayed of any of the stroke's own frames; they are the only rows that include what a press waits for before its first draft.set. A stroke has one press, so each run adds one sample to each.",
        "load":launch::load(load_start),
        "load_average_1m_end":load_end,
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
        "resources":{
            "scratch":last["state"]["scratch"],
            "rss_samples":usage["rss_samples"],
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
    result["gpu_samples"] = json!(
        gpu_samples
            .iter()
            .map(|sample| json!({
                "draft_revision": sample.draft_revision,
                "positions": sample.positions,
                "input_to_presented_frame_ms": sample.input_to_presented_ms,
                "gpu_tick_to_drawn_frame_ms": sample.tick_to_drawn_ms,
            }))
            .collect::<Vec<_>>()
    );
    report_geometry(&mut result, options);
    stamp(&mut result, &header);
    write_json(&out.join("latency.json"), &result)?;
    ensure(hash(&source)? == source_hash, "The source changed")?;

    println!("PASS editor latency (paint): {}", out.display());
    Ok(())
}

/// How long a crop-start run holds each draft open, and waits after each Cancel and before the
/// first Start: longer than the Performance section's one-second sampling interval, so the frame
/// captured at its end carries a sample taken while the draft, or the photograph alone, was on
/// screen.
const CROP_START_HOLD_MS: u64 = 1500;

/// The crop-start run's script and the step numbers of its Starts: the recipe, the Performance
/// section opened and a settled baseline, then per sample a Start held open, its Cancel and a
/// settle.
fn crop_start_script(options: &Options) -> (Vec<script::Step>, Vec<usize>) {
    let mut steps = geometry_preconditions(options);
    if options.curve_layer {
        steps.push(crate::scenario::recipe::moderate_curve());
    }
    if options.basic {
        steps.push(basic_precondition(options));
    }
    if options.detail {
        steps.push(crate::scenario::recipe::moderate_detail());
    }
    if options.presence {
        steps.push(crate::scenario::recipe::full_presence());
    }
    steps.push(script::Step::performance(true));
    steps.push(script::Step::wait(CROP_START_HOLD_MS));
    let mut starts = Vec::new();
    for _ in 0..options.samples {
        steps.push(script::Step::Draft(DraftStep::Start));
        // Script steps are numbered from one, in the order the editor runs them.
        starts.push(steps.len());
        steps.push(script::Step::wait(CROP_START_HOLD_MS));
        steps.push(script::Step::Draft(DraftStep::Cancel));
        steps.push(script::Step::wait(CROP_START_HOLD_MS));
    }
    (steps, starts)
}

fn run_crop_start(root: &Path, out: &Path, bin: &Path, options: &Options) -> Result {
    ensure(
        (1..=14).contains(&options.samples),
        "Crop-start samples must be 1..14: each is four script steps, and the script takes at most 64",
    )?;
    let run = Run::tool(root, out, TOOL, bin, Duration::from_secs(90))?;
    run.check(|run| crop_start(run, options))
}

/// A Performance-section figure of one captured frame, in MiB.
fn resource_mib(frame: &Value, path: &[&str]) -> Option<f64> {
    let mut value = &frame["state"]["performance"]["resources"];
    for key in path {
        value = &value[*key];
    }
    value.as_f64().map(|bytes| bytes / (1024.0 * 1024.0))
}

/// The crop-start mode's launch and its report, in `run`.
fn crop_start(run: &mut Run, options: &Options) -> Result {
    let (root, out) = (&run.root().to_path_buf(), &run.out().to_path_buf());
    let source = options.source.canonicalize()?;
    let source_hash = hash(&source)?;
    let (steps, starts) = crop_start_script(options);
    ensure(
        steps.len() <= script::MAX_SCRIPT_STEPS,
        "Crop-start recipe and samples exceed the 64-step evidence bound",
    )?;
    let load_start = launch::load_average(root);
    let launch = gesture_launch(
        out,
        "crop-start",
        "crop-start-script.json",
        &steps,
        &source,
        false,
    )
    .watch(sampled(root, "crop-start"));
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
                .is_some_and(|recorded| recorded.len() == steps.len()),
        format!("A crop-start step failed or never ran: {}", app["script"]),
    )?;
    let events = scenario::events(&evidence.join("events.jsonl"))?;
    let header = run.provenance(&events)?;
    let frames = app["frames"].as_array().ok_or("Missing frames")?;
    check_curve_layer_seed(frames, &steps, options.curve_layer)?;
    // The opened frame comes first, then one frame per step.
    let frame = |step: usize| frame_at(frames, step, "crop-start");
    let baseline = frame(starts[0] - 1)?;
    // The first event of `name` after the script_step that sent `step`.
    let after = |step: usize, name: &str| -> Result<f64> {
        let sent = events
            .iter()
            .position(|event| {
                event["event"] == "script_step" && event["detail"]["step"] == json!(step)
            })
            .ok_or_else(|| format!("Step {step} was never sent"))?;
        let found = events[sent + 1..]
            .iter()
            .find(|event| event["event"] == name)
            .ok_or_else(|| format!("No {name} followed step {step}"))?;
        Ok(elapsed(found)? - elapsed(&events[sent])?)
    };
    let mut to_started = Vec::new();
    let mut to_captured = Vec::new();
    let mut memory = Vec::new();
    let mut gpu = Vec::new();
    let mut per_start = Vec::new();
    for &step in &starts {
        let started = after(step, "crop_draft_started")?;
        let captured = after(step, "frame_captured")?;
        let shown = frame(step)?;
        ensure(
            shown["state"]["crop"]["drafting"] == json!(true)
                && shown["state"]["crop"]["input_stage_loaded"] == json!(true),
            format!("Step {step}'s frame shows no crop draft on its input stage"),
        )?;
        let held = frame(step + 1)?;
        let (held_memory, held_gpu) = (
            resource_mib(held, &["memory", "bytes"]),
            resource_mib(held, &["gpu", "allocated_bytes"]),
        );
        to_started.push(started);
        to_captured.push(captured);
        memory.extend(held_memory);
        gpu.extend(held_gpu);
        per_start.push(json!({
            "step":step,
            "start_to_crop_draft_started_ms":started,
            "start_to_stage_frame_captured_ms":captured,
            "held_memory_mib":held_memory,
            "held_gpu_allocated_mib":held_gpu,
            "stage_frame":held["state"]["crop"]["input_stage_frame"],
            "stage_resident_bytes":held["state"]["surface"]["gpu"]["stage_resident_bytes"],
        }));
    }
    let load_end = launch::load_average(root);
    let mut rows = vec![
        stats::row("start_to_crop_draft_started", "ms", to_started),
        stats::row("start_to_stage_frame_captured", "ms", to_captured),
        stats::row("held_draft_memory", "MiB", memory),
        stats::row("held_draft_gpu_allocated", "MiB", gpu),
        stats::scalar(
            "baseline_memory",
            "MiB",
            resource_mib(baseline, &["memory", "bytes"]),
        ),
        stats::scalar(
            "baseline_gpu_allocated",
            "MiB",
            resource_mib(baseline, &["gpu", "allocated_bytes"]),
        ),
    ];
    let last = frames.last().ok_or("No frame was captured")?;
    rows.extend(resource_rows(&usage, last));
    let mut result = json!({
        "status":"passed",
        "source":source,
        "source_sha256":source_hash,
        "source_dimensions":baseline["state"]["source_dimensions"],
        "preview_dimensions":baseline["state"]["preview_dimensions"],
        "backend":baseline["state"]["backend"],
        "physical_size":baseline["physical_size"],
        "scale":baseline["scale"],
        "full_basic_layer":options.basic,
        "presence_layer":options.presence,
        "detail_layer":options.detail,
        "lens_layer":options.lens,
        "mode":"crop-start",
        "samples":options.samples,
        "method":format!("Background evidence launch of the release binary, warm filesystem cache, at Fit. Each sample is a crop draft Start step held open for {CROP_START_HOLD_MS} ms, then Cancel and {CROP_START_HOLD_MS} ms more. Times are from the script_step event that sent the Start: to crop_draft_started, which the Start's own update logs, and to the frame_captured of the Start step, which the editor captures once the crop layer's input stage is on screen, so that figure includes the window readback. Memory and GPU are the Performance section's resources.read figures in the frame captured at the end of each hold, sampled at most one second earlier while the draft was open."),
        "rows":rows,
        "starts":per_start,
        "load":launch::load(load_start),
        "load_average_1m_end":load_end,
        "resources":{"rss_samples":usage["rss_samples"]},
        "scope":"Opening a crop draft at Fit on this source and recipe: CropMessage::Start to crop_draft_started, and to the captured frame showing the input stage; and the process memory and GPU allocation while the draft is open, beside the settled baseline before the first Start.",
    });
    report_geometry(&mut result, options);
    stamp(&mut result, &header);
    write_json(&out.join("latency.json"), &result)?;
    ensure(hash(&source)? == source_hash, "The source changed")?;
    println!("PASS editor latency (crop-start): {}", out.display());
    Ok(())
}

pub fn run(root: &Path, out: &Path, bin: &Path, options: Options) -> Result {
    ensure(
        cfg!(target_os = "macos"),
        "Editor latency measurement currently reads native macOS ps only",
    )?;
    ensure(
        !options.idle || matches!(options.mode, Mode::Drag | Mode::Commit),
        "--idle requires --mode drag or commit",
    )?;
    ensure(
        options.mode != Mode::CropStart || options.zoom.is_none(),
        "Crop-start is measured at Fit; omit --zoom",
    )?;
    if let Some(zoom) = options.zoom {
        ensure(
            zoom.is_finite() && (10.0..=1600.0).contains(&zoom),
            "--zoom is a percentage from 10 to 1600",
        )?;
    }
    ensure(
        !options.moving_pan || (options.mode == Mode::Burst && options.zoom.is_some()),
        "--moving-pan requires --mode burst and --zoom PERCENT",
    )?;
    ensure(
        options.control == Control::Slider || matches!(options.mode, Mode::Drag | Mode::Commit),
        "--control curve measures a drag or a commit; pass --mode drag or --mode commit",
    )?;
    ensure(
        !options.gpu_preview_off || matches!(options.mode, Mode::Drag | Mode::Commit),
        "--no-gpu-preview measures a drag or a commit; pass --mode drag or --mode commit",
    )?;
    if let Some(ms) = options.warm_ms {
        ensure(
            options.mode == Mode::Drag && (1..=script::MAX_WAIT_MS).contains(&ms),
            format!(
                "--warm waits 1 to {} ms before a drag; pass --mode drag",
                script::MAX_WAIT_MS
            ),
        )?;
    }
    if let Some(count) = options.contend {
        ensure(
            options.mode == Mode::Drag && (1..=CONTEND_MAX).contains(&count),
            format!("--contend queues 1 to {CONTEND_MAX} exports before a drag; pass --mode drag"),
        )?;
        ensure(
            !options.idle,
            "--contend and --idle measure different things; run them separately",
        )?;
    }
    if options.mode == Mode::Viewport {
        return run_viewport(root, out, bin, &options);
    }
    if options.mode == Mode::Burst {
        return run_burst(root, out, bin, &options);
    }
    if options.mode == Mode::Paint {
        return run_paint(root, out, bin, &options);
    }
    if options.mode == Mode::Hover {
        return run_hover(root, out, bin, &options);
    }
    if options.mode == Mode::CropStart {
        return run_crop_start(root, out, bin, &options);
    }
    ensure(
        (1..=60).contains(&options.samples),
        "Samples must be 1..60; the evidence script accepts at most 64 steps",
    )?;
    ensure(
        options.control != Control::Curve || options.samples <= 32,
        "Curve samples must be 1..32 so every middle-point fraction stays in range",
    )?;
    let field = resolve_field(options.control, options.action, options.parameter)?;
    // The editor's own evidence deadline is 25 s; allow for the launch wrapper around it.
    let run = Run::tool(root, out, TOOL, bin, Duration::from_secs(60))?;
    run.check(|run| gesture(run, &options, &field))
}

/// A drag or commit run's launch and report, followed by an idle launch on its committed catalog
/// when `--idle` asks for one.
fn gesture(run: &mut Run, options: &Options, field: &FieldTarget) -> Result {
    let (root, out) = (&run.root().to_path_buf(), &run.out().to_path_buf());
    let source = options.source.canonicalize()?;
    let source_hash = hash(&source)?;
    let load_start = launch::load_average(root);
    // A drag needs one more value than the measured sample count: the extra one is the release,
    // whose own drafted preview the commit supersedes, so it is measured to the settled histogram
    // instead. A commit run measures every value it sends.
    let drag = options.mode == Mode::Drag;
    let values = gesture_values(options.samples + usize::from(drag), options.control, field);
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

    let kind = source_tag(&source)?;
    let contention = out.join("contention");
    if options.contend.is_some() {
        fs::create_dir_all(&contention)?;
    }
    let steps = measured_script(options, field, kind, &values, drag, &contention);
    ensure(
        steps.len() <= script::MAX_SCRIPT_STEPS,
        "The latency script exceeds the 64-step evidence bound",
    )?;
    let gesture = gesture_launch(
        out,
        "gesture",
        "gesture-script.json",
        &steps,
        &source,
        field.curve == Some(CurveOwner::Proof),
    )
    .watch(sampled(root, "gesture"));
    let Launched {
        dir: evidence,
        watched: usage,
    } = run.launch(gesture)?;

    let app = read_json(&evidence.join("result.json"))?;
    ensure(
        app["status"] == "captured",
        "The gesture run captured nothing",
    )?;
    let events = scenario::events(&evidence.join("events.jsonl"))?;
    let header = run.provenance(&events)?;
    ensure(
        app["had_input_errors"] == json!(false)
            && app["script"]
                .as_array()
                .is_some_and(|recorded| recorded.len() == steps.len()),
        format!("A gesture step failed or never ran: {}", app["script"]),
    )?;
    let frames = app["frames"].as_array().ok_or("Missing frames")?;
    check_curve_layer_seed(frames, &steps, options.curve_layer)?;
    let last = frames.last().ok_or("No frame was captured")?;
    if options.detail {
        let seed = steps
            .iter()
            .position(|step| *step == crate::scenario::recipe::moderate_detail())
            .ok_or("The Detail precondition step is missing")?;
        let seeded = frames
            .get(seed + 1)
            .ok_or("No frame follows the Detail precondition")?;
        check_detail_precondition(seeded, true)?;
    }

    if let Some(owner) = &field.curve {
        // Frame 0 is the open; frame `n` follows step `n`.
        let setup = frames
            .get(setup_steps(options, field, kind).len())
            .ok_or("No captured frame follows the curve viewport setup")?;
        let curve = setup["state"]["control_ui"]["curves"]
            .as_array()
            .and_then(|curves| {
                curves.iter().find(|curve| {
                    curve["action"] == field.action.as_str()
                        && curve["parameter"] == field.parameter.as_str()
                })
            });
        let sampled = curve.is_some_and(|curve| {
            curve["sample_count"] == 257 && curve["sample_source_entry"] == curve["display_entry"]
        });
        match owner {
            CurveOwner::Proof => ensure(
                sampled
                    && setup["state"]["developer"] == true
                    && setup["state"]["expanded"][CONTROLS_MODULE] == true
                    && setup["state"]["tools_scroll"] == 1.0,
                "The proof curve was not expanded, scrolled into view and sampled to 257 points before timing",
            )?,
            CurveOwner::Module(module) => {
                let seeded = curve
                    .and_then(|curve| curve["points"].as_array())
                    .is_some_and(|points| {
                        points.len() == MID_TONE_SEED.len()
                            && points.iter().zip(MID_TONE_SEED).all(|(point, [x, y])| {
                                point[0].as_f64() == Some(x) && point[1].as_f64() == Some(y)
                            })
                    });
                ensure(
                    sampled
                        && seeded
                        && setup["state"]["developer"] == false
                        && setup["state"]["expanded"][module.as_str()] == true
                        && setup["state"]["tools_scroll"] == 0.0,
                    format!(
                        "The {module} curve was not seeded with {MID_TONE_SEED:?}, expanded, scrolled into view and sampled to 257 points before timing"
                    ),
                )?;
            }
        }
    }

    let measured = inputs(&events, options.control, field)?;
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
    // What the frame captured after each drained input's own step shows: a GPU input's frame is the
    // GPU stage's output of its own revision, as the surface reported drawing it. A CPU input's
    // frame may already have been redrawn on the GPU by then — its tick asked for the boundary, and
    // the plan of the same revision is drawn once it is held — so it is recorded, not checked.
    let first_input = setup_steps(options, field, kind).len()
        + usize::from(options.zoom.is_some())
        + usize::from(options.warm_ms.is_some())
        + options.contend.unwrap_or(0);
    // The GPU stage's compile queue just before the first input: what the warm list handed it, and
    // how much had compiled.
    let before =
        &frame_at(frames, first_input, "the frame before the gesture")?["state"]["surface"]["gpu"];
    let compile_queue = json!({
        "warm_ms": options.warm_ms,
        "compiles": before["gpu_preview_compiles"],
        "compiled": before["gpu_preview_compiled"],
    });
    let mut captured = Vec::new();
    for (index, input) in drained.iter().enumerate() {
        let frame = frame_at(frames, first_input + index + 1, "a drained input's step")?;
        let gpu = &frame["state"]["surface"]["gpu"];
        if input.path == FramePath::Gpu {
            ensure(
                gpu["drawing_path"] == "gpu"
                    && gpu["drawn_gpu_revision"].as_u64() == input.draft_revision,
                format!(
                    "{} was drawn on the GPU for draft revision {:?}, but its step's capture shows \
                     path {} at revision {}",
                    frame["file"],
                    input.draft_revision,
                    gpu["drawing_path"],
                    gpu["drawn_gpu_revision"]
                ),
            )?;
        }
        captured.push(json!({"frame": frame["file"], "path": gpu["drawing_path"],
            "drawn_gpu_revision": gpu["drawn_gpu_revision"], "gpu_ms": frame["state"]["status_bar"]["gpu_ms"],
            "approximate": gpu["gpu_preview"]["drag"]["approximate"]}));
    }
    if options.gpu_preview_off {
        ensure(
            last["state"]["workspace"]["gpu_preview"] == false
                && drained.iter().all(|input| {
                    input.path == FramePath::Cpu
                        && input
                            .reason
                            .as_deref()
                            .is_none_or(|reason| reason == "preference-off")
                }),
            "--no-gpu-preview left the GPU preview on, or a drained input drew another way",
        )?;
    }
    let unpreviewed = measured.iter().filter(|input| input.unpreviewed).count();
    ensure(
        !drained.iter().any(|input| input.unpreviewed),
        format!(
            "{unpreviewed} of {} draft.set answers of {} carried no preview job (slider_draft_unpreviewed): the core refused to preview a drafted value, so the drag has no frame per input to time",
            measured.len(),
            field.action
        ),
    )?;
    ensure(
        drained.iter().all(|input| input.displayed_ms.is_finite()),
        "An input's frame was never presented — a CPU tick's preview job never displayed, or a GPU \
         tick's plan never drawn — so the gesture was not drained per step",
    )?;

    // Each path's figures over the drained inputs it drew: `to - from`, where both were observed.
    let span = |path: Option<FramePath>, to: fn(&Input) -> f64, from: fn(&Input) -> f64| {
        drained
            .iter()
            .filter(|input| path.is_none_or(|path| input.path == path))
            .map(|input| to(input) - from(input))
            .filter(|ms| ms.is_finite())
            .collect::<Vec<f64>>()
    };
    let presented: fn(&Input) -> f64 = |input| input.displayed_ms;
    let drawn: fn(&Input) -> f64 = |input| input.drawn_ms;
    let queued: fn(&Input) -> f64 = |input| input.queued_ms;
    let sent: fn(&Input) -> f64 = |input| input.sent_ms;
    let input_to_frame = span(None, presented, sent);
    let set_round_trip = span(None, queued, sent);
    let render_and_upload = span(Some(FramePath::Cpu), presented, queued);
    let contended = match options.contend {
        Some(count) => Some(contention_windows(&events, &app, count)?),
        None => None,
    };
    let in_contention = |input: &Input| {
        contended.as_ref().is_some_and(|(windows, _)| {
            windows
                .iter()
                .any(|(start, end)| (*start..=*end).contains(&input.sent_ms))
        })
    };
    let input_p95 = stats::Distribution::of(input_to_frame.clone()).map(|d| d.p95);
    // A gesture's press: its `slider_draft_begin`, logged in the update that opens the draft, paired
    // with the first measured `draft.set` after it and, in a drag, the frame that set's preview job
    // was presented as. Every later input of a gesture is timed from its own `draft.set` above, so
    // these are the only rows that include what the press waits for before its first `draft.set`.
    // A drag has one press per run and a commit run one per sample.
    let presses: Vec<f64> = events
        .iter()
        .filter(|event| event["event"] == "slider_draft_begin")
        .map(elapsed)
        .collect::<Result<Vec<_>>>()?;
    let press_to_first_set: Vec<f64> = presses
        .iter()
        .filter_map(|press| {
            measured
                .iter()
                .find(|input| input.sent_ms >= *press)
                .map(|input| input.sent_ms - press)
        })
        .collect();
    let press_to_first_frame: Vec<f64> = presses
        .first()
        .zip(drained.first())
        .map(|(press, input)| input.displayed_ms - press)
        .into_iter()
        .collect();
    let mut rows = vec![
        stats::row("input_to_presented_frame", "ms", input_to_frame),
        stats::row("input_to_drawn_frame", "ms", span(None, drawn, sent)),
        stats::row(
            "gpu_input_to_presented_frame",
            "ms",
            span(Some(FramePath::Gpu), presented, sent),
        ),
        stats::row(
            "cpu_input_to_presented_frame",
            "ms",
            span(Some(FramePath::Cpu), presented, sent),
        ),
        stats::row("draft_set_round_trip", "ms", set_round_trip),
        stats::row("render_and_upload", "ms", render_and_upload),
        stats::row(
            "gpu_tick_to_drawn_frame",
            "ms",
            span(Some(FramePath::Gpu), presented, queued),
        ),
        stats::row("press_to_first_draft_set", "ms", press_to_first_set),
        stats::row("press_to_first_presented_frame", "ms", press_to_first_frame),
    ];
    let contended_samples: Vec<f64> = drained
        .iter()
        .filter(|input| in_contention(input))
        .map(|input| input.displayed_ms - input.sent_ms)
        .collect();
    if let Some((_, report)) = &contended {
        ensure(
            !contended_samples.is_empty(),
            format!(
                "No drained input was sent while an export ran, so nothing was measured under \
                 contention: {report}"
            ),
        )?;
        rows.push(stats::row(
            "contended_input_to_presented_frame",
            "ms",
            contended_samples.iter().copied(),
        ));
    }
    let idle = if options.idle && drag {
        Some(idle_after_dissolve(&events)?)
    } else {
        None
    };
    if let Some(idle) = &idle {
        let check = &idle["check"];
        rows.push(stats::scalar(
            "idle_after_dissolve_process_cpu_percent_one_core",
            "%",
            check["process_cpu_percent_one_core"].as_f64(),
        ));
        rows.push(counter(
            "idle_after_dissolve_drawn_frames",
            "frames",
            check["drawn_frames_delta"].as_u64(),
        ));
        rows.push(counter(
            "idle_after_dissolve_views",
            "views",
            check["views_delta"].as_u64(),
        ));
    }

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
    for (metric, samples) in [
        ("final_input_to_settled_histogram", settled_from_input),
        ("commit_to_settled_histogram", settled_from_commit),
        ("final_input_to_committed_frame", presented_from_input),
        ("commit_to_committed_frame", presented_from_commit),
    ] {
        rows.push(stats::row(metric, "ms", samples));
    }
    rows.extend(resource_rows(&usage, last));

    let mut result = json!({
        "status":"passed",
        "source":source,
        "source_sha256":source_hash,
        "source_dimensions":last["state"]["source_dimensions"],
        "preview_dimensions":last["state"]["preview_dimensions"],
        "backend":last["state"]["backend"],
        "physical_size":last["physical_size"],
        "scale":last["scale"],
        "crop_angle_deg":options.crop,
        "full_basic_layer":options.basic,
        "presence_layer":options.presence,
        "detail_layer":options.detail,
        "lens_layer":options.lens,
        "mode":options.mode.name(),
        "control":options.control.name(),
        "control_action":field.action,
        "control_parameter":field.parameter,
        "field_range":if options.control == Control::Curve { Value::Null } else { json!({"min":field.min,"max":field.max,"step":field.step}) },
        "effect_scope":match (options.control, field.action.as_str(), field.parameter.as_str()) {
            (Control::Curve, action, parameter) => match &field.curve {
                Some(CurveOwner::Module(module)) => format!("{module} {action} {parameter}: point {DRAGGED_POINT} of the curve seeded with {MID_TONE_SEED:?}, a mid-tone point, is dragged through draft.begin/set/commit while the curve canvas is visible; every drafted frame runs the curve's colour unit."),
                _ => "Developer proof curve: identity colour operation. Draft/preview scheduling and GPU upload are timed while the curve canvas is visible; the curve does not alter photo pixels.".to_owned(),
            },
            (Control::Slider, SET_BASIC, EXPOSURE) => "Basic exposure: the photograph's colour pass is measured with the generated slider.".to_owned(),
            (Control::Slider, "set-raw", parameter) => format!("{} {parameter}: the slider is measured through draft.begin/set/commit exactly as Basic exposure is. Each drafted value is previewed approximately on the planes developed at the committed white balance (approximate_white_balance frames, never analysed); each release commits and redevelops the mosaic before its exact frame and histogram.", field.action),
            (Control::Slider, action, parameter) => format!("{action} {parameter}: the slider is measured through draft.begin/set/commit exactly as Basic exposure is."),
        },
        "view_setup":match &field.curve {
            Some(CurveOwner::Proof) => json!({"developer":true,"proof_section":CONTROLS_MODULE,
                "collapsed":["luxforge.basic","luxforge.pixel","luxforge.transform","luxforge.crop"],
                "raw_section":"absent for the JPEG latency source",
                "tools_scroll":1.0}),
            Some(CurveOwner::Module(module)) => json!({"developer":false,"section":module,
                "seed":{"action":field.action,"parameter":field.parameter,"points":MID_TONE_SEED},
                "dragged_point":DRAGGED_POINT,
                "steps":script::write(&curve_view_steps(field, kind, options.mask)),
                "tools_scroll":0.0}),
            None => Value::Null,
        },
        "samples":options.samples,
        "gesture_values":values,
        "method":"Background evidence launch of the release binary, warm filesystem cache. In drag mode one scripted control step per input is left open, so the step settles only when the gesture has drained: every interval is one input, one draft.set and one frame, drawn by the GPU stage from the tick's plan with no preview job or by the CPU from the tick's preview job. In commit mode each step is a whole gesture, moved and released at once, so each sample is one committed frame and its exact histogram. Presented means, for a CPU frame, preview_displayed: the update in which the rendered raster became the photo surface's source, drawn by the redraw that update requests; for a GPU frame, the surface's first draw of the plan tagged with the input's draft revision (surface_frame_drawn), since its pixels exist only once the surface draws them. Drawn means the surface's first draw of the frame on either path, the moment its draw is encoded. Neither is display scanout.",
        "provisional_input_to_frame_target":{"p95_below_ms":16.0,"acceptable_below_ms":32.0,"measured_p95_ms":input_p95,
            "met":input_p95.map(|ms| ms < 16.0),"acceptable":input_p95.map(|ms| ms < 32.0)},
        "rows":rows,
        "press_note":"press_to_first_draft_set and press_to_first_presented_frame start at the gesture's slider_draft_begin, logged in the update that opens its draft, and end at its first draft.set and, in a drag, at the preview_displayed of that set's preview job; they are the only rows that include what a press waits for before its first draft.set. A drag has one press, so each run adds one sample; a commit run has one per sample and no drafted frame survives its commit.",
        "render_and_upload_note":"The photo surface writes the raster into its own texture during the frame that draws it, so there is no upload step to time: render_and_upload covers a CPU frame's render and the hand-over together. A GPU frame's gpu_tick_to_drawn_frame runs from the update that handed its plan to the surface to the draw that evaluated and drew it.",
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
            "scratch":last["state"]["scratch"],
            "rss_samples":usage["rss_samples"],
            "note":"RSS is sampled about every 50 ms by ps and includes captures, GPU resources and allocator retention; it is not a CPU-heap figure. The scratch object is the owner render context's colour budget at the last captured frame, with peak_bytes (the scratch_peak_bytes row) its high-water mark over the whole run.",
        },
        "load":launch::load(load_start),
        "load_average_1m_end":launch::load_average(root),
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
    result["frames"] = json!(
        drained
            .iter()
            .zip(captured)
            .map(|(input, captured)| {
                let mut sample = input.sample();
                sample["captured"] = captured;
                if contended.is_some() {
                    sample["contended"] = json!(in_contention(input));
                }
                sample
            })
            .collect::<Vec<_>>()
    );
    result["gpu_preview"] = json!(!options.gpu_preview_off);
    result["paths"] = paths(drained, &events);
    result["compile_queue_before_gesture"] = compile_queue;
    result["contention"] = contended.map_or(Value::Null, |(_, report)| report);
    result["idle_after_dissolve"] = idle.unwrap_or(Value::Null);
    if options.contend.is_some() {
        // The exports are there to hold the pool, not evidence: what they wrote is in the report.
        let _ = fs::remove_dir_all(&contention);
    }
    result["zoom_percent"] = json!(options.zoom);
    report_geometry(&mut result, options);
    stamp(&mut result, &header);
    write_json(&out.join("latency.json"), &result)?;
    ensure(hash(&source)? == source_hash, "The source changed")?;

    if options.idle {
        hold_and_idle(run, options, &source, last, &usage)?;
    }
    println!("PASS editor latency: {}", out.display());
    Ok(())
}

/// Which path drew a drag's drained inputs, why each CPU tick took the CPU path, and the whole run's
/// ticks by path as the editor logged them.
fn paths(drained: &[Input], events: &[Value]) -> Value {
    let mut reasons = BTreeMap::<String, usize>::new();
    for input in drained.iter().filter(|input| input.path == FramePath::Cpu) {
        *reasons
            .entry(input.reason.clone().unwrap_or_else(|| "none logged".into()))
            .or_default() += 1;
    }
    let ticks = |path: &str| {
        events
            .iter()
            .filter(|event| event["event"] == "gpu_preview_tick" && event["detail"]["path"] == path)
            .count()
    };
    json!({
        "gpu_frames": drained.iter().filter(|input| input.path == FramePath::Gpu).count(),
        "cpu_frames": drained.iter().filter(|input| input.path == FramePath::Cpu).count(),
        "cpu_reasons": reasons,
        "run_gpu_ticks": ticks("gpu"),
        "run_cpu_ticks": ticks("cpu"),
        "note": "gpu_frames and cpu_frames count the drained inputs by the path that drew each one's frame; cpu_reasons is what each CPU tick's gpu_preview_tick named (boundary-pending for the gesture's first tick, which asks for the boundary). run_gpu_ticks and run_cpu_ticks count every tick of the run, its release and burst step's included.",
    })
}

/// Whether an `activity.list` entry is an export job's.
fn is_export(entry: &Value) -> bool {
    entry["kind"] == "export"
}

/// When the step that sent the API call `method` was answered, on the run's clock: the first such
/// step's settle.
fn answered_ms(events: &[Value], method: &str) -> Result<f64> {
    let asked = events
        .iter()
        .position(|event| {
            event["event"] == "script_step" && event["detail"]["request"]["api"]["method"] == method
        })
        .ok_or_else(|| format!("The run logged no {method} step"))?;
    events[asked..]
        .iter()
        .find(|event| event["event"] == "script_step_settled")
        .map(elapsed)
        .ok_or_else(|| format!("The {method} step never settled"))?
}

/// A contended run's contention window on the run's clock, and its report. The export lane runs
/// one job at a time with the rest waiting, and the run queues every export before the first can
/// finish, so the lane is busy without a gap from the first export's acceptance to the last one's
/// end. That end is read back from the `activity.list` the run makes after the release: the answer
/// itself while an export still runs, otherwise the latest finished export's end, `ended_ms_ago`
/// before the answer. The board keeps only work that ran 250 ms or longer, so each export the
/// answer names is listed with its own window, and an export too short to be kept is inside the
/// lane's window all the same.
fn contention_windows(
    events: &[Value],
    app: &Value,
    queued: usize,
) -> Result<(Vec<(f64, f64)>, Value)> {
    let script = app["script"]
        .as_array()
        .ok_or("The run recorded no script")?;
    let method = |record: &Value| record["request"]["api"]["method"].clone();
    let accepted = script
        .iter()
        .filter(|record| method(record) == "export.jpeg" && record["result"]["job_id"].is_string())
        .count();
    ensure(
        accepted == queued,
        format!("Only {accepted} of {queued} contention exports were queued"),
    )?;
    let answer = script
        .iter()
        .find(|record| method(record) == "activity.list")
        .map(|record| &record["result"])
        .ok_or("The contended run recorded no activity.list answer")?;
    let first = answered_ms(events, "export.jpeg")?;
    let answered = answered_ms(events, "activity.list")?;
    let entries = |list: &str| {
        answer[list]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|entry| is_export(entry))
            .collect::<Vec<_>>()
    };
    let running = entries("active");
    let finished: Vec<(f64, f64)> = entries("recent")
        .iter()
        .filter_map(|entry| {
            let end = answered - entry["ended_ms_ago"].as_f64()?;
            Some((end - entry["duration_ms"].as_f64()?, end))
        })
        .collect();
    let end = if running.is_empty() {
        finished
            .iter()
            .map(|(_, end)| *end)
            .reduce(f64::max)
            .ok_or("No export ran long enough for the activity board to keep its end")?
    } else {
        answered
    };
    ensure(
        end > first,
        format!("The exports ended at {end} ms, before the first was accepted at {first} ms"),
    )?;
    let report = json!({
        "exports_queued": queued,
        "lane_busy_ms": [first, end],
        "still_running_at_answer": running.len(),
        "finished_export_windows_ms": finished.iter().map(|(start, end)| json!([start, end])).collect::<Vec<_>>(),
        "activity_answered_ms": answered,
        "activity": answer,
        "note": "Each export renders the committed stack exactly on the shared pool, then encodes and writes it, on the export lane: one job runs and the rest wait, so the lane is busy without a gap from the first export's acceptance to the last one's end. An input is contended when it was sent inside that window, which spans the jobs' encodes and writes as well as their renders. The end comes from activity.list after the release; the exported files are removed after the run.",
    });
    Ok((vec![(first, end)], report))
}

/// The idle check a drag run with `--idle` made after its release: what the editor drew, updated
/// and spent in its window, and the dissolves that ran before it, at least one of which must have
/// — the release's committed frame replacing the drag's last GPU frame.
fn idle_after_dissolve(events: &[Value]) -> Result<Value> {
    let (at, check) = events
        .iter()
        .enumerate()
        .rev()
        .find(|(_, event)| event["event"] == "idle_check")
        .ok_or("The idle step recorded no idle check")?;
    let before = |name: &str| {
        events[..at]
            .iter()
            .filter(|event| event["event"] == name)
            .count()
    };
    let started = before("gpu_dissolve_started");
    ensure(
        started > 0,
        "No dissolve ran before the idle check: the release's frame replaced no GPU frame",
    )?;
    Ok(json!({
        "check": check["detail"],
        "dissolves_started": started,
        "dissolves_ended": before("gpu_dissolve_ended"),
        "dissolves_cancelled": before("gpu_dissolve_cancelled"),
        "method": "In the gesture's own launch, after the release: the Performance section closed, then an idle evidence step with evidence's own ticks and captures suspended, a settle for the dissolve and the committed frame's exact phase, then a window over which the surface's drawn frames, the views built and the process's CPU time are counted. The window's own start counts one frame and one view.",
    }))
}

/// Everything the burst report's own figures come from, computed purely from one run's
/// `events.jsonl`. Pulling this out of [`run_burst`] is what lets it be proven against a synthetic
/// event list rather than only against a real launch.
struct BurstAnalysis {
    /// `slider_step_value` events: every value the paced driver actually handed to the owner,
    /// independent of what the core's own gesture round trip did with it. A coalesced value the
    /// core never turned into a `draft.set` still counts here, as scripted.
    sent_values: usize,
    /// Pan commands sent on the same paced ticks as slider values.
    pan_moves: usize,
    /// `preview_displayed` events, and the surface's first draws of GPU ticks' plans
    /// (`surface_frame_drawn`), from the first `slider_step_value` onward, drafted and committed
    /// alike: the count `presented_fps` divides by the same window's seconds. A frame presented
    /// before the gesture started (the initial open) is not one of these.
    presented_frames: usize,
    /// Of the drafted inputs that reached the screen, how many were drawn on the GPU and how many
    /// by the CPU.
    gpu_frames: usize,
    cpu_frames: usize,
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
    /// `preview_exact_cancelled` events from full-resolution phases a newer request superseded.
    /// Zero is a valid result when this workload leaves no exact phase in flight.
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
    let mut displayed_events: Vec<f64> = events
        .iter()
        .filter(|event| event["event"] == "preview_displayed")
        .map(elapsed)
        .collect::<Result<Vec<_>>>()?;
    // A GPU frame is presented by its draw: the surface's first draw of a GPU tick's plan.
    displayed_events.extend(
        drawn_frames(events)?
            .into_iter()
            .filter(|(_, frame)| frame["path"] == "gpu")
            .map(|(at, _)| at),
    );
    displayed_events.sort_by(f64::total_cmp);
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

    let drawn_on = |path| drafted.iter().filter(|input| input.path == path).count();
    Ok(BurstAnalysis {
        sent_values: value_events.len(),
        pan_moves: counted("slider_step_pan"),
        presented_frames,
        gpu_frames: drawn_on(FramePath::Gpu),
        cpu_frames: drawn_on(FramePath::Cpu),
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
    // The editor's own evidence deadline is 25 s; the burst itself paces BURST_SECONDS of values
    // through real round trips, so this allows generously for both plus the launch wrapper around
    // them.
    let run = Run::tool(root, out, TOOL, bin, Duration::from_secs(60))?;
    run.check(|run| burst(run, options))
}

/// The burst's launch and its report, in `run`.
fn burst(run: &mut Run, options: &Options) -> Result {
    let (root, out) = (&run.root().to_path_buf(), &run.out().to_path_buf());
    let source = options.source.canonicalize()?;
    let source_hash = hash(&source)?;
    let load_start = launch::load_average(root);
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

    let steps = burst_script(options, &field, &values, interval_ms);
    let gesture = gesture_launch(
        out,
        "gesture",
        "gesture-script.json",
        &steps,
        &source,
        false,
    )
    .watch(sampled(root, "gesture"));
    let Launched {
        dir: evidence,
        watched: usage,
    } = run.launch(gesture)?;

    let app = read_json(&evidence.join("result.json"))?;
    ensure(
        app["status"] == "captured",
        "The burst run captured nothing",
    )?;
    let events = scenario::events(&evidence.join("events.jsonl"))?;
    let header = run.provenance(&events)?;
    ensure(
        app["had_input_errors"] == json!(false)
            && app["script"]
                .as_array()
                .is_some_and(|recorded| recorded.len() == steps.len()),
        format!("The burst step failed or never ran: {}", app["script"]),
    )?;
    let frames = app["frames"].as_array().ok_or("Missing frames")?;
    check_curve_layer_seed(frames, &steps, options.curve_layer)?;
    let mut burst_stale_draws = None;
    if options.moving_pan {
        for (index, frame) in frames.iter().enumerate() {
            let blanks = gpu_count(frame, "blank_photo_draws")?;
            burst_stale_draws = Some(gpu_count(frame, "stale_photo_draws")?);
            ensure(
                blanks == 0,
                format!("Moving-pan burst frame {index} followed {blanks} blank photo draws"),
            )?;
        }
    }
    let last = frames.last().ok_or("No frame was captured")?;
    let before_burst = frames
        .get(frames.len().saturating_sub(2))
        .ok_or("No frame was captured before the burst")?;
    check_detail_precondition(before_burst, options.detail)?;
    let gpu_delta = |field: &str| {
        before_burst["state"]["surface"]["gpu"][field]
            .as_u64()
            .zip(last["state"]["surface"]["gpu"][field].as_u64())
            .map(|(before, after)| after.saturating_sub(before))
    };

    let analysis = analyze_burst(&events, &field)?;
    if options.moving_pan {
        ensure(
            analysis.pan_moves == values.len(),
            "A zoomed burst did not pan on every paced slider tick",
        )?;
    }
    let approximate = approximate_frames(&events);
    let region_events: Vec<Value> = events
        .iter()
        .filter(|event| {
            event["event"] == "preview_displayed" && event["detail"]["path"] == "region"
        })
        .map(|event| {
            json!({
                "elapsed_ms":event["elapsed_ms"],
                "generation":event["detail"]["generation"],
                "draft_revision":event["detail"]["draft_revision"],
                "entry_id":event["detail"]["entry_id"],
                "source_fingerprint":event["detail"]["source_fingerprint"],
                "region":event["detail"]["region"],
                "quality":event["detail"]["quality"],
                "viewport_declined":event["detail"]["viewport_declined"],
            })
        })
        .collect();

    let mut result = json!({
        "status":"passed",
        "source":source,
        "source_sha256":source_hash,
        "source_dimensions":last["state"]["source_dimensions"],
        "preview_dimensions":last["state"]["preview_dimensions"],
        "backend":last["state"]["backend"],
        "physical_size":last["physical_size"],
        "scale":last["scale"],
        "crop_angle_deg":options.crop,
        "full_basic_layer":options.basic,
        "detail_layer":options.detail,
        "mode":"burst",
        "control_action":field.action,
        "control_parameter":field.parameter,
        "samples":Value::Null,
        "gesture_values":values,
        "approximate_white_balance_frames":approximate,
        "method":format!("Background evidence launch of the release binary, warm filesystem cache. --samples is ignored: every burst run of a field sends the same fixed {} values over {} s at {} values/s. An interior origin follows a triangle peaking at {} of the smaller half of its declared range (±2 EV on exposure); a limit origin follows one inward triangle to that fraction of the available range and back. Values are paced one per tick of the desktop's own paced slider step rather than sent all at once, so the driver's real coalescing runs on them. Presented means, for a CPU frame, preview_displayed: the update in which the rendered raster became the photo surface's source, drawn by the redraw that update requests; for a GPU frame, the surface's first draw of the tick's plan (surface_frame_drawn). Neither is display scanout.", values.len(), BURST_SECONDS, BURST_RATE_PER_SEC, BURST_PEAK_FRACTION),
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
            "gpu_frames":analysis.gpu_frames,
            "cpu_frames":analysis.cpu_frames,
            "cancelled_exact":analysis.cancelled_exact,
            "cancelled_exact_note":(analysis.cancelled_exact == 0).then_some("No exact phase was cancelled in this run; cancellation depends on timing and workload"),
            "proxy":analysis.proxy,
        },
        "resources":{
            "scratch":last["state"]["scratch"],
            "rss_samples":usage["rss_samples"],
            "note":"RSS is sampled about every 50 ms by ps and includes captures, GPU resources and allocator retention; it is not a CPU-heap figure. The scratch object is the owner render context's colour budget at the last captured frame, with peak_bytes its high-water mark over the whole run.",
        },
        "workspace":last["state"]["workspace"],
        "histogram":last["state"]["histogram"],
        "checks":[
            "Every scripted value reached a captured tick and its own slider_step_value event",
            "Presented frames are counted from preview_displayed and the GPU frames' draws, and drafted staleness is paired with its own slider_draft_set by generation or by GPU draft revision, exactly as drag mode pairs them",
            "Source SHA-256 is unchanged"
        ],
    });
    result["load"] = launch::load(load_start);
    result["load_average_1m_end"] = json!(launch::load_average(root));
    result["zoom_percent"] = json!(options.zoom);
    result["moving_pan"] = json!(options.moving_pan);
    result["burst"]["pan_moves"] = json!(analysis.pan_moves);
    let mut rows = vec![
        stats::scalar("presented_fps", "fps", Some(analysis.presented_fps)),
        stats::row("staleness_ms", "ms", analysis.staleness_ms),
        stats::row("frame_gap_ms", "ms", analysis.frame_gap_ms),
        stats::scalar("max_gap_ms", "ms", Some(analysis.max_gap_ms)),
        counter("draw_encoded_frames", "count", gpu_delta("drawn_frames")),
        counter("photo_texture_writes", "count", gpu_delta("photo_writes")),
        counter("photo_upload_bytes", "bytes", gpu_delta("upload_bytes")),
    ];
    if options.moving_pan {
        rows.push(counter(
            "blank_photo_draws",
            "count",
            Some(gpu_count(last, "blank_photo_draws")?),
        ));
        rows.push(counter("stale_photo_draws", "count", burst_stale_draws));
    }
    rows.extend(resource_rows(&usage, last));
    result["rows"] = json!(rows);
    result["burst"]["regions"] = json!(region_events);
    result["burst"]["surface_gpu"] = last["state"]["surface"]["gpu"].clone();
    result["burst"]["adoption_note"] = json!(
        "presented_frames/presented_fps count preview_displayed adoption events and the first draws of GPU ticks' plans. The surface can adopt several phases before one draw; draw_encoded_frames counts actual photo-surface draw encoding between captured frames, not display scanout."
    );
    report_geometry(&mut result, options);
    stamp(&mut result, &header);
    write_json(&out.join("latency.json"), &result)?;
    ensure(hash(&source)? == source_hash, "The source changed")?;

    println!("PASS editor latency (burst): {}", out.display());
    Ok(())
}

/// Reopen the gesture's own committed catalog, then observe its ordinary idle state. This keeps
/// the selected profile, Perspective, crop and any other committed edits identical to the measured
/// gesture's final stack. A scripted process has exited by then, so an ordinary second launch is
/// needed for the 30-second window.
/// The catalog reopened for idle contains the gesture's final recipe, including edited Detail
/// strengths. It must retain every committed layer's identity, payload, mask and artifact links.
fn idle_catalog_stack(catalog: &Path, source: &Path) -> Result<Value> {
    let service = luxforge_core::EditorService::open(catalog)?;
    let assets = service.assets(None, 1)?;
    ensure(
        assets.assets.len() == 1 && assets.next.is_none(),
        "The idle catalog must contain exactly the measured source",
    )?;
    ensure(
        assets.assets[0].locator.canonicalize()? == source.canonicalize()?,
        "The idle catalog reopened a different source",
    )?;
    let state = service.state(&assets.assets[0].id)?;
    let layers: Vec<_> = state
        .current_entry
        .snapshot
        .recipe
        .layers
        .iter()
        .map(|layer| {
            json!({"id":layer.id,"effect":layer.effect_id,"payload":layer.payload,
            "mask":layer.mask,"artifacts":layer.artifacts})
        })
        .collect();
    Ok(json!({"entry":state.current_entry.id,"revision":state.revision,"layers":layers}))
}

fn check_idle_stack(frame: &Value, reopened: &Value) -> Result {
    let captured = &frame["state"]["stack"];
    ensure(
        captured["entry"].is_string()
            && captured["revision"].is_u64()
            && captured["layers"].is_array()
            && ["entry", "revision", "layers"]
                .into_iter()
                .all(|key| captured[key] == reopened[key]),
        "The idle catalog differs from the gesture's committed entry or recipe",
    )
}

fn hold_and_idle(
    run: &mut Run,
    options: &Options,
    source: &Path,
    frame: &Value,
    hold_usage: &Value,
) -> Result {
    let (root, out) = (&run.root().to_path_buf(), &run.out().to_path_buf());
    let catalog = out.join("app/catalog.sqlite");
    ensure(
        catalog.is_file(),
        "The gesture catalog is missing for idle measurement",
    )?;
    let reopened_stack = idle_catalog_stack(&catalog, source)?;
    check_idle_stack(frame, &reopened_stack)?;
    let load_start = launch::load_average(root);

    // The second process: the same catalog, no script, left idle after its first frame.
    let data = out.join("idle-data");
    let (log, events) = (out.join("idle.log"), data.join("logs/events.jsonl"));
    let idle_root = root.clone();
    let idle = idle_launch(&catalog, &data, source, options.control == Control::Curve)
        .deadline(Duration::from_secs(30))
        .watch(Box::new(move |child, _, deadline| {
            let poll = Poll {
                every: Duration::from_millis(50),
                from: Instant::now(),
                deadline,
                late: "The idle process never adopted a histogram",
            };
            let histogram = || {
                Ok(fs::read_to_string(&events)
                    .unwrap_or_default()
                    .contains("\"event\":\"analysis_adopted\"")
                    .then_some(()))
            };
            if let Watched::Exited(_) = watch(child, poll, histogram, |_| Ok(()))? {
                return Err(format!(
                    "The idle process exited before its histogram: {}",
                    fs::read_to_string(&log).unwrap_or_default()
                )
                .into());
            }
            // One second of settling, then a 30 second window: the same idle window `measure`
            // takes. Its events are counted while it still runs.
            let window = stats::idle_window(&idle_root, child.child.id())?;
            let events = scenario::events(&events)?.len();
            Ok((None, json!({"rows":window.rows(),"events":events})))
        }));
    let idle = run.launch(idle)?.watched;
    // The gesture process's resources, then the idle window's figures, as rows of the one shape.
    // The scratch budget travels in the state snapshot written beside a captured frame, and an
    // ordinary launch captures none, so the idle process cannot report it. The gesture process
    // above does, and it runs the same committed stack.
    let mut rows = resource_rows(hold_usage, frame);
    rows.extend(stats::rows(&idle).iter().cloned());
    write_json(
        &out.join("resources.json"),
        &json!({
            "status":"passed",
            "workload":"The gesture's own committed stack, reopened from its catalog with histogram on",
            "source":source,"source_dimensions":frame["state"]["source_dimensions"],
            "lens_layer":options.lens,"perspective_layer":options.perspective,"crop_angle_deg":options.crop,
            "load":launch::load(load_start),"load_average_1m_end":launch::load_average(root),
            "curve_layer":options.curve_layer,
            "detail_layer":options.detail,"stack":frame["state"]["stack"],
            "reopened_stack":reopened_stack,
            "rows":rows,
            "gesture_process":{
                "scratch":frame["state"]["scratch"],
                "workspace":frame["state"]["workspace"],
                "histogram":frame["state"]["histogram"],
                "controls":frame["state"]["controls"],
            },
            "idle_process":{
                "events":idle["events"],
                "scratch":"not observable: the budget travels in the state snapshot beside a captured frame, and an ordinary launch captures none",
            },
            "method":"The gesture process's own committed catalog is reused unchanged. The second process opens the same source and exact stack, renders and reduces it, then is left alone; CPU is the ps CPU-time delta over 30 seconds after one second of settling. This includes the ordinary editor's open Performance section sampler. The child is then killed, so this is not clean-close evidence. RSS includes GPU resources and allocator retention and is not separated. Native GPU and scratch figures come from the gesture's correlated captured state, not an idle-process capture.",
        }),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pan_refinement_samples_require_their_own_generation_and_identity() {
        let mut events = vec![
            json!({"event":"script_step","elapsed_ms":10,"detail":{"step":1}}),
            json!({"event":"preview_view_requested","elapsed_ms":12,"detail":{"intent":"interactive","generation":7}}),
            json!({"event":"preview_displayed","elapsed_ms":15,"detail":{"generation":99,"path":"region","quality":"interactive","draft_revision":3,"entry_id":"entry","source_fingerprint":"hash"}}),
            json!({"event":"preview_displayed","elapsed_ms":20,"detail":{"generation":7,"path":"region","quality":"interactive","draft_revision":3,"entry_id":"entry","source_fingerprint":"hash"}}),
            json!({"event":"preview_view_requested","elapsed_ms":150,"detail":{"intent":"settle","generation":8}}),
            json!({"event":"preview_displayed","elapsed_ms":180,"detail":{"generation":8,"path":"region","quality":"exact","draft_revision":3,"entry_id":"entry","source_fingerprint":"hash"}}),
            json!({"event":"script_step","elapsed_ms":200,"detail":{"step":4}}),
        ];
        let samples = pan_region_samples(&events, &[(1, 4)]).unwrap();
        assert_eq!(samples[0]["pan_to_interactive_ms"], 10.0);
        assert_eq!(samples[0]["pan_to_exact_ms"], 170.0);
        assert_eq!(samples[0]["refinement_request_to_exact_ms"], 30.0);
        events[5]["detail"]["draft_revision"] = json!(4);
        assert!(pan_region_samples(&events, &[(1, 4)]).is_err());
        events[5]["detail"]["draft_revision"] = json!(3);
        events[5]["detail"]["generation"] = json!(99);
        assert!(pan_region_samples(&events, &[(1, 4)]).is_err());
    }

    #[test]
    fn viewport_combines_raw_observations_instead_of_percentiles() {
        let reports = [
            json!({"rows":[stats::row("pan","ms",[1.0,2.0])]}),
            json!({"rows":[stats::row("pan","ms",[10.0,20.0])]} ),
        ];
        let rows = combined_rows(&reports);
        assert_eq!(rows[0]["distribution"]["count"], 4);
        assert_eq!(rows[0]["distribution"]["p50"], 2.0);
        assert_eq!(rows[0]["distribution"]["p95"], 20.0);
    }

    #[test]
    fn lens_geometry_is_kept_for_crop_opening_paint_and_viewport() {
        let source = PathBuf::from("lens-24mp.jpg");
        let options = Options {
            source: &source,
            samples: 14,
            mode: Mode::CropStart,
            control: Control::Slider,
            action: Some("set-perspective"),
            parameter: Some("horizontal"),
            crop: Some(2.5),
            idle: false,
            basic: false,
            presence: false,
            curve_layer: false,
            detail: false,
            lens: true,
            perspective: true,
            mask: false,
            zoom: Some(100.0),
            moving_pan: false,
            mask_overlay: false,
            contend: None,
            warm_ms: None,
            gpu_preview_off: false,
        };
        let geometry = geometry_preconditions(&options);
        // A JPEG's Lens precondition is the section and its Apply.
        assert_eq!(geometry.len(), 4);
        assert_eq!(
            geometry[..2],
            crate::scenario::recipe::lens_profile(false)[..]
        );
        assert_eq!(
            geometry[2],
            script::Step::call(
                "edit.set-perspective",
                json!({"horizontal":20,"vertical":-10})
            )
        );
        assert_eq!(
            geometry[3],
            script::Step::call("edit.crop-fit", json!({"aspect":"16:9","angle":2.5}))
        );
        let (crop, starts) = crop_start_script(&options);
        let field = FieldTarget::lookup("set-perspective", "horizontal").unwrap();
        let (viewport, _) = viewport_script(&options, &field);
        for steps in [crop, paint_script(&options, paint_path(30)), viewport] {
            assert_eq!(&steps[..geometry.len()], geometry.as_slice());
            assert!(steps.len() <= script::MAX_SCRIPT_STEPS);
            assert_eq!(
                script::parse(&script::write(&steps).to_string()).unwrap(),
                steps
            );
        }
        assert_eq!(starts.len(), 14);
    }

    #[test]
    fn idle_reopens_the_gesture_catalog_with_its_provider_registry() {
        let out = Path::new("/out");
        let catalog = out.join("app/catalog.sqlite");
        let args = idle_launch(
            &catalog,
            &out.join("idle-data"),
            Path::new("/photo.raw"),
            true,
        )
        .command(out);
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--catalog", "/out/app/catalog.sqlite"])
        );
        assert!(args.iter().any(|arg| arg == "--developer"));
        assert!(!args.iter().any(|arg| arg == "--evidence-script"));
    }

    #[test]
    fn detail_idle_accepts_the_edited_payload_but_requires_the_same_committed_stack() {
        let edited = json!({"id":"detail-layer","effect":luxforge_core::DETAIL_EFFECT,
            "payload":{"sharpening":35.0,"luminance":40.0,"colour":40.0},
            "mask":null,"artifacts":[]});
        let stack = json!({"entry":"edited-entry","revision":3,"layers":[edited]});
        let frame = json!({"state":{"stack":stack}});
        assert!(
            check_detail_precondition(&frame, true).is_err(),
            "The seed is intentionally no longer moderate"
        );
        assert!(check_idle_stack(&frame, &stack).is_ok());
        let mut mismatched = stack.clone();
        mismatched["layers"][0]["payload"]["sharpening"] = json!(60.0);
        assert!(check_idle_stack(&frame, &mismatched).is_err());
        mismatched = stack.clone();
        mismatched["layers"][0]["id"] = json!("replaced-detail");
        assert!(check_idle_stack(&frame, &mismatched).is_err());
        mismatched = stack.clone();
        mismatched["entry"] = json!("old-entry");
        assert!(check_idle_stack(&frame, &mismatched).is_err());
        mismatched = stack.clone();
        mismatched["revision"] = json!(2);
        assert!(check_idle_stack(&frame, &mismatched).is_err());
    }

    #[test]
    fn native_resource_rows_keep_gpu_residency_and_missing_counters_explicit() {
        let last = json!({"state":{"scratch":{"peak_bytes":123},"surface":{"gpu":{"full_resident_bytes":456,"region_resident_bytes":78,"retiring_bytes":90,"stage_resident_bytes":12}}}});
        let report = json!({"rows":resource_rows(&Value::Null,&last)});
        assert_eq!(
            stats::distribution(&report, "last_surface_full_resident_bytes").unwrap()["p50"],
            456.0
        );
        assert!(stats::distribution(&report, "last_native_gpu_allocated").is_none());
    }

    /// Every script this harness can write, into `$SCRIPT_DUMP/editor-latency/`, over each mode,
    /// control, precondition, zoom and `--moving-pan`, and every launch's argument list: the proof
    /// that a change to how scripts are written or launches are made leaves the scripts these
    /// measurements run and the arguments they pass the same.
    #[test]
    #[ignore]
    fn dump_scripts() {
        let dir =
            PathBuf::from(std::env::var("SCRIPT_DUMP").expect("SCRIPT_DUMP names a directory"))
                .join("editor-latency");
        fs::create_dir_all(&dir).unwrap();
        let put = |name: String, steps: Value| {
            write_json(&dir.join(format!("{name}.json")), &steps).unwrap();
        };
        let source = PathBuf::from("/photo.jpg");
        let mixer = || FieldTarget {
            action: "set-mixer".into(),
            parameter: "red-hue".into(),
            min: -100.0,
            max: 100.0,
            step: 1.0,
            origin: 0.0,
            curve: None,
        };
        for crop in [None, Some(8.0)] {
            for mask in [false, true] {
                for basic in [false, true] {
                    for zoom in [None, Some(200.0)] {
                        let options = |control, zoom, moving_pan| Options {
                            source: &source,
                            samples: 5,
                            mode: Mode::Drag,
                            control,
                            action: None,
                            parameter: None,
                            crop,
                            idle: false,
                            basic,
                            presence: false,
                            curve_layer: false,
                            detail: false,
                            lens: false,
                            perspective: false,
                            mask,
                            zoom,
                            moving_pan,
                            mask_overlay: false,
                            contend: None,
                            warm_ms: None,
                            gpu_preview_off: false,
                        };
                        let mut tag = format!("crop{}-mask{mask}-basic{basic}", crop.is_some());
                        if let Some(zoom) = zoom {
                            tag.push_str(&format!("-zoom{zoom}"));
                        }
                        for (control, name, field) in [
                            (Control::Slider, "slider", FieldTarget::basic_exposure()),
                            (Control::Slider, "mixer", mixer()),
                            (Control::Curve, "curve", FieldTarget::proof_curve()),
                            (Control::Curve, "tone-curve", tone_curve()),
                        ] {
                            for drag in [true, false] {
                                let values = gesture_values(5 + usize::from(drag), control, &field);
                                put(
                                    format!(
                                        "{name}-{}-{tag}",
                                        if drag { "drag" } else { "commit" }
                                    ),
                                    script::write(&gesture_script(
                                        &options(control, zoom, false),
                                        &field,
                                        SourceTag::Jpeg,
                                        &values,
                                        drag,
                                    )),
                                );
                            }
                            if control == Control::Slider {
                                // `--moving-pan` needs a zoom, so it is written only beside one.
                                let pans: &[bool] = if zoom.is_some() {
                                    &[false, true]
                                } else {
                                    &[false]
                                };
                                for &moving_pan in pans {
                                    let values = field.burst_values();
                                    put(
                                        format!(
                                            "{name}-burst-{tag}{}",
                                            if moving_pan { "-movingpan" } else { "" }
                                        ),
                                        script::write(&burst_script(
                                            &options(control, zoom, moving_pan),
                                            &field,
                                            &values,
                                            burst_interval_ms(),
                                        )),
                                    );
                                }
                            }
                        }
                        put(
                            format!("paint-{tag}"),
                            script::write(&paint_script(
                                &options(Control::Slider, zoom, false),
                                paint_path(6),
                            )),
                        );
                    }
                    // The viewport journey runs only at 100 or 200 percent.
                    for zoom in [100.0, 200.0] {
                        let options = Options {
                            source: &source,
                            samples: 5,
                            mode: Mode::Viewport,
                            control: Control::Slider,
                            action: None,
                            parameter: None,
                            crop,
                            idle: false,
                            basic,
                            presence: false,
                            curve_layer: false,
                            detail: false,
                            lens: false,
                            perspective: false,
                            mask,
                            zoom: Some(zoom),
                            moving_pan: false,
                            mask_overlay: false,
                            contend: None,
                            warm_ms: None,
                            gpu_preview_off: false,
                        };
                        let tag =
                            format!("crop{}-mask{mask}-basic{basic}-zoom{zoom}", crop.is_some());
                        for (name, field) in [
                            ("slider", FieldTarget::basic_exposure()),
                            ("mixer", mixer()),
                        ] {
                            let (steps, positions) = viewport_script(&options, &field);
                            put(format!("{name}-viewport-{tag}"), script::write(&steps));
                            put(format!("{name}-viewport-{tag}-positions"), json!(positions));
                        }
                    }
                }
            }
        }
        // Every launch's arguments, as it passes them in a run written to `/out`.
        let out = Path::new("/out");
        for (name, log, file, developer) in [
            ("gesture", "gesture", "gesture-script.json", false),
            ("curve", "gesture", "gesture-script.json", true),
            ("viewport", "viewport", "viewport-script.json", false),
        ] {
            let launch = gesture_launch(out, log, file, &[], &source, developer);
            put(format!("{name}-arguments"), json!(launch.command(out)));
        }
        let catalog = out.join("app/catalog.sqlite");
        put(
            "idle-arguments".into(),
            json!(idle_launch(&catalog, &out.join("idle-data"), &source, false).command(out)),
        );
    }

    /// The default Basic exposure target: the field the synthetic burst events carry.
    fn unused_field() -> FieldTarget {
        FieldTarget::basic_exposure()
    }

    #[test]
    fn detail_measurement_preconditions_keep_paint_global() {
        let source = PathBuf::from("unused.jpg");
        let mut options = Options {
            source: &source,
            samples: 30,
            mode: Mode::Paint,
            control: Control::Slider,
            action: None,
            parameter: None,
            crop: None,
            idle: true,
            basic: false,
            presence: false,
            curve_layer: false,
            detail: true,
            lens: false,
            perspective: false,
            mask: false,
            mask_overlay: true,
            contend: None,
            warm_ms: None,
            gpu_preview_off: false,
            zoom: None,
            moving_pan: false,
        };
        let detail = crate::scenario::recipe::moderate_detail();
        let paint = paint_precondition(&options);
        assert_eq!(
            paint.first(),
            Some(&detail),
            "Detail must be committed before mask selection changes the target"
        );
        assert!(
            paint
                .iter()
                .skip(1)
                .any(|s| matches!(s, script::Step::Mask(MaskStep::Paint(PaintStep::NewMask))))
        );
        options.detail = false;
        assert!(!paint_precondition(&options).contains(&detail));
    }

    #[test]
    fn detail_burst_precondition_preserves_the_basic_gesture_and_other_setup() {
        let source = PathBuf::from("unused.jpg");
        let mut options = Options {
            source: &source,
            samples: 30,
            mode: Mode::Burst,
            control: Control::Slider,
            action: None,
            parameter: None,
            crop: Some(2.0),
            idle: false,
            basic: true,
            presence: false,
            curve_layer: false,
            detail: false,
            lens: false,
            perspective: false,
            mask: true,
            mask_overlay: false,
            contend: None,
            warm_ms: None,
            gpu_preview_off: false,
            zoom: Some(100.0),
            moving_pan: true,
        };
        let field = FieldTarget::basic_exposure();
        let values = field.burst_values();
        let interval = burst_interval_ms();
        let baseline = burst_script(&options, &field, &values, interval);
        let detail = crate::scenario::recipe::moderate_detail();
        assert!(!baseline.contains(&detail));
        assert_eq!(
            baseline.last(),
            Some(&burst_gesture_step(&field, &burst_values(), interval, true))
        );
        assert!(baseline.contains(&basic_precondition(&options)));
        assert!(baseline.contains(&script::Step::View(ViewStep::Percent(100.0))));
        for step in crop_precondition(&options)
            .into_iter()
            .chain(mask_precondition())
        {
            assert!(baseline.contains(&step));
        }

        options.detail = true;
        let with_detail = burst_script(&options, &field, &values, interval);
        assert_eq!(
            with_detail.iter().filter(|step| **step == detail).count(),
            1
        );
        let detail_index = with_detail.iter().position(|step| *step == detail).unwrap();
        let mask_index = with_detail
            .iter()
            .position(|step| *step == mask_precondition()[0])
            .unwrap();
        assert!(detail_index < mask_index, "Detail is a global precondition");
        assert_eq!(
            with_detail
                .into_iter()
                .filter(|step| *step != detail)
                .collect::<Vec<_>>(),
            baseline,
            "--detail must only add its shared recipe precondition"
        );
    }

    #[test]
    fn combined_viewport_keeps_global_detail_before_the_masked_basic_target() {
        let source = PathBuf::from("lens-24mp.jpg");
        let options = Options {
            source: &source,
            samples: 5,
            mode: Mode::Viewport,
            control: Control::Slider,
            action: None,
            parameter: None,
            crop: Some(2.5),
            idle: false,
            basic: true,
            presence: false,
            curve_layer: false,
            detail: true,
            lens: true,
            perspective: true,
            mask: true,
            zoom: Some(100.0),
            moving_pan: false,
            mask_overlay: false,
            contend: None,
            warm_ms: None,
            gpu_preview_off: false,
        };
        let field = FieldTarget::basic_exposure();
        let (steps, positions) = viewport_script(&options, &field);
        let geometry = geometry_preconditions(&options);
        let detail = crate::scenario::recipe::moderate_detail();
        assert_eq!(&steps[..geometry.len()], geometry.as_slice());
        assert_eq!(steps[geometry.len()], detail);
        assert_eq!(steps[geometry.len() + 1], mask_precondition()[0]);
        assert!(steps.contains(&basic_precondition(&options)));
        assert_eq!(steps.iter().filter(|step| **step == detail).count(), 1);
        assert!(positions.into_iter().all(|index| index <= steps.len()));
        assert_eq!(
            script::parse(&script::write(&steps).to_string()).unwrap(),
            steps
        );
    }

    #[test]
    fn detail_measurement_refuses_missing_masked_or_reordered_restoration() {
        let detail = json!({"effect":luxforge_core::DETAIL_EFFECT,"mask":null,
            "payload":{"sharpening":60.0,"luminance":40.0,"colour":40.0}});
        let basic = json!({"effect":luxforge_core::BASIC_EFFECT,"payload":{"exposure":0.5}});
        let frame = |layers: Value| json!({"state":{"stack":{"layers":layers}}});
        assert!(check_detail_precondition(&frame(json!([detail, basic])), true).is_ok());
        assert!(check_detail_precondition(&frame(json!([basic])), true).is_err());
        assert!(check_detail_precondition(&frame(json!([basic, detail])), true).is_err());
        assert!(check_detail_precondition(&frame(json!([detail, basic])), false).is_err());
        let mut masked = detail;
        masked["mask"] = json!("mask-1");
        assert!(check_detail_precondition(&frame(json!([masked, basic])), true).is_err());
    }

    #[test]
    fn lens_latency_precondition_applies_the_detected_profile_before_the_gesture() {
        let source = PathBuf::from("lens-24mp.jpg");
        let options = Options {
            source: &source,
            samples: 30,
            mode: Mode::Drag,
            control: Control::Slider,
            action: Some("set-perspective"),
            parameter: Some("horizontal"),
            crop: Some(2.5),
            idle: false,
            basic: false,
            presence: false,
            curve_layer: false,
            detail: false,
            lens: true,
            perspective: false,
            mask: false,
            zoom: None,
            moving_pan: false,
            mask_overlay: false,
            contend: None,
            warm_ms: None,
            gpu_preview_off: false,
        };
        let field = FieldTarget::lookup("set-perspective", "horizontal").unwrap();
        let values = field.gesture_values(30);
        let steps = gesture_script(&options, &field, SourceTag::Jpeg, &values, true);
        assert_eq!(
            &steps[..2],
            &crate::scenario::recipe::lens_profile(false)[..]
        );
        assert!(
            matches!(&steps[1],script::Step::Controls(script::ControlsStep::QueryChoiceApply {action}) if action=="select-lens-profile")
        );
        assert_eq!(
            script::parse(&script::write(&steps).to_string()).unwrap(),
            steps
        );
        assert!(steps.len() <= script::MAX_SCRIPT_STEPS);
        // A RAW is imported with its detected profile applied: with Lens the script only opens
        // the section, and its baseline turns the profile off.
        let raw = luxforge_testbase::paths::temp_path("lens-latency.nef");
        fs::write(&raw, [0x49, 0x49, 0x2a, 0x00]).unwrap();
        let with = Options {
            source: &raw,
            ..options
        };
        let steps = geometry_preconditions(&with);
        assert_eq!(steps[..1], crate::scenario::recipe::lens_profile(true)[..]);
        assert!(!steps.iter().any(|step| matches!(
            step,
            script::Step::Controls(script::ControlsStep::QueryChoiceApply { .. })
        )));
        let without = Options {
            lens: false,
            ..with
        };
        assert_eq!(
            geometry_preconditions(&without)[0],
            crate::scenario::recipe::lens_off()
        );
        fs::remove_file(raw).unwrap();
    }

    /// `--control curve --action set-curve --parameter luminance`: the Tone curve's own field.
    fn tone_curve() -> FieldTarget {
        resolve_field(Control::Curve, Some("set-curve"), Some("luminance")).unwrap()
    }

    /// A drag or commit run's options over a JPEG with the given control and preconditions.
    fn curve_options(source: &Path, mask: bool, crop: Option<f64>) -> Options<'_> {
        Options {
            source,
            samples: 5,
            mode: Mode::Drag,
            control: Control::Curve,
            action: Some("set-curve"),
            parameter: Some("luminance"),
            curve_layer: false,
            detail: false,
            lens: false,
            perspective: false,
            crop,
            idle: false,
            basic: false,
            presence: false,
            mask,
            zoom: None,
            moving_pan: false,
            mask_overlay: false,
            contend: None,
            warm_ms: None,
            gpu_preview_off: false,
        }
    }

    #[test]
    fn real_curve_precondition_keeps_numeric_gestures_and_precedes_mask_selection() {
        let source = PathBuf::from("unused.jpg");
        let options = Options {
            control: Control::Slider,
            action: None,
            parameter: None,
            curve_layer: true,
            detail: true,
            basic: true,
            lens: true,
            perspective: true,
            zoom: Some(100.0),
            ..curve_options(&source, true, Some(2.5))
        };
        let field = resolve_field(options.control, options.action, options.parameter).unwrap();
        assert_eq!(field.action, SET_BASIC);
        assert!(
            field.curve.is_none(),
            "A real Curve layer does not select the proof curve"
        );
        let geometry = geometry_preconditions(&options);
        let curve = crate::scenario::recipe::moderate_curve();
        let numeric = [
            setup_steps(&options, &field, SourceTag::Jpeg),
            burst_script(&options, &field, &field.burst_values(), burst_interval_ms()),
            viewport_script(&options, &field).0,
        ];
        for steps in numeric {
            assert_eq!(&steps[..geometry.len()], geometry.as_slice());
            assert_eq!(steps[geometry.len()], curve);
            let mask = steps
                .iter()
                .position(|step| *step == mask_precondition()[0])
                .unwrap();
            assert!(geometry.len() < mask);
            assert_eq!(steps.iter().filter(|step| **step == curve).count(), 1);
            assert!(steps.contains(&crate::scenario::recipe::moderate_detail()));
            assert!(
                !steps
                    .iter()
                    .any(|step| matches!(step, script::Step::Curve(_)))
            );
            assert_eq!(
                script::parse(&script::write(&steps).to_string()).unwrap(),
                steps
            );
        }
        for steps in [
            paint_script(&options, paint_path(6)),
            hover_script(&options).unwrap().0,
            crop_start_script(&options).0,
        ] {
            assert_eq!(steps.iter().filter(|step| **step == curve).count(), 1);
            assert_eq!(
                script::parse(&script::write(&steps).to_string()).unwrap(),
                steps
            );
        }
    }

    #[test]
    fn curve_precondition_capture_refuses_identity_missing_or_masked_layers() {
        let steps = [crate::scenario::recipe::moderate_curve()];
        let mut frames = [
            json!({}),
            json!({"state":{"stack":{"layers":[{
                "id":"curve-layer","effect":luxforge_core::CURVE_EFFECT,"mask":null,
                "payload":{"luminance":crate::scenario::recipe::MODERATE_CURVE},"artifacts":[]
            }]}}}),
        ];
        assert!(check_curve_layer_seed(&frames, &steps, true).is_ok());
        frames[1]["state"]["stack"]["layers"][0]["mask"] = json!("masked");
        assert!(check_curve_layer_seed(&frames, &steps, true).is_err());
        frames[1]["state"]["stack"]["layers"][0]["mask"] = Value::Null;
        frames[1]["state"]["stack"]["layers"][0]["payload"]["luminance"] = json!([[0, 0], [1, 1]]);
        assert!(check_curve_layer_seed(&frames, &steps, true).is_err());
        assert!(check_curve_layer_seed(&frames, &[], true).is_err());
        assert!(check_curve_layer_seed(&[], &[], false).is_ok());
    }

    /// The curve steps of a script, as `(action, parameter, index, points)`.
    fn curve_moves(steps: &[script::Step]) -> Vec<(String, String, usize, Vec<[f32; 2]>)> {
        steps
            .iter()
            .filter_map(|step| match step {
                script::Step::Curve(CurveStep {
                    action,
                    parameter,
                    event: CurveStepEvent::Move { index, points },
                    ..
                }) => Some((action.clone(), parameter.clone(), *index, points.clone())),
                _ => None,
            })
            .collect()
    }

    /// `--control curve --action set-curve --parameter luminance` resolves the Tone curve's
    /// registered field: its own action and parameter, owned by `luxforge.curve`, over the curve's
    /// `0..=1` coordinates, and the launch is an ordinary one.
    #[test]
    fn a_curve_action_and_parameter_resolve_a_module_curve() {
        let field = tone_curve();
        assert_eq!(field.action, "set-curve");
        assert_eq!(field.parameter, "luminance");
        assert_eq!(
            field.curve,
            Some(CurveOwner::Module("luxforge.curve".into()))
        );
        assert_eq!((field.min, field.max), (0.0, 1.0));
        // The seed is admissible for the declared parameter: the registry's own check accepts it.
        let registry = ModuleRegistry::builtin();
        let (_, action) = registry.action("set-curve").unwrap();
        let declared = action.parameter("luminance").unwrap();
        assert!(luxforge_core::check_value(declared, &json!(MID_TONE_SEED)).is_ok());

        // Its drafts pair by the module's own parameter, exactly as the proof curve's do by master.
        let points = json!([[0.0, 0.0], [0.5, 0.625], [1.0, 1.0]]);
        let events = vec![
            json!({"event":"slider_draft_set","elapsed_ms":10.0,
                "detail":{"fields":{"luminance":points}}}),
            json!({"event":"slider_draft_preview","elapsed_ms":20.0,
                "detail":{"value":points,"generation":4,"draft_revision":1}}),
            json!({"event":"preview_displayed","elapsed_ms":31.0,
                "detail":{"generation":4,"draft_revision":1}}),
        ];
        let paired = inputs(&events, Control::Curve, &field).unwrap();
        assert_eq!(paired.len(), 1);
        assert_eq!(paired[0].value, 0.625);
        assert_eq!(paired[0].displayed_ms - paired[0].sent_ms, 21.0);

        // Neither half of the pair alone resolves anything.
        assert!(resolve_field(Control::Curve, Some("set-curve"), None).is_err());
        assert!(resolve_field(Control::Curve, None, Some("luminance")).is_err());
    }

    /// A number parameter with `--control curve`, and a curve parameter without it, are refused
    /// with messages naming the parameter; so are an unknown action, a missing parameter and the
    /// developer proof's action, which the curve control reaches only without `--action`.
    #[test]
    fn a_number_parameter_is_refused_with_control_curve() {
        let message = |control, action, parameter| {
            resolve_field(control, Some(action), Some(parameter))
                .err()
                .unwrap_or_else(|| panic!("{action} {parameter} resolved"))
                .to_string()
        };
        let number = message(Control::Curve, "set-mixer", "red-hue");
        assert!(number.contains("red-hue"), "{number}");
        assert!(number.contains("not a curve"), "{number}");
        let exposure = message(Control::Curve, "set-basic", "exposure");
        assert!(exposure.contains("exposure"), "{exposure}");
        let curve = message(Control::Slider, "set-curve", "luminance");
        assert!(curve.contains("luminance"), "{curve}");
        assert!(curve.contains("--control curve"), "{curve}");
        let missing = message(Control::Curve, "set-curve", "red");
        assert!(missing.contains("red"), "{missing}");
        assert!(message(Control::Curve, "edit.nothing", "luminance").contains("edit.nothing"));
        // The proof's action is not a product module's: without --action is how it is measured.
        assert!(message(Control::Curve, SET_CONTROLS, MASTER).contains(SET_CONTROLS));
        assert!(message(Control::Curve, "reset-curve", "luminance").contains("reset-curve"));
    }

    /// Without `--action`, `--control curve` keeps the developer proof curve: the same target, the
    /// same view steps and the same gesture through `set-controls` `master`, with nothing seeded.
    #[test]
    fn without_an_action_the_curve_control_keeps_the_proof_curve() {
        let field = resolve_field(Control::Curve, None, None).unwrap();
        assert_eq!(field.action, SET_CONTROLS);
        assert_eq!(field.parameter, MASTER);
        assert_eq!(field.curve, Some(CurveOwner::Proof));
        let source = PathBuf::from("unused.jpg");
        let options = Options {
            action: None,
            parameter: None,
            ..curve_options(&source, false, Some(8.0))
        };
        let values = gesture_values(6, Control::Curve, &field);
        let steps = gesture_script(&options, &field, SourceTag::Jpeg, &values, true);
        let mut expected = vec![script::Step::call(
            "edit.crop-fit",
            json!({"aspect":"16:9","angle":8.0}),
        )];
        expected.extend(
            [
                "luxforge.basic",
                "luxforge.pixel",
                "luxforge.transform",
                "luxforge.crop",
            ]
            .map(|module| script::Step::section(module, false)),
        );
        expected.push(script::Step::section(CONTROLS_MODULE, true));
        expected.push(script::Step::tools_scroll(1.0));
        assert_eq!(steps[..expected.len()], expected[..]);
        assert_eq!(setup_steps(&options, &field, SourceTag::Jpeg), expected);
        assert!(
            !steps
                .iter()
                .any(|step| matches!(step, script::Step::Api { method, .. } if method.starts_with("edit.set-"))),
            "the proof curve is seeded"
        );
        let moves = curve_moves(&steps);
        assert_eq!(moves.len(), values.len() + 1);
        for (action, parameter, index, points) in moves {
            assert_eq!(
                (action.as_str(), parameter.as_str(), index),
                (SET_CONTROLS, MASTER, 1)
            );
            assert!(points.iter().all(|point| point[0] == 0.5));
        }
    }

    /// A module's curve commits [`MID_TONE_SEED`] through its own action after every other
    /// precondition and before the first sample, so point 1, the one the gesture drags, is a
    /// mid-tone point rather than the white point; a masked run seeds the masked layer it drafts.
    #[test]
    fn a_module_curve_seeds_a_mid_tone_point_before_the_drag() {
        let field = tone_curve();
        let source = PathBuf::from("unused.jpg");
        for mask in [false, true] {
            let options = curve_options(&source, mask, None);
            for drag in [true, false] {
                let values = gesture_values(6, Control::Curve, &field);
                let steps = gesture_script(&options, &field, SourceTag::Jpeg, &values, drag);
                let seeds: Vec<usize> = steps
                    .iter()
                    .enumerate()
                    .filter(|(_, step)| {
                        matches!(step, script::Step::Api { method, .. } if method == "edit.set-curve")
                    })
                    .map(|(index, _)| index)
                    .collect();
                assert_eq!(seeds.len(), 1, "one seed commit");
                let seed = seeds[0];
                let mut params = json!({"luminance": [[0.0, 0.0], [0.5, 0.5], [1.0, 1.0]]});
                if mask {
                    params["mask"] = json!({"name": "Mask 1"});
                    // The mask exists, and is selected, before the seed names it.
                    assert_eq!(steps[..3], mask_precondition()[..]);
                    assert!(seed >= 3);
                }
                assert_eq!(steps[seed], script::Step::call("edit.set-curve", params));
                let first_move = steps
                    .iter()
                    .position(|step| matches!(step, script::Step::Curve(_)))
                    .unwrap();
                assert!(seed < first_move, "the seed precedes the first sample");
                assert_eq!(
                    first_move,
                    setup_steps(&options, &field, SourceTag::Jpeg).len(),
                    "the readiness frame follows the seed and the view steps"
                );
                // Point 1 sits at x 0.5 in the seed, and every move drags that point there.
                assert_eq!(MID_TONE_SEED[DRAGGED_POINT], [0.5, 0.5]);
                let moves = curve_moves(&steps);
                assert_eq!(moves.len(), values.len() + usize::from(drag));
                for (action, parameter, index, points) in moves {
                    assert_eq!(
                        (action.as_str(), parameter.as_str(), index),
                        ("set-curve", "luminance", DRAGGED_POINT)
                    );
                    assert!(
                        points
                            .iter()
                            .all(|point| point[0] == DRAGGED_X && (0.0..=1.0).contains(&point[1]))
                    );
                }
                assert!(steps.len() <= script::MAX_SCRIPT_STEPS);
            }
        }
        // The largest run the harness takes still fits the evidence script.
        let options = Options {
            samples: 32,
            basic: true,
            presence: true,
            ..curve_options(&source, true, Some(8.0))
        };
        let values = gesture_values(33, Control::Curve, &field);
        let steps = gesture_script(&options, &field, SourceTag::Raw, &values, true);
        assert!(steps.len() <= script::MAX_SCRIPT_STEPS, "{}", steps.len());
    }

    /// The view steps collapse every section the panel lists above the module's, as it lists them
    /// for the photo's kind and workspace, expand the module's and scroll the panel to its top.
    #[test]
    fn the_view_steps_collapse_the_sections_above_the_module_and_expand_it() {
        let field = tone_curve();
        let collapsed = |modules: &[&str]| {
            let mut steps: Vec<script::Step> = modules
                .iter()
                .map(|module| script::Step::section(*module, false))
                .collect();
            steps.push(script::Step::section("luxforge.curve", true));
            steps.push(script::Step::tools_scroll(0.0));
            steps
        };
        // The Crop, Presets and Basic sections sit above the Tone curve. The RAW module, between them in
        // the registry, declares no controls and so draws no section for either kind of photo.
        assert_eq!(
            curve_view_steps(&field, SourceTag::Jpeg, false),
            collapsed(&["luxforge.crop", "luxforge.presets", "luxforge.basic"])
        );
        assert_eq!(
            curve_view_steps(&field, SourceTag::Raw, false),
            collapsed(&["luxforge.crop", "luxforge.presets", "luxforge.basic"])
        );
        // The mask workspace lists only modules with a maskable effect.
        assert_eq!(
            curve_view_steps(&field, SourceTag::Jpeg, true),
            collapsed(&["luxforge.basic"])
        );
        // Every collapsed section is one the registry orders before the module, and the panel
        // lists it: no developer section, and nothing below the module is touched.
        let registry = ModuleRegistry::builtin();
        let order: Vec<&str> = registry
            .descriptors()
            .into_iter()
            .map(|module| module.id.as_str())
            .collect();
        let curve = order.iter().position(|id| *id == "luxforge.curve").unwrap();
        for step in curve_view_steps(&field, SourceTag::Raw, false) {
            if let script::Step::Section(section) = step {
                let position = order.iter().position(|id| *id == section.module).unwrap();
                assert_eq!(section.expanded, position == curve, "{}", section.module);
                assert!(position <= curve, "{} is below the curve", section.module);
            }
        }
        // A slider has no view steps.
        assert!(
            curve_view_steps(&FieldTarget::basic_exposure(), SourceTag::Jpeg, false).is_empty()
        );
    }

    /// A JPEG is told from a RAW file by its start-of-image marker.
    #[test]
    fn the_source_kind_is_read_from_the_first_bytes() {
        let dir =
            std::env::temp_dir().join(format!("luxforge-latency-kind-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let jpeg = dir.join("a.raw-named-jpeg");
        fs::write(&jpeg, [0xFF, 0xD8, 0xFF, 0xE0]).unwrap();
        let raw = dir.join("a.jpg");
        fs::write(&raw, b"II*\0").unwrap();
        assert_eq!(source_tag(&jpeg).unwrap(), SourceTag::Jpeg);
        assert_eq!(source_tag(&raw).unwrap(), SourceTag::Raw);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// A crop-start script at its largest sample count fits the evidence script, and the step
    /// numbers it reports are its Starts, each followed by its hold, its Cancel and a settle.
    #[test]
    fn a_crop_start_script_numbers_its_starts_and_fits_the_script_limit() {
        let source = PathBuf::from("unused.jpg");
        let options = Options {
            source: &source,
            samples: 14,
            mode: Mode::CropStart,
            control: Control::Slider,
            action: None,
            parameter: None,
            crop: None,
            idle: false,
            basic: true,
            presence: true,
            curve_layer: false,
            detail: false,
            lens: false,
            perspective: false,
            mask: false,
            mask_overlay: false,
            contend: None,
            warm_ms: None,
            gpu_preview_off: false,
            zoom: None,
            moving_pan: false,
        };
        let (steps, starts) = crop_start_script(&options);
        assert!(steps.len() <= script::MAX_SCRIPT_STEPS, "{}", steps.len());
        assert_eq!(starts.len(), 14);
        for start in starts {
            assert_eq!(steps[start - 1], script::Step::Draft(DraftStep::Start));
            assert_eq!(steps[start], script::Step::wait(CROP_START_HOLD_MS));
            assert_eq!(steps[start + 1], script::Step::Draft(DraftStep::Cancel));
        }
        assert_eq!(
            script::parse(&script::write(&steps).to_string()).as_deref(),
            Ok(steps.as_slice())
        );
    }

    #[test]
    fn input_latency_uses_first_region_of_a_refined_generation() {
        let events = vec![
            json!({"event":"slider_draft_set","elapsed_ms":10.0,"detail":{"fields":{"exposure":0.5}}}),
            json!({"event":"slider_draft_preview","elapsed_ms":13.0,"detail":{
                "value":0.5,"generation":7,"draft_revision":2
            }}),
            json!({"event":"preview_displayed","elapsed_ms":28.0,"detail":{
                "generation":7,"draft_revision":2,"path":"region","quality":"interactive"
            }}),
            json!({"event":"preview_displayed","elapsed_ms":60.0,"detail":{
                "generation":7,"draft_revision":2,"path":"region","quality":"exact"
            }}),
        ];
        let paired = inputs(&events, Control::Slider, &unused_field()).unwrap();
        assert_eq!(paired.len(), 1);
        assert_eq!(paired[0].displayed_ms - paired[0].sent_ms, 18.0);
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

    #[test]
    fn paint_phase_samples_pair_region_adoption_with_its_region_worker_result() {
        let events = vec![
            json!({"event":"script_step","elapsed_ms":9.0,"detail":{"request":{"mask":{"stroke":{
                "interval_ms":24,"points":[[0.2,0.5]]
            }}}}}),
            json!({"event":"mask_draft_set","elapsed_ms":10.0}),
            json!({"event":"mask_draft_preview","elapsed_ms":11.0,"detail":{
                "generation":7,
                "round_trip_ms":{"executor_wait":0.0,"draft_set":0.5,"preview_job":0.25,"return_to_queue":0.25}
            }}),
            json!({"event":"preview_result_received","elapsed_ms":20.0,"detail":{
                "generation":7,"phase":"region","queue_wait_ms":2.0,"render_ms":5.0
            }}),
            json!({"event":"preview_displayed","elapsed_ms":21.0,"detail":{
                "generation":7,"path":"region","quality":"interactive","proxy_approximate":true
            }}),
        ];
        let (queued, samples) = paced_stroke_phase_samples(&events, 1).unwrap();
        assert_eq!(queued, 1);
        assert_eq!(samples.len(), 1);
        let sample = &samples[0];
        assert_eq!(sample.phase, "region");
        assert_eq!(sample.worker_render_ms, 5.0);
        assert_eq!(sample.queue_wait_ms, 2.0);
        assert_eq!(sample.result_to_surface_ms, 1.0);
        assert_eq!(sample.input_to_presented_ms, 11.0);
    }

    /// A position is answered by the first presented frame whose `draft.set` already carried it,
    /// so one whose own frame was superseded is answered by the newer frame that carries it; the
    /// stroke's quarters are read from the positions a frame carried; and each frame's overlay lag
    /// is its own generation's.
    #[test]
    fn a_position_waits_for_the_first_frame_that_carries_it() {
        let events = vec![
            json!({"event":"script_step","elapsed_ms":0.0,"detail":{"request":{"mask":{"stroke":{
                "interval_ms":24,"points":[[0.2,0.5],[0.3,0.5],[0.4,0.5],[0.5,0.5]]
            }}}}}),
            json!({"event":"mask_stroke_position","elapsed_ms":10.0,"detail":{"index":0}}),
            json!({"event":"mask_stroke_position","elapsed_ms":34.0,"detail":{"index":1}}),
            json!({"event":"mask_stroke_position","elapsed_ms":58.0,"detail":{"index":2}}),
            json!({"event":"mask_stroke_position","elapsed_ms":82.0,"detail":{"index":3}}),
            json!({"event":"mask_overlay","elapsed_ms":24.0,"detail":{"generation":1}}),
            json!({"event":"mask_overlay","elapsed_ms":99.0,"detail":{"generation":4}}),
        ];
        let sample = |generation, positions, displayed_ms| PaintPhaseSample {
            generation,
            positions: Some(positions),
            phase: "proxy",
            proxy: true,
            displayed_ms,
            input_to_presented_ms: 0.0,
            owner_round_trip_ms: 0.0,
            executor_wait_ms: 0.0,
            draft_set_ms: 0.0,
            preview_job_ms: 0.0,
            return_to_queue_ms: 0.0,
            queue_wait_ms: 0.0,
            worker_render_ms: 0.0,
            before_worker_result_ms: 0.0,
            result_to_surface_ms: 0.0,
        };
        // Position 2's own frame was superseded: position 3's frame, which carried both, is the
        // first to show it.
        let samples = [sample(1, 1, 20.0), sample(2, 2, 44.0), sample(4, 4, 95.0)];
        let presented = presented_paint_frames(&samples, &[]);
        let latencies = position_latencies(&events, 4, &presented).expect("paired positions");
        assert_eq!(latencies, vec![(0, 10.0), (1, 10.0), (2, 37.0), (3, 13.0)]);
        assert_eq!(overlay_lags(&events, &samples).unwrap(), vec![4.0, 4.0]);
        assert_eq!(stroke_quarter(Some(1), 4), Some("early"));
        assert_eq!(stroke_quarter(Some(2), 4), None);
        assert_eq!(stroke_quarter(Some(4), 4), Some("late"));
        assert_eq!(stroke_quarter(None, 4), None);
    }

    #[test]
    fn a_strokes_press_is_timed_to_its_first_set_and_its_first_frame() {
        let events = vec![
            json!({"event":"mask_draft_set","elapsed_ms":1.0}),
            json!({"event":"script_step","elapsed_ms":9.0,"detail":{"request":{"mask":{"stroke":{
                "interval_ms":24,"points":[[0.2,0.5],[0.3,0.5]]
            }}}}}),
            json!({"event":"mask_stroke_position","elapsed_ms":12.0,"detail":{"index":0}}),
            json!({"event":"mask_draft_begin","elapsed_ms":12.5}),
            json!({"event":"mask_draft_set","elapsed_ms":13.0}),
            json!({"event":"mask_stroke_position","elapsed_ms":36.0,"detail":{"index":1}}),
            json!({"event":"mask_draft_set","elapsed_ms":36.5}),
        ];
        let sample = |displayed_ms| PaintPhaseSample {
            generation: 1,
            positions: Some(1),
            phase: "proxy",
            proxy: true,
            displayed_ms,
            input_to_presented_ms: 0.0,
            owner_round_trip_ms: 0.0,
            executor_wait_ms: 0.0,
            draft_set_ms: 0.0,
            preview_job_ms: 0.0,
            return_to_queue_ms: 0.0,
            queue_wait_ms: 0.0,
            worker_render_ms: 0.0,
            before_worker_result_ms: 0.0,
            result_to_surface_ms: 0.0,
        };
        let presented = presented_paint_frames(&[sample(40.0), sample(22.0)], &[]);
        let (set, frame) = stroke_press(&events, 2, &presented).expect("a press");
        assert_eq!(set, 1.0);
        assert_eq!(frame, Some(10.0), "the earliest of the stroke's frames");
        assert_eq!(stroke_press(&events, 2, &[]).expect("a press").1, None);
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
            "the synthetic run contains no exact cancellation event"
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

    /// Interior fields follow the same triangle, scaled about their own origin: exposure's — Basic's,
    /// on a JPEG and a RAW photo alike — is the historical ±2 EV one value for value, and the RAW
    /// temperature's and tint's stay inside their declared ranges on their own steps, peaking at
    /// 40% of the smaller half of the range.
    #[test]
    fn a_fields_burst_is_the_triangle_scaled_to_its_own_range() {
        assert_eq!(FieldTarget::basic_exposure().burst_values(), burst_values());
        for (action, parameter, peak) in [
            ("set-raw", "temperature", 1800.0),
            ("set-raw", "tint", 40.0),
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

    #[test]
    fn detail_strength_bursts_change_values_and_return_to_the_limit_origin() {
        for parameter in ["sharpening", "luminance", "colour"] {
            let mut field =
                resolve_field(Control::Slider, Some("set-detail"), Some(parameter)).unwrap();
            assert_eq!(field.origin, field.min);
            assert_eq!(field.min, 0.0);
            // The declared Detail fields start at their lower limit; also pin the inward direction
            // for a field whose origin is its upper limit.
            for origin in [field.min, field.max] {
                field.origin = origin;
                let values = field.burst_values();
                assert_eq!(values.len(), burst_values().len());
                assert_eq!(values.first(), Some(&origin));
                assert_eq!(values.last(), Some(&origin));
                let middle = values.len() / 2;
                assert_ne!(
                    values[middle], origin,
                    "{parameter} must exercise real edits"
                );
                let excursion = (values[middle] - origin).abs();
                assert!(
                    (excursion - BURST_PEAK_FRACTION * (field.max - field.min)).abs() <= field.step,
                    "{parameter}: {excursion}"
                );
                for value in &values {
                    assert!(
                        (field.min..=field.max).contains(value),
                        "{parameter}: {value}"
                    );
                    assert_eq!((value / field.step).round() * field.step, *value);
                }
                let direction = if origin == field.min { 1.0 } else { -1.0 };
                assert!(
                    values[..=middle]
                        .windows(2)
                        .all(|pair| direction * pair[1] >= direction * pair[0])
                );
                assert!(
                    values[middle..]
                        .windows(2)
                        .all(|pair| direction * pair[1] <= direction * pair[0])
                );
            }
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
        let field = FieldTarget::proof_curve();
        let values = gesture_values(31, Control::Curve, &field);
        let gesture = gesture_steps(&values, Control::Curve, &field);
        assert_eq!(gesture.len(), 31);
        let steps = script::write(&gesture);
        assert_eq!(steps[0]["curve"]["finish"], "open");
        assert_eq!(steps[30]["curve"]["finish"], "release");
        assert_eq!(steps[0]["curve"]["points"][0][0], 0.5);
        assert_eq!(values[0], 1.0 / 32.0);
        assert_eq!(values[30], 31.0 / 32.0);
        assert_eq!(gesture_values(33, Control::Curve, &field)[32], 33.0 / 64.0);
        let wire: Vec<Value> = serde_json::from_str(&steps.to_string()).unwrap();
        for (step, expected) in wire.iter().zip(&values) {
            let fraction = step["curve"]["points"][0][1].as_f64().unwrap();
            assert_eq!(fraction, *expected);
            assert_eq!(f64::from(fraction as f32), *expected);
        }
        let setup = curve_view_steps(&field, SourceTag::Jpeg, false);
        assert_eq!(setup.len(), 6);
        assert_eq!(setup.last().unwrap(), &script::Step::tools_scroll(1.0));
        let burst = burst_step(&values, Control::Curve, &field).to_value();
        assert_eq!(burst["curve"]["points"].as_array().unwrap().len(), 31);
        for point in burst["curve"]["points"].as_array().unwrap() {
            assert!((0.0..=1.0).contains(&point[1].as_f64().unwrap()));
        }
        assert!(setup.len() + gesture.len() + 2 <= script::MAX_SCRIPT_STEPS); // optional crop, then burst
    }

    /// A GPU tick answers the draft.set of its own update and is presented by the surface's first
    /// draw of its plan, named by its draft and revision; another draft's frame of the same
    /// revision is not it. A CPU tick keeps its preview job's frame and its reason, and its draw is
    /// named by its generation.
    #[test]
    fn a_gpu_tick_is_presented_by_the_draw_of_its_own_revision() {
        let field = FieldTarget::basic_exposure();
        let events = vec![
            json!({"event":"slider_draft_set","elapsed_ms":10.0,
                "detail":{"fields":{"exposure":0.1}}}),
            json!({"event":"gpu_preview_tick","elapsed_ms":10.25,"detail":{"path":"cpu",
                "reason":"boundary-pending","draft_id":"d","draft_revision":1,"generation":5}}),
            json!({"event":"slider_draft_preview","elapsed_ms":10.5,
                "detail":{"value":0.1,"generation":5,"draft_revision":1}}),
            json!({"event":"preview_displayed","elapsed_ms":40.0,
                "detail":{"generation":5,"draft_revision":1}}),
            json!({"event":"surface_frame_drawn","elapsed_ms":60.0,
                "detail":{"path":"cpu","drawn_ms":45.0,"generation":5,"picture":3}}),
            json!({"event":"slider_draft_set","elapsed_ms":70.0,
                "detail":{"fields":{"exposure":0.2}}}),
            json!({"event":"gpu_preview_tick","elapsed_ms":70.25,"detail":{"path":"gpu",
                "draft_id":"d","draft_revision":2,"boundary":1}}),
            json!({"event":"surface_frame_drawn","elapsed_ms":75.0,"detail":{"path":"gpu",
                "drawn_ms":71.0,"draft_id":"e","draft_revision":2,"boundary":1}}),
            json!({"event":"surface_frame_drawn","elapsed_ms":90.0,"detail":{"path":"gpu",
                "drawn_ms":78.5,"draft_id":"d","draft_revision":2,"boundary":1}}),
        ];
        let paired = inputs(&events, Control::Slider, &field).unwrap();
        assert_eq!(paired.len(), 2);
        let (cpu, gpu) = (&paired[0], &paired[1]);
        assert_eq!(
            (cpu.path, cpu.reason.as_deref(), cpu.generation),
            (FramePath::Cpu, Some("boundary-pending"), Some(5))
        );
        assert_eq!((cpu.displayed_ms, cpu.drawn_ms), (40.0, 45.0));
        assert_eq!(
            (gpu.path, gpu.draft_revision, gpu.draft_id.as_deref()),
            (FramePath::Gpu, Some(2), Some("d"))
        );
        assert_eq!(gpu.queued_ms, 70.25);
        assert_eq!((gpu.displayed_ms, gpu.drawn_ms), (78.5, 78.5));
        let sample = gpu.sample();
        assert_eq!(sample["path"], "gpu");
        assert_eq!(sample["input_to_presented_ms"], json!(8.5));
        // A GPU tick whose plan was never drawn is not presented.
        let undrawn = inputs(&events[..8], Control::Slider, &field).unwrap();
        assert!(undrawn[1].displayed_ms.is_nan());
    }

    /// `--warm` and `--contend` put their steps between the preconditions and the first input, and
    /// the activity read follows the release; `--idle` closes the Performance section and checks
    /// idle after the release instead. Without them the script is the plain drag's.
    #[test]
    fn a_drag_places_its_warm_contention_and_idle_steps_around_the_gesture() {
        let source = PathBuf::from("unused.jpg");
        let dir = PathBuf::from("/tmp/contention");
        let field = FieldTarget::basic_exposure();
        let options = Options {
            source: &source,
            samples: 3,
            mode: Mode::Drag,
            control: Control::Slider,
            action: None,
            parameter: None,
            crop: None,
            idle: false,
            basic: true,
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
            gpu_preview_off: false,
        };
        let values = gesture_values(4, Control::Slider, &field);
        let plain = gesture_script(&options, &field, SourceTag::Jpeg, &values, true);
        assert_eq!(
            measured_script(&options, &field, SourceTag::Jpeg, &values, true, &dir),
            plain
        );
        let contended = Options {
            contend: Some(2),
            warm_ms: Some(500),
            ..options
        };
        let steps = measured_script(&contended, &field, SourceTag::Jpeg, &values, true, &dir);
        assert_eq!(steps.len(), plain.len() + 4);
        assert_eq!(steps[0], basic_precondition(&contended));
        assert_eq!(steps[1], script::Step::Wait { ms: 500 });
        assert_eq!(steps[2..4], contention_steps(&dir, 2)[..]);
        assert_eq!(
            steps[4..8],
            plain[1..5],
            "the drag's inputs and its release"
        );
        assert_eq!(steps[8], script::Step::call("activity.list", json!({})));
        assert_eq!(steps.last(), plain.last(), "then the burst step");
        let idle = Options {
            idle: true,
            ..options
        };
        let steps = measured_script(&idle, &field, SourceTag::Jpeg, &values, true, &dir);
        assert_eq!(
            steps[5..7],
            [
                script::Step::Performance { expanded: false },
                script::Step::Idle(IDLE_AFTER_DISSOLVE)
            ]
        );
        let commit = measured_script(&idle, &field, SourceTag::Jpeg, &values, false, &dir);
        assert_eq!(
            commit,
            gesture_script(&idle, &field, SourceTag::Jpeg, &values, false)
        );
        // `--no-gpu-preview` turns the preference off from the palette before anything else.
        let off = Options {
            gpu_preview_off: true,
            ..options
        };
        let steps = measured_script(&off, &field, SourceTag::Jpeg, &values, true, &dir);
        assert_eq!(
            steps[0],
            script::Step::Palette(PaletteStep::Run("gpu preview".into()))
        );
        assert_eq!(steps[1..], plain[..]);
    }

    /// The lane is busy from the first export's acceptance to the end the activity board reads
    /// back: the answer itself while one still runs, otherwise the latest finished export's end.
    #[test]
    fn a_contended_runs_window_runs_from_the_first_export_to_the_last_ones_end() {
        let export =
            |job: &str| json!({"request":{"api":{"method":"export.jpeg"}},"result":{"job_id":job}});
        let mut app = json!({"script":[export("a"), export("b"),
            {"request":{"api":{"method":"activity.list"}},"result":{"active":[],"recent":[
                {"kind":"export","duration_ms":300.0,"ended_ms_ago":100.0},
                {"kind":"render","duration_ms":900.0,"ended_ms_ago":10.0}]}}]});
        let step = |at: f64, method: &str| {
            [
                json!({"event":"script_step","elapsed_ms":at,
                    "detail":{"request":{"api":{"method":method}}}}),
                json!({"event":"script_step_settled","elapsed_ms":at + 2.0}),
            ]
        };
        let events: Vec<Value> = [
            step(100.0, "export.jpeg"),
            step(150.0, "export.jpeg"),
            step(1000.0, "activity.list"),
        ]
        .into_iter()
        .flatten()
        .collect();
        let (windows, report) = contention_windows(&events, &app, 2).unwrap();
        assert_eq!(windows, vec![(102.0, 902.0)]);
        assert_eq!(
            report["finished_export_windows_ms"],
            json!([[602.0, 902.0]])
        );
        app["script"][2]["result"]["active"] = json!([{"kind":"export","elapsed_ms":50.0}]);
        let (windows, _) = contention_windows(&events, &app, 2).unwrap();
        assert_eq!(windows, vec![(102.0, 1002.0)]);
        assert!(
            contention_windows(&events, &app, 3).is_err(),
            "a refused export"
        );
    }

    /// The idle check after a release reports its window and the dissolves before it, and a run
    /// with no dissolve before its check is refused.
    #[test]
    fn idle_after_a_dissolve_needs_a_dissolve_before_its_check() {
        let check = json!({"event":"idle_check","elapsed_ms":9000.0,"detail":{"passed":true,
            "drawn_frames_delta":1,"views_delta":1,"process_cpu_percent_one_core":0.25}});
        let started = json!({"event":"gpu_dissolve_started","elapsed_ms":100.0});
        let ended = json!({"event":"gpu_dissolve_ended","elapsed_ms":260.0});
        let report = idle_after_dissolve(&[started, ended, check.clone()]).unwrap();
        assert_eq!(report["dissolves_started"], 1);
        assert_eq!(report["dissolves_ended"], 1);
        assert_eq!(report["check"]["process_cpu_percent_one_core"], 0.25);
        assert!(idle_after_dissolve(&[check]).is_err());
    }

    /// A stroke position drawn on the GPU pairs its `mask_draft_set` with the GPU tick of its own
    /// update, counts the positions that set carried, and is presented by the draw of its plan; a
    /// tick superseded before its draw is counted and not sampled.
    #[test]
    fn a_stroke_position_drawn_on_the_gpu_is_timed_to_the_draw_of_its_plan() {
        let set = |at: f64, points: usize| {
            json!({"event":"mask_draft_set","elapsed_ms":at,
                "detail":{"fields":{"points":vec![[0.5, 0.5]; points]}}})
        };
        let tick = |at: f64, revision: u64| {
            json!({"event":"gpu_preview_tick","elapsed_ms":at,
                "detail":{"path":"gpu","draft_id":"m","draft_revision":revision}})
        };
        let drawn = |at: f64, revision: u64| {
            json!({"event":"surface_frame_drawn","elapsed_ms":at + 5.0,
                "detail":{"path":"gpu","drawn_ms":at,"draft_id":"m","draft_revision":revision}})
        };
        let events = vec![
            json!({"event":"script_step","elapsed_ms":0.0,"detail":{"request":{"mask":{"stroke":{
                "interval_ms":24,"points":[[0.2,0.5],[0.3,0.5],[0.4,0.5]]
            }}}}}),
            set(10.0, 2),
            tick(10.5, 2),
            set(12.0, 3),
            tick(12.5, 3),
            drawn(19.0, 3),
        ];
        let (ticks, samples) = paced_stroke_gpu_samples(&events, 3).unwrap();
        assert_eq!(ticks, 2);
        assert_eq!(samples.len(), 1, "revision 2 was superseded before a draw");
        let sample = &samples[0];
        assert_eq!((sample.draft_revision, sample.positions), (3, Some(3)));
        assert_eq!(
            (sample.input_to_presented_ms, sample.tick_to_drawn_ms),
            (7.0, 6.5)
        );
        assert_eq!(presented_paint_frames(&[], &samples), vec![(19.0, Some(3))]);
    }

    #[test]
    fn a_reused_generation_pairs_only_frames_after_the_input_was_queued() {
        let field = FieldTarget::basic_exposure();
        let mut events = vec![
            json!({"event":"preview_displayed","elapsed_ms":5.0,
                "detail":{"generation":4,"draft_revision":null}}),
            json!({"event":"slider_draft_set","elapsed_ms":10.0,
                "detail":{"fields":{"exposure":0.0}}}),
            json!({"event":"slider_draft_preview","elapsed_ms":20.0,
                "detail":{"value":0.0,"generation":4,"draft_revision":1}}),
        ];
        let paired = inputs(&events, Control::Slider, &field).unwrap();
        assert_eq!(paired.len(), 1);
        assert!(paired[0].displayed_ms.is_nan());
        events.push(json!({"event":"preview_displayed","elapsed_ms":30.0,
            "detail":{"generation":4,"draft_revision":1}}));
        let paired = inputs(&events, Control::Slider, &field).unwrap();
        assert_eq!(paired[0].displayed_ms, 30.0);
        events[3]["detail"]["draft_revision"] = json!(2);
        assert!(inputs(&events, Control::Slider, &field).is_err());
        events[3]["detail"]["draft_revision"] = json!(1);
        events.extend([
            json!({"event":"slider_draft_set","elapsed_ms":40.0,
                "detail":{"fields":{"exposure":0.0}}}),
            json!({"event":"slider_draft_preview","elapsed_ms":50.0,
                "detail":{"value":0.0,"generation":4,"draft_revision":2}}),
            json!({"event":"preview_displayed","elapsed_ms":60.0,
                "detail":{"generation":4,"draft_revision":2}}),
        ]);
        let paired = inputs(&events, Control::Slider, &field).unwrap();
        assert_eq!(paired[0].displayed_ms, 30.0);
        assert_eq!(paired[1].displayed_ms, 60.0);
    }

    #[test]
    fn curve_midpoint_pairs_one_draft_set_with_its_displayed_generation() {
        let points = json!([[0.0, 0.0], [0.5, 0.375], [1.0, 1.0]]);
        let events = vec![
            json!({"event":"slider_draft_set","elapsed_ms":10.0,
                "detail":{"fields":{"master":points}}}),
            json!({"event":"slider_draft_preview","elapsed_ms":20.0,
                "detail":{"value":points,"generation":7,"draft_revision":2}}),
            json!({"event":"preview_displayed","elapsed_ms":35.0,
                "detail":{"generation":7,"draft_revision":2}}),
        ];
        let proof = FieldTarget::proof_curve();
        let paired = inputs(&events, Control::Curve, &proof).unwrap();
        assert_eq!(paired.len(), 1);
        assert_eq!(paired[0].value, 0.375);
        assert_eq!(paired[0].displayed_ms - paired[0].sent_ms, 25.0);
        let mut wrong = events;
        wrong[2]["detail"]["draft_revision"] = json!(3);
        assert!(inputs(&wrong, Control::Curve, &proof).is_err());
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

        // The RAW white balance is a field patch too. Its temperature's range holds no zero, so
        // its gesture starts at the declared 6504 K and every value stays inside 2000..12000 on
        // its 10 K step.
        let kelvin = resolve_field(Control::Slider, Some("set-raw"), Some("temperature")).unwrap();
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
        let field = resolve_field(Control::Slider, Some("set-raw"), Some("tint")).unwrap();
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

    /// Every default exposure value is the decimal the slider sends, so a 30- or 100-sample drag
    /// never asks for a value no rail fraction reaches (the 19th was once 0.5700000000000001).
    #[test]
    fn gesture_values_on_a_decimal_step_are_the_decimals_the_slider_sends() {
        let field = FieldTarget::basic_exposure();
        for count in [31, 101] {
            for (index, value) in field.gesture_values(count).iter().enumerate() {
                let hundredths = (value * 100.0).round();
                assert_eq!(*value, hundredths / 100.0, "value {index} of {count}");
                assert_eq!(value.to_string().parse::<f64>().unwrap(), *value);
            }
        }
        assert_eq!(field.gesture_values(31)[18], 0.57);
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
            curve: None,
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
