//! The `ai-setup` smoke scenario: setting up AI tools from `module.status` setups
//! (`docs/design/ai-tools-workspace.md#setting-up`) on the real editor, background-only, with
//! correlated state, log and capture, over the developer fake models.
//!
//! Two developer launches on the four-quadrant fixture. The first paces the fake downloads, so a
//! setup's progress can be captured, and cuts the Replace encoder's first transfer halfway. In the
//! Corrections band Remove's kind carries the download glyph and its group shows AI editing's
//! setup. Set up opens the setup sheet from the consent `module.setup.install` answered, and Not
//! now denies every scope; asked again, the sheet says it was declined. Download grants every scope
//! in one answer and installs the setup, captured while it downloads; Cancel cancels its job and
//! leaves the download paused, and Resume finishes it. A prompt offers the Replace add-on, whose
//! download is cut: its line says it didn't install, and Try again resumes and finishes it. With
//! `ai.analysis` on, Settings › Models lists the object finder as one part captioned with both
//! tools that use it; its Download… opens the sheet of the smallest setup holding it, which
//! installs both of the part's files under one progress, and its Remove removes both. The second
//! launch holds the model minimums against a forced 1 GB of memory: Remove's group says the
//! machine is too small, with no button, and the Models tab says so too.
use crate::{
    corrections_smoke::{MODULE, correction, key},
    scenario::{Checked, Frame, Plan, Step},
    *,
};
use luxforge_evidence::{
    self as script, CapabilityAction, CapabilityStep, CorrectionStep, RemoveStep,
};

pub const SCENARIO: &str = "ai-setup";
pub const ABOUT: &str = "Setting up AI tools from module.status setups on the developer fake models: Remove's kind marked and its group's setup line, Set up's sheet from the setup consent with its includes and the requests its buttons send, Not now denying every scope and the declined note, Download granting every scope in one answer, a paced download's progress, Cancel pausing it and Resume finishing it, the Replace add-on's cut download failing and resumed by Try again, Settings › Models' one row per part with its tools named, the object finder's Download… opening its smallest setup and installing both files under one progress, its Remove removing both, and a forced 1 GB machine too small for any of it";

/// The first launch: the paced, cut setups.
pub const SETUP: &str = "setup";
/// The second launch: the machine too small.
pub const SMALL: &str = "small";
/// The memory the second launch holds the minimums against, below the fakes' 2 GB.
pub const SMALL_MEMORY: u64 = 1 << 30;
/// The developer resource whose first transfer the first launch cuts.
pub const CUT: &str = "fake-text-encoder";
pub const NOTE: &str = "The `setup` launch paces the developer fake downloads (`--evidence-paced-downloads`) and cuts the Replace encoder's first transfer halfway (`--evidence-cut-download fake-text-encoder`); the `small` launch holds the model minimums against 1 GB of memory (`--evidence-machine-memory 1073741824`).";

const AI: &str = "luxforge.ai";
const PROMPT: &str = "a red bicycle";
const FINDER: [&str; 2] = ["fake-segmenter-encoder", "fake-segmenter-decoder"];

fn remove(step: RemoveStep) -> script::Step {
    correction(CorrectionStep::Remove(step))
}

fn ai(name: &str, action: CapabilityAction) -> Step {
    Step::new(name, CapabilityStep::new(AI, action)).commits(0)
}

fn ai_running(name: &str, action: CapabilityAction) -> Step {
    Step::new(name, CapabilityStep::new(AI, action).no_wait()).commits(0)
}

/// The paced, cut setups, in order.
pub fn setup_plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("opened").no_draft(),
        Step::new("mode", key("q")).commits(0).mode(MODULE),
        Step::new("missing", correction(CorrectionStep::Kind("remove".into())))
            .commits(0)
            .status_starts("Set up AI editing first"),
        Step::new("asked", remove(RemoveStep::Setup))
            .commits(0)
            .no_notices(),
        ai("declined", CapabilityAction::Consent(false)),
        Step::new("asked-again", remove(RemoveStep::Setup))
            .commits(0)
            .no_notices(),
        ai_running("downloading", CapabilityAction::Consent(true)),
        Step::new("cancelling", remove(RemoveStep::Setup)).commits(0),
        ai("paused", CapabilityAction::Settle),
        Step::new("resuming", remove(RemoveStep::Setup)).commits(0),
        ai("installed", CapabilityAction::Settle),
        Step::new("prompt", remove(RemoveStep::Prompt(PROMPT.into()))).commits(0),
        Step::new("replace-asked", remove(RemoveStep::Setup))
            .commits(0)
            .no_notices(),
        ai("replace-failed", CapabilityAction::Consent(true)),
        Step::new("replace-retrying", remove(RemoveStep::Setup)).commits(0),
        ai("replace-installed", CapabilityAction::Settle),
        Step::new(
            "analysis",
            script::Step::agent("flags.set", json!({"flag": "ai.analysis", "value": true})),
        )
        .commits(0),
        Step::new("models", script::Step::settings_tab("models")).commits(0),
        ai("finder-asked", CapabilityAction::Download(FINDER[0].into())),
        ai_running("finder-downloading", CapabilityAction::Consent(true)),
        ai("finder-installed", CapabilityAction::Settle),
        ai("finder-removed", CapabilityAction::Remove(FINDER[1].into())),
    ])
}

