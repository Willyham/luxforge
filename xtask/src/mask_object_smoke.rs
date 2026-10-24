//! The `mask-object` smoke scenario: Masks › Object (`docs/design/ai-tools-workspace.md`, "Object in
//! the Masks panel") on the real editor, background-only, with correlated state, log and capture,
//! over the developer fake segmenter.
//!
//! One developer launch on the four-quadrant fixture, whose fake decoder finds each quadrant as one
//! object. A second client turns on `ai.analysis`, which lists Object… in the kind menus' Select
//! group. Chosen without the object finder, it starts `mask-object`'s setup, whose consent is the
//! setup sheet; Download installs it. Chosen again it enters picking: both models load and the
//! photograph is analysed once. A hover outlines the top-left quadrant and a click commits one
//! `mask.create-model` entry, `Add object (model)`; picking stays armed. A loose U-shaped drag
//! through the two top quadrants is answered Several: two numbered pins, none chosen yet; the second
//! is chosen, and Use commits it into the same mask. Escape leaves picking. Add component › Object…
//! adds the bottom-left quadrant. A second client's removal under the masks stales each model row
//! with its reason in plain words; Keep hides the first row's note and sends nothing; Recompute on
//! the second sends its stored prompt again and replaces it with one `mask.set-model` entry.
use crate::{
    corrections_smoke::{NEAR, PATCH_HALF},
    scenario::{Checked, Checks, Frame, Plan, Step, Tolerance, plan::only},
    *,
};
use luxforge_evidence::{
    self as script, CapabilityAction, CapabilityStep, KindMenuStep, MaskStep, ObjectStep,
    Reference, WorkspaceStep,
};

pub const SCENARIO: &str = "mask-object";
pub const ABOUT: &str = "Masks › Object on the developer fake segmenter: Object… in the Select group once ai.analysis is on, its setup sheet and download, picking entered with both models loaded and one analysis, a hovered quadrant outlined, a click's one Add object (model) entry with picking still armed, a loose drag's numbered pins, a pin chosen and Use's one entry, Escape, Add component › Object…, a second client's removal staling each model row with its reason, Keep hiding a note and sending nothing, and Recompute's one mask.set-model entry";

const AI: &str = "luxforge.ai";
const MASK_MODE: &str = "mask";
/// The setup Masks › Object needs.
const SETUP: &str = "mask-object";
const ADDED: &str = "Add object (model)";
/// The clicked and hovered positions, in the top-left, top-right and bottom-left quadrants.
const LEFT: [f64; 2] = [0.25, 0.25];
const RIGHT: [f64; 2] = [0.75, 0.25];
const LOWER: [f64; 2] = [0.25, 0.75];
/// A loose drag down the top-left quadrant, across and up the top-right one: the fake picker
/// answers Several, the top-left quadrant's candidate first.
const U: [[f64; 2]; 4] = [[0.25, 0.1], [0.25, 0.4], [0.75, 0.4], [0.75, 0.1]];
/// The outline moves a hovered patch by more than this per channel, in 8-bit codes.
const TINTED: f64 = 40.0;

fn object(step: ObjectStep) -> MaskStep {
    MaskStep::Object(step)
}

fn quiet(name: &str, step: impl Into<script::Step>) -> Step {
    Step::new(name, step).commits(0).mode(MASK_MODE)
}

fn entry(name: &str, step: impl Into<script::Step>, label: &str) -> Step {
    Step::new(name, step)
        .commits(1)
        .label(label)
        .mode(MASK_MODE)
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
        quiet("mask-mode", WorkspaceStep::default().mode(MASK_MODE)),
        quiet("asked", object(ObjectStep::Choose(KindMenuStep::NewMask))).no_notices(),
        quiet(
            "installed",
            CapabilityStep::new(AI, CapabilityAction::Consent(true)),
        ),
        quiet("entered", object(ObjectStep::Choose(KindMenuStep::NewMask))),
        quiet("hovered", object(ObjectStep::Hover(Some(LEFT)))),
        entry("created", object(ObjectStep::Click(LEFT)), ADDED)
            .masks(1)
            .component_names(&["Model 1"]),
        quiet("pins", object(ObjectStep::Drag(U.to_vec()))),
        quiet("pin-chosen", object(ObjectStep::Pin(2))),
        entry("used", object(ObjectStep::Use), ADDED)
            .masks(1)
            .component_names(&["Model 1", "Model 2"]),
        quiet("done", object(ObjectStep::Escape)),
        quiet(
            "add-entered",
            object(ObjectStep::Choose(KindMenuStep::AddComponent)),
        ),
        quiet("add-hovered", object(ObjectStep::Hover(Some(LOWER)))),
        entry("add-clicked", object(ObjectStep::Click(LOWER)), ADDED)
            .masks(1)
            .component_names(&["Model 1", "Model 2", "Model 3"]),
        quiet("add-done", object(ObjectStep::Escape)),
        Step::new(
            "removed",
            script::Step::agent(
                "edit.add-correction",
                json!({"points": [[0.3, 0.3]], "radius": 0.02}),
            ),
        )
        .commits(1)
        .mode(MASK_MODE),
        quiet("kept", object(ObjectStep::Keep(Reference::name("Model 1")))),
        entry(
            "recomputed",
            object(ObjectStep::Recompute(Reference::name("Model 2"))),
            "Update Model 2",
        ),
    ])
}

