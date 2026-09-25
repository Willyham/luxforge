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
    scenario::{Bright, Checked, Frame, Plan, Run, Scan, Step, Tolerance, pixels, plan::only},
    *,
};
use luxforge_core::PRESENCE_EFFECT;

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
/// How bright a `mask-on-black` patch must read before this scenario calls it covered, and how
/// dark before it calls it uncovered. The overlay paints coverage straight into all three
/// channels, so full coverage is 255 and none is 0; these are margins, not tuned thresholds.
const COVERED: f64 = 200.0;
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
/// radius, and committed as one entry. Only the commit moves the history.
fn component(name: &str, mode: &str, shape: &Shape, label: &str) -> [Step; 4] {
    [
        Step::new(format!("{name}-mode"), json!({"mask":{"mode":mode}})).commits(0),
        Step::new(format!("{name}-new"), json!({"mask":{"add":RADIAL}})).commits(0),
        Step::new(
            format!("{name}-swept"),
            json!({"mask":{"sweep":{"from":shape.from(),"to":shape.to()}}}),
        )
        .commits(0),
        Step::new(format!("{name}-applied"), json!({"mask":{"apply":true}}))
            .commits(1)
            .label(label)
            .no_draft(),
    ]
}

/// Every frame, in order: the open, then one per step. Every step says what it commits, so the
/// four components are four entries and nothing between them commits; `verify` below checks the
/// composition, the list and what the photograph shows.
pub fn plan(_: &[PathBuf]) -> Plan {
    let mut steps = vec![
        // The fixture as launched: no Presence layer and nothing drafted.
        Step::opened("opened").no_layer(PRESENCE_EFFECT).no_draft(),
        // 1: Mask mode, through the same `workspace.set` the mode strip sends.
        Step::new("mask-mode", json!({"workspace":{"mode":"mask"}})).commits(0),
        // 2-5: the first component. A new mask whose first component is a radial, swept from its
        // centre out to one radius, the pointer lifted, then committed.
        Step::new("add-new", json!({"mask":{"new":RADIAL}})).commits(0),
        Step::new(
            "add-swept",
            json!({"mask":{"sweep":{"from":A.from(),"to":A.to()}}}),
        )
        .commits(0),
        Step::new("add-released", json!({"mask":{"release":true}})).commits(0),
        Step::new("add-applied", json!({"mask":{"apply":true}}))
            .commits(1)
            .label("Add radial")
            .no_draft(),
        // 6: the coverage itself on screen, which is what every reading below is taken from.
        Step::new(
            "overlay-on",
            json!({"workspace":{"mask_overlay":"mask-on-black"}}),
        )
        .commits(0),
    ];
    // 7-10: a subtract.
    steps.extend(component("subtract", "subtract", &S, "Add subtract radial"));
    // 11-14: an intersect.
    steps.extend(component(
        "intersect",
        "intersect",
        &I,
        "Add intersect radial",
    ));
    // 15-18: a second add, over the region the subtraction took out.
    steps.extend(component("restore", "add", &D, "Add radial"));
    // 19-22: each component's own contribution, by putting the pointer on its row. This is what
    // makes a subtraction on top of a gradient legible instead of guesswork. Pointing commits
    // nothing.
    steps.extend((0..4).map(|row| Step::new(hover(row), json!({"mask":{"hover":row}})).commits(0)));
    steps.extend([
        // 23: the pointer off the list, so the composition is shown again.
        Step::new("hover-off", json!({"mask":{"hover":null}})).commits(0),
        // 24: the one move this list refuses: a subtract at the front. The panel states that rule
        // on the row rather than offering the move, so only an explicit position reaches the
        // host's own refusal — and the refusal is what ends this step, because a refused command
        // renders nothing for it to settle on. It commits nothing.
        Step::new(
            "front-refused",
            json!({"mask":{"row":{"component":1,"index":0}}}),
        )
        .commits(0)
        .refused("validation: mask Mask 1 begins with a subtract component"),
        // 25: the reorder that does change the picture: the second add above the subtraction, so
        // what it put back is taken out again.
        Step::new(
            "reorder",
            json!({"mask":{"row":{"component":3,"index":1}}}),
        )
        .commits(1)
        .label("Move Radial 4")
        .no_draft(),
        // 26: the overlay off, leaving the photograph.
        Step::new("overlay-off", json!({"workspace":{"mask_overlay":"off"}})).commits(0),
        // 27: Presence through the mask, as the panel's own drag: the sections below the list are
        // bound to the open mask, so this commits a masked spatial layer.
        Step::new(
            "dehaze",
            json!({"slider":{"action":PRESENCE,"parameter":DEHAZE,"values":[12.0,DEHAZED],"release":true}}),
        )
        .commits(1)
        .label("Mask 1 · Dehaze +30")
        .no_draft()
        .payload(PRESENCE_EFFECT, json!({ DEHAZE: DEHAZED })),
        // 28: and undone: the layer gone, the reorder's entry current again.
        Step::new("undo", json!({"api":{"method":"history.undo","params":{}}}))
            .commits(1)
            .label("Move Radial 4")
            .no_draft()
            .no_layer(PRESENCE_EFFECT),
    ]);
    Plan::new(steps)
}