/// The machine too small, in order.
pub fn small_plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("opened").no_draft(),
        Step::new("mode", key("q")).commits(0).mode(MODULE),
        Step::new(
            "too-small",
            correction(CorrectionStep::Kind("remove".into())),
        )
        .commits(0),
        Step::new("models", script::Step::settings_tab("models")).commits(0),
    ])
}

/// Remove's band model in a frame.
fn band(frame: &Frame) -> &Value {
    &frame.state()["corrections"]["band"]["remove"]
}

/// Remove's main setup line in a frame.
fn main_line(frame: &Frame) -> &Value {
    &band(frame)["unoffered"]["setup"]
}

/// The line the desktop derives for `setup` in a frame.
fn line<'a>(frame: &'a Frame, setup: &str) -> &'a Value {
    &frame.state()["capabilities"][AI]["setups"][setup]
}

fn sheet(frame: &Frame) -> &Value {
    &frame.state()["setup_sheet"]
}

/// The methods of a list of requests.
fn methods(requests: &Value) -> Vec<&str> {
    requests
        .as_array()
        .map(|requests| {
            requests
                .iter()
                .filter_map(|request| request["method"].as_str())
                .collect()
        })
        .unwrap_or_default()
}

/// The Models tab's row titled `title`.
fn model_row<'a>(frame: &'a Frame, title: &str) -> Result<&'a Value> {
    let tab = &frame.state()["settings"]["models"];
    tab["rows"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["module_id"] == AI && row["title"] == title)
        })
        .ok_or_else(|| format!("The Models tab lists no {title} row: {tab}").into())
}

/// The fill module's resource states in a frame's Models tab, by id.
fn resource_state<'a>(frame: &'a Frame, id: &str) -> &'a Value {
    frame.state()["settings"]["models"]["status"][AI]["resources"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == id))
        .map_or(&Value::Null, |row| &row["state"])
}

pub fn verify(_: &mut crate::scenario::Run, launches: &[Checked]) -> Result {
    let [setup, small] = launches else {
        return Err(format!("Expected two launches, found {}", launches.len()).into());
    };
    setting_up(setup)?;
    too_small(small)
}

