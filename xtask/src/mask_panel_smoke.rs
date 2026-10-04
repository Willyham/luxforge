//! The `mask-panel` smoke scenario: the Masks panel's own states, over the recipe the Develop · mask
//! mode board draws.
//!
//! The recipe is built the way a person builds it, through the panel's gestures: `Sky` (a linear),
//! `Face` (a radial add, a subtracting brush of two strokes and an intersecting luminance range) and
//! `Foreground` (a linear whose overlay is hidden with its eye), each renamed through the same
//! `mask.rename` and `mask.rename-component` requests the rename field sends, and Basic Exposure
//! +0.60 and Presence Clarity +18 applied through `Face`. On that recipe it captures what the
//! design's rendered acceptance names: the list with three masks and the open one's rows, the
//! selected component's fields, a hover on a component row, the overlay control in each of its four
//! modes and both tints, the New mask menu, the Brush section appearing when a brush is armed and
//! disappearing when it is put down, the scope chip on the bound sections, and the draft bar for a
//! stroke. Its last frame, `radial-dragged`, is the board's own state: `Face` open, `Radial 1`
//! selected with its handles resting on the canvas, its rotation handle dragged and committed as one
//! entry, the green tint on.
//!
//! Every frame's state is checked against what the plan put there — which masks are listed and in
//! what order, the open mask, the selected component, the overlay setting and what the canvas draws,
//! and the canvas mode — so a row that moved or a selection that leaked is a failure named by its
//! step, as the `workspace` scenario checks its panels. Field texts are not asserted: the panel's
//! number formatting is its own business, so values are compared as numbers.
use crate::{
    scenario::{Checked, Checks, Frame, Plan, Run, Step, plan::only},
    *,
};
use luxforge_core::{BASIC_EFFECT, PRESENCE_EFFECT};
use luxforge_evidence::{
    self as script, DragHandle, KindMenuStep, MaskStep, PaintStep, Reference, WorkspaceStep,
};

pub const SCENARIO: &str = "mask-panel";
/// The photograph the design's boards are drawn over, so a frame compares with them side by side.
pub const FIXTURE: &str = "docs/design/develop-workspace/html/sapa.jpg";
/// What `reproduce.md` says about the run.
pub const NOTE: &str = "Opens the photograph the design boards are drawn over. Three masks built through the panel (Sky, Face of a radial, a subtracting brush and an intersecting luminance range, Foreground with its overlay hidden), Exposure and Clarity through Face, then the panel's states: the New mask menu, the selected component's fields, the overlay in each mode and tint, a hovered row, the Brush section armed and put down, and the draft bar for a stroke and for a radial. Compare `radial-dragged`, the last frame, with `docs/design/develop-workspace/mask-mode.png`.";

const LINEAR: &str = "linear";
const RADIAL: &str = "radial";
const LUMINANCE: &str = "luminance-range";
const MASK_MODE: &str = "mask";
const BASIC_MODULE: &str = "luxforge.basic";
const PRESENCE_MODULE: &str = "luxforge.presence";

const SKY: &str = "Sky";
const FACE: &str = "Face";
const FOREGROUND: &str = "Foreground";
const RADIAL_1: &str = "Radial 1";
const BRUSH_1: &str = "Brush 1";
const LUMINANCE_1: &str = "Luminance 1";
/// What the host names a new mask and a new luminance range here.
const NEW_MASK: &str = "Mask 1";
const NEW_LUMINANCE: &str = "Luminance range 1";

/// The photograph's aspect, `W/H`: 1440 × 1800.
const ASPECT: f64 = 1440.0 / 1800.0;
/// Face's radial as the board states it: its centre, as fractions of the photograph, and its two
/// radii in mask-space units, where one unit is the stage's height on both axes.
const RADIAL_AT: [f64; 2] = [0.52, 0.31];
const RADIUS: [f64; 2] = [0.18, 0.24];
/// Face's exposure and clarity, as the board's history names them.
const EXPOSURE: f64 = 0.6;
const CLARITY: f64 = 18.0;
/// Face's amount, as the board's row reads it.
const AMOUNT: f64 = 80.0;
/// The angle the board's radial is turned to, in degrees.
const ANGLE: f64 = -12.0;
/// How far right of the radial's centre the rotation drag is pressed, as a fraction of the width.
const LEVER: f64 = 0.2;

