//! The `mask-linear` smoke scenario: phase A's whole slice on the real editor, in two launches.
//!
//! Launch 1 enters Mask mode, draws a linear gradient with one sweep, commits it, lifts Exposure
//! through it with a slider gesture, edits the same masked layer again from JSON — naming the mask
//! by the name the host gave it, which is the only way a script can name a mask `mask.create-linear`
//! assigned the identity of — turns the coverage overlay on and off again, and undoes. Launch 2
//! reopens the same catalog in a new process and walks the history.
//!
//! Every frame is checked against the photograph's own measured change, not only against the state
//! it recorded: the gradient runs down the picture, so the top of the frame sits at coverage 0 and
//! must stay where it was while the bottom is lifted.
use crate::{
    scenario::{Checked, Frame, Plan, Run, Step, Tolerance, pixels},
    *,
};
use luxforge_core::BASIC_EFFECT;
use luxforge_evidence::{self as script, MaskStep, PreviewStep, SliderStep, WorkspaceStep};

pub const SCENARIO: &str = "mask-linear";
/// The golden four-quadrant fixture, unrotated: content and output coordinates coincide, so the
/// normalized positions the script sweeps through are the ones the capture is measured at.
pub const FIXTURE: &str = "fixtures/s0/orientation-1.jpg";
/// What `reproduce.md` says about the run's two launches.
pub const NOTE: &str = "Two launches over one catalog: the first draws and edits through a mask, the second reopens it.";

const BASIC: &str = "set-basic";
const EXPOSURE: &str = "exposure";
const LINEAR_METHOD: &str = "mask.create-linear";
/// Where the gradient's two ends sit, in normalized content coordinates: coverage 0 above `Y0`,
/// coverage 1 below `Y1`.
const Y0: f64 = 0.30;
const Y1: f64 = 0.70;
/// The exposure the slider gesture commits, then what the JSON client raises it to.
const DRAGGED: f64 = 2.0;
const RETYPED: f64 = 2.5;
/// The history entries those two commits make, each naming the mask the layer is bound to.
const DRAGGED_LABEL: &str = "Mask 1 · Exposure +2.00 EV";
const RETYPED_LABEL: &str = "Mask 1 · Exposure +2.50 EV";

/// Where the two measured patches sit inside the displayed photograph, as fractions of its own
/// drawn rectangle. The first is above the gradient's start and the second below its end, each
/// clear of the fixture's white centre stripe and its black dashes.
const UNCOVERED: [f64; 2] = [0.25, 0.12];
const COVERED: [f64; 2] = [0.25, 0.88];
/// Half the side of a measured patch, in capture pixels.
const PATCH_HALF: i64 = 6;
/// How far a patch's mean luminance must move before this scenario calls it lifted. A +2 EV lift of
/// a mid-blue quadrant moves it by tens of codes, so this is a margin, not a tuned threshold.
const LIFTED: f64 = 12.0;
/// How close two mean luminance readings must stay before this scenario calls a patch untouched.
/// Only JPEG-free renderer readback is compared, so this is tight on purpose.
const UNTOUCHED: f64 = 1.0;

/// The gradient exactly as the sweep draws it, in the fields its gesture drafts.
fn swept() -> Value {
    json!({"x0":0.5,"y0":Y0,"x1":0.5,"y1":Y1})
}

