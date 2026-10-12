//! The `generative` smoke scenario: Remove, the Corrections band's generative fill
//! (`docs/design/ai-editing.md#desktop-surface`), on the real editor, background-only, with
//! correlated state, log and capture.
//!
//! One developer launch on the four-quadrant fixture. Remove is chosen with nothing set up and
//! names the experiment's flag; the flag is set through the API and Remove names the missing model
//! and its Download; the fake generator's grant and install go through the API as any client's
//! would. A stroke across the boundary of two quadrants is the selection; Generate is captured
//! while it runs (the ring and the status bar's progress), Escape cancels it, Generate runs to its
//! one candidate (owner, 2026-10-11), Regenerate adds one more, and the first is chosen again and
//! previewed on the canvas. A second client accepts the desktop's newest job's candidate. A prompt makes the action
//! Replace and offers the text encoder's download, which goes through the consent notice; Replace
//! then runs and Accept commits it. The fill of a flat-coloured context is that context's mean, so
//! a fill across two quadrants is legible in the capture: the left quadrant's colour inside the
//! selection becomes a mix of both.
use crate::{
    corrections_smoke::{MODULE, NEAR, PATCH_HALF, SIZE, correction, field, key},
    scenario::{Checked, Checks, Frame, Plan, Step, Tolerance, plan::only},
    *,
};
use luxforge_evidence::{
    self as script, CapabilityAction, CapabilityStep, CorrectionStep, PressStep, RemoveStep,
    RemoveUntil,
};

pub const SCENARIO: &str = "generative";
pub const ABOUT: &str = "Remove in the Corrections band on the developer fake models: the flag and model reasons, a painted selection, Generate's progress and Cancel, one candidate, Regenerate's second, a chosen candidate's preview, a second client's accept, a prompt's Replace with the text encoder's consented download, and Accept";

/// The developer AI module, which serves the fill tiers and their fake models.
const AI: &str = "luxforge.ai";
const GENERATOR: &str = "fake-generator";
const ORIGIN: &str = "https://developer.luxforge.invalid";
const PROMPT: &str = "a red bicycle";

/// The selection: a stroke across the boundary between the top two quadrants.
const STROKE: [[f64; 2]; 4] = [[0.42, 0.30], [0.47, 0.30], [0.53, 0.30], [0.58, 0.30]];
/// A patch inside the selection, on the left quadrant's side.
const INSIDE: [f64; 2] = [0.46, 0.30];
/// A patch well outside it, in the same quadrant.
const OUTSIDE: [f64; 2] = [0.25, 0.20];

/// How far the draft selection's translucent tint moves a patch, at least, per channel in 8-bit
/// codes: it is drawn over the photograph, not instead of it.
const TINTED: f64 = 8.0;

fn remove(step: RemoveStep) -> script::Step {
    correction(CorrectionStep::Remove(step))
}

fn ai(action: CapabilityAction) -> CapabilityStep {
    CapabilityStep::new(AI, action)
}