/// Where a sweep from the radial's centre ends so its radii are [`RADIUS`]: a sweep puts
/// `radius_x = |Δx| · W/H` and `radius_y = |Δy|` on the draft.
fn radial_to() -> [f64; 2] {
    [RADIAL_AT[0] + RADIUS[0] / ASPECT, RADIAL_AT[1] + RADIUS[1]]
}

/// A swing of the rotation grip that turns the radial by `degrees`: pressed level with the centre
/// and dragged, in two moves, to where a line from the centre at that angle crosses the same
/// vertical. The grip turns the ellipse by the angle the pointer swings through in mask space, where
/// a unit of width is `W/H` of a unit of height.
fn rotation(degrees: f64) -> Vec<[f64; 2]> {
    let [x, y] = RADIAL_AT;
    let rise = LEVER * ASPECT * degrees.to_radians().tan();
    vec![
        [x + LEVER, y],
        [x + LEVER, y + rise / 2.0],
        [x + LEVER, y + rise],
    ]
}

/// A step that commits nothing, in Mask mode.
fn quiet(name: &str, step: impl Into<script::Step>) -> Step {
    Step::new(name, step).commits(0).mode(MASK_MODE)
}

/// One committed entry, labelled as the history row reads it, in Mask mode.
fn entry(name: &str, step: impl Into<script::Step>, label: impl Into<String>) -> Step {
    Step::new(name, step)
        .commits(1)
        .label(label)
        .mode(MASK_MODE)
}

/// A new mask of one drawn kind, swept, committed by its release and renamed: four frames, two
/// entries.
/// Once renamed it is listed after `before`, open, and holds its one drawn component.
///
/// The host names a new mask with the first `Mask N` no mask holds, so once the one before it has
/// been renamed every new mask here is `Mask 1`. The first mask's entry says what it adds; every
/// later one names the mask it creates, as a history row does once there are masks to tell apart.
fn drawn(
    prefix: &str,
    kind: &str,
    sweep: [[f64; 2]; 2],
    added: &str,
    listed: &[&str],
    component: &str,
) -> [Step; 4] {
    let [from, to] = sweep;
    let name = listed[listed.len() - 1];
    [
        quiet(&format!("{prefix}-new"), MaskStep::New(kind.into())),
        quiet(&format!("{prefix}-swept"), MaskStep::Sweep { from, to }),
        entry(&format!("{prefix}-applied"), MaskStep::Release, added).no_draft(),
        entry(
            &format!("{prefix}-renamed"),
            script::Step::call(
                "mask.rename",
                json!({"mask": {"name": NEW_MASK}, "name": name}),
            ),
            format!("Rename {NEW_MASK} to {name}"),
        )
        .mask_names(listed)
        .open_mask(Some(name))
        .component_names(&[component])
        .components(&[&format!("add {kind}")]),
    ]
}

/// One request through Face, as the control that sends it addresses the open mask.
fn through_face(name: &str, method: &str, fields: Value, label: &str) -> Step {
    let mut params = json!({"mask": {"name": FACE}});
    params
        .as_object_mut()
        .expect("an object")
        .extend(fields.as_object().expect("an object").clone());
    entry(name, script::Step::call(method, params), label)
}

const THREE: &[&str] = &[SKY, FACE, FOREGROUND];
/// Face's rows once its three components are drawn: their names, and each one's mode and kind.
const FACE_NAMES: &[&str] = &[RADIAL_1, BRUSH_1, LUMINANCE_1];
const FACE_ROWS: &[&str] = &["add radial", "subtract brush", "intersect luminance-range"];

/// Face open with its three rows, among the three masks.
fn face(step: Step) -> Step {
    step.mask_names(THREE)
        .open_mask(Some(FACE))
        .component_names(FACE_NAMES)
        .components(FACE_ROWS)
}

/// Face open with Radial 1 selected and the overlay in `setting` and `colour`.
fn overlay(step: Step, setting: &str, colour: &str) -> Step {
    face(step)
        .selected_component(Some(RADIAL_1))
        .mask_overlay(setting, colour)
}

