//! The `mask-combine` smoke scenario: phase B's claim, which is that combining mask kinds and
//! modes in one mask is ordinary.
//!
//! One mask is built up on the canvas out of four radial components — an `add`, a `subtract`, an
//! `intersect` and a second `add` — through the Add row's mode control and the gesture that
//! follows it. After each commit the coverage overlay is read at four fixed points of the
//! photograph, so what the composition algebra says is checked against the pixels the editor drew
//! rather than against the state it recorded. Each component's own contribution is then read the
//! same way, by putting the pointer on its row. A reorder moves the second `add` above the
//! subtraction, which is the one reorder this list admits that changes the picture, and the
//! coverage follows. Then the overlay goes off, a Presence adjustment is dragged through the mask,
//! and one undo takes it back.
//!
//! **Why the overlay is what is measured.** `mask-on-black` paints the composed coverage as an
//! opaque greyscale at the display's own resolution, so a point at coverage 1 reads white and one
//! at coverage 0 reads black. That makes the algebra legible in the capture itself: the four
//! points below are chosen so that each commit moves exactly one of them, and every one of them
//! sits clear of every component's feather band, so each reading is an endpoint and not a ramp.
//!
//! The geometry is stated in **mask-space units**, where one unit is the stage's height on both
//! axes (`docs/design/mask-study.md`), because that is the space a radial's radii live in. A
//! sweep from a centre to a point puts `radius_x = |Δx| · W/H` and `radius_y = |Δy|` on the draft,
//! so each component below is swept to `(x + r · H/W, y + r)` and is a circle.
use crate::{
    scenario::{Checked, Checks, Frame, Plan, Run, Step, Tolerance, plan::only},
    *,
};
use luxforge_core::PRESENCE_EFFECT;
use luxforge_evidence::{
    self as script, MaskRow, MaskStep, Reference, RowStep, SliderStep, WorkspaceStep,
};

pub const SCENARIO: &str = "mask-combine";
/// The Presence fixture: its bottom-right quadrant is a flat mid-grey, which is where the two
/// patches that measure the masked Presence adjustment sit. `cargo xtask generate-fixtures`.
pub const FIXTURE: &str = "fixtures/generated/presence.jpg";
/// What `reproduce.md` says about the run: the fixture it needs generated, and what it does.
pub const NOTE: &str = "Generate the fixture first with `cargo xtask generate-fixtures --output fixtures/generated`.\n\nOne mask of four radial components in three modes, its coverage read from the `mask-on-black` overlay after every commit, a refused reorder, a reorder that changes the picture, a masked Presence drag and an undo.";

const RADIAL: &str = "radial";
const PRESENCE: &str = "set-presence";
const DEHAZE: &str = "dehaze";
/// What the masked Presence gesture commits. Dehaze is the one Presence unit that moves a flat
/// field at all — Texture and Clarity are neighbourhood operations and leave one exactly as it was,
/// which `presence` asserts — so it is the one this fixture's flat quadrant can measure. It is
/// deliberately short of `+100`, which drives this fixture's flat grey to black: a reading at the
/// end of the range would be produced by a broken render as readily as by a correct one.
const DEHAZED: f64 = 30.0;

/// One component: where its gesture presses, and the radius that press is swept out to, in
/// mask-space units. The sweep's own end point is derived, so the radius is stated once.
struct Shape {
    x: f64,
    y: f64,
    radius: f64,
}

/// The fixture's aspect, `W/H`. 1440 × 960.
const ASPECT: f64 = 1.5;

/// The four components, in the order they are drawn and composed: an `add`, a `subtract` taken out
/// of it, an `intersect` that keeps only part of what is left, and a second `add` that puts the
/// subtracted region back.
const A: Shape = Shape {
    x: 0.470,
    y: 0.490,
    radius: 0.675,
};
const S: Shape = Shape {
    x: 0.575,
    y: 0.250,
    radius: 0.315,
};
const I: Shape = Shape {
    x: 0.600,
    y: 0.490,
    radius: 0.450,
};
const D: Shape = Shape {
    x: 0.570,
    y: 0.250,
    radius: 0.240,
};