/// The modes of the open mask's components, in list order: the composition, in one line.
fn modes(frame: &Frame) -> Result<Vec<String>> {
    Ok(frame
        .components()?
        .iter()
        .map(|component| component["mode"].as_str().unwrap_or_default().to_owned())
        .collect())
}

/// The identities of the open mask's components, in list order.
fn component_ids(frame: &Frame) -> Result<Vec<String>> {
    Ok(frame
        .components()?
        .iter()
        .map(|component| component["id"].as_str().unwrap_or_default().to_owned())
        .collect())
}

/// The stack's one Presence layer, or `None` when the stack holds none.
fn presence_layer(frame: &Frame) -> Option<&Value> {
    frame.layer(PRESENCE_EFFECT)
}

/// Where the photograph is drawn inside the capture.
///
/// It is found once, on the opened fixture, and reused: the zoom is Fit for the whole run and the
/// panels never move, so the rectangle is the same in every frame — and it cannot be found again
/// from a frame the coverage overlay has painted black, which is most of them. The vertical extent
/// is taken first, for the reason [`Scan::Tallest`] records.
const BOUNDS: Bright = Bright {
    threshold: 32,
    scan: Scan::Tallest { last_row: false },
    least: Some((200, 100)),
};

/// All four probes of one capture: the mean Rec. 709 luminance of a small patch at each.
fn probes(frame: &Frame, bounds: [u32; 4]) -> Result<[f64; 4]> {
    let image = frame.image()?;
    let mut out = [0.0; 4];
    for (slot, at) in out.iter_mut().zip(PROBES) {
        *slot = pixels::mean_luminance(image, pixels::at(bounds, at), PATCH_HALF)?;
    }
    Ok(out)
}

/// One coverage frame against what the composition algebra says it must be: `1` for covered, `0`
/// for uncovered, at each of the four probes in turn.
fn coverage(frame: &Frame, bounds: [u32; 4], what: &str, expected: [u8; 4]) -> Result<[f64; 4]> {
    let read = probes(frame, bounds)?;
    for ((value, want), name) in read.iter().zip(expected).zip(PROBE_NAMES) {
        let ok = if want == 1 {
            *value >= COVERED
        } else {
            *value <= UNCOVERED
        };
        ensure(
            ok,
            format!(
                "{what}: {name} read {value:.1}, which is not coverage {want} \
                 (covered is >= {COVERED}, uncovered <= {UNCOVERED})"
            ),
        )?;
    }
    Ok(read)
}

/// The one launch, once it has held its plan: every step against the algebra and the pixels.
pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let checks = checks(only(launches)?)?;
    run.record("checks", checks.clone());
    write_json(
        &run.out().join("mask-combine-checks.json"),
        &json!({"checks": checks}),
    )?;
    Ok(())
}

