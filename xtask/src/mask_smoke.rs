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
    scenario::{Checked, Checks, Frame, Plan, Run, Step, Tolerance},
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
        // The fixture as launched: no mask, no Basic layer and nothing drafted.
        Step::opened("opened")
            .no_layer(BASIC_EFFECT)
            .no_draft()
            .masks(0),
        // 1: Mask mode, through the same `workspace.set` the mode strip sends. A canvas mode
        // commits nothing.
        Step::new("mask-mode", WorkspaceStep::default().mode("mask"))
            .commits(0)
            .mode("mask"),
        // 2: a new mask whose first component is a linear gradient: the gesture opens and drafts,
        // and nothing is committed.
        Step::new("new", MaskStep::New("linear".into()))
            .commits(0)
            .masks(0),
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
        .draft(LINEAR_METHOD, swept())
        .masks(0),
        // 4: the pointer lifted. The gradient stays; nothing is committed by a release.
        Step::new("release", MaskStep::Release)
            .commits(0)
            .draft(LINEAR_METHOD, swept())
            .masks(0),
        // 5: Apply: one history entry, one mask of one linear component, no layer bound to it yet.
        Step::new("apply", MaskStep::Apply)
            .commits(1)
            .label("Add linear")
            .no_draft()
            .no_layer(BASIC_EFFECT)
            .masks(1)
            .components(&["add linear"]),
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
        Step::new("overlay-on", WorkspaceStep::default().mask_overlay("tint"))
            .commits(0)
            .workspace("mask_overlay", json!("tint")),
        // 9: and off again.
        Step::new("overlay-off", WorkspaceStep::default().mask_overlay("off"))
            .commits(0)
            .workspace("mask_overlay", json!("off")),
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
            .no_draft()
            .masks(1),
        // 1: Mask mode again, so the reopened masks are shown as well as stored, with their
        // components derived.
        Step::new("mask-mode", WorkspaceStep::default().mode("mask"))
            .commits(0)
            .components(&["add linear"]),
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

/// How many layers the one mask lists as bound to it.
fn bound(frame: &Frame) -> Result<usize> {
    frame.only_mask()?["layers"]
        .as_array()
        .map(Vec::len)
        .ok_or_else(|| "The listed mask names no bound layers".into())
}

/// Both measured patches of one capture, above the gradient and below it: the mean Rec. 709
/// luminance of each, in the photograph's own drawn rectangle.
fn patches(frame: &Frame) -> Result<[f64; 2]> {
    Ok([
        frame.luminance_at(UNCOVERED, PATCH_HALF)?,
        frame.luminance_at(COVERED, PATCH_HALF)?,
    ])
}

/// Both patches of `frame` against `reference`'s, each under its own tolerance.
fn compare_both(
    checks: &mut Checks,
    frame: &Frame,
    what: &str,
    reference: [f64; 2],
    [above, below]: [Tolerance; 2],
) -> Result<[f64; 2]> {
    let read = patches(frame)?;
    checks.compare(
        frame,
        &format!("{what}, above the gradient"),
        read[0],
        reference[0],
        above,
    )?;
    checks.compare(
        frame,
        &format!("{what}, below the gradient"),
        read[1],
        reference[1],
        below,
    )?;
    Ok(read)
}

/// What launch 1 leaves for launch 2 to be checked against.
struct Left {
    revision: u64,
    mask: String,
    name: String,
    component: String,
    layer: String,
    opened: [f64; 2],
    undone: [f64; 2],
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
    let mut checks = Checks::new();
    let left = verify_launch1(launch1, &mut checks)?;
    verify_launch2(launch2, &left, &mut checks)?;
    checks.write(
        run.out(),
        SCENARIO,
        json!({
            "mask": left.mask,
            "component": left.component,
            "layer": left.layer,
            "lifted_margin": LIFTED,
            "untouched_tolerance": UNTOUCHED,
            "scope": "Mean Rec. 709 luminance of two patches of the photograph the editor records drawing, read back from the renderer; not a colorimetric claim",
        }),
    )
}