/// Launch 1: the open, then one frame per step. The expectations here are what each step commits
/// and records; `verify` below checks the gesture, the mask and what the photograph shows.
pub fn launch1(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        // The fixture as launched: no Basic layer and nothing drafted.
        Step::opened("opened").no_layer(BASIC_EFFECT).no_draft(),
        // 1: Mask mode, through the same `workspace.set` the mode strip sends. A canvas mode
        // commits nothing.
        Step::new("mask-mode", WorkspaceStep::default().mode("mask")).commits(0),
        // 2: a new mask whose first component is a linear gradient: the gesture opens and drafts,
        // and nothing is committed.
        Step::new("new", MaskStep::New("linear".into())).commits(0),
        // 3: the drag itself, one sweep from the untouched side towards the affected one, with the
        // pointer still down: the frame is the picture mid-gesture, the gradient drafted exactly
        // where the sweep drew it and still uncommitted.
        Step::new(
            "sweep",
            MaskStep::Sweep {
                from: [0.5, Y0],
                to: [0.5, Y1],
            },
        )
        .commits(0)
        .draft(LINEAR_METHOD, swept()),
        // 4: the pointer lifted. The gradient stays; nothing is committed by a release.
        Step::new("release", MaskStep::Release)
            .commits(0)
            .draft(LINEAR_METHOD, swept()),
        // 5: Apply: one history entry, one mask, no layer bound to it yet.
        Step::new("apply", MaskStep::Apply)
            .commits(1)
            .label("Add linear")
            .no_draft()
            .no_layer(BASIC_EFFECT),
        // 6: the masked Exposure gesture. The sections below the list are bound to the mask the
        // commit opened, so this is the panel's own drag on the masked layer.
        Step::new(
            "drag",
            SliderStep::new(BASIC, EXPOSURE, [0.8, 1.4, DRAGGED]).release(),
        )
        .commits(1)
        .label(DRAGGED_LABEL)
        .no_draft()
        .payload(BASIC_EFFECT, json!({ EXPOSURE: DRAGGED })),
        // 7: the same layer from JSON, naming the mask by the name the host gave it: updated in
        // place, not replaced.
        Step::new(
            "json-edit",
            script::Step::call(
                "edit.set-basic",
                json!({"mask":{"name":"Mask 1"},"exposure":RETYPED}),
            ),
        )
        .commits(1)
        .label(RETYPED_LABEL)
        .payload(BASIC_EFFECT, json!({ EXPOSURE: RETYPED }))
        .same_layer(BASIC_EFFECT, "drag"),
        // 8: the coverage overlay on, which commits nothing.
        Step::new("overlay-on", WorkspaceStep::default().mask_overlay("tint")).commits(0),
        // 9: and off again.
        Step::new("overlay-off", WorkspaceStep::default().mask_overlay("off")).commits(0),
        // 10: undo, back to the exposure the drag committed, on the same layer.
        Step::new("undo", script::Step::api("history.undo"))
            .commits(1)
            .label(DRAGGED_LABEL)
            .payload(BASIC_EFFECT, json!({ EXPOSURE: DRAGGED }))
            .same_layer(BASIC_EFFECT, "drag"),
    ])
}

/// Launch 2: the reopen of launch 1's catalog, then its history walked. Nothing it does commits.
pub fn launch2(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        // The reopen: the entry launch 1 ended on, its layer holding the drag's exposure.
        Step::opened("reopened")
            .label(DRAGGED_LABEL)
            .payload(BASIC_EFFECT, json!({ EXPOSURE: DRAGGED }))
            .no_draft(),
        // 1: Mask mode again, so the reopened masks are shown as well as stored.
        Step::new("mask-mode", WorkspaceStep::default().mode("mask")).commits(0),
        // 2: the entry the gradient was committed in, selected from the history.
        Step::new("mask-entry", PreviewStep::Sequence(1)).commits(0),
        // 3: back to the current state.
        Step::new("current", PreviewStep::Current)
            .commits(0)
            .payload(BASIC_EFFECT, json!({ EXPOSURE: DRAGGED })),
    ])
}

fn mask_id(frame: &Frame) -> Result<&str> {
    frame.only_mask()?["id"]
        .as_str()
        .ok_or_else(|| "The listed mask has no identity".into())
}

/// The stack's one Basic layer, or `None` when the stack holds none.
fn basic_layer(frame: &Frame) -> Option<&Value> {
    frame.layer(BASIC_EFFECT)
}