/// Every step, in order, against the algebra and against the pixels.
fn checks(launch: &Checked) -> Result<Value> {
    let opened_frame = launch.at("opened")?;
    let bounds = pixels::bright_bounds(opened_frame, BOUNDS)?;
    let mut shows = Vec::new();
    let mut record = |frame: &Value, what: &str, detail: Value| {
        shows.push(json!({"frame":frame["file"],"shows":what,"detail":detail}));
    };

    // The fixture as launched, with no mask in the recipe (the plan holds it to no Presence
    // layer).
    ensure(
        opened_frame.masks()?.is_empty(),
        "The fixture opened with a mask already in the recipe",
    )?;
    let opened = probes(opened_frame, bounds)?;
    record(
        opened_frame,
        "the fixture as launched, with no mask in the recipe",
        json!({"patches":opened,"bounds":bounds}),
    );

    // The first component committed. One mask, one `add` component.
    let first = launch.at("add-applied")?;
    ensure(
        modes(first)? == ["add"],
        format!("The first commit holds {:?}", modes(first)?),
    )?;
    let mask = first.only_mask()?["id"]
        .as_str()
        .ok_or("The listed mask has no identity")?
        .to_owned();

    // The composition after each commit, read off the coverage itself. Each one moves exactly one
    // probe, which is what makes the algebra legible in the captures.
    let stages: [(&str, &str, [u8; 4], &str); 4] = [
        (
            "overlay-on",
            "the first add alone",
            [1, 1, 1, 0],
            "one radial: everything inside it is selected",
        ),
        (
            "subtract-applied",
            "the add minus the subtract",
            [1, 0, 1, 0],
            "the subtracted radial takes its own region back out",
        ),
        (
            "intersect-applied",
            "intersected",
            [1, 0, 0, 0],
            "the intersect keeps only what both it and what came before hold",
        ),
        (
            "restore-applied",
            "the second add restored",
            [1, 1, 0, 0],
            "a second add puts back exactly the region the subtraction removed",
        ),
    ];
    let mut readings = Vec::new();
    for (step, what, expected, caption) in stages {
        let frame = launch.at(step)?;
        let read = coverage(frame, bounds, what, expected)?;
        readings
            .push(json!({"frame":frame["file"],"stage":what,"expected":expected,"patches":read}));
        record(
            frame,
            caption,
            json!({"modes":modes(frame)?,"expected":expected,"patches":read}),
        );
    }
    // Four components, and — the plan holds every step from the open to here to what it commits —
    // four history entries.
    let finished = launch.at("restore-applied")?;
    ensure(
        modes(finished)? == ["add", "subtract", "intersect", "add"],
        format!("The finished mask holds {:?}", modes(finished)?),
    )?;
    let drawn = component_ids(finished)?;

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
        let read = coverage(frame, bounds, &format!("component {row} alone"), expected)?;
        record(
            frame,
            "one component's own contribution, shown by pointing at its row",
            json!({"component":drawn[row],"mode":modes(frame)?[row],"expected":expected,"patches":read}),
        );
    }

    // The pointer off the list, and the composition again.
    let hover_off = launch.at("hover-off")?;
    let composed = coverage(
        hover_off,
        bounds,
        "the composition with the pointer off the list",
        [1, 1, 0, 0],
    )?;
    record(
        hover_off,
        "the composed mask again, with the pointer off the list",
        json!({"patches":composed}),
    );

    // The refused reorder. The plan holds the step to the host's own refusal and to no commit; the
    // reason names the rule, the list did not move, and the coverage is exactly what it was — and
    // the run reached this frame at all, which is the point: a refused command renders nothing, so
    // the step is ended by the refusal.
    let refused = launch.at("front-refused")?;
    let reason = refused["step"]["reason"]
        .as_str()
        .ok_or("The refused step recorded no reason")?;
    ensure(
        reason.contains("add"),
        format!("The refusal's reason is {reason:?}"),
    )?;
    ensure(
        component_ids(refused)? == drawn && modes(refused)? == modes(finished)?,
        "The refused reorder moved the list",
    )?;
    let after_refusal = coverage(
        refused,
        bounds,
        "the coverage after a refused reorder",
        [1, 1, 0, 0],
    )?;
    record(
        refused,
        "a reorder the host refused: its reason on the step, the list and the coverage unmoved",
        json!({"reason":reason,"patches":after_refusal,"modes":modes(refused)?}),
    );

    // The reorder that changes the picture. The second add now applies before the subtraction
    // rather than after it, so what it restored is taken out again.
    let reorder = launch.at("reorder")?;
    ensure(
        modes(reorder)? == ["add", "add", "subtract", "intersect"],
        format!("The reordered mask holds {:?}", modes(reorder)?),
    )?;
    ensure(
        component_ids(reorder)?
            == [
                drawn[0].clone(),
                drawn[3].clone(),
                drawn[1].clone(),
                drawn[2].clone(),
            ],
        format!("The reorder produced {:?}", component_ids(reorder)?),
    )?;
    let reordered = coverage(
        reorder,
        bounds,
        "the coverage after the reorder",
        [1, 0, 0, 0],
    )?;
    record(
        reorder,
        "the second add moved above the subtraction, so the region it restored is removed again",
        json!({"modes":modes(reorder)?,"label":reorder.label()?,"patches":reordered}),
    );

    // The overlay off. The mask is a selection and nothing else: no layer is bound to it yet, so
    // the photograph is byte-unchanged from the one that opened.
    let overlay_off = launch.at("overlay-off")?;
    ensure(
        overlay_off.only_mask()?["layers"] == json!([]),
        "A mask with no adjustment already has a layer bound to it",
    )?;
    let bare = probes(overlay_off, bounds)?;
    for (index, name) in PROBE_NAMES.iter().enumerate() {
        pixels::compare(
            &format!("{name} with the mask drawn and no layer bound to it"),
            bare[index],
            opened[index],
            Tolerance::Within(PRESENCE_UNTOUCHED),
        )?;
    }
    record(
        overlay_off,
        "the overlay off: a mask on its own is a selection, not an edit",
        json!({"patches":bare}),
    );

    // Presence through the mask. The flat quadrant moves inside the mask and is left exactly as it
    // was outside it, and the layer the panel committed names the mask.
    let dehaze = launch.at("dehaze")?;
    let layer = presence_layer(dehaze).ok_or("The masked gesture committed no Presence layer")?;
    ensure(
        layer["mask"] == json!(mask),
        format!("The committed Presence layer names {}", layer["mask"]),
    )?;
    let dehazed = probes(dehaze, bounds)?;
    pixels::compare(
        "the covered patch under masked Presence",
        dehazed[0],
        bare[0],
        Tolerance::Apart(PRESENCE_MOVED),
    )?;
    ensure(
        dehazed[0] > 1.0 && dehazed[0] < 254.0,
        format!(
            "The covered patch read {:.2}, which is against the end of the range: a clipped \
             reading is produced by a broken render as readily as by a correct one",
            dehazed[0]
        ),
    )?;
    pixels::compare(
        "the uncovered patch under masked Presence",
        dehazed[3],
        bare[3],
        Tolerance::Within(PRESENCE_UNTOUCHED),
    )?;
    record(
        dehaze,
        "one part of the photograph adjusted through the composed mask and the other left alone",
        json!({"layer":layer["id"],DEHAZE:DEHAZED,"label":dehaze.label()?,
               "covered":dehazed[0],"uncovered":dehazed[3],"before":bare}),
    );

    // Undo. The plan holds the layer to gone; the mask and every component keep their identities
    // in their reordered order, and the photograph is back to the one the mask alone left.
    let undo = launch.at("undo")?;
    ensure(
        component_ids(undo)? == component_ids(reorder)?,
        "Undo changed the component identities or their order",
    )?;
    let undone = probes(undo, bounds)?;
    for (index, name) in PROBE_NAMES.iter().enumerate() {
        pixels::compare(
            &format!("{name} after undo"),
            undone[index],
            bare[index],
            Tolerance::Within(PRESENCE_UNTOUCHED),
        )?;
    }
    record(
        undo,
        "the undone state: the composed mask, with nothing applied through it",
        json!({"patches":undone,"label":undo.label()?}),
    );

    Ok(json!({
        "mask": mask,
        "components": drawn,
        "reordered": component_ids(reorder)?,
        "modes": modes(reorder)?,
        "revision": undo.revision()?,
        "refused_step": {"step": refused["step"]["step"], "reason": reason},
        "coverage": readings,
        "presence": {"covered": dehazed[0], "uncovered": dehazed[3], "before": bare},
        "covered_threshold": COVERED,
        "uncovered_threshold": UNCOVERED,
        "frames": shows,
        "scope": "Mean Rec. 709 luminance of four patches of the displayed photograph, read back from the renderer; the coverage readings are of the mask-on-black overlay, which paints coverage straight into all three channels, and are not a colorimetric claim",
    }))
}
