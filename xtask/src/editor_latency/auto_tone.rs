//! `editor-latency --mode auto-tone`: Auto tone's input to committed entry and to adopted frame,
//! measured end to end on the release editor ([design], "Measurement").
//!
//! Each sample is one real Auto input: the Basic section's Auto button (the generated control's
//! action, through the same message a click sends) or, with `--shortcut`, `⌘U` through the key
//! table. Two figures come from events the editor logs, both from the `script_step` that sent the
//! input:
//!
//! - **entry**: to `analysis_action_answered`, the action's answer reaching the desktop with the
//!   entry it committed and the report it carried;
//! - **frame**: to the first exact frame of that entry on screen, a `preview_displayed` (neither
//!   a draft, a proxy nor a zoom's hand-over; at Fit the reference's exact frame is reduced to the
//!   view) or a `preview_pixels_reused` naming it.
//!
//! The report's own sample (grid points, and the 13 bytes a point retains: RGB as three `f32` and
//! one source-clipped flag, as `GridSample::bytes` counts them) and the renderer that read it are
//! kept per sample; a GPU run whose answer names the reference, or the reverse, fails.
//!
//! The sample cache decides warm and cold, as the engine measurement's does. Before each sample a
//! step returns Auto's eight fields to zero, so the input always commits:
//!
//! - **warm** also sets Tint to the sample's number, so every committed stack is new and its
//!   frame is drawn, never reused, while the analysis grid, which reads the stage Basic receives,
//!   stays the one the unmeasured first Auto read;
//! - **cold** (`--cold`) then commits a crop inset by a further 0.1% per sample: a new output
//!   geometry, so a grid no earlier read retained.
//!
//! Solve time is not visible from the desktop: the answer carries no timing. The engine
//! measurement `auto_tone_measure_photo_sized_inputs` records read and solve time on the same
//! sources and renderers.
//!
//! [design]: ../../../docs/design/auto-tone.md
use super::*;
use luxforge_core::auto_tone::FIELDS;
use luxforge_evidence::ControlsStep;

/// What a person presses for Auto, and which sample cache the measured inputs meet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AutoTone {
    /// Each input meets a grid no earlier read retained.
    pub cold: bool,
    /// `⌘U` through the key table rather than the Auto button.
    pub shortcut: bool,
}

/// How long the editor is left alone after each change and each input: long enough for a 60 MP
/// stack's frame to land before the next step supersedes it.
const SETTLE_MS: u64 = 2500;
/// Steps before the first sample: a settle, the unmeasured first Auto and its settle.
const PREAMBLE_STEPS: usize = 3;
/// The most samples a run takes, over as many launches as they need.
const MAX_SAMPLES: usize = 60;
/// A grid point's retained bytes: RGB as three `f32` and one source-clipped flag.
const POINT_BYTES: u64 = 13;

fn input(auto: AutoTone) -> script::Step {
    if auto.shortcut {
        script::Step::key("Command+U")
    } else {
        script::Step::Controls(ControlsStep::Action {
            action: "auto-tone".into(),
            background: false,
        })
    }
}

/// The steps before sample `number` (from one): Auto's fields back to zero, and the change that
/// makes its stack new, Tint when warm, a further-inset crop when cold.
fn resets(auto: AutoTone, number: usize) -> Vec<script::Step> {
    let mut fields: serde_json::Map<String, Value> = FIELDS
        .iter()
        .map(|field| ((*field).to_owned(), json!(0)))
        .collect();
    if auto.cold {
        let inset = 0.001 * number as f64;
        vec![
            script::Step::call("edit.set-basic", Value::Object(fields)),
            script::Step::call(
                "edit.crop",
                json!({"angle":0,"x":inset,"y":inset,"width":1.0-2.0*inset,"height":1.0-2.0*inset}),
            ),
        ]
    } else {
        fields.insert("tint".into(), json!(number));
        vec![script::Step::call("edit.set-basic", Value::Object(fields))]
    }
}

/// Steps per sample: the resets, a settle, the input and a settle.
fn sample_steps(auto: AutoTone) -> usize {
    resets(auto, 1).len() + 3
}

/// Samples one launch takes within the evidence script's step bound.
fn per_launch(auto: AutoTone) -> usize {
    (script::MAX_SCRIPT_STEPS - PREAMBLE_STEPS) / sample_steps(auto)
}