/// Both measured patches of one capture, above the gradient and below it: the mean Rec. 709
/// luminance of each, in the photograph's own drawn rectangle. The two top quadrant colours are
/// what the mask never touches — the gradient reaches coverage 0 above `Y0` — so the rectangle is
/// found from those alone: a bounds search over all four colours would lose the bottom of the
/// picture the moment the masked layer lifted it.
fn patches(frame: &Frame) -> Result<[f64; 2]> {
    let bounds = pixels::top_quadrant_bounds(frame)?;
    let image = frame.image()?;
    Ok([
        pixels::mean_luminance(image, pixels::at(bounds, UNCOVERED), PATCH_HALF)?,
        pixels::mean_luminance(image, pixels::at(bounds, COVERED), PATCH_HALF)?,
    ])
}

/// Both launches, once each has held its plan: launch 1's gesture, mask and pixels, then launch 2
/// against the identities and measurements launch 1 left.
pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let [launch1, launch2] = launches else {
        return Err(format!(
            "Expected launch1 and launch2, found {} launches",
            launches.len()
        )
        .into());
    };
    let checks = verify_launch1(launch1)?;
    run.record("launch1", checks.clone());
    let reopened = verify_launch2(launch2, &checks)?;
    run.record("launch2", reopened.clone());
    write_json(
        &run.out().join("mask-linear-checks.json"),
        &json!({"launch1": checks, "launch2": reopened}),
    )?;
    Ok(())
}

