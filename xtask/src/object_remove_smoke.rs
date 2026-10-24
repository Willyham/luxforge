//! The `object-remove` smoke scenario: Corrections › Object (`docs/design/ai-tools-workspace.md`,
//! "Object") on the real editor, background-only, with correlated state, log and capture, over the
//! developer fake segmenter, generator and runtime.
//!
//! One developer launch on the generated twelve-patch fixture (`fixtures/generated/range.jpg`),
//! whose flat patches the fake decoder finds as objects one by one, with paced downloads. A second
//! client turns on `ai.analysis`; in the Corrections band Object is chosen and, without its setup,
//! its segment is marked and its group shows the setup line. Set up opens the setup sheet, whose
//! one answer grants every scope; the setup's three files download under one progress bar, captured
//! while they run, and once installed the session loads the object finder and analyses the
//! photograph. A hover outlines the orange patch and a click selects it, tinted and numbered; a
//! loose U-shaped drag through the white and grey patches is answered Several, two numbered pins,
//! the second chosen and used, which replaces the selection. A short drag inside the black patch
//! finds nothing: its footprint stays tinted under Nothing found here, and Return hands the stroke
//! to Remove as its painted selection. Object again: orange clicked, red and white ⇧-clicked, white
//! ⌥-clicked away with nothing sent; Paint shadow arms the brush, and a stroke in the light grey
//! patch below is the shadow. Generate is captured while it runs, Escape cancels it, Generate runs
//! to its candidate and Accept commits one entry. Then the Escape order: pins first, then the
//! selection, then the mode.
use crate::{
    corrections_smoke::{MODULE, PATCH_HALF, correction, key},
    scenario::{Checked, Checks, Frame, Plan, Step, Tolerance, plan::only},
    *,
};
use luxforge_evidence::{
    self as script, CapabilityAction, CapabilityStep, CorrectionStep, MaskStep, ObjectStep,
    RemoveStep, RemoveUntil,
};

pub const SCENARIO: &str = "object-remove";
pub const ABOUT: &str = "Corrections › Object on the developer fake segmenter and generator: Object offered once ai.analysis is on, marked and showing its setup line without its setup, the setup sheet's one answer granting every scope and its files under one progress bar, the session's loads and one analysis, a hover outline, a click's tinted and numbered selection, a loose drag's pins and Use, Nothing found's tinted footprint and its hand-off to Remove, ⇧-click adding and ⌥-click taking away with nothing sent, Paint shadow's stroke, Generate's progress, Escape cancelling it, Generate's candidate and Accept's one entry, and the Escape order";
pub const NOTE: &str = "Generate the fixture first with `cargo xtask generate-fixtures --output fixtures/generated`. The launch paces the developer fake downloads (`--evidence-paced-downloads`), so the setup's progress can be captured.";
pub const FIXTURE: &str = "fixtures/generated/range.jpg";

const AI: &str = "luxforge.ai";
/// The setup Corrections › Object needs.
const SETUP: &str = "object";
/// Patch centres of the four-by-three fixture.
const WHITE: [f64; 2] = [0.125, 0.5];
const ORANGE: [f64; 2] = [0.875, 0.5];
const RED: [f64; 2] = [0.875, 0.833];
/// The light grey patch below the black one, where the shadow is painted.
const SHADE: [f64; 2] = [0.625, 0.833];
/// A loose drag down the white patch, across and up the grey one: Several, two pins.
const U: [[f64; 2]; 4] = [[0.125, 0.4], [0.125, 0.6], [0.375, 0.6], [0.375, 0.4]];
/// A short drag down the black patch into the light grey one below it: the stroke's box finds the
/// black part above the edge, which explains too little of the stroke to be the object, and each
/// whole patch has too little of itself under the stroke to be a target, so Nothing.
const SHORT: [[f64; 2]; 2] = [[0.625, 0.6], [0.625, 0.7]];
/// The shadow stroke, across the light grey patch.
const SHADOW: [[f64; 2]; 3] = [[0.57, 0.833], [0.625, 0.833], [0.68, 0.833]];
/// How far a selected object's translucent tint moves a patch, at least, per channel in 8-bit
/// codes: it is drawn over the photograph, not instead of it.
const TINTED: f64 = 8.0;
/// How far an untouched patch may move between two frames, per channel in 8-bit codes.
const STILL: f64 = 3.0;
const LABEL: &str = "Add Generative fill 1";