impl Shape {
    /// Where the sweep ends: a press at the centre dragged out to one radius on both axes.
    const fn to(&self) -> [f64; 2] {
        [self.x + self.radius / ASPECT, self.y + self.radius]
    }

    const fn from(&self) -> [f64; 2] {
        [self.x, self.y]
    }
}

/// The four points every coverage frame is read at, as fractions of the photograph's own drawn
/// rectangle. They are named for what the composition does to them.
///
/// `KEEP` and `OUT` are both inside the fixture's flat grey quadrant, so the same two patches also
/// measure the masked Presence adjustment: one inside the finished mask and one outside it.
const KEEP: [f64; 2] = [0.635, 0.660];
const CUT: [f64; 2] = [0.600, 0.310];
const INTERSECTED: [f64; 2] = [0.270, 0.475];
const OUT: [f64; 2] = [0.885, 0.860];
const PROBES: [[f64; 2]; 4] = [KEEP, CUT, INTERSECTED, OUT];
const PROBE_NAMES: [&str; 4] = ["keep", "cut", "intersected", "out"];

/// Half the side of a measured patch, in capture pixels. The nearest boundary to any probe is
/// about 29 capture pixels away, so a patch this size is well clear of every one of them.
const PATCH_HALF: i64 = 6;
/// How close to full coverage's 255 a `mask-on-black` patch must read before this scenario calls it
/// covered (200 or brighter), and how close to no coverage's 0 before it calls it uncovered (40 or
/// darker). The overlay paints coverage straight into all three channels; these are margins, not
/// tuned thresholds.
const COVERED_WITHIN: f64 = 55.0;
const UNCOVERED: f64 = 40.0;
/// How far the flat quadrant's mean must move before this scenario calls the masked Presence
/// adjustment visible, and how close it must stay before it calls a patch untouched. Both are
/// read from renderer readback with no JPEG between, so the untouched bound is tight on purpose.
const PRESENCE_MOVED: f64 = 4.0;
const PRESENCE_UNTOUCHED: f64 = 1.0;

/// The step that puts the pointer on component `row`'s row.
fn hover(row: usize) -> String {
    format!("hover-{row}")
}

/// One more component after the first: its mode chosen on the Add row before the gesture rather
/// than guessed from a modifier afterwards, a radial added, swept from its centre out to one
/// radius, and committed as one entry, after which the mask holds `components`. Only the commit
/// moves the history.
fn component(name: &str, mode: &str, shape: &Shape, label: &str, components: &[&str]) -> [Step; 4] {
    [
        Step::new(format!("{name}-mode"), MaskStep::Mode(mode.into())).commits(0),
        Step::new(format!("{name}-new"), MaskStep::Add(RADIAL.into())).commits(0),
        Step::new(
            format!("{name}-swept"),
            MaskStep::Sweep {
                from: shape.from(),
                to: shape.to(),
            },
        )
        .commits(0),
        Step::new(format!("{name}-applied"), MaskStep::Apply)
            .commits(1)
            .label(label)
            .no_draft()
            .components(components),
    ]
}

/// The finished mask's components, in the order they were drawn, and after the reorder.
const FINISHED: [&str; 4] = [
    "add radial",
    "subtract radial",
    "intersect radial",
    "add radial",
];
const REORDERED: [&str; 4] = [
    "add radial",
    "add radial",
    "subtract radial",
    "intersect radial",
];