/// Every frame, in order.
pub fn plan(_: &[PathBuf]) -> Plan {
    let api = |name: &str, method: &str, params: Value| {
        Step::new(name, script::Step::call(method, params)).commits(0)
    };
    Plan::new(vec![
        Step::opened("opened").no_draft(),
        Step::new("mode", key("q")).commits(0).mode(MODULE),
        // Nothing set up: the experiment's flag is the first thing to act on.
        Step::new(
            "remove-off",
            correction(CorrectionStep::Kind("remove".into())),
        )
        .commits(0)
        .status_starts("Generative fill is an experiment that is off"),
        api(
            "flag",
            "flags.set",
            json!({"flag": "ai.generative", "value": true}),
        ),
        // The flag on: the model is missing, with its Download.
        Step::new(
            "remove-model",
            correction(CorrectionStep::Kind("remove".into())),
        )
        .commits(0)
        .status_starts("Generative fill needs its model installed"),
        // The fake generator's download granted and installed through the API.
        api(
            "allowed",
            "module.permission.grant",
            json!({"module_id": AI, "capability": format!("download-{GENERATOR}"),
                   "scope": {"resource": GENERATOR, "version": "1", "origin": ORIGIN}}),
        ),
        api(
            "installing",
            "module.resource.install",
            json!({"module_id": AI, "resource_id": GENERATOR}),
        ),
        Step::new("installed", ai(CapabilityAction::Settle))
            .commits(0)
            .status_starts("Paint over what to remove"),
        Step::new("size", field("radius", SIZE)).commits(0),
        Step::new(
            "painted",
            correction(CorrectionStep::Press(PressStep {
                at: STROKE[0],
                to: STROKE[1..].to_vec(),
                modifiers: Vec::new(),
                release: true,
                interval_ms: None,
            })),
        )
        .commits(0)
        .status_starts("Selection painted"),
        // Captured while it runs: the ring on the region and the progress in the status bar.
        Step::new(
            "generating",
            remove(RemoveStep::Generate(RemoveUntil::Progress)),
        )
        .commits(0)
        .status_starts("Generative fill \u{b7} candidate"),
        // Escape cancels the running generation first and keeps the selection.
        Step::new("cancelled", key("Escape"))
            .commits(0)
            .status_starts("Generation cancelled"),
        Step::new("generated", remove(RemoveStep::Generate(RemoveUntil::Done)))
            .commits(0)
            .status_starts("1 candidate"),
        Step::new(
            "regenerated",
            remove(RemoveStep::Regenerate(RemoveUntil::Done)),
        )
        .commits(0)
        .status_starts("1 candidate"),
        Step::new("chosen", remove(RemoveStep::Candidate(0))).commits(0),
        // A second client accepts a candidate the desktop generated.
        Step::new(
            "agent-accepted",
            script::Step::agent(
                "edit.accept-correction-patch",
                json!({"job_id": {"started_by": "task.fill-generative"}, "candidate": 0}),
            ),
        )
        .commits(1)
        .label("Add Generative fill 1"),
        // A prompt makes it Replace, which needs the text encoder: its download is offered, asked
        // for through the consent notice and installed.
        Step::new("prompt", remove(RemoveStep::Prompt(PROMPT.into()))).commits(0),
        Step::new("encoder-asked", remove(RemoveStep::Download))
            .commits(0)
            .notice("Allow AI editing to download a resource?"),
        Step::new("encoder-installed", ai(CapabilityAction::Consent(true))).commits(0),
        Step::new("replaced", remove(RemoveStep::Generate(RemoveUntil::Done)))
            .commits(0)
            .status_starts("1 candidate"),
        Step::new("accepted", remove(RemoveStep::Accept))
            .commits(1)
            .label(format!("Add Replace \"{PROMPT}\" 2"))
            .status_starts("Accepted"),
    ])
}

/// Remove's band model in a frame.
fn band(frame: &Frame) -> &Value {
    &frame.state()["corrections"]["band"]["remove"]
}

/// Remove's own record in a frame: its jobs, selection, held previews and last request.
fn record(frame: &Frame) -> &Value {
    &frame.state()["corrections"]["remove"]
}

fn rgb(frame: &Frame, at: [f64; 2]) -> Result<[f64; 3]> {
    frame.rgb_at(at, PATCH_HALF)
}

/// The largest per-channel difference between two patches.
fn apart(one: [f64; 3], two: [f64; 3]) -> f64 {
    one.iter()
        .zip(two)
        .map(|(one, two)| (one - two).abs())
        .fold(0.0, f64::max)
}