fn setting_up(launch: &Checked) -> Result {
    // Missing: the group's line in Remove's own words, its kind marked with the download glyph.
    let missing = launch.at("missing")?;
    let main = main_line(missing);
    ensure(
        band(missing)["needs_setup"] == json!(true)
            && main["setup"] == "remove"
            && main["state"] == "missing"
            && main["action"] == "Set up"
            && main["detail"]
                .as_str()
                .is_some_and(|detail| detail.starts_with("One-time download \u{b7} "))
            && methods(&main["requests"]) == ["module.setup.install"]
            && main["requests"][0]["params"]["setup"] == "remove",
        format!("Remove's missing setup: {}", band(missing)),
    )?;
    // The sheet is opened from the setup's consent: its includes, and the requests each button
    // sends, which are what Copy as JSON request copies.
    let asked = sheet(launch.at("asked")?);
    ensure(
        asked["setup"] == "remove"
            && asked["title"] == "Remove anything, fill it in naturally"
            && asked["includes"].as_array().map(Vec::len) == Some(1)
            && asked["includes"][0]["name"] == "Generative fill"
            && asked["includes"][0]["installed"] == json!(false)
            && methods(&asked["requests"]["allow"])
                == ["module.permission.grant", "module.setup.install"]
            && methods(&asked["requests"]["dismiss"]) == ["module.permission.deny"]
            && asked["dismiss"] == "Not now",
        format!("Remove's setup sheet: {asked}"),
    )?;
    let declined = launch.at("declined")?;
    ensure(
        sheet(declined).is_null() && main_line(declined)["state"] == "missing",
        format!(
            "Not now closes the sheet and leaves the setup missing: {}",
            main_line(declined)
        ),
    )?;
    let again = sheet(launch.at("asked-again")?);
    ensure(
        again["notes"].as_array().is_some_and(|notes| {
            notes
                .iter()
                .any(|note| note == "You chose Not now before. Nothing has been downloaded.")
        }),
        format!("The sheet asked again says it was declined: {again}"),
    )?;
    // Downloading: the line's progress and Cancel, which cancels every job of the setup.
    let downloading = main_line(launch.at("downloading")?);
    ensure(
        downloading["state"] == "installing"
            && downloading["action"] == "Cancel"
            && downloading["progress"].is_number()
            && methods(&downloading["requests"]) == ["job.cancel"],
        format!("Remove's setup downloading: {downloading}"),
    )?;
    let paused = launch.at("paused")?;
    let line_paused = main_line(paused);
    ensure(
        line_paused["state"] == "missing" && line_paused["action"] == "Resume",
        format!("Cancel pauses the download, which Resume continues: {line_paused}"),
    )?;
    let installed = launch.at("installed")?;
    ensure(
        band(installed)["unoffered"].is_null()
            && band(installed)["needs_setup"] == json!(false)
            && band(installed)["tiers"] == json!(["generative"])
            && line(installed, "remove")["state"] == "installed",
        format!("Remove once set up: {}", band(installed)),
    )?;
    // The Replace add-on: its sheet in its own words, its cut download failing, Try again.
    let prompt = launch.at("prompt")?;
    ensure(
        band(prompt)["addon"]["setup"] == "replace" && band(prompt)["addon"]["action"] == "Add",
        format!("The prompt offers Replace: {}", band(prompt)),
    )?;
    let replace = sheet(launch.at("replace-asked")?);
    ensure(
        replace["setup"] == "replace"
            && replace["title"] == "Fill it with what you describe"
            && replace["includes"][0]["name"] == "Replace",
        format!("Replace's setup sheet: {replace}"),
    )?;
    let failed = &band(launch.at("replace-failed")?)["addon"];
    ensure(
        failed["state"] == "failed"
            && failed["action"] == "Try again"
            && failed["text"].as_str().is_some_and(|text| {
                text == "Replace didn't install: the connection to developer.luxforge.invalid was lost"
            }),
        format!("Replace's cut download: {failed}"),
    )?;
    let replaced = launch.at("replace-installed")?;
    ensure(
        band(replaced)["addon"].is_null() && line(replaced, "replace")["state"] == "installed",
        format!("Replace resumed and installed: {}", band(replaced)),
    )?;
    // Settings › Models: one row per part, the object finder listed with ai.analysis on and
    // captioned with both tools that use it.
    let models = launch.at("models")?;
    let finder = model_row(models, "Object finder")?;
    let fill = model_row(models, "Generative fill")?;
    ensure(
        finder["resources"] == json!(FINDER)
            && finder["caption"] == "Corrections \u{203a} Object, Masks \u{203a} Object"
            && finder["download"]["setup"] == "mask-object"
            && fill["caption"] == "Corrections \u{203a} Remove, Corrections \u{203a} Object"
            && fill["state"] == "Installed"
            && fill["download"].is_null(),
        format!("The Models tab's parts: {finder} {fill}"),
    )?;
    let asked = sheet(launch.at("finder-asked")?);
    ensure(
        asked["setup"] == "mask-object"
            && asked["title"] == "Select an object by pointing at it"
            && asked["includes"][0]["name"] == "Object finder"
            && methods(&asked["requests"]["allow"])
                == [
                    "module.permission.grant",
                    "module.permission.grant",
                    "module.setup.install",
                ],
        format!("The object finder's Download… sheet: {asked}"),
    )?;
    // Both files of the part under one progress, and the Object setup's other part ready.
    let downloading = launch.at("finder-downloading")?;
    let mask = line(downloading, "mask-object");
    let object = line(downloading, "object");
    let row = model_row(downloading, "Object finder")?;
    ensure(
        mask["state"] == "installing"
            && mask["progress"].is_number()
            && object["state"] == "installing"
            && object["parts"]
                == json!([
                    {"name": "Object finder", "text": object["parts"][0]["text"], "ready": false},
                    {"name": "Generative fill", "text": "Ready \u{2713}", "ready": true},
                ])
            && row["state"]
                .as_str()
                .is_some_and(|state| state.starts_with("Installing")),
        format!("The object finder downloading: {mask} {object} {row}"),
    )?;
    let finder_installed = launch.at("finder-installed")?;
    ensure(
        model_row(finder_installed, "Object finder")?["state"] == "Installed"
            && FINDER
                .iter()
                .all(|id| resource_state(finder_installed, id) == "installed")
            && methods(&model_row(finder_installed, "Object finder")?["remove_requests"])
                == ["module.resource.remove", "module.resource.remove"],
        format!(
            "The object finder installed: {}",
            model_row(finder_installed, "Object finder")?
        ),
    )?;
    let removed = launch.at("finder-removed")?;
    let row = model_row(removed, "Object finder")?;
    ensure(
        FINDER
            .iter()
            .all(|id| resource_state(removed, id) == "not-installed")
            && row["download"]["setup"] == "mask-object",
        format!("The part's Remove removed both files: {row}"),
    )
}

fn too_small(launch: &Checked) -> Result {
    let small = launch.at("too-small")?;
    let main = main_line(small);
    ensure(
        band(small)["needs_setup"] == json!(true)
            && main["state"] == "cannot-run"
            && main["action"].is_null()
            && main["text"].as_str().is_some_and(|text| {
                text.starts_with("AI editing needs a computer with 2 GB of memory. This ")
                    && text.ends_with(" has 1 GB.")
            }),
        format!("Remove on a machine too small: {main}"),
    )?;
    let models = launch.at("models")?;
    let fill = model_row(models, "Generative fill")?;
    let machine = models.state()["settings"]["models"]["machine"]
        .as_str()
        .unwrap_or_default();
    ensure(
        fill["fits"] == json!(false)
            && fill["download"].is_null()
            && machine.starts_with("This machine has 1 GB of memory. ")
            && machine.contains("Generative fill needs 2 GB, more than it has"),
        format!("The Models tab on a machine too small: {fill} {machine}"),
    )
}