/// One launch's script, for samples `first..first + count` (numbered from one), and the step
/// numbers of its measured inputs.
fn launch_script(auto: AutoTone, first: usize, count: usize) -> (Vec<script::Step>, Vec<usize>) {
    let mut steps = vec![
        script::Step::wait(SETTLE_MS),
        input(auto),
        script::Step::wait(SETTLE_MS),
    ];
    let mut inputs = Vec::new();
    for number in first..first + count {
        steps.extend(resets(auto, number));
        steps.push(script::Step::wait(SETTLE_MS));
        steps.push(input(auto));
        // Script steps are numbered from one, in the order the editor runs them.
        inputs.push(steps.len());
        steps.push(script::Step::wait(SETTLE_MS));
    }
    (steps, inputs)
}

pub(super) fn run(
    root: &Path,
    out: &Path,
    bin: &Path,
    options: &Options,
    auto: AutoTone,
) -> Result {
    let per_launch = per_launch(auto);
    ensure(
        (1..=MAX_SAMPLES).contains(&options.samples),
        format!(
            "Auto tone samples must be 1..{MAX_SAMPLES}, {per_launch} to a launch within the \
             64-step script"
        ),
    )?;
    ensure(
        options.crop.is_none()
            && !options.basic
            && !options.presence
            && !options.curve_layer
            && !options.detail
            && !options.lens
            && !options.perspective
            && !options.mask
            && options.zoom.is_none()
            && options.action.is_none()
            && options.parameter.is_none(),
        "Auto tone is measured on the photograph as opened, at Fit; omit the recipe and view flags",
    )?;
    ensure(
        auto.cold || source_tag(options.source)? != SourceTag::Raw,
        "A warm Auto tone run varies Basic's Tint, which a RAW's Basic refuses; measure a JPEG",
    )?;
    // Each launch within its own deadline: its settles, and room for the launch and the inputs.
    let deadline =
        Duration::from_secs(90) + Duration::from_millis(2 * SETTLE_MS * (per_launch as u64 + 1));
    let run = Run::tool(root, out, TOOL, bin, deadline)?;
    run.check(|run| measure(run, options, auto))
}

/// One measured input's figures.
struct Sample {
    entry_ms: f64,
    frame_ms: f64,
    bytes: Option<u64>,
    record: Value,
}

/// Whether a `preview_displayed` is an exact frame of `entry`: the committed stack's own frame,
/// not a drag's draft or display-size proxy, a zoom's hand-over or a drafted RAW white balance.
/// A reduced frame counts: at Fit the reference's exact frame reaches the screen reduced to the
/// view's bounds, labelled exact, and is never followed by an unreduced one.
fn exact_frame_of(detail: &Value, entry: &str) -> bool {
    detail["entry_id"] == entry
        && detail["draft_revision"].is_null()
        && detail["proxy"] != json!(true)
        && detail["reason"].is_null()
        && detail["approximate_white_balance"] != json!(true)
}

/// The input sent by `step`, read from `events`: its answer, its exact frame and its report.
fn sample(events: &[Value], step: usize, gpu: bool) -> Result<Sample> {
    let sent = events
        .iter()
        .position(|event| event["event"] == "script_step" && event["detail"]["step"] == json!(step))
        .ok_or_else(|| format!("Step {step} was never sent"))?;
    let start = elapsed(&events[sent])?;
    let later = &events[sent + 1..];
    let answered = later
        .iter()
        .find(|event| {
            event["event"] == "analysis_action_answered" && event["detail"]["action"] == "auto-tone"
        })
        .ok_or_else(|| format!("Step {step}'s Auto was never answered"))?;
    let detail = &answered["detail"];
    ensure(
        detail["error"].is_null(),
        format!("Step {step}'s Auto failed: {}", detail["error"]),
    )?;
    let entry = detail["entry_id"]
        .as_str()
        .ok_or_else(|| format!("Step {step}'s answer names no entry"))?;
    let report = &detail["report"];
    let renderer = report["renderer"]["record"].as_str().unwrap_or_default();
    ensure(
        renderer == if gpu { "gpu" } else { "reference" },
        format!("Step {step}'s sample was read by {renderer:?}, not the run's renderer"),
    )?;
    let frame = later
        .iter()
        .find(|event| match event["event"].as_str() {
            Some("preview_displayed") => exact_frame_of(&event["detail"], entry),
            Some("preview_pixels_reused") => event["detail"]["identity"]["entry_id"] == entry,
            _ => false,
        })
        .ok_or_else(|| format!("No exact frame of step {step}'s entry {entry} was shown"))?;
    let bytes = report["sample"]["count"]
        .as_u64()
        .map(|points| points * POINT_BYTES);
    let (entry_ms, frame_ms) = (elapsed(answered)? - start, elapsed(frame)? - start);
    Ok(Sample {
        entry_ms,
        frame_ms,
        bytes,
        record: json!({
            "step":step,
            "entry_id":entry,
            "input_to_committed_entry_ms":entry_ms,
            "input_to_adopted_frame_ms":frame_ms,
            "frame_event":frame["event"],
            "frame_path":frame["detail"]["path"],
            "frame_reduced":frame["detail"]["reduced"],
            "renderer":report["renderer"],
            "sample":report["sample"],
            "sample_bytes":bytes,
            "values":report["values"],
        }),
    })
}