pub fn verify(run: &mut crate::scenario::Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let mut checks = Checks::new();
    let off = launch.at("remove-off")?;
    ensure(
        band(off)["unoffered"]["action"] == json!("Settings \u{203a} Experiments")
            && band(off)["tiers"] == json!([]),
        format!("Remove with the flag off: {}", band(off)),
    )?;
    let missing = launch.at("remove-model")?;
    ensure(
        band(missing)["unoffered"]["action"] == json!("Download"),
        format!("Remove without the model: {}", band(missing)),
    )?;
    let installed = launch.at("installed")?;
    ensure(
        band(installed)["tiers"] == json!(["generative"])
            && band(installed)["unoffered"].is_null()
            && band(installed)["action"] == json!("Generate"),
        format!("The Generative tier once installed: {}", band(installed)),
    )?;
    // The draft selection is tinted on the canvas.
    let opened = launch.at("opened")?;
    let painted = launch.at("painted")?;
    ensure(
        band(painted)["selection"] == json!(true),
        format!("The painted selection: {}", band(painted)),
    )?;
    checks.compare(
        painted,
        "the selection's tint, against the photograph",
        apart(rgb(painted, INSIDE)?, rgb(opened, INSIDE)?),
        0.0,
        Tolerance::Above(TINTED),
    )?;
    // The running generation: its ring's fraction and sentence, and the photograph locked.
    let generating = launch.at("generating")?;
    let running = &band(generating)["running"];
    ensure(
        running["sentence"]
            .as_str()
            .is_some_and(|sentence| sentence.starts_with("Generative fill \u{b7} candidate"))
            && running["fraction"].is_number()
            && record(generating)["lock"].is_string(),
        format!("The running generation: {}", band(generating)),
    )?;
    let cancelled = launch.at("cancelled")?;
    ensure(
        record(cancelled)["jobs"][0]["status"] == json!("cancelled")
            && record(cancelled)["lock"].is_null()
            && band(cancelled)["selection"] == json!(true),
        format!("Escape cancelled the generation: {}", record(cancelled)),
    )?;
    let generated = launch.at("generated")?;
    let strip = band(generated)["strip"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    ensure(
        strip.len() == 1 && strip[0]["chosen"] == json!(true),
        format!("Generate's strip: {}", band(generated)),
    )?;
    ensure(
        record(generated)["previews"].as_array().map(Vec::len) == Some(1),
        format!("Each preview read once: {}", record(generated)),
    )?;
    // The chosen candidate is previewed on the canvas: inside the selection the left quadrant's
    // colour becomes a mix of both quadrants', and outside it nothing changes.
    let chosen = launch.at("chosen")?;
    ensure(
        band(chosen)["strip"][0]["chosen"] == json!(true),
        format!("The first candidate chosen again: {}", band(chosen)),
    )?;
    checks.compare(
        chosen,
        "the chosen candidate over the selection, against the photograph",
        apart(rgb(chosen, INSIDE)?, rgb(opened, INSIDE)?),
        0.0,
        Tolerance::Above(NEAR),
    )?;
    checks.compare(
        chosen,
        "outside the selection, against the photograph",
        apart(rgb(chosen, OUTSIDE)?, rgb(opened, OUTSIDE)?),
        0.0,
        Tolerance::AtMost(NEAR),
    )?;
    let regenerated = launch.at("regenerated")?;
    let jobs = record(regenerated)["jobs"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    ensure(
        band(regenerated)["strip"].as_array().map(Vec::len) == Some(2)
            && jobs.len() == 3
            && jobs[2]["seed"] == jobs[1]["next_seed"],
        format!(
            "Regenerate adds one from the next seed: {}",
            record(regenerated)
        ),
    )?;
    // The second client's accept names the desktop's newest job.
    let accepted_by_agent = launch.at("agent-accepted")?;
    ensure(
        accepted_by_agent["step"]["resolved"]["job_id"] == jobs[2]["job_id"],
        "The agent's accept resolved to the desktop's newest job",
    )?;
    let prompt = launch.at("prompt")?;
    ensure(
        band(prompt)["action"] == json!("Replace")
            && band(prompt)["encoder"]["resource"] == json!("fake-text-encoder"),
        format!("The prompt without the encoder: {}", band(prompt)),
    )?;
    let replaced = launch.at("replaced")?;
    ensure(
        band(replaced)["encoder"].is_null()
            && band(replaced)["strip"]
                .as_array()
                .and_then(|strip| strip.last())
                .is_some_and(|cell| cell["label"] == json!(format!("Replace \"{PROMPT}\""))),
        format!("Replace's strip: {}", band(replaced)),
    )?;
    let sent = &record(replaced)["last_request"];
    ensure(
        sent["method"] == json!("task.fill-generative")
            && sent["params"]["prompt"] == json!(PROMPT),
        format!("Replace sent the prompt: {sent}"),
    )?;
    let accepted = launch.at("accepted")?;
    ensure(
        band(accepted)["selection"] == json!(false) && band(accepted)["strip"] == json!([]),
        format!("Accept clears the selection and strip: {}", band(accepted)),
    )?;
    checks.compare(
        accepted,
        "the accepted fill, against the photograph",
        apart(rgb(accepted, INSIDE)?, rgb(opened, INSIDE)?),
        0.0,
        Tolerance::Above(NEAR),
    )?;
    checks.write(
        run.out(),
        SCENARIO,
        json!({
            "near_codes": NEAR,
            "tinted_codes": TINTED,
            "scope": "Mean 8-bit RGB of small patches of the captured photograph over a flat four-quadrant fixture, read as changed or unchanged against the opened frame; the developer fake generator fills with its context's mean, so a change is legible, not a colorimetric or model-quality claim",
        }),
    )
}