/// Launch 1, step by step. Returns the identities and measurements launch 2 is checked against.
fn verify_launch1(launch: &Checked) -> Result<Value> {
    let mut shows = Vec::new();
    let mut record = |frame: &Value, what: &str, detail: Value| {
        shows.push(json!({"frame":frame["file"],"shows":what,"detail":detail}));
    };

    // The fixture as launched: no mask in the recipe (the plan holds it to no Basic layer).
    let opened_frame = launch.at("opened")?;
    ensure(
        opened_frame.masks()?.is_empty(),
        "The fixture opened with a mask already in the recipe",
    )?;
    let opened = patches(opened_frame)?;
    record(
        opened_frame,
        "the fixture as launched, with no mask in the recipe",
        json!({"patches":opened}),
    );

    // Mask mode.
    let mode = launch.at("mask-mode")?;
    ensure(
        mode["state"]["workspace"]["mode"] == json!("mask"),
        format!(
            "The workspace step did not enter Mask mode: {}",
            mode["state"]["workspace"]
        ),
    )?;
    record(
        mode,
        "Mask mode with an empty Masks panel",
        json!({"caption":mode["state"]["masks"]["caption"]}),
    );

    // The gesture is open and drafting, and no mask is committed.
    let new = launch.at("new")?;
    let opened_draft = &new["state"]["mask_draft"];
    ensure(
        opened_draft["kind"] == json!("linear") && opened_draft["mask"] == Value::Null,
        format!("The new step holds no open create gesture: {opened_draft}"),
    )?;
    ensure(
        new.masks()?.is_empty(),
        "Opening the gesture committed a mask",
    )?;
    record(
        new,
        "an open linear-gradient gesture on the canvas, drafted and uncommitted",
        json!({"draft":opened_draft}),
    );

    // The sweep put the gradient exactly where the script drew it, pointer down, still
    // uncommitted.
    let sweep = launch.at("sweep")?;
    let swept_draft = &sweep["state"]["mask_draft"];
    ensure(
        swept_draft["shape"] == swept() && swept_draft["dragging"] == json!(true),
        format!("The sweep's gesture holds {swept_draft}"),
    )?;
    ensure(sweep.masks()?.is_empty(), "The sweep committed a mask")?;
    record(
        sweep,
        "the gradient swept from the untouched side to the affected one, still a draft",
        json!({"draft":swept_draft}),
    );

    // The pointer lifted. The gradient the sweep drew is still exactly where it was and the
    // gesture is still open, because a release commits nothing on its own.
    let release = launch.at("release")?;
    let released = &release["state"]["mask_draft"];
    ensure(
        released["shape"] == swept() && released["dragging"] == json!(false),
        format!("The release's gesture holds {released}"),
    )?;
    ensure(release.masks()?.is_empty(), "The release committed a mask")?;
    record(
        release,
        "the drawn gradient with the pointer lifted, still uncommitted",
        json!({"draft":released}),
    );

    // Apply. One mask, one linear component, no layer bound to it — so the photograph is
    // byte-unchanged: a mask on its own is a selection, not an edit.
    let apply = launch.at("apply")?;
    ensure(
        apply["state"]["mask_draft"] == Value::Null,
        "The gesture is still open after Apply",
    )?;
    let mask = mask_id(apply)?.to_owned();
    let mask_name = apply.only_mask()?["name"]
        .as_str()
        .ok_or("The listed mask has no name")?
        .to_owned();
    let component = {
        let listed = apply.components()?;
        ensure(
            listed.len() == 1 && listed[0]["kind"] == json!("linear"),
            format!("The committed mask holds {}", json!(listed)),
        )?;
        listed[0]["id"]
            .as_str()
            .ok_or("The component has no identity")?
            .to_owned()
    };
    ensure(
        apply.only_mask()?["layers"] == json!([]),
        "A freshly drawn mask already has a layer bound to it",
    )?;
    let applied = patches(apply)?;
    pixels::compare(
        "a mask with no layer, above the gradient",
        applied[0],
        opened[0],
        Tolerance::Within(UNTOUCHED),
    )?;
    pixels::compare(
        "a mask with no layer, below the gradient",
        applied[1],
        opened[1],
        Tolerance::Within(UNTOUCHED),
    )?;
    record(
        apply,
        "the committed mask in the panel and the photograph unchanged by it",
        json!({"mask":mask,"name":mask_name,"component":component,"label":apply.label()?,"patches":applied}),
    );

    // The masked Exposure gesture, released. The picture is lifted below the gradient and
    // untouched above it, and the layer the panel committed names the mask.
    let drag = launch.at("drag")?;
    let layer = basic_layer(drag).ok_or("The masked gesture committed no Basic layer")?;
    ensure(
        layer["mask"] == json!(mask),
        format!("The committed Basic layer names {}", layer["mask"]),
    )?;
    let layer_id = layer["id"]
        .as_str()
        .ok_or("The masked layer has no identity")?
        .to_owned();
    ensure(
        drag.only_mask()?["layers"]
            .as_array()
            .is_some_and(|layers| layers.len() == 1),
        format!(
            "The mask lists {} bound layers",
            drag.only_mask()?["layers"]
        ),
    )?;
    let dragged = patches(drag)?;
    pixels::compare(
        "above the gradient after the masked drag",
        dragged[0],
        opened[0],
        Tolerance::Within(UNTOUCHED),
    )?;
    pixels::compare(
        "below the gradient after the masked drag",
        dragged[1],
        opened[1],
        Tolerance::Above(LIFTED),
    )?;
    record(
        drag,
        "one part of the photograph lifted through the mask and the other left alone",
        json!({"layer":layer_id,"exposure":DRAGGED,"patches":dragged,"label":drag.label()?}),
    );

    // The same layer again from JSON, naming the mask by name. The step's own record must show
    // the identity the name resolved to, which is what makes the request reproducible.
    let edit = launch.at("json-edit")?;
    let step = &edit["step"];
    ensure(
        step["resolved"]["mask"] == json!(mask),
        format!(
            "The JSON step resolved {} rather than the mask",
            step["resolved"]
        ),
    )?;
    let retyped = patches(edit)?;
    pixels::compare(
        "above the gradient after the JSON edit",
        retyped[0],
        opened[0],
        Tolerance::Within(UNTOUCHED),
    )?;
    pixels::compare(
        "below the gradient after the JSON edit",
        retyped[1],
        dragged[1],
        Tolerance::Above(LIFTED),
    )?;
    record(
        edit,
        "the same masked layer raised again from JSON, by the mask's name",
        json!({"resolved":step["resolved"],"exposure":RETYPED,"patches":retyped}),
    );

    // The coverage overlay. It is drawn over the covered part of the picture only, so the
    // uncovered patch is exactly where it was and the covered one is not.
    let overlay = launch.at("overlay-on")?;
    ensure(
        overlay["state"]["workspace"]["mask_overlay"] == json!("tint"),
        format!(
            "The overlay step left the workspace at {}",
            overlay["state"]["workspace"]["mask_overlay"]
        ),
    )?;
    let tinted = patches(overlay)?;
    pixels::compare(
        "above the gradient with the overlay on",
        tinted[0],
        retyped[0],
        Tolerance::Within(UNTOUCHED),
    )?;
    // Strictly more than the untouched tolerance: the negation of `Within(UNTOUCHED)`, which
    // `Tolerance::Apart`, being inclusive, is not.
    ensure(
        (tinted[1] - retyped[1]).abs() > UNTOUCHED,
        format!(
            "The overlay drew nothing over the covered part: {:.2} against {:.2}",
            tinted[1], retyped[1]
        ),
    )?;
    record(
        overlay,
        "the coverage overlay tinting exactly the covered part of the photograph",
        json!({"patches":tinted,"colour":overlay["state"]["workspace"]["mask_overlay_colour"]}),
    );

    // The overlay off again, and the photograph back to what it was under it.
    let cleared_frame = launch.at("overlay-off")?;
    ensure(
        cleared_frame["state"]["workspace"]["mask_overlay"] == json!("off"),
        "The overlay did not switch off",
    )?;
    let cleared = patches(cleared_frame)?;
    pixels::compare(
        "above the gradient with the overlay off",
        cleared[0],
        retyped[0],
        Tolerance::Within(UNTOUCHED),
    )?;
    pixels::compare(
        "below the gradient with the overlay off",
        cleared[1],
        retyped[1],
        Tolerance::Within(UNTOUCHED),
    )?;
    record(
        cleared_frame,
        "the overlay switched off, leaving the edited photograph",
        json!({"patches":cleared}),
    );

    // Undo. One entry back: the masked layer holds what the drag committed again, the mask and
    // its component keep their identities, and the picture follows.
    let undo = launch.at("undo")?;
    ensure(
        mask_id(undo)? == mask && undo.component(0)?["id"] == json!(component),
        "Undo changed the mask or component identity",
    )?;
    let undone = patches(undo)?;
    pixels::compare(
        "above the gradient after undo",
        undone[0],
        opened[0],
        Tolerance::Within(UNTOUCHED),
    )?;
    pixels::compare(
        "below the gradient after undo",
        undone[1],
        dragged[1],
        Tolerance::Within(UNTOUCHED),
    )?;
    record(
        undo,
        "the undone state: the drag's exposure again, through the same mask",
        json!({"patches":undone,"label":undo.label()?}),
    );

    Ok(json!({
        "mask": mask,
        "mask_name": mask_name,
        "component": component,
        "layer": layer_id,
        "revision": undo.revision()?,
        "label": undo.label()?,
        "exposure": DRAGGED,
        "patches": {"opened": opened, "dragged": dragged, "undone": undone},
        "lifted_margin": LIFTED,
        "untouched_tolerance": UNTOUCHED,
        "frames": shows,
        "scope": "Mean Rec. 709 luminance of two patches of the displayed photograph, read back from the renderer; not a colorimetric claim",
    }))
}