/// Launch 1, step by step. Returns the identities and measurements launch 2 is checked against.
fn verify_launch1(launch: &Checked, checks: &mut Checks) -> Result<Left> {
    let same = Tolerance::Within(UNTOUCHED);
    let lifted = Tolerance::Above(LIFTED);
    let opened = patches(launch.at("opened")?)?;

    // The gesture is open and drafting a create.
    let opened_draft = &launch.at("new")?.state()["mask_draft"];
    ensure(
        opened_draft["kind"] == json!("linear") && opened_draft["mask"] == Value::Null,
        format!("The new step holds no open create gesture: {opened_draft}"),
    )?;
    // The sweep put the gradient exactly where the script drew it, pointer down; the release
    // leaves it there with the gesture still open, because a release commits nothing on its own.
    for (step, dragging) in [("sweep", true), ("release", false)] {
        let draft = &launch.at(step)?.state()["mask_draft"];
        ensure(
            draft["shape"] == swept() && draft["dragging"] == json!(dragging),
            format!("The {step}'s gesture holds {draft}"),
        )?;
    }

    // Apply. One mask, one linear component, no layer bound to it — so the photograph is
    // byte-unchanged: a mask on its own is a selection, not an edit.
    let apply = launch.at("apply")?;
    ensure(
        apply.state()["mask_draft"] == Value::Null,
        "The gesture is still open after Apply",
    )?;
    let mask = mask_id(apply)?.to_owned();
    let name = apply.only_mask()?["name"]
        .as_str()
        .ok_or("The listed mask has no name")?
        .to_owned();
    let component = apply.component(0)?["id"]
        .as_str()
        .ok_or("The component has no identity")?
        .to_owned();
    ensure(
        bound(apply)? == 0,
        "A freshly drawn mask already has a layer bound to it",
    )?;
    compare_both(checks, apply, "a mask with no layer", opened, [same, same])?;

    // The masked Exposure gesture, released. The picture is lifted below the gradient and
    // untouched above it, and the layer the panel committed names the mask.
    let drag = launch.at("drag")?;
    let layer = drag
        .layer(BASIC_EFFECT)
        .ok_or("The masked gesture committed no Basic layer")?;
    ensure(
        layer["mask"] == json!(mask),
        format!("The committed Basic layer names {}", layer["mask"]),
    )?;
    let layer = layer["id"]
        .as_str()
        .ok_or("The masked layer has no identity")?
        .to_owned();
    ensure(
        bound(drag)? == 1,
        format!(
            "The mask lists {} bound layers",
            drag.only_mask()?["layers"]
        ),
    )?;
    let dragged = compare_both(
        checks,
        drag,
        "after the masked drag",
        opened,
        [same, lifted],
    )?;

    // The same layer again from JSON, naming the mask by name. The step's own record must show
    // the identity the name resolved to, which is what makes the request reproducible.
    let edit = launch.at("json-edit")?;
    ensure(
        edit["step"]["resolved"]["mask"] == json!(mask),
        format!(
            "The JSON step resolved {} rather than the mask",
            edit["step"]["resolved"]
        ),
    )?;
    let retyped = patches(edit)?;
    checks.compare(
        edit,
        "after the JSON edit, above the gradient",
        retyped[0],
        opened[0],
        same,
    )?;
    checks.compare(
        edit,
        "after the JSON edit, below the gradient",
        retyped[1],
        dragged[1],
        lifted,
    )?;

    // The coverage overlay. It is drawn over the covered part of the picture only, so the
    // uncovered patch is exactly where it was and the covered one is not; switched off, the
    // photograph is back to what it was under it.
    let overlay = launch.at("overlay-on")?;
    compare_both(
        checks,
        overlay,
        "with the overlay on",
        retyped,
        [same, Tolerance::Beyond(UNTOUCHED)],
    )?;
    compare_both(
        checks,
        launch.at("overlay-off")?,
        "with the overlay off",
        retyped,
        [same, same],
    )?;

    // Undo. One entry back: the masked layer holds what the drag committed again, the mask and
    // its component keep their identities, and the picture follows.
    let undo = launch.at("undo")?;
    ensure(
        mask_id(undo)? == mask && undo.component(0)?["id"] == json!(component),
        "Undo changed the mask or component identity",
    )?;
    let undone = patches(undo)?;
    checks.compare(
        undo,
        "after undo, above the gradient",
        undone[0],
        opened[0],
        same,
    )?;
    checks.compare(
        undo,
        "after undo, below the gradient",
        undone[1],
        dragged[1],
        same,
    )?;

    Ok(Left {
        revision: undo.revision()?,
        mask,
        name,
        component,
        layer,
        opened,
        undone,
    })
}