fn object(step: ObjectStep) -> script::Step {
    MaskStep::Object(step).into()
}

fn remove(step: RemoveStep) -> script::Step {
    correction(CorrectionStep::Remove(step))
}

fn quiet(name: &str, step: impl Into<script::Step>) -> Step {
    Step::new(name, step).commits(0).mode(MODULE)
}

fn ai(name: &str, action: CapabilityAction) -> Step {
    Step::new(name, CapabilityStep::new(AI, action)).commits(0)
}

/// Every frame, in order.
pub fn plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("opened").no_draft(),
        Step::new(
            "analysis",
            script::Step::agent("flags.set", json!({"flag": "ai.analysis", "value": true})),
        )
        .commits(0),
        quiet("mode", key("q")),
        // Without its setup: the segment marked, the group's setup line.
        quiet("object", correction(CorrectionStep::Kind(SETUP.into()))),
        quiet("asked", remove(RemoveStep::Setup)).no_notices(),
        Step::new(
            "downloading",
            CapabilityStep::new(AI, CapabilityAction::Consent(true)).no_wait(),
        )
        .commits(0),
        ai("installed", CapabilityAction::Settle),
        // Hover, click: one object selected, tinted and numbered.
        quiet("hovered", object(ObjectStep::Hover(Some(ORANGE)))),
        quiet("clicked", object(ObjectStep::Click(ORANGE))),
        // A loose drag: two pins, the second chosen and used, which replaces the selection.
        quiet("pins", object(ObjectStep::Drag(U.to_vec()))),
        quiet("pin-chosen", object(ObjectStep::Pin(2))),
        quiet("used", object(ObjectStep::Use)),
        // A drag that finds nothing, handed to Remove.
        quiet("nothing", object(ObjectStep::Drag(SHORT.to_vec()))),
        quiet("handed-off", object(ObjectStep::Done)),
        // Object again: orange, ⇧ red and white, ⌥ white away.
        quiet(
            "object-again",
            correction(CorrectionStep::Kind(SETUP.into())),
        ),
        quiet("orange-hovered", object(ObjectStep::Hover(Some(ORANGE)))),
        quiet("orange", object(ObjectStep::Click(ORANGE))),
        quiet("red-hovered", object(ObjectStep::Hover(Some(RED)))),
        quiet("red", object(ObjectStep::ShiftClick(RED))),
        quiet("white-hovered", object(ObjectStep::Hover(Some(WHITE)))),
        quiet("white", object(ObjectStep::ShiftClick(WHITE))),
        quiet("taken", object(ObjectStep::AltClick(WHITE))),
        // Paint shadow: one stroke, the effect region.
        quiet("shadow-armed", remove(RemoveStep::Shadow)),
        quiet("shadow", object(ObjectStep::Drag(SHADOW.to_vec()))),
        quiet(
            "generating",
            remove(RemoveStep::Generate(RemoveUntil::Progress)),
        )
        .status_starts("Generative fill \u{b7} candidate"),
        quiet("cancelled", key("Escape")).status_starts("Generation cancelled"),
        quiet("generated", remove(RemoveStep::Generate(RemoveUntil::Done)))
            .status_starts("1 candidate"),
        Step::new("accepted", remove(RemoveStep::Accept))
            .commits(1)
            .label(LABEL)
            .mode(MODULE)
            .status_starts("Accepted"),
        // The Escape order: pins, then the selection, then the mode.
        quiet("last-hovered", object(ObjectStep::Hover(Some(ORANGE)))),
        quiet("last", object(ObjectStep::Click(ORANGE))),
        quiet("last-pins", object(ObjectStep::Drag(U.to_vec()))),
        quiet("escape-pins", key("Escape")),
        quiet("escape-selection", key("Escape")).status_starts("Selection cleared"),
        Step::new("escape-mode", key("Escape"))
            .commits(0)
            .mode("pointer"),
    ])
}

/// Remove's band model in a frame, with Object's group.
fn band(frame: &Frame) -> &Value {
    &frame.state()["corrections"]["band"]["remove"]
}