/// Every frame, in order: the open, then one per step, each with what the panel must show.
pub fn plan(_: &[PathBuf]) -> Plan {
    let mut steps = vec![
        Step::opened("opened")
            .no_layer(BASIC_EFFECT)
            .no_draft()
            .masks(0),
        // 1: Mask mode, through the same `workspace.set` the mode strip sends.
        quiet("mask-mode", WorkspaceStep::default().mode(MASK_MODE)),
    ];
    // 2-5: Sky, a linear from the top of the photograph down over its upper third.
    steps.extend(drawn(
        "sky",
        LINEAR,
        [[0.5, 0.0], [0.5, 0.32]],
        "Add linear",
        &[SKY],
        "Linear 1",
    ));
    // 6-9: Face, a radial at the board's centre and radii, with no Brush section yet.
    let [new, swept, applied, renamed] = drawn(
        "face",
        RADIAL,
        [RADIAL_AT, radial_to()],
        "Mask 1 · Add radial",
        &[SKY, FACE],
        RADIAL_1,
    );
    steps.extend([new, swept, applied, renamed.brush_section(false)]);
    let face_open = |step: Step| step.open_mask(Some(FACE));
    steps.extend([
        // 10-12: a subtracting brush on Face. Arming it shows the Brush section, and putting it down
        // before it has painted anything takes the section away again, leaving Radial 1 — selected
        // when it was applied — as it was.
        quiet("subtract-mode", MaskStep::Mode("subtract".into())),
        face_open(quiet("brush-armed", MaskStep::Paint(PaintStep::NewBrush))).brush_section(true),
        face_open(quiet("brush-put-down", MaskStep::Cancel))
            .no_draft()
            .component_names(&[RADIAL_1])
            .components(&["add radial"])
            .selected_component(Some(RADIAL_1))
            .brush_section(false),
        // 13-15: armed again, and two strokes, one entry each, taken out of the radial's lower part.
        quiet("brush-rearmed", MaskStep::Paint(PaintStep::NewBrush)),
        entry(
            "stroke-1",
            MaskStep::stroke([[0.44, 0.47], [0.60, 0.49]], true),
            "Face · Add subtract brush",
        ),
        face_open(entry(
            "stroke-2",
            MaskStep::stroke([[0.40, 0.42], [0.52, 0.44]], true),
            "Face · Update Brush 1",
        ))
        .component_names(&FACE_NAMES[..2])
        .components(&FACE_ROWS[..2])
        .selected_component(Some(BRUSH_1))
        .brush_section(true),
        // 16: a third stroke held down: the draft bar for a stroke.
        quiet(
            "stroke-held",
            MaskStep::stroke([[0.55, 0.20], [0.62, 0.22]], false),
        ),
        // 17: the brush put down, the held stroke with it. Put down after painting, the brush leaves
        // its section to the painted component the strokes selected, as the panel shows a brush
        // component's settings while it is selected.
        face_open(quiet("brush-down", MaskStep::Cancel))
            .no_draft()
            .selected_component(Some(BRUSH_1))
            .brush_section(true),
        // 18-19: an intersecting luminance range, typed rather than drawn, so it commits at once.
        quiet("intersect-mode", MaskStep::Mode("intersect".into())),
        entry(
            "luminance-added",
            MaskStep::Add(LUMINANCE.into()),
            "Face · Add intersect luminance range",
        ),
        // 20: the name the board gives it, through the rename field's own request.
        face(through_face(
            "luminance-renamed",
            "mask.rename-component",
            json!({"component": {"name": NEW_LUMINANCE}, "name": LUMINANCE_1}),
            &format!("Face · Rename {NEW_LUMINANCE} to {LUMINANCE_1}"),
        ))
        .mask_names(&[SKY, FACE]),
        // 21: back to add, which a new mask's first component must be.
        quiet("add-mode", MaskStep::Mode("add".into())),
    ]);
    // 22-25: Foreground, a linear from the bottom of the photograph up over its lower quarter.
    steps.extend(drawn(
        "foreground",
        LINEAR,
        [[0.5, 1.0], [0.5, 0.72]],
        "Mask 1 · Add linear",
        THREE,
        "Linear 1",
    ));
    steps.extend([
        // 26: Foreground's overlay hidden with its eye.
        quiet(
            "foreground-hidden",
            MaskStep::Eye(Reference::name(FOREGROUND)),
        ),
        // 27: Face open again, as clicking its row does.
        face(quiet("face-open", MaskStep::Select(Reference::name(FACE)))),
        // 28-30: Face's amount, then Exposure and Clarity through it, each the request the panel's
        // own controls send with the open mask as their target.
        through_face(
            "face-amount",
            "mask.set-amount",
            json!({ "amount": AMOUNT }),
            "Face · Amount 80",
        ),
        through_face(
            "exposure",
            "edit.set-basic",
            json!({ "exposure": EXPOSURE }),
            "Face · Exposure +0.60 EV",
        ),
        face(through_face(
            "clarity",
            "edit.set-presence",
            json!({ "clarity": CLARITY }),
            "Face · Clarity +18",
        ))
        .mask_overlay("off", "green"),
        // 31-32: Basic collapsed and Presence expanded, as the board lays the sections out.
        quiet(
            "basic-collapsed",
            script::Step::section(BASIC_MODULE, false),
        )
        .collapsed(BASIC_MODULE),
        quiet(
            "presence-expanded",
            script::Step::section(PRESENCE_MODULE, true),
        )
        .expanded(PRESENCE_MODULE)
        .collapsed(BASIC_MODULE),
        // 33-34: the New mask menu, and Escape putting it away.
        face(quiet(
            "new-mask-menu",
            MaskStep::Menu(KindMenuStep::NewMask),
        ))
        .mask_menu(Some("new_mask")),
        face(quiet(
            "menu-closed",
            script::Step::Key {
                key: script::KEY_ESCAPE.into(),
            },
        ))
        .mask_menu(None),
        // 35: Radial 1 selected: its fields open beneath its row, and no Brush section.
        face(quiet(
            "radial-selected",
            MaskStep::SelectComponent(Some(Reference::name(RADIAL_1))),
        ))
        .selected_component(Some(RADIAL_1))
        .brush_section(false),
        // 36-40: the overlay in each mode and both tints.
        overlay(
            quiet(
                "overlay-green",
                WorkspaceStep::default()
                    .mask_overlay("tint")
                    .mask_overlay_colour("green"),
            ),
            "tint",
            "green",
        ),
        overlay(
            quiet(
                "overlay-white",
                WorkspaceStep::default().mask_overlay_colour("white"),
            ),
            "tint",
            "white",
        ),
        overlay(
            quiet(
                "overlay-mask-on-black",
                WorkspaceStep::default().mask_overlay("mask-on-black"),
            ),
            "mask-on-black",
            "white",
        ),
        overlay(
            quiet(
                "overlay-image-on-black",
                WorkspaceStep::default().mask_overlay("image-on-black"),
            ),
            "image-on-black",
            "white",
        ),
        overlay(
            quiet("overlay-off", WorkspaceStep::default().mask_overlay("off")),
            "off",
            "white",
        ),
        // 41: the board's own tint.
        overlay(
            quiet(
                "overlay-tint",
                WorkspaceStep::default()
                    .mask_overlay("tint")
                    .mask_overlay_colour("green"),
            ),
            "tint",
            "green",
        ),
        // 42-43: the pointer on Brush 1's row, which shows its own contribution, and off again.
        overlay(
            quiet(
                "hover-brush",
                MaskStep::Hover(Some(Reference::name(BRUSH_1))),
            ),
            "tint",
            "green",
        ),
        overlay(quiet("hover-off", MaskStep::Hover(None)), "tint", "green"),
        // 44-45: Radial 1 selected, its handles resting on the canvas with no draft open, and its
        // rotation grip swung to the board's angle: one entry, committed on release.
        overlay(
            quiet(
                "radial-resting",
                MaskStep::SelectComponent(Some(Reference::name(RADIAL_1))),
            ),
            "tint",
            "green",
        )
        .no_draft(),
        overlay(
            entry(
                "radial-dragged",
                MaskStep::Drag {
                    handle: DragHandle::Rotation,
                    points: rotation(ANGLE),
                    release: true,
                },
                format!("{FACE} · Update {RADIAL_1}"),
            ),
            "tint",
            "green",
        )
        .no_draft(),
    ]);
    Plan::new(steps)
}