/// Launch 2: the same catalog in a new process. Identities, bindings and history navigation, each
/// against what launch 1 left.
fn verify_launch2(launch: &Checked, left: &Left, checks: &mut Checks) -> Result {
    let same = Tolerance::Within(UNTOUCHED);
    // The reopen itself. Revision, mask, component and bound layer all come back with the
    // identities launch 1 wrote; the plan holds its label and stored exposure to the ones launch 1
    // ended on.
    let reopened = launch.at("reopened")?;
    ensure(
        reopened.revision()? == left.revision,
        format!(
            "Launch 2 reopened at revision {} rather than {}",
            reopened.revision()?,
            left.revision
        ),
    )?;
    ensure(
        mask_id(reopened)? == left.mask && reopened.only_mask()?["name"] == json!(left.name),
        format!("Launch 2 reopened the mask as {}", reopened.only_mask()?),
    )?;
    let layer = reopened
        .layer(BASIC_EFFECT)
        .ok_or("Launch 2 reopened without the masked layer")?;
    ensure(
        layer["id"] == json!(left.layer) && layer["mask"] == json!(left.mask),
        format!("Launch 2's masked layer is {layer}"),
    )?;

    // Mask mode, where the components are derived. The component keeps its identity too, and the
    // panel names the layer bound to the mask.
    let mode = launch.at("mask-mode")?;
    ensure(
        mode.component(0)?["id"] == json!(left.component),
        format!(
            "Launch 2 reopened the components as {}",
            json!(mode.components()?)
        ),
    )?;
    ensure(
        bound(mode)? == 1,
        format!(
            "The reopened mask lists {} bound layers",
            mode.only_mask()?["layers"]
        ),
    )?;
    compare_both(checks, mode, "after the reopen", left.undone, [same, same])?;

    // The entry the gradient was committed in, selected from the reopened history. The mask exists
    // there, no layer is bound to it yet, and the photograph is the unedited one.
    let historical = launch.at("mask-entry")?;
    ensure(
        historical.state()["selection"] != Value::Null,
        format!(
            "Launch 2 did not select a history entry: {}",
            historical.state()["selection"]
        ),
    )?;
    let displayed = &historical.state()["stack"]["displayed"]["layers"];
    ensure(
        displayed
            .as_array()
            .is_some_and(|layers| layers.iter().all(|layer| layer["mask"] == Value::Null)),
        format!("The mask-create entry already renders a masked layer: {displayed}"),
    )?;
    compare_both(
        checks,
        historical,
        "at the mask-create entry",
        left.opened,
        [same, same],
    )?;

    // Back to the current state, which is the reopened one again; the plan holds its exposure to
    // the drag's.
    let current = launch.at("current")?;
    ensure(
        current.revision()? == left.revision,
        format!("Returning to current left revision {}", current.revision()?),
    )?;
    compare_both(
        checks,
        current,
        "back at current",
        left.undone,
        [same, same],
    )?;
    Ok(())
}