/// Every frame, in order: the open, then one per step. Every step says what it commits, so the
/// four components are four entries and nothing between them commits; `verify` below checks the
/// composition, the list and what the photograph shows.
pub fn plan(_: &[PathBuf]) -> Plan {
    let mut steps = vec![
        // The fixture as launched: no mask, no Presence layer and nothing drafted.
        Step::opened("opened")
            .no_layer(PRESENCE_EFFECT)
            .no_draft()
            .masks(0),
        // 1: Mask mode, through the same `workspace.set` the mode strip sends.
        Step::new("mask-mode", WorkspaceStep::default().mode("mask"))
            .commits(0)
            .mode("mask"),
        // 2-5: the first component. A new mask whose first component is a radial, swept from its
        // centre out to one radius, the pointer lifted, then committed.
        Step::new("add-new", MaskStep::New(RADIAL.into())).commits(0),
        Step::new(
            "add-swept",
            MaskStep::Sweep {
                from: A.from(),
                to: A.to(),
            },
        )
        .commits(0),
        Step::new("add-released", MaskStep::Release).commits(0),
        Step::new("add-applied", MaskStep::Apply)
            .commits(1)
            .label("Add radial")
            .no_draft()
            .masks(1)
            .components(&FINISHED[..1]),
        // 6: the coverage itself on screen, which is what every reading below is taken from.
        Step::new(
            "overlay-on",
            WorkspaceStep::default().mask_overlay("mask-on-black"),
        )
        .commits(0)
        .workspace("mask_overlay", json!("mask-on-black")),
    ];
    // 7-10: a subtract.
    steps.extend(component(
        "subtract",
        "subtract",
        &S,
        "Add subtract radial",
        &FINISHED[..2],
    ));
    // 11-14: an intersect.
    steps.extend(component(
        "intersect",
        "intersect",
        &I,
        "Add intersect radial",
        &FINISHED[..3],
    ));
    // 15-18: a second add, over the region the subtraction took out.
    steps.extend(component("restore", "add", &D, "Add radial", &FINISHED));
    // 19-22: each component's own contribution, by putting the pointer on its row. This is what
    // makes a subtraction on top of a gradient legible instead of guesswork. Pointing commits
    // nothing.
    steps.extend(
        (0..4).map(|row| {
            Step::new(hover(row), MaskStep::Hover(Some(Reference::Index(row)))).commits(0)
        }),
    );
    steps.extend([
        // 23: the pointer off the list, so the composition is shown again.
        Step::new("hover-off", MaskStep::Hover(None)).commits(0),
        // 24: the one move this list refuses: a subtract at the front. The panel states that rule
        // on the row rather than offering the move, so only an explicit position reaches the
        // host's own refusal — and the refusal is what ends this step, because a refused command
        // renders nothing for it to settle on. It commits nothing, and the list does not move.
        Step::new(
            "front-refused",
            MaskStep::Row(MaskRow {
                component: Reference::Index(1),
                edit: RowStep::Move(0),
            }),
        )
        .commits(0)
        .refused("validation: mask Mask 1 begins with a subtract component")
        .components(&FINISHED),
        // 25: the reorder that does change the picture: the second add above the subtraction, so
        // what it put back is taken out again.
        Step::new(
            "reorder",
            MaskStep::Row(MaskRow {
                component: Reference::Index(3),
                edit: RowStep::Move(1),
            }),
        )
        .commits(1)
        .label("Move Radial 4")
        .no_draft()
        .components(&REORDERED),
        // 26: the overlay off, leaving the photograph.
        Step::new("overlay-off", WorkspaceStep::default().mask_overlay("off"))
            .commits(0)
            .workspace("mask_overlay", json!("off")),
        // 27: Presence through the mask, as the panel's own drag: the sections below the list are
        // bound to the open mask, so this commits a masked spatial layer.
        Step::new(
            "dehaze",
            SliderStep::new(PRESENCE, DEHAZE, [12.0, DEHAZED]).release(),
        )
        .commits(1)
        .label("Mask 1 · Dehaze +30")
        .no_draft()
        .payload(PRESENCE_EFFECT, json!({ DEHAZE: DEHAZED })),
        // 28: and undone: the layer gone, the reorder's entry current again.
        Step::new("undo", script::Step::api("history.undo"))
            .commits(1)
            .label("Move Radial 4")
            .no_draft()
            .no_layer(PRESENCE_EFFECT),
    ]);
    Plan::new(steps)
}

/// The identities of the open mask's components, in list order.
fn component_ids(frame: &Frame) -> Result<Vec<String>> {
    Ok(frame
        .components()?
        .iter()
        .map(|component| component["id"].as_str().unwrap_or_default().to_owned())
        .collect())
}