/// The draft bar a frame drew, checked to lead with Face in the accent and to name `subject` — the
/// component and its mode — beside the icon of `kind`. Its readout is not read: the numbers are the
/// panel's formatting, and the shape itself is compared as numbers.
fn draft_bar<'a>(frame: &'a Frame, subject: &str, kind: &str) -> Result<&'a Value> {
    let bar = &frame.state()["draft_bar"];
    ensure(
        bar["title"] == json!(FACE)
            && bar["subject"] == json!(subject)
            && bar["kind"] == json!(kind),
        format!("The draft bar is {bar}, expected {FACE} · {subject} with the {kind} icon"),
    )?;
    Ok(bar)
}

/// One listed mask's row, by name.
fn mask_row<'a>(frame: &'a Frame, name: &str) -> Result<&'a Value> {
    frame
        .masks()?
        .iter()
        .find(|mask| mask["name"] == json!(name))
        .ok_or_else(|| format!("No mask named {name} is listed").into())
}

/// The status line names Face and `component`.
fn names_face(frame: &Frame, component: &str) -> Result<Value> {
    let status = frame.state()["status_bar"]["message"].clone();
    ensure(
        status
            .as_str()
            .is_some_and(|line| line.contains(FACE) && line.contains(component)),
        format!("The status line does not name {FACE} and {component}: {status}"),
    )?;
    Ok(status)
}