/// Object's own record in a frame: the kind, the selection and its shadow.
fn record(frame: &Frame) -> &Value {
    &frame.state()["corrections"]["remove"]["object"]
}

/// Remove's own record in a frame: its selection, jobs and last request.
fn removing(frame: &Frame) -> &Value {
    &frame.state()["corrections"]["remove"]
}

fn session(frame: &Frame) -> &Value {
    &frame.state()["object_pick"]["session"]
}

fn selects(frame: &Frame) -> &Value {
    &frame.state()["object_pick"]["selects"]
}

/// The frame shows the canvas notice titled `title`.
fn notice(frame: &Frame, title: &str) -> bool {
    frame.notices().iter().any(|shown| shown == title)
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

/// The selected objects' prompts in a frame, in pick order, each with its artifact held.
fn objects(frame: &Frame) -> Result<Vec<Value>> {
    let objects = record(frame)["objects"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for (index, object) in objects.iter().enumerate() {
        ensure(
            object["number"] == json!(index + 1) && object["artifact"].is_string(),
            format!(
                "Object {} numbered in pick order, its artifact held: {object}",
                index + 1
            ),
        )?;
    }
    Ok(objects
        .into_iter()
        .map(|object| object["prompt"].clone())
        .collect())
}

fn point(at: [f64; 2]) -> Value {
    json!({"point": [at]})
}

pub fn verify(run: &mut crate::scenario::Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let mut checks = Checks::new();
    setting_up(launch)?;
    picking(launch, &mut checks)?;
    removing_them(launch, &mut checks)?;
    escaping(launch)?;
    checks.write(
        run.out(),
        SCENARIO,
        json!({
            "tinted_codes": TINTED,
            "still_codes": STILL,
            "scope": "State, history, log and mean 8-bit RGB of small patches of the captured photograph over the generated twelve-patch fixture with the developer fake segmenter, whose masks follow colour similarity so an object is a flat patch, and the fake generator: a wiring, parity and overlay claim, not a model-quality or latency claim",
        }),
    )
}

/// Object offered, its setup asked for in one answer and downloaded under one bar, and the session
/// armed once installed.
fn setting_up(launch: &Checked) -> Result {
    let chosen = launch.at("object")?;
    let setup = &band(chosen)["object"]["setup"];
    ensure(
        band(chosen)["object_offered"] == json!(true)
            && band(chosen)["object_needs_setup"] == json!(true)
            && record(chosen)["kind"] == json!("object")
            && setup["setup"] == json!(SETUP)
            && setup["action"]
                .as_str()
                .is_some_and(|action| action.starts_with("Set up"))
            && session(chosen).is_null(),
        format!("Object without its setup: {}", band(chosen)),
    )?;
    let sheet = &launch.at("asked")?.state()["setup_sheet"];
    let allow: Vec<&str> = sheet["requests"]["allow"]
        .as_array()
        .map(|requests| {
            requests
                .iter()
                .filter_map(|request| request["method"].as_str())
                .collect()
        })
        .unwrap_or_default();
    ensure(
        sheet["setup"] == json!(SETUP)
            && allow
                == [
                    "module.permission.grant",
                    "module.permission.grant",
                    "module.permission.grant",
                    "module.setup.install",
                ]
            && sheet["allow"]
                .as_str()
                .is_some_and(|allow| allow.starts_with("Download "))
            && sheet["includes"].as_array().map(Vec::len) == Some(2),
        format!("Object's setup sheet, its one Download granting each file's scope: {sheet}"),
    )?;
    let downloading = launch.at("downloading")?;
    let line = &downloading.state()["capabilities"][AI]["setups"][SETUP];
    ensure(
        downloading.state()["setup_sheet"].is_null()
            && line["state"] == json!("installing")
            && line["progress"].is_number()
            && band(downloading)["object"]["setup"]["progress"] == line["progress"],
        format!("The setup's files under one progress bar: {line}"),
    )?;
    let installed = launch.at("installed")?;
    let armed = session(installed);
    ensure(
        band(installed)["object_needs_setup"] == json!(false)
            && band(installed)["object"]["setup"].is_null()
            && armed["purpose"] == json!("object")
            && armed["analyses"] == json!(1)
            && armed["analysis_error"].is_null(),
        format!("The session armed once installed: {armed}"),
    )?;
    Ok(())
}

/// Hover, click, pins and Use, and Nothing's hand-off.
fn picking(launch: &Checked, checks: &mut Checks) -> Result {
    let installed = launch.at("installed")?;
    let hovered = launch.at("hovered")?;
    ensure(
        session(hovered)["last_request"]["params"]["point"] == json!([ORANGE])
            && session(hovered)["overlay"]["bounds"].is_array()
            && session(hovered)["analyses"] == json!(1),
        format!("The hover's pick: {}", session(hovered)),
    )?;
    let clicked = launch.at("clicked")?;
    ensure(
        objects(clicked)? == [point(ORANGE)]
            && band(clicked)["object"]["rows"] == json!([{"number": 1, "selecting": false}])
            && selects(clicked)["last_request"]["params"]["point"] == json!([ORANGE])
            && band(clicked)["selection"] == json!(true),
        format!(
            "The click's selection: {} {}",
            record(clicked),
            band(clicked)
        ),
    )?;
    checks.compare(
        clicked,
        "the selected orange patch's tint, against the photograph before it",
        apart(rgb(clicked, ORANGE)?, rgb(installed, ORANGE)?),
        0.0,
        Tolerance::Above(TINTED),
    )?;
    checks.compare(
        clicked,
        "the red patch beside it, against the photograph before it",
        apart(rgb(clicked, RED)?, rgb(installed, RED)?),
        0.0,
        Tolerance::AtMost(STILL),
    )?;
    let pins = launch.at("pins")?;
    let pinned = &session(pins)["pins"];
    ensure(
        session(pins)["last_request"]["params"]["previews"] == json!("all")
            && pinned.as_array().map(Vec::len) == Some(2)
            && notice(pins, "2 objects found")
            && objects(pins)? == [point(ORANGE)],
        format!("The drag's pins, the selection kept: {pinned}"),
    )?;
    let chosen = launch.at("pin-chosen")?;
    let candidate = session(chosen)["pins"][1]["candidate"].clone();
    ensure(
        session(chosen)["pins"][1]["chosen"] == json!(true),
        format!("Pin 2 chosen: {}", session(chosen)["pins"]),
    )?;
    let used = launch.at("used")?;
    let prompts = objects(used)?;
    ensure(
        prompts.len() == 1
            && prompts[0]["candidate"] == candidate
            && session(used)["pins"].is_null(),
        format!("Use replaced the selection with the chosen pin: {prompts:?}"),
    )?;
    let nothing = launch.at("nothing")?;
    let hint = &band(nothing)["hint"];
    ensure(
        session(nothing)["nothing"].is_number()
            && notice(nothing, "Nothing found here")
            && hint.as_str().is_some_and(|hint| {
                hint.starts_with("Nothing found here \u{b7} Remove the painted area")
            }),
        format!("Nothing found, the hand-off offered: {hint}"),
    )?;
    checks.compare(
        nothing,
        "the nothing stroke's tinted footprint in the black patch",
        apart(rgb(nothing, [0.625, 0.62])?, rgb(installed, [0.625, 0.62])?),
        0.0,
        Tolerance::Above(TINTED),
    )?;
    let handed = launch.at("handed-off")?;
    ensure(
        record(handed)["kind"] == json!("remove")
            && band(handed)["selection"] == json!(true)
            && removing(handed)["selection"]["points"]
                .as_u64()
                .is_some_and(|n| n >= 2)
            && session(handed).is_null(),
        format!(
            "The stroke handed to Remove: {} {}",
            record(handed),
            removing(handed)
        ),
    )?;
    let again = launch.at("object-again")?;
    ensure(
        record(again)["kind"] == json!("object")
            && band(again)["selection"] == json!(false)
            && session(again)["purpose"] == json!("object"),
        format!("Switching kinds clears the selection: {}", band(again)),
    )?;
    Ok(())
}

/// ⇧ adds, ⌥ takes away, Paint shadow, Generate, Escape's cancel and Accept's one entry.
fn removing_them(launch: &Checked, checks: &mut Checks) -> Result {
    let red = launch.at("red")?;
    ensure(
        objects(red)? == [point(ORANGE), point(RED)],
        format!("⇧-click added red: {}", record(red)),
    )?;
    let white = launch.at("white")?;
    ensure(
        objects(white)? == [point(ORANGE), point(RED), point(WHITE)]
            && band(white)["object"]["rows"].as_array().map(Vec::len) == Some(3),
        format!("⇧-click added white: {}", record(white)),
    )?;
    let taken = launch.at("taken")?;
    ensure(
        objects(taken)? == [point(ORANGE), point(RED)]
            && selects(taken)["sent"] == selects(white)["sent"],
        format!(
            "⌥-click took white away with nothing sent: {} {}",
            record(taken),
            selects(taken)
        ),
    )?;
    checks.compare(
        taken,
        "the white patch once taken away, against it hovered before it was picked",
        apart(rgb(taken, WHITE)?, rgb(launch.at("white-hovered")?, WHITE)?),
        0.0,
        Tolerance::AtMost(STILL),
    )?;
    let armed = launch.at("shadow-armed")?;
    ensure(
        record(armed)["shadow_armed"] == json!(true)
            && band(armed)["object"]["shadow_armed"] == json!(true),
        format!("Paint shadow armed: {}", record(armed)),
    )?;
    let shadow = launch.at("shadow")?;
    ensure(
        record(shadow)["shadow_armed"] == json!(false)
            && record(shadow)["effect"]["points"]
                .as_u64()
                .is_some_and(|n| n >= 2)
            && objects(shadow)? == [point(ORANGE), point(RED)],
        format!("The shadow painted: {}", record(shadow)),
    )?;
    checks.compare(
        shadow,
        "the painted shadow's tint, against the photograph before it",
        apart(rgb(shadow, SHADE)?, rgb(armed, SHADE)?),
        0.0,
        Tolerance::Above(TINTED),
    )?;
    let generating = launch.at("generating")?;
    let sent = &removing(generating)["last_request"];
    let artifacts: Vec<&Value> = record(shadow)["objects"]
        .as_array()
        .map(|objects| objects.iter().map(|object| &object["artifact"]).collect())
        .unwrap_or_default();
    ensure(
        sent["method"] == json!("task.fill-generative")
            && sent["params"]["object-artifacts"]
                .as_array()
                .is_some_and(|sent| sent.iter().collect::<Vec<_>>() == artifacts)
            && sent["params"]["effect-points"].is_array()
            && sent["params"]["effect-radius"].is_number()
            && sent["params"].get("points").is_none()
            && band(generating)["running"]["fraction"].is_number(),
        format!("Generate named both objects and the shadow: {sent}"),
    )?;
    let cancelled = launch.at("cancelled")?;
    ensure(
        band(cancelled)["running"].is_null() && band(cancelled)["selection"] == json!(true),
        format!("Escape cancelled the generation: {}", band(cancelled)),
    )?;
    let generated = launch.at("generated")?;
    ensure(
        band(generated)["strip"].as_array().map(Vec::len) == Some(1)
            && removing(generated)["shown"].is_array(),
        format!("Generate's candidate shown: {}", band(generated)),
    )?;
    let accepted = launch.at("accepted")?;
    ensure(
        band(accepted)["selection"] == json!(false)
            && band(accepted)["strip"] == json!([])
            && removing(accepted)["last_request"]["method"]
                == json!("edit.accept-correction-patch"),
        format!("Accept committed the shown candidate: {}", band(accepted)),
    )?;
    Ok(())
}

/// Escape: pins, then the selection, then the mode.
fn escaping(launch: &Checked) -> Result {
    let pins = launch.at("last-pins")?;
    ensure(
        session(pins)["pins"].as_array().map(Vec::len) == Some(2) && objects(pins)?.len() == 1,
        format!("Pins over a selection: {}", session(pins)),
    )?;
    let first = launch.at("escape-pins")?;
    ensure(
        session(first)["pins"].is_null() && objects(first)?.len() == 1,
        format!("The first Escape puts the pins down: {}", session(first)),
    )?;
    let second = launch.at("escape-selection")?;
    ensure(
        objects(second)?.is_empty() && session(second)["purpose"] == json!("object"),
        format!("The second Escape clears the selection: {}", record(second)),
    )?;
    let third = launch.at("escape-mode")?;
    ensure(
        session(third).is_null(),
        format!("The third Escape leaves the mode: {}", session(third)),
    )
}