/// All four probes of one capture: the mean Rec. 709 luminance of a small patch at each, in the
/// photograph's recorded rectangle.
fn probes(frame: &Frame) -> Result<[f64; 4]> {
    let mut out = [0.0; 4];
    for (slot, at) in out.iter_mut().zip(PROBES) {
        *slot = frame.luminance_at(at, PATCH_HALF)?;
    }
    Ok(out)
}

/// One coverage frame against what the composition algebra says it must be: `1` for covered, `0`
/// for uncovered, at each of the four probes in turn. A covered probe reads within
/// [`COVERED_WITHIN`] of the overlay's full 255, an uncovered one within [`UNCOVERED`] of its 0.
fn coverage(checks: &mut Checks, frame: &Frame, what: &str, expected: [u8; 4]) -> Result {
    let read = probes(frame)?;
    for ((value, want), name) in read.iter().zip(expected).zip(PROBE_NAMES) {
        let (full, within) = if want == 1 {
            (255.0, COVERED_WITHIN)
        } else {
            (0.0, UNCOVERED)
        };
        checks.compare(
            frame,
            &format!("{what}: {name} at coverage {want}"),
            *value,
            full,
            Tolerance::Within(within),
        )?;
    }
    Ok(())
}

/// Every step, in order, against the algebra and against the pixels.
pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let mut checks = Checks::new();
    let opened = probes(launch.at("opened")?)?;
    let mask = launch.at("add-applied")?.only_mask()?["id"]
        .as_str()
        .ok_or("The listed mask has no identity")?
        .to_owned();

    // The composition after each commit, read off the coverage itself. Each one moves exactly one
    // probe, which is what makes the algebra legible in the captures: one radial selects
    // everything inside it; the subtracted radial takes its own region back out; the intersect
    // keeps only what both it and what came before hold; a second add puts back exactly the region
    // the subtraction removed.
    for (step, what, expected) in [
        ("overlay-on", "the first add alone", [1, 1, 1, 0]),
        (
            "subtract-applied",
            "the add minus the subtract",
            [1, 0, 1, 0],
        ),
        ("intersect-applied", "intersected", [1, 0, 0, 0]),
        ("restore-applied", "the second add restored", [1, 1, 0, 0]),
    ] {
        coverage(&mut checks, launch.at(step)?, what, expected)?;
    }
    let drawn = component_ids(launch.at("restore-applied")?)?;

    // Each component's own contribution, shown by pointing at its row. What the overlay draws is
    // the component's own field, before its mode is applied, so the subtract's row reads 1 where it
    // subtracts. Nothing is selected by pointing at one, and the plan holds each to no commit.
    //
    // Rows 1 and 3 read the same at these four points, because the subtract and the second add are
    // concentric by construction — that is what lets the second add restore exactly what the
    // subtract removed. What separates them is the composition, which the subtract's and the
    // second add's commits measure, and the radii, which the captures show.
    let alone: [[u8; 4]; 4] = [[1, 1, 1, 0], [0, 1, 0, 0], [1, 1, 0, 0], [0, 1, 0, 0]];
    for (row, expected) in alone.into_iter().enumerate() {
        let step = hover(row);
        let frame = launch.at(&step)?;
        ensure(
            frame
                .components()?
                .iter()
                .position(|component| component["hovered"] == json!(true))
                == Some(row),
            format!("Step {step} shows {} hovered", json!(frame.components()?)),
        )?;
        coverage(
            &mut checks,
            frame,
            &format!("component {row} alone"),
            expected,
        )?;
    }

    // The pointer off the list, and the composition again.
    coverage(
        &mut checks,
        launch.at("hover-off")?,
        "the composition with the pointer off the list",
        [1, 1, 0, 0],
    )?;

    // The refused reorder. The plan holds the step to the host's own refusal, to no commit and to
    // the list it had; the reason names the rule, no component moved, and the coverage is exactly
    // what it was — and the run reached this frame at all, which is the point: a refused command
    // renders nothing, so the step is ended by the refusal.
    let refused = launch.at("front-refused")?;
    let reason = refused["step"]["reason"]
        .as_str()
        .ok_or("The refused step recorded no reason")?;
    ensure(
        reason.contains("add"),
        format!("The refusal's reason is {reason:?}"),
    )?;
    ensure(
        component_ids(refused)? == drawn,
        "The refused reorder moved the list",
    )?;
    coverage(
        &mut checks,
        refused,
        "the coverage after a refused reorder",
        [1, 1, 0, 0],
    )?;

    // The reorder that changes the picture. The second add now applies before the subtraction
    // rather than after it, so what it restored is taken out again.
    let reorder = launch.at("reorder")?;
    let reordered = [&drawn[0], &drawn[3], &drawn[1], &drawn[2]].map(String::to_owned);
    ensure(
        component_ids(reorder)? == reordered,
        format!("The reorder produced {:?}", component_ids(reorder)?),
    )?;
    coverage(
        &mut checks,
        reorder,
        "the coverage after the reorder",
        [1, 0, 0, 0],
    )?;

    // The overlay off. The mask is a selection and nothing else: no layer is bound to it yet, so
    // the photograph is byte-unchanged from the one that opened.
    let overlay_off = launch.at("overlay-off")?;
    ensure(
        overlay_off.only_mask()?["layers"] == json!([]),
        "A mask with no adjustment already has a layer bound to it",
    )?;
    let bare = probes(overlay_off)?;
    for (index, name) in PROBE_NAMES.iter().enumerate() {
        checks.compare(
            overlay_off,
            &format!("{name} with the mask drawn and no layer bound to it"),
            bare[index],
            opened[index],
            Tolerance::Within(PRESENCE_UNTOUCHED),
        )?;
    }

    // Presence through the mask. The flat quadrant moves inside the mask and is left exactly as it
    // was outside it, and the layer the panel committed names the mask.
    let dehaze = launch.at("dehaze")?;
    let layer = dehaze
        .layer(PRESENCE_EFFECT)
        .ok_or("The masked gesture committed no Presence layer")?;
    ensure(
        layer["mask"] == json!(mask),
        format!("The committed Presence layer names {}", layer["mask"]),
    )?;
    let dehazed = probes(dehaze)?;
    checks.compare(
        dehaze,
        "the covered patch under masked Presence",
        dehazed[0],
        bare[0],
        Tolerance::Apart(PRESENCE_MOVED),
    )?;
    // Strictly inside 1..254, which is strictly within 126.5 of 127.5: a clipped reading is
    // produced by a broken render as readily as by a correct one.
    checks.compare(
        dehaze,
        "the covered patch under masked Presence, clear of the ends of the range",
        dehazed[0],
        127.5,
        Tolerance::Under(126.5),
    )?;
    checks.compare(
        dehaze,
        "the uncovered patch under masked Presence",
        dehazed[3],
        bare[3],
        Tolerance::Within(PRESENCE_UNTOUCHED),
    )?;

    // Undo. The plan holds the layer to gone; the mask and every component keep their identities
    // in their reordered order, and the photograph is back to the one the mask alone left.
    let undo = launch.at("undo")?;
    ensure(
        component_ids(undo)? == reordered,
        "Undo changed the component identities or their order",
    )?;
    let undone = probes(undo)?;
    for (index, name) in PROBE_NAMES.iter().enumerate() {
        checks.compare(
            undo,
            &format!("{name} after undo"),
            undone[index],
            bare[index],
            Tolerance::Within(PRESENCE_UNTOUCHED),
        )?;
    }

    checks.write(
        run.out(),
        SCENARIO,
        json!({
            "mask": mask,
            "components": drawn,
            "reordered": reordered,
            "refused_step": {"step": refused["step"]["step"], "reason": reason},
            "covered_within": COVERED_WITHIN,
            "uncovered_within": UNCOVERED,
            "scope": "Mean Rec. 709 luminance of four patches of the photograph the editor records drawing, read back from the renderer; the coverage readings are of the mask-on-black overlay, which paints coverage straight into all three channels, and are not a colorimetric claim",
        }),
    )
}