fn masks(frame: &Frame) -> &Value {
    &frame.state()["masks"]
}

fn session(frame: &Frame) -> &Value {
    &frame.state()["object_pick"]["session"]
}

fn selects(frame: &Frame) -> &Value {
    &frame.state()["object_pick"]["selects"]
}

/// The model row of the open mask's `index`th component.
fn model(frame: &Frame, index: usize) -> &Value {
    &masks(frame)["components"][index]["model"]
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

/// The frame's hover outline holds `at` and stays off `away`.
fn outlines(frame: &Frame, at: [f64; 2], away: [f64; 2]) -> Result {
    let bounds: Vec<f64> = session(frame)["overlay"]["bounds"]
        .as_array()
        .map(|b| b.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default();
    let inside = |p: [f64; 2]| {
        bounds.len() == 4
            && bounds[0] <= p[0]
            && p[0] <= bounds[2]
            && bounds[1] <= p[1]
            && p[1] <= bounds[3]
    };
    ensure(
        inside(at) && !inside(away),
        format!(
            "The hovered object's outline holds {at:?} and not {away:?}: {}",
            session(frame)
        ),
    )
}

/// The selection the last pick sent, as an API client sends it.
fn selected_with(frame: &Frame, prompt: Value) -> Result {
    let sent = &selects(frame)["last_request"];
    ensure(
        sent["method"] == json!("task.select-object")
            && prompt.as_object().is_some_and(|prompt| {
                prompt
                    .iter()
                    .all(|(key, value)| &sent["params"][key] == value)
            }),
        format!("The selection sent {sent}, holding {prompt}"),
    )
}

pub fn verify(run: &mut crate::scenario::Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let mut checks = Checks::new();
    // The Select group lists Object…, marked for setup; choosing it opens the setup sheet and
    // picks nothing.
    let asked = launch.at("asked")?;
    let select = &masks(asked)["select"];
    ensure(
        select[0]["label"] == json!("Object\u{2026}")
            && select[0]["needs_setup"] == json!(true)
            && select[0]["tag"] == json!("set up\u{2026}"),
        format!("Object… in the Select group, needing its setup: {select}"),
    )?;
    let sheet = &asked.state()["setup_sheet"];
    ensure(
        sheet.is_object() && session(asked).is_null(),
        format!(
            "Object… without the object finder opens the setup sheet: {sheet} {}",
            session(asked)
        ),
    )?;
    let installed = launch.at("installed")?;
    let line = &installed.state()["capabilities"][AI]["setups"][SETUP];
    ensure(
        installed.state()["setup_sheet"].is_null()
            && masks(installed)["select"][0]["tag"] == json!("point at it"),
        format!("The setup installed and the sheet closed: {line}"),
    )?;
    // Entering loads both models and analyses once; the bar names the gesture.
    let entered = launch.at("entered")?;
    let record = session(entered);
    let bar = &entered.state()["draft_bar"];
    ensure(
        record["purpose"] == json!("mask")
            && record["mapped"] == json!(true)
            && record["analyses"] == json!(1)
            && record["loads"] == json!([])
            && record["analysis_error"].is_null()
            && bar["title"] == json!("New mask")
            && bar["subject"] == json!("Object"),
        format!("Picking entered: {record}, bar {bar}"),
    )?;
    // The hover outlines the top-left quadrant from that analysis.
    let hovered = launch.at("hovered")?;
    outlines(hovered, LEFT, RIGHT)?;
    ensure(
        session(hovered)["analyses"] == json!(1)
            && session(hovered)["last_request"]["method"] == json!("query.pick-object")
            && session(hovered)["last_request"]["params"]["point"] == json!([LEFT]),
        format!("The hover's pick: {}", session(hovered)),
    )?;
    checks.compare(
        hovered,
        "the hovered quadrant's outline, against the photograph before it",
        apart(rgb(hovered, LEFT)?, rgb(entered, LEFT)?),
        0.0,
        Tolerance::Above(TINTED),
    )?;
    checks.compare(
        hovered,
        "the quadrant beside it, against the photograph before it",
        apart(rgb(hovered, RIGHT)?, rgb(entered, RIGHT)?),
        0.0,
        Tolerance::AtMost(NEAR),
    )?;
    // The click: one create, picking still armed on the mask it made.
    let created = launch.at("created")?;
    selected_with(created, json!({"point": [LEFT]}))?;
    ensure(
        created.state()["last_mask_request"]["method"] == json!("mask.create-model")
            && session(created).is_object()
            && masks(created)["object"]["create"] == json!(true)
            && masks(created)["object"]["mask"].is_string(),
        format!(
            "The click's create, still picking: {} {}",
            created.state()["last_mask_request"]["method"],
            masks(created)["object"]
        ),
    )?;
    // The loose drag: the stroke asked about with every preview, two pins.
    let pins = launch.at("pins")?;
    let asked = &session(pins)["last_request"];
    let pinned = &session(pins)["pins"];
    ensure(
        asked["params"]["previews"] == json!("all")
            && asked["params"]["stroke"]
                .as_array()
                .is_some_and(|s| s.len() >= 2)
            && pinned.as_array().map(Vec::len) == Some(2)
            && pinned[0]["chosen"] == json!(false)
            && pinned[1]["chosen"] == json!(false)
            && masks(pins)["object"]["pins"] == json!({"up": 2, "chosen": 0}),
        format!("The drag's pins: {asked} {pinned}"),
    )?;
    let toggled = launch.at("pin-chosen")?;
    ensure(
        session(toggled)["pins"][0]["chosen"] == json!(false)
            && session(toggled)["pins"][1]["chosen"] == json!(true),
        format!("Pin 2 chosen: {}", session(toggled)["pins"]),
    )?;
    let candidate = session(toggled)["pins"][1]["candidate"].clone();
    let used = launch.at("used")?;
    selected_with(used, json!({"candidate": candidate}))?;
    ensure(
        used.state()["last_mask_request"]["method"] == json!("mask.add-model")
            && session(used)["pins"].is_null(),
        format!("Use's add: {}", used.state()["last_mask_request"]["method"]),
    )?;
    let done = launch.at("done")?;
    ensure(
        session(done).is_null()
            && masks(done)["object"].is_null()
            && done.state()["draft_bar"].is_null(),
        format!("Escape leaves picking: {}", session(done)),
    )?;
    // Each model row: the class, how it was picked, when and by what.
    for (index, picked) in [(0, "picked from a point"), (1, "picked from a stroke")] {
        let row = model(done, index);
        ensure(
            row["class"] == json!("Object")
                && row["picked"] == json!(picked)
                && row["computed"].is_string()
                && row["part"]
                    .as_str()
                    .is_some_and(|part| part.starts_with("Object finder, version "))
                && row["stale"].is_null(),
            format!("Model row {index}: {row}"),
        )?;
    }
    // Add component › Object… adds into the open mask.
    let added = launch.at("add-entered")?;
    ensure(
        masks(added)["object"]["create"] == json!(false) && session(added)["mapped"] == json!(true),
        format!("Add component › Object…: {}", masks(added)["object"]),
    )?;
    let clicked = launch.at("add-clicked")?;
    ensure(
        clicked.state()["last_mask_request"]["method"] == json!("mask.add-model")
            && clicked.state()["last_mask_request"]["params"]["mode"] == json!("add"),
        format!(
            "Add component's add: {}",
            clicked.state()["last_mask_request"]
        ),
    )?;
    // The removal stales every model row with its reason in plain words.
    let removed = launch.at("removed")?;
    let stale = "Needs recompute \u{b7} an object was removed after this was picked";
    for index in 0..3 {
        let row = model(removed, index);
        ensure(
            row["stale"] == json!(stale) && row["reasons"] == json!(["repair-prefix"]),
            format!("Model row {index} after the removal: {row}"),
        )?;
    }
    // Keep sends nothing and hides only that row's note.
    let kept = launch.at("kept")?;
    ensure(
        model(kept, 0)["stale"].is_null()
            && model(kept, 0)["reasons"] == json!(["repair-prefix"])
            && model(kept, 1)["stale"] == json!(stale)
            && kept.state()["last_mask_request"] == removed.state()["last_mask_request"]
            && selects(kept)["sent"] == selects(removed)["sent"],
        format!("Keep: {} {}", model(kept, 0), selects(kept)),
    )?;
    // Recompute sends the stored stroke prompt again and replaces the row's selection.
    let recomputed = launch.at("recomputed")?;
    let request = &recomputed.state()["last_mask_request"];
    ensure(
        request["method"] == json!("mask.set-model")
            && selects(recomputed)["last_request"]["params"]["candidate"] == candidate
            && selects(recomputed)["last_request"]["params"]["stroke"].is_array()
            && model(recomputed, 1)["stale"].is_null()
            && model(recomputed, 1)["reasons"] == json!([])
            && model(recomputed, 2)["stale"] == json!(stale),
        format!("Recompute: {request} {}", model(recomputed, 1)),
    )?;
    checks.write(
        run.out(),
        SCENARIO,
        json!({
            "tinted_codes": TINTED,
            "near_codes": NEAR,
            "scope": "State, history and log over the flat four-quadrant fixture with the developer fake segmenter, whose masks follow colour similarity, so an object is a quadrant: a wiring, parity and overlay claim, not a model-quality or latency claim",
        }),
    )
}
