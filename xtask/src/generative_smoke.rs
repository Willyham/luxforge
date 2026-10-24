//! The `generative` smoke scenario: Remove, the Corrections band's generative fill
//! (`docs/design/ai-editing.md#desktop-surface`), on the real editor, background-only, with
//! correlated state, log and capture.
//!
//! One developer launch on the four-quadrant fixture. The tools panel has no AI editing section
//! (its setup is the Corrections band's). In the Corrections band Remove is offered and chosen, and
//! shows AI editing's setup with Set up, which asks through the setup sheet before the fake generator installs. The photograph is given a
//! global Basic edit, and a stroke across the boundary of two quadrants is the selection; Generate
//! is captured while it runs (the ring, the status bar's progress and the tools panel's notice that
//! the editor may be slow), Escape cancels it, Generate
//! runs to its one candidate, Regenerate adds one more, and the first is chosen again: each chosen
//! candidate is shown in the photograph as a candidate draft, through the Basic edit above it. A
//! second client accepts the desktop's newest job's candidate, after which the shown one, generated
//! against the photograph without it, is refused and the photograph is left unfilled. A prompt
//! makes the action Replace and offers the Replace add-on, whose Add asks through the setup sheet
//! in the feature's words; Replace then runs, its candidate is shown, and Accept commits it: the
//! accepted frame is the shown one. The fill of a flat-coloured context is
//! that context's mean, so a fill across two quadrants is legible in the capture: the left
//! quadrant's colour inside the selection becomes a mix of both.
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
pub const ABOUT: &str = "No AI editing section in the tools panel, and Remove in the Corrections band on the developer fake models: offered, AI editing's setup sheet and its consented download, a Basic edit and a painted selection, Generate's progress and Cancel, one candidate, Regenerate's second, a chosen candidate shown in the photograph as a candidate draft, a second client's accept refusing it, a prompt's Replace with the Replace add-on's setup sheet and download, its candidate shown and accepted as shown";

/// The developer AI module, which serves the fill tiers and their fake models.
const AI: &str = "luxforge.ai";
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
/// How far a shown candidate's frame and its accepted frame may be apart, per channel of a patch
/// mean in 8-bit codes: the same stack drawn twice, within the GPU's tolerance of the reference.
const MATCHED: f64 = 4.0;
/// The global Basic edit the candidates are shown and accepted under.
const DEVELOPED: [(&str, f64); 2] = [("exposure", 0.4), ("contrast", 20.0)];

fn remove(step: RemoveStep) -> script::Step {
    correction(CorrectionStep::Remove(step))
}

fn ai(action: CapabilityAction) -> CapabilityStep {
    CapabilityStep::new(AI, action)
}

/// Every frame, in order.
pub fn plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        // The tools panel with nothing installed: no AI editing section, since its setup is the
        // Corrections band's.
        Step::opened("opened").no_draft(),
        Step::new("mode", key("q")).commits(0).mode(MODULE),
        // Remove is offered; AI editing is not set up, so the band shows its setup.
        Step::new(
            "remove-model",
            correction(CorrectionStep::Kind("remove".into())),
        )
        .commits(0)
        .status_starts("Set up AI editing first"),
        // Set up asks first, through the setup sheet in the feature's words rather than a notice,
        // and the fake generator installs once allowed.
        Step::new("generator-asked", remove(RemoveStep::Setup))
            .commits(0)
            .no_notices(),
        Step::new("installed", ai(CapabilityAction::Consent(true)))
            .commits(0)
            .status_starts("Paint over what to remove"),
        Step::new("size", field("radius", SIZE)).commits(0),
        Step::new(
            "developed",
            script::Step::call(
                "edit.set-basic",
                Value::Object(
                    DEVELOPED
                        .iter()
                        .map(|(name, value)| ((*name).to_owned(), json!(value)))
                        .collect(),
                ),
            ),
        )
        .commits(1),
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
        // A prompt makes it Replace, which needs the Replace add-on: its Add is offered, asked for
        // through the consent notice and installed.
        Step::new("prompt", remove(RemoveStep::Prompt(PROMPT.into()))).commits(0),
        Step::new("encoder-asked", remove(RemoveStep::Setup))
            .commits(0)
            .no_notices(),
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

