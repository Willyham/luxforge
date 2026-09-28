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
//! stroke and for a radial. Its last frame, `radial-dragged`, is the board's own state: `Face` open,
//! `Radial 1` reopened and its rotation handle dragged, the green tint on.
//!
//! Every frame's state is checked against what the plan put there — which masks are listed and in
//! what order, the open mask, the selected component, the overlay setting and what the canvas draws,
//! and the canvas mode — so a row that moved or a selection that leaked is a failure named by its
//! step, as the `workspace` scenario checks its panels. Field texts are not asserted: the panel's
//! number formatting is its own business, so values are compared as numbers.
use crate::{
    scenario::{Checked, Frame, Plan, Run, Step, plan::only},
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

/// A step that commits nothing.
fn quiet(name: &str, step: impl Into<script::Step>) -> Step {
    Step::new(name, step).commits(0)
}

/// One committed entry, labelled as the history row reads it.
fn entry(name: &str, step: impl Into<script::Step>, label: impl Into<String>) -> Step {
    Step::new(name, step).commits(1).label(label)
}

/// A new mask of one drawn kind, swept, released, committed and renamed: five frames, two entries.
///
/// The host names a new mask with the first `Mask N` no mask holds, so once the one before it has
/// been renamed every new mask here is `Mask 1`. The first mask's entry says what it adds; every
/// later one names the mask it creates, as a history row does once there are masks to tell apart.
fn drawn(prefix: &str, kind: &str, sweep: [[f64; 2]; 2], added: &str, name: &str) -> [Step; 5] {
    let [from, to] = sweep;
    [
        quiet(&format!("{prefix}-new"), MaskStep::New(kind.into())),
        quiet(&format!("{prefix}-swept"), MaskStep::Sweep { from, to }),
        quiet(&format!("{prefix}-released"), MaskStep::Release),
        entry(&format!("{prefix}-applied"), MaskStep::Apply, added).no_draft(),
        entry(
            &format!("{prefix}-renamed"),
            script::Step::call(
                "mask.rename",
                json!({"mask": {"name": NEW_MASK}, "name": name}),
            ),
            format!("Rename {NEW_MASK} to {name}"),
        ),
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

/// Every frame, in order: the open, then one per step.
pub fn plan(_: &[PathBuf]) -> Plan {
    let mut steps = vec![
        Step::opened("opened").no_layer(BASIC_EFFECT).no_draft(),
        // 1: Mask mode, through the same `workspace.set` the mode strip sends.
        quiet("mask-mode", WorkspaceStep::default().mode(MASK_MODE)),
    ];
    // 2-6: Sky, a linear from the top of the photograph down over its upper third.
    steps.extend(drawn(
        "sky",
        LINEAR,
        [[0.5, 0.0], [0.5, 0.32]],
        "Add linear",
        SKY,
    ));
    // 7-11: Face, a radial at the board's centre and radii.
    steps.extend(drawn(
        "face",
        RADIAL,
        [RADIAL_AT, radial_to()],
        "Mask 1 · Add radial",
        FACE,
    ));
    steps.extend([
        // 12-14: a subtracting brush on Face. Arming it shows the Brush section, and putting it down
        // before it has painted anything takes the section away again.
        quiet("subtract-mode", MaskStep::Mode("subtract".into())),
        quiet("brush-armed", MaskStep::Paint(PaintStep::NewBrush)),
        quiet("brush-put-down", MaskStep::Cancel).no_draft(),
        // 15-17: armed again, and two strokes, one entry each, taken out of the radial's lower part.
        quiet("brush-rearmed", MaskStep::Paint(PaintStep::NewBrush)),
        entry(
            "stroke-1",
            MaskStep::stroke([[0.44, 0.47], [0.60, 0.49]], true),
            "Face · Add subtract brush",
        ),
        entry(
            "stroke-2",
            MaskStep::stroke([[0.40, 0.42], [0.52, 0.44]], true),
            "Face · Update Brush 1",
        ),
        // 18: a third stroke held down: the draft bar for a stroke.
        quiet(
            "stroke-held",
            MaskStep::stroke([[0.55, 0.20], [0.62, 0.22]], false),
        ),
        // 19: the brush put down, the held stroke with it: the Brush section goes.
        quiet("brush-down", MaskStep::Cancel).no_draft(),
        // 20-21: an intersecting luminance range, typed rather than drawn, so it commits at once.
        quiet("intersect-mode", MaskStep::Mode("intersect".into())),
        entry(
            "luminance-added",
            MaskStep::Add(LUMINANCE.into()),
            "Face · Add intersect luminance range",
        ),
        // 22: the name the board gives it, through the rename field's own request.
        through_face(
            "luminance-renamed",
            "mask.rename-component",
            json!({"component": {"name": NEW_LUMINANCE}, "name": LUMINANCE_1}),
            &format!("Face · Rename {NEW_LUMINANCE} to {LUMINANCE_1}"),
        ),
        // 23: back to add, which a new mask's first component must be.
        quiet("add-mode", MaskStep::Mode("add".into())),
    ]);
    // 24-28: Foreground, a linear from the bottom of the photograph up over its lower quarter.
    steps.extend(drawn(
        "foreground",
        LINEAR,
        [[0.5, 1.0], [0.5, 0.72]],
        "Mask 1 · Add linear",
        FOREGROUND,
    ));
    steps.extend([
        // 29: Foreground's overlay hidden with its eye.
        quiet(
            "foreground-hidden",
            MaskStep::Eye(Reference::name(FOREGROUND)),
        ),
        // 30: Face open again, as clicking its row does.
        quiet("face-open", MaskStep::Select(Reference::name(FACE))),
        // 31-33: Face's amount, then Exposure and Clarity through it, each the request the panel's
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
        through_face(
            "clarity",
            "edit.set-presence",
            json!({ "clarity": CLARITY }),
            "Face · Clarity +18",
        ),
        // 34-35: Basic collapsed and Presence expanded, as the board lays the sections out.
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
        // 36-37: the New mask menu, and Escape putting it away.
        quiet("new-mask-menu", MaskStep::Menu(KindMenuStep::NewMask)),
        quiet(
            "menu-closed",
            script::Step::Key {
                key: script::KEY_ESCAPE.into(),
            },
        ),
        // 38: Radial 1 selected: its fields open beneath its row.
        quiet(
            "radial-selected",
            MaskStep::SelectComponent(Some(Reference::name(RADIAL_1))),
        ),
        // 39-43: the overlay in each mode and both tints.
        quiet(
            "overlay-green",
            WorkspaceStep::default()
                .mask_overlay("tint")
                .mask_overlay_colour("green"),
        ),
        quiet(
            "overlay-white",
            WorkspaceStep::default().mask_overlay_colour("white"),
        ),
        quiet(
            "overlay-mask-on-black",
            WorkspaceStep::default().mask_overlay("mask-on-black"),
        ),
        quiet(
            "overlay-image-on-black",
            WorkspaceStep::default().mask_overlay("image-on-black"),
        ),
        quiet("overlay-off", WorkspaceStep::default().mask_overlay("off")),
        // 44: the board's own tint.
        quiet(
            "overlay-tint",
            WorkspaceStep::default()
                .mask_overlay("tint")
                .mask_overlay_colour("green"),
        ),
        // 45-46: the pointer on Brush 1's row, which shows its own contribution, and off again.
        quiet(
            "hover-brush",
            MaskStep::Hover(Some(Reference::name(BRUSH_1))),
        ),
        quiet("hover-off", MaskStep::Hover(None)),
        // 47-48: Radial 1's shape reopened and its rotation grip swung to the board's angle: the
        // draft bar for a radial, and the board's own state.
        quiet("shape-open", MaskStep::EditShape(Reference::name(RADIAL_1))),
        quiet(
            "radial-dragged",
            MaskStep::Drag {
                handle: DragHandle::Rotation,
                points: rotation(ANGLE),
            },
        ),
    ]);
    Plan::new(steps)
}

/// The listed masks' names, in list order.
fn mask_names(frame: &Frame) -> Result<Vec<String>> {
    Ok(frame
        .masks()?
        .iter()
        .map(|mask| mask["name"].as_str().unwrap_or_default().to_owned())
        .collect())
}

/// The open mask's name, or `None` when no mask is open.
fn open_mask(frame: &Frame) -> Result<Option<String>> {
    let selected = &frame.state()["masks"]["selected"];
    if selected.is_null() {
        return Ok(None);
    }
    Ok(frame
        .masks()?
        .iter()
        .find(|mask| &mask["id"] == selected)
        .map(|mask| mask["name"].as_str().unwrap_or_default().to_owned()))
}

/// The open mask's components as `name mode`, in list order.
fn components(frame: &Frame) -> Result<Vec<String>> {
    Ok(frame
        .components()?
        .iter()
        .map(|component| {
            format!(
                "{} {}",
                component["name"].as_str().unwrap_or_default(),
                component["mode"].as_str().unwrap_or_default()
            )
        })
        .collect())
}

/// The component whose row is selected, by name.
fn selected_component(frame: &Frame) -> Result<Option<String>> {
    Ok(frame
        .components()?
        .iter()
        .find(|component| component["selected"] == json!(true))
        .map(|component| component["name"].as_str().unwrap_or_default().to_owned()))
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

/// What one frame's panel placed where: the session's open mask, selected component, overlay and
/// mode beside the listed rows. Every frame records it, and the checks below compare it with what
/// the plan put there.
struct Placement {
    masks: Vec<String>,
    open: Option<String>,
    components: Vec<String>,
    selected: Option<String>,
    overlay: String,
    colour: String,
    effective: String,
    forced: bool,
    mode: String,
    menu: Value,
    brush_visible: bool,
}

impl Placement {
    fn of(frame: &Frame) -> Result<Self> {
        let state = frame.state();
        let text = |value: &Value| value.as_str().unwrap_or_default().to_owned();
        Ok(Self {
            masks: mask_names(frame)?,
            open: open_mask(frame)?,
            components: components(frame)?,
            selected: selected_component(frame)?,
            overlay: text(&state["mask_overlay"]["setting"]),
            colour: text(&state["masks"]["overlay_colour"]),
            effective: text(&state["mask_overlay"]["effective"]),
            forced: state["mask_overlay"]["forced"] == json!(true),
            mode: text(&state["workspace"]["mode"]),
            menu: state["masks"]["menu"].clone(),
            brush_visible: state["masks"]["brush_visible"] == json!(true),
        })
    }

    fn record(&self) -> Value {
        json!({"masks":self.masks,"open":self.open,"components":self.components,
               "selected":self.selected,"overlay":self.overlay,"colour":self.colour,
               "effective":self.effective,"forced":self.forced,"mode":self.mode,
               "menu":self.menu,"brush_visible":self.brush_visible})
    }
}

/// What one frame must show of the panel: the masks in order, the open one, its components, the
/// selected row and the overlay. `None` leaves that part unchecked.
#[derive(Default)]
struct Want {
    masks: Option<&'static [&'static str]>,
    open: Option<Option<&'static str>>,
    components: Option<&'static [&'static str]>,
    selected: Option<Option<&'static str>>,
    overlay: Option<(&'static str, &'static str)>,
    menu: Option<Value>,
    brush_visible: Option<bool>,
}

impl Want {
    fn check(&self, step: &str, at: &Placement) -> Result {
        let fail = |what: &str, shown: &dyn std::fmt::Debug, want: &dyn std::fmt::Debug| {
            format!("Step {step}: {what} is {shown:?}, expected {want:?}")
        };
        if let Some(masks) = self.masks {
            ensure(at.masks == masks, fail("the mask list", &at.masks, &masks))?;
        }
        if let Some(open) = self.open {
            ensure(
                at.open.as_deref() == open,
                fail("the open mask", &at.open, &open),
            )?;
        }
        if let Some(components) = self.components {
            ensure(
                at.components == components,
                fail("the component list", &at.components, &components),
            )?;
        }
        if let Some(selected) = self.selected {
            ensure(
                at.selected.as_deref() == selected,
                fail("the selected component", &at.selected, &selected),
            )?;
        }
        if let Some((overlay, colour)) = self.overlay {
            ensure(
                at.overlay == overlay && at.colour == colour,
                fail(
                    "the overlay",
                    &(&at.overlay, &at.colour),
                    &(overlay, colour),
                ),
            )?;
        }
        if let Some(menu) = &self.menu {
            ensure(&at.menu == menu, fail("the open menu", &at.menu, menu))?;
        }
        if let Some(visible) = self.brush_visible {
            ensure(
                at.brush_visible == visible,
                fail("the Brush section", &at.brush_visible, &visible),
            )?;
        }
        ensure(
            at.mode == MASK_MODE || step == "opened",
            fail("the canvas mode", &at.mode, &MASK_MODE),
        )
    }
}

const THREE: &[&str] = &[SKY, FACE, FOREGROUND];
const FACE_ROWS: &[&str] = &["Radial 1 add", "Brush 1 subtract", "Luminance 1 intersect"];

/// Each step's placement, as the plan put it there.
fn wants() -> Vec<(&'static str, Want)> {
    let face = || Want {
        masks: Some(THREE),
        open: Some(Some(FACE)),
        components: Some(FACE_ROWS),
        ..Want::default()
    };
    let overlay = |mode, colour| Want {
        selected: Some(Some(RADIAL_1)),
        overlay: Some((mode, colour)),
        ..face()
    };
    vec![
        (
            "sky-renamed",
            Want {
                masks: Some(&[SKY]),
                open: Some(Some(SKY)),
                components: Some(&["Linear 1 add"]),
                ..Want::default()
            },
        ),
        (
            "face-renamed",
            Want {
                masks: Some(&[SKY, FACE]),
                open: Some(Some(FACE)),
                components: Some(&["Radial 1 add"]),
                brush_visible: Some(false),
                ..Want::default()
            },
        ),
        (
            "brush-armed",
            Want {
                open: Some(Some(FACE)),
                brush_visible: Some(true),
                ..Want::default()
            },
        ),
        (
            "brush-put-down",
            Want {
                open: Some(Some(FACE)),
                components: Some(&["Radial 1 add"]),
                selected: Some(None),
                brush_visible: Some(false),
                ..Want::default()
            },
        ),
        (
            "stroke-2",
            Want {
                open: Some(Some(FACE)),
                components: Some(&["Radial 1 add", "Brush 1 subtract"]),
                selected: Some(Some(BRUSH_1)),
                brush_visible: Some(true),
                ..Want::default()
            },
        ),
        // Put down after painting, the brush leaves its section to the painted component the
        // strokes selected, as the panel shows a brush component's settings while it is selected.
        (
            "brush-down",
            Want {
                open: Some(Some(FACE)),
                selected: Some(Some(BRUSH_1)),
                brush_visible: Some(true),
                ..Want::default()
            },
        ),
        (
            "luminance-renamed",
            Want {
                masks: Some(&[SKY, FACE]),
                ..face()
            },
        ),
        (
            "foreground-renamed",
            Want {
                masks: Some(THREE),
                open: Some(Some(FOREGROUND)),
                components: Some(&["Linear 1 add"]),
                ..Want::default()
            },
        ),
        ("face-open", face()),
        (
            "clarity",
            Want {
                overlay: Some(("off", "green")),
                ..face()
            },
        ),
        (
            "new-mask-menu",
            Want {
                menu: Some(json!("new_mask")),
                ..face()
            },
        ),
        (
            "menu-closed",
            Want {
                menu: Some(Value::Null),
                ..face()
            },
        ),
        (
            "radial-selected",
            Want {
                selected: Some(Some(RADIAL_1)),
                brush_visible: Some(false),
                ..face()
            },
        ),
        ("overlay-green", overlay("tint", "green")),
        ("overlay-white", overlay("tint", "white")),
        ("overlay-mask-on-black", overlay("mask-on-black", "white")),
        ("overlay-image-on-black", overlay("image-on-black", "white")),
        ("overlay-off", overlay("off", "white")),
        ("overlay-tint", overlay("tint", "green")),
        ("hover-brush", overlay("tint", "green")),
        ("hover-off", overlay("tint", "green")),
        ("shape-open", overlay("tint", "green")),
        ("radial-dragged", overlay("tint", "green")),
    ]
}

/// The one launch, once it has held its plan: every frame's placement, then what each named frame
/// is evidence of.
pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let checks = checks(only(launches)?)?;
    run.record("checks", checks.clone());
    write_json(&run.out().join("mask-panel-checks.json"), &checks)?;
    Ok(())
}

fn checks(launch: &Checked) -> Result<Value> {
    let mut frames = Vec::new();
    let mut shows = Vec::new();
    let mut record = |frame: &Frame, what: &str, detail: Value| {
        shows.push(json!({"frame":frame["file"],"shows":what,"detail":detail}));
    };

    // Every frame's placement, recorded, and checked where the plan put something.
    let wants = wants();
    for name in launch.names() {
        let frame = launch.at(name)?;
        let at = Placement::of(frame)?;
        if let Some((_, want)) = wants.iter().find(|(step, _)| step == name) {
            want.check(name, &at)?;
        }
        frames.push(
            json!({"step":name,"frame":frame["file"],"placement":at.record(),
            "canvas_rect":frame["canvas_rect"],"surface_columns":frame["surface_columns"]}),
        );
    }

    // The opened photograph, with no mask in the recipe.
    let opened = launch.at("opened")?;
    ensure(
        opened.masks()?.is_empty(),
        "The photograph opened with a mask",
    )?;

    // The Brush section appears when a brush is armed on Face and goes when it is put down.
    let armed = launch.at("brush-armed")?;
    ensure(
        armed.state()["masks"]["brush"]["armed"] == json!(true),
        "Arming the brush left it unarmed",
    )?;
    record(
        armed,
        "the Brush section, shown because a brush is armed on Face",
        armed.state()["masks"]["brush"].clone(),
    );
    let put_down = launch.at("brush-put-down")?;
    ensure(
        put_down.state()["masks"]["brush"]["armed"] == json!(false)
            && put_down.state()["draft_bar"].is_null(),
        "Putting the brush down left it armed, or its draft bar drawn",
    )?;
    record(
        put_down,
        "the brush put down before it painted: the Brush section is gone",
        json!({"brush_visible": false}),
    );
    let down = launch.at("brush-down")?;
    ensure(
        down.state()["masks"]["brush"]["armed"] == json!(false),
        "Putting the brush down after the held stroke left it armed",
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
    let stroke_status = held.state()["status_bar"]["message"].clone();
    ensure(
        stroke_status
            .as_str()
            .is_some_and(|line| line.contains(FACE) && line.contains(BRUSH_1)),
        format!("The held stroke's status line does not name Face and Brush 1: {stroke_status}"),
    )?;
    record(
        held,
        "the draft bar for a stroke held down on Face's brush",
        json!({"bar":stroke_bar,"status":stroke_status}),
    );
    ensure(
        down.state()["draft_bar"].is_null(),
        "The brush put down left its draft bar",
    )?;

    // Foreground's eye: its overlay hidden, the mask still applied.
    let hidden = launch.at("foreground-hidden")?;
    let row = mask_row(hidden, FOREGROUND)?;
    ensure(
        row["visible"] == json!(false),
        "Foreground's eye left its overlay visible",
    )?;
    for (name, visible) in [(SKY, true), (FACE, true), (FOREGROUND, false)] {
        let last = launch.at("radial-dragged")?;
        ensure(
            mask_row(last, name)?["visible"] == json!(visible),
            format!("{name}'s eye changed after the press"),
        )?;
    }
    record(
        hidden,
        "Foreground's overlay hidden with its eye",
        row.clone(),
    );

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
    record(
        clarity,
        "Exposure and Clarity through Face: the dot on its row and Face's chip on the bound sections",
        json!({"face":face,"scopes":scopes}),
    );

    // The New mask menu, open and then put away.
    record(
        launch.at("new-mask-menu")?,
        "the New mask menu open",
        launch.at("new-mask-menu")?.state()["masks"]["kinds"].clone(),
    );

    // Radial 1 selected: its fields open beneath the row, at the geometry it was swept to.
    let selected = launch.at("radial-selected")?;
    let fields = &selected.component(0)?["fields"];
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
    record(
        selected,
        "Radial 1 selected: its fields beneath its row",
        fields.clone(),
    );

    // The overlay control in each mode and both tints, drawn as the setting asks.
    for step in [
        "overlay-green",
        "overlay-white",
        "overlay-mask-on-black",
        "overlay-image-on-black",
        "overlay-off",
        "overlay-tint",
    ] {
        let frame = launch.at(step)?;
        let overlay = &frame.state()["mask_overlay"];
        ensure(
            overlay["effective"] == overlay["setting"] && overlay["forced"] == json!(false),
            format!("Step {step}: the canvas draws {overlay}"),
        )?;
        record(
            frame,
            "the overlay control in one of its modes",
            json!({"overlay":overlay,"colour":frame.state()["masks"]["overlay_colour"]}),
        );
    }

    // A hover on Brush 1's row: only that row is hovered.
    let hover = launch.at("hover-brush")?;
    let hovered: Vec<&str> = hover
        .components()?
        .iter()
        .filter(|component| component["hovered"] == json!(true))
        .map(|component| component["name"].as_str().unwrap_or_default())
        .collect();
    ensure(
        hovered == [BRUSH_1],
        format!("The hovered rows are {hovered:?}"),
    )?;
    record(
        hover,
        "the pointer on Brush 1's row: the overlay shows its own contribution",
        json!({"hovered":hovered}),
    );
    let off = launch.at("hover-off")?;
    ensure(
        off.components()?
            .iter()
            .all(|component| component["hovered"] == json!(false)),
        "A row is still hovered with the pointer off the list",
    )?;

    // The draft bar for a radial: Radial 1 reopened on Face, and its grip swung to the board's
    // angle with nothing else about it moved.
    let opened_shape = launch.at("shape-open")?;
    draft_bar(opened_shape, "Radial 1 · Add", RADIAL)?;
    let dragged = launch.at("radial-dragged")?;
    let bar = draft_bar(dragged, "Radial 1 · Add", RADIAL)?;
    ensure(
        bar["done"] == json!(false) && bar["can_apply"] == json!(true),
        format!("The radial's draft bar cannot apply: {bar}"),
    )?;
    let shape = &dragged.state()["mask_draft"]["shape"];
    let before = &opened_shape.state()["mask_draft"]["shape"];
    let angle = shape["angle"].as_f64().unwrap_or(f64::NAN);
    ensure(
        (angle - ANGLE).abs() < 0.5,
        format!("The grip turned the radial to {angle}°, expected about {ANGLE}°"),
    )?;
    for field in ["x", "y", "radius_x", "radius_y", "feather"] {
        ensure(
            shape[field] == before[field],
            format!(
                "Swinging the grip moved {field} from {} to {}",
                before[field], shape[field]
            ),
        )?;
    }
    let status = dragged.state()["status_bar"]["message"].clone();
    ensure(
        status
            .as_str()
            .is_some_and(|line| line.contains(FACE) && line.contains(RADIAL_1)),
        format!("The radial's status line does not name Face and Radial 1: {status}"),
    )?;
    ensure(
        components(dragged)? == FACE_ROWS,
        "Reopening the radial changed Face's rows",
    )?;
    record(
        dragged,
        "the draft bar for Face's Radial 1 with its handle dragged: the mask-mode board's state",
        json!({"bar":bar,"shape":shape,"status":status}),
    );

    Ok(json!({
        "frames": frames,
        "shows": shows,
        "board": {"compare": "radial-dragged", "with": "docs/design/develop-workspace/mask-mode.png"},
        "density": "not checked: no frame records the tools panel's content height",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plan_builds_the_board_and_names_every_placement_it_checks() {
        let plan = plan(&[]);
        plan.validate().unwrap();
        let names: Vec<&str> = plan.steps().iter().map(|step| step.name()).collect();
        for (step, _) in wants() {
            assert!(names.contains(&step), "{step} is checked but not planned");
        }
        assert_eq!(names.last(), Some(&"radial-dragged"));
        // The radial is swept to the board's radii.
        let to = radial_to();
        assert!(((to[0] - RADIAL_AT[0]) * ASPECT - RADIUS[0]).abs() < 1e-12);
        assert!(((to[1] - RADIAL_AT[1]) - RADIUS[1]).abs() < 1e-12);
    }

    #[test]
    fn a_placement_the_plan_did_not_put_there_fails_naming_its_step() {
        let frame = Frame::state_only(&json!({"state":{
            "masks":{"masks":[{"id":"a","name":"Sky"},{"id":"b","name":"Face"}],
                     "selected":"b","components":[{"name":"Radial 1","mode":"add","selected":true}],
                     "overlay_colour":"green","menu":null,"brush_visible":false},
            "mask_overlay":{"setting":"tint","effective":"tint","forced":false},
            "workspace":{"mode":"mask"}
        }}));
        let at = Placement::of(&frame).unwrap();
        assert_eq!(at.open.as_deref(), Some("Face"));
        assert_eq!(at.selected.as_deref(), Some("Radial 1"));
        let want = Want {
            masks: Some(&[SKY, FACE]),
            open: Some(Some(FACE)),
            components: Some(&["Radial 1 add"]),
            selected: Some(Some(RADIAL_1)),
            overlay: Some(("tint", "green")),
            menu: Some(Value::Null),
            brush_visible: Some(false),
        };
        want.check("s", &at).unwrap();
        let error = Want {
            masks: Some(THREE),
            ..Want::default()
        }
        .check("s", &at)
        .unwrap_err()
        .to_string();
        assert!(error.starts_with("Step s: the mask list"), "{error}");
        let error = Want {
            overlay: Some(("tint", "white")),
            ..Want::default()
        }
        .check("s", &at)
        .unwrap_err()
        .to_string();
        assert!(error.contains("the overlay"), "{error}");
    }
}