/// Launch 2: the same catalog in a new process. Identities, bindings and history navigation, each
/// against what launch 1 left.
fn verify_launch2(launch: &Checked, launch1: &Value) -> Result<Value> {
    // The reopen itself. Revision, mask, component and bound layer all come back with the
    // identities launch 1 wrote; the plan holds its label and stored exposure to the ones launch 1
    // ended on.
    let reopened = launch.at("reopened")?;
    ensure(
        json!(reopened.revision()?) == launch1["revision"],
        format!(
            "Launch 2 reopened at revision {} rather than {}",
            reopened.revision()?,
            launch1["revision"]
        ),
    )?;
    ensure(
        json!(mask_id(reopened)?) == launch1["mask"]
            && reopened.only_mask()?["name"] == launch1["mask_name"],
        format!("Launch 2 reopened the mask as {}", reopened.only_mask()?),
    )?;
    let layer = basic_layer(reopened).ok_or("Launch 2 reopened without the masked layer")?;
    ensure(
        layer["id"] == launch1["layer"] && layer["mask"] == launch1["mask"],
        format!("Launch 2's masked layer is {layer}"),
    )?;

    // Mask mode, where the components are derived. The component keeps its identity too, and the
    // panel names the layer bound to the mask.
    let mode = launch.at("mask-mode")?;
    let listed = mode.components()?;
    ensure(
        listed.len() == 1 && listed[0]["id"] == launch1["component"],
        format!("Launch 2 reopened the components as {}", json!(listed)),
    )?;
    ensure(
        mode.only_mask()?["layers"]
            .as_array()
            .is_some_and(|layers| layers.len() == 1),
        format!(
            "The reopened mask lists {} bound layers",
            mode.only_mask()?["layers"]
        ),
    )?;
    let reopened_patches = patches(mode)?;
    let undone: [f64; 2] = serde_json::from_value(launch1["patches"]["undone"].clone())?;
    pixels::compare(
        "above the gradient after the reopen",
        reopened_patches[0],
        undone[0],
        Tolerance::Within(UNTOUCHED),
    )?;
    pixels::compare(
        "below the gradient after the reopen",
        reopened_patches[1],
        undone[1],
        Tolerance::Within(UNTOUCHED),
    )?;

    // The entry the gradient was committed in, selected from the reopened history. The mask exists
    // there, no layer is bound to it yet, and the photograph is the unedited one.
    let historical = launch.at("mask-entry")?;
    ensure(
        historical["state"]["selection"] != Value::Null,
        format!(
            "Launch 2 did not select a history entry: {}",
            historical["state"]["selection"]
        ),
    )?;
    let displayed = &historical["state"]["stack"]["displayed"]["layers"];
    ensure(
        displayed
            .as_array()
            .is_some_and(|layers| layers.iter().all(|layer| layer["mask"] == Value::Null)),
        format!("The mask-create entry already renders a masked layer: {displayed}"),
    )?;
    let historical_patches = patches(historical)?;
    let opened: [f64; 2] = serde_json::from_value(launch1["patches"]["opened"].clone())?;
    pixels::compare(
        "above the gradient at the mask-create entry",
        historical_patches[0],
        opened[0],
        Tolerance::Within(UNTOUCHED),
    )?;
    pixels::compare(
        "below the gradient at the mask-create entry",
        historical_patches[1],
        opened[1],
        Tolerance::Within(UNTOUCHED),
    )?;

    // Back to the current state, which is the reopened one again; the plan holds its exposure to
    // the drag's.
    let current = launch.at("current")?;
    ensure(
        json!(current.revision()?) == launch1["revision"],
        format!("Returning to current left revision {}", current.revision()?),
    )?;
    let current_patches = patches(current)?;
    pixels::compare(
        "above the gradient back at current",
        current_patches[0],
        undone[0],
        Tolerance::Within(UNTOUCHED),
    )?;
    pixels::compare(
        "below the gradient back at current",
        current_patches[1],
        undone[1],
        Tolerance::Within(UNTOUCHED),
    )?;

    Ok(json!({
        "mask": launch1["mask"],
        "component": launch1["component"],
        "layer": launch1["layer"],
        "revision": reopened.revision()?,
        "patches": {
            "reopened": reopened_patches,
            "mask_create_entry": historical_patches,
            "back_at_current": current_patches,
        },
    }))
}