/// The one launch, once it has held its plan — which holds every frame's placement: the masks in
/// order, the open one, its components, the selected row, the overlay, the menu, the Brush section
/// and the canvas mode — then what each named frame is evidence of.
pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let mut checks = Checks::new();

    // The Brush section appears when a brush is armed on Face and goes when it is put down.
    let armed = launch.at("brush-armed")?;
    ensure(
        armed.state()["masks"]["brush"]["armed"] == json!(true),
        "Arming the brush left it unarmed",
    )?;
    let put_down = launch.at("brush-put-down")?;
    ensure(
        put_down.state()["masks"]["brush"]["armed"] == json!(false)
            && put_down.state()["draft_bar"].is_null(),
        "Putting the brush down left it armed, or its draft bar drawn",
    )?;
    let down = launch.at("brush-down")?;
    ensure(
        down.state()["masks"]["brush"]["armed"] == json!(false)
            && down.state()["draft_bar"].is_null(),
        "Putting the brush down after the held stroke left it armed, or its draft bar drawn",
    )?;

    // The draft bar for a stroke: the stroke is down on Face's brush, and the bar ends with Done.
    let held = launch.at("stroke-held")?;
    let stroke_bar = draft_bar(held, "Brush 1 · Subtract", "brush")?;
    ensure(
        stroke_bar["done"] == json!(true),
        format!("The stroke's draft bar offers no Done: {stroke_bar}"),
    )?;
    let stroke = &held.state()["mask_draft"];
    ensure(
        stroke["stroke"]["painting"] == json!(true),
        format!("The held stroke is not painting: {stroke}"),
    )?;
    let status = names_face(held, BRUSH_1)?;
    checks.note(
        held,
        "the draft bar for a stroke held down on Face's brush",
        json!({"bar": stroke_bar, "status": status}),
    );

    // Foreground's eye: its overlay hidden, the mask still applied, and every eye as it was left at
    // the end.
    let hidden = launch.at("foreground-hidden")?;
    ensure(
        mask_row(hidden, FOREGROUND)?["visible"] == json!(false),
        "Foreground's eye left its overlay visible",
    )?;
    let last = launch.at("radial-dragged")?;
    for (name, visible) in [(SKY, true), (FACE, true), (FOREGROUND, false)] {
        ensure(
            mask_row(last, name)?["visible"] == json!(visible),
            format!("{name}'s eye changed after the press"),
        )?;
    }

    // Face's amount, dot and bound layers; the sections bound to it carry its chip.
    let clarity = launch.at("clarity")?;
    let face = mask_row(clarity, FACE)?;
    ensure(
        face["non_neutral"] == json!(true),
        format!("Face shows no edited dot: {face}"),
    )?;
    let basic = clarity
        .layer(BASIC_EFFECT)
        .ok_or("No Basic layer was committed through Face")?;
    let presence = clarity
        .layer(PRESENCE_EFFECT)
        .ok_or("No Presence layer was committed through Face")?;
    ensure(
        basic["mask"] == face["id"] && presence["mask"] == face["id"],
        "The Basic and Presence layers are not bound to Face",
    )?;
    ensure(
        (basic["payload"]["exposure"].as_f64().unwrap_or(f64::NAN) - EXPOSURE).abs() < 1e-9
            && (presence["payload"]["clarity"].as_f64().unwrap_or(f64::NAN) - CLARITY).abs() < 1e-9,
        format!(
            "Face's layers hold {} and {}",
            basic["payload"], presence["payload"]
        ),
    )?;
    let scopes = &clarity.state()["scopes"];
    for module in [BASIC_MODULE, PRESENCE_MODULE] {
        ensure(
            scopes[module] == json!(FACE),
            format!("{module}'s band carries no Face chip: {scopes}"),
        )?;
    }

    // Radial 1 selected: its fields open beneath the row, at the geometry it was swept to.
    let fields = &launch.at("radial-selected")?.component(0)?["fields"];
    for (field, want) in [
        ("x", RADIAL_AT[0]),
        ("y", RADIAL_AT[1]),
        ("radius_x", RADIUS[0]),
        ("radius_y", RADIUS[1]),
    ] {
        let value = fields[field].as_f64().unwrap_or(f64::NAN);
        ensure(
            (value - want).abs() < 0.005,
            format!("Radial 1's {field} field reads {value}, expected about {want}"),
        )?;
    }

    // The overlay control in each mode and both tints, drawn as the setting asks.
    for step in [
        "overlay-green",
        "overlay-white",
        "overlay-mask-on-black",
        "overlay-image-on-black",
        "overlay-off",
        "overlay-tint",
    ] {
        let overlay = &launch.at(step)?.state()["mask_overlay"];
        ensure(
            overlay["effective"] == overlay["setting"] && overlay["forced"] == json!(false),
            format!("Step {step}: the canvas draws {overlay}"),
        )?;
    }

    // A hover on Brush 1's row: only that row is hovered, and none once the pointer is off.
    let hovered = |step: &str| -> Result<Vec<String>> {
        Ok(launch
            .at(step)?
            .components()?
            .iter()
            .filter(|component| component["hovered"] == json!(true))
            .map(|component| component["name"].as_str().unwrap_or_default().to_owned())
            .collect())
    };
    ensure(
        hovered("hover-brush")? == [BRUSH_1],
        format!("The hovered rows are {:?}", hovered("hover-brush")?),
    )?;
    ensure(
        hovered("hover-off")?.is_empty(),
        "A row is still hovered with the pointer off the list",
    )?;

    // Radial 1's handles at rest on Face, holding no draft, and its grip swung to the board's
    // angle and committed on release with nothing else about it moved. The handles rest again on
    // what was committed.
    let resting = launch.at("radial-resting")?;
    let handles = |name: &str, frame: &Frame| -> Result<Value> {
        let handles = &frame.state()["mask_handles"];
        ensure(
            handles["summary"]["kind"] == json!(RADIAL)
                && handles["summary"]["op"] == json!("Update")
                && handles["mapped"] == json!(true),
            format!("{name}: Radial 1's handles are not resting: {handles}"),
        )?;
        ensure(
            frame.state()["draft_bar"].is_null(),
            format!("{name}: resting handles drew a draft bar"),
        )?;
        Ok(handles["summary"]["shape"].clone())
    };
    let before = handles("radial-resting", resting)?;
    let shape = handles("radial-dragged", last)?;
    let angle = shape["angle"].as_f64().unwrap_or(f64::NAN);
    ensure(
        (angle - ANGLE).abs() < 0.5,
        format!("The grip turned the radial to {angle}°, expected about {ANGLE}°"),
    )?;
    let fields = &last.component(0)?["fields"];
    for field in ["x", "y", "radius_x", "radius_y", "feather"] {
        ensure(
            shape[field] == before[field],
            format!(
                "Swinging the grip moved {field} from {} to {}",
                before[field], shape[field]
            ),
        )?;
    }
    let stored = fields["angle"].as_f64().unwrap_or(f64::NAN);
    ensure(
        (stored - angle).abs() < 0.5,
        format!("Radial 1's committed angle reads {stored}, its handles {angle}"),
    )?;
    checks.note(
        last,
        "Face's Radial 1 with its resting rotation handle dragged and committed: the mask-mode board's state",
        json!({"shape": shape, "before": before}),
    );

    checks.write(
        run.out(),
        SCENARIO,
        json!({
            "board": {"compare": "radial-dragged", "with": "docs/design/develop-workspace/mask-mode.png"},
            "density": "not checked: no frame records the tools panel's content height",
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plan_builds_the_board() {
        let plan = plan(&[]);
        plan.validate().unwrap();
        let names: Vec<&str> = plan.steps().iter().map(|step| step.name()).collect();
        assert_eq!(names.last(), Some(&"radial-dragged"));
        // The radial is swept to the board's radii.
        let to = radial_to();
        assert!(((to[0] - RADIAL_AT[0]) * ASPECT - RADIUS[0]).abs() < 1e-12);
        assert!(((to[1] - RADIAL_AT[1]) - RADIUS[1]).abs() < 1e-12);
    }
}