/// The frame shows `cell`'s candidate in the photograph: its candidate draft is the session's
/// draft, holding that candidate, and the photograph on screen was drawn from it.
fn shows(frame: &Frame, cell: &Value) -> Result {
    let draft = frame.draft();
    ensure(
        record(frame)["shown"] == json!([cell["job_id"], cell["candidate"]])
            && draft["action"] == json!("accept-correction-patch")
            && draft["fields"]["job_id"] == cell["job_id"]
            && draft["fields"]["candidate"] == cell["candidate"],
        format!(
            "The chosen candidate {cell} shown as a candidate draft: {draft} {}",
            record(frame)
        ),
    )?;
    frame.displays_draft()
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
    // The tools panel lists Basic but no AI editing section; the setup is still recorded, for the
    // Corrections band.
    let panel = launch.at("opened")?;
    let expanded = &panel.state()["expanded"];
    let setup = &panel.state()["capabilities"][AI]["setup"];
    ensure(
        !expanded["luxforge.basic"].is_null() && expanded[AI].is_null() && setup.is_object(),
        format!("The tools panel's sections {expanded} and AI editing's setup {setup}"),
    )?;
    // Remove is offered, not chosen.
    let on = launch.at("mode")?;
    ensure(
        band(on)["chosen"] == json!(false),
        format!("Remove offered: {}", band(on)),
    )?;
    let missing = launch.at("remove-model")?;
    ensure(
        band(missing)["unoffered"]["setup"]["action"] == json!("Set up")
            && band(missing)["unoffered"]["setup"]["setup"] == json!("remove"),
        format!("Remove without the model: {}", band(missing)),
    )?;
    // Set up and Add each ask through the setup sheet, in its feature's words: Remove in three
    // steps over the download's facts, Replace in one picture; once answered, neither stays.
    let asked = &launch.at("generator-asked")?.state()["setup_sheet"];
    ensure(
        asked["title"] == json!("Remove anything, fill it in naturally")
            && asked["steps"].as_array().map(Vec::len) == Some(3)
            && asked["steps"][0]["picture"] == json!("paint-over")
            && asked["facts"][0]["headline"]
                .as_str()
                .is_some_and(|fact| fact.ends_with(" download"))
            && asked["allow"]
                .as_str()
                .is_some_and(|allow| allow.starts_with("Download "))
            && asked["dismiss"] == json!("Not now"),
        format!("Remove's setup sheet: {asked}"),
    )?;
    let encoder = &launch.at("encoder-asked")?.state()["setup_sheet"];
    ensure(
        encoder["title"] == json!("Fill it with what you describe")
            && encoder["hero"] == json!("describe")
            && encoder["steps"] == json!([])
            && encoder["promise"] == json!("Your photos and what you type stay on this computer."),
        format!("Replace's setup sheet: {encoder}"),
    )?;
    for answered in ["installed", "encoder-installed"] {
        let sheet = &launch.at(answered)?.state()["setup_sheet"];
        ensure(
            sheet.is_null(),
            format!("The setup sheet after {answered}: {sheet}"),
        )?;
    }
    let installed = launch.at("installed")?;
    ensure(
        band(installed)["tiers"] == json!(["generative"])
            && band(installed)["unoffered"].is_null()
            && band(installed)["action"] == json!("Generate"),
        format!("The Generative tier once installed: {}", band(installed)),
    )?;
    // The draft selection is tinted on the canvas, over the developed photograph.
    let developed = launch.at("developed")?;
    let painted = launch.at("painted")?;
    ensure(
        band(painted)["selection"] == json!(true),
        format!("The painted selection: {}", band(painted)),
    )?;
    checks.compare(
        painted,
        "the selection's tint, against the photograph",
        apart(rgb(painted, INSIDE)?, rgb(developed, INSIDE)?),
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
    // The tools panel says the editor may be slow while it runs, and says nothing once it ends.
    let notice = &generating.state()["tools_notice"];
    ensure(
        notice.as_str().is_some_and(|notice| {
            notice == "Generative fill in progress. The editor may be slow while it runs."
        }),
        format!("The tools panel's notice while generating: {notice}"),
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
        generated.state()["tools_notice"].is_null(),
        format!(
            "The tools panel's notice once generated: {}",
            generated.state()["tools_notice"]
        ),
    )?;
    ensure(
        record(generated)["previews"].as_array().map(Vec::len) == Some(1),
        format!("Each preview read once: {}", record(generated)),
    )?;
    shows(generated, &strip[0])?;
    // The chosen candidate is shown in the photograph through the Basic edit above it: inside the
    // selection the left quadrant's colour becomes a mix of both quadrants', and outside it
    // nothing changes.
    let chosen = launch.at("chosen")?;
    ensure(
        band(chosen)["strip"][0]["chosen"] == json!(true),
        format!("The first candidate chosen again: {}", band(chosen)),
    )?;
    shows(chosen, &band(chosen)["strip"][0])?;
    checks.compare(
        chosen,
        "the chosen candidate over the selection, against the developed photograph",
        apart(rgb(chosen, INSIDE)?, rgb(developed, INSIDE)?),
        0.0,
        Tolerance::Above(NEAR),
    )?;
    checks.compare(
        chosen,
        "outside the selection, against the developed photograph",
        apart(rgb(chosen, OUTSIDE)?, rgb(developed, OUTSIDE)?),
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
    // The shown candidate was generated against the photograph without that correction: its
    // draft is refused, in the accept's words, and the photograph is left unfilled.
    let refused = record(accepted_by_agent);
    ensure(
        refused["shown"].is_null()
            && refused["unshown"] == json!([jobs[1]["job_id"], 0])
            && band(accepted_by_agent)["notice"]["code"] == json!("conflict")
            && accepted_by_agent.draft().is_null(),
        format!(
            "The shown candidate refused after the agent's accept: {refused} {}",
            band(accepted_by_agent)
        ),
    )?;
    let prompt = launch.at("prompt")?;
    ensure(
        band(prompt)["action"] == json!("Replace")
            && band(prompt)["addon"]["setup"] == json!("replace")
            && band(prompt)["addon"]["action"] == json!("Add"),
        format!("The prompt without the encoder: {}", band(prompt)),
    )?;
    let replaced = launch.at("replaced")?;
    ensure(
        band(replaced)["addon"].is_null()
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
    let replacing = band(replaced)["strip"]
        .as_array()
        .and_then(|strip| strip.last())
        .cloned()
        .unwrap_or_default();
    shows(replaced, &replacing)?;
    let accepted = launch.at("accepted")?;
    ensure(
        band(accepted)["selection"] == json!(false)
            && band(accepted)["strip"] == json!([])
            && accepted.draft().is_null(),
        format!("Accept clears the selection and strip: {}", band(accepted)),
    )?;
    let committed = &record(accepted)["last_request"];
    ensure(
        committed["method"] == json!("edit.accept-correction-patch")
            && committed["params"]["job_id"] == replacing["job_id"]
            && committed["params"]["candidate"] == replacing["candidate"],
        format!("Accept committed the shown candidate: {committed}"),
    )?;
    checks.compare(
        accepted,
        "the accepted fill, against the developed photograph",
        apart(rgb(accepted, INSIDE)?, rgb(developed, INSIDE)?),
        0.0,
        Tolerance::Above(NEAR),
    )?;
    // What was shown is what Accept committed: inside the selection and outside it.
    checks.compare(
        replaced,
        "the shown candidate inside the selection, against the accepted frame",
        apart(rgb(replaced, INSIDE)?, rgb(accepted, INSIDE)?),
        0.0,
        Tolerance::Within(MATCHED),
    )?;
    checks.compare(
        replaced,
        "the shown candidate outside the selection, against the accepted frame",
        apart(rgb(replaced, OUTSIDE)?, rgb(accepted, OUTSIDE)?),
        0.0,
        Tolerance::Within(MATCHED),
    )?;
    checks.write(
        run.out(),
        SCENARIO,
        json!({
            "near_codes": NEAR,
            "tinted_codes": TINTED,
            "matched_codes": MATCHED,
            "scope": "Mean 8-bit RGB of small patches of the captured photograph over a flat four-quadrant fixture, read as changed or unchanged against the frame after the Basic edit, and the shown candidate's frame against the accepted frame within the matched codes; the developer fake generator fills with its context's mean, so a change is legible, not a colorimetric or model-quality claim",
        }),
    )
}