fn measure(run: &mut Run, options: &Options, auto: AutoTone) -> Result {
    let (root, out) = (&run.root().to_path_buf(), &run.out().to_path_buf());
    let source = options.source.canonicalize()?;
    let source_hash = hash(&source)?;
    let load_start = launch::load_average(root);
    let gpu = !options.no_gpu_render;
    let per_launch = per_launch(auto);
    let mut samples = Vec::new();
    let (mut header, mut opened) = (Value::Null, Value::Null);
    let mut first = 1;
    while first <= options.samples {
        let count = per_launch.min(options.samples + 1 - first);
        let (steps, inputs) = launch_script(auto, first, count);
        ensure(
            steps.len() <= script::MAX_SCRIPT_STEPS,
            "An Auto tone launch exceeds the 64-step evidence bound",
        )?;
        let name = format!("auto-tone-{}", first.div_ceil(per_launch));
        let launch = Launch::named(&name)
            .script(&format!("{name}-script.json"), script::write(&steps))
            .open_all(std::slice::from_ref(&source))
            .order(ORDER);
        let launch = match options.window {
            Some([width, height]) => {
                launch.window([width.to_string().as_str(), height.to_string().as_str()])
            }
            None => launch,
        };
        let Launched { dir: evidence, .. } = run.launch(rendered_by(launch, options))?;
        let app = read_json(&evidence.join("result.json"))?;
        ensure(
            app["status"] == "captured"
                && app["had_input_errors"] == json!(false)
                && app["script"]
                    .as_array()
                    .is_some_and(|recorded| recorded.len() == steps.len()),
            format!("An Auto tone step failed or never ran: {}", app["script"]),
        )?;
        let events = scenario::events(&evidence.join("events.jsonl"))?;
        header = run.provenance(&events)?;
        let frames = app["frames"].as_array().ok_or("Missing frames")?;
        if options.no_gpu_render {
            ensure(
                frames
                    .last()
                    .is_some_and(|last| last["state"]["renderer"]["record"] == "reference"),
                "--reference-renderer left the GPU drawing the photograph",
            )?;
        }
        let frame = frame_at(frames, 0, "auto-tone")?;
        opened = json!({
            "source_dimensions":frame["state"]["source_dimensions"],
            "preview_dimensions":frame["state"]["preview_dimensions"],
            "backend":frame["state"]["backend"],
            "physical_size":frame["physical_size"],
            "scale":frame["scale"],
        });
        for step in inputs {
            samples.push(sample(&events, step, gpu)?);
        }
        first += count;
    }
    let load_end = launch::load_average(root);
    let rows = vec![
        stats::row(
            "input_to_committed_entry",
            "ms",
            samples.iter().map(|sample| sample.entry_ms),
        ),
        stats::row(
            "input_to_adopted_frame",
            "ms",
            samples.iter().map(|sample| sample.frame_ms),
        ),
        stats::row(
            "sample_bytes",
            "bytes",
            samples
                .iter()
                .filter_map(|sample| sample.bytes.map(|bytes| bytes as f64)),
        ),
    ];
    let (cache, input) = (
        if auto.cold { "cold" } else { "warm" },
        if auto.shortcut { "shortcut" } else { "button" },
    );
    let renderer = if gpu { "gpu" } else { "reference" };
    let change = if auto.cold {
        "then a crop inset 0.1% further each sample, so its grid is new"
    } else {
        "with Tint set to the sample's number, so the stack is new and its grid retained"
    };
    let sent = if auto.shortcut {
        "Command+U through the key table"
    } else {
        "the Auto button's action"
    };
    let mut result = json!({
        "status":"passed",
        "source":source,
        "source_sha256":source_hash,
        "opened":opened,
        "mode":"auto-tone",
        "renderer":renderer,
        "sample_cache":cache,
        "input":input,
        "samples":options.samples,
        "launches":options.samples.div_ceil(per_launch),
        "method":format!("Background evidence launches of the release binary, warm filesystem cache, at Fit, at most {per_launch} samples to a launch. Each launch settles {SETTLE_MS} ms and runs one unmeasured Auto; then each sample returns Auto's eight fields to zero {change}, settles {SETTLE_MS} ms, sends the input ({sent}) and settles {SETTLE_MS} ms. Times are from the script_step event that sent the input to analysis_action_answered, the answer with its committed entry on the desktop, and to the first exact preview_displayed or preview_pixels_reused of that entry. Sample bytes are the answer's grid points at 13 bytes each."),
        "rows":rows,
        "inputs":samples.iter().map(|sample| sample.record.clone()).collect::<Vec<_>>(),
        "load":launch::load(load_start),
        "load_average_1m_end":load_end,
        "scope":format!("Auto tone on this source as opened, {cache} sample cache, {renderer} renderer: input to the committed entry's answer on the desktop, and to its exact frame on screen. Solve time is not visible here; the engine measurement auto_tone_measure_photo_sized_inputs records read and solve time."),
    });
    stamp(&mut result, &header);
    write_json(&out.join("latency.json"), &result)?;
    ensure(hash(&source)? == source_hash, "The source changed")?;
    println!(
        "PASS editor latency (auto-tone, {cache}, {renderer}): {}",
        out.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference's frame at Fit is its exact frame reduced to the view: it is the sample's
    /// frame, while a drag's proxy and a draft of the same entry before it are not.
    #[test]
    fn the_reference_frame_reduced_to_the_view_is_the_exact_frame() {
        let entry = "entry-1";
        let displayed = |at: f64, detail: Value| {
            let mut detail = detail;
            detail["entry_id"] = json!(entry);
            json!({"event":"preview_displayed","elapsed_ms":at,"detail":detail})
        };
        let events = [
            json!({"event":"script_step","elapsed_ms":100.0,"detail":{"step":6}}),
            json!({"event":"analysis_action_answered","elapsed_ms":380.0,"detail":{
                "action":"auto-tone","entry_id":entry,"error":null,
                "report":{"renderer":{"record":"reference"},"sample":{"count":10}}}}),
            displayed(
                400.0,
                json!({"draft_revision":3,"proxy":false,"reduced":true,"reason":null}),
            ),
            displayed(
                420.0,
                json!({"draft_revision":null,"proxy":true,"reduced":true,"reason":null}),
            ),
            displayed(
                730.0,
                json!({"draft_revision":null,"proxy":false,"reduced":true,"reason":null,
                    "approximate_white_balance":false,"path":"surface"}),
            ),
        ];
        let measured = sample(&events, 6, false).unwrap();
        assert_eq!(
            (measured.entry_ms, measured.frame_ms, measured.bytes),
            (280.0, 630.0, Some(130))
        );
        assert!(sample(&events, 6, true).is_err());
    }

    #[test]
    fn auto_tone_launches_fit_the_script_bound_and_number_their_inputs() {
        for cold in [false, true] {
            let auto = AutoTone {
                cold,
                shortcut: cold,
            };
            let count = per_launch(auto);
            let (steps, inputs) = launch_script(auto, 4, count);
            assert!(steps.len() <= script::MAX_SCRIPT_STEPS);
            assert!(steps.len() + sample_steps(auto) > script::MAX_SCRIPT_STEPS);
            assert_eq!(inputs.len(), count);
            for (index, step) in inputs.into_iter().enumerate() {
                assert_eq!(steps[step - 1], input(auto));
                let resets = resets(auto, 4 + index);
                let before = step - 2 - resets.len();
                assert_eq!(steps[before..step - 2], resets[..]);
            }
        }
    }
}
